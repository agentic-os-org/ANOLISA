//! Replay rejection remains exact with a slow old invocation and no completed history.

use super::*;

fn value(call: &str) -> Value {
    json!({"session_id":"session", "tool":{"call_id":call,"native_name":"tool","input":{"n":1}}})
}

#[test]
fn numeric_spellings_match_without_accepting_changed_values_or_types() {
    for (original, native) in [
        ("1.0", "1"),
        ("1e2", "100"),
        ("-0.0", "0"),
        ("9007199254740993", "9007199254740992"),
        ("1000000000000000128", "1000000000000000100"),
    ] {
        let original: Value = serde_json::from_str(original).unwrap();
        let native: Value = serde_json::from_str(native).unwrap();
        assert_eq!(
            digest("tool", &json!({"nested":[original]})).unwrap(),
            digest("tool", &json!({"nested":[native]})).unwrap()
        );
    }
    for changed in [json!(2), json!(true), json!("1"), Value::Null] {
        assert_ne!(
            digest("tool", &json!(1.0)).unwrap(),
            digest("tool", &changed).unwrap()
        );
    }
    assert_ne!(
        digest("tool", &json!(9007199254740992_u64)).unwrap(),
        digest("tool", &json!(9007199254740994_u64)).unwrap()
    );
}

#[test]
fn old_active_call_survives_unbounded_later_retirement() {
    let mut ledger = Ledger::default();
    ledger
        .register(1, "slow", "tool", &json!({"n":1}), 2)
        .unwrap();
    for sequence in 2..10_002 {
        let call = format!("call-{sequence}");
        ledger
            .register(sequence, &call, "tool", &json!({"n":1}), 2)
            .unwrap();
        ledger.check(&value(&call)).unwrap();
        ledger.active.remove(&call);
        assert_eq!(ledger.active.len(), 1);
        assert!(ledger.check(&value(&call)).is_err());
        assert!(ledger
            .register(sequence, &call, "tool", &json!({"n":1}), 2)
            .is_err());
        ledger.check(&value("slow")).unwrap();
    }
    assert_eq!(ledger.issued, 10_001);
    ledger.active.remove("slow");
    assert!(ledger.check(&value("slow")).is_err());
    assert!(ledger.check_close("session", "slow").is_err());
}

#[test]
fn overload_consumes_sequence_and_changed_identity_remains_rejected() {
    let mut ledger = Ledger::default();
    assert!(ledger.register(2, "gap", "tool", &json!({}), 1).is_err());
    ledger
        .register(1, "first", "tool", &json!({"n":1}), 1)
        .unwrap();
    assert!(ledger
        .register(2, "overload", "tool", &json!({"n":1}), 1)
        .is_err());
    ledger.check(&value("first")).unwrap();
    let mut changed = value("first");
    changed["tool"]["input"]["n"] = json!(2);
    assert!(ledger.check(&changed).is_err());
    changed = value("first");
    changed["session_id"] = json!("foreign");
    assert!(ledger.check(&changed).is_err());
    ledger.active.remove("first");
    assert!(ledger
        .register(2, "overload", "tool", &json!({"n":1}), 1)
        .is_err());
    ledger
        .register(3, "new", "tool", &json!({"n":1}), 1)
        .unwrap();
    ledger.active.get_mut("new").unwrap().closing = true;
    assert!(ledger.check(&value("new")).is_err());
}
