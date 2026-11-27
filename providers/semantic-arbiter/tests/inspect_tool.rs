//! Native inspection tools and fully attributed read-only shell calls settle
//! locally; sensitive or unattributable reads stay with the model.

#[path = "support/fixture.rs"]
mod fixture;

use std::time::Duration;

use fixture::*;
use serde_json::json;

fn inspect_config(fixture: &Fixture, endpoint: &str) -> serde_json::Value {
    let mut config = fixture.config(endpoint);
    config["task"]["allowed_resource_ids"] = json!([]);
    config["scenarios"] = json!(["preservation", "task_scope", "snapshot"]);
    config["tools"]["Bash"] = json!({"command_pointer":"/command"});
    config["tools"]["Read"] = json!({"path_pointer":"/file_path"});
    config["tools"]["Grep"] = json!({"path_pointer":"/path"});
    config["tools"]["Glob"] = json!({"path_pointer":"/path"});
    config["tools"]["LS"] = json!({"path_pointer":"/path"});
    config
}

#[test]
fn inspection_tools_settle_protection_locally_but_scope_asks_the_model() {
    for (tool, input) in [
        (
            "Read",
            json!({"file_path":"greeting.txt","offset":1,"limit":20}),
        ),
        ("Glob", json!({"pattern":"**/*.rs"})),
        ("LS", json!({"path":"."})),
        (
            "Grep",
            json!({"pattern":"fn main","path":"src","output_mode":"content"}),
        ),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let config = inspect_config(&fixture, &server.endpoint);
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!(tool);
        request["event"]["tool"]["name"] = json!(tool.to_lowercase());
        request["event"]["tool"]["input"] = input.clone();
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{tool}");
        let call = &fixture.records()[0];
        let evaluation = call.get("evaluation");
        assert_eq!(evaluation["candidate"]["changes"], json!([]), "{tool}");
        assert_eq!(evaluation["candidate"]["native_tool"]["input"], input);
        assert_eq!(
            call.get("evidence"),
            json!({"source": "git-rebuild", "status": "no_targets"}),
            "{tool}: no resource needs a rebuild fact"
        );
        let result = call.get("result");
        // Scope is always semantic for reads: an unrelated read is out of scope
        // even without a write effect. Protection settles locally; only the
        // task-scope question reaches the model.
        assert!(server.received.try_recv().is_ok(), "{tool}");
        assert_trace(&result, "task_scope", "allowed", "model_fallback", None);
        assert_eq!(
            call.get("model_request")["questions"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["task_scope"]
        );
        assert_trace(
            &result,
            "preservation",
            "allowed",
            "local_policy",
            Some("preserve_no_mutation"),
        );
        assert_trace(
            &result,
            "snapshot",
            "allowed",
            "local_policy",
            Some("no_loss_read_only"),
        );
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            "allow"
        );
        assert_eq!(
            result["evaluation"]["outcome"]["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["action"]["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["audit"]
        );
    }
}

#[test]
fn sensitive_and_unattributable_reads_reach_the_model_with_full_input() {
    // The mirrored case names an unresolved write resource, so protection
    // units also fall back; the read-only cases keep protection local.
    for (tool, input, signal, mirrored) in [
        ("Read", json!({"file_path":".env"}), "sensitive_read", false),
        (
            "Read",
            json!({"file_path":"config/.env.production"}),
            "sensitive_read",
            false,
        ),
        (
            "Read",
            json!({"file_path":"../outside.txt"}),
            "opaque_execution",
            true,
        ),
        (
            "Bash",
            json!({"command":"cat .env"}),
            "sensitive_read",
            false,
        ),
        (
            "Bash",
            json!({"command":"grep -r KEY ."}),
            "sensitive_read",
            false,
        ),
        (
            "Bash",
            json!({"command":"printenv"}),
            "sensitive_read",
            false,
        ),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.01", 200, Duration::ZERO);
        let config = inspect_config(&fixture, &server.endpoint);
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!(tool);
        request["event"]["tool"]["name"] = json!(tool.to_lowercase());
        request["event"]["tool"]["input"] = input.clone();
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{tool}: {input}");
        let call = &fixture.records()[0];
        let body = server
            .received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(body["state"]["candidate"]["native_tool"]["input"], input);
        assert!(
            body["state"]["local_context"]["signals"]
                .as_array()
                .unwrap()
                .contains(&json!({"kind":signal,"resource_id":null})),
            "{tool}: {input}"
        );
        let questions: Vec<_> = body["questions"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        if mirrored {
            assert_eq!(questions, ["preservation", "snapshot", "task_scope"]);
        } else {
            assert_eq!(
                questions,
                ["task_scope"],
                "protection units stay local for read-only calls"
            );
        }
        let result = call.get("result");
        assert_trace(&result, "task_scope", "allowed", "model_fallback", None);
        assert_eq!(result["evaluation"]["answers"][1]["value"], "outside_scope");
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            "block"
        );
    }
}

#[test]
fn read_only_shell_calls_settle_protection_locally_and_skip_evidence() {
    for command in [
        "ls -la",
        "git status --short",
        "cat greeting.txt",
        "git log --oneline -5",
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let config = inspect_config(&fixture, &server.endpoint);
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!("Bash");
        request["event"]["tool"]["name"] = json!("bash");
        request["event"]["tool"]["input"] = json!({"command":command});
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{command}");
        let call = &fixture.records()[0];
        assert_eq!(call.get("evidence")["status"], "no_targets", "{command}");
        assert_eq!(call.get("evaluation")["candidate"]["changes"], json!([]));
        let result = call.get("result");
        // Protection settles locally; only task scope asks the model.
        assert!(server.received.try_recv().is_ok(), "{command}");
        assert_eq!(
            call.get("model_request")["questions"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["task_scope"]
        );
        for (unit, policy) in [
            ("preservation", "preserve_no_mutation"),
            ("snapshot", "no_loss_read_only"),
        ] {
            assert_trace(&result, unit, "allowed", "local_policy", Some(policy));
        }
        assert_trace(&result, "task_scope", "allowed", "model_fallback", None);
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            "allow"
        );
    }
}

#[test]
fn working_state_destruction_floors_snapshot_without_a_snapshot_question() {
    for command in [
        "git reset --hard HEAD",
        "git clean -fd",
        "git checkout -- .",
        "git restore .",
    ] {
        let fixture = Fixture::new();
        let server = Server::new(
            r#"{"preservation":0.01,"task_scope":0.99,"snapshot":0.99}"#,
            200,
            Duration::ZERO,
        );
        let config = inspect_config(&fixture, &server.endpoint);
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!("Bash");
        request["event"]["tool"]["name"] = json!("bash");
        request["event"]["tool"]["input"] = json!({"command":command});
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{command}");
        let call = &fixture.records()[0];
        let body = server
            .received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            body["questions"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            ["preservation", "task_scope"],
            "{command}: the snapshot floor needs no model question"
        );
        assert!(
            body["state"]["local_context"]["signals"]
                .as_array()
                .unwrap()
                .contains(&json!({"kind":"working_state_destruction","resource_id":null})),
            "{command}"
        );
        let result = call.get("result");
        assert_trace(
            &result,
            "snapshot",
            "evidence_gap",
            "local_policy",
            Some("protect_working_state"),
        );
        assert_eq!(
            result["evaluation"]["answers"][2]["value"],
            "needs_snapshot"
        );
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            "allow"
        );
        assert_eq!(
            result["evaluation"]["outcome"]["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["action"]["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["snapshot", "audit"],
            "{command}"
        );
    }
}

#[test]
fn dry_run_and_index_only_git_forms_do_not_floor_snapshot() {
    // (command, fully read-only projection) — a dry run or index-only form
    // names no working-tree write; a soft reset stays unattributed.
    for (command, read_only) in [
        ("git clean -nfd", true),
        ("git restore --staged .", true),
        ("git reset --soft HEAD~1", false),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let config = inspect_config(&fixture, &server.endpoint);
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!("Bash");
        request["event"]["tool"]["name"] = json!("bash");
        request["event"]["tool"]["input"] = json!({"command":command});
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{command}");
        let call = &fixture.records()[0];
        let evaluation = call.get("evaluation");
        assert!(
            !evaluation["local_context"]["signals"]
                .as_array()
                .unwrap()
                .contains(&json!({"kind":"working_state_destruction","resource_id":null})),
            "{command}"
        );
        let result = call.get("result");
        if read_only {
            assert_eq!(evaluation["candidate"]["changes"], json!([]), "{command}");
            assert_trace(
                &result,
                "snapshot",
                "allowed",
                "local_policy",
                Some("no_loss_read_only"),
            );
        } else {
            assert_trace(&result, "snapshot", "evidence_gap", "model_fallback", None);
        }
        assert_eq!(
            result["evaluation"]["outcome"]["obligations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["action"]["kind"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["audit"],
            "{command}"
        );
    }
}
