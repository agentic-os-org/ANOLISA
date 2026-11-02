use super::*;
use actix_web::{App, test as awtest};
use agentsight_trajectory_collector::{TrajectoryRecord, TrajectoryStore};
use cosh_types::checkpoint::{
    ChangeType, DiffEntry, GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2, GuardedRollbackEvidenceV2,
    GuardedRollbackOutcomeV2, WorkspaceGenerationTokenV2, WsCkptRequest, WsCkptResponse,
};
use std::{
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    os::unix::net::UnixListener,
    sync::Arc,
    thread,
};

fn document() -> AtifTrajectory {
    serde_json::from_value(json!({
        "agent": {"name": "fixture", "version": "test"}, "session_id": "fixture-session",
        "steps": [
            {"step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:00Z", "message": "",
             "tool_calls": [{"tool_call_id": "call-1", "function_name": "ws-ckpt-checkpoint", "arguments": {"workspace": "/workspace/fixture"}}]},
            {"step_id": 2, "source": "agent", "timestamp": "2026-01-01T00:00:10Z", "message": "",
             "observation": {"results": [{"source_call_id": "call-1", "content": {"success": true}}]}},
            {"step_id": 3, "source": "agent", "timestamp": "2026-01-01T00:00:10Z", "message": "Q&A"}
        ]
    })).unwrap()
}

#[test]
fn literal_workspace_and_success_are_required() {
    let mut doc = document();
    let cfg = Config::default();
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/fixture")
    );
    let call = &mut doc.steps[0].tool_calls.as_mut().unwrap()[0];
    call.function_name = "Bash".into();
    for cmd in [
        "ws-ckpt checkpoint -w '/workspace/with space' -i s1",
        "ws-ckpt status --workspace='/workspace/with space'",
    ] {
        call.arguments = json!({"command": cmd});
        assert_eq!(
            explicit_workspace(call).as_deref(),
            Some("/workspace/with space")
        );
    }
    for cmd in [
        "echo ws-ckpt -w /wrong",
        "ws-ckpt status -w $WS",
        "ws-ckpt status -w /one; ws-ckpt status -w /two",
    ] {
        call.arguments = json!({"command": cmd});
        assert!(explicit_workspace(call).is_none());
    }
    let result = &mut doc.steps[1].observation.as_mut().unwrap().results[0];
    result.content = Some(json!({"success": false, "exit_code": 0}));
    assert!(!succeeded(result, "Bash"));
    result.content = None;
    assert!(!succeeded(result, "Bash"));
    assert!(workspace(&doc, &cfg).unwrap().is_none());
}

#[test]
fn colorized_checkpoint_receipt_resolves_claude_workspace() {
    let mut doc = document();
    doc.agent.name = "claude-code".into();
    let call = &mut doc.steps[0].tool_calls.as_mut().unwrap()[0];
    call.function_name = "Bash".into();
    call.arguments = json!({"command": "ws-ckpt checkpoint -w /workspace/claude -s claude-accept"});
    doc.steps[1].observation.as_mut().unwrap().results[0].content = Some(json!(
        "\x1b[32m✓ Checkpoint created: claude-accept\x1b[0m\nCompleted in 0.013s"
    ));
    assert_eq!(
        workspace(&doc, &Config::default()).unwrap().as_deref(),
        Some("/workspace/claude")
    );
}

#[test]
fn qwen_shell_resolves_workspace_only_after_successful_command() {
    let mut doc = document();
    doc.agent.name = "QwenCode".into();
    let call = &mut doc.steps[0].tool_calls.as_mut().unwrap()[0];
    call.function_name = "run_shell_command".into();
    let command = "ws-ckpt checkpoint -w /workspace/qwen -s qwen-accept";
    call.arguments = json!({"command": command});
    let cfg = Config::default();
    let output = format!(
        "Command: {command}\nDirectory: (root)\nOutput: ✓ Checkpoint created: qwen-accept\nCompleted in 0.016s"
    );
    let footer = "\nError: (none)\nExit Code: 0\nSignal: (none)\nProcess Group PGID: (none)\n\nArtifact reminder.";
    doc.steps[1].observation.as_mut().unwrap().results[0].content =
        Some(json!(format!("{output}{footer}")));
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/qwen")
    );

    // The tool footer takes precedence over any success-looking command output.
    doc.steps[1].observation.as_mut().unwrap().results[0].content = Some(json!(format!(
        "{output}{footer}\nError: failed\nExit Code: 1\nSignal: (none)\nProcess Group PGID: (none)"
    )));
    assert_eq!(workspace(&doc, &cfg).unwrap(), None);
}

