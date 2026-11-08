use actix_web::{App, http::StatusCode, test as awtest, web};
use agentsight_trajectory_collector::{TrajectoryRecord, TrajectoryStore};
use chrono::TimeZone;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use super::*;
use crate::server::workspace_evidence::{WorkspaceCandidate, workspace_candidates};
use crate::server::workspace_snapshot_client::{ChangeType, DiffEntry, WsCkptClient, WsCkptError};

fn document(steps: Vec<Value>) -> AtifTrajectory {
    serde_json::from_value(json!({
        "schema_version": "ATIF-v1.7",
        "agent": {"name": "test", "version": "1"},
        "steps": steps,
    }))
    .expect("valid ATIF fixture")
}

fn tool_step(
    step_id: usize,
    timestamp: &str,
    function_name: &str,
    arguments: Value,
    outcome: Option<bool>,
) -> Value {
    let call_id = format!("call-{step_id}");
    json!({
        "step_id": step_id,
        "source": "agent",
        "timestamp": timestamp,
        "tool_calls": [{
            "tool_call_id": call_id,
            "function_name": function_name,
            "arguments": arguments,
        }],
        "observation": {"results": [{
            "source_call_id": call_id,
            "content": outcome.map(|success| json!({"success": success})),
            "extra": outcome.map(|success| json!({"is_error": !success})),
        }]},
    })
}

fn agent_step(step_id: usize, timestamp: Option<&str>) -> Value {
    json!({
        "step_id": step_id,
        "source": "agent",
        "timestamp": timestamp,
    })
}

fn snapshot(id: &str, created_at: &str) -> WorkspaceSnapshot {
    WorkspaceSnapshot {
        id: id.to_string(),
        workspace: "/tmp/work".to_string(),
        meta: super::super::workspace_snapshot_client::SnapshotMeta {
            created_at: created_at.to_string(),
            message: None,
            pinned: false,
            missing: false,
            parent_id: None,
        },
    }
}

#[cfg(unix)]
fn route_state(document: &AtifTrajectory) -> web::Data<AppState> {
    let store = TrajectoryStore::new_with_path(std::path::Path::new(":memory:"))
        .expect("in-memory trajectory store");
    store
        .upsert_trajectory(&TrajectoryRecord {
            session_id: "route-session".to_string(),
            schema_version: document.schema_version.clone(),
            agent_name: "test".to_string(),
            model_name: None,
            num_steps: document.steps.len() as i64,
            total_prompt_tokens: None,
            total_completion_tokens: None,
            start_time: None,
            end_time: None,
            first_user_message: None,
            last_user_message: None,
            atif_json: serde_json::to_string(document).expect("serialize route ATIF"),
            project: "test".to_string(),
            source: "test".to_string(),
            is_subagent: false,
            file_path: "/tmp/route-session.jsonl".to_string(),
            file_size: 1,
            file_mtime_ns: 1,
        })
        .expect("store route ATIF");
    let auth = Arc::new(crate::server::auth::DashboardAuth::init(
        &crate::config::ServerAuthConfig { enabled: false },
        std::path::Path::new("/tmp"),
    ));
    web::Data::new(AppState {
        storage_path: std::path::PathBuf::from(":memory:"),
        genai_store: None,
        start_time: Instant::now(),
        health_store: Arc::new(RwLock::new(crate::health::HealthStore::new())),
        interruption_store: None,
        evaluation_store: Arc::new(
            crate::grader::EvaluationStore::new_with_path(std::path::Path::new(":memory:"))
                .expect("in-memory evaluation store"),
        ),
        enforcement: None,
        containment: None,
        audit_service: Arc::new(agentsight_audit::AuditService::new(
            crate::security::SecurityStore::open_in_memory()
                .expect("in-memory security store")
                .audit_store(),
        )),
        security_observability: crate::server::SecurityObservabilityConfig::default(),
        auth,
        optimize: None,
        reuse_store: None,
        config_path: None,
        storage_budget: Arc::new(crate::storage_budget::StorageBudget::new(
            None,
            &crate::config::StorageConfig::default(),
        )),
        trajectory_store: Arc::new(RwLock::new(Some(Arc::new(store)))),
        reuse_llm_judge_enabled: false,
        causal_store: None,
    })
}

#[cfg(unix)]
fn route_client() -> (tempfile::TempDir, web::Data<WsCkptClient>) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary route CLI directory");
    let executable = directory.path().join("ws-ckpt");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
