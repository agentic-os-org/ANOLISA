use super::*;

#[test]
fn raw_cli_streaming_tool_approval_renders_before_agent_finishes() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(2_500)),
        (b"exit\n".to_vec(), Duration::from_millis(1_000)),
    ]);

    assert!(output.contains("Preparing a streamed tool request before finishing."));
    assert_approval_prompt_visible(&output);
    assert!(output.contains("Bash · medium risk"));
    assert!(output.contains("$ git status --short"));
    assert!(!output.contains("Subject: Bash"));
    assert!(!output.contains("Subject: tool Bash"));
    assert!(!output.contains("Command: git status --short"));
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("sent to shell"), "{output}");
    assert!(!output.contains("Bash tool - approved"), "{output}");
    assert!(output.contains("$ git status --short"), "{output}");
    assert!(!output.contains("Tool result for request req-1"));
    assert!(!output.contains("Received approved tool result"));
    let approval_marker = if output.contains("Approval req-1") {
        "Approval req-1"
    } else {
        "Approval required"
    };
    assert_inline_before_followup(
        &output,
        "Preparing a streamed tool request before finishing.",
        approval_marker,
    );
    assert!(!output.contains("Analysis continued after the approved command"));
    assert!(!output.contains("stdout captured; [Details]"), "{output}");
    assert!(!output.contains("tool request - approved by user"));
    assert!(!output.contains("Running command"), "{output}");
    assert!(!output.contains("tool-1 tool: executed"));
    assert!(!output.contains("Thinking...Approval"));
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approved_bash_tool_prints_native_command_and_stdout() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream pwd tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(1_200)),
        (b"exit\n".to_vec(), Duration::from_millis(1_000)),
    ]);
    let expected_cwd = env!("CARGO_MANIFEST_DIR");

    assert!(output.contains("Preparing a streamed pwd request before finishing."));
    assert_approval_prompt_visible(&output);
    assert!(output.contains("Bash · "), "{output}");
    assert!(output.contains("$ pwd"), "{output}");
    assert!(output.contains(expected_cwd), "{output}");
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("sent to shell"), "{output}");
    assert!(!output.contains("Tool result for request req-1"));
    assert_inline_before_followup(&output, "$ pwd", expected_cwd);
    assert!(!output.contains("Tool called: Bash called"), "{output}");
    assert!(!output.contains("stdout captured; [Details]"), "{output}");
    assert!(!output.contains("Command: pwd"), "{output}");
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approved_bash_tool_streams_delayed_output_before_analysis() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (
            b"?? stream delayed tool approval\n".to_vec(),
            Duration::ZERO,
        ),
        (b"\n".to_vec(), Duration::from_millis(1_200)),
        (b"exit\n".to_vec(), Duration::from_millis(2_600)),
    ]);
    let normalized = output.replace('\r', "");

    assert!(output.contains("Preparing a delayed streamed tool request before finishing."));
    assert_approval_prompt_visible(&output);
    assert!(
        output.contains("$ sleep 1; echo a; sleep 1; echo b"),
        "{output}"
    );
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("sent to shell"), "{output}");
    assert!(normalized.contains("a\nb"), "{output}");
    assert!(
        output.contains("Command result analysis for req-1: foreground shell evidence received"),
        "{output}"
    );
    assert!(!output.contains("Tool result for request req-1"));
    assert!(!output.contains("shell: completed"), "{output}");
    assert_inline_before_followup(&normalized, "$ sleep 1; echo a; sleep 1; echo b", "a\nb");
    assert_inline_before_followup(&normalized, "a\nb", "Command result analysis for req-1");
    assert!(!output.contains("stdout captured; [Details]"), "{output}");
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approved_bash_tool_streams_stderr_to_transcript() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream stderr tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(3_000)),
        (b"\n".to_vec(), Duration::from_millis(2_000)),
        (b"exit\n".to_vec(), Duration::from_millis(4_000)),
    ]);

    assert!(output.contains("Preparing a stderr streamed tool request before finishing."));
    assert_approval_prompt_visible(&output);
    assert!(
        output.contains("$ printf 'out\\n'; printf 'err\\n' >&2"),
        "{output}"
    );
    assert!(output.contains("out"), "{output}");
    assert!(output.contains("err"), "{output}");
    assert!(output.contains("sent to shell"), "{output}");
    assert!(!output.contains("Tool result for request req-1"));
    assert_inline_before_followup(&output, "$ printf 'out\\n'; printf 'err\\n' >&2", "out");
    assert!(!output.contains("stderr captured; /details"), "{output}");
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approved_sudo_tool_is_emitted_to_foreground_shell() {
    let home = temp_shell_home("approval-sudo-shell");
    write_cosh_config(
        &home,
        r#"[shell]
readonly_disabled = ["git status", "pwd"]"#,
    );
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let fake_sudo = bin_dir.join("sudo");
    write_executable(
        &fake_sudo,
        "#!/bin/sh\nprintf 'fake-sudo:'\n\"$@\"\nprintf '\\n'\n",
    );

    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        vec![
            (b"?? stream sudo tool approval\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            (b"exit\n".to_vec(), Duration::from_millis(2_000)),
        ],
    );

    assert_approval_prompt_visible(&output);
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("$ sudo printf approved-sudo"), "{output}");
    assert!(output.contains("fake-sudo:approved-sudo"), "{output}");
    assert!(
        !output.contains("Tool result for request req-1"),
        "{output}"
    );
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_approved_ssh_tool_receives_foreground_input() {
    let home = temp_shell_home("approval-fake-ssh");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("ssh"),
        "#!/bin/sh\nprintf 'fake-ssh prompt:'\nIFS= read -r line\nprintf 'fake-ssh received:%s\\n' \"$line\"\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_marker_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        &[
            ("cosh-osc$ ", b"?? stream ssh tool approval\n"),
            ("Approval req-1", b"\n"),
            ("fake-ssh prompt:", b"hello-from-user\n"),
            ("fake-ssh received:hello-from-user", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("Bash tool sent to shell"), "{output}");
    assert!(output.contains("$ ssh fake-host"), "{output}");
    assert!(output.contains("fake-ssh prompt:"), "{output}");
    assert!(
        output.contains("fake-ssh received:hello-from-user"),
        "{output}"
    );
    assert!(
        !output.contains("Tool result for request req-1"),
        "{output}"
    );
}