#[test]
fn cosh_checkpoint_receipt_resolves_workspace_on_the_same_daemon() {
    let mut doc = document();
    let call = &mut doc.steps[0].tool_calls.as_mut().unwrap()[0];
    call.function_name = "shell".into();
    call.arguments = json!({"command": "/bin/cosh-cli checkpoint create --workspace /workspace/cosh --id cosh-accept --socket /run/cosh-test.sock"});
    let receipt = json!({
        "ok": true,
        "data": {"workspace": "/workspace/cosh", "snapshot_id": "cosh-accept"},
        "meta": {"subsystem": "checkpoint", "dry_run": false}
    });
    doc.steps[1].observation.as_mut().unwrap().results[0].content =
        Some(json!(receipt.to_string()));
    let cfg = Config {
        socket: Some("/run/cosh-test.sock".into()),
        ..Config::default()
    };
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/cosh")
    );
    let other_daemon = Config {
        socket: Some("/run/other.sock".into()),
        ..Config::default()
    };
    assert_eq!(workspace(&doc, &other_daemon).unwrap(), None);
    let mut failed_receipt = receipt;
    failed_receipt["ok"] = json!(false);
    doc.steps[1].observation.as_mut().unwrap().results[0].content =
        Some(json!(failed_receipt.to_string()));
    assert_eq!(workspace(&doc, &cfg).unwrap(), None);
}

#[test]
fn latest_successful_bash_with_dynamic_workspace_keeps_last_known_workspace() {
    let mut doc = document();
    let mut call = doc.steps[0].tool_calls.as_ref().unwrap()[0].clone();
    call.tool_call_id = "call-2".into();
    call.function_name = "Bash".into();
    doc.steps[2].tool_calls = Some(vec![call]);
    doc.steps[2].timestamp = Some("2026-01-01T00:00:20Z".into());
    doc.steps[2].observation = doc.steps[1].observation.clone();
    doc.steps[2].observation.as_mut().unwrap().results[0].source_call_id = Some("call-2".into());
    let cfg = Config::default();
    for command in [
        r#"ws-ckpt checkpoint -w "$WS" -i later"#,
        r#"/usr/local/bin/ws-ckpt checkpoint --workspace="${WS}" -i later"#,
        r#"ws-ckpt checkpoint -w "$(pwd)" -i later"#,
    ] {
        doc.steps[2].tool_calls.as_mut().unwrap()[0].arguments = json!({"command": command});
        assert_eq!(
            workspace(&doc, &cfg).unwrap().as_deref(),
            Some("/workspace/fixture"),
            "unresolved Bash arguments keep the last known workspace: {command}"
        );
    }
    doc.steps[2].tool_calls.as_mut().unwrap()[0].arguments =
        json!({"command": "echo 'ws-ckpt checkpoint -w /other'"});
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/fixture")
    );
    doc.steps[2].tool_calls.as_mut().unwrap()[0].arguments =
        json!({"command": "ws-ckpt checkpoint -w /workspace/newer -i later"});
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/newer")
    );
    doc.steps[2].tool_calls.as_mut().unwrap()[0].arguments =
        json!({"command": r#"ws-ckpt checkpoint -w "$WS" -i later"#});
    doc.steps[2].observation.as_mut().unwrap().results[0].content = Some(json!({"success": false}));
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/fixture")
    );
}

