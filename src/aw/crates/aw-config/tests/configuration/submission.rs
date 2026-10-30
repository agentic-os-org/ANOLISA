//! Input policy cannot be projected out by tool selectors or claim tool-only actions.

use super::*;

#[test]
fn submission_accepts_blocking_without_tool_selectors_or_native_steps() {
    let mut value = example();
    let mut event = value["spec"]["events"]["tool.before"].clone();
    event.as_object_mut().unwrap().remove("match");
    event.as_object_mut().unwrap().remove("guard");
    for step in event["steps"].as_array_mut().unwrap() {
        step.as_object_mut().unwrap().remove("match");
    }
    value["spec"]["events"]["input.submit"] = event;
    assert!(parse(&value).is_ok());
    for pointer in [
        "/spec/events/input.submit/match",
        "/spec/events/input.submit/steps/0/match",
    ] {
        let mut invalid = value.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        invalid.pointer_mut(parent).unwrap()[field] = json!({"tools":["*"]});
        assert!(parse(&invalid).is_err());
    }
    for effect in ["replace_result", "replace_input", "ask"] {
        let mut invalid = value.clone();
        invalid["spec"]["events"]["input.submit"]["steps"][0]["effects"] = json!([effect]);
        assert!(parse(&invalid).is_err());
    }
}
