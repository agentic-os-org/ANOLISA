// Shared integration-test helpers. Each test binary that declares `mod support`
// compiles this file independently and uses only the subset it needs, so a
// helper unused by one binary is not dead code across the suite.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixStream;
use uuid::Uuid;

pub async fn run_frozen_pap_crud_scenario(path: &Path, fixture: &Value) {
    let objects = fixture["objects"].as_object().unwrap();
    let mut variables = BTreeMap::new();
    let mut request_ids = BTreeSet::new();

    for step in fixture["steps"].as_array().unwrap() {
        let step_name = step["name"].as_str().unwrap();
        let request_value = expand_fixture(&step["request"], objects, &variables, 0);
        let response = request_json(path, &request_value).await;

        let request_id = response["requestId"]
            .as_str()
            .unwrap_or_else(|| panic!("{step_name} did not return a requestId"));
        Uuid::parse_str(request_id)
            .unwrap_or_else(|error| panic!("{step_name} returned a non-UUID requestId: {error}"));
        assert!(
            request_ids.insert(request_id.to_owned()),
            "{step_name} reused daemon requestId {request_id}"
        );
        variables.insert(
            "request_id".to_owned(),
            Value::String(request_id.to_owned()),
        );

        for capture in step["captures"].as_array().unwrap() {
            let name = capture["name"].as_str().unwrap();
            let pointer = capture["pointer"].as_str().unwrap();
            let captured = response
                .pointer(pointer)
                .unwrap_or_else(|| panic!("{step_name} did not return capture {name} at {pointer}"))
                .clone();
            match capture["format"].as_str().unwrap() {
                "uuid" => {
                    let candidate = captured.as_str().unwrap_or_else(|| {
                        panic!("{step_name} capture {name} is not a string UUID")
                    });
                    Uuid::parse_str(candidate).unwrap_or_else(|error| {
                        panic!("{step_name} capture {name} is not a UUID: {error}")
                    });
                }
                format => panic!("unsupported fixture capture format {format}"),
            }
            assert!(
                variables.insert(name.to_owned(), captured).is_none(),
                "fixture captured {name} more than once"
            );
        }

        let expected = expand_fixture(&step["expectedResponse"], objects, &variables, 0);
        assert_eq!(response, expected, "unexpected response for {step_name}");
    }

    let resource_ids = ["policy_id", "scope_id", "binding_id"]
        .map(|name| variables.get(name).unwrap().as_str().unwrap());
    assert_ne!(resource_ids[0], resource_ids[1]);
    assert_ne!(resource_ids[0], resource_ids[2]);
    assert_ne!(resource_ids[1], resource_ids[2]);
}

pub async fn request_json(path: &Path, request_value: &Value) -> Value {
    let mut payload = serde_json::to_vec(request_value).unwrap();
    payload.push(b'\n');
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream.write_all(&payload).await.unwrap();
    let mut response = Vec::new();
    BufReader::new(stream)
        .read_until(b'\n', &mut response)
        .await
        .unwrap();
    assert_eq!(response.pop(), Some(b'\n'));
    serde_json::from_slice(&response).unwrap()
}

fn expand_fixture(
    value: &Value,
    objects: &serde_json::Map<String, Value>,
    variables: &BTreeMap<String, Value>,
    depth: u8,
) -> Value {
    assert!(depth < 32, "fixture reference cycle");
    match value {
        Value::Object(fields) if fields.len() == 1 && fields.contains_key("$ref") => {
            let reference = fields["$ref"].as_str().unwrap();
            expand_fixture(
                objects
                    .get(reference)
                    .unwrap_or_else(|| panic!("unknown fixture object {reference}")),
                objects,
                variables,
                depth + 1,
            )
        }
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        expand_fixture(value, objects, variables, depth + 1),
                    )
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|value| expand_fixture(value, objects, variables, depth + 1))
                .collect(),
        ),
        Value::String(candidate) if candidate.starts_with("${") && candidate.ends_with('}') => {
            let name = &candidate[2..candidate.len() - 1];
            variables
                .get(name)
                .unwrap_or_else(|| panic!("unknown fixture variable {name}"))
                .clone()
        }
        _ => value.clone(),
    }
}