case "$1" in
  status) printf '%s' '{"workspaces":[{"ws_id":"ws-1","path":"/tmp/work","snapshot_count":2}]}' ;;
  list) printf '%s' '{"snapshots":[{"id":"snap-route","workspace":"/tmp/work","meta":{"created_at":"2026-01-01T10:05:00Z","pinned":false,"missing":false}},{"id":"foreign","workspace":"/tmp/other","meta":{"created_at":"2026-01-01T10:06:00Z","pinned":false,"missing":false}}],"next_cursor":null}' ;;
  diff) printf '%s' '[{"path":"new-file","change_type":"Added","detail":null}]' ;;
  *) exit 9 ;;
esac
"#,
    )
    .expect("write route CLI");
    let mut permissions = std::fs::metadata(&executable)
        .expect("route CLI metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).expect("make route CLI executable");
    let client = web::Data::new(WsCkptClient::new(executable));
    (directory, client)
}

#[cfg(unix)]
fn scripted_client(script: &str) -> (tempfile::TempDir, WsCkptClient) {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary scripted CLI directory");
    let executable = directory.path().join("ws-ckpt");
    std::fs::write(&executable, script).expect("write scripted CLI");
    let mut permissions = std::fs::metadata(&executable)
        .expect("scripted CLI metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).expect("make scripted CLI executable");
    (directory, WsCkptClient::new(executable))
}

#[test]
fn workspace_evidence_requires_successful_newer_explicit_operations() {
    let mut ignored = agent_step(1, Some("2026-01-01T09:00:00Z"));
    ignored["extra"] = json!({"cwd": "/from-cwd"});
    let document = document(vec![
        ignored,
        tool_step(
            2,
            "2026-01-01T10:00:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "/old"}),
            Some(true),
        ),
        tool_step(
            3,
            "2026-01-01T11:00:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "/failed"}),
            Some(false),
        ),
        tool_step(
            4,
            "2026-01-01T12:00:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "/unknown"}),
            None,
        ),
        tool_step(
            5,
            "2026-01-01T09:30:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "/stale"}),
            Some(true),
        ),
        tool_step(
            6,
            "2026-01-01T13:00:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "/new"}),
            Some(true),
        ),
    ]);

    let candidates = workspace_candidates(&document);
    let paths: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    assert_eq!(paths, ["/old", "/new"]);
    assert_eq!(candidates[1].step_index, 5);
}

#[test]
fn workspace_evidence_accepts_recorded_config_and_static_bash_only() {
    let document = document(vec![
        tool_step(
            1,
            "2026-01-01T10:00:00Z",
            "ws-ckpt-config",
            json!({"action": "update", "key": "workspace", "value": "/configured"}),
            Some(true),
        ),
        tool_step(
            2,
            "2026-01-01T11:00:00Z",
            "Bash",
            json!({"command": "ws-ckpt checkpoint --workspace '/from bash'"}),
            Some(true),
        ),
        tool_step(
            3,
            "2026-01-01T12:00:00Z",
            "Bash",
            json!({"command": "ws-ckpt checkpoint -w /unsafe && ws-ckpt checkpoint -w /other"}),
            Some(true),
        ),
        tool_step(
            4,
            "2026-01-01T13:00:00Z",
            "Bash",
            json!({"command": "ws-ckpt checkpoint -w $WORKSPACE"}),
            Some(true),
        ),
    ]);

    let candidates = workspace_candidates(&document);
    let paths: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    assert_eq!(paths, ["/configured", "/from bash"]);
    assert_eq!(candidates[0].evidence, "config_update");
    assert_eq!(candidates[1].evidence, "bash_argument");
}

#[test]
fn workspace_evidence_decodes_string_payloads_and_merges_call_outcomes() {
    let mut user_step = tool_step(
        1,
        "2026-01-01T10:00:00Z",
        "ws-ckpt-checkpoint",
        json!({"workspace": "/user"}),
        Some(true),
    );
    user_step["source"] = json!("user");
    let invalid_time = tool_step(
        2,
        "not-a-time",
        "ws-ckpt-checkpoint",
        json!({"workspace": "/invalid-time"}),
        Some(true),
    );
    let mut encoded = tool_step(
        3,
        "2026-01-01T11:00:00Z",
        "ws-ckpt-checkpoint",
        Value::String(r#"{"workspace":"/json-args"}"#.to_string()),
        None,
    );
    encoded["observation"]["results"][0]["content"] = json!(r#"{"success":true}"#);
    let mut merged = tool_step(
        4,
        "2026-01-01T12:00:00Z",
        "ws-ckpt-checkpoint",
        json!({"workspace": "/merged-failure"}),
        Some(true),
    );
    let results = merged["observation"]["results"]
        .as_array_mut()
        .expect("observation results array");
    results.push(json!({
        "source_call_id": "call-4",
        "content": {"success": false},
        "extra": {"is_error": true},
    }));
    results.push(json!({"content": {"success": true}}));

    let document = document(vec![
        user_step,
        invalid_time,
        encoded,
        merged,
        tool_step(
            5,
            "2026-01-01T13:00:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "/json-args"}),
            Some(true),
        ),
        tool_step(
            6,
            "2026-01-01T14:00:00Z",
            "ws-ckpt-checkpoint",
            json!({"workspace": "relative/path"}),
            Some(true),
        ),
        tool_step(
            7,
            "2026-01-01T15:00:00Z",
            "ws-ckpt-checkpoint",
            json!(42),
            Some(true),
        ),
    ]);

    let candidates = workspace_candidates(&document);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, "/json-args");
    assert_eq!(candidates[0].step_index, 4);
    assert_eq!(candidates[0].evidence, "tool_argument");
}

#[test]
fn workspace_evidence_rejects_malformed_config_and_ambiguous_shell() {
    let invalid = [
        (
            "ws-ckpt-config",
            json!({"action": "read", "key": "workspace", "value": "/bad"}),
        ),
        (
            "ws-ckpt-config",
            json!({"action": "update", "key": "other", "value": "/bad"}),
        ),
        (
            "ws-ckpt-config",
            json!({"action": "update", "key": "workspace", "value": ""}),
        ),
        (
            "ws-ckpt-config",
            json!({"action": "update", "key": "workspace", "value": "bad\0path"}),
        ),
        ("ws-ckpt-checkpoint", Value::String("not-json".to_string())),
        ("unrelated-tool", json!({"workspace": "/bad"})),
        ("Bash", json!({"command": "echo /bad"})),
        ("Bash", json!({"command": "ws-ckpt"})),
        ("Bash", json!({"command": "ws-ckpt freeze -w /bad"})),
        (
            "Bash",
            json!({"command": "ws-ckpt checkpoint -w /one -w /two"}),
        ),
        (
            "Bash",
            json!({"command": "ws-ckpt checkpoint --workspace=relative"}),
        ),
        ("Bash", json!({"command": "ws-ckpt checkpoint -w ''"})),
        (
            "Bash",
            json!({"command": "ws-ckpt checkpoint -w '/unterminated"}),
        ),
        ("Bash", json!({"command": "ws-ckpt checkpoint -w /bad\\"})),
    ];
    let mut steps: Vec<_> = invalid
        .into_iter()
        .enumerate()
        .map(|(index, (function_name, arguments))| {
            tool_step(
                index + 1,
                "2026-01-01T10:00:00Z",
                function_name,
                arguments,
                Some(true),
            )
        })
        .collect();
    for command in [
        r"ws-ckpt checkpoint -w /from\ shell",
        r"ws-ckpt checkpoint --workspace=/from-equals",
        r#"/usr/bin/ws-ckpt status --workspace='/from absolute'"#,
    ] {
        let step_id = steps.len() + 1;
        steps.push(tool_step(
            step_id,
            "2026-01-01T10:00:00Z",
            "Bash",
            json!({"command": command}),
            Some(true),
        ));
    }

    let candidates = workspace_candidates(&document(steps));
    let paths: Vec<_> = candidates
        .iter()
        .map(|candidate| candidate.path.as_str())
        .collect();
    assert_eq!(paths, ["/from shell", "/from-equals", "/from absolute"]);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.evidence == "bash_argument")
    );
}

