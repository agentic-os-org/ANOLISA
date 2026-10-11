//! Frozen V1 query payloads and caller-selected response envelopes.

use std::sync::Arc;
use std::time::Duration;

use asc_daemon_core::{PeerCredentials, PrincipalPolicy, PrincipalRole};
use asc_daemon_handler::{DaemonDispatcher, JsonRejectionEncoder, QueryHandler};
use asc_daemon_service::ShutdownToken;
use asc_pap::PapService;
use asc_pap_repository_memory::ProcessLocalPapRepository;
use asc_persistence_sqlite::observability::owned::OwnedObservabilityWriter;
use asc_persistence_sqlite::query::{SqliteObservabilityQueries, SqliteSecurityQueries};
use asc_persistence_sqlite::security_events::{SqliteEventQuerySource, SqliteEventWriter};
use serde_json::{Value, json};

mod support;

struct LocalUser;
impl PrincipalPolicy for LocalUser {
    fn role_for(&self, _: PeerCredentials) -> PrincipalRole {
        PrincipalRole::LocalUser
    }
}

fn bind_uid(value: &mut Value, uid: u32) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if key == "uid" && *value == 0 {
                    *value = json!(uid);
                } else {
                    bind_uid(value, uid);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                bind_uid(value, uid);
            }
        }
        _ => {}
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // One shared socket proves both dialects over the same seeded stores.
async fn v1_envelope_preserves_obs_queries_and_caller_selection() {
    let directory = tempfile::tempdir().unwrap();
    let socket = directory.path().join("daemon.sock");
    let database = directory.path().join("security-events.db");
    let observations = directory.path().join("observability.db");
    let uid = rustix::process::getuid().as_raw();
    let mut fixture: Value = serde_json::from_str(include_str!(
        "../../../fixtures/query/v1-query-responses.json"
    ))
    .unwrap();
    bind_uid(&mut fixture, uid);
    let writer = SqliteEventWriter::new(&database).unwrap();
    for event in fixture["events"].as_array().unwrap() {
        writer.write(&serde_json::from_value(event.clone()).unwrap());
    }
    let obs_writer = OwnedObservabilityWriter::new(&observations).unwrap();
    for record in fixture["observations"].as_array().unwrap() {
        obs_writer
            .write(&serde_json::from_value(record.clone()).unwrap(), uid)
            .unwrap();
    }
    let dispatcher = Arc::new(
        DaemonDispatcher::new(
            PapService::new(Arc::new(ProcessLocalPapRepository::default())),
            Arc::new(LocalUser),
            asc_daemon::scan_application(
                asc_action_runtime::testing::discarding_finalizer(),
                Arc::new(asc_capability_pii_scan::PiiRuleSet::builtin().unwrap()),
            ),
        )
        .with_queries(
            QueryHandler::default()
                .with_security_queries(SqliteEventQuerySource::new(&database).unwrap())
                .with_observability_queries(
                    asc_daemon_core::query::ObservabilityQueryService::new(
                        SqliteObservabilityQueries::new(observations),
                        SqliteSecurityQueries::new(database),
                    ),
                ),
        ),
    );
    let shutdown = ShutdownToken::new();
    let config = asc_daemon::BootstrapConfig::new(&socket);
    let task = tokio::spawn(asc_daemon::serve(
        config,
        dispatcher,
        Arc::new(JsonRejectionEncoder),
        shutdown.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !socket.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();

    for case in fixture["cases"].as_array().unwrap() {
        let response = legacy_call(
            &socket,
            case["method"].as_str().unwrap(),
            case["params"].clone(),
        )
        .await;
        let request_id = response["request_id"].as_str().unwrap();
        uuid::Uuid::parse_str(request_id).unwrap();
        assert_eq!(
            response,
            json!({"request_id":request_id,"ok":true,"data":case["expected"],
            "stdout":"","stderr":"","exit_code":0}),
            "{} {}",
            case["method"],
            case["params"]
        );
    }
    for (method, params) in [
        ("obs.runs.list", json!({})),
        ("obs.timeline.get", json!({"session_id":"session-1"})),
        ("sec.events.get", json!({})),
        ("sec.events.count_by", json!({})),
    ] {
        let response = legacy_call(&socket, method, params).await;
        assert_eq!(response["ok"], false);
        assert_eq!(response["error"]["code"], "bad_request");
        assert_eq!(response["exit_code"], 1);
        assert_eq!(response["stderr"], response["error"]["message"]);
        assert_eq!(response["data"], json!({}));
    }
    for (method, params) in [
        ("sec.summary", json!({})),
        ("sec.events.list", json!({})),
        ("sec.events.get", json!({"event_id":"code"})),
        ("sec.events.count_by", json!({"group_by":"category"})),
    ] {
        let legacy = legacy_call(&socket, method, params.clone()).await;
        let modern =
            support::request_json(&socket, &json!({"method":method,"params":params})).await;
        assert!(modern.get("result").is_some(), "{method}: {modern}");
        let request_id = legacy["request_id"].as_str().unwrap();
        uuid::Uuid::parse_str(request_id).unwrap();
        assert_eq!(
            legacy,
            json!({"request_id":request_id,"ok":true,"data":modern["result"],
                "stdout":"","stderr":"","exit_code":0}),
            "{method}"
        );
    }
    let health = legacy_call(&socket, "daemon.health", json!({})).await;
    assert_eq!(health["ok"], true);
    assert_eq!(health["exit_code"], 0);
    let modern =
        support::request_json(&socket, &json!({"method":"daemon.health","params":{}})).await;
    assert!(modern.get("ok").is_none());
    for snapshot in [&health["data"], &modern["result"]] {
        assert_eq!(snapshot["status"], "ok");
        assert_eq!(snapshot["pid"], std::process::id());
        assert!(snapshot["uptime_seconds"].as_f64().unwrap() >= 0.0);
    }
    assert!(
        modern["result"]["uptime_seconds"].as_f64().unwrap()
            >= health["data"]["uptime_seconds"].as_f64().unwrap()
    );
    // Caller selects the envelope even when the existing router has no implementation.
    let unknown = legacy_call(&socket, "daemon.unknown", json!({})).await;
    assert!(unknown.get("request_id").is_some());
    assert!(unknown.get("requestId").is_none());
    assert_eq!(unknown["ok"], false);
    assert_eq!(unknown["error"]["code"], "unknown_method");
    assert_eq!(unknown["exit_code"], 1);
    assert_eq!(unknown["data"], json!({}));
    let denied = legacy_call(&socket, "policy.templates.list", json!({})).await;
    assert_eq!(denied["ok"], false);
    assert_eq!(denied["error"]["code"], "permission_denied");

    // trace_context is optional and does not select the response dialect.
    let legacy = support::request_json(
        &socket,
        &json!({"method":"obs.sessions.list","params":{},"caller":"agentsight"}),
    )
    .await;
    assert_eq!(legacy["ok"], true);
    assert_eq!(legacy["data"], fixture["cases"][0]["expected"]);
    let mut legacy = legacy_call(&socket, "action.pii_scan", json!({"text":"hello"})).await;
    let mut modern = support::request_json(
        &socket,
        &json!({"method":"action.pii_scan","params":{"text":"hello"}}),
    )
    .await;
    assert_eq!(legacy["ok"], true);
    // Separate executions may take different times; retain the field and check its type.
    assert!(legacy["data"]["elapsed_ms"].is_u64());
    assert!(modern["result"]["elapsed_ms"].is_u64());
    legacy["data"]["elapsed_ms"] = json!(0);
    modern["result"]["elapsed_ms"] = json!(0);
    assert_eq!(legacy["data"], modern["result"]);
    for caller in [Value::Null, json!("other")] {
        let mut request = json!({"method":"obs.sessions.list","params":{},"trace_context":{}});
        if !caller.is_null() {
            request["caller"] = caller;
        }
        let response = support::request_json(&socket, &request).await;
        assert!(response.get("ok").is_none());
        assert_eq!(response["error"]["code"], "invalid_request");
    }

    // The same socket still speaks the strict modern protocol, including the
    // modern-only owner and pagination fields removed from legacy projections.
    let modern =
        support::request_json(&socket, &json!({"method":"obs.sessions.list","params":{}})).await;
    assert_eq!(modern["result"]["items"][0]["uid"], uid);
    assert!(modern.get("requestId").is_some());
    assert!(modern.get("request_id").is_none());
    let modern =
        support::request_json(&socket, &json!({"method":"sec.events.list","params":{}})).await;
    assert_eq!(modern["result"]["total"], 3);
    assert!(modern.get("ok").is_none());
    let rejected = support::request_json(
        &socket,
        &json!({"method":"obs.sessions.list","params":{"uid":0},
            "trace_context":{"uid":0},"caller":"agentsight","timeout_ms":5000}),
    )
    .await;
    assert_eq!(rejected["error"]["code"], "bad_request");
    shutdown.request();
    task.await.unwrap().unwrap();
}

async fn legacy_call(socket: &std::path::Path, method: &str, params: Value) -> Value {
    support::request_json(
        socket,
        &json!({"method":method,"params":params,
        "trace_context":{},"caller":"agentsight","timeout_ms":5000}),
    )
    .await
}
