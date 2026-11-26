//! Versioned Hook Source capability contracts and 16-Hook capability cell projections.

use aw_contracts::{
    canonical,
    source::{
        event_supports_effect, event_supports_failure_action, is_hook_event,
        project_capability_cells, CapabilityCell, CapabilityStatus, FailureAction, HookAuthority,
        HookEffect, HookPointCapability, SourceCapability, SourceKind, HOOK_EVENTS, VERSION,
    },
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::process::Command;

fn document() -> Value {
    let fixtures: Value =
        canonical::parse(include_bytes!("fixtures/source-plan-vectors.json")).unwrap();
    fixtures["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "native-qwenpaw")
        .unwrap()["document"]
        .clone()
}

fn parse(value: &Value) -> Result<SourceCapability, aw_contracts::Error> {
    SourceCapability::parse(&serde_json::to_vec(value).unwrap())
}

#[test]
fn source_capability_round_trip_preserves_version_identity_and_hooks() {
    let value = document();
    let capability = parse(&value).unwrap();
    assert_eq!(capability.version(), VERSION);
    assert_eq!(capability.source_id(), "qwenpaw-native");
    assert_eq!(capability.kind(), SourceKind::Native);
    assert_eq!(capability.adapter(), "qwenpaw");
    assert_eq!(capability.contract(), "installed-native-hooks/v1");
    assert_eq!(capability.entrypoint(), "app");
    assert_eq!(capability.hooks().len(), 3);
    let before = capability.hook("tool.before").unwrap();
    assert_eq!(before.mode, HookAuthority::Control);
    assert!(before.supports_effect(HookEffect::ReplaceInput));
    assert!(before.supports_failure_action(FailureAction::Block));
    assert!(!before.supports_failure_action(FailureAction::WithholdResult));
    let after = capability.hook("tool.after").unwrap();
    assert!(after.supports_effect(HookEffect::ReplaceResult));
    assert!(after.supports_failure_action(FailureAction::WithholdResult));
    assert!(!after.supports_failure_action(FailureAction::Block));
    assert_eq!(serde_json::to_value(&capability).unwrap(), value);
}

#[test]
fn source_capability_rejects_unknown_fields_invalid_identifiers_and_duplicate_keys() {
    for (field, value) in [
        ("version", json!("aw-source-capability/v2")),
        ("source_id", json!("bad id")),
        ("source_id", json!("bad\nid")),
        ("kind", json!("ebpf")),
        ("adapter", json!("qwenpaw\n")),
        ("contract", json!(" ")),
        ("contract", json!("v1\n")),
        ("entrypoint", json!("\t")),
        ("entrypoint", json!("app\u{2028}")),
        ("unexpected", json!(true)),
    ] {
        let mut bad = document();
        bad[field] = value;
        assert!(parse(&bad).is_err(), "{field}");
    }
    for field in document().as_object().unwrap().keys() {
        let mut bad = document();
        bad.as_object_mut().unwrap().remove(field);
        assert!(parse(&bad).is_err(), "{field}");
    }
    let raw = serde_json::to_string(&document()).unwrap();
    let duplicate = raw.replacen('{', "{\"kind\":\"native\",", 1);
    assert!(SourceCapability::parse(duplicate.as_bytes()).is_err());
}

#[test]
fn sight_and_observe_sources_cannot_claim_control_authority_or_effects() {
    let mut sight = json!({
        "version": VERSION,
        "source_id": "sight-ebpf",
        "kind": "sight",
        "adapter": "qwenpaw",
        "contract": "agentsight/v1",
        "entrypoint": "ebpf",
        "hooks": {
            "tool.before": {
                "mode": "observe",
                "effects": ["observe"],
                "failure_actions": []
            }
        }
    });
    assert!(parse(&sight).is_ok());

    sight["hooks"]["tool.before"]["mode"] = json!("control");
    assert!(parse(&sight).is_err(), "Sight cannot claim control mode");

    let mut python_observe = json!({
        "version": VERSION,
        "source_id": "python-probe",
        "kind": "python",
        "adapter": "qwenpaw",
        "contract": "dowhen/v1",
        "entrypoint": "app",
        "hooks": {
            "tool.before": {
                "mode": "observe",
                "effects": ["observe", "block"],
                "failure_actions": []
            }
        }
    });
    assert!(
        parse(&python_observe).is_err(),
        "observe mode cannot declare block effect"
    );

    python_observe["hooks"]["tool.before"]["effects"] = json!(["observe"]);
    python_observe["hooks"]["tool.before"]["failure_actions"] = json!(["block"]);
    assert!(
        parse(&python_observe).is_err(),
        "observe mode cannot declare block failure action"
    );

    python_observe["hooks"]["tool.before"]["failure_actions"] = json!(["report"]);
    assert!(parse(&python_observe).is_ok());
}

#[test]
fn event_specific_effect_and_failure_action_boundaries_are_enforced() {
    assert_eq!(HOOK_EVENTS.len(), 16);
    for event in HOOK_EVENTS {
        assert!(is_hook_event(event));
        assert!(event_supports_effect(event, HookEffect::Observe));
        assert!(event_supports_failure_action(event, FailureAction::Report));
    }
    assert!(!is_hook_event("unknown.event"));

    for (event, effects, actions) in [
        ("tool.after", json!(["observe", "block"]), json!([])),
        ("tool.after", json!(["observe", "replace_input"]), json!([])),
        (
            "tool.before",
            json!(["observe", "replace_result"]),
            json!([]),
        ),
        (
            "tool.before",
            json!(["observe", "block"]),
            json!(["withhold_result"]),
        ),
        ("tool.before", json!(["block"]), json!([])),
        ("session.start", json!(["observe", "block"]), json!([])),
        ("session.end", json!(["observe", "block"]), json!([])),
    ] {
        let mut bad = document();
        bad["hooks"][event] = json!({
            "mode": "control",
            "effects": effects,
            "failure_actions": actions
        });
        assert!(parse(&bad).is_err(), "{event}");
    }
}

#[test]
fn capability_cell_projection_covers_all_16_semantic_hooks() {
    let vectors: Value =
        canonical::parse(include_bytes!("fixtures/source-plan-vectors.json")).unwrap();
    let mut sources = Vec::new();
    for entry in vectors["vectors"].as_array().unwrap() {
        if entry["kind"] == "source-capability-v1alpha1"
            && entry["document"]["adapter"] == "qwenpaw"
        {
            sources.push(SourceCapability::from_value(entry["document"].clone()).unwrap());
        }
    }
    let cells = project_capability_cells("qwenpaw", &sources).unwrap();
    assert_eq!(cells.len(), 16);
    for event in HOOK_EVENTS {
        assert!(cells.contains_key(event), "{event}");
        cells[event].validate().unwrap();
    }
    assert_eq!(
        cells["tool.before"].sources,
        vec![SourceKind::Native, SourceKind::Python, SourceKind::Sight]
    );
    assert_eq!(cells["tool.before"].mode, Some(HookAuthority::Control));
    assert_eq!(
        cells["model.before_request"].mode,
        Some(HookAuthority::Observe)
    );
    assert_eq!(cells["session.start"].status, CapabilityStatus::Unsupported);
    let cells_value = serde_json::to_value(&cells).unwrap();
    assert_eq!(cells_value, vectors["capability_cells"]["cells"]);
    assert_eq!(
        canonical::bytes(&cells_value).unwrap(),
        vectors["capability_cells"]["canonical"]
            .as_str()
            .unwrap()
            .as_bytes()
    );
    assert_eq!(
        canonical::document_digest(&cells_value).unwrap(),
        vectors["capability_cells"]["digest"]
    );

    let mut mismatched = CapabilityCell {
        adapter: "qwenpaw".into(),
        event: "session.start".into(),
        status: CapabilityStatus::Unsupported,
        sources: vec![SourceKind::Native],
        mode: None,
        effects: Vec::new(),
        failure_actions: Vec::new(),
    };
    assert!(mismatched.validate().is_err());
    mismatched.status = CapabilityStatus::Supported;
    mismatched.mode = Some(HookAuthority::Observe);
    mismatched.effects = vec![HookEffect::Observe];
    assert!(mismatched.validate().is_ok());

    let foreign = SourceCapability::new(
        "qoder-native",
        SourceKind::Native,
        "qoder",
        "native/v1",
        "cli",
        BTreeMap::from([(
            "tool.before".into(),
            HookPointCapability {
                post_permission: false,
                mode: HookAuthority::Control,
                effects: vec![HookEffect::Observe, HookEffect::Block],
                failure_actions: Vec::new(),
            },
        )]),
    )
    .unwrap();
    assert!(project_capability_cells("qwenpaw", &[foreign]).is_err());
}

#[test]
fn cross_language_vectors_match_in_rust_python_and_node() {
    let fixture_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/source-plan-vectors.json"
    );
    let vectors: Value =
        canonical::parse(include_bytes!("fixtures/source-plan-vectors.json")).unwrap();
    for entry in vectors["vectors"].as_array().unwrap() {
        let kind = entry["kind"].as_str().unwrap();
        let doc = &entry["document"];
        let encoded = canonical::bytes(doc).unwrap();
        assert_eq!(
            encoded,
            entry["canonical"].as_str().unwrap().as_bytes(),
            "{}",
            entry["name"]
        );
        assert_eq!(
            canonical::document_digest(doc).unwrap(),
            entry["digest"],
            "{}",
            entry["name"]
        );
        if kind == "source-capability-v1alpha1" {
            let parsed = SourceCapability::parse(&encoded).unwrap();
            assert_eq!(parsed.to_canonical_bytes().unwrap(), encoded);
        }
    }

    let schemas_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/schemas");
    let py_status = Command::new("python3")
        .args([
            "-c",
            r#"
import hashlib, json, sys
from pathlib import Path
from jsonschema import Draft202012Validator

data = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
schemas_dir = Path(sys.argv[2])
validators = {
    name: Draft202012Validator(json.loads((schemas_dir / f"{name}.schema.json").read_text(encoding="utf-8")))
    for name in ("source-capability-v1alpha1", "resolved-hook-plan-v1alpha1")
}
assert len(data["hook_events"]) == 16
for v in data["vectors"]:
    validators[v["kind"]].validate(v["document"])
    enc = json.dumps(v["document"], sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    assert enc.decode("utf-8") == v["canonical"], v["name"]
    assert hashlib.sha256(enc).hexdigest() == v["digest"], v["name"]
cells = data["capability_cells"]
c_enc = json.dumps(cells["cells"], sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
assert c_enc.decode("utf-8") == cells["canonical"]
assert hashlib.sha256(c_enc).hexdigest() == cells["digest"]
"#,
            fixture_path,
            schemas_dir,
        ])
        .status()
        .unwrap();
    assert!(py_status.success());

    let node_status = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            r#"
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
function encode(value) {
  if (Array.isArray(value)) return '[' + value.map(encode).join(',') + ']';
  if (value !== null && typeof value === 'object') {
    return '{' + Object.keys(value).sort().map(
      key => JSON.stringify(key) + ':' + encode(value[key])
    ).join(',') + '}';
  }
  return JSON.stringify(value);
}
const data = JSON.parse(readFileSync(process.argv[1], 'utf8'));
assert.equal(data.hook_events.length, 16);
for (const v of data.vectors) {
  const enc = encode(v.document);
  assert.equal(enc, v.canonical, v.name);
  assert.equal(createHash('sha256').update(enc, 'utf8').digest('hex'), v.digest, v.name);
}
const cEnc = encode(data.capability_cells.cells);
assert.equal(cEnc, data.capability_cells.canonical);
assert.equal(createHash('sha256').update(cEnc, 'utf8').digest('hex'), data.capability_cells.digest);
"#,
            fixture_path,
        ])
        .status()
        .unwrap();
    assert!(node_status.success());
}
