//! Process-boundary regressions for issue #10315: a headless cosh-core whose
//! shell parent dies mid-turn, or whose stdout pipe breaks, must cancel the
//! provider request and exit instead of orphaning until the turn completes.
//! Parent death is triggered only after an observable proves the turn is in
//! flight, so loaded hosts cannot move the death outside the watched window.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use wait_timeout::ChildExt;

mod common;

const INIT: &str =
    r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#;
const USER: &str = r#"{"type":"user","message":{"role":"user","content":"write a long essay about mountains"},"parent_tool_use_id":null}"#;
// The wrapper backgrounds core keeping the test's stdin pipe on fd 3 (sh
// would otherwise redirect background stdin to /dev/null) and exits only
// once the test releases the trigger FIFO, i.e. after a witnessed in-flight
// turn. The release path must not exec anything: earlier versions polled with
// an external `sleep`, and an exec intercepted by a host security agent can
// stall for seconds inside fork/vfork, which pushed the wrapper past the exit
// deadline under load (#10360). Blocking builtin `read` and an immediate exit
// keep the path exec-free.
const WRAPPER: &str = "exec 3<&0; core=\"$1\"; pidfile=\"$2\"; trigger=\"$3\"; shift 3; \"$core\" \"$@\" <&3 & echo $! > \"$pidfile\"; read -r _ < \"$trigger\"; exit 0";

fn configure(home: &Path, address: SocketAddr) {
    let directory = home.join(".copilot-shell");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"
[ai.providers.test]
type = "openai"
api_key = "local-test"
base_url = "http://{address}/v1"
model = "slow-model"
"#
        ),
    )
    .unwrap();
    fs::write(directory.join("telemetry_disabled"), b"").unwrap();
}

fn slow_model_server(
    words: usize,
    interval: Duration,
) -> (SocketAddr, mpsc::Receiver<()>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buffer = [0_u8; 8192];
        assert!(stream.read(&mut buffer).unwrap() > 0);
        started_tx.send(()).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        for index in 0..words {
            let chunk = format!(
                "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"word{index} \"}},\"finish_reason\":null}}]}}\n\n"
            );
            if stream.write_all(chunk.as_bytes()).is_err() {
                return;
            }
            let _ = stream.flush();
            std::thread::sleep(interval);
        }
        let _ = stream.write_all(
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
        );
    });
    (address, started_rx, thread)
}

fn core_command(home: &Path, workspace: &Path) -> Command {
    let mut command = common::cosh_core_command(home);
    command
        .args(["--headless", "--workspace"])
        .arg(workspace)
        .env_remove("OPENAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .env_remove("OPENAI_BASE_URL");
    command
}

fn read_jsonl(dir: &Path, out: &mut String) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            read_jsonl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl")
            || path.to_string_lossy().ends_with(".jsonl.active")
        {
            out.push_str(&fs::read_to_string(&path).unwrap());
        }
    }
}