#[test]
fn latest_unresolved_success_must_not_reuse_an_earlier_workspace() {
    let mut doc = document();
    let mut latest = doc.steps[0].clone();
    latest.step_id = 4;
    latest.timestamp = Some("2026-01-01T00:00:20Z".into());
    let call = &mut latest.tool_calls.as_mut().unwrap()[0];
    call.tool_call_id = "call-2".into();
    call.arguments = json!({"id": "later-snapshot"});
    doc.steps.push(latest);
    let mut result = doc.steps[1].clone();
    result.step_id = 5;
    result.timestamp = Some("2026-01-01T00:00:21Z".into());
    result.observation.as_mut().unwrap().results[0].source_call_id = Some("call-2".into());
    doc.steps.push(result);

    let mut cfg = Config::default();
    assert_eq!(workspace(&doc, &cfg).unwrap(), None, "missing binding");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin-config");
    std::fs::write(&path, r#"{"workspace":"/workspace/newer"}"#).unwrap();
    cfg.plugins.insert(
        "fixture".into(),
        Plugin {
            runtime: "openclaw".into(),
            config_path: path.clone(),
            workspace_override: None,
        },
    );
    assert_eq!(workspace(&doc, &cfg).unwrap(), None, "changed config");
    let old = timestamp("2025-01-01T00:00:00Z").unwrap();
    let set_old_mtime = || {
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old.into()))
            .unwrap();
    };
    set_old_mtime();
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/newer")
    );
    cfg.plugins.get_mut("fixture").unwrap().workspace_override = Some("/workspace/override".into());
    assert_eq!(
        workspace(&doc, &cfg).unwrap(),
        None,
        "unknown override history"
    );
    cfg.plugins.get_mut("fixture").unwrap().workspace_override = None;
    std::fs::write(&path, "{}").unwrap();
    set_old_mtime();
    assert_eq!(workspace(&doc, &cfg).unwrap(), None, "missing workspace");
    doc.steps[3].timestamp = None;
    assert_eq!(workspace(&doc, &cfg).unwrap(), None, "missing call time");

    doc.steps[4].observation.as_mut().unwrap().results[0].content = Some(json!({"success": false}));
    assert_eq!(
        workspace(&doc, &cfg).unwrap().as_deref(),
        Some("/workspace/fixture"),
        "failed newer operation does not supersede the workspace"
    );
}

#[test]
fn plugin_config_is_bound_to_an_instance_and_not_backfilled() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("plugin-config");
    let mut doc = document();
    doc.steps[0].tool_calls.as_mut().unwrap()[0].arguments = json!({"id": "s1"});
    for (runtime, raw) in [
        ("openclaw", r#"{"workspace":"/workspace/configured"}"#),
        (
            "hermes",
            "plugins:\n  ws-ckpt:\n    workspace: /workspace/configured\n",
        ),
    ] {
        std::fs::write(&path, raw).unwrap();
        let old: std::time::SystemTime = timestamp("2025-01-01T00:00:00Z").unwrap().into();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        let mut cfg = Config::default();
        cfg.plugins.insert(
            "fixture".into(),
            Plugin {
                runtime: runtime.into(),
                config_path: path.clone(),
                workspace_override: None,
            },
        );
        assert_eq!(
            workspace(&doc, &cfg).unwrap().as_deref(),
            Some("/workspace/configured")
        );
        cfg.plugins.get_mut("fixture").unwrap().workspace_override = Some("/workspace/env".into());
        cfg.modified_at = Some(timestamp("2025-01-01T00:00:00Z").unwrap());
        assert_eq!(
            workspace(&doc, &cfg).unwrap().as_deref(),
            Some("/workspace/env")
        );
        cfg.modified_at = Some(Utc::now());
        assert!(
            workspace(&doc, &cfg).unwrap().is_none(),
            "new environment binding cannot identify an old call"
        );
        std::fs::write(&path, raw).unwrap();
        assert!(
            workspace(&doc, &cfg).unwrap().is_none(),
            "new config cannot identify an old operation"
        );
    }
}

#[test]
fn automatic_snapshots_use_the_bound_runtime_config_without_tool_calls() {
    let dir = tempfile::tempdir().unwrap();
    let old = timestamp("2025-01-01T00:00:00Z").unwrap();
    let mut cfg = Config {
        modified_at: Some(old),
        ..Config::default()
    };
    for (agent, runtime, raw, expected) in [
        (
            "OpenClaw",
            "openclaw",
            r#"{"workspace":"/workspace/openclaw","autoCheckpoint":true}"#,
            "/workspace/openclaw",
        ),
        (
            "hermes",
            "hermes",
            "plugins:\n  ws-ckpt:\n    workspace: /workspace/hermes\n    autoCheckpoint: true\n",
            "/workspace/hermes",
        ),
    ] {
        let path = dir.path().join(agent);
        let set_config = |raw: &str| {
            std::fs::write(&path, raw).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(old.into()))
                .unwrap();
        };
        set_config(raw);
        cfg.plugins.insert(
            agent.into(),
            Plugin {
                runtime: runtime.into(),
                config_path: path.clone(),
                workspace_override: None,
            },
        );

        let mut doc = document();
        doc.agent.name = agent.into();
        assert_eq!(
            workspace(&doc, &cfg).unwrap().as_deref(),
            Some("/workspace/fixture")
        );
        doc.steps[0].tool_calls = None;
        assert_eq!(workspace(&doc, &cfg).unwrap().as_deref(), Some(expected));
        set_config(&raw.replace("true", "false"));
        assert_eq!(workspace(&doc, &cfg).unwrap(), None);
        std::fs::write(&path, raw).unwrap();
        assert_eq!(workspace(&doc, &cfg).unwrap(), None);
    }
}

