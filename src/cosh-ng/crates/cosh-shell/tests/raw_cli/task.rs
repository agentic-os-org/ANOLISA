use super::*;

#[cfg(target_os = "linux")]
fn login_path_task_fixture(label: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let home = temp_shell_home(label);
    let profile = home.join("shell-profile");
    fs::create_dir(&profile).expect("create independent Shell profile");
    let binary = profile.join("cosh-shell");
    fs::copy(env!("CARGO_BIN_EXE_cosh-shell"), &binary).expect("copy actual Shell");
    assert!(!profile.join("cosh-gateway").exists());
    let bin = home.join("profile-bin");
    fs::create_dir(&bin).expect("create login-only Gateway directory");
    let gateway = TASK_GATEWAY.replace(
        "printf '%s\\n' \"$*\" >> \"$TASK_GATEWAY_ARGV\"",
        "printf '<%s>' \"$@\" >> \"$HOME/gateway-exact-argv\"\n\
         printf '\\n' >> \"$HOME/gateway-exact-argv\"\n\
         printf '%s\\n' \"$PATH\" >> \"$HOME/gateway-path\"\n\
         printf '%s\\n' \"$*\" >> \"$TASK_GATEWAY_ARGV\"",
    ).replace(
        "  *)\n    printf '%s\\n' 'unexpected Task command'",
        "  *' list '*)\n\
         printf '%s\\n' '{\"tasks\":[{\"task_id\":\"login-task-123\",\"state\":\"succeeded\",\"revision\":1}]}'\n    ;;\n\
         *' get login-task-123 '*)\n\
         printf '%s\\n' '{\"task_id\":\"login-task-123\",\"state\":\"succeeded\",\"revision\":1}'\n    ;;\n\
         *' events login-task-123 '*)\n\
         printf '%s\\n' '{\"events\":[],\"has_more\":false}'\n    ;;\n\
         *)\n    printf '%s\\n' 'unexpected Task command'",
    );
    write_executable(&bin.join("cosh-gateway"), &gateway);
    fs::create_dir(home.join("relative-gateway")).expect("create untrusted relative directory");
    write_executable(
        &home.join("relative-gateway/cosh-gateway"),
        "#!/bin/sh\nprintf spoof >> \"$HOME/spoof-gateway\"\nexit 2\n",
    );
    fs::write(
        home.join(".bash_profile"),
        "printf 'profile-hit\\n' >> \"$HOME/profile-hits\"\n\
         export PATH=\"$HOME/profile-bin:relative-gateway:$PATH\"\n\
         PS1='task-login$ '\n",
    )
    .expect("write login profile");
    (home, binary)
}

#[cfg(target_os = "linux")]
fn run_login_path_task(home: &Path, binary: &Path, steps: &[(&str, &[u8])]) -> String {
    let home_text = home.display().to_string();
    let argv = home.join("gateway.argv").display().to_string();
    let goal = home.join("gateway.goal").display().to_string();
    run_raw_cli_binary_with_args_env_current_dir_and_marker_input(
        binary,
        "fake",
        &["--shell", "bash", "--login"],
        &[
            ("HOME", &home_text),
            ("PATH", "/usr/bin:/bin"),
            ("COSH_SHELL_ISOLATED", "0"),
            ("COSH_SHELL_LOGIN_IDENTITY", "1"),
            ("COSH_SHELL_BOOTSTRAP_PATH", RAW_CLI_UNSET_ENV),
            ("COSH_SHELL_STARTUP_BANNER", "0"),
            ("COSH_GATEWAY_EXECUTABLE", RAW_CLI_UNSET_ENV),
            ("COSH_GATEWAY_SOCKET", RAW_CLI_UNSET_ENV),
            ("TASK_GATEWAY_ARGV", &argv),
            ("TASK_GATEWAY_GOAL", &goal),
        ],
        home,
        steps,
    )
}