fn wait_for(deadline: Instant, mut predicate: impl FnMut() -> bool) -> bool {
    while Instant::now() < deadline {
        if predicate() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    predicate()
}

fn reap_exit(pid: i32, deadline: Instant) -> i32 {
    loop {
        match waitpid(Pid::from_raw(pid), Some(WaitPidFlag::WNOHANG)).unwrap() {
            WaitStatus::Exited(_, code) => return code,
            WaitStatus::StillAlive => {
                assert!(
                    Instant::now() < deadline,
                    "core did not exit after parent death"
                );
                std::thread::sleep(Duration::from_millis(100));
            }
            other => panic!("unexpected wait status: {other:?}"),
        }
    }
}

fn spawn_wrapper(
    home: &Path,
    workspace: &Path,
    audit_dir: Option<&Path>,
    stdout_log: Option<&Path>,
    stderr_log: Option<&Path>,
) -> (std::process::Child, PathBuf, PathBuf) {
    let pidfile = home.join("core.pid");
    let trigger = home.join("kill-now");
    nix::unistd::mkfifo(
        &trigger,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();
    let mut wrapper = Command::new("sh");
    wrapper
        .arg("-c")
        .arg(WRAPPER)
        .arg("sh")
        .arg(common::binary_path())
        .arg(&pidfile)
        .arg(&trigger)
        .arg("--headless")
        .arg("--workspace")
        .arg(workspace)
        .stdin(Stdio::piped())
        .stdout(match stdout_log {
            Some(path) => Stdio::from(fs::File::create(path).unwrap()),
            None => Stdio::piped(),
        });
    match stderr_log {
        Some(path) => wrapper.stderr(Stdio::from(fs::File::create(path).unwrap())),
        None => wrapper.stderr(Stdio::piped()),
    };
    common::opt_out_telemetry(&mut wrapper, home);
    wrapper.env("HOME", home);
    if let Some(dir) = audit_dir {
        wrapper.env("COSH_AUDIT_DIR", dir);
    }
    wrapper
        .env_remove("OPENAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .env_remove("OPENAI_BASE_URL");
    let child = wrapper.spawn().unwrap();
    (child, pidfile, trigger)
}

fn send_lines(child: &mut std::process::Child, lines: &[&str]) {
    let stdin = child.stdin.as_mut().unwrap();
    for line in lines {
        writeln!(stdin, "{line}").unwrap();
    }
    stdin.flush().unwrap();
}

fn core_pid(pidfile: &Path) -> i32 {
    fs::read_to_string(pidfile).unwrap().trim().parse().unwrap()
}

fn release_fifo(path: &Path) -> bool {
    use nix::fcntl::{open, OFlag};
    use nix::sys::stat::Mode;
    use std::os::unix::io::FromRawFd;
    let Ok(fd) = open(path, OFlag::O_WRONLY | OFlag::O_NONBLOCK, Mode::empty()) else {
        return false;
    };
    let mut file = unsafe { fs::File::from_raw_fd(fd) };
    file.write_all(b"release\n").is_ok()
}

fn release_trigger(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !release_fifo(path) {
        assert!(Instant::now() < deadline, "trigger FIFO has no reader");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn parent_death_midturn_cancels_turn() {
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let stderr_log = home.path().join("core.stderr.log");
    let (address, started_rx, server) = slow_model_server(60, Duration::from_millis(200));
    configure(home.path(), address);
    let (mut wrapper, pidfile, trigger) =
        spawn_wrapper(home.path(), workspace.path(), None, None, Some(&stderr_log));
    send_lines(&mut wrapper, &[INIT, USER]);
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("provider request started");
    release_trigger(&trigger);
    assert!(
        wrapper
            .wait_timeout(Duration::from_secs(5))
            .unwrap()
            .is_some(),
        "wrapper parent did not exit"
    );
    // Observe the core's own exit rather than EOF on the shell's stdout pipe:
    // a sibling test thread forking in this harness can inherit that write end
    // across its fork->exec window, and the EOF would then not arrive until
    // the sibling's child exits, unrelated to parent death (#10360).
    let exit_code = reap_exit(core_pid(&pidfile), Instant::now() + Duration::from_secs(10));
    assert_eq!(exit_code, 75, "expected EXIT_PARENT_LOST");
    let stderr = fs::read_to_string(&stderr_log).unwrap_or_default();
    assert!(
        stderr.contains("parent process lost"),
        "missing parent-lost diagnostic: {stderr}"
    );
    let _ = wrapper.wait();
    server.join().unwrap();
}

#[test]
fn stdout_epipe_exits_transport_failure() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let stderr_log = home.path().join("core.stderr.log");
    // No live server: the bound address is dropped so any provider dial is
    // refused; the assertion is about the broken stdout, not the provider.
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    configure(home.path(), address);
    // Close the read end before the core exists so the first emit must hit
    // EPIPE. With `Stdio::piped()` the harness briefly owns a reader, and a
    // sibling test thread forking in the shared harness can inherit it across
    // its fork->exec window: the emits then succeed, the EOF from
    // `wait_timeout` closing stdin ends the session, and the case exits 0
    // (#10360). A pipe with no reader has no such window.
    let (read_end, write_end) = nix::unistd::pipe().unwrap();
    drop(read_end);
    let mut child = core_command(home.path(), workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::from(fs::File::from(write_end)))
        .stderr(Stdio::from(fs::File::create(&stderr_log).unwrap()))
        .spawn()
        .unwrap();
    send_lines(&mut child, &[INIT, USER]);
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .unwrap()
        .expect("cosh-core did not exit after stdout EPIPE");
    assert_eq!(
        status.code(),
        Some(74),
        "expected EXIT_CONTROL_TRANSPORT_FAILURE"
    );
    let stderr = fs::read_to_string(&stderr_log).unwrap_or_default();
    assert!(
        stderr.contains("control transport emit failed"),
        "missing emit-failure diagnostic: {stderr}"
    );
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn normal_turn_completes_with_parent_alive() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let (address, _started_rx, server) = slow_model_server(3, Duration::from_millis(50));
    configure(home.path(), address);
    let mut child = core_command(home.path(), workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    send_lines(&mut child, &[INIT, USER]);
    drop(child.stdin.take().unwrap());
    let output = child
        .wait_timeout(Duration::from_secs(15))
        .unwrap()
        .map(|_| child.wait_with_output().unwrap())
        .expect("normal turn did not finish");
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("word0"), "stream text missing: {stdout}");
    assert!(
        stdout.contains("\"subtype\":\"success\""),
        "result missing: {stdout}"
    );
    server.join().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn parent_death_closed_pipes_exit_75_and_audit_cancel() {
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let audit_dir = tempfile::tempdir().unwrap();
    // The audit root safety check requires a private (0700) directory.
    fs::set_permissions(audit_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let (address, started_rx, server) = slow_model_server(60, Duration::from_millis(200));
    configure(home.path(), address);
    let (mut wrapper, pidfile, trigger) = spawn_wrapper(
        home.path(),
        workspace.path(),
        Some(audit_dir.path()),
        None,
        None,
    );
    send_lines(&mut wrapper, &[INIT, USER]);
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("provider request started");
    // The dying shell closes both output pipes; the cancel path must not
    // panic on stderr EPIPE and must still finalize the audit lifecycle.
    drop(wrapper.stdout.take().unwrap());
    drop(wrapper.stderr.take().unwrap());
    release_trigger(&trigger);
    assert!(
        wrapper
            .wait_timeout(Duration::from_secs(5))
            .unwrap()
            .is_some(),
        "wrapper parent did not exit"
    );
    let exit_code = reap_exit(core_pid(&pidfile), Instant::now() + Duration::from_secs(10));
    assert_eq!(exit_code, 75, "expected EXIT_PARENT_LOST, not a panic exit");
    assert!(
        !home
            .path()
            .join(".copilot-shell/cosh-core/sessions")
            .exists(),
        "aborted turn must not be persisted"
    );
    let mut events = String::new();
    read_jsonl(audit_dir.path(), &mut events);
    assert!(
        events.contains("session.ended"),
        "session.ended missing: {events}"
    );
    let mut turn_failed = 0;
    let mut provider_cancelled = 0;
    for line in events.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match value["event_type"].as_str() {
            Some("turn.failed") => {
                turn_failed += 1;
                assert_eq!(value["outcome"]["status"], "cancelled", "{line}");
                assert_eq!(value["data"]["reason_code"], "parent_lost", "{line}");
            }
            Some("provider.request.cancelled") => {
                provider_cancelled += 1;
                assert_eq!(value["outcome"]["status"], "cancelled", "{line}");
                assert_eq!(value["data"]["finish_category"], "parent_lost", "{line}");
            }
            _ => {}
        }
    }
    assert_eq!(
        turn_failed, 1,
        "turn.failed must be recorded once: {events}"
    );
    assert_eq!(
        provider_cancelled, 1,
        "provider.request.cancelled must be recorded once: {events}"
    );
    server.join().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn parent_death_between_turns_no_double_terminal() {
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let audit_dir = tempfile::tempdir().unwrap();
    fs::set_permissions(audit_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let (address, started_rx, server) = slow_model_server(3, Duration::from_millis(50));
    let directory = home.path().join(".copilot-shell");
    fs::create_dir_all(&directory).unwrap();
    let hook_marker = home.path().join("hook-started");
    let hook_fifo = home.path().join("hook-fifo");
    let hook_marker2 = home.path().join("hook2-started");
    assert!(Command::new("mkfifo")
        .arg(&hook_fifo)
        .status()
        .unwrap()
        .success());
    fs::write(
        directory.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"
[ai.providers.test]
type = "openai"
api_key = "local-test"
base_url = "http://{address}/v1"
model = "slow-model"
[hooks]
enabled = true
[[hooks.UserPromptSubmit]]
command = "if [ -f '{0}' ]; then touch '{2}'; read line < '{1}'; else touch '{0}'; fi"
name = "slow-hook"
"#,
            hook_marker.display(),
            hook_fifo.display(),
            hook_marker2.display()
        ),
    )
    .unwrap();
    fs::write(directory.join("telemetry_disabled"), b"").unwrap();
    let stdout_log = home.path().join("core.stdout.log");
    let stderr_log = home.path().join("core.stderr.log");
    let (mut wrapper, pidfile, trigger) = spawn_wrapper(
        home.path(),
        workspace.path(),
        Some(audit_dir.path()),
        Some(&stdout_log),
        Some(&stderr_log),
    );
    send_lines(&mut wrapper, &[INIT, USER]);
    started_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("first provider request started");
    assert!(
        wait_for(Instant::now() + Duration::from_secs(10), || {
            fs::read_to_string(&stdout_log)
                .map(|s| s.contains("\"type\":\"result\""))
                .unwrap_or(false)
        }),
        "first turn did not complete; stdout: {}",
        fs::read_to_string(&stdout_log).unwrap_or_default()
    );
    send_lines(&mut wrapper, &[USER]);
    assert!(
        wait_for(Instant::now() + Duration::from_secs(10), || hook_marker2
            .exists()),
        "second turn hook did not start"
    );
    release_trigger(&trigger);
    assert!(
        wrapper
            .wait_timeout(Duration::from_secs(5))
            .unwrap()
            .is_some(),
        "wrapper parent did not exit"
    );
    let exit_code = reap_exit(core_pid(&pidfile), Instant::now() + Duration::from_secs(10));
    assert_eq!(exit_code, 75, "expected EXIT_PARENT_LOST");
    let mut events = String::new();
    read_jsonl(audit_dir.path(), &mut events);
    assert!(
        events.contains("session.ended"),
        "session.ended missing: {events}"
    );
    let mut completed_turns: Vec<String> = Vec::new();
    let mut failed_turns: Vec<String> = Vec::new();
    for line in events.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let turn = value["identity"]["turn_id"].as_str().unwrap_or_default();
        match value["event_type"].as_str() {
            Some("turn.completed") => completed_turns.push(turn.to_string()),
            Some("turn.failed") => failed_turns.push(turn.to_string()),
            _ => {}
        }
    }
    assert_eq!(
        completed_turns.len(),
        1,
        "first turn must complete once: {events}"
    );
    assert!(
        failed_turns.is_empty(),
        "no turn may receive a terminal after completion: {events}"
    );
    let _ = release_fifo(&hook_fifo);
    server.join().unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn parent_death_audit_rejected_provider_not_cancelled() {
    nix::sys::prctl::set_child_subreaper(true).unwrap();
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let audit_dir = tempfile::tempdir().unwrap();
    fs::set_permissions(audit_dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let (address, started_rx, server) = slow_model_server(60, Duration::from_millis(200));
    let directory = home.path().join(".copilot-shell");
    fs::create_dir_all(&directory).unwrap();
    let hook_marker = home.path().join("hook-started");
    let hook_fifo = home.path().join("hook-fifo");
    let hook_marker2 = home.path().join("hook2-started");
    assert!(Command::new("mkfifo")
        .arg(&hook_fifo)
        .status()
        .unwrap()
        .success());
    fs::write(
        directory.join("config.toml"),
        format!(
            r#"
[ai]
active_provider = "test"
[ai.providers.test]
type = "openai"
api_key = "local-test"
base_url = "http://{address}/v1"
model = "slow-model"
[audit]
mode = "required"
[hooks]
enabled = true
[[hooks.UserPromptSubmit]]
command = "if [ -f '{0}' ]; then touch '{2}'; read line < '{1}'; else touch '{0}'; fi"
name = "slow-hook"
"#,
            hook_marker.display(),
            hook_fifo.display(),
            hook_marker2.display()
        ),
    )
    .unwrap();
    fs::write(directory.join("telemetry_disabled"), b"").unwrap();
    let stdout_log = home.path().join("core.stdout.log");
    let stderr_log = home.path().join("core.stderr.log");
    let (mut wrapper, pidfile, trigger) = spawn_wrapper(
        home.path(),
        workspace.path(),
        Some(audit_dir.path()),
        Some(&stdout_log),
        Some(&stderr_log),
    );
    send_lines(&mut wrapper, &[INIT]);
    let mut segments = Vec::new();
    fn find_active(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                find_active(&path, out);
            } else if path.to_string_lossy().ends_with(".jsonl.active") {
                out.push(path);
            }
        }
    }
    assert!(
        wait_for(Instant::now() + Duration::from_secs(10), || {
            segments.clear();
            find_active(audit_dir.path(), &mut segments);
            segments.len() == 1
        }),
        "active audit segment did not appear"
    );
    fs::set_permissions(&segments[0], fs::Permissions::from_mode(0o644)).unwrap();
    send_lines(&mut wrapper, &[USER]);
    assert!(
        wait_for(Instant::now() + Duration::from_secs(10), || {
            fs::read_to_string(&stdout_log)
                .map(|s| s.contains("\"is_error\":true"))
                .unwrap_or(false)
        }),
        "audit barrier rejection did not surface as an error result"
    );
    fs::set_permissions(&segments[0], fs::Permissions::from_mode(0o600)).unwrap();
    // A subsequent turn keeps the core in flight (slow UserPromptSubmit hook)
    // so parent loss runs finalize_parent_lost with whatever tracking the
    // rejected turn left behind.
    send_lines(&mut wrapper, &[USER]);
    assert!(
        wait_for(Instant::now() + Duration::from_secs(10), || hook_marker2
            .exists()),
        "second turn hook did not start"
    );
    release_trigger(&trigger);
    assert!(
        wrapper
            .wait_timeout(Duration::from_secs(5))
            .unwrap()
            .is_some(),
        "wrapper parent did not exit"
    );
    let exit_code = reap_exit(core_pid(&pidfile), Instant::now() + Duration::from_secs(10));
    assert_eq!(exit_code, 75, "expected EXIT_PARENT_LOST");
    let stderr = fs::read_to_string(&stderr_log).unwrap_or_default();
    assert!(
        stderr.contains("parent process lost"),
        "missing parent-lost diagnostic: {stderr}"
    );
    assert!(
        started_rx.recv_timeout(Duration::from_secs(1)).is_err(),
        "provider must receive zero connections when the audit barrier rejects"
    );
    let mut events = String::new();
    read_jsonl(audit_dir.path(), &mut events);
    assert!(
        !events.contains("provider.request.cancelled"),
        "rejected provider request must not gain a cancelled terminal: {events}"
    );
    assert!(
        !events.contains("parent_lost"),
        "parent loss must not append terminals for the rejected turn: {events}"
    );
    assert!(
        events.contains("session.ended"),
        "session.ended missing: {events}"
    );
    let _ = release_fifo(&hook_fifo);
    std::mem::forget(server);
}
