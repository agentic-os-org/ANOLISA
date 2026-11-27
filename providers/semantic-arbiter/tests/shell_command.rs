//! Lexical and opaque native-tool projection through the real Provider process.
//! Loopback probabilities exercise fallback contracts, not model accuracy.

#[path = "support/fixture.rs"]
mod fixture;

use std::time::Duration;

use fixture::*;
use serde_json::json;

#[test]
fn shell_commands_project_named_targets_and_leave_the_rest_to_the_model() {
    let unresolved = ("replace", "command:unresolved");
    // (command, extra input, allowed ceiling, expected code, changes, control, model called)
    for (command, extra, ceiling, code, changes, control, model) in [
        (
            "echo hi > greeting.txt",
            None,
            false,
            "shadow_evaluated",
            vec![("replace", "greeting")],
            "block",
            false,
        ),
        (
            "cat > ./greeting.txt <<'EOF'\nrm -rf x\nEOF",
            None,
            false,
            "shadow_evaluated",
            vec![("replace", "greeting")],
            "block",
            false,
        ),
        (
            "rm -rf .",
            None,
            false,
            "shadow_evaluated",
            vec![("delete", "command:unresolved"), ("delete", "greeting")],
            "block",
            false,
        ),
        (
            "sed -i 's/a/b/' private.txt",
            None,
            true,
            "shadow_evaluated",
            vec![("replace", "private")],
            "block",
            false,
        ),
        (
            "touch new.txt",
            None,
            false,
            "shadow_evaluated",
            vec![("replace", "path:new.txt")],
            "allow",
            true,
        ),
        // Every target is a change; a resource any part deletes is reported deleted.
        (
            "touch new.txt private.txt && rm -f private.txt",
            None,
            false,
            "shadow_evaluated",
            vec![("replace", "path:new.txt"), ("delete", "private")],
            "allow",
            true,
        ),
        // The preserved second target blocks the call although the first is permitted.
        (
            "touch new.txt; echo hi >> greeting.txt",
            None,
            false,
            "shadow_evaluated",
            vec![("replace", "path:new.txt"), ("replace", "greeting")],
            "block",
            false,
        ),
        (
            "python build.py",
            None,
            false,
            "shadow_evaluated",
            vec![unresolved],
            "allow",
            true,
        ),
        (
            "python build.py",
            None,
            true,
            "shadow_evaluated",
            vec![unresolved],
            "block",
            false,
        ),
        (
            "echo hi > greeting.txt",
            Some(("dir_path", json!("sub"))),
            false,
            "shadow_evaluated",
            vec![unresolved],
            "allow",
            true,
        ),
        // Fully attributed read-only commands project no change; protection
        // settles locally but task scope still asks the model.
        (
            "git status && ls 2>/dev/null",
            None,
            false,
            "shadow_evaluated",
            vec![],
            "allow",
            true,
        ),
        (
            "git -C other status",
            None,
            false,
            "shadow_evaluated",
            vec![],
            "allow",
            true,
        ),
        (
            "rustc --version",
            None,
            false,
            "shadow_evaluated",
            vec![],
            "allow",
            true,
        ),
        (
            "ls",
            Some(("shell", json!(true))),
            false,
            "shadow_evaluated",
            vec![unresolved],
            "allow",
            true,
        ),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let mut config = fixture.config(&server.endpoint);
        config["tools"]["Bash"] = json!({"command_pointer":"/command"});
        config["task"]["preserve_resource_ids"] = json!(["greeting"]);
        if !ceiling {
            config["task"]["allowed_resource_ids"] = json!([]);
        }
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!("Bash");
        request["event"]["tool"]["name"] = json!("bash");
        request["event"]["tool"]["input"] = json!({"command":command, "description":"synthetic"});
        if let Some((key, value)) = extra {
            request["event"]["tool"]["input"][key] = value;
        }
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request.clone(), true);
        assert_eq!(diagnostic["code"], code, "{command}");
        let received = server.received.try_recv();
        assert_eq!(received.is_ok(), model, "{command}");
        if code != "shadow_evaluated" {
            continue;
        }
        let dir = &fixture.records()[0];
        let evaluation = dir.get("evaluation");
        let expected: Vec<_> = changes
            .iter()
            .map(|(operation, id)| json!({"operation":operation,"resource_id":id}))
            .collect();
        assert_eq!(
            evaluation["candidate"]["changes"],
            json!(expected),
            "{command}"
        );
        assert_eq!(
            evaluation["candidate"]["native_tool"]["input"]["command"],
            command
        );
        assert_eq!(
            dir.get("result")["evaluation"]["outcome"]["control"]["candidate"],
            control,
            "{command}"
        );
        if let Ok(body) = received {
            assert_eq!(
                body["state"]["candidate"]["native_tool"]["input"],
                request["event"]["tool"]["input"]
            );
            assert_eq!(dir.get("model_request")["state"], body["state"]);
            assert_eq!(dir.get("model_result")["status"], "parsed");
            assert_trace(
                &dir.get("result"),
                "task_scope",
                "allowed",
                "model_fallback",
                None,
            );
        } else {
            assert!(!dir.has("model_request"));
            assert!(!dir.has("model_result"));
        }
    }
}