#[test]
fn association_uses_agent_boundaries_and_fixed_query_time() {
    let document = document(vec![
        agent_step(1, Some("2026-01-01T10:00:00Z")),
        json!({"step_id": 2, "source": "user", "timestamp": "2026-01-01T10:05:00Z"}),
        agent_step(3, Some("2026-01-01T10:10:00Z")),
    ]);
    let snapshots = vec![
        snapshot("before", "2026-01-01T09:59:59Z"),
        snapshot("start", "2026-01-01T10:00:00Z"),
        snapshot("second", "2026-01-01T10:09:59Z"),
        snapshot("boundary", "2026-01-01T10:10:00Z"),
        snapshot("at-r", "2026-01-01T11:00:00Z"),
    ];
    let query_time = Utc
        .with_ymd_and_hms(2026, 1, 1, 11, 0, 0)
        .single()
        .expect("valid query time");

    let (groups, anomalies) = associate(&document, &snapshots, query_time);
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].step_index, 0);
    assert_eq!(groups[0].interval_end, "2026-01-01T10:10:00.000000000Z");
    assert_eq!(
        groups[0]
            .snapshots
            .iter()
            .map(|snapshot| snapshot.id.as_str())
            .collect::<Vec<_>>(),
        ["second", "start"]
    );
    assert_eq!(groups[1].step_index, 2);
    assert_eq!(groups[1].interval_end, "2026-01-01T11:00:00.000000000Z");
    assert_eq!(groups[1].snapshots[0].id, "boundary");
    assert!(
        anomalies
            .iter()
            .any(|anomaly| anomaly.code == "snapshot_outside_agent_steps")
    );
    assert!(
        anomalies
            .iter()
            .any(|anomaly| anomaly.code == "snapshot_after_query_time")
    );
}

