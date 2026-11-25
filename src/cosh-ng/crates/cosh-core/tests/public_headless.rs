use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

mod common;
#[path = "public_headless/boundaries.rs"]
mod public_boundaries;

fn configure_mock(home: &std::path::Path) {
    configure_model(home, "mock-history");
}

fn configure_model(home: &std::path::Path, model: &str) {
    std::fs::create_dir_all(home.join(".copilot-shell")).expect("create config directory");
    std::fs::write(
        home.join(".copilot-shell/config.toml"),
        format!(
            r#"
[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"
model = "{model}"
"#,
        ),
    )
    .expect("write mock provider config");
}

fn public_command(home: &std::path::Path) -> Command {
    let mut command = common::cosh_core_command(home);
    command.args(["--headless", "--execution-profile=public-headless-v1"]);
    command.env_remove("COSH_AI_PROVIDER");
    command.env_remove("COSH_MODEL");
    command.env_remove("COSH_APPROVAL_MODE");
    command.env_remove("COSH_MAX_TURNS");
    command
}

fn run(home: &std::path::Path, args: &[&str], input: Option<&[u8]>) -> Output {
    run_with_env(home, args, input, &[])
}

fn run_with_env(
    home: &std::path::Path,
    args: &[&str],
    input: Option<&[u8]>,
    environment: &[(&str, &str)],
) -> Output {
    let mut child = public_command(home)
        .args(args)
        .envs(environment.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn public headless");
    if let Some(input) = input {
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(input)
            .expect("write stdin");
    }
    drop(child.stdin.take());
    child.wait_with_output().expect("wait for public headless")
}

#[test]
fn public_text_returns_only_the_final_answer() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    let output = run(home.path(), &["hello"], None);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        "mock history: hello\n"
    );
}

#[test]
fn public_json_projects_success_and_exit_zero() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    let output = run(home.path(), &["--output=json", "hello"], None);

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("single JSON result");
    assert_eq!(result["schema_version"], 1);
    assert_eq!(result["status"], "completed");
    assert_eq!(result["error_code"], serde_json::Value::Null);
    assert_eq!(result["answer"], "mock history: hello");
}

#[test]
fn public_stdin_dash_reads_the_complete_prompt() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    let output = run(home.path(), &["-"], Some(b"first line\nsecond line"));

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        "mock history: first line\nsecond line\n"
    );
}

#[test]
fn public_missing_credentials_return_auth_required() {
    let home = tempfile::tempdir().expect("temp home");
    common::cosh_core_command(home.path());

    let output = run(home.path(), &["--output=json", "hello"], None);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("single JSON result");
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error_code"], "auth_required");
}

#[test]
fn public_rejects_empty_and_invalid_budget_input() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    for args in [
        &[""][..],
        &["--max-turns=0", "hello"][..],
        &["--timeout=forever", "hello"][..],
    ] {
        let output = run(home.path(), args, None);
        assert_eq!(output.status.code(), Some(2), "args {args:?}: {output:?}");
    }
}

#[test]
fn legacy_core_rejects_public_only_options() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    for option in [
        "--output=json",
        "--file-tools=read",
        "--exec=host",
        "--max-turns=1",
        "--timeout=1s",
    ] {
        let output = common::cosh_core_command(home.path())
            .args(["--headless", option, "hello"])
            .stdin(Stdio::null())
            .output()
            .expect("run legacy Core with public option");

        assert_eq!(output.status.code(), Some(2), "option {option}: {output:?}");
        assert!(output.stdout.is_empty(), "option {option}: {output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("public cosh --headless"),
            "option {option}: {output:?}"
        );
    }
}