fn snapshot(id: &str, time: &str) -> Value {
    json!({"id": id, "workspace": "/workspace/fixture", "meta": {"created_at": time, "message": "fixture"}})
}

#[test]
fn hermes_checkpoint_result_overrides_the_logged_step_interval() {
    let mut doc = document();
    doc.agent.name = "hermes".into();
    let mut checkpoint = doc.steps[0].clone();
    checkpoint.step_id = 4;
    checkpoint.timestamp = Some("2026-01-01T00:00:11Z".into());
    checkpoint.tool_calls = Some(vec![
        serde_json::from_value(json!({
            "tool_call_id": "checkpoint-call", "function_name": "terminal",
            "arguments": {"command": "/usr/bin/ws-ckpt checkpoint -w /workspace/fixture -s s1"},
            "extra": {"result_timestamp": "2026-01-01T00:00:11.001Z"}
        }))
        .unwrap(),
    ]);
    checkpoint.observation = Some(
        serde_json::from_value(json!({"results": [{
            "source_call_id": "checkpoint-call",
            "content": r#"{"output":"✓ Checkpoint created: s1\nCompleted in 0.012s","exit_code":0}"#
        }]}))
        .unwrap(),
    );
    doc.steps.push(checkpoint);
    let as_of = timestamp("2026-01-01T00:00:20Z").unwrap();
    let rows = vec![
        snapshot("s1", "2026-01-01T00:00:09.5Z"),
        snapshot("unrelated", "2026-01-01T00:00:09.6Z"),
    ];
    let checkpoints = hermes_checkpoint_steps(&doc, "/workspace/fixture", "ws-fixture");
    let value = associate(&doc, rows.clone(), as_of, &checkpoints);
    assert_eq!(value["step_snapshots"]["4"][0]["id"], "s1");
    assert_eq!(value["step_snapshots"]["1"][0]["id"], "unrelated");
    assert!(hermes_checkpoint_steps(&doc, "/workspace/other", "ws-other").is_empty());

    let result = &mut doc.steps[3].observation.as_mut().unwrap().results[0];
    result.source_call_id = Some("different-call".into());
    assert!(hermes_checkpoint_steps(&doc, "/workspace/fixture", "ws-fixture").is_empty());
    let result = &mut doc.steps[3].observation.as_mut().unwrap().results[0];
    result.source_call_id = Some("checkpoint-call".into());
    result.content = Some(json!({"output": "Checkpoint created: s1", "exit_code": 1}));
    assert!(hermes_checkpoint_steps(&doc, "/workspace/fixture", "ws-fixture").is_empty());
}