#[test]
fn cd_back_to_the_workspace_root_restores_the_preservation_gate() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut config = fixture.config(&server.endpoint);
    config["tools"]["Bash"] = json!({"command_pointer":"/command"});
    config["task"]["preserve_resource_ids"] = json!(["greeting"]);
    config["task"]["allowed_resource_ids"] = json!([]);
    let mut request = fixture.request(config);
    request["event"]["tool"]["native_name"] = json!("Bash");
    request["event"]["tool"]["name"] = json!("bash");
    let root = fixture.root.path().display();
    request["event"]["tool"]["input"] =
        json!({"command":format!("cd {root} && rm greeting.txt"), "description":"synthetic"});
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    assert!(
        server.received.try_recv().is_err(),
        "a named preserved target blocks without the model"
    );
    let dir = &fixture.records()[0];
    assert_eq!(
        dir.get("evaluation")["candidate"]["changes"],
        json!([{"operation":"delete","resource_id":"greeting"}])
    );
    assert_eq!(
        dir.get("result")["evaluation"]["outcome"]["control"]["candidate"],
        "block"
    );
}

#[test]
fn cd_into_a_subdirectory_leaves_relative_deletion_to_the_model() {
    let fixture = Fixture::new();
    let server = Server::new("0.99", 200, Duration::ZERO);
    let mut config = fixture.config(&server.endpoint);
    config["tools"]["Bash"] = json!({"command_pointer":"/command"});
    config["task"]["preserve_resource_ids"] = json!(["greeting"]);
    config["task"]["allowed_resource_ids"] = json!([]);
    let mut request = fixture.request(config);
    request["event"]["tool"]["native_name"] = json!("Bash");
    request["event"]["tool"]["name"] = json!("bash");
    let root = fixture.root.path().display();
    request["event"]["tool"]["input"] =
        json!({"command":format!("cd {root}/data && rm greeting.txt"), "description":"synthetic"});
    fixture.capture(&request);
    let (_, diagnostic) = invoke(&fixture, request, true);
    assert_eq!(diagnostic["code"], "shadow_evaluated");
    assert!(
        server.received.try_recv().is_ok(),
        "an unattributable relative deletion still reaches the model"
    );
    let dir = &fixture.records()[0];
    assert_eq!(
        dir.get("evaluation")["candidate"]["changes"],
        json!([{"operation":"delete","resource_id":"command:unresolved"}])
    );
}

