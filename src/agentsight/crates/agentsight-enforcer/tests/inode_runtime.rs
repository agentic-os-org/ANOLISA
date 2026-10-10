#![cfg(all(feature = "actplane", target_os = "linux"))]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, CredentialExfiltrationPolicy, DestinationScope, PolicyMode,
    SecurityEvent, SecurityEventKind,
};
use agentsight_enforcer::{ActPlaneBackend, EnforcementBackend, SubscriberClass};
use uuid::Uuid;

const WORKER: &str = r#"
import errno, os, socket, sys
path = sys.argv[1]
fd = os.open(path, os.O_RDONLY)
print('ready', flush=True)
for line in sys.stdin:
    op = line.strip()
    try:
        if op == 'unlink':
            os.unlink(path)
        elif op == 'rename':
            os.rename(path, path + '.renamed')
        elif op == 'readfd':
            os.lseek(fd, 0, os.SEEK_SET)
            os.read(fd, 4096)
        elif op == 'read':
            with open(path, 'rb') as f:
                f.read()
        elif op == 'mmap':
            import mmap
            with mmap.mmap(fd, 0, access=mmap.ACCESS_READ) as m:
                m.read()
        elif op.startswith('connect '):
            with socket.socket() as s:
                s.setblocking(False)
                s.connect_ex((op.split()[1], 9))
        elif op == 'exit':
            break
        else:
            raise ValueError(op)
        print('ok', flush=True)
    except OSError as e:
        print('errno=' + str(e.errno), flush=True)
"#;

struct Worker {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl Worker {
    fn spawn(path: &str) -> Self {
        let mut child = Command::new("unshare")
            .args(["-n", "python3", "-u", "-c", WORKER, path])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn network-isolated test process");
        let input = child.stdin.take().expect("child stdin");
        let mut output = BufReader::new(child.stdout.take().expect("child stdout"));
        let mut ready = String::new();
        output.read_line(&mut ready).expect("worker ready");
        assert_eq!(ready.trim(), "ready");
        Self {
            child,
            input,
            output,
        }
    }

    fn pid(&self) -> i32 {
        self.child.id() as i32
    }

    fn start_time(&self) -> u64 {
        fs::read_to_string(format!("/proc/{}/stat", self.pid()))
            .expect("process stat")
            .rsplit_once(") ")
            .expect("comm terminator")
            .1
            .split_whitespace()
            .nth(19)
            .expect("start time")
            .parse()
            .expect("numeric start time")
    }

    fn run(&mut self, command: &str) -> String {
        writeln!(self.input, "{command}").expect("worker command");
        self.input.flush().expect("flush command");
        let mut result = String::new();
        self.output.read_line(&mut result).expect("worker result");
        result.trim().to_owned()
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = writeln!(self.input, "exit");
        let _ = self.input.flush();
        let _ = self.child.wait();
    }
}

struct Files {
    root: PathBuf,
    pin_root: PathBuf,
}

impl Files {
    fn new(profile: &str) -> Self {
        let name = format!("ap-{}", &Uuid::new_v4().simple().to_string()[..10]);
        let root = PathBuf::from("/tmp").join(&name);
        let pin_root = PathBuf::from("/sys/fs/bpf").join(name);
        fs::create_dir(&root).expect("test directory");
        fs::write(
            root.join("source"),
            b"synthetic fixture, not a credential\n",
        )
        .expect("source fixture");
        // This ignored test must run alone; runtime threads are joined before the next profile.
        unsafe {
            std::env::set_var("ACTPLANE_PINNED_PROFILE", profile);
            std::env::set_var("ACTPLANE_BPF_PIN_ROOT", &pin_root);
        }
        Self { root, pin_root }
    }