#[test]
fn association_sorts_snapshots_by_parsed_time_and_id() {
    let document = document(vec![agent_step(1, Some("2026-01-01T10:00:00Z"))]);
    let snapshots = vec![
        snapshot("whole", "2026-01-01T10:00:00Z"),
        snapshot("half", "2026-01-01T10:00:00.500000Z"),
        snapshot("millis-b", "2026-01-01T10:00:00.250Z"),
        snapshot("nanos", "2026-01-01T10:00:00.250000001Z"),
        snapshot("millis-a", "2026-01-01T10:00:00.250000000Z"),
    ];
    let query_time = Utc
        .with_ymd_and_hms(2026, 1, 1, 11, 0, 0)
        .single()
        .expect("valid query time");

    let (groups, anomalies) = associate(&document, &snapshots, query_time);
    assert!(anomalies.is_empty());
    assert_eq!(groups.len(), 1);
    assert_eq!(
        groups[0]
            .snapshots
            .iter()
            .map(|snapshot| snapshot.id.as_str())
            .collect::<Vec<_>>(),
        ["half", "nanos", "millis-a", "millis-b", "whole"]
    );
}

#[test]
fn association_reports_missing_step_and_snapshot_timestamps() {
    let document = document(vec![
        agent_step(1, Some("2026-01-01T10:00:00Z")),
        agent_step(2, None),
    ]);
    let snapshots = vec![snapshot("invalid", "not-a-time")];
    let (_, anomalies) = associate(&document, &snapshots, Utc::now());
    assert!(
        anomalies
            .iter()
            .any(|anomaly| anomaly.code == "step_timestamp_missing")
    );
    assert!(
        anomalies
            .iter()
            .any(|anomaly| anomaly.code == "snapshot_timestamp_invalid")
    );
}

#[test]
fn cursor_round_trip_preserves_fixed_time_and_document_identity() {
    let cursor = PageCursor {
        document_digest: "digest".to_string(),
        workspace_id: "ws-id".to_string(),
        query_time: "2026-01-01T11:00:00Z".to_string(),
        inner: "opaque/cursor + value".to_string(),
    };
    let encoded = encode_cursor(&cursor).expect("encode cursor");
    let decoded = decode_cursor(&encoded).expect("decode cursor");
    assert_eq!(decoded.document_digest, cursor.document_digest);
    assert_eq!(decoded.workspace_id, cursor.workspace_id);
    assert_eq!(decoded.query_time, cursor.query_time);
    assert_eq!(decoded.inner, cursor.inner);
    assert!(decode_cursor("not-hex").is_err());
}

#[test]
fn inverse_diff_preserves_rename_paths_containing_arrows() {
    let changes = vec![
        DiffEntry {
            path: "added-current".to_string(),
            change_type: ChangeType::Added,
            detail: None,
        },
        DiffEntry {
            path: "deleted-current".to_string(),
            change_type: ChangeType::Deleted,
            detail: None,
        },
        DiffEntry {
            path: "modified-current".to_string(),
            change_type: ChangeType::Modified,
            detail: None,
        },
        DiffEntry {
            path: "new → file".to_string(),
            change_type: ChangeType::Renamed,
            detail: Some("dir → old → new → file".to_string()),
        },
    ];
    let inverse = invert_diff(&changes).expect("invert known changes");
    assert_eq!(inverse[0].change_type, ChangeType::Deleted);
    assert_eq!(inverse[1].change_type, ChangeType::Added);
    assert_eq!(inverse[2].change_type, ChangeType::Modified);
    assert_eq!(inverse[3].path, "dir → old");
    assert_eq!(inverse[3].detail.as_deref(), Some("new → file → dir → old"));
}