#[test]
fn opaque_object_mirrors_reach_all_semantic_units_with_complete_native_input() {
    let mut cases = vec![
        (
            "CustomTool",
            json!({"action":"inspect","nested":{"arguments":[1,"raw",false]}}),
        ),
        (
            "Read",
            json!({"file_path":"private.txt","offset":3,"limit":10}),
        ),
        ("Bash", json!({"command":"ls","shell":true})),
        (
            "Bash",
            json!({"command":42,"description":"malformed command field"}),
        ),
        ("Bash", json!({"command":"ls","dir_path":false})),
        (
            "Write",
            json!({"file_path":"greeting.txt","content":"ok","other_path":"private.txt"}),
        ),
        (
            "Write",
            json!({"file_path":null,"content":"retained malformed path"}),
        ),
        (
            "Write",
            json!({"file_path":"greeting.txt","content":["not a string"]}),
        ),
        (
            "Edit",
            json!({"file_path":"greeting.txt","old_string":42,"new_string":"new"}),
        ),
        (
            "Delete",
            json!({"file_path":"greeting.txt","paths":["private.txt"]}),
        ),
    ];
    for path in [
        "odd:name.txt".to_owned(),
        "产物.txt".to_owned(),
        "a".repeat(200),
        "../greeting.txt".to_owned(),
        "./greeting.txt".to_owned(),
        "dir//greeting.txt".to_owned(),
    ] {
        cases.push((
            "Write",
            json!({"file_path":path,"content":"raw replacement"}),
        ));
    }
    for (name, input) in cases {
        let fixture = Fixture::new();
        let server = Server::new(
            r#"{"preservation":0.01,"task_scope":0.99,"snapshot":0.99}"#,
            200,
            Duration::ZERO,
        );
        let mut config = fixture.config(&server.endpoint);
        config["scenarios"] = json!(["preservation", "task_scope", "snapshot"]);
        config["task"]["allowed_resource_ids"] = json!([]);
        config["tools"]["Bash"] = json!({"command_pointer":"/command"});
        config["tools"]["Delete"] = json!({"operation":"delete","path_pointer":"/file_path"});
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!(name);
        request["event"]["tool"]["input"] = input.clone();
        fixture.capture(&request);
        let (response, diagnostic) = invoke(&fixture, request, true);
        assert_eq!(diagnostic["code"], "shadow_evaluated", "{name}: {input}");
        assert_eq!(response["effects"], json!([]));
        let body = server
            .received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            body["questions"]
                .as_object()
                .unwrap()
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            ["preservation", "snapshot", "task_scope"]
        );
        assert_eq!(
            body["state"]["candidate"],
            json!({
            "changes":[{"operation":"replace","resource_id":"command:unresolved"}],
            "native_tool":{"name":name,"input":input}})
        );
        assert_eq!(body["state"]["evidence"], json!([]));
        assert_eq!(body["state"]["local_context"]["files"], json!([]));
        assert!(body["state"]["local_context"]["signals"]
            .as_array()
            .unwrap()
            .contains(&json!({"kind":"opaque_execution","resource_id":null})));
        let call = &fixture.records()[0];
        assert_eq!(call.get("model_request")["state"], body["state"]);
        assert_eq!(
            call.get("evaluation")["local_context"],
            body["state"]["local_context"]
        );
        assert_eq!(call.get("model_result")["status"], "parsed");
        let result = call.get("result");
        for (unit, baseline) in [
            ("preservation", "evidence_gap"),
            ("task_scope", "allowed"),
            ("snapshot", "evidence_gap"),
        ] {
            assert_trace(&result, unit, baseline, "model_fallback", None);
        }
        for trace in result["evaluation"]["judgments"].as_array().unwrap() {
            assert_eq!(trace["model_used"], true);
            assert_eq!(trace["error"], serde_json::Value::Null);
            assert_eq!(
                trace["probability"],
                if trace["unit"] == "preservation" {
                    json!({"raw":0.01,"yes_ppm":10000,"no_ppm":990000})
                } else {
                    json!({"raw":0.99,"yes_ppm":990000,"no_ppm":10000})
                }
            );
        }
        assert_eq!(
            result["evaluation"]["answers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|answer| answer["value"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["allow", "allow", "needs_snapshot"]
        );
        assert_eq!(
            result["evaluation"]["outcome"]["control"]["candidate"],
            "allow"
        );
        assert!(!fixture.root.path().join("greeting.txt").exists());
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn opaque_semantic_gaps_keep_all_failed_judgments_instead_of_skipping() {
    for key in [false, true] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 500, Duration::ZERO);
        let mut config = fixture.config(&server.endpoint);
        config["task"]["allowed_resource_ids"] = json!([]);
        config["scenarios"] = json!(["preservation", "task_scope", "snapshot"]);
        let mut request = fixture.request(config);
        let input = json!({"unknown_action":{"targets":["greeting.txt","private.txt"]}});
        request["event"]["tool"]["native_name"] = json!("CustomTool");
        request["event"]["tool"]["input"] = input.clone();
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, key);
        assert_eq!(diagnostic["code"], "shadow_evaluated");
        let call = &fixture.records()[0];
        assert_model_error(call, if key { "http" } else { "missing_credentials" });
        let projection = call.get("model_request");
        assert_eq!(
            projection["state"]["candidate"]["native_tool"]["input"],
            input
        );
        assert_eq!(projection["questions"].as_object().unwrap().len(), 3);
        let result = call.get("result");
        assert_eq!(
            result["evaluation"]["judgments"].as_array().unwrap().len(),
            3
        );
        for (unit, baseline) in [
            ("preservation", "evidence_gap"),
            ("task_scope", "allowed"),
            ("snapshot", "evidence_gap"),
        ] {
            assert_trace(&result, unit, baseline, "model_fallback", None);
        }
        if key {
            let body = server
                .received
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            assert_eq!(projection["state"], body["state"]);
        } else {
            assert!(server.received.try_recv().is_err());
        }
    }
}

