//! Installed-package acceptance without Python, Agent accounts or source helpers.
//! Native framework adoption is validated separately through the documented user flow.

use aw_service::{Client, Operation};
use serde_json::{json, Value};
use std::{
    fs::{self, File, Permissions},
    io::Write,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Process {
    child: Child,
    finished: bool,
}
impl Process {
    fn start(program: &Path, arguments: &[&std::ffi::OsStr], work: &Path, name: &str) -> Self {
        let log_path = work.join(format!("{name}.log"));
        let log = File::create(&log_path).unwrap();
        let child = Command::new(program)
            .args(arguments)
            .current_dir(work)
            .env("AGENT_SEC_DATA_DIR", work.join("sec-data"))
            .env("OTEL_SDK_DISABLED", "true")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .process_group(0)
            .spawn()
            .unwrap();
        let mut ledger = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(work.join("processes.jsonl"))
            .unwrap();
        writeln!(ledger,"{}",json!({"pid":child.id(),"pgid":child.id(),"program":program,"args":arguments.iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>(),"cwd":work,"log":log_path,"ports":[],"lifetime_seconds":120,"stop":format!("kill -TERM -- -{}",child.id())})).unwrap();
        Self {
            child,
            finished: false,
        }
    }
    fn wait(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                self.finished = true;
                assert!(status.success());
                return;
            }
            assert!(Instant::now() < deadline, "owned service did not stop");
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // SAFETY: these tests own the unreaped process-group leader until this wait.
        unsafe {
            libc::kill(-(self.child.id() as i32), libc::SIGKILL);
        }
        let _ = self.child.wait();
    }
}
fn wait_socket(socket: &Path, process: &mut Process) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        if let Some(status) = process.child.try_wait().unwrap() {
            process.finished = true;
            panic!("service exited before binding socket: {status}");
        }
        assert!(Instant::now() < deadline, "socket did not appear");
        thread::sleep(Duration::from_millis(20));
    }
}
fn rpc(client: &Client, value: Value) -> Value {
    client
        .call(
            Operation::from_json(&serde_json::to_vec(&value).unwrap()).unwrap(),
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap()
}

#[test]
#[ignore = "requires installed Preview binaries and root or AW_TEST_SEC_SOCKET"]
fn installed_binaries_scan_and_recover_audit() {
    let prefix = PathBuf::from(std::env::var_os("AW_TEST_PREFIX").expect("AW_TEST_PREFIX"));
    let work = PathBuf::from(std::env::var_os("AW_TEST_WORK_DIR").expect("AW_TEST_WORK_DIR"));
    fs::create_dir(&work).unwrap();
    fs::set_permissions(&work, Permissions::from_mode(0o700)).unwrap();
    let socket = std::env::var_os("AW_TEST_SEC_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| work.join("sec.sock"));
    let config = work.join("aw.yaml");
    let state = work.join("state");
    aw_package::configure(&aw_package::Settings {
        prefix: &prefix,
        config: &config,
        state: &state,
        socket: &socket,
        qoder: Path::new("/bin/true"),
        node: Path::new("/bin/true"),
        openclaw: Path::new("/bin/true"),
    })
    .unwrap();
    let provider = prefix.join("libexec/aw/providers/sec-core");
    let sec_config = work.join("skillsec.json");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&sec_config)
        .unwrap();
    file.write_all(
        serde_json::to_string(&json!({"stateDir":work.join("skillsec")}))
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    drop(file);
    let mut sec = if std::env::var_os("AW_TEST_SEC_SOCKET").is_none() {
        // SAFETY: geteuid reads the current effective user.
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "use root or an existing AW_TEST_SEC_SOCKET"
        );
        Some(Process::start(
            &provider.join("agent-sec-daemon"),
            &[
                "serve".as_ref(),
                "--socket".as_ref(),
                socket.as_os_str(),
                "--skillsec-config".as_ref(),
                sec_config.as_os_str(),
            ],
            &work,
            "sec-core",
        ))
    } else {
        None
    };
    if let Some(process) = sec.as_mut() {
        wait_socket(&socket, process);
    }
    let aw = prefix.join("bin/aw");
    let args = [
        "serve".as_ref(),
        "--config".as_ref(),
        config.as_os_str(),
        "--state-dir".as_ref(),
        state.as_os_str(),
    ];
    let mut service = Process::start(&aw, &args, &work, "aw");
    let aw_socket = state.join("aw.sock");
    wait_socket(&aw_socket, &mut service);
    let client = Client::connect(&aw_socket, Instant::now() + Duration::from_secs(5)).unwrap();
    let generation = client.identity().generation.clone();
    let mut audits = Vec::new();
    for (target, tool) in [("qoder", "Bash"), ("openclaw", "exec")] {
        let binding = rpc(
            &client,
            json!({"method":"bind","target":target,"capabilities":{"adapter":target,"version":"installed-package-test","entrypoint":"synthetic","events":{"tool.before":["observe","block"],"tool.after":["observe"]}},"cwd":work,"environment":{"PATH":"/usr/bin:/bin"}}),
        );
        let instance = &binding["instance_id"];
        for (event, code, expected) in [
            ("tool.before", "printf AW_PREVIEW_SAFE", "observe"),
            (
                "tool.before",
                "git -c http.sslVerify=false --version",
                "block",
            ),
            ("tool.after", "printf AW_PREVIEW_SAFE", "observe"),
        ] {
            let handle = rpc(
                &client,
                json!({"method":"open_event","instance_id":instance,"event":{"name":event,"agent":{"adapter":target,"binding_id":target,"instance_id":instance},"session_id":"installed-test","tool":{"name":tool,"native_name":tool,"call_id":null,"input":{"command":code},"result":if event=="tool.after" { json!({"text":"AW_PREVIEW_SAFE"}) } else { Value::Null }},"native":{}}}),
            );
            for step in handle["steps"].as_array().unwrap() {
                let output = rpc(
                    &client,
                    json!({"method":"invoke_step","event_id":handle["event_id"],"instance_id":instance,"step_id":step}),
                );
                assert_eq!(output["status"], "ok", "{output}");
                assert!(
                    output["effects"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|effect| effect["type"] == expected),
                    "{output}"
                );
            }
            rpc(
                &client,
                json!({"method":"close_event","event_id":handle["event_id"],"instance_id":instance}),
            );
            let audit = rpc(&client, json!({"method":"audit","key":handle["event_id"]}));
            assert_eq!(audit["terminal"], true);
            audits.push(handle["event_id"].clone());
        }
        rpc(&client, json!({"method":"unbind","instance_id":instance}));
    }
    rpc(&client, json!({"method":"stop"}));
    service.wait();
    drop(service);
    let mut service = Process::start(&aw, &args, &work, "aw-restart");
    wait_socket(&aw_socket, &mut service);
    let client = Client::connect(&aw_socket, Instant::now() + Duration::from_secs(5)).unwrap();
    assert_ne!(client.identity().generation, generation);
    for key in &audits {
        assert_eq!(
            rpc(&client, json!({"method":"audit","key":key}))["terminal"],
            true
        );
    }
    rpc(&client, json!({"method":"stop"}));
    service.wait();
    drop(service);
    drop(sec);
    assert!(!aw_socket.exists());
    fs::write(work.join("result.json"),serde_json::to_vec_pretty(&json!({"passed":true,"targets":["qoder","openclaw"],"native_agents":"not_tested","audit_survives_restart":true,"aw_socket_removed":true,"events":audits})).unwrap()).unwrap();
}
