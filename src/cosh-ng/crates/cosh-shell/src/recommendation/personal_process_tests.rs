use std::collections::VecDeque;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::recommendation::personal_runner::{
    run_initialized, InitializeResult, ProcessFailure, RunnerCommand,
};

fn fixture(script: &str) -> RunnerCommand {
    RunnerCommand {
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        env: Vec::new(),
        cwd: PathBuf::from("/tmp"),
    }
}

#[derive(Clone, Copy)]
enum WriteAction {
    Zero,
    Partial(usize),
    WouldBlock,
}

struct ScriptedDeadlineWriter {
    actions: VecDeque<WriteAction>,
    wait_ready: bool,
    flush_would_block: bool,
}

impl Write for ScriptedDeadlineWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.actions.pop_front().unwrap_or(WriteAction::WouldBlock) {
            WriteAction::Zero => Ok(0),
            WriteAction::Partial(count) => Ok(count.min(bytes.len())),
            WriteAction::WouldBlock => Err(io::ErrorKind::WouldBlock.into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.flush_would_block {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(())
        }
    }
}

impl DeadlineWriter for ScriptedDeadlineWriter {
    fn wait_writable(&self, _timeout: Duration) -> io::Result<bool> {
        Ok(self.wait_ready)
    }
}

#[test]
fn deadline_write_distinguishes_zero_and_partial_transport_failure() {
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut zero = ScriptedDeadlineWriter {
        actions: VecDeque::from([WriteAction::Zero]),
        wait_ready: true,
        flush_would_block: false,
    };
    assert_eq!(
        write_all_before(&mut zero, b"body", deadline),
        Err(ProcessFailure::Transport)
    );

    let mut partial = ScriptedDeadlineWriter {
        actions: VecDeque::from([WriteAction::Partial(2), WriteAction::Zero]),
        wait_ready: true,
        flush_would_block: false,
    };
    assert_eq!(
        write_all_before(&mut partial, b"body", deadline),
        Err(ProcessFailure::TransportAfterWrite)
    );
}

#[test]
fn deadline_write_times_out_when_stdin_stays_blocked() {
    let mut blocked = ScriptedDeadlineWriter {
        actions: VecDeque::from([WriteAction::WouldBlock]),
        wait_ready: false,
        flush_would_block: false,
    };

    assert_eq!(
        write_all_before(
            &mut blocked,
            b"body",
            Instant::now() + Duration::from_millis(10),
        ),
        Err(ProcessFailure::Timeout)
    );
}

#[test]
fn deadline_write_after_partial_bytes_is_conservatively_sent() {
    let mut blocked = ScriptedDeadlineWriter {
        actions: VecDeque::from([WriteAction::Partial(2), WriteAction::WouldBlock]),
        wait_ready: false,
        flush_would_block: false,
    };

    assert_eq!(
        write_all_before(
            &mut blocked,
            b"body",
            Instant::now() + Duration::from_millis(10),
        ),
        Err(ProcessFailure::TimeoutAfterWrite)
    );
}

#[test]
fn deadline_flush_times_out_when_stdin_stays_blocked() {
    let mut blocked = ScriptedDeadlineWriter {
        actions: VecDeque::from([WriteAction::Partial(4)]),
        wait_ready: false,
        flush_would_block: true,
    };

    assert_eq!(
        write_all_before(
            &mut blocked,
            b"body",
            Instant::now() + Duration::from_millis(10),
        ),
        Err(ProcessFailure::TimeoutAfterWrite)
    );
}

#[test]
fn maps_core_jsonl_without_exposing_stderr() {
    let script = r#"
        IFS= read -r init
        case "$init" in *'"protocol_version":1'*) ;; *) exit 2 ;; esac
        printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"recommendation-init","response":{"subtype":"initialize","capabilities":{}}}}'
        printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1","model":"m","tools":[]}'
        IFS= read -r user
        case "$user" in *activity-secret*) ;; *) exit 3 ;; esac
        printf '%s\n' 'stderr-secret' >&2
        printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"{\"ok\":"}}}'
        printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"true}"}}}'
        printf '%s\n' '{"type":"assistant","session_id":"s1","message":{"content":[{"type":"text","text":"{\"ok\":true}"}]}}'
        printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"result":"completed","session_id":"s1"}'
        IFS= read -r shutdown
    "#;
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(script)).expect("spawn fixture");

    let output = run_initialized(&mut process, "activity-secret").expect("valid protocol");

    assert_eq!(output, "{\"ok\":true}");
}

