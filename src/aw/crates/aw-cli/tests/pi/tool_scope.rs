//! Pi's retained tool names and native result envelopes survive Host selector skips.

use super::*;
#[allow(dead_code)]
#[path = "../regression/tool_scope.rs"]
mod scope;

fn scoped_setup(fixture: &Fixture) -> Value {
    let document = setup(fixture);
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pi/scoped.mjs"),
        fixture.root.join("app.mjs"),
    )
    .unwrap();
    document
}

#[test]
fn scoped_callbacks_preserve_names_skips_and_later_denials() {
    for denied in [false, true] {
        let fixture = Fixture::new();
        let mut document = scoped_setup(&fixture);
        document["spec"]["agents"]["sdk"] = json!({"adapter":"qoder-sdk","argv":["/bin/true"]});
        for (id, tools, behavior) in [
            ("first", "file_read", "native-error"),
            ("observe", "native:pi:test", "observe"),
            ("middle", "native:qoder-sdk:test", "native-error"),
            (
                "decision",
                "native:pi:test",
                if denied { "deny" } else { "observe" },
            ),
            ("last", "bash", "native-error"),
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
        document["spec"]["events"]["tool.before"]["match"] = json!({"tools":["native:pi:test"]});
        scope::step(
            &fixture,
            &mut document,
            "tool.after",
            "after",
            &["*"],
            "observe",
        );
        let service = Service::start(&fixture, &document);
        assert_eq!(fixture.successful(launch(&fixture))["denied"], denied);
        assert_eq!(fixture.root.join("side-effect").exists(), !denied);
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
            assert_eq!(request["event"]["tool"]["name"], "test");
            assert_eq!(request["event"]["tool"]["native_name"], "test");
        }
        let mut before = vec![("first", true), ("observe", false), ("decision", false)];
        // The foreign SDK step is removed at admission, before a Pi event exists.
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
fn skipped_final_replacement_preserves_native_envelope_and_failure() {
    for error in [false, true] {
        let fixture = Fixture::new();
        let mut document = scoped_setup(&fixture);
        scope::step(
            &fixture,
            &mut document,
            "tool.before",
            "first",
            &["file_read"],
            "native-error",
        );
        scope::step(
            &fixture,
            &mut document,
            "tool.after",
            "observe",
            &["native:pi:test"],
            "observe",
        );
        scope::step(
            &fixture,
            &mut document,
            "tool.after",
            "compress",
            &["bash"],
            "replace",
        );
        let service = Service::start(&fixture, &document);
        let mut command = launch(&fixture);
        if error {
            command.env("PI_SCOPE_ERROR", "1");
        }
        let report = fixture.successful(command);
        assert_eq!(report["denied"], false);
        assert_eq!(report["result"], report["expected"]);
        assert_eq!(report["result"]["isError"], error);
        assert!(fixture.root.join("side-effect").exists());
        let requests = scope::invocations(&fixture);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["label"], "observe");
        assert_eq!(requests[0]["event"]["tool"]["result"], report["expected"]);
        scope::drained(&service);
        scope::audit(&fixture, "tool.before", &[("first", true)], "completed");
        scope::audit(
            &fixture,
            "tool.after",
            &[("observe", false), ("compress", true)],
            "completed",
        );
        service.released(&fixture);
    }
}

#[test]
fn all_skipped_callbacks_close_and_native_permissions_keep_precedence() {
    for (event_mismatch, native_denial) in [(false, false), (true, false), (false, true)] {
        let fixture = Fixture::new();
        let mut document = scoped_setup(&fixture);
        for event in ["tool.before", "tool.after"] {
            for id in ["first", "middle", "last"] {
                scope::step(
                    &fixture,
                    &mut document,
                    event,
                    id,
                    &[if event_mismatch {
                        "native:pi:test"
                    } else {
                        "bash"
                    }],
                    "native-error",
                );
            }
            if event_mismatch {
                document["spec"]["events"][event]["match"] = json!({"tools":["bash"]});
            }
        }
        let service = Service::start(&fixture, &document);
        let mut command = launch(&fixture);
        if native_denial {
            command.env("PI_NATIVE_DENY", "1");
        }
        assert_eq!(fixture.successful(command)["denied"], native_denial);
        assert_eq!(fixture.root.join("side-effect").exists(), !native_denial);
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