    fn source(&self) -> String {
        self.root
            .join("source")
            .to_str()
            .expect("ASCII path")
            .to_owned()
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.pin_root);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn file_guard() {
    let files = Files::new("agent-file-guard");
    let source = files.source();
    let mut worker = Worker::spawn(&source);
    let backend = ActPlaneBackend::open().expect("file guard runtime");
    assert!(
        backend
            .health()
            .expect("health")
            .capabilities
            .file_delete_guard
    );
    let events = backend.subscribe(Uuid::new_v4(), SubscriberClass::Required);
    let binding_id = Uuid::new_v4();
    backend.apply(ApplyPolicy {
        binding_id,
        agent_id: "inode-test".into(),
        session_id: None,
        root_pid: worker.pid(),
        process_start_time: worker.start_time(),
        policy_id: "inode-guard".into(),
        policy_revision: "1".into(),
        policy_dsl: format!(
            "source AGENT = exec \"**\"\nrule unrelated:\nnotify open file \"/tmp/never-matches\" if AGENT\nbecause \"unrelated\"\nrule protected:\nblock unlink file \"{source}\" if AGENT\nbecause \"keep fixture\"\n"
        ),
        policy_mode: Some(PolicyMode::Enforce),
    }).expect("apply file guard");
    for operation in ["unlink", "rename"] {
        assert_eq!(worker.run(operation), "errno=1");
        assert!(PathBuf::from(&source).exists());
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .expect("guard audit event");
        assert_eq!(event.operation, operation);
        assert_eq!(event.target, source);
        assert_eq!(event.rule_id.as_deref(), Some("protected"));
        assert_eq!(event.reason.as_deref(), Some("keep fixture"));
        assert!(event.blocked);
    }
    let unguarded_path = files.root.join("outside");
    fs::hard_link(&source, &unguarded_path).expect("outside-domain hardlink");
    fs::remove_file(&unguarded_path).expect("unbound caller is not blocked");
    backend.detach(binding_id).expect("detach guard");
    assert_eq!(worker.run("unlink"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
}

fn receive_chain(events: &Receiver<SecurityEvent>, source: &str) {
    let mut found_source = false;
    let mut found_taint = false;
    let mut found_network = false;
    for _ in 0..12 {
        let event = events
            .recv_timeout(Duration::from_secs(3))
            .expect("credential security event");
        match event.kind {
            SecurityEventKind::FileAction(action) => {
                assert_eq!(action.path, source);
                assert_eq!(action.operation, "open");
                found_source = true;
            }
            SecurityEventKind::TaintTransition(_) => found_taint = true,
            SecurityEventKind::NetworkAction(_) => found_network = true,
            SecurityEventKind::EnforcementState(state) => {
                panic!("unexpected runtime state: {state:?}")
            }
            SecurityEventKind::PolicyDecision(decision) => {
                assert!(!decision.blocked);
                assert_eq!(decision.mode, PolicyMode::Audit);
                assert!(found_source && found_taint && found_network);
                return;
            }
        }
    }
    panic!("missing credential decision");
}

fn credentials(read_operation: &str) {
    let files = Files::new("credential-exfiltration");
    let source = files.source();
    let mut worker = Worker::spawn(&source);
    let backend = ActPlaneBackend::open().expect("credential runtime");
    let events = backend.subscribe_security_events();
    let binding_id = Uuid::new_v4();
    backend
        .apply_credential_policy(ApplyCredentialPolicy {
            binding_id,
            agent_id: "inode-test".into(),
            session_id: None,
            root_pid: worker.pid(),
            process_start_time: worker.start_time(),
            policy: CredentialExfiltrationPolicy {
                policy_id: "credential-fixture".into(),
                revision: 1,
                source_patterns: vec![source.clone()],
                trusted_endpoints: vec!["9.9.9.9".into()],
                taint_label: "CREDENTIAL".into(),
                taint_ttl_secs: 900,
                destination_scope: DestinationScope::PublicIpv4,
                mode: PolicyMode::Audit,
            },
        })
        .expect("apply credential policy");
    assert_eq!(worker.run("connect 8.8.8.8"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    {
        let mut unbound = Worker::spawn(&source);
        assert_eq!(unbound.run(read_operation), "ok");
        assert_eq!(unbound.run("connect 8.8.8.8"), "ok");
        assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    }
    assert_eq!(worker.run(read_operation), "ok");
    assert_eq!(worker.run("connect 9.9.9.9"), "ok");
    assert_eq!(worker.run("connect 127.0.0.1"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    assert_eq!(worker.run("connect 8.8.8.8"), "ok");
    receive_chain(&events, &source);
    backend
        .detach(binding_id)
        .expect("detach credential policy");
    while events.try_recv().is_ok() {}
    assert_eq!(worker.run("read"), "ok");
    assert_eq!(worker.run("connect 8.8.8.8"), "ok");
    assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
    assert_eq!(worker.run("unlink"), "ok");
}

#[test]
#[ignore = "requires root, active BPF LSM, unlimited memlock, and --test-threads=1"]
fn inode_guard_and_credential_events_on_live_kernel() {
    file_guard();
    for read_operation in ["read", "readfd", "mmap"] {
        credentials(read_operation);
    }
}