#[cfg(target_os = "linux")]
fn assert_login_path_task_contract(home: &Path, output: &str) {
    assert_eq!(
        fs::read_to_string(home.join("profile-hits"))
            .unwrap()
            .lines()
            .count(),
        1,
        "{output}"
    );
    let paths = fs::read_to_string(home.join("gateway-path")).expect("actual Gateway PATH");
    assert!(!paths.is_empty(), "{output}");
    for path in paths.lines() {
        assert!(
            path.starts_with(&format!("{}:", home.join("profile-bin").display())),
            "{path}\n{output}"
        );
        assert!(
            !path
                .split(':')
                .any(|entry| entry == "relative-gateway" || entry.is_empty()),
            "{path}"
        );
    }
    assert!(!home.join("spoof-gateway").exists(), "{output}");
    assert_eq!(
        fs::read_to_string(home.join("fresh-input")).unwrap(),
        "fresh\n",
        "{output}"
    );
    assert!(!home.join("forbidden-effect").exists(), "{output}");
    assert!(!output.contains("bash: /task"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_task_queries_use_only_trusted_login_path_once() {
    let (home, binary) = login_path_task_fixture("task-login-queries");
    let output = run_login_path_task(
        &home,
        &binary,
        &[
            ("task-login$ ", b"/task list\n"),
            ("Task", b""),
            ("task-login$ ", b"/task show login-task-123\n"),
            ("Task", b""),
            (
                "task-login$ ",
                b"printf 'fresh\\n' >> \"$HOME/fresh-input\"; printf 'fresh-done\\n'\n",
            ),
            ("fresh-done", b""),
            ("task-login$ ", b"exit\n"),
        ],
    );
    eprintln!("task login query transcript:\n{output}");
    assert!(output.contains("Persistent Tasks"), "{output}");
    assert!(output.contains("Task result"), "{output}");
    assert_login_path_task_contract(&home, &output);
    let argv = fs::read_to_string(home.join("gateway-exact-argv")).unwrap();
    assert_eq!(argv.lines().count(), 3, "{argv}");
    assert!(
        argv.contains("<task><--output><jsonl><list><--limit><20>"),
        "{argv}"
    );
    assert!(
        argv.contains("<task><--output><jsonl><get><login-task-123>"),
        "{argv}"
    );
    assert!(
        argv.contains("<task><--output><jsonl><events><login-task-123><--after><0><--limit><64>"),
        "{argv}"
    );
    let _ = fs::remove_dir_all(home);
}

#[cfg(target_os = "linux")]
#[test]
fn raw_cli_task_submission_preserves_stdin_and_login_path_once() {
    let (home, binary) = login_path_task_fixture("task-login-submit");
    let output = run_login_path_task(
        &home,
        &binary,
        &[
            (
                "task-login$ ",
                b"/task goal; printf forbidden > forbidden-effect\n",
            ),
            ("Create persistent Task · Step 1 of 4 · Goal", b"\n"),
            ("Create persistent Task · Step 2 of 4 · Runtime", b"\n"),
            ("Create persistent Task · Step 3 of 4 · Checkpoint", b"\n"),
            ("Create persistent Task · Step 4 of 4 · Review", b"\n"),
            ("Persistent Task submitted", b""),
            (
                "task-login$ ",
                b"printf 'fresh\\n' >> \"$HOME/fresh-input\"; printf 'fresh-done\\n'\n",
            ),
            ("fresh-done", b""),
            ("task-login$ ", b"exit\n"),
        ],
    );
    eprintln!("task login submission transcript:\n{output}");
    assert_login_path_task_contract(&home, &output);
    let argv = fs::read_to_string(home.join("gateway-exact-argv")).unwrap();
    assert_eq!(
        argv.lines()
            .filter(|line| line.contains("<submit>"))
            .count(),
        1,
        "{argv}"
    );
    assert!(argv.contains("<--runtime><core>"), "{argv}");
    assert!(argv.contains("<--approval-policy><allow-all>"), "{argv}");
    assert_eq!(
        fs::read_to_string(home.join("gateway.goal")).unwrap(),
        "goal; printf forbidden > forbidden-effect"
    );
    let _ = fs::remove_dir_all(home);
}

const TASK_GATEWAY: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$TASK_GATEWAY_ARGV"
case " $* " in
  *' capabilities '*)
    printf '%s\n' '{"event":"task_capabilities","launch_schema_version":1,"default_workspace":{"scope_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","display_name":"raw-cli-workspace"},"runtimes":[{"runtime":"core","readiness":{"status":"ready"},"security":{"delegated_local_authority":true,"gateway_brokered_effects":false,"checkpoint_is_baseline_only":false}},{"runtime":"codex","readiness":{"status":"ready"},"security":{"delegated_local_authority":true,"gateway_brokered_effects":false,"checkpoint_is_baseline_only":false}}],"checkpoint":{"status":"ready"},"default_approval":"allow_all"}'
    ;;
  *' submit '*)
    cat > "$TASK_GATEWAY_GOAL"
    printf '%s\n' '{"event":"task","task_id":"tsk_00000000-0000-0000-0000-000000000123"}'
    ;;
  *)
    printf '%s\n' 'unexpected Task command' >&2
    exit 2
    ;;