#[test]
fn inverse_diff_rejects_an_ambiguous_rename() {
    for detail in [Some("unrelated".to_string()), None] {
        let changes = vec![DiffEntry {
            path: "new".to_string(),
            change_type: ChangeType::Renamed,
            detail,
        }];
        assert!(invert_diff(&changes).is_err());
    }
}

#[test]
fn document_targets_and_embedded_paths_fail_closed() {
    let invalid_targets = [
        DocumentTarget {
            session_id: None,
            conversation_id: None,
            trajectory_path: None,
        },
        DocumentTarget {
            session_id: Some("session".to_string()),
            conversation_id: Some("conversation".to_string()),
            trajectory_path: None,
        },
        DocumentTarget {
            session_id: Some(String::new()),
            conversation_id: None,
            trajectory_path: None,
        },
    ];
    for target in invalid_targets {
        assert!(matches!(
            target.selector(),
            Err(SnapshotApiError::BadRequest(_))
        ));
    }
    let conversation = DocumentTarget {
        session_id: None,
        conversation_id: Some("conversation".to_string()),
        trajectory_path: None,
    };
    assert!(matches!(
        conversation.selector(),
        Ok(Selector::Conversation("conversation"))
    ));

    let mut by_trajectory = document(vec![]);
    by_trajectory.trajectory_id = Some("child-id".to_string());
    let mut by_session = document(vec![]);
    by_session.session_id = Some("child-session".to_string());
    let anonymous = document(vec![]);
    let mut root = document(vec![]);
    root.subagent_trajectories = Some(vec![by_trajectory, by_session, anonymous]);
    for (path, expected_index) in [
        (r#"["child-id"]"#, 0),
        (r#"["child-session"]"#, 1),
        (r##"["#2"]"##, 2),
    ] {
        let selected = select_document(&root, Some(path)).expect("resolve embedded document");
        let expected = &root.subagent_trajectories.as_ref().expect("children")[expected_index];
        assert!(std::ptr::eq(selected, expected));
    }
    for path in ["not-json", r#"[""]"#, r#"["missing"]"#] {
        assert!(select_document(&root, Some(path)).is_err());
    }
    let too_deep = serde_json::to_string(&vec!["child"; MAX_PATH_DEPTH + 1])
        .expect("serialize excessive path");
    assert!(matches!(
        select_document(&root, Some(&too_deep)),
        Err(SnapshotApiError::BadRequest(_))
    ));
}

#[test]
fn cursor_time_and_anomaly_bounds_are_enforced() {
    assert_eq!(
        SnapshotApiError::Unavailable("offline".to_string())
            .response()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(parse_time("not-a-time").is_err());
    for encoded in ["", "0", "zz", "00"] {
        assert!(decode_cursor(encoded).is_err());
    }
    let oversized = PageCursor {
        document_digest: "digest".to_string(),
        workspace_id: "workspace".to_string(),
        query_time: "2026-01-01T11:00:00Z".to_string(),
        inner: "x".repeat(MAX_CURSOR_BYTES),
    };
    assert!(matches!(
        encode_cursor(&oversized),
        Err(SnapshotApiError::Unavailable(_))
    ));
    assert!(decode_cursor(&"00".repeat(MAX_CURSOR_BYTES)).is_err());

    let mut anomalies: Vec<_> = (0..=MAX_ANOMALIES)
        .map(|index| anomaly("test", format!("anomaly {index}"), None))
        .collect();
    cap_anomalies(&mut anomalies);
    assert_eq!(anomalies.len(), MAX_ANOMALIES + 1);
    assert_eq!(anomalies[MAX_ANOMALIES].code, "anomalies_truncated");
}

#[cfg(unix)]
#[actix_web::test]
async fn workspace_snapshot_routes_bind_identity_and_stay_read_only() {
    let mut document = document(vec![tool_step(
        1,
        "2026-01-01T10:00:00Z",
        "ws-ckpt-checkpoint",
        json!({"workspace": "/tmp/work"}),
        Some(true),
    )]);
    document.session_id = Some("route-session".to_string());
    let (_directory, client) = route_client();
    let app = awtest::init_service(
        App::new()
            .app_data(route_state(&document))
            .app_data(client)
            .service(
                web::scope("/api")
                    .service(list_workspace_snapshots)
                    .service(workspace_snapshot_detail),
            ),
    )
    .await;

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::get()
            .uri("/api/atif/workspace-snapshots?session_id=route-session&conversation_id=other")
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::get()
            .uri("/api/atif/workspace-snapshots?session_id=missing")
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::get()
            .uri("/api/atif/workspace-snapshots?session_id=route-session")
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let list: Value = awtest::read_body_json(response).await;
    assert_eq!(list["workspace"]["ws_id"], "ws-1");
    assert_eq!(list["groups"][0]["step_index"], 0);
    assert_eq!(list["groups"][0]["snapshots"][0]["id"], "snap-route");
    assert_eq!(
        list["groups"][0]["snapshots"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(list["anomalies"].as_array().is_some_and(|anomalies| {
        anomalies
            .iter()
            .any(|anomaly| anomaly["code"] == "snapshot_workspace_mismatch")
    }));
    assert_eq!(list["document_digest"].as_str().map(str::len), Some(64));

    let detail = json!({
        "session_id": "route-session",
        "document_digest": list["document_digest"],
        "query_time": list["query_time"],
        "workspace": list["workspace"],
        "snapshot_id": "snap-route",
        "step_index": 0,
    });
    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri("/api/atif/workspace-snapshots/detail")
            .set_json(&detail)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = awtest::read_body_json(response).await;
    assert_eq!(body["snapshot"]["id"], "snap-route");
    assert_eq!(body["target_to_current"][0]["change_type"], "Added");
    assert_eq!(body["current_to_target"][0]["change_type"], "Deleted");
    assert_eq!(body["rollback_enabled"], false);
    assert!(
        body["rollback_disabled_reason"]
            .as_str()
            .is_some_and(|reason| {
                reason.contains("stopped writers") && reason.contains("privileged authorization")
            })
    );

    let mut stale = detail.clone();
    stale["document_digest"] = Value::String("0".repeat(64));
    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri("/api/atif/workspace-snapshots/detail")
            .set_json(&stale)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);

    let mut invalid_snapshot = detail;
    invalid_snapshot["snapshot_id"] = Value::String("../snap-route".to_string());
    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri("/api/atif/workspace-snapshots/detail")
            .set_json(&invalid_snapshot)
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = awtest::call_service(
        &app,
        awtest::TestRequest::post()
            .uri("/api/atif/workspace-snapshots/rollback")
            .set_json(json!({"snapshot_id": "snap-route"}))
            .to_request(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[cfg(unix)]
#[test]
fn cli_client_reads_structured_status_list_and_diff() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary fake CLI directory");
    let executable = directory.path().join("fake-ws-ckpt");
    std::fs::write(
        &executable,
        r#"#!/bin/sh
case "$1" in
  status) printf '%s' '{"workspaces":[{"ws_id":"ws-1","path":"/tmp/work","snapshot_count":1}]}' ;;
  list) printf '%s' '{"snapshots":[{"id":"snap-1","workspace":"/tmp/work","meta":{"created_at":"2026-01-01T10:00:00Z","pinned":false,"missing":false}}],"next_cursor":null}' ;;
  diff) printf '%s' '[{"path":"file","change_type":"Modified","detail":null}]' ;;
  *) exit 2 ;;
esac
"#,
    )
    .expect("write fake CLI");
    let mut permissions = std::fs::metadata(&executable)
        .expect("fake CLI metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&executable, permissions).expect("make fake CLI executable");

    let client = WsCkptClient::new(executable);
    let workspace = client.status("/tmp/work").expect("verify workspace");
    assert_eq!(workspace.ws_id, "ws-1");
    let page = client.list(&workspace, 20, None).expect("list snapshots");
    assert_eq!(page.snapshots[0].id, "snap-1");
    assert!(page.next_cursor.is_none());
    let changes = client
        .diff_to_current(&workspace, "snap-1")
        .expect("read diff");
    assert_eq!(changes[0].change_type, ChangeType::Modified);
    assert!(client.status("/other").is_err());
}

#[cfg(unix)]
#[test]
fn cli_client_rejects_invalid_status_and_forwards_cursor() {
    let (_directory, client) = scripted_client(
        r#"#!/bin/sh
case "$1" in
  status)
    case "$3" in
      /zero) printf '%s' '{"workspaces":[]}' ;;
      /many) printf '%s' '{"workspaces":[{"ws_id":"ws-1","path":"/one"},{"ws_id":"ws-2","path":"/two"}]}' ;;
      /empty) printf '%s' '{"workspaces":[{"ws_id":" ","path":"/empty"}]}' ;;
      /relative) printf '%s' '{"workspaces":[{"ws_id":"ws-1","path":"relative"}]}' ;;
      /mismatch) printf '%s' '{"workspaces":[{"ws_id":"ws-1","path":"/other"}]}' ;;
      /invalid) printf '%s' 'not-json' ;;
      /failure) printf '%s' 'denied' >&2; exit 7 ;;
      /valid) printf '%s' '{"workspaces":[{"ws_id":"ws-valid","path":"/valid"}]}' ;;
      *) printf '%s' 'unknown workspace' >&2; exit 8 ;;
    esac ;;
  list)
    if [ "$8" != "--cursor" ] || [ "$9" != "next page" ]; then
      printf '%s' 'cursor not forwarded' >&2
      exit 8
    fi
    printf '%s' '{"snapshots":[],"next_cursor":null}' ;;
  *) exit 9 ;;
