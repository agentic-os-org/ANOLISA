//! Shared boolean user script and desired policy for both native launcher fixtures.

use crate::support::Fixture;
use serde_json::{json, Value};
use std::fs;

pub fn configure(fixture: &Fixture, document: &mut Value) {
    let script = fixture.root.join("boolean check.py");
    fs::write(
        &script,
        r#"import json, sys
event = json.load(sys.stdin)
assert 'request_id' not in event and 'input_digest' not in event and 'method' not in event
def contains(value):
    if isinstance(value, str): return '12345' in value
    if isinstance(value, list): return any(contains(item) for item in value)
    if isinstance(value, dict): return any(contains(item) for item in value.values())
    return False
if event['tool']['input']['command'] == 'invalid':
    print('null')
else:
    print(json.dumps(contains(event['tool']['input'])))
"#,
    )
    .unwrap();
    document["spec"]["providers"] = json!({"check":{
        "protocol":"aw-provider/v1alpha1",
        "transport":{"type":"stdio","location":"agent","argv":[env!("CARGO_BIN_EXE_aw"),"policy"]},
        "timeout_ms":2000,"max_output_bytes":4096,
        "config":{"version":1,"argv":[fixture.python,script],"timeout_ms":1000,
        "on_true":{"type":"block","reason_code":"parameter_contains_12345"}}}});
    document["spec"]["events"] = json!({"tool.before":{"enabled":true,"required":true,
        "steps":[{"id":"boolean-check","provider":"check","operation":"check","effects":["block"],"on_error":"block"}]}});
}