#[test]
fn non_objects_and_invalid_opaque_candidates_still_reject_before_task_loading() {
    for (name, input, code) in [
        ("Write".to_owned(), json!(null), "unsupported_input"),
        (
            "Write".to_owned(),
            json!(["greeting.txt"]),
            "unsupported_input",
        ),
        (
            "Write".to_owned(),
            json!("not an object"),
            "unsupported_input",
        ),
        (
            "CustomTool".to_owned(),
            json!(["opaque but not an object"]),
            "unmapped_tool",
        ),
        (
            "x".repeat(129),
            json!({"payload":"raw"}),
            "candidate_invalid",
        ),
        (
            "CustomTool".to_owned(),
            json!({"payload":"x".repeat(70000)}),
            "oversized_candidate",
        ),
    ] {
        let fixture = Fixture::new();
        let server = Server::new("0.99", 200, Duration::ZERO);
        let mut config = fixture.config(&server.endpoint);
        config["task"]["allowed_resource_ids"] = json!([]);
        let mut request = fixture.request(config);
        request["event"]["tool"]["native_name"] = json!(name);
        request["event"]["tool"]["input"] = input;
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request.clone(), true);
        assert_eq!(diagnostic["code"], code, "{name}");
        let call = &fixture.records()[0];
        assert_eq!(call.names(), ["input", "result"]);
        assert_eq!(call.get("input")["aw_request"]["event"], request["event"]);
        assert_eq!(
            call.get("result"),
            json!({"status":"uncovered","code":code,"evaluation":null})
        );
        assert!(server.received.try_recv().is_err());
    }
}

#[test]
fn read_only_opacity_does_not_authorize_task_and_fallback_errors_are_evaluated() {
    for (key, status) in [(true, 200), (true, 500), (false, 200)] {
        let fixture = Fixture::new();
        let server = Server::new("0.01", status, Duration::ZERO);
        let mut config = fixture.config(&server.endpoint);
        config["task"]["allowed_resource_ids"] = json!([]);
        config["tools"]["Bash"] = json!({"command_pointer":"/command"});
        let mut request = fixture.request(config);
        // A sensitive read stays semantic: read-only is not task permission.
        let input = json!({"command":"cat .env","description":"read-only is not task permission"});
        request["event"]["tool"]["native_name"] = json!("Bash");
        request["event"]["tool"]["input"] = input.clone();
        fixture.capture(&request);
        let (_, diagnostic) = invoke(&fixture, request, key);
        assert_eq!(diagnostic["code"], "shadow_evaluated");
        let call = &fixture.records()[0];
        assert_eq!(
            call.get("model_request")["state"]["candidate"]["native_tool"]["input"],
            input
        );
        if key {
            let body = server
                .received
                .recv_timeout(Duration::from_secs(1))
                .unwrap();
            assert_eq!(body["state"]["candidate"]["native_tool"]["input"], input);
        } else {
            assert!(server.received.try_recv().is_err());
        }
        if key && status == 200 {
            assert_eq!(call.get("model_result")["status"], "parsed");
            assert_eq!(
                call.get("result")["evaluation"]["answers"][0]["value"],
                "outside_scope"
            );
            assert_eq!(
                call.get("result")["evaluation"]["outcome"]["control"]["candidate"],
                "block"
            );
        } else {
            assert_model_error(call, if key { "http" } else { "missing_credentials" });
        }
        assert_trace(
            &call.get("result"),
            "task_scope",
            "allowed",
            "model_fallback",
            None,
        );
    }
}