#[test]
fn public_rejects_oversized_and_non_utf8_stdin() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    let oversized = run(
        home.path(),
        &["--output=json", "-"],
        Some(&vec![b'x'; 1024 * 1024 + 1]),
    );
    assert_eq!(oversized.status.code(), Some(2), "{oversized:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&oversized.stdout).expect("oversized JSON result");
    assert_eq!(result["error_code"], "input_too_large");

    let invalid = run(home.path(), &["--output=json", "-"], Some(&[0xff]));
    assert_eq!(invalid.status.code(), Some(2), "{invalid:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&invalid.stdout).expect("invalid UTF-8 JSON result");
    assert_eq!(result["error_code"], "invalid_input");
}

#[test]
fn public_provider_failure_is_not_a_success() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        r#"
[ai]
active_provider = "mock"

[ai.providers.mock]
type = "mock"
model = "mock-partial-error"
"#,
    )
    .expect("write failing provider config");

    let output = run(home.path(), &["--output=json", "hello"], None);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("provider failure JSON result");
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error_code"], "provider_error");
}

#[test]
fn public_timeout_covers_a_silent_provider() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind silent provider");
    let address = listener.local_addr().expect("provider address");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept provider request");
        let mut buffer = [0; 1024];
        let _ = stream.read(&mut buffer);
        std::thread::sleep(Duration::from_secs(3));
    });
    let home = tempfile::tempdir().expect("temp home");
    std::fs::create_dir_all(home.path().join(".copilot-shell")).expect("create config directory");
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        format!(
            "[ai]\nactive_provider = 'silent'\n[ai.providers.silent]\ntype = 'openai'\nbase_url = 'http://{address}/v1'\napi_key = 'fixture'\nmodel = 'fixture'\n"
        ),
    )
    .expect("write silent provider config");

    let started = Instant::now();
    let output = run(
        home.path(),
        &["--output=json", "--timeout=1s", "hello"],
        None,
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("timeout JSON result");
    assert_eq!(result["error_code"], "timeout");
    server.join().expect("silent provider thread");
}

#[test]
fn public_prompt_does_not_wait_for_stdin_eof() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());
    let mut child = public_command(home.path())
        .arg("hello")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn with open stdin");
    let stdin = child.stdin.take().expect("open stdin");
    let output = child.wait_with_output().expect("prompt ignores stdin");

    drop(stdin);
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

#[test]
fn public_sigint_does_not_return_success() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind slow provider");
    let address = listener.local_addr().expect("provider address");
    let (connected_tx, connected_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let _ = listener.accept();
        connected_tx.send(()).expect("report provider connection");
        std::thread::sleep(Duration::from_secs(2));
    });
    let home = tempfile::tempdir().expect("temp home");
    std::fs::create_dir_all(home.path().join(".copilot-shell")).expect("create config directory");
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        format!(
            "[ai]\nactive_provider = 'slow'\n[ai.providers.slow]\ntype = 'openai'\nbase_url = 'http://{address}/v1'\napi_key = 'fixture'\nmodel = 'fixture'\n"
        ),
    )
    .expect("write slow provider config");
    let child = public_command(home.path())
        .args(["--timeout=20s", "hello"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn slow public headless");
    connected_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("provider connected before SIGINT");
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(child.id() as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .expect("send SIGINT");
    let output = child.wait_with_output().expect("collect interrupted run");

    assert_ne!(output.status.code(), Some(0), "{output:?}");
    server.join().expect("slow provider thread");
}

#[test]
fn public_skips_optional_runtime_startup() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let hook_marker = workspace.path().join("hook-started");
    let mcp_marker = workspace.path().join("mcp-started");
    std::fs::create_dir_all(home.path().join(".copilot-shell")).expect("create config directory");
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        format!(
            r#"
[ai]
active_provider = "mock"
[ai.providers.mock]
type = "mock"
[hooks]
enabled = true
[[hooks.SessionStart]]
command = "touch {}"
[mcp.servers.sentinel]
command = "sh"
args = ["-c", "touch {}; sleep 30"]
"#,
            hook_marker.display(),
            mcp_marker.display()
        ),
    )
    .expect("write optional runtime config");

    let output = run(
        home.path(),
        &[
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "hello",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!hook_marker.exists());
    assert!(!mcp_marker.exists());
}

#[test]
fn public_write_mode_makes_a_real_workspace_diff() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-write");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=write",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "write the requested file",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("headless-note.txt"))
            .expect("written workspace file"),
        "fixed by public headless\n"
    );
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("write JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("Wrote")));
}

