use actix_web::{App, http::StatusCode, test as awtest, web};
use agentsight_trajectory_collector::{TrajectoryRecord, TrajectoryStore};
use chrono::TimeZone;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use super::*;
use crate::server::workspace_evidence::workspace_candidates;
use crate::server::workspace_snapshot_client::{ChangeType, DiffEntry, WsCkptClient};

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
            path: "new → file".to_string(),
            change_type: ChangeType::Renamed,
            detail: Some("dir → old → new → file".to_string()),
        },
    ];
    let inverse = invert_diff(&changes).expect("invert known changes");
    assert_eq!(inverse[0].change_type, ChangeType::Deleted);
    assert_eq!(inverse[1].change_type, ChangeType::Added);
    assert_eq!(inverse[2].path, "dir → old");
    assert_eq!(inverse[2].detail.as_deref(), Some("new → file → dir → old"));
}

#[test]
fn inverse_diff_rejects_an_ambiguous_rename() {
    let changes = vec![DiffEntry {
        path: "new".to_string(),
        change_type: ChangeType::Renamed,
        detail: Some("unrelated".to_string()),
    }];
    assert!(invert_diff(&changes).is_err());
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