#[test]
fn intervals_preserve_precision_equal_boundaries_and_fixed_tail() {
    let doc = document();
    let end = timestamp("2026-01-01T00:00:20Z").unwrap();
    let value = associate(
        &doc,
        vec![
            snapshot("before", "2025-12-31T23:59:59Z"),
            snapshot("nanosecond", "2026-01-01T00:00:09.999999999Z"),
            snapshot("boundary", "2026-01-01T08:00:10+08:00"),
            snapshot("tail", "2026-01-01T00:00:19Z"),
            snapshot("cutoff", "2026-01-01T00:00:20Z"),
            snapshot("invalid", "not a timestamp"),
        ],
        end,
        &[],
    );
    assert_eq!(value["step_snapshots"]["1"][0]["id"], "nanosecond");
    assert!(value["step_snapshots"].get("2").is_none());
    assert_eq!(value["step_snapshots"]["3"].as_array().unwrap().len(), 2);
    assert_eq!(value["unassigned"].as_array().unwrap().len(), 3);
    let clock = associate(
        &doc,
        vec![snapshot("clock", "2026-01-01T00:00:05Z")],
        timestamp("2026-01-01T00:00:08Z").unwrap(),
        &[],
    );
    assert!(clock["step_snapshots"].as_object().unwrap().is_empty());
    assert_eq!(clock["unassigned"][0]["reason"], "step_clock_after_query");
    let mut broken = doc.clone();
    broken.steps[1].timestamp = None;
    assert!(
        associate(
            &broken,
            vec![snapshot("gap", "2026-01-01T00:00:05Z")],
            end,
            &[]
        )["step_snapshots"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    broken.steps[1].timestamp = Some("2025-12-31T23:59:59Z".into());
    assert!(
        associate(
            &broken,
            vec![snapshot("reverse", "2026-01-01T00:00:05Z")],
            end,
            &[],
        )["step_snapshots"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

fn state(dir: &Path, config: Value) -> web::Data<AppState> {
    let path = dir.join("agentsight.json");
    std::fs::write(&path, json!({"ws_ckpt": config}).to_string()).unwrap();
    let mut state = super::super::tests::test_app_state(1000).into_inner();
    Arc::get_mut(&mut state).unwrap().config_path = Some(path);
    Arc::get_mut(&mut state).unwrap().storage_path = dir.join("genai_events.db");
    web::Data::from(state)
}

#[actix_web::test]
async fn api_joins_real_cli_json_pages_and_rejects_mixed_revisions() {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("ws-ckpt");
    std::fs::write(&binary, r#"#!/bin/sh
case "$1" in
status) printf '%s' '{"workspaces":[{"ws_id":"ws-123456","path":"/workspace/fixture","snapshot_count":2}]}' ;;
list) case " $* " in
  *' --cursor page2 '*) printf '%s' '{"snapshots":[{"id":"s2","meta":{"created_at":"2026-01-01T00:00:12Z"}}],"next_cursor":null}' ;;
  *) printf '%s' '{"snapshots":[{"id":"s1","meta":{"created_at":"2026-01-01T00:00:01Z"}}],"next_cursor":"page2"}' ;;
esac ;;
*) exit 1 ;;
esac
"#).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut data = state(dir.path(), json!({"binary": binary})).into_inner();
    Arc::get_mut(&mut data).unwrap().auth = Arc::new(super::super::auth::DashboardAuth::init(
        &crate::config::ServerAuthConfig { enabled: true },
        dir.path(),
    ));
    let data = web::Data::from(data);
    let store = TrajectoryStore::new_with_path(&dir.path().join("trajectories.db")).unwrap();
    store
        .upsert_trajectory(&TrajectoryRecord {
            session_id: "fixture-session".into(),
            schema_version: "ATIF-v1.7".into(),
            agent_name: "fixture".into(),
            model_name: None,
            num_steps: 3,
            total_prompt_tokens: None,
            total_completion_tokens: None,
            start_time: None,
            end_time: None,
            first_user_message: None,
            last_user_message: None,
            atif_json: serde_json::to_string(&document()).unwrap(),
            project: "fixture".into(),
            source: "fixture".into(),
            is_subagent: false,
            file_path: "fixture.jsonl".into(),
            file_size: 0,
            file_mtime_ns: 0,
        })
        .unwrap();
    *data.trajectory_store.write().unwrap() = Some(Arc::new(store));
    let app = awtest::init_service(
        App::new()
            .app_data(data)
            .service(get_snapshots)
            .service(snapshot_availability),
    )
    .await;
    for (id, expected) in [
        ("fixture-session", StatusCode::NO_CONTENT),
        ("missing", StatusCode::NOT_FOUND),
    ] {
        let response = awtest::call_service(
            &app,
            awtest::TestRequest::default()
                .method(actix_web::http::Method::HEAD)
                .uri(&format!("/ws-ckpt/session/{id}/snapshots"))
                .to_request(),
        )
        .await;
        assert_eq!(response.status(), expected);
        assert!(awtest::read_body(response).await.is_empty());
    }
    let first: Value = awtest::call_and_read_body_json(
        &app,
        awtest::TestRequest::get()
            .uri("/ws-ckpt/trajectory/fixture-session/snapshots")
            .to_request(),
    )
    .await;
    assert_eq!(first["scope"]["path"], "/workspace/fixture");
    assert_eq!(first["step_snapshots"]["1"][0]["id"], "s1");
    assert_eq!(first["restore_enabled"], false);
    let cursor = first["completeness"]["next_cursor"].as_str().unwrap();
    let uri = format!("/ws-ckpt/trajectory/fixture-session/snapshots?cursor={cursor}");
    let second: Value =
        awtest::call_and_read_body_json(&app, awtest::TestRequest::get().uri(&uri).to_request())
            .await;
    assert_eq!(second["association"], first["association"]);
    assert_eq!(second["step_snapshots"]["3"][0]["id"], "s2");
    assert_eq!(second["completeness"]["complete"], true);
    let mut invalid: Cursor =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(cursor).unwrap()).unwrap();
    invalid.revision = "stale".into();
    let uri = format!(
        "/ws-ckpt/trajectory/fixture-session/snapshots?cursor={}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&invalid).unwrap())
    );
    assert_eq!(
        awtest::call_service(&app, awtest::TestRequest::get().uri(&uri).to_request())
            .await
            .status(),
        StatusCode::CONFLICT
    );
}