#[test]
fn public_read_mode_cannot_write() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-write");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=read",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "try the write",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!workspace.path().join("headless-note.txt").exists());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("denial JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("denied")));
}

#[test]
fn public_file_tools_require_an_existing_workspace() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());

    let missing = run(
        home.path(),
        &["--output=json", "--file-tools=read", "read"],
        None,
    );
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("workspace input result");
    assert_eq!(result["error_code"], "invalid_input");

    let absent = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=read",
            "--workspace",
            "/definitely/absent/cosh-headless-workspace",
            "read",
        ],
        None,
    );
    assert_eq!(absent.status.code(), Some(1), "{absent:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&absent.stdout).expect("workspace failure result");
    assert_eq!(result["error_code"], "workspace_unavailable");
}

#[test]
fn public_file_write_rejects_workspace_escape() {
    let root = tempfile::tempdir().expect("temp root");
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).expect("workspace");
    let home = tempfile::tempdir().expect("temp home");
    configure_model(home.path(), "mock-public-escape");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=write",
            "--workspace",
            workspace.to_str().expect("workspace"),
            "try the escape",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!root.path().join("escape-marker").exists());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("escape JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("workspace")));
}

#[test]
fn public_question_request_returns_user_input_required() {
    let home = tempfile::tempdir().expect("temp home");
    configure_model(home.path(), "mock-public-ask");

    let output = run(home.path(), &["--output=json", "need input"], None);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("question JSON result");
    assert_eq!(result["error_code"], "user_input_required");
}

#[test]
fn public_max_turns_stops_a_tool_loop() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    std::fs::write(workspace.path().join("input.txt"), "fixture").expect("fixture");
    configure_model(home.path(), "mock-public-loop");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=read",
            "--max-turns=1",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "loop",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("max turns JSON result");
    assert_eq!(result["error_code"], "max_turns");
}

#[test]
fn public_task_tool_budget_is_cumulative() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    std::fs::write(workspace.path().join("input.txt"), "fixture").expect("fixture");
    configure_model(home.path(), "mock-public-loop");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=read",
            "--max-turns=50",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "loop",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("tool budget JSON result");
    assert_eq!(result["error_code"], "max_tool_calls");
}

#[test]
fn public_file_write_rejects_symlink_escape() {
    let root = tempfile::tempdir().expect("temp root");
    let workspace = root.path().join("workspace");
    let outside = root.path().join("outside");
    std::fs::create_dir(&workspace).expect("workspace");
    std::fs::create_dir(&outside).expect("outside");
    std::os::unix::fs::symlink(&outside, workspace.join("link")).expect("workspace symlink");
    let home = tempfile::tempdir().expect("temp home");
    configure_model(home.path(), "mock-public-symlink");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--file-tools=write",
            "--workspace",
            workspace.to_str().expect("workspace"),
            "try the symlink",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!outside.join("escape-marker").exists());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("symlink JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("workspace")));
}

#[test]
fn public_host_exec_returns_nonzero_output_to_the_model() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-failure");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run the failing fixture",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("command-started")).expect("command marker"),
        "started"
    );
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("command JSON result");
    let answer = result["answer"].as_str().expect("command answer");
    assert!(answer.contains("failure-output"), "{answer}");
    assert!(answer.contains("7"), "{answer}");
}

#[test]
fn public_exec_is_off_by_default_with_zero_target_start() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-failure");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "attempt the command",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(!workspace.path().join("command-started").exists());
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("denial JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("denied")));
}

