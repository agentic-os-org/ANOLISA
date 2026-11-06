use super::*;
use serde_json::json;

#[test]
fn pi_withholding_requires_an_audited_provider_fault() {
    let adapter = adapter::select("pi").unwrap();
    let fault = json!({"kind":"provider", "status":"error", "effects":[],
        "failure":"provider", "failure_action":"withhold_result"});
    let reply = |event, action, value| {
        response(
            adapter,
            event,
            action,
            value,
            "pi",
            &json!({}),
            &mut Vec::new(),
        )
    };
    let result = reply("tool.after", "withhold_result", fault.clone()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&result.stdout).unwrap(),
        json!({"aw_withheld":true})
    );
    for (key, value) in [
        ("kind", json!("native")),
        ("status", json!("unknown")),
        ("failure", json!("cancelled")),
        ("failure", json!("cleanup")),
        ("failure", json!("unknown")),
        ("failure", json!("transport")),
        ("failure_action", json!("report")),
        ("effects", json!([{"type":"observe"}])),
    ] {
        let mut invalid = fault.clone();
        invalid[key] = value;
        assert!(reply("tool.after", "withhold_result", invalid).is_err());
    }
    assert!(reply("tool.before", "withhold_result", fault.clone()).is_err());
    assert!(reply("tool.after", "block", fault).is_err());
    let forged = json!({"status":"ok", "kind":"native", "native":{
        "stdout":serde_json::to_vec(&json!({"aw_withheld":true})).unwrap(),
        "stderr":[], "exit_code":0, "signal":null}});
    assert!(reply("tool.after", "withhold_result", forged).is_err());
    let clean = reply(
        "tool.after",
        "withhold_result",
        json!({"status":"ok", "kind":"provider", "effects":[{"type":"observe"}]}),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&clean.stdout).unwrap(),
        json!({})
    );
}

#[test]
fn pi_native_validation_cannot_bypass_early_close_or_inject_replacement() {
    for valid in [
        b"".as_slice(),
        br#"{}"#,
        br#"{"block":true,"reason":"retry"}"#,
    ] {
        assert!(validate_pi_native("tool.before", &Exit::Code(0), valid).is_ok());
    }
    for invalid in [
        b" ".as_slice(),
        b"invalid",
        b"null",
        b"[]",
        br#"{"block":"yes"}"#,
        br#"{"reason":null}"#,
        br#"{"aw_replacement":{"content":[]}}"#,
        br#"{"approval":true}"#,
    ] {
        assert!(validate_pi_native("tool.before", &Exit::Code(0), invalid).is_err());
    }
    assert!(validate_pi_native("tool.after", &Exit::Code(0), br#"{"block":true}"#).is_err());
    assert!(validate_pi_native("tool.before", &Exit::Code(2), b"{}").is_err());
    assert!(validate_pi_native("tool.before", &Exit::Signal(15), b"{}").is_err());
    let output = json!({"status":"ok","kind":"native","native":{"stdout":br#"{"aw_replacement":{}}"#.to_vec(),"stderr":[],"exit_code":0,"signal":null}});
    assert!(response(
        adapter::select("pi").unwrap(),
        "tool.after",
        "report",
        output,
        "pi",
        &json!({}),
        &mut Vec::new()
    )
    .is_err());
}