esac
"#;

#[test]
fn raw_cli_task_form_submits_edited_typed_launch_once() {
    let home = temp_shell_home("task-form-submit");
    let gateway = home.join("cosh-gateway");
    let argv_log = home.join("gateway.argv");
    let goal_log = home.join("gateway.goal");
    let socket = home.join("gateway.sock");
    write_executable(&gateway, TASK_GATEWAY);

    let home_text = home.to_string_lossy().to_string();
    let gateway_text = gateway.to_string_lossy().to_string();
    let argv_text = argv_log.to_string_lossy().to_string();
    let goal_text = goal_log.to_string_lossy().to_string();
    let socket_text = socket.to_string_lossy().to_string();
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_text),
            ("COSH_GATEWAY_EXECUTABLE", &gateway_text),
            ("COSH_GATEWAY_SOCKET", &socket_text),
            ("TASK_GATEWAY_ARGV", &argv_text),
            ("TASK_GATEWAY_GOAL", &goal_text),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", b"/task prefilled goal\n"),
            ("Create persistent Task · Step 1 of 4 · Goal", b" edited\n"),
            (
                "Create persistent Task · Step 2 of 4 · Runtime",
                b"\x1b[C\n",
            ),
            (
                "Create persistent Task · Step 3 of 4 · Checkpoint",
                b"\x1b[C\x1b[C\n",
            ),
            ("Create persistent Task · Step 4 of 4 · Review", b"\n"),
            ("Persistent Task submitted", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    let argv = fs::read_to_string(&argv_log).expect("Gateway argv log");
    let submit_calls = argv
        .lines()
        .filter(|line| line.contains(" submit "))
        .count();
    assert_eq!(submit_calls, 1, "{argv}");
    let submit = argv
        .lines()
        .find(|line| line.contains(" submit "))
        .expect("submit argv");
    assert!(submit.contains("--runtime codex"), "{submit}");
    assert!(submit.contains("--checkpoint off"), "{submit}");
    assert!(submit.contains("--approval-policy allow-all"), "{submit}");
    assert!(
        submit.contains(
            "--expected-workspace-digest \
             aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        "{submit}"
    );
    assert!(!submit.contains("--runtime-profile"), "{submit}");
    assert_eq!(
        fs::read_to_string(&goal_log).expect("Gateway goal"),
        "prefilled goal edited"
    );

    assert!(output.contains("prefilled goal"), "{output}");
    assert!(output.contains("Runtime: Codex (ACP)"), "{output}");
    assert!(output.contains("Checkpoint: Off"), "{output}");
    assert!(output.contains("Approval: allow_all"), "{output}");
    assert!(
        output.contains("tsk_00000000-0000-0000-0000-000000000123"),
        "{output}"
    );
    assert!(!output.contains("bash: /task"), "{output}");

    let _ = fs::remove_dir_all(home);
}

const SNAPSHOT_TASK_ID: &str = "tsk_00000000-0000-0000-0000-000000000321";
const SNAPSHOT_ID: &str = "ckp_00000000-0000-0000-0000-000000000654";
const SNAPSHOT_PREVIEW_DIGEST: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const SNAPSHOT_GATEWAY: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$TASK_GATEWAY_ARGV"
case " $* " in
  *' get tsk_00000000-0000-0000-0000-000000000321 '*)
    printf '%s\n' '{"event":"task","task_id":"tsk_00000000-0000-0000-0000-000000000321","state":"succeeded","revision":9}'
    ;;
  *' snapshot list tsk_00000000-0000-0000-0000-000000000321 '*)
    printf '%s\n' '{"event":"task_snapshots","task_id":"tsk_00000000-0000-0000-0000-000000000321","state":"succeeded","revision":9,"workspace":{"scope_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","display_name":"raw-cli-workspace"},"snapshots":[{"snapshot_id":"ckp_00000000-0000-0000-0000-000000000654","kind":"baseline","run_id":"run_00000000-0000-0000-0000-000000000111"}]}'
    ;;
  *' snapshot preview tsk_00000000-0000-0000-0000-000000000321 ckp_00000000-0000-0000-0000-000000000654 '*)
    printf '%s\n' '{"event":"task_snapshot_preview","task_id":"tsk_00000000-0000-0000-0000-000000000321","state":"succeeded","revision":9,"workspace":{"scope_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","display_name":"raw-cli-workspace"},"snapshot_id":"ckp_00000000-0000-0000-0000-000000000654","changes":[{"path":"src/main.rs","change":"modified"}],"preview_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}'
    ;;
  *' snapshot diff tsk_00000000-0000-0000-0000-000000000321 ckp_00000000-0000-0000-0000-000000000654 '*)
    printf '%s\n' '{"event":"task_snapshot_preview","task_id":"tsk_00000000-0000-0000-0000-000000000321","state":"succeeded","revision":9,"workspace":{"scope_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","display_name":"raw-cli-workspace"},"snapshot_id":"ckp_00000000-0000-0000-0000-000000000654","changes":[{"path":"src/main.rs","change":"modified"}],"preview_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}'
    ;;
  *' snapshot switch tsk_00000000-0000-0000-0000-000000000321 ckp_00000000-0000-0000-0000-000000000654 '*)
    if [ "${TASK_SWITCH_CWD_ERROR:-}" = 1 ]; then
      printf '%s\n' 'CwdOccupied: process cwd is inside workspace' >&2
      exit 2
    fi
    printf '%s\n' '{"event":"task_snapshot_switched","task_id":"tsk_00000000-0000-0000-0000-000000000321","snapshot_id":"ckp_00000000-0000-0000-0000-000000000654","recovery_snapshot_id":"ckp_00000000-0000-0000-0000-000000000999","from":"live-before-switch","to":"ckp_00000000-0000-0000-0000-000000000654"}'
    ;;
  *)
    printf '%s\n' 'unexpected Task snapshot command' >&2
    exit 2
    ;;
