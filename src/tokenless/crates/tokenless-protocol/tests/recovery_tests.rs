use serde_json::json;
use tokenless_protocol::{BeforeModelCapabilities, PostToolCapabilities, RecoveryMethod};

#[test]
fn recovery_wire_is_strict_and_round_trips() {
    for value in [
        json!({"kind":"none"}),
        json!({"kind":"shell"}),
        json!({"kind":"tool","name":"tenant-retrieve_2"}),
    ] {
        let method: RecoveryMethod = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(method).unwrap(), value);
    }
    for value in [
        json!({}),
        json!({"kind":"unknown"}),
        json!({"kind":"tool"}),
        json!({"kind":"none","name":"retrieve"}),
        json!({"kind":"shell","extra":true}),
        json!({"kind":"tool","name":"retrieve","extra":true}),
    ] {
        assert!(
            serde_json::from_value::<RecoveryMethod>(value.clone()).is_err(),
            "{value}"
        );
    }
}

#[test]
fn tool_names_are_checked_at_native_and_wire_boundaries() {
    for name in [
        "".into(),
        "a".repeat(65),
        "tool name".into(),
        "tool\nname".into(),
        "tool;rm".into(),
        "工具".into(),
    ] {
        assert!(RecoveryMethod::tool(&name).is_err());
        assert!(
            serde_json::from_value::<RecoveryMethod>(json!({"kind":"tool","name":name})).is_err()
        );
    }
    assert!(RecoveryMethod::tool("a".repeat(64)).is_ok());
}

#[test]
fn recovery_is_required_and_replaces_boolean_capabilities() {
    for value in [
        json!({}),
        json!({"retrieval_available":false}),
        json!({"recovery":{"kind":"none"},"retrieval_available":false}),
    ] {
        assert!(serde_json::from_value::<BeforeModelCapabilities>(value.clone()).is_err());
        assert!(serde_json::from_value::<PostToolCapabilities>(value).is_err());
    }
    assert!(
        serde_json::from_value::<BeforeModelCapabilities>(json!({"recovery":{"kind":"none"}}))
            .is_ok()
    );
    assert!(
        serde_json::from_value::<PostToolCapabilities>(json!({"recovery":{"kind":"shell"}}))
            .is_ok()
    );
}
