use super::*;

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
