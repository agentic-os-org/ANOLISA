use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Mutex, OnceLock};

use super::form::{TaskCheckpoint, TaskRuntime};
use super::{
    list_tasks, render_submission_progress, render_submission_result, render_task_command,
    safe_task_field, safe_text, show_task, submit_task, MAX_RENDERED_TASK_FIELD_BYTES,
};
use crate::raw_input::RawInputCapture;
use crate::runtime::state::InlineState;

struct GatewayEnvironment;

impl GatewayEnvironment {
    fn set(executable: &std::path::Path) -> Self {
        std::env::set_var("COSH_GATEWAY_EXECUTABLE", executable);
        std::env::set_var("COSH_GATEWAY_SOCKET", "/tmp/cosh-test-gateway.sock");
        Self
    }
}

impl Drop for GatewayEnvironment {
    fn drop(&mut self) {
        std::env::remove_var("COSH_GATEWAY_EXECUTABLE");
        std::env::remove_var("COSH_GATEWAY_SOCKET");
    }
}

fn environment_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
}

#[test]
fn task_command_opens_one_form_and_prefills_natural_language_goal() {
    let _lock = environment_lock();
    let root = tempfile::tempdir().unwrap();
    let gateway = root.path().join("cosh-gateway");
    fs::write(
        &gateway,
        r#"#!/bin/sh
case " $* " in
  *' --output jsonl capabilities '*)
printf '%s\n' '{"event":"task_capabilities","launch_schema_version":1,"default_workspace":{"scope_digest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","display_name":"cosh-ng"},"runtimes":[{"runtime":"core","readiness":{"status":"ready"},"security":{"delegated_local_authority":true,"gateway_brokered_effects":false,"checkpoint_is_baseline_only":false}},{"runtime":"codex","readiness":{"status":"ready"},"security":{"delegated_local_authority":true,"gateway_brokered_effects":false,"checkpoint_is_baseline_only":false}}],"checkpoint":{"status":"ready"},"default_approval":"allow_all"}'
;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&gateway, fs::Permissions::from_mode(0o700)).unwrap();
    let _environment = GatewayEnvironment::set(&gateway);
    for (arguments, expected_goal) in [
        ("", ""),
        (
            "update the \"serde\" dependency",
            "update the \"serde\" dependency",
        ),
    ] {
        let mut state = InlineState::default();
        let mut output = Vec::new();
        assert!(!render_task_command(arguments, &mut state, &mut output).unwrap());
        let RawInputCapture::TextQuestion { initial_text, .. } =
            super::form::pending_task_form_capture(&state).expect("Task form capture")
        else {
            panic!("expected Task goal text capture");
        };
        assert_eq!(initial_text, expected_goal);
    }
}

#[test]
fn reserved_snapshot_commands_reject_invalid_arity_without_opening_a_task_form() {
    for arguments in [
        "snapshots one two",
        "snapshot",
        "snapshot preview task-only",
        "snapshot switch task snapshot extra",
    ] {
        let mut state = InlineState::default();
        let mut output = Vec::new();
        assert!(render_task_command(arguments, &mut state, &mut output).unwrap());
        assert!(
            super::form::pending_task_form_capture(&state).is_none(),
            "{arguments}"
        );
        assert!(
            super::snapshot::pending_task_snapshot_capture(&state).is_none(),
            "{arguments}"
        );
        let rendered = String::from_utf8_lossy(&output);
        assert!(rendered.contains("/task snapshot preview"), "{rendered}");
    }
}