#[actix_web::test]
async fn restore_is_disabled_and_confirmation_tokens_cannot_be_forged() {
    let dir = tempfile::tempdir().unwrap();
    let data = state(dir.path(), json!({}));
    let app = awtest::init_service(
        App::new()
            .app_data(data.clone())
            .service(prepare)
            .service(execute),
    )
    .await;
    let request = awtest::TestRequest::post()
        .uri("/ws-ckpt/restore/prepare")
        .set_json(json!({
            "target": {"workspace": "ws-123456", "snapshot_id": "s1"}, "writers_stopped": true,
            "expected_preview": fixture_fingerprint()
        }))
        .to_request();
    assert!(
        !awtest::call_service(&app, request)
            .await
            .status()
            .is_success()
    );
    let request = awtest::TestRequest::post()
        .uri("/ws-ckpt/restore/execute")
        .set_json(json!({"confirmation_token": "forged"}))
        .to_request();
    assert!(
        !awtest::call_service(&app, request)
            .await
            .status()
            .is_success()
    );
    assert!(open_confirmation(&data, "forged").is_err());
    assert!(!dir.path().join(".secret_salt").exists());
}

fn restore_state(dir: &Path, socket: String) -> web::Data<AppState> {
    let binary = dir.join("ws-ckpt");
    std::fs::write(
        &binary,
        r#"#!/bin/sh
case "$1" in
status) printf '%s' '{"workspaces":[{"ws_id":"ws-123456","path":"/workspace/fixture"}]}' ;;
*) exit 1 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut state = state(
        dir,
        json!({
            "binary": binary, "socket": socket, "restore_enabled": true
        }),
    )
    .into_inner();
    Arc::get_mut(&mut state).unwrap().auth = Arc::new(super::super::auth::DashboardAuth::init(
        &crate::config::ServerAuthConfig { enabled: true },
        dir,
    ));
    web::Data::from(state)
}

fn guarded_preview(request: &WsCkptRequest) -> WsCkptResponse {
    let WsCkptRequest::GuardedRollbackPreviewV2 {
        registered_path,
        ws_id,
        expected_generation,
        target_snapshot_id,
    } = request
    else {
        panic!("expected guarded preview")
    };
    WsCkptResponse::GuardedRollbackPreviewV2Ok {
        protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
        registered_path: registered_path.clone(),
        ws_id: ws_id.clone(),
        generation: *expected_generation,
        target_snapshot_id: target_snapshot_id.clone(),
        diff_digest: [6; 32],
        changes: vec![DiffEntry {
            path: "deleted.txt".into(),
            change_type: ChangeType::Deleted,
            detail: None,
        }],
        caller_uid: unsafe { libc::geteuid() },
    }
}

fn fixture_fingerprint() -> PreviewFingerprint {
    PreviewFingerprint {
        generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
        diff_digest: [6; 32],
    }
}

fn restore_daemon(
    count: usize,
    reply: impl Fn(&WsCkptRequest) -> WsCkptResponse + Send + 'static,
) -> (
    tempfile::TempDir,
    String,
    thread::JoinHandle<Vec<WsCkptRequest>>,
) {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("ws-ckpt.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let handle = thread::spawn(move || {
        (0..count)
            .map(|_| {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0; 4];
                stream.read_exact(&mut length).unwrap();
                let mut payload = vec![0; u32::from_le_bytes(length) as usize];
                stream.read_exact(&mut payload).unwrap();
                let request: WsCkptRequest = bincode::deserialize(&payload).unwrap();
                let payload = bincode::serialize(&reply(&request)).unwrap();
                stream
                    .write_all(&(payload.len() as u32).to_le_bytes())
                    .unwrap();
                stream.write_all(&payload).unwrap();
                request
            })
            .collect()
    });
    (dir, socket.to_string_lossy().into_owned(), handle)
}

fn identity(request: &WsCkptRequest) -> WsCkptResponse {
    let WsCkptRequest::WorkspaceIdentityV2 { registration_path } = request else {
        panic!("expected workspace identity")
    };
    WsCkptResponse::WorkspaceIdentityV2Ok {
        protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
        ws_id: "ws-123456".into(),
        registered_path: registration_path.clone(),
        generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
    }
}

