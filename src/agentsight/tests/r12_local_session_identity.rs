// Local collection is a desktop module; compile its real source on Linux too.
// Existing desktop-only legacy fallback predates the let-chain lint.
#[allow(clippy::collapsible_if)]
#[path = "../src/local/collector/converter.rs"]
mod converter;
use converter::convert_jsonl_content_to_atif;
use serde_json::{Value, json};

fn convert(events: &[Value]) -> agentsight_atif::AtifTrajectory {
    let content = events
        .iter()
        .map(|event| format!("{event}\n"))
        .collect::<String>();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    std::fs::write(&path, &content).unwrap();
    let result = converter::convert_jsonl_to_atif(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        serde_json::to_value(convert_jsonl_content_to_atif(&content).unwrap()).unwrap()
    );
    result.validate_step_ids().unwrap();
    result
}

#[test]
fn claude_message_metadata_survives_file_conversion_and_analysis() {
    let result = convert(&[
        json!({"type":"user","sessionId":"session-fixture","version":"2.0.1",
            "message":{"content":"inspect this"}}),
        json!({"type":"assistant","sessionId":"session-fixture","version":"2.0.1",
            "message":{"model":"fixture-model","content":[{"type":"text","text":"done"}]}}),
    ]);
    assert_eq!(result.session_id.as_deref(), Some("session-fixture"));
    assert_eq!(result.agent.version, "2.0.1");
    assert_eq!(result.agent.model_name.as_deref(), Some("fixture-model"));
    let analyzed =
        agentsight_opt::AtifTrajectory::from_json(&serde_json::to_string(&result).unwrap())
            .unwrap();
    assert_eq!(analyzed.session_id, "session-fixture");
    assert_eq!(analyzed.model_name(), "fixture-model");
}

#[test]
fn explicit_runtime_metadata_wins_over_message_fallbacks() {
    for config_first in [true, false] {
        let message = json!({"type":"assistant","sessionId":"fallback","version":"old",
            "message":{"model":"step-model","content":[{"type":"text","text":"done"}]}});
        let config = json!({"type":"runtime-config","sessionId":"explicit","version":"new","model":"default-model"});
        let events = if config_first {
            vec![config, message]
        } else {
            vec![message, config]
        };
        let result = convert(&events);
        assert_eq!(result.session_id.as_deref(), Some("explicit"));
        assert_eq!(result.agent.version, "new");
        assert_eq!(result.agent.model_name.as_deref(), Some("default-model"));
        assert_eq!(result.steps[0].model_name.as_deref(), Some("step-model"));
    }
}

#[test]
fn empty_metadata_does_not_mask_later_identification() {
    let result = convert(&[
        json!({"type":"user","sessionId":"","version":null,"message":{"content":"start"}}),
        json!({"type":"user","sessionId":"known","version":"v1","message":{"content":"next"}}),
    ]);
    assert_eq!(result.session_id.as_deref(), Some("known"));
    assert_eq!(result.agent.version, "v1");
}
