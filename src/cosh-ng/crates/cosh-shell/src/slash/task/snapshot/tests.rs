use super::*;

#[test]
fn change_lines_are_bounded_and_terminal_safe() {
    let value = serde_json::json!({
        "changes": [{"path": "src/main.rs\u{1b}[2J", "change_type": "modified"}]
    });
    let lines = change_lines(&value);
    assert_eq!(lines, ["modified  src/main.rs[2J"]);
    assert!(!lines[0].contains('\u{1b}'));
}

#[test]
fn snapshot_list_accepts_gateway_identity_field_names() {
    let value = serde_json::json!({
        "snapshots": [
            {"snapshot_id": "snap-a", "kind": "baseline", "run_id": "run-a"},
            {"checkpoint_id": "snap-b", "source": "pre_effect", "run_id": "run-b", "approval_id": "approval-b"}
        ]
    });
    assert_eq!(
        snapshot_list_lines(&value),
        [
            "snap-a  baseline  run-a",
            "snap-b  pre_effect  run-b  approval-b"
        ]
    );
}

#[test]
fn only_completed_task_states_admit_snapshot_operations() {
    for state in ["succeeded", "failed", "cancelled"] {
        assert!(is_terminal_task_state(state), "{state}");
    }
    for state in [
        "submitted",
        "queued",
        "running",
        "waiting_approval",
        "waiting_input",
        "suspended",
        "unknown",
    ] {
        assert!(!is_terminal_task_state(state), "{state}");
    }
}

#[test]
fn snapshot_projection_rejects_active_and_cross_task_responses() {
    let task_id = "tsk_00000000-0000-0000-0000-000000000001";
    let active = serde_json::json!({
        "task_id": task_id,
        "state": "running",
        "revision": 4
    });
    let projection = task_projection(&active, task_id).expect("read-only snapshot projection");
    assert_eq!(projection.state, "running");
    let error = terminal_task_projection(&active, task_id).unwrap_err();
    assert!(error.contains("switching snapshots is available only after"));

    let other = serde_json::json!({
        "task_id": "tsk_00000000-0000-0000-0000-000000000002",
        "state": "succeeded",
        "revision": 5
    });
    let error = task_projection(&other, task_id).unwrap_err();
    assert!(error.contains("did not match the requested Task"));
}

#[test]
fn switch_confirmation_defaults_to_cancel() {
    let mut state = InlineState::default();
    state.task_snapshot.pending_switch = Some(PendingSnapshotSwitch {
        panel_id: "switch-panel".to_owned(),
        task_id: "tsk_00000000-0000-0000-0000-000000000001".to_owned(),
        snapshot_id: "ckp_00000000-0000-0000-0000-000000000001".to_owned(),
        task_state: "succeeded".to_owned(),
        workspace: "raw-cli-workspace".to_owned(),
        expected_revision: 7,
        preview_digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
            .to_owned(),
        idempotency_key: "switch-key".to_owned(),
        preview_lines: vec!["modified  src/main.rs".to_owned()],
        selected: SWITCH_CANCEL_INDEX,
    });
    let RawInputCapture::Question {
        selected,
        option_count,
        ..
    } = pending_task_snapshot_capture(&state).expect("snapshot confirmation capture")
    else {
        panic!("expected Question capture");
    };
    assert_eq!(selected, SWITCH_CANCEL_INDEX);
    assert_eq!(option_count, SWITCH_OPTION_COUNT);

    let mut output = Vec::new();
    render_snapshot_switch_confirmation(&mut state, &mut output).unwrap();
    let rendered = String::from_utf8_lossy(&output);
    assert!(
        rendered.contains("launched outside this managed workspace"),
        "{rendered}"
    );
}

#[test]
fn cwd_occupied_error_explains_how_to_unblock_switch() {
    let state = InlineState::default();
    let mut output = Vec::new();
    render_snapshot_error(
        &state,
        &mut output,
        "CwdOccupied: process cwd is inside workspace".to_owned(),
    )
    .unwrap();
    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("Snapshot switch blocked"), "{rendered}");
    assert!(rendered.contains("Exit this COSH session"), "{rendered}");
    assert!(
        rendered.contains("cd in the embedded shell does not move cosh-shell"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Preview and diff remain safe"),
        "{rendered}"
    );
}

#[test]
fn change_lines_disclose_gateway_omissions() {
    let value = serde_json::json!({
        "changes": [{"path": "src/main.rs", "change": "modified"}],
        "changes_omitted": 400,
    });
    let lines = change_lines(&value);
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains("400 additional changes"));
    assert!(lines[1].contains("full diff"));
}