#[test]
fn public_exec_requires_workspace_and_rejects_forced_sandbox() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());
    let missing = run(home.path(), &["--output=json", "--exec=host", "run"], None);
    assert_eq!(missing.status.code(), Some(2), "{missing:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&missing.stdout).expect("workspace JSON result");
    assert_eq!(result["error_code"], "invalid_input");

    let workspace = tempfile::tempdir().expect("temp workspace");
    let unauthenticated = tempfile::tempdir().expect("unauthenticated home");
    common::cosh_core_command(unauthenticated.path());
    let forbidden = run_with_env(
        unauthenticated.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run",
        ],
        None,
        &[("AGENT_SEC_COSH_SANDBOX_ALL_SHELL", "1")],
    );
    assert_eq!(forbidden.status.code(), Some(1), "{forbidden:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&forbidden.stdout).expect("sandbox JSON result");
    assert_eq!(result["error_code"], "execution_forbidden");
}

#[test]
fn public_invalid_explicit_audit_policy_fails_before_provider() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());
    let policy = home.path().join("invalid-audit.toml");
    std::fs::write(&policy, "not valid TOML").expect("invalid policy");

    let output = run_with_env(
        home.path(),
        &["--output=json", "hello"],
        None,
        &[("COSH_AUDIT_POLICY", policy.to_str().expect("policy path"))],
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("policy JSON result");
    assert_eq!(result["error_code"], "policy_unavailable");
}

fn read_pid(workspace: &std::path::Path, name: &str) -> i32 {
    std::fs::read_to_string(workspace.join(name))
        .expect("PID file")
        .trim()
        .parse()
        .expect("numeric PID")
}

fn assert_not_running(pid: i32) {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
    let stopped = match stat {
        Ok(content) => content.ends_with(") Z\n"),
        Err(_) => true,
    };
    assert!(stopped, "process {pid} is still running");
}

#[test]
fn public_command_reaps_background_group_after_normal_exit() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-background");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "leave a background child",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_not_running(read_pid(workspace.path(), "background.pid"));
}

#[test]
fn public_command_timeout_kills_its_process_group() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-timeout");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "time out the command",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("command timeout JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("timed out")));
    assert_not_running(read_pid(workspace.path(), "timeout-child.pid"));
}

#[test]
fn public_task_timeout_cancels_the_command_group() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-long");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--timeout=1s",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "cancel the long command",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("task timeout JSON result");
    assert_eq!(result["error_code"], "timeout");
    assert_not_running(read_pid(workspace.path(), "long-child.pid"));
}

#[test]
fn public_in_band_question_returns_user_input_required() {
    let home = tempfile::tempdir().expect("temp home");
    configure_model(home.path(), "mock-public-in-band-ask");

    let output = run(home.path(), &["--output=json", "need input"], None);

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("in-band question JSON result");
    assert_eq!(result["error_code"], "user_input_required");
}

#[test]
fn public_json_output_limit_still_returns_a_result_object() {
    let home = tempfile::tempdir().expect("temp home");
    configure_mock(home.path());
    std::fs::write(
        home.path().join(".copilot-shell/config.toml"),
        r#"
[ai]
active_provider = "mock"
[ai.providers.mock]
type = "mock"
model = "mock-history"
[session.compaction]
model_context_window = 1000000
"#,
    )
    .expect("write large-context mock config");
    let input = vec![0; 600 * 1024];

    let output = run(home.path(), &["--output=json", "-"], Some(&input));

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output-limit JSON result");
    assert_eq!(result["status"], "failed");
    assert_eq!(result["error_code"], "output_limit");
}

#[test]
fn public_command_reports_uid_cwd_and_missing_controlling_terminal() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    let expected_cwd = std::fs::canonicalize(workspace.path()).expect("canonical workspace");
    configure_model(home.path(), "mock-public-no-ctty");

    let output = run(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "inspect the execution context",
        ],
        None,
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("execution context JSON result");
    let answer = result["answer"].as_str().expect("execution context answer");
    assert!(answer.contains("NO_CTTY"), "{answer}");
    assert!(answer.contains("UID="), "{answer}");
    assert!(
        answer.contains(&format!("CWD={}", expected_cwd.display())),
        "{answer}"
    );
}

