//! Compound edits retain the snapshot on step failure after Provider insertion.

use super::{Fixture, MINIMAL};
use aw_package::config_edit::CommandHook;
use serde_json::json;

#[test]
fn failed_command_edits_retain_the_entire_working_snapshot() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut editor = fixture.editor();
    let argv = vec!["/unexecuted/check".into()];
    let hook = |id, provider, effect, on_error| CommandHook {
        id,
        provider,
        argv: &argv,
        effect,
        on_error,
        reason_code: None,
    };
    editor
        .add_command_hook("tool.before", hook("first", "first", "observe", "report"))
        .unwrap();
    let before = editor.as_value().clone();
    for (event, id, provider, effect, on_error) in [
        ("tool.before", "first", "orphan", "block", "block"),
        ("tool.after", "new", "orphan", "block", "report"),
        ("tool.after", "new", "orphan", "observe", "block"),
        ("unknown.event", "new", "orphan", "observe", "report"),
    ] {
        assert!(editor
            .add_command_hook(event, hook(id, provider, effect, on_error))
            .is_err());
        assert_eq!(editor.as_value(), &before);
    }
    editor.save().unwrap();
    assert_eq!(fixture.editor().as_value(), &before);
    fixture.only_configuration_remains();
}

#[test]
fn disabled_events_and_conflicting_provider_names_are_not_changed() {
    let fixture = Fixture::new(MINIMAL.as_bytes());
    let mut editor = fixture.editor();
    editor
        .add_event("session.start", json!({"enabled":false,"steps":[]}))
        .unwrap();
    let argv = vec!["/unexecuted/check".into()];
    let hook = CommandHook {
        id: "check",
        provider: "existing",
        argv: &argv,
        effect: "observe",
        on_error: "report",
        reason_code: None,
    };
    let before = editor.as_value().clone();
    assert!(editor.add_command_hook("session.start", hook).is_err());
    assert_eq!(editor.as_value(), &before);
    editor.add_provider("existing", json!({"transport":{"type":"stdio","location":"agent","argv":["/different/script"]},"timeout_ms":1000,"max_output_bytes":32})).unwrap();
    let before = editor.as_value().clone();
    let hook = CommandHook {
        id: "check",
        provider: "existing",
        argv: &argv,
        effect: "observe",
        on_error: "report",
        reason_code: None,
    };
    assert!(editor.add_command_hook("tool.before", hook).is_err());
    assert_eq!(editor.as_value(), &before);
}