#[test]
fn task_submit_and_list_use_the_selected_launch_policy() {
    let _lock = environment_lock();
    let root = tempfile::tempdir().unwrap();
    let gateway = root.path().join("cosh-gateway");
    let argv = root.path().join("argv");
    let goal = root.path().join("goal");
    fs::write(
        &gateway,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\ncase \" $* \" in *' submit '*) cat > '{}'; printf '%s\\n' '{{\"event\":\"task\",\"task_id\":\"tsk_00000000-0000-0000-0000-000000000001\"}}' ;; *) printf '%s\\n' '{{\"event\":\"tasks\",\"tasks\":[{{\"task_id\":\"tsk_00000000-0000-0000-0000-000000000001\",\"state\":\"succeeded\",\"revision\":7}}]}}' ;; esac\n",
            argv.display(),
            goal.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&gateway, fs::Permissions::from_mode(0o700)).unwrap();
    let _environment = GatewayEnvironment::set(&gateway);

    let task_id = submit_task(
        "update dependencies",
        TaskRuntime::Codex,
        TaskCheckpoint::On,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap();
    assert_eq!(task_id, "tsk_00000000-0000-0000-0000-000000000001");
    assert_eq!(fs::read_to_string(&goal).unwrap(), "update dependencies");
    let submitted_args = fs::read_to_string(&argv).unwrap();
    assert!(
        submitted_args.contains("--runtime codex"),
        "{submitted_args}"
    );
    assert!(
        !submitted_args.contains("--runtime-profile"),
        "{submitted_args}"
    );
    assert!(
        submitted_args.contains("--checkpoint on"),
        "{submitted_args}"
    );
    assert!(
        submitted_args.contains("--approval-policy allow-all"),
        "{submitted_args}"
    );
    assert!(
        submitted_args.contains(
            "--expected-workspace-digest \
             aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ),
        "{submitted_args}"
    );
    assert!(
        submitted_args.contains("--socket /tmp/cosh-test-gateway.sock"),
        "{submitted_args}"
    );

    assert_eq!(
        list_tasks().unwrap(),
        ["tsk_00000000-0000-0000-0000-000000000001  succeeded  revision 7"]
    );
}

#[test]
fn core_task_submit_uses_typed_runtime_and_auto_checkpoint() {
    let _lock = environment_lock();
    let root = tempfile::tempdir().unwrap();
    let gateway = root.path().join("cosh-gateway");
    let argv = root.path().join("argv");
    fs::write(
        &gateway,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{}'\ncat >/dev/null\nprintf '%s\\n' '{{\"task_id\":\"tsk_00000000-0000-0000-0000-000000000002\"}}'\n",
            argv.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&gateway, fs::Permissions::from_mode(0o700)).unwrap();
    let _environment = GatewayEnvironment::set(&gateway);

    submit_task(
        "run checks",
        TaskRuntime::Core,
        TaskCheckpoint::Auto,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .unwrap();
    let submitted_args = fs::read_to_string(&argv).unwrap();
    assert!(
        submitted_args.contains("--runtime core"),
        "{submitted_args}"
    );
    assert!(
        !submitted_args.contains("--runtime-profile"),
        "{submitted_args}"
    );
    assert!(
        submitted_args.contains("--checkpoint auto"),
        "{submitted_args}"
    );
    assert_eq!(submitted_args.matches(" submit ").count(), 1);
}

#[test]
fn task_show_replays_every_durable_event_page() {
    let _lock = environment_lock();
    let root = tempfile::tempdir().unwrap();
    let gateway = root.path().join("cosh-gateway");
    fs::write(
        &gateway,
        r#"#!/bin/sh
case " $* " in
  *' get '*)
printf '%s\n' '{"event":"task","task_id":"tsk_00000000-0000-0000-0000-000000000001","state":"succeeded","revision":5}'
;;
  *' events '*' --after 0 '*)
printf '%s\n' '{"event":"task_events","events":[{"revision":1,"event":{"event":"runtime_event_recorded","run_id":"run-1","update":{"update":"progress","summary":"我"}}},{"revision":2,"event":{"event":"runtime_event_recorded","run_id":"run-1","update":{"update":"progress","summary":"会"}}}],"has_more":true,"next_revision":2}'
;;
  *' events '*' --after 2 '*)
printf '%s\n' '{"event":"task_events","events":[{"revision":3,"event":{"event":"runtime_event_recorded","run_id":"run-1","update":{"update":"progress","summary":"读取"}}},{"revision":4,"event":{"event":"runtime_event_recorded","run_id":"run-1","update":{"update":"progress","summary":"文件"}}},{"revision":5,"event":{"event":"task_succeeded"}}],"has_more":false,"next_revision":5}'
;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&gateway, fs::Permissions::from_mode(0o700)).unwrap();
    let _environment = GatewayEnvironment::set(&gateway);

    assert_eq!(
        show_task("tsk_00000000-0000-0000-0000-000000000001").unwrap(),
        [
            "tsk_00000000-0000-0000-0000-000000000001  succeeded  revision 5",
            "我会读取文件",
            "task succeeded",
        ]
    );
}

#[test]
fn task_transcript_breaks_paragraphs_at_non_progress_events() {
    let mut transcript = super::TaskTranscript::default();
    for event in [
        serde_json::json!({
            "event": "runtime_event_recorded",
            "run_id": "run-1",
            "update": {"update": "progress", "summary": "Run"}
        }),
        serde_json::json!({
            "event": "runtime_event_recorded",
            "run_id": "run-1",
            "update": {"update": "progress", "summary": " tests"}
        }),
        serde_json::json!({"event": "approval_requested"}),
        serde_json::json!({
            "event": "runtime_event_recorded",
            "run_id": "run-1",
            "update": {"update": "progress", "summary": "Done\ncleanly"}
        }),
        serde_json::json!({"event": "task_succeeded"}),
    ] {
        transcript.record(&event);
    }
    assert_eq!(
        transcript.finish().0,
        ["Run tests", "Done cleanly", "task succeeded"]
    );
}

#[test]
fn task_result_text_drops_terminal_controls() {
    assert_eq!(safe_text("ok\u{1b}[31m\nnext"), "ok[31m next");
    let bounded = safe_task_field(&"任务".repeat(MAX_RENDERED_TASK_FIELD_BYTES));
    assert!(bounded.ends_with('…'));
    assert!(bounded.len() <= MAX_RENDERED_TASK_FIELD_BYTES + '…'.len_utf8());
    assert!(!bounded.chars().any(char::is_control));
}

#[test]
fn submission_panels_use_submitted_status_and_safe_task_id() {
    let mut state = InlineState::default();
    let mut output = Vec::new();
    render_submission_progress(&state, &mut output).unwrap();
    render_submission_result(
        Ok("tsk_123\u{1b}[2J\nspoof".to_owned()),
        &state,
        &mut output,
    )
    .unwrap();
    let english = String::from_utf8_lossy(&output);
    assert!(english.contains("Submitting persistent Task…"), "{english}");
    assert!(english.contains("Persistent Task submitted"), "{english}");
    assert!(!english.contains("Persistent Task started"), "{english}");
    assert!(english.contains("Task: tsk_123[2J spoof"), "{english}");
    assert!(!english.contains('\u{1b}'), "{english}");

    state.language = crate::config::Language::ZhCn;
    output.clear();
    render_submission_result(Err("gateway offline".to_owned()), &state, &mut output).unwrap();
    let chinese = String::from_utf8_lossy(&output);
    assert!(chinese.contains("持久 Task 未提交"), "{chinese}");
    assert!(chinese.contains("Task 未提交"), "{chinese}");
}
