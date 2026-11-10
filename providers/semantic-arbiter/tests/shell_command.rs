//! Lexical Bash projection through the real Provider process.

#[path = "support/fixture.rs"]
mod fixture;

use std::time::Duration;

use fixture::*;
use serde_json::json;

#[test]
fn shell_commands_project_named_targets_and_leave_the_rest_to_the_model() {
    // (command, extra input, allowed ceiling, expected code, resource, control, model called)
    for (command, extra, ceiling, code, resource, control, model) in [
        (
            "echo hi > greeting.txt",
            None,
            false,
            "shadow_evaluated",
            "greeting",
            "block",
            false,
        ),
        (
            "cat > ./greeting.txt <<'EOF'\nrm -rf x\nEOF",
            None,
            false,
            "shadow_evaluated",
            "greeting",
            "block",
            false,
        ),
        (
            "rm -rf .",
            None,
            false,
            "shadow_evaluated",
            "greeting",
            "block",
            false,
        ),
        (
            "sed -i 's/a/b/' private.txt",
            None,
            true,
            "shadow_evaluated",
            "private",
            "block",
            false,
        ),
        (
            "touch new.txt",
            None,
            false,
            "shadow_evaluated",
            "path:new.txt",
            "allow",
            true,
        ),
        (
            "python build.py",
            None,
            false,
            "shadow_evaluated",
            "command:unresolved",
            "allow",
            true,
        ),
        (
            "python build.py",
            None,
            true,
            "shadow_evaluated",
            "command:unresolved",
            "block",
            false,
        ),
        (
            "echo hi > greeting.txt",
            Some(("dir_path", json!("sub"))),
            false,
            "shadow_evaluated",
            "command:unresolved",
            "allow",
            true,
        ),
        (
            "git status && ls 2>/dev/null",
            None,
            false,
            "no_mutation",
            "",
            "",
            false,
        ),
        (
            "ls",
            Some(("shell", json!(true))),
            false,
            "unsupported_input",
            "",
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
        assert_eq!(
            evaluation["candidate"]["resource_id"], resource,
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