#[actix_web::test]
async fn restore_requires_writer_confirmation_then_reconciles_unknown_without_retry() {
    let (socket_dir, socket, daemon) = restore_daemon(6, |request| match request {
        WsCkptRequest::WorkspaceIdentityV2 { .. } => identity(request),
        WsCkptRequest::GuardedRollbackPreviewV2 { .. } => {
            let mut response = guarded_preview(request);
            if let WsCkptResponse::GuardedRollbackPreviewV2Ok { changes, .. } = &mut response {
                let directory = "nested/".repeat(15);
                changes.extend((1..20_000).map(|index| DiffEntry {
                    path: format!("node_modules/{directory}file-{index}.js"),
                    change_type: ChangeType::Modified,
                    detail: None,
                }));
            }
            response
        }
        WsCkptRequest::GuardedRollbackV2 {
            registered_path,
            ws_id,
            expected_generation,
            target_snapshot_id,
            expected_diff_digest,
            operation_id,
            operation_digest,
        } => WsCkptResponse::GuardedRollbackV2Uncertain {
            evidence: GuardedRollbackEvidenceV2 {
                ws_id: ws_id.clone(),
                registered_path: registered_path.clone(),
                expected_generation: *expected_generation,
                target_snapshot_id: target_snapshot_id.clone(),
                expected_diff_digest: *expected_diff_digest,
                operation_id: operation_id.clone(),
                operation_digest: *operation_digest,
                caller_uid: unsafe { libc::geteuid() },
                outcome: GuardedRollbackOutcomeV2::Unknown {
                    reason: "reply lost".into(),
                },
            },
        },
        WsCkptRequest::GuardedRollbackEvidenceV2 {
            ws_id,
            operation_id,
            operation_digest,
        } => WsCkptResponse::GuardedRollbackEvidenceV2Ok {
            evidence: Some(GuardedRollbackEvidenceV2 {
                ws_id: ws_id.clone(),
                registered_path: "/workspace/fixture".into(),
                expected_generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
                target_snapshot_id: "s1".into(),
                expected_diff_digest: [6; 32],
                operation_id: operation_id.clone(),
                operation_digest: *operation_digest,
                caller_uid: unsafe { libc::geteuid() },
                outcome: GuardedRollbackOutcomeV2::Succeeded {
                    resulting_generation: WorkspaceGenerationTokenV2::from_bytes([8; 32]),
                },
            }),
        },
        _ => panic!("unexpected request: {request:?}"),
    });
    let data = restore_state(socket_dir.path(), socket.clone());
    let app = awtest::init_service(
        App::new()
            .app_data(data.clone())
            .service(diff)
            .service(prepare)
            .service(execute)
            .service(restore_result),
    )
    .await;
    let target = json!({"workspace":"ws-123456", "snapshot_id":"s1"});
    let stopped = awtest::TestRequest::post()
        .uri("/ws-ckpt/restore/prepare")
        .set_json(json!({
            "target": target, "writers_stopped": false, "expected_preview": fixture_fingerprint()
        }))
        .to_request();
    assert!(
        !awtest::call_service(&app, stopped)
            .await
            .status()
            .is_success()
    );
    let preview: Value = awtest::call_and_read_body_json(
        &app,
        awtest::TestRequest::post()
            .uri("/ws-ckpt/diff")
            .set_json(&target)
            .to_request(),
    )
    .await;
    assert_eq!(preview["changes"].as_array().unwrap().len(), 20_000);
    assert!(serde_json::to_vec(&preview["changes"]).unwrap().len() > 2 * 1024 * 1024);
    let prepare_body = json!({
        "target": target, "writers_stopped": true, "expected_preview": preview["restore_preview"]
    });
    assert!(serde_json::to_vec(&prepare_body).unwrap().len() < 1_024);
    let prepared: Value = awtest::call_and_read_body_json(
        &app,
        awtest::TestRequest::post()
            .uri("/ws-ckpt/restore/prepare")
            .set_json(prepare_body)
            .to_request(),
    )
    .await;
    let token = prepared["confirmation_token"].as_str().unwrap();
    assert!(token.len() < 1_024);
    assert!(prepared.get("preview").is_none());
    let config_path = socket_dir.path().join("agentsight.json");
    std::fs::write(
        &config_path,
        json!({"ws_ckpt":{"socket":"/changed.sock"}}).to_string(),
    )
    .unwrap();
    assert!(open_confirmation(&data, token).is_err());
    std::fs::write(
        &config_path,
        json!({"ws_ckpt":{
            "binary": socket_dir.path().join("ws-ckpt"), "socket": socket, "restore_enabled": true
        }})
        .to_string(),
    )
    .unwrap();
    let unknown: Value = awtest::call_and_read_body_json(
        &app,
        awtest::TestRequest::post()
            .uri("/ws-ckpt/restore/execute")
            .set_json(json!({"confirmation_token": token}))
            .to_request(),
    )
    .await;
    assert_eq!(unknown["status"], "unknown");
    std::fs::write(
        &config_path,
        json!({"ws_ckpt":{"socket": socket, "restore_enabled": false}}).to_string(),
    )
    .unwrap();
    let reconciled: Value = awtest::call_and_read_body_json(
        &app,
        awtest::TestRequest::post()
            .uri("/ws-ckpt/restore/result")
            .set_json(json!({"confirmation_token": token}))
            .to_request(),
    )
    .await;
    assert!(reconciled["evidence"].is_object());
    assert!(matches!(
        daemon.join().unwrap().as_slice(),
        [
            WsCkptRequest::WorkspaceIdentityV2 { .. },
            WsCkptRequest::GuardedRollbackPreviewV2 { .. },
            WsCkptRequest::WorkspaceIdentityV2 { .. },
            WsCkptRequest::GuardedRollbackPreviewV2 { .. },
            WsCkptRequest::GuardedRollbackV2 { .. },
            WsCkptRequest::GuardedRollbackEvidenceV2 { .. }
        ]
    ));
}