#[test]
fn activity_body_is_stdin_only_and_absent_from_cancel_artifacts() {
    let sentinel = "ACTIVITY_SENTINEL_7f49d2";
    let root = std::env::temp_dir().join(format!(
        "cosh-analyzer-stdin-only-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    fs::create_dir(&root).expect("create fixture directory");
    let argv_path = root.join("argv.txt");
    let env_path = root.join("env.txt");
    let script = format!(
        r#"
            printf '%s\n' "$0" "$@" > '{argv}'
            env > '{env}'
            IFS= read -r init
            printf '%s\n' '{{"type":"control_response","response":{{"subtype":"success","request_id":"recommendation-init","response":{{"subtype":"initialize","capabilities":{{}}}}}}}}'
            printf '%s\n' '{{"type":"system","subtype":"init","session_id":"s1","model":"m","tools":[]}}'
            IFS= read -r user
            printf '%s\n' '{{"type":"control_request","request_id":"q1","request":{{"subtype":"ask_user","question":"continue?","options":[],"allow_free_text":true,"multi_select":false}}}}'
            sleep 30
        "#,
        argv = argv_path.display(),
        env = env_path.display(),
    );
    let mut command = fixture(&script);
    command.cwd = root.clone();
    let mut process = CoshCoreAnalyzerProcess::spawn(command).expect("spawn fixture");
    let process_arguments = process_arguments(process.child.id());

    assert!(!process_arguments
        .windows(sentinel.len())
        .any(|window| window == sentinel.as_bytes()));

    assert_eq!(
        run_initialized(&mut process, sentinel),
        Err(
            crate::recommendation::personal_runner::RunnerError::InteractiveEvent {
                body_sent: true
            }
        )
    );

    for entry in fs::read_dir(&root).expect("read fixture artifacts") {
        let path = entry.expect("artifact").path();
        let bytes = fs::read(&path).expect("read artifact");
        assert!(
            !bytes
                .windows(sentinel.len())
                .any(|window| window == sentinel.as_bytes()),
            "sentinel leaked to {}",
            path.display()
        );
    }
    fs::remove_dir_all(root).expect("remove fixture directory");
}

#[test]
fn verified_group_guard_rejects_owner_parent_start_and_pgid_mismatches() {
    let process =
        CoshCoreAnalyzerProcess::spawn(fixture("sleep 30")).expect("spawn guarded process");
    let identity = ProcessGroupIdentity {
        owner_pid: process.owner_pid,
        owner_start_identity: process
            .owner_start_identity
            .clone()
            .expect("owner start identity"),
        leader_pid: process.child.id(),
        leader_start_identity: process
            .leader_start_identity
            .clone()
            .expect("leader start identity"),
        process_group_id: process.process_group_id,
    };
    assert!(process_group_identity_matches(&identity));

    assert!(verified_terminate_process_group(&identity));
    assert!(!group_has_live_members(identity.process_group_id));

    let mut process =
        CoshCoreAnalyzerProcess::spawn(fixture("sleep 30")).expect("spawn guarded process");
    let identity = ProcessGroupIdentity {
        owner_pid: process.owner_pid,
        owner_start_identity: process
            .owner_start_identity
            .clone()
            .expect("owner start identity"),
        leader_pid: process.child.id(),
        leader_start_identity: process
            .leader_start_identity
            .clone()
            .expect("leader start identity"),
        process_group_id: process.process_group_id,
    };

    let mut wrong_owner = identity.clone();
    wrong_owner.owner_pid = wrong_owner.owner_pid.saturating_add(1);
    assert!(!verified_terminate_process_group(&wrong_owner));
    let mut wrong_start = identity.clone();
    wrong_start.leader_start_identity.push_str("-reused");
    assert!(!verified_terminate_process_group(&wrong_start));
    let mut wrong_group = identity.clone();
    wrong_group.process_group_id = wrong_group.process_group_id.saturating_add(1);
    assert!(!verified_terminate_process_group(&wrong_group));
    assert!(process.child.try_wait().expect("child state").is_none());

    process.cancel();
}

#[cfg(target_os = "linux")]
fn process_arguments(pid: u32) -> Vec<u8> {
    fs::read(format!("/proc/{pid}/cmdline")).expect("read process cmdline")
}

#[cfg(target_os = "macos")]
fn process_arguments(pid: u32) -> Vec<u8> {
    let output = Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .expect("read process command");
    assert!(output.status.success());
    output.stdout
}

#[test]
fn requires_initialize_success_before_system_init() {
    let script = r#"
        IFS= read -r init
        printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1","model":"m","tools":[]}'
        sleep 2
    "#;
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(script)).expect("spawn fixture");

    assert_eq!(
        process.initialize(Duration::from_millis(50)),
        Err(ProcessFailure::Timeout)
    );
    process.cancel();
}

#[test]
fn rejects_failed_initialize_response_immediately() {
    let script = r#"
        IFS= read -r init
        printf '%s\n' '{"type":"control_response","response":{"subtype":"error","request_id":"recommendation-init","response":{"subtype":"initialize","capabilities":{}}}}'
        sleep 2
    "#;
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(script)).expect("spawn fixture");

    assert_eq!(
        process.initialize(Duration::from_secs(1)),
        Err(ProcessFailure::Transport)
    );
    process.cancel();
}

