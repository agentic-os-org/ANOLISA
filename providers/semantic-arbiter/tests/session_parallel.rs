//! Public Host and real-binary regressions for sessionless task binding and isolation.

#[path = "support/fixture.rs"]
mod fixture;
#[path = "support/parallel.rs"]
mod parallel;

use std::collections::BTreeSet;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use fixture::*;
use parallel::*;
use serde_json::{json, Value};

fn model_body<'a>(bodies: &'a [Value], event: &Value) -> &'a Value {
    let matching: Vec<_> = bodies
        .iter()
        .filter(|body| {
            body["state"]["authoritative_task"]["session_id"] == session(event)
                && body["state"]["candidate"]["native_tool"]["input"] == event["tool"]["input"]
        })
        .collect();
    assert_eq!(matching.len(), 1);
    matching[0]
}

fn rejected(fixture: &Fixture, event: &Value, code: &str) {
    let dir = record(fixture, event);
    let result = read(&dir.join("40-result.json"));
    assert_eq!(result["status"], "uncovered");
    assert_eq!(result["code"], code);
    assert!(result["evaluation"].is_null());
    for name in [
        "05-task.json",
        "10-evaluation.json",
        "20-model-request.json",
        "30-model-result.json",
    ] {
        assert!(
            !dir.join(name).exists(),
            "rejected input must not publish {name}"
        );
    }
    no_secrets(fixture);
}

fn received(server: &Server) -> Value {
    server
        .received
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
}

#[test]
fn same_aw_instance_parallel_sessions_share_task_without_identity_collision() {
    let fixture = Fixture::new();
    let server = ParallelServer::new(4, 4);
    let config = config(&fixture, &server.endpoint);
    let host = prepare(&fixture, &config, "target", "parallel-sessions");
    assert!(fixture.records().is_empty());
    let events: Vec<_> = (0..4)
        .map(|index| {
            event(
                &fixture,
                &config,
                "target",
                "same-instance",
                &format!("native/session+{index}"),
            )
        })
        .collect();
    let jobs: Vec<_> = events.iter().map(|event| (&host, event)).collect();
    concurrent(&jobs);
    let bodies = server.finish();
    let evaluations: Vec<_> = events
        .iter()
        .map(|event| evaluated(&fixture, event, &config, model_body(&bodies, event)))
        .collect();
    for field in ["session_id", "candidate_id"] {
        let ids: BTreeSet<_> = evaluations
            .iter()
            .map(|value| value["binding"][field].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 4, "{field}");
    }
    for value in &evaluations[1..] {
        for field in [
            "instance_id",
            "workspace_id",
            "task_version",
            "config_revision",
            "candidate_revision",
        ] {
            assert_eq!(
                value["binding"][field], evaluations[0]["binding"][field],
                "{field}"
            );
        }
    }
    assert_eq!(fixture.records().len(), 4);
}

#[test]
fn same_native_session_parallel_aw_instances_are_isolated() {
    let fixture = Fixture::new();
    let server = ParallelServer::new(3, 3);
    let config = config(&fixture, &server.endpoint);
    let hosts: Vec<_> = (0..3)
        .map(|_| prepare(&fixture, &config, "target", "parallel-instances"))
        .collect();
    assert!(fixture.records().is_empty());
    let events: Vec<_> = (0..3)
        .map(|index| {
            event(
                &fixture,
                &config,
                "target",
                &format!("instance/{index}"),
                "reused-native-session",
            )
        })
        .collect();
    concurrent(&hosts.iter().zip(&events).collect::<Vec<_>>());
    let bodies = server.finish();
    let evaluations: Vec<_> = events
        .iter()
        .map(|event| evaluated(&fixture, event, &config, model_body(&bodies, event)))
        .collect();
    for field in ["session_id", "instance_id", "candidate_id"] {
        let ids: BTreeSet<_> = evaluations
            .iter()
            .map(|value| value["binding"][field].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 3, "{field}");
    }
    assert_eq!(fixture.records().len(), 3);
}

