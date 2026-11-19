//! Process-boundary regressions for issue #10315: a headless cosh-core whose
//! shell parent dies mid-turn, or whose stdout pipe breaks, must cancel the
//! provider request and exit instead of orphaning until the turn completes.

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use wait_timeout::ChildExt;

mod common;

const INIT: &str =
    r#"{"type":"control_request","request_id":"init-1","request":{"subtype":"initialize"}}"#;
const USER: &str = r#"{"type":"user","message":{"role":"user","content":"write a long essay about mountains"},"parent_tool_use_id":null}"#;

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

fn kill_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGKILL,
    );
}

#[test]
fn parent_death_midturn_cancels_turn() {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let (address, started_rx, server) = slow_model_server(60, Duration::from_millis(200));
    configure(home.path(), address);
    let pidfile = home.path().join("core.pid");
    let stderr_log = home.path().join("core.stderr.log");
    // The wrapper backgrounds cosh-core and exits after one second, so the
    // kernel reparents core while the provider stream is still running.
    // fd 3 keeps the test's stdin pipe: sh otherwise redirects background
    // commands' stdin to /dev/null and core would see EOF immediately.
    let mut wrapper = Command::new("sh");
    wrapper
        .arg("-c")
        .arg("exec 3<&0; core=\"$1\"; pidfile=\"$2\"; shift 2; \"$core\" \"$@\" <&3 & echo $! > \"$pidfile\"; sleep 1; exit 0")
        .arg("sh")
        .arg(common::binary_path())
        .arg(&pidfile)
        .arg("--headless")
        .arg("--workspace")
        .arg(workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(fs::File::create(&stderr_log).unwrap()));
    common::opt_out_telemetry(&mut wrapper, home.path());
    wrapper
        .env("HOME", home.path())
        .env_remove("OPENAI_API_KEY")
        .env_remove("DASHSCOPE_API_KEY")
        .env_remove("OPENAI_BASE_URL");
    let mut wrapper = wrapper.spawn().unwrap();
    {
        let stdin = wrapper.stdin.as_mut().unwrap();
        writeln!(stdin, "{INIT}").unwrap();
        writeln!(stdin, "{USER}").unwrap();
        stdin.flush().unwrap();
    }
    if started_rx.recv_timeout(Duration::from_secs(10)).is_err() {
        let log = fs::read_to_string(&stderr_log).unwrap_or_default();
        let pid = fs::read_to_string(&pidfile).unwrap_or_default();
        let pid_num: u32 = pid.trim().parse().unwrap_or(0);
        kill_pid(pid_num);
        let mut out = String::new();
        if let Some(mut stdout) = wrapper.stdout.take() {
            let _ = stdout.read_to_string(&mut out);
        }
        panic!("provider request never started; core pid={pid}\nstdout:\n{out}\nstderr:\n{log}");
    }
    assert!(
        wrapper.wait_timeout(Duration::from_secs(5)).unwrap().is_some(),
        "wrapper parent did not exit"
    );
    let orphaned_at = Instant::now();
    let core_pid: u32 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Stdout EOF is the exit proof: the wrapper is dead, so core holds the
    // last write end, and a zombie still reaped slowly would keep kill(pid,0)
    // alive past the actual exit.
    let stdout = wrapper.stdout.take().unwrap();
    let (eof_tx, eof_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut sink = Vec::new();
        let _ = stdout.read_to_end(&mut sink);
        let _ = eof_tx.send(());
    });
    let exited = eof_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    kill_pid(core_pid);
    assert!(
        exited,
        "cosh-core orphan kept stdout open >5s after parent death (elapsed {:?})",
        orphaned_at.elapsed()
    );
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
    // No live server: the bound address is dropped so any provider dial is
    // refused; the assertion is about the broken stdout, not the provider.
    let address = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    configure(home.path(), address);
    let stderr_log = home.path().join("core.stderr.log");
    let mut child = core_command(home.path(), workspace.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(fs::File::create(&stderr_log).unwrap()))
        .spawn()
        .unwrap();
    drop(child.stdout.take().unwrap());
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{INIT}").unwrap();
        writeln!(stdin, "{USER}").unwrap();
        stdin.flush().unwrap();
    }
    let status = child
        .wait_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap_or_else(|| {
            let log = fs::read_to_string(&stderr_log).unwrap_or_default();
            panic!("cosh-core still alive after stdout EPIPE; stderr:\n{log}");
        });
    let log = fs::read_to_string(&stderr_log).unwrap_or_default();
    assert_eq!(
        status.code(),
        Some(74),
        "expected EXIT_CONTROL_TRANSPORT_FAILURE; stderr:\n{log}"
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
    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, "{INIT}").unwrap();
        writeln!(stdin, "{USER}").unwrap();
        stdin.flush().unwrap();
    }
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
