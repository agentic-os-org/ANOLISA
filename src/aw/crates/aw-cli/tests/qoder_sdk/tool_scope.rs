//! Real SDK launch callbacks retain routing and native vetoes under tool selection.

use super::*;
#[allow(dead_code)]
#[path = "../regression/tool_scope.rs"]
mod scope;

#[test]
fn scoped_callbacks_keep_skips_neutral_and_later_denials_authoritative() {
    for denied in [false, true] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        document["spec"]["agents"]["pi"] = json!({"adapter":"pi","argv":["/bin/true"]});
        document["spec"]["providers"] = json!({});
        document["spec"]["events"] = json!({});
        for (id, tools, behavior) in [
            ("first", "file_read", "native-error"),
            ("observe", "bash", "observe"),
            ("middle", "native:pi:Bash", "native-error"),
            (
                "decision",
                "native:qoder-sdk:Bash",
                if denied { "deny" } else { "observe" },
            ),
            ("last", "file_write", "native-error"),
        ] {
            scope::step(
                &fixture,
                &mut document,
                "tool.before",
                id,
                &[tools],
                behavior,
            );
        }
        document["spec"]["events"]["tool.before"]["match"] =
            json!({"tools":["native:qoder-sdk:Bash"]});
        scope::step(
            &fixture,
            &mut document,
            "tool.after",
            "after",
            &["*"],
            "observe",
        );
        document["spec"]["events"]["tool.after"]["steps"][0]
            .as_object_mut()
            .unwrap()
            .remove("match");
        let service = Service::start(&fixture, &document);
        assert_eq!(fixture.successful(launch(&fixture))["denied"], denied);
        assert_eq!(fixture.root.join("executed").exists(), !denied);
        assert!(fixture.root.join("query-closed").exists());
        scope::drained(&service);
        let requests = scope::invocations(&fixture);
        let labels: Vec<_> = requests
            .iter()
            .map(|request| request["label"].as_str().unwrap())
            .collect();
        assert_eq!(
            labels,
            if denied {
                vec!["observe", "decision"]
            } else {
                vec!["observe", "decision", "after"]
            }
        );
        for request in requests {
            assert_eq!(request["event"]["tool"]["name"], "bash");
            assert_eq!(request["event"]["tool"]["native_name"], "Bash");
        }
        let mut before = vec![("first", true), ("observe", false), ("decision", false)];
        if !denied {
            before.push(("last", true));
        }
        scope::audit(
            &fixture,
            "tool.before",
            &before,
            if denied { "closed" } else { "completed" },
        );
        if !denied {
            scope::audit(&fixture, "tool.after", &[("after", false)], "completed");
        }
        service.released(&fixture);
    }
}

#[test]
fn all_skipped_callbacks_close_and_preserve_existing_native_vetoes() {
    for (event_mismatch, native_denial) in [(false, false), (true, false), (false, true)] {
        let fixture = Fixture::new();
        let mut document = setup(&fixture);
        document["spec"]["providers"] = json!({});
        document["spec"]["events"] = json!({});
        for event in ["tool.before", "tool.after"] {
            for id in ["first", "middle", "last"] {
                scope::step(
                    &fixture,
                    &mut document,
                    event,
                    id,
                    // Native/normalized aliases remain runtime decisions; provably
                    // disjoint selectors are removed before native registration.
                    &[if event_mismatch {
                        "native:qoder-sdk:Bash"
                    } else {
                        "file_read"
                    }],
                    "native-error",
                );
            }
            if event_mismatch {
                document["spec"]["events"][event]["match"] = json!({"tools":["file_read"]});
            }
        }
        if native_denial {
            let path = fixture.root.join("app.mjs");
            let source = fs::read_to_string(&path).unwrap().replace(
                "settingSources: [], cwd: process.cwd(),",
                "settingSources: [], cwd: process.cwd(), hooks: { PreToolUse: [{ hooks: [async () => ({ hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: 'native veto' } })] }] },",
            );
            fs::write(path, source).unwrap();
        }
        let service = Service::start(&fixture, &document);
        assert_eq!(
            fixture.successful(launch(&fixture))["denied"],
            native_denial
        );
        assert_eq!(fixture.root.join("executed").exists(), !native_denial);
        assert!(scope::invocations(&fixture).is_empty());
        scope::drained(&service);
        if !native_denial {
            for event in ["tool.before", "tool.after"] {
                scope::audit(
                    &fixture,
                    event,
                    &[("first", true), ("middle", true), ("last", true)],
                    "completed",
                );
            }
        }
        service.released(&fixture);
    }
}