#[test]
fn raw_cli_approved_pager_tool_receives_q() {
    let home = temp_shell_home("approval-fake-pager");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("fake-pager"),
        "#!/bin/bash\nprintf 'fake-pager waiting\\n'\nIFS= read -r -n 1 key\nprintf 'fake-pager key:%s\\n' \"$key\"\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_marker_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        &[
            ("cosh-osc$ ", b"?? stream pager tool approval\n"),
            ("Approval req-1", b"\n"),
            ("fake-pager waiting", b"q"),
            ("fake-pager key:q", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("$ fake-pager"), "{output}");
    assert!(output.contains("fake-pager waiting"), "{output}");
    assert!(output.contains("fake-pager key:q"), "{output}");
    assert!(
        !output.contains("Tool result for request req-1"),
        "{output}"
    );
    assert_no_pager_transport_leak(&output);
}

/// Repository holding exactly one commit, plus the `GIT_DIR`/`GIT_WORK_TREE`
/// values that point the harness shell at it. The shared raw-CLI git fixture
/// has no commits, so `git log` needs a repository of its own here.
struct GitLogFixture {
    git_dir: String,
    work_tree: String,
    head: String,
}

fn git_log_fixture(home: &Path) -> GitLogFixture {
    let work_tree = home.join("git-log-repo");
    fs::create_dir_all(&work_tree).unwrap();
    let git = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-C")
            .arg(&work_tree)
            .args(args)
            // HOME is a fresh directory, so identity cannot come from a user
            // gitconfig.
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap_or_else(|err| panic!("git {args:?}: {err}"));
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("git output is utf-8")
    };

    git(&["-c", "init.defaultBranch=main", "init", "--quiet"]);
    git(&["config", "user.email", "pager-fixture@example.com"]);
    git(&["config", "user.name", "Pager Fixture"]);
    git(&[
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--quiet",
        "--allow-empty",
        "-m",
        "implicit pager fixture commit",
    ]);
    let head = git(&["log", "-1", "--format=%h"]).trim().to_string();
    assert!(!head.is_empty(), "fixture head hash is empty");

    GitLogFixture {
        git_dir: work_tree.join(".git").to_string_lossy().to_string(),
        work_tree: work_tree.to_string_lossy().to_string(),
        head,
    }
}