#[actix_web::test]
async fn explicit_restore_refusal_reports_known_no_effect() {
    let (dir, socket, daemon) = restore_daemon(1, |request| {
        assert!(matches!(request, WsCkptRequest::GuardedRollbackV2 { .. }));
        WsCkptResponse::GuardedRollbackV2Rejected {
            code: cosh_types::checkpoint::GuardedRollbackRejectionCodeV2::CwdOccupied,
            message: "writer still active".into(),
        }
    });
    let data = restore_state(dir.path(), socket.clone());
    // An older sealed token contained the entire preview and extra fields.
    let legacy = json!({
        "preview": CkptGuardedRollbackPreviewV2 {
            protocol_version: GUARDED_CHECKPOINT_PROTOCOL_VERSION_V2,
            registered_path: "/workspace/fixture".into(),
            ws_id: "ws-123456".into(),
            generation: WorkspaceGenerationTokenV2::from_bytes([7; 32]),
            target_snapshot_id: "s1".into(),
            diff_digest: [6; 32],
            changes: vec![],
            caller_uid: unsafe { libc::geteuid() },
        },
        "operation_id": uuid::Uuid::new_v4().to_string(),
        "operation_digest": vec![9u8; 32],
        "socket": socket,
        "protection_snapshot_id": "protection-fixture"
    });
    let token = secret::seal(&legacy.to_string(), dir.path()).unwrap();
    let app = awtest::init_service(App::new().app_data(data).service(execute)).await;
    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri("/ws-ckpt/restore/execute")
            .set_json(json!({"confirmation_token": token}))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body: Value = awtest::read_body_json(response).await;
    assert_eq!(body["effect"], "known_no_effect");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("refresh changes and confirm again")
    );
    assert_eq!(daemon.join().unwrap().len(), 1);
}

#[actix_web::test]
async fn restore_rejects_changed_preview_before_execute() {
    let (socket_dir, socket, daemon) = restore_daemon(4, |request| match request {
        WsCkptRequest::WorkspaceIdentityV2 { .. } => identity(request),
        WsCkptRequest::GuardedRollbackPreviewV2 { .. } => guarded_preview(request),
        _ => panic!("restore must not execute after a failed prepare: {request:?}"),
    });
    let data = restore_state(socket_dir.path(), socket);
    let app =
        awtest::init_service(App::new().app_data(data).service(prepare).service(execute)).await;
    for fingerprint in [
        PreviewFingerprint {
            diff_digest: [9; 32],
            ..fixture_fingerprint()
        },
        PreviewFingerprint {
            generation: WorkspaceGenerationTokenV2::from_bytes([9; 32]),
            ..fixture_fingerprint()
        },
    ] {
        let response = awtest::call_service(&app, awtest::TestRequest::post().uri("/ws-ckpt/restore/prepare")
            .set_json(json!({"target":{"workspace":"ws-123456","snapshot_id":"s1"}, "writers_stopped":true, "expected_preview":fingerprint})).to_request()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
    assert_eq!(daemon.join().unwrap().len(), 4);
}