#[test]
fn same_native_session_parallel_adapter_and_binding_domains_are_isolated() {
    let fixture = Fixture::new();
    let server = ParallelServer::new(3, 3);
    let qoder = config(&fixture, &server.endpoint);
    let mut pi = qoder.clone();
    pi["adapter"] = json!("pi");
    let specs = [
        (&qoder, "target"),
        (&qoder, "other-target"),
        (&pi, "target"),
    ];
    let hosts: Vec<_> = specs
        .iter()
        .map(|(config, target)| prepare(&fixture, config, target, "identity-domains"))
        .collect();
    let events: Vec<_> = specs
        .iter()
        .map(|(config, target)| event(&fixture, config, target, "same-instance", "same-session"))
        .collect();
    concurrent(&hosts.iter().zip(&events).collect::<Vec<_>>());
    let bodies = server.finish();
    let evaluations: Vec<_> = specs
        .iter()
        .zip(&events)
        .map(|((config, _), event)| evaluated(&fixture, event, config, model_body(&bodies, event)))
        .collect();
    for field in ["session_id", "instance_id", "candidate_id"] {
        assert_eq!(
            evaluations
                .iter()
                .map(|value| value["binding"][field].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            3
        );
    }
    assert_eq!(fixture.records().len(), 3);
}

#[test]
fn same_session_later_call_keeps_task_session_and_changes_candidate() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let config = config(&fixture, &server.endpoint);
    let host = prepare(&fixture, &config, "target", "later-call");
    let first = event(&fixture, &config, "target", "same-instance", "same-session");
    assert_eq!(invoke_host(&host, &first), "shadow_evaluated");
    let first_evaluation = evaluated(&fixture, &first, &config, &received(&server));
    let mut later = first.clone();
    later["tool"]["call_id"] = json!("later-call/+2");
    later["tool"]["input"]["content"] = json!("different complete replacement payload");
    assert_eq!(invoke_host(&host, &later), "shadow_evaluated");
    let later_evaluation = evaluated(&fixture, &later, &config, &received(&server));
    assert_eq!(first_evaluation["task"], later_evaluation["task"]);
    for field in [
        "session_id",
        "instance_id",
        "workspace_id",
        "task_version",
        "config_revision",
    ] {
        assert_eq!(
            first_evaluation["binding"][field],
            later_evaluation["binding"][field]
        );
    }
    for field in ["candidate_id", "candidate_revision"] {
        assert_ne!(
            first_evaluation["binding"][field],
            later_evaluation["binding"][field]
        );
    }
    assert_eq!(fixture.records().len(), 2);
    assert!(server.received.try_recv().is_err());
}

#[test]
fn parallel_distinct_aw_tasks_keep_text_provenance_and_versions_isolated() {
    let fixture = Fixture::new();
    let server = ParallelServer::new(3, 3);
    let configs: Vec<_> = (0..3)
        .map(|index| {
            let mut config = config(&fixture, &server.endpoint);
            let text = format!("Update the synthetic greeting for isolated task {index} only.");
            config["task"]["text"] = json!(text);
            config["task"]["version"] = json!(format!("task-v{index}"));
            config["task"]["provenance"] = provenance(&text);
            config["task"]["provenance"]["captured_by"] =
                json!(format!("synthetic-operator-{index}"));
            config["task"]["provenance"]["captured_at_ms"] = json!(1_700_000_000_000u64 + index);
            config
        })
        .collect();
    let hosts: Vec<_> = configs
        .iter()
        .map(|config| prepare(&fixture, config, "target", "isolated-tasks"))
        .collect();
    assert!(fixture.records().is_empty());
    let events: Vec<_> = configs
        .iter()
        .enumerate()
        .map(|(index, config)| {
            event(
                &fixture,
                config,
                "target",
                &format!("aw-instance-{index}"),
                "same-native-session",
            )
        })
        .collect();
    concurrent(&hosts.iter().zip(&events).collect::<Vec<_>>());
    let bodies = server.finish();
    let evaluations: Vec<_> = configs
        .iter()
        .zip(&events)
        .map(|(config, event)| evaluated(&fixture, event, config, model_body(&bodies, event)))
        .collect();
    for field in [
        "session_id",
        "instance_id",
        "config_revision",
        "task_version",
        "candidate_id",
    ] {
        assert_eq!(
            evaluations
                .iter()
                .map(|value| value["binding"][field].as_str().unwrap())
                .collect::<BTreeSet<_>>()
                .len(),
            3,
            "{field}"
        );
    }
    assert_eq!(fixture.records().len(), 3);
}

#[test]
fn aw_revision_task_version_and_workspace_changes_keep_context_distinct() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let base = config(&fixture, &server.endpoint);
    let mut task = base.clone();
    task["task"]["version"] = json!("v2");
    let mut workspace = base.clone();
    workspace["workspace_id"] = json!("other-synthetic-workspace");
    let cases = [
        (&base, "original"),
        (&base, "revision-only"),
        (&task, "task-version"),
        (&workspace, "workspace"),
    ];
    let mut evaluations = Vec::new();
    for (config, name) in cases {
        let host = prepare(&fixture, config, "target", name);
        let mut event = event(&fixture, config, "target", "same-instance", "same-session");
        event["native"]["synthetic_extension"]["case"] = json!(name);
        assert_eq!(invoke_host(&host, &event), "shadow_evaluated");
        evaluations.push(evaluated(&fixture, &event, config, &received(&server)));
    }
    for value in &evaluations[1..] {
        for field in [
            "session_id",
            "instance_id",
            "candidate_id",
            "candidate_revision",
        ] {
            assert_eq!(
                value["binding"][field], evaluations[0]["binding"][field],
                "{field}"
            );
        }
    }
    assert_eq!(
        evaluations
            .iter()
            .map(|value| value["binding"]["config_revision"].as_str().unwrap())
            .collect::<BTreeSet<_>>()
            .len(),
        4
    );
    assert_eq!(evaluations[0]["task"], evaluations[1]["task"]);
    assert_ne!(
        evaluations[0]["binding"]["task_version"],
        evaluations[2]["binding"]["task_version"]
    );
    assert_ne!(
        evaluations[0]["binding"]["workspace_id"],
        evaluations[3]["binding"]["workspace_id"]
    );
    assert_eq!(fixture.records().len(), 4);
    assert!(server.received.try_recv().is_err());
}