esac
"#;

#[test]
fn raw_cli_lists_task_owned_snapshots_without_reaching_bash() {
    let home = temp_shell_home("task-snapshot-list");
    let gateway = home.join("cosh-gateway");
    let argv_log = home.join("gateway.argv");
    let socket = home.join("gateway.sock");
    write_executable(&gateway, SNAPSHOT_GATEWAY);

    let home_text = home.to_string_lossy().to_string();
    let gateway_text = gateway.to_string_lossy().to_string();
    let argv_text = argv_log.to_string_lossy().to_string();
    let socket_text = socket.to_string_lossy().to_string();
    let command = format!("/task snapshots {SNAPSHOT_TASK_ID}\n");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_text),
            ("COSH_GATEWAY_EXECUTABLE", &gateway_text),
            ("COSH_GATEWAY_SOCKET", &socket_text),
            ("TASK_GATEWAY_ARGV", &argv_text),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", command.as_bytes()),
            ("Task snapshots", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains(SNAPSHOT_TASK_ID), "{output}");
    assert!(output.contains(SNAPSHOT_ID), "{output}");
    assert!(output.contains("baseline"), "{output}");
    assert!(!output.contains("bash: /task"), "{output}");
    let argv = fs::read_to_string(&argv_log).expect("Gateway argv log");
    assert_eq!(argv.matches(" snapshot list ").count(), 1, "{argv}");

    let _ = fs::remove_dir_all(home);
}