/// No surface a user reads may carry the pager environment cosh applies around
/// an agent handoff (issue #1988). Matches the assignment form the transport
/// would use, which is what NON_INTERACTIVE_PAGER_PREFIX pins on the Rust side;
/// a bare variable name is legitimate in a command that reads it.
fn assert_no_pager_transport_leak(output: &str) {
    for marker in [
        "PAGER=cat",
        "GIT_PAGER=cat",
        "MANPAGER=cat",
        "SYSTEMD_PAGER=cat",
    ] {
        assert!(!output.contains(marker), "leaked {marker}: {output}");
    }
}

#[test]
fn raw_cli_approved_git_log_tool_never_waits_for_a_pager() {
    let home = temp_shell_home("approval-git-implicit-pager");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    // Started only if Git actually resolves a pager; it never exits on its own,
    // so the run would stall here without the implicit-pager policy.
    write_executable(
        &bin_dir.join("fake-pager"),
        "#!/bin/bash\nprintf 'fake-pager waiting\\n'\nIFS= read -r -n 1 key\nprintf 'fake-pager key:%s\\n' \"$key\"\n",
    );
    let fixture = git_log_fixture(&home);
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_and_delayed_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("GIT_DIR", &fixture.git_dir),
            ("GIT_WORK_TREE", &fixture.work_tree),
            // A host that already exports GIT_PAGER=cat would satisfy the
            // no-pager assertions without the policy under test, and would also
            // break the restore-to-unset assertion below.
            ("PAGER", RAW_CLI_UNSET_ENV),
            ("GIT_PAGER", RAW_CLI_UNSET_ENV),
            ("MANPAGER", RAW_CLI_UNSET_ENV),
            ("SYSTEMD_PAGER", RAW_CLI_UNSET_ENV),
        ],
        vec![
            (
                b"?? stream git pager tool approval\n".to_vec(),
                Duration::ZERO,
            ),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            // Deliberately never sends `q`.
            (b"echo after-git\n".to_vec(), Duration::from_millis(2_000)),
            (
                b"printf 'after=%s/%s/%s/%s\\n' \"${PAGER-unset}\" \"${GIT_PAGER-unset}\" \"${MANPAGER-unset}\" \"${SYSTEMD_PAGER-unset}\"\n"
                    .to_vec(),
                Duration::from_millis(600),
            ),
            (b"exit\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );

    assert!(
        output.contains("$ git -c core.pager=fake-pager --paginate log -1 --format=%h"),
        "{output}"
    );
    assert!(output.contains("sent to shell"), "{output}");
    assert!(!output.contains("fake-pager waiting"), "{output}");
    // The hash only appears if Git's output actually reached the transcript;
    // `%h` in the echoed command line cannot satisfy this on its own.
    assert!(output.contains(&fixture.head), "{output}");
    assert!(output.contains("after-git"), "{output}");
    assert!(
        output.contains("Command result analysis for req-1: foreground shell evidence received"),
        "{output}"
    );
    // Variables the user never had must be unset again, not left as `cat`.
    assert!(output.contains("after=unset/unset/unset/unset"), "{output}");
    // A forensics command adds no interactive noise to the receipt.
    assert!(!output.contains("Press q to leave a pager"), "{output}");
    assert_no_pager_transport_leak(&output);
}

#[test]
fn raw_cli_approved_less_tool_hints_before_taking_the_foreground() {
    let home = temp_shell_home("approval-explicit-less");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("less"),
        "#!/bin/bash\nprintf 'fake-less waiting\\n'\nIFS= read -r -n 1 key\nprintf 'fake-less key:%s\\n' \"$key\"\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_marker_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        &[
            ("cosh-osc$ ", b"?? stream less tool approval\n"),
            ("Approval req-1", b"\n"),
            ("fake-less waiting", b"q"),
            ("fake-less key:q", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("$ less Cargo.toml"), "{output}");
    // Fragments are kept short: the receipt panel wraps the hint.
    assert!(
        output.contains("This command will run interactively in the foreground"),
        "{output}"
    );
    assert!(output.contains("Press q to leave a pager."), "{output}");
    // An explicitly interactive command keeps its PTY and its keystrokes.
    assert!(output.contains("fake-less waiting"), "{output}");
    assert!(output.contains("fake-less key:q"), "{output}");
    assert_no_pager_transport_leak(&output);
}

#[test]
fn raw_cli_approved_less_tool_hints_in_zh_language_env() {
    let home = temp_shell_home("approval-explicit-less-zh");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("less"),
        "#!/bin/bash\nprintf 'fake-less waiting\\n'\nIFS= read -r -n 1 key\nprintf 'fake-less key:%s\\n' \"$key\"\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("COSH_SHELL_LANG", "zh-CN"),
        ],
        &[
            ("cosh-osc$ ", b"?? stream less tool approval\n"),
            ("审批 req-1", b"\n"),
            ("fake-less waiting", b"q"),
            ("fake-less key:q", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("$ less Cargo.toml"), "{output}");
    assert!(output.contains("此命令将在前台交互运行"), "{output}");
    assert!(output.contains("键盘输入会直接发送给它"), "{output}");
    assert!(output.contains("通常按 q"), "{output}");
    assert!(!output.contains("Press q to leave a pager"), "{output}");
    assert!(output.contains("fake-less waiting"), "{output}");
    assert!(output.contains("fake-less key:q"), "{output}");
    assert_no_pager_transport_leak(&output);
}

#[test]
fn raw_cli_user_typed_git_log_keeps_its_own_pager_configuration() {
    let home = temp_shell_home("user-native-git-pager");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    // Behaves like a real pager: the paged text arrives on stdin, keystrokes
    // come from the controlling terminal.
    write_executable(
        &bin_dir.join("fake-pager"),
        "#!/bin/bash\ncat\nprintf 'fake-pager waiting\\n'\nIFS= read -r key < /dev/tty\nprintf 'fake-pager released:%s\\n' \"$key\"\n",
    );
    let fixture = git_log_fixture(&home);
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_str),
            ("PATH", &path),
            ("GIT_DIR", &fixture.git_dir),
            ("GIT_WORK_TREE", &fixture.work_tree),
            // A host exporting GIT_PAGER=cat would neutralize the pager this
            // test needs Git to resolve from `-c core.pager`.
            ("PAGER", RAW_CLI_UNSET_ENV),
            ("GIT_PAGER", RAW_CLI_UNSET_ENV),
            ("MANPAGER", RAW_CLI_UNSET_ENV),
            ("SYSTEMD_PAGER", RAW_CLI_UNSET_ENV),
        ],
        &[
            (
                "cosh-osc$ ",
                b"git -c core.pager=fake-pager --paginate log -1 --format=%h\n",
            ),
            ("fake-pager waiting", b"q\n"),
            ("fake-pager released:q", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    // The implicit-pager policy is scoped to agent handoffs: a command the user
    // typed still resolves the pager their Git configuration asked for, and
    // their keystrokes still reach it.
    assert!(output.contains("fake-pager waiting"), "{output}");
    assert!(output.contains("fake-pager released:q"), "{output}");
    assert_no_pager_transport_leak(&output);
}

#[test]
fn raw_cli_approved_repl_tool_receives_followup_lines() {
    let home = temp_shell_home("approval-fake-repl");
    let bin_dir = home.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    write_executable(
        &bin_dir.join("fake-repl"),
        "#!/bin/sh\nprintf 'fake-repl ready\\n'\nIFS= read -r first\nprintf 'fake-repl next\\n'\nIFS= read -r second\nprintf 'fake-repl lines:%s/%s\\n' \"$first\" \"$second\"\n",
    );
    let old_path = std::env::var("PATH").unwrap_or_default();
    let path = format!("{}:{old_path}", bin_dir.display());
    let home_str = home.to_string_lossy().to_string();
    let output = run_raw_cli_serial_with_args_env_and_marker_input(
        "fake",
        &[],
        &[("HOME", &home_str), ("PATH", &path)],
        &[
            ("cosh-osc$ ", b"?? stream repl tool approval\n"),
            ("Approval req-1", b"\n"),
            ("fake-repl ready", b"plain natural language for repl\n"),
            ("fake-repl next", b".exit\n"),
            ("fake-repl lines:plain natural language for repl/.exit", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("$ fake-repl"), "{output}");
    assert!(output.contains("fake-repl ready"), "{output}");
    assert!(
        output.contains("fake-repl lines:plain natural language for repl/.exit"),
        "{output}"
    );
    assert!(!output.contains("AI request"), "{output}");
    assert!(
        !output.contains("Tool result for request req-1"),
        "{output}"
    );
}

#[test]
fn raw_cli_approved_bash_tool_drops_stale_pre_approval_followup() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream stale tool approval\n".to_vec(), Duration::ZERO),
        (b"\n".to_vec(), Duration::from_millis(1_400)),
        (b"exit\n".to_vec(), Duration::from_millis(500)),
    ]);
    let expected_cwd = env!("CARGO_MANIFEST_DIR");

    assert!(output.contains("Preparing a command before approval."));
    assert_approval_prompt_visible(&output);
    assert!(output.contains("req-1"), "{output}");
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(output.contains("sent to shell"), "{output}");
    assert!(output.contains("$ pwd"), "{output}");
    assert!(output.contains(expected_cwd), "{output}");
    assert!(!output.contains("Tool result for request req-1"));
    assert!(
        !output.contains("STALE APPROVAL TEXT SHOULD NOT RENDER"),
        "{output}"
    );
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_denied_bash_tool_does_not_render_stale_executed_claim() {
    let output = run_raw_cli_ask_with_delayed_input(vec![
        (b"?? stream pwd tool approval\n".to_vec(), Duration::ZERO),
        (b"\x1b[C\x1b[C\n".to_vec(), Duration::from_millis(800)),
        (b"exit\n".to_vec(), Duration::from_millis(300)),
    ]);
    let expected_cwd = env!("CARGO_MANIFEST_DIR");

    assert!(output.contains("Preparing a streamed pwd request before finishing."));
    assert_approval_prompt_visible(&output);
    assert!(output.contains("Bash · "), "{output}");
    assert!(output.contains("Denied req-1"), "{output}");
    assert!(!output.contains("No command ran."), "{output}");
    assert!(
        output.contains("Command was not executed for req-1"),
        "{output}"
    );
    assert!(!output.contains(expected_cwd), "{output}");
    assert!(
        !output.contains("approved Bash command finished"),
        "{output}"
    );
    assert!(
        !output.contains("Command result analysis for req-1"),
        "{output}"
    );
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_denied_bash_tool_uses_zh_language_env() {
    let output = run_raw_cli_ask_with_args_env_and_delayed_input(
        &[],
        &[("COSH_SHELL_LANG", "zh-CN")],
        vec![
            (b"?? stream pwd tool approval\n".to_vec(), Duration::ZERO),
            (b"\x1b[C\x1b[C\n".to_vec(), Duration::from_millis(2_500)),
            (b"exit\n".to_vec(), Duration::from_millis(300)),
        ],
    );
    let expected_cwd = env!("CARGO_MANIFEST_DIR");

    assert_zh_approval_prompt_visible(&output);
    assert!(output.contains("Bash · "), "{output}");
    assert!(!output.contains("对象: Bash"), "{output}");
    assert!(output.contains("已拒绝 req-1"), "{output}");
    assert!(output.contains("$ pwd"), "{output}");
    assert!(
        output.contains("Command was not executed for req-1"),
        "{output}"
    );
    assert!(!output.contains("Approval required"), "{output}");
    assert!(!output.contains("Approval req-1"), "{output}");
    assert!(!output.contains("Subject: Bash"), "{output}");
    assert!(!output.contains("Denied req-1"), "{output}");
    assert!(!output.contains(expected_cwd), "{output}");
    assert!(
        !output.contains("approved Bash command finished"),
        "{output}"
    );
    assert!(!output.contains("bash:"));
}

#[test]
fn raw_cli_user_approved_bash_tool_supports_pipe() {
    let output = run_raw_cli_with_delayed_input(
        "fake",
        vec![
            (b"?? stream piped tool approval\n".to_vec(), Duration::ZERO),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            (b"exit\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );

    assert!(output.contains("Preparing a piped streamed tool request before finishing."));
    assert_approval_prompt_visible(&output);
    assert!(output.contains("Bash · "));
    assert!(output.contains("$ ps aux | head"));
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Blocked req-1"), "{output}");
    assert!(output.contains("$ ps aux | head"), "{output}");
    assert!(
        !output.contains("cosh-shell: blocked shell metacharacter"),
        "{output}"
    );
    assert!(output.contains("sent to shell"), "{output}");
    assert!(!output.contains("approved Bash command finished"));
    assert!(!output.contains("Tool result for request req-1"));
    assert!(!output.contains("Received approved tool result"));
    assert!(!output.contains("Analysis continued after the approved command"));
    assert!(!output.contains("Thinking...Approval"));
}

#[test]
fn raw_cli_user_approved_pipeline_supports_quoted_multiline_script() {
    assert_user_approved_pipeline_supports_quoted_multiline_script(&[]);
}

#[test]
fn raw_cli_zsh_user_approved_pipeline_supports_quoted_multiline_script() {
    if Command::new("zsh").arg("--version").output().is_err() {
        return;
    }
    assert_user_approved_pipeline_supports_quoted_multiline_script(&["--shell", "zsh"]);
}

fn assert_user_approved_pipeline_supports_quoted_multiline_script(args: &[&str]) {
    let output = run_raw_cli_with_args_and_delayed_input(
        "fake",
        args,
        vec![
            (
                b"?? stream quoted multiline pipeline approval\n".to_vec(),
                Duration::ZERO,
            ),
            (b"\n".to_vec(), Duration::from_millis(1_200)),
            (b"exit\n".to_vec(), Duration::from_millis(1_000)),
        ],
    );
    let normalized = strip_ansi_escape(&output).replace('\r', "");

    assert!(
        output.contains("Preparing a quoted multiline pipeline request before finishing."),
        "{output}"
    );
    assert_approval_prompt_visible(&output);
    assert!(output.contains("Approved req-1"), "{output}");
    assert!(!output.contains("Blocked req-1"), "{output}");
    assert!(normalized.contains("\nalpha\nbeta\n"), "{output}");
    assert!(output.contains("sent to shell"), "{output}");
}