#[test]
fn public_custom_policy_ask_is_terminal_and_never_starts_the_command() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-failure");
    let policy = home.path().join("require-approval.toml");
    std::fs::write(
        &policy,
        r#"
version = "v1-test"
default = "Allow"
[[rules]]
name = "review-command"
matches.subsystem = "cosh"
matches.operation = "run_command"
outcome = "RequireApproval"
"#,
    )
    .expect("write approval policy");

    let output = run_with_env(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run the command",
        ],
        None,
        &[("COSH_AUDIT_POLICY", policy.to_str().expect("policy path"))],
    );

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("approval JSON result");
    assert_eq!(result["error_code"], "approval_required");
    assert!(!workspace.path().join("command-started").exists());
}

#[test]
fn public_custom_policy_argument_denial_is_enforced() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-failure");
    let policy = home.path().join("deny-arguments.toml");
    std::fs::write(
        &policy,
        r#"
version = "v1-test"
default = "Allow"
[[rules]]
name = "deny-command-arguments"
matches.subsystem = "cosh"
matches.operation = "run_command"
outcome = "Deny"
[[rules.matches.arg]]
key = "args"
"#,
    )
    .expect("write argument policy");

    let output = run_with_env(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run the command",
        ],
        None,
        &[("COSH_AUDIT_POLICY", policy.to_str().expect("policy path"))],
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("argument denial JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("denied")));
    assert!(!workspace.path().join("command-started").exists());
}

#[test]
fn public_environment_policy_cannot_override_an_existing_deny() {
    let home = tempfile::tempdir().expect("temp home");
    let workspace = tempfile::tempdir().expect("temp workspace");
    configure_model(home.path(), "mock-public-command-failure");
    let user_policy_dir = home.path().join(".copilot-shell/cosh");
    std::fs::create_dir_all(&user_policy_dir).expect("user policy directory");
    std::fs::write(
        user_policy_dir.join("audit.toml"),
        "version = \"v1-deny\"\ndefault = \"Deny\"\n",
    )
    .expect("write denying user policy");
    let override_policy = home.path().join("allow.toml");
    std::fs::write(
        &override_policy,
        "version = \"v1-allow\"\ndefault = \"Allow\"\n",
    )
    .expect("write allowing environment policy");

    let output = run_with_env(
        home.path(),
        &[
            "--output=json",
            "--exec=host",
            "--workspace",
            workspace.path().to_str().expect("workspace"),
            "run the command",
        ],
        None,
        &[(
            "COSH_AUDIT_POLICY",
            override_policy.to_str().expect("override policy path"),
        )],
    );

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("narrowing JSON result");
    assert!(result["answer"]
        .as_str()
        .is_some_and(|answer| answer.contains("denied")));
    assert!(!workspace.path().join("command-started").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn public_text_does_not_open_an_unrelated_workspace() {
    use std::os::unix::fs::PermissionsExt;

    if unsafe { nix::libc::geteuid() } != 0 {
        return;
    }
    let home = tempfile::tempdir_in("/tmp").expect("temp home");
    configure_mock(home.path());
    std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755))
        .expect("readable home");
    std::fs::set_permissions(
        home.path().join(".copilot-shell"),
        std::fs::Permissions::from_mode(0o755),
    )
    .expect("readable config directory");
    std::fs::set_permissions(
        home.path().join(".copilot-shell/config.toml"),
        std::fs::Permissions::from_mode(0o644),
    )
    .expect("readable config");
    let private = home.path().join("private");
    std::fs::create_dir(&private).expect("private directory");
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))
        .expect("private directory mode");
    let executable = home.path().join("cosh-core");
    std::fs::copy(common::binary_path(), &executable).expect("copy public binary");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
        .expect("readable public binary");

    let home_env = format!("HOME={}", home.path().display());
    let output = Command::new("/usr/sbin/runuser")
        .args([
            "-u",
            "nobody",
            "--",
            "env",
            home_env.as_str(),
            executable.to_str().expect("executable path"),
            "--headless",
            "--execution-profile=public-headless-v1",
            "hello",
        ])
        .current_dir(&private)
        .stdin(Stdio::null())
        .output()
        .expect("spawn public headless through runuser");

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        "mock history: hello\n"
    );
}