#[test]
fn raw_cli_snapshot_switch_confirms_exact_preview_once() {
    let home = temp_shell_home("task-snapshot-switch");
    let gateway = home.join("cosh-gateway");
    let argv_log = home.join("gateway.argv");
    let socket = home.join("gateway.sock");
    write_executable(&gateway, SNAPSHOT_GATEWAY);

    let home_text = home.to_string_lossy().to_string();
    let gateway_text = gateway.to_string_lossy().to_string();
    let argv_text = argv_log.to_string_lossy().to_string();
    let socket_text = socket.to_string_lossy().to_string();
    let command = format!("/task snapshot switch {SNAPSHOT_TASK_ID} {SNAPSHOT_ID}\n");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_text),
            ("COSH_GATEWAY_EXECUTABLE", &gateway_text),
            ("COSH_GATEWAY_SOCKET", &socket_text),
            ("TASK_GATEWAY_ARGV", &argv_text),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", command.as_bytes()),
            ("Switch workspace to this Task snapshot?", b"\x1b[D\n"),
            ("Task snapshot switched", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("src/main.rs"), "{output}");
    assert!(output.contains("Task snapshot switched"), "{output}");
    assert!(!output.contains("bash: /task"), "{output}");
    let argv = fs::read_to_string(&argv_log).expect("Gateway argv log");
    assert_eq!(argv.matches(" snapshot preview ").count(), 1, "{argv}");
    assert_eq!(argv.matches(" snapshot switch ").count(), 1, "{argv}");
    let switch = argv
        .lines()
        .find(|line| line.contains(" snapshot switch "))
        .expect("snapshot switch argv");
    assert!(
        switch.contains(&format!("--preview-digest {SNAPSHOT_PREVIEW_DIGEST}")),
        "{switch}"
    );
    assert!(switch.contains("--expected-revision 9"), "{switch}");
    assert!(
        switch.contains("--idempotency-key cosh-shell-snapshot-switch-"),
        "{switch}"
    );

    let _ = fs::remove_dir_all(home);
}

#[test]
fn raw_cli_snapshot_switch_defaults_to_cancel_without_mutation() {
    let home = temp_shell_home("task-snapshot-switch-cancel");
    let gateway = home.join("cosh-gateway");
    let argv_log = home.join("gateway.argv");
    let socket = home.join("gateway.sock");
    write_executable(&gateway, SNAPSHOT_GATEWAY);

    let home_text = home.to_string_lossy().to_string();
    let gateway_text = gateway.to_string_lossy().to_string();
    let argv_text = argv_log.to_string_lossy().to_string();
    let socket_text = socket.to_string_lossy().to_string();
    let command = format!("/task snapshot switch {SNAPSHOT_TASK_ID} {SNAPSHOT_ID}\n");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_text),
            ("COSH_GATEWAY_EXECUTABLE", &gateway_text),
            ("COSH_GATEWAY_SOCKET", &socket_text),
            ("TASK_GATEWAY_ARGV", &argv_text),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", command.as_bytes()),
            ("Switch workspace to this Task snapshot?", b"\n"),
            ("Snapshot switch cancelled", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("The workspace was not changed"), "{output}");
    let argv = fs::read_to_string(&argv_log).expect("Gateway argv log");
    assert_eq!(argv.matches(" snapshot preview ").count(), 1, "{argv}");
    assert!(!argv.contains(" snapshot switch "), "{argv}");

    let _ = fs::remove_dir_all(home);
}