esac
"#,
    );

    for reference in [
        "/zero",
        "/many",
        "/empty",
        "/relative",
        "/mismatch",
        "/invalid",
    ] {
        assert!(
            matches!(client.status(reference), Err(WsCkptError::Invalid(_))),
            "{reference} should be rejected"
        );
    }
    let error = client
        .status("/failure")
        .expect_err("failed status command");
    assert!(matches!(
        error,
        WsCkptError::Command(message) if message == "denied"
    ));

    let workspace = client.status("/valid").expect("valid workspace");
    let page = client
        .list(&workspace, 7, Some("next page"))
        .expect("forward continuation cursor");
    assert!(page.snapshots.is_empty());
    assert!(page.next_cursor.is_none());
}

#[cfg(unix)]
#[test]
fn snapshot_list_reports_unverified_workspace_and_verification_fallback() {
    let (_directory, client) = scripted_client(
        r#"#!/bin/sh
if [ "$1" = "status" ] && [ "$3" = "/good" ]; then
  printf '%s' '{"workspaces":[{"ws_id":"ws-good","path":"/good"}]}'
  exit 0
fi
printf '%s' 'unregistered' >&2
exit 7
"#,
    );
    let mut document = document(vec![tool_step(
        1,
        "2026-01-01T10:00:00Z",
        "ws-ckpt-checkpoint",
        json!({"workspace": "/bad"}),
        Some(true),
    )]);
    document.session_id = Some("route-session".to_string());
    let state = route_state(&document);
    let response = list_page(
        state.get_ref(),
        &SnapshotListQuery {
            target: DocumentTarget {
                session_id: Some("route-session".to_string()),
                conversation_id: None,
                trajectory_path: None,
            },
            cursor: None,
            limit: None,
        },
        &client,
    )
    .expect("return an anomaly report instead of failing the list");
    assert!(response.workspace.is_none());
    assert!(response.evidence.is_none());
    assert!(response.groups.is_empty());
    assert!(response.next_cursor.is_none());
    assert!(
        response
            .anomalies
            .iter()
            .any(|anomaly| anomaly.code == "workspace_unverified")
    );

    let observed_at = Utc
        .with_ymd_and_hms(2026, 1, 1, 10, 0, 0)
        .single()
        .expect("valid observation time");
    let candidates = vec![
        WorkspaceCandidate {
            path: "/good".to_string(),
            step_index: 2,
            observed_at,
            evidence: "tool_argument",
        },
        WorkspaceCandidate {
            path: "/bad".to_string(),
            step_index: 5,
            observed_at,
            evidence: "tool_argument",
        },
    ];
    let (workspace, evidence, anomalies) = verify_latest(&client, candidates);
    assert_eq!(workspace.expect("older valid workspace").ws_id, "ws-good");
    assert_eq!(evidence.expect("older valid evidence").step_index, 2);
    assert!(
        anomalies
            .iter()
            .any(|anomaly| anomaly.code == "workspace_verification_failed")
    );
}