#[test]
fn rejects_unsupported_initialize_version_immediately() {
    let script = r#"
        IFS= read -r init
        printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"recommendation-init","response":{"subtype":"initialize","protocol_version":9,"capabilities":{}}}}'
        printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1","model":"m","tools":[]}'
        sleep 2
    "#;
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(script)).expect("spawn fixture");

    assert_eq!(
        process.initialize(Duration::from_secs(1)),
        Err(ProcessFailure::Transport)
    );
    process.cancel();
}

#[test]
fn maps_auth_before_body_and_interactive_events_after_body() {
    let auth_script = r#"
        IFS= read -r init
        printf '%s\n' '{"type":"control_request","request_id":"auth-1","request":{"subtype":"auth_required","reason":"not_configured","providers":[]}}'
        sleep 2
    "#;
    let mut auth =
        CoshCoreAnalyzerProcess::spawn(fixture(auth_script)).expect("spawn auth fixture");
    assert_eq!(
        auth.initialize(Duration::from_secs(1)),
        Ok(InitializeResult::AuthRequired)
    );
    auth.cancel();

    let question_script = r#"
        IFS= read -r init
        printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"recommendation-init","response":{"subtype":"initialize","capabilities":{}}}}'
        printf '%s\n' '{"type":"system","subtype":"init","session_id":"s1","model":"m","tools":[]}'
        IFS= read -r user
        printf '%s\n' '{"type":"control_request","request_id":"q1","request":{"subtype":"ask_user","question":"continue?","options":[],"allow_free_text":true,"multi_select":false}}'
        sleep 2
    "#;
    let mut question =
        CoshCoreAnalyzerProcess::spawn(fixture(question_script)).expect("spawn question fixture");
    assert_eq!(
        run_initialized(&mut question, "body"),
        Err(
            crate::recommendation::personal_runner::RunnerError::InteractiveEvent {
                body_sent: true
            }
        )
    );
}

#[test]
fn rejects_oversized_jsonl_line() {
    let script = r#"
        IFS= read -r init
        head -c 140000 /dev/zero | tr '\000' x
        printf '\n'
        sleep 2
    "#;
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(script)).expect("spawn fixture");

    assert_eq!(
        process.initialize(Duration::from_secs(1)),
        Err(ProcessFailure::Transport)
    );
    process.cancel();
}

