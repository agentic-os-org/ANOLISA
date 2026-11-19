//! Lexical Bash projection through the real Provider process.

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
        (
            "git status && ls 2>/dev/null",
            None,
            false,
            "no_mutation",
            vec![],
            "",
            false,
        ),
        (
            "git -C other status",
            None,
            false,
            "no_mutation",
            vec![],
            "",
            false,
        ),
        (
            "rustc --version",
            None,
            false,
            "no_mutation",
            vec![],
            "",
            false,
        ),
        (
            "ls",
            Some(("shell", json!(true))),
            false,
            "unsupported_input",
            vec![],
            "",
            false,
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
        let (_, diagnostic) = invoke(&fixture, request, true);
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
            assert!(body.to_string().contains(&command.replace('\n', "\\n")));
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