#[test]
fn raw_cli_snapshot_preview_and_diff_render_read_only_notices() {
    let home = temp_shell_home("task-snapshot-readonly");
    let gateway = home.join("cosh-gateway");
    let argv_log = home.join("gateway.argv");
    let socket = home.join("gateway.sock");
    write_executable(&gateway, SNAPSHOT_GATEWAY);

    let home_text = home.to_string_lossy().to_string();
    let gateway_text = gateway.to_string_lossy().to_string();
    let argv_text = argv_log.to_string_lossy().to_string();
    let socket_text = socket.to_string_lossy().to_string();
    let preview = format!("/task snapshot preview {SNAPSHOT_TASK_ID} {SNAPSHOT_ID}\n");
    let diff = format!("/task snapshot diff {SNAPSHOT_TASK_ID} {SNAPSHOT_ID}\n");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_text),
            ("COSH_GATEWAY_EXECUTABLE", &gateway_text),
            ("COSH_GATEWAY_SOCKET", &socket_text),
            ("TASK_GATEWAY_ARGV", &argv_text),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", preview.as_bytes()),
            ("Snapshot switch preview", b""),
            ("cosh-osc$ ", diff.as_bytes()),
            ("Task snapshot diff", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(output.contains("modified"), "{output}");
    assert!(output.contains("src/main.rs"), "{output}");
    assert!(output.contains("raw-cli-workspace"), "{output}");
    assert!(!output.contains("bash: /task"), "{output}");
    let argv = fs::read_to_string(&argv_log).expect("Gateway argv log");
    assert_eq!(argv.matches(" snapshot preview ").count(), 1, "{argv}");
    assert_eq!(argv.matches(" snapshot diff ").count(), 1, "{argv}");
    assert!(!argv.contains(" snapshot switch "), "{argv}");

    let _ = fs::remove_dir_all(home);
}

#[test]
fn raw_cli_snapshot_switch_explains_cwd_occupant_block() {
    let home = temp_shell_home("task-snapshot-switch-cwd");
    let gateway = home.join("cosh-gateway");
    let argv_log = home.join("gateway.argv");
    let socket = home.join("gateway.sock");
    write_executable(&gateway, SNAPSHOT_GATEWAY);

    let home_text = home.to_string_lossy().to_string();
    let gateway_text = gateway.to_string_lossy().to_string();
    let argv_text = argv_log.to_string_lossy().to_string();
    let socket_text = socket.to_string_lossy().to_string();
    let command = format!("/task snapshot switch {SNAPSHOT_TASK_ID} {SNAPSHOT_ID}\n");
    let output = run_raw_cli_with_args_env_current_dir_and_marker_input(
        "fake",
        &[],
        &[
            ("HOME", &home_text),
            ("COSH_GATEWAY_EXECUTABLE", &gateway_text),
            ("COSH_GATEWAY_SOCKET", &socket_text),
            ("TASK_GATEWAY_ARGV", &argv_text),
            ("TASK_SWITCH_CWD_ERROR", "1"),
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &[
            ("cosh-osc$ ", command.as_bytes()),
            ("Switch workspace to this Task snapshot?", b"\x1b[D\n"),
            ("Snapshot switch blocked", b""),
            ("cosh-osc$ ", b"exit\n"),
        ],
    );

    assert!(
        output.contains("restart COSH from outside the workspace"),
        "{output}"
    );
    assert!(output.contains("Preview and diff remain safe"), "{output}");
    assert!(!output.contains("Task snapshot switched"), "{output}");
    let argv = fs::read_to_string(&argv_log).expect("Gateway argv log");
    assert_eq!(argv.matches(" snapshot switch ").count(), 1, "{argv}");

    let _ = fs::remove_dir_all(home);
}
