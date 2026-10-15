//! Presentation schemas reject executable data and do not extend event effects.

use aw_provider::{view, Protocol};
use serde_json::{json, Value};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("examples/views")
            .join(format!("{name}.json")),
    )
    .unwrap()
}

#[test]
fn published_panel_states_and_correlated_view_fixtures_are_valid() {
    let protocol = view::Protocol::new().unwrap();
    for name in [
        "panel-ok",
        "panel-offline",
        "panel-component-missing",
        "panel-no-data",
        "panel-partial",
        "panel-stale",
    ] {
        let panel = protocol.check_panel(&fixture(name)).unwrap();
        assert_eq!(panel["api_version"], view::PANEL_VERSION);
        assert_eq!(panel["providers"][0]["admission"], "unknown");
    }
    for method in ["describe", "metrics"] {
        let request = protocol
            .parse_request(&fixture(&format!("request-{method}")))
            .unwrap();
        protocol
            .check_response(&request, &fixture(&format!("response-{method}")))
            .unwrap();
        assert!(Protocol::new()
            .unwrap()
            .parse_request(&fixture(&format!("request-{method}")))
            .is_err());
    }
}

#[test]
fn presentation_boundary_rejects_ambiguity_commands_wrong_identity_and_limits() {
    let protocol = view::Protocol::new().unwrap();
    let request = protocol.parse_request(&fixture("request-metrics")).unwrap();
    let response: Value = serde_json::from_slice(&fixture("response-metrics")).unwrap();
    for key in [
        "protocol",
        "operation",
        "request_id",
        "provider_id",
        "config_revision",
    ] {
        let mut value = response.clone();
        value[key] = json!("other");
        assert!(protocol
            .check_response(&request, &serde_json::to_vec(&value).unwrap())
            .is_err());
    }
    for (pointer, value) in [
        ("/budget_ms", json!(0)),
        ("/budget_ms", json!(2001)),
        ("/parameters/limit", json!(201)),
        ("/view", json!("execute")),
    ] {
        let mut bad = request.clone();
        *bad.pointer_mut(pointer).unwrap() = value;
        assert!(protocol
            .parse_request(&serde_json::to_vec(&bad).unwrap())
            .is_err());
    }
    let mut bad = response.clone();
    bad["effects"] = json!([{"type":"block"}]);
    assert!(protocol
        .check_response(&request, &serde_json::to_vec(&bad).unwrap())
        .is_err());
    bad = response;
    bad["data"]["before_text"] = json!("private body");
    assert!(protocol
        .check_response(&request, &serde_json::to_vec(&bad).unwrap())
        .is_err());
    let mut panel: Value = serde_json::from_slice(&fixture("panel-ok")).unwrap();
    panel["providers"][0]["admission"] = json!("accepted");
    assert!(protocol
        .check_panel(&serde_json::to_vec(&panel).unwrap())
        .is_err());
    for bytes in [
        br#"{"x":1,"x":2}"#.as_slice(),
        br#"{"x":18446744073709551616}"#,
        b"{} {}",
    ] {
        assert!(view::decode(bytes).is_err());
    }
    assert!(view::decode(&vec![b' '; aw_provider::MAX_MESSAGE_BYTES + 1]).is_err());
    let mut deep = "null".to_owned();
    for _ in 0..34 {
        deep = format!("[{deep}]");
    }
    assert!(view::decode(deep.as_bytes()).is_err());
}

#[test]
fn recent_results_require_data_only_for_success() {
    let protocol = view::Protocol::new().unwrap();
    let panel: Value = serde_json::from_slice(&fixture("panel-ok")).unwrap();
    for (status, code, data) in [
        ("ok", Value::Null, Value::Null),
        (
            "ok",
            json!("query_failed"),
            panel["recent_execution"]["data"].clone(),
        ),
        (
            "unavailable",
            json!("query_failed"),
            panel["recent_execution"]["data"].clone(),
        ),
    ] {
        let mut invalid = panel.clone();
        invalid["recent_execution"]["status"] = json!(status);
        invalid["recent_execution"]["code"] = code;
        invalid["recent_execution"]["data"] = data;
        assert!(protocol
            .check_panel(&serde_json::to_vec(&invalid).unwrap())
            .is_err());
    }
}