#[test]
fn unsupported_first_event_does_not_bind_task_and_later_valid_event_evaluates() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let config = config(&fixture, &server.endpoint);
    let host = prepare(&fixture, &config, "target", "unsupported-first");
    let mut first = event(
        &fixture,
        &config,
        "target",
        "same-instance",
        "unsupported-session",
    );
    first["tool"]["native_name"] = json!("Bash");
    first["tool"]["input"] = json!({"command":"synthetic command, never executed"});
    assert_eq!(invoke_host(&host, &first), "unmapped_tool");
    rejected(&fixture, &first, "unmapped_tool");
    assert!(server.received.try_recv().is_err());
    let legal = event(
        &fixture,
        &config,
        "target",
        "same-instance",
        "later-legal-session",
    );
    assert_eq!(invoke_host(&host, &legal), "shadow_evaluated");
    evaluated(&fixture, &legal, &config, &received(&server));
    assert_eq!(fixture.records().len(), 2);
    assert!(server.received.try_recv().is_err());
}

#[test]
fn rejected_first_context_and_size_do_not_publish_task_or_poison_later_calls() {
    for (pointer, value, code) in [
        (
            "/native/cwd",
            json!("/not-the-synthetic-workspace"),
            "context_mismatch",
        ),
        ("/agent/instance_id", Value::Null, "missing_identity"),
        (
            "/tool/input",
            json!({"file_path":"greeting.txt","content":"ok","other_path":"private.txt"}),
            "unsupported_input",
        ),
        (
            "/tool/input/content",
            json!("x".repeat(70_000)),
            "oversized_candidate",
        ),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let config = config(&fixture, &server.endpoint);
        let host = prepare(&fixture, &config, "target", "rejected-first");
        let legal = event(&fixture, &config, "target", "same-instance", "same-session");
        let mut first = legal.clone();
        *first.pointer_mut(pointer).unwrap() = value;
        assert_eq!(invoke_host(&host, &first), code);
        rejected(&fixture, &first, code);
        assert!(server.received.try_recv().is_err());
        assert_eq!(invoke_host(&host, &legal), "shadow_evaluated");
        evaluated(&fixture, &legal, &config, &received(&server));
        assert_eq!(fixture.records().len(), 2);
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn schema_permitted_malformed_sessions_reject_before_task_publication_and_network() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let config = config(&fixture, &server.endpoint);
    for malformed in [
        Value::Null,
        json!("   "),
        json!("native\ncontrol"),
        json!("native\u{7f}control"),
        json!("é".repeat(257)),
    ] {
        let mut request = fixture.request(config.clone());
        request["event"]["session_id"] = malformed;
        let event = request["event"].clone();
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "missing_identity");
        rejected(&fixture, &event, "missing_identity");
        assert!(server.received.try_recv().is_err());
    }
    let host = prepare(&fixture, &config, "target", "malformed-recovery");
    for native_session in ["s".repeat(512), "legal-session-after-rejections".into()] {
        let legal = event(
            &fixture,
            &config,
            "target",
            "same-instance",
            &native_session,
        );
        assert_eq!(invoke_host(&host, &legal), "shadow_evaluated");
        evaluated(&fixture, &legal, &config, &received(&server));
    }
    assert_eq!(fixture.records().len(), 7);
    assert!(server.received.try_recv().is_err());
}

#[test]
fn aw_schema_rejects_overlong_native_session_without_private_records_or_model() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let config = config(&fixture, &server.endpoint);
    let host = prepare(&fixture, &config, "target", "overlong-session");
    let overlong = event(
        &fixture,
        &config,
        "target",
        "same-instance",
        &"s".repeat(513),
    );
    let cancelled = AtomicBool::new(false);
    let selected = host
        .event(
            overlong,
            Instant::now() + Duration::from_secs(8),
            &cancelled,
        )
        .unwrap();
    let invocation = selected.invoke("shadow").unwrap();
    assert!(invocation.result.is_err());
    assert!(fixture.records().is_empty());
    assert!(server.received.try_recv().is_err());
    no_secrets(&fixture);
}