#[test]
fn rejects_oversized_total_stdout() {
    let script = r#"
        IFS= read -r init
        i=0
        while [ "$i" -lt 14 ]; do
            printf '"'
            head -c 20000 /dev/zero | tr '\000' x
            printf '"\n'
            i=$((i + 1))
        done
        sleep 2
    "#;
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(script)).expect("spawn fixture");

    assert_eq!(
        process.initialize(Duration::from_secs(3)),
        Err(ProcessFailure::Transport)
    );
    process.cancel();
}

#[test]
fn cancel_kills_the_process_group_after_one_second_grace() {
    let marker = std::env::temp_dir().join(format!(
        "cosh-process-cancel-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    let script = format!(
        "trap 'printf term > {0}' TERM; printf ready > {0}; (trap '' TERM; sleep 30) & wait",
        marker.display()
    );
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(&script)).expect("spawn fixture");
    let process_group = process.process_group_id();
    let ready_deadline = Instant::now() + Duration::from_secs(1);
    while !marker.exists() && Instant::now() < ready_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists());

    process.cancel();

    assert_eq!(
        fs::read_to_string(&marker).expect("read TERM marker"),
        "term"
    );
    let reaped_deadline = Instant::now() + Duration::from_secs(1);
    while group_has_live_members(process_group) && Instant::now() < reaped_deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!group_has_live_members(process_group));
    let _ = fs::remove_file(marker);
}

#[test]
fn cancel_does_not_signal_when_leader_identity_mismatches() {
    let marker = std::env::temp_dir().join(format!(
        "cosh-process-identity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    let script = format!("trap 'printf term > {0}' TERM; sleep 30", marker.display());
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(&script)).expect("spawn fixture");
    process.leader_start_identity = process.leader_start_identity.take().map(different_identity);
    assert!(process.leader_start_identity.is_some());

    process.cancel();

    assert!(!marker.exists());
    process.child.kill().expect("kill fixture");
    process.child.wait().expect("wait fixture");
}

#[test]
fn cancel_reaps_child_when_process_identity_is_unavailable() {
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture("sleep 30")).expect("spawn fixture");
    process.owner_start_identity = None;

    process.cancel();

    assert!(process.child.try_wait().expect("poll fixture").is_some());
}

#[test]
fn cancel_returns_without_signalling_after_leader_exits() {
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture("exit 0")).expect("spawn fixture");
    let deadline = Instant::now() + Duration::from_secs(5);
    while process.child.try_wait().expect("poll fixture").is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(process.child.try_wait().expect("poll fixture").is_some());

    let started = Instant::now();
    process.cancel();

    assert!(started.elapsed() < Duration::from_millis(100));
}

#[cfg(target_os = "linux")]
#[test]
fn leader_exit_does_not_release_live_process_group_and_cancel_reaps_child() {
    let child_file = std::env::temp_dir().join(format!(
        "cosh-analyzer-child-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    let script = format!(
        "(trap '' HUP TERM; sleep 30) & printf '%s' $! > {}; exit 0",
        child_file.display()
    );
    let mut process = CoshCoreAnalyzerProcess::spawn(fixture(&script)).expect("spawn fixture");
    let identity = ProcessGroupIdentity {
        owner_pid: process.owner_pid,
        owner_start_identity: process
            .owner_start_identity
            .clone()
            .expect("owner start identity"),
        leader_pid: process.child.id(),
        leader_start_identity: process
            .leader_start_identity
            .clone()
            .expect("leader start identity"),
        process_group_id: process.process_group_id,
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    while (!child_file.exists() || process.child.try_wait().expect("poll leader").is_none())
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(process.child.try_wait().expect("poll leader").is_some());
    assert!(group_has_live_members(identity.process_group_id));

    let reported_gone = analyzer_process_is_gone(&identity);
    if reported_gone {
        unsafe {
            nix::libc::kill(-(identity.process_group_id as i32), nix::libc::SIGKILL);
        }
    }
    assert!(!reported_gone);

    process.cancel();
    let still_live = group_has_live_members(identity.process_group_id);
    if still_live {
        unsafe {
            nix::libc::kill(-(identity.process_group_id as i32), nix::libc::SIGKILL);
        }
    }
    let _ = fs::remove_file(child_file);
    assert!(!still_live);
}

fn different_identity(identity: String) -> String {
    format!("{identity}-different")
}
