use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

pub(super) fn expand(value: Value, objects: &BTreeMap<String, Value>) -> Value {
    expand_references(value, objects, &mut BTreeSet::new())
}

fn expand_references(
    value: Value,
    objects: &BTreeMap<String, Value>,
    visiting: &mut BTreeSet<String>,
) -> Value {
    match value {
        Value::Object(mut map) => {
            if let Some(reference) = map.remove("$ref") {
                assert!(
                    map.is_empty(),
                    "reference must not silently override fields"
                );
                let name = reference
                    .as_str()
                    .expect("fixture reference must be a string");
                assert!(
                    visiting.insert(name.into()),
                    "cyclic fixture reference: {name}"
                );
                let object = objects
                    .get(name)
                    .unwrap_or_else(|| panic!("missing fixture reference: {name}"));
                let expanded = expand_references(object.clone(), objects, visiting);
                visiting.remove(name);
                expanded
            } else {
                Value::Object(
                    map.into_iter()
                        .map(|(k, v)| (k, expand_references(v, objects, visiting)))
                        .collect(),
                )
            }
        }
        Value::Array(values) => Value::Array(
            values
                .into_iter()
                .map(|v| expand_references(v, objects, visiting))
                .collect(),
        ),
        other => other,
    }
}

#[test]
fn nested_shared_objects_expand_to_complete_independent_values() {
    use serde_json::json;
    let objects = BTreeMap::from([
        (
            "spec".into(),
            json!({"policy": "complete policy", "revision": 7}),
        ),
        (
            "pending".into(),
            json!({"spec": {"$ref": "spec"}, "status": {"phase": "PENDING_APPLY"}}),
        ),
    ]);
    let input = json!([{"$ref": "pending"}, {"$ref": "pending"}]);
    let mut actual = expand(input.clone(), &objects);
    let expected = json!([
        {"spec": {"policy": "complete policy", "revision": 7}, "status": {"phase": "PENDING_APPLY"}},
        {"spec": {"policy": "complete policy", "revision": 7}, "status": {"phase": "PENDING_APPLY"}}
    ]);
    assert_eq!(actual, expected);
    actual[0]["spec"]["revision"] = json!(8);
    assert_eq!(actual[1], expected[1]);
    assert_eq!(expand(input, &objects), expected);
}

#[test]
#[should_panic(expected = "cyclic fixture reference")]
fn cyclic_references_are_rejected() {
    use serde_json::json;
    let objects = BTreeMap::from([
        ("a".into(), json!({"next": {"$ref": "b"}})),
        ("b".into(), json!({"next": {"$ref": "a"}})),
    ]);
    expand(json!({"$ref": "a"}), &objects);
}

#[test]
#[should_panic(expected = "missing fixture reference")]
fn missing_nested_references_are_rejected() {
    use serde_json::json;
    let objects = BTreeMap::from([("a".into(), json!({"spec": {"$ref": "missing"}}))]);
    expand(json!({"$ref": "a"}), &objects);
}

#[test]
#[should_panic(expected = "reference must not silently override fields")]
fn reference_overrides_are_rejected() {
    use serde_json::json;
    let objects = BTreeMap::from([("a".into(), json!({"status": {"phase": "PENDING_APPLY"}}))]);
    expand(json!({"$ref": "a", "status": {"phase": "READY"}}), &objects);
}

#[test]
#[should_panic(expected = "fixture reference must be a string")]
fn non_string_references_are_rejected() {
    use serde_json::json;
    expand(json!({"$ref": 7}), &BTreeMap::new());
}
