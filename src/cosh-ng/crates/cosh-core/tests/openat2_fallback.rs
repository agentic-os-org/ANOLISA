#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;
use serde_json::{json, Value};

mod common;

const FILE_TOOLS: &[&str] = &[
    "read_file",
    "read_many_files",
    "list_directory",
    "glob",
    "grep",
    "write_file",
    "edit",
];

fn command(home: &std::path::Path, errno: Option<i32>) -> Command {
    let mut command = common::cosh_core_command(home);
    command
        .current_dir(home)
        .env("COSH_STATES_DIR", home.join("states"))
        .env_remove("COSH_AI_PROVIDER")
        .env_remove("OPENAI_BASE_URL")
        .args(["--headless", "--bare"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(errno) = errno {
        // SAFETY: the closure only installs a syscall filter in the forked
        // child. It neither allocates nor touches the parent process's state.
        unsafe {
            command.pre_exec(move || {
                let mut filter = [
                    libc::sock_filter {
                        code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                        jt: 0,
                        jf: 0,
                        // seccomp_data begins with the syscall number.
                        k: 0,
                    },
                    libc::sock_filter {
                        code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                        jt: 0,
                        jf: 1,
                        k: libc::SYS_openat2 as u32,
                    },
                    libc::sock_filter {
                        code: (libc::BPF_RET | libc::BPF_K) as u16,
                        jt: 0,
                        jf: 0,
                        k: libc::SECCOMP_RET_ERRNO | errno as u32,
                    },
                    libc::sock_filter {
                        code: (libc::BPF_RET | libc::BPF_K) as u16,
                        jt: 0,
                        jf: 0,
                        k: libc::SECCOMP_RET_ALLOW,
                    },
                ];
                let program = libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_mut_ptr(),
                };
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    command
}

fn run(command: &mut Command) -> Output {
    run_with_input(command, None)
}

fn run_with_input(command: &mut Command, input: Option<&str>) -> Output {
    let mut child = command.spawn().expect("spawn filtered cosh-core");
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("piped stdin");
        writeln!(stdin, "{input}").expect("write approval response");
    }
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if child.try_wait().expect("poll cosh-core").is_some() {
            return child.wait_with_output().expect("collect cosh-core output");
        }
        if Instant::now() >= deadline {
            child.kill().expect("kill timed out cosh-core");
            let output = child.wait_with_output().expect("reap timed out cosh-core");
            panic!(
                "cosh-core timed out: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn mock_config(home: &std::path::Path) {
    std::fs::write(
        home.join(".copilot-shell/config.toml"),
        "[ai]\nactive_provider = 'mock'\n[ai.providers.mock]\ntype = 'mock'\n",
    )
    .unwrap();
}

fn messages(output: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid JSONL"))
        .collect()
}

#[test]
fn missing_openat2_starts_and_answers_hello_without_file_tools() {
    for selection in [None, Some("shell,read_file,write_file,edit")] {
        let home = tempfile::tempdir().unwrap();
        let mut command = command(home.path(), Some(libc::ENOSYS));
        mock_config(home.path());
        command.arg("hello");
        if let Some(selection) = selection {
            command.args(["--tools", selection]);
        }
        let output = run(&mut command);
        assert!(output.status.success(), "{output:?}");
        let messages = messages(&output);
        let init = messages.iter().find(|m| m["subtype"] == "init").unwrap();
        let tools = init["tools"].as_array().unwrap();
        assert!(tools.contains(&json!("shell")));
        for name in FILE_TOOLS {
            assert!(!tools.contains(&json!(name)), "{init}");
        }
        assert!(messages.iter().any(|m| m["type"] == "assistant"));
        assert!(messages
            .iter()
            .any(|m| m["type"] == "result" && m["is_error"] == false));
        assert!(String::from_utf8_lossy(&output.stderr).contains("file tools are disabled"));
    }
}

#[test]
fn supported_openat2_keeps_file_tools() {
    let home = tempfile::tempdir().unwrap();
    let mut command = command(home.path(), None);
    mock_config(home.path());
    let output = run(command.arg("hello"));
    assert!(output.status.success(), "{output:?}");
    let messages = messages(&output);
    let init = messages.iter().find(|m| m["subtype"] == "init").unwrap();
    for name in FILE_TOOLS {
        assert!(
            init["tools"].as_array().unwrap().contains(&json!(name)),
            "{init}"
        );
    }
    assert!(!String::from_utf8_lossy(&output.stderr).contains("file tools are disabled"));
}

#[test]
fn permission_errors_still_fail_startup() {
    let home = tempfile::tempdir().unwrap();
    let output = run(&mut command(home.path(), Some(libc::EPERM)));
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to open workspace root"));
    assert!(output.stdout.is_empty());
}

#[test]
fn brokered_profiles_keep_their_startup_requirement() {
    for profile in [
        "gateway-brokered-v1",
        "gateway-brokered-checkpoint-v1",
        "gateway-brokered-workspace-write-v1",
    ] {
        let home = tempfile::tempdir().unwrap();
        let output =
            run(command(home.path(), Some(libc::ENOSYS)).args(["--execution-profile", profile]));
        assert_eq!(output.status.code(), Some(2), "{profile}: {output:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("require Linux openat2 support"));
        assert!(output.stdout.is_empty());
    }
}

fn serve(listener: TcpListener, shell_call: bool) -> Vec<Value> {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut requests = Vec::new();
    for turn in 0..if shell_call { 2 } else { 1 } {
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "provider request timed out");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("accept provider request: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(&mut stream);
        let mut length = None;
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                if key.eq_ignore_ascii_case("content-length") {
                    length = Some(value.trim().parse::<usize>().unwrap());
                }
            }
        }
        let mut body = vec![0; length.expect("provider content length")];
        reader.read_exact(&mut body).unwrap();
        requests.push(serde_json::from_slice(&body).unwrap());
        let delta = if shell_call && turn == 0 {
            json!({"tool_calls":[{"index":0,"id":"call_cat","type":"function",
                "function":{"name":"shell","arguments":"{\"command\":\"cat input.txt\"}"}}]})
        } else {
            json!({"content":"hello completed"})
        };
        let chunk = json!({"choices":[{"index":0,"delta":delta}]});
        let body = format!("data: {chunk}\n\ndata: [DONE]\n\n");
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    }
    requests
}

#[test]
fn provider_gets_guidance_and_can_read_via_shell() {
    for (shell_available, approval_mode) in [(true, "trust"), (true, "recommend"), (false, "trust")]
    {
        let home = tempfile::tempdir().unwrap();
        let mut command = command(home.path(), Some(libc::ENOSYS));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::fs::write(home.path().join("input.txt"), "legacy-kernel-file-content").unwrap();
        std::fs::write(
            home.path().join(".copilot-shell/config.toml"),
            format!("[ai]\nactive_provider = 'fixture'\n[ai.providers.fixture]\ntype = 'openai'\nbase_url = 'http://{address}/v1'\napi_key = 'fixture-key'\nmodel = 'fixture-model'\n"),
        ).unwrap();
        command.args(["--approval-mode", approval_mode, "hello"]);
        if !shell_available {
            command.args(["--tools", ""]);
        }
        let server = thread::spawn(move || serve(listener, shell_available));
        let output = if approval_mode == "recommend" {
            command.stdin(Stdio::piped());
            run_with_input(
                &mut command,
                Some(
                    r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-0","response":{"behavior":"deny","message":"fixture denial"}}}"#,
                ),
            )
        } else {
            run(&mut command)
        };
        let requests = server.join().expect("provider fixture thread");
        assert!(output.status.success(), "{output:?}");
        let prompt = requests[0]["messages"][0]["content"].as_str().unwrap();
        assert!(prompt.contains("Linux openat2 is unavailable"), "{prompt}");
        for name in FILE_TOOLS {
            assert!(prompt.contains(name), "{prompt}");
            assert!(!requests[0]["tools"].as_array().is_some_and(|tools| {
                tools.iter().any(|tool| tool["function"]["name"] == *name)
            }));
        }
        if shell_available {
            assert!(prompt.contains("Use the shell tool"));
            assert!(prompt.contains("approval policy and hooks"));
            let tool_result = requests[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|message| message["role"] == "tool")
                .expect("shell result returned to the provider");
            if approval_mode == "trust" {
                assert!(tool_result["content"]
                    .as_str()
                    .unwrap()
                    .contains("legacy-kernel-file-content"));
            } else {
                assert!(!tool_result["content"]
                    .as_str()
                    .unwrap()
                    .contains("legacy-kernel-file-content"));
                assert!(
                    messages(&output).iter().any(|message| {
                        message["type"] == "control_request"
                            && message["request"]["subtype"] == "can_use_tool"
                    }),
                    "{output:?}"
                );
            }
        } else {
            assert!(prompt.contains("shell tool is also unavailable"));
            assert!(!prompt.contains("Use the shell tool"));
        }
        assert!(messages(&output)
            .iter()
            .any(|m| m["type"] == "result" && m["is_error"] == false));
    }
}
