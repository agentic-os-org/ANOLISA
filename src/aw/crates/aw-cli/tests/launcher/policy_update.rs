//! The policy comparison command remains offline and preserves ordinary validation.

use super::support::Fixture;
use serde_json::{json, Value};
use std::{fs, process::Output};

fn validate(fixture: &Fixture, candidate: &Value, against: bool) -> Output {
    validate_bytes(fixture, &serde_json::to_vec(candidate).unwrap(), against)
}

fn validate_bytes(fixture: &Fixture, candidate: &[u8], against: bool) -> Output {
    let next = fixture.root.join("next.json");
    fs::write(&next, candidate).unwrap();
    let mut command = fixture.command();
    command.args(["validate", "--config"]).arg(&next);
    if against {
        command.arg("--against").arg(fixture.root.join("aw.json"));
    }
    let output = fixture.run(command);
    assert_eq!(fs::read(next).unwrap(), candidate);
    assert!(!fixture.root.join("state").exists());
    assert!(!fixture.root.join("trace.jsonl").exists());
    output
}

#[test]
fn policy_scope_validation_accepts_equivalent_observation_integer_spellings() {
    let fixture = Fixture::new();
    let mut baseline = fixture.document();
    baseline["spec"]["agents"]["qoder"]["evidence"] = json!({
        "source_id":"sight","endpoint":"http://127.0.0.1:12345",
        "query_timeout_ms":1000,"query_bytes":4096,"max_items":8,"max_bytes":8192});
    fixture.save(&baseline);
    let original = fs::read(fixture.root.join("aw.json")).unwrap();
    let candidate = serde_json::to_string(&baseline)
        .unwrap()
        .replace("\"query_timeout_ms\":1000", "\"query_timeout_ms\":1e3")
        .replace("\"query_bytes\":4096", "\"query_bytes\":4096.0")
        .replace("\"max_items\":8", "\"max_items\":8.0")
        .replace("\"max_bytes\":8192", "\"max_bytes\":8.192e3");
    let output = validate_bytes(&fixture, candidate.as_bytes(), true);
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        output.stdout,
        b"policy update scope valid; runtime activation is not checked\n"
    );
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(fixture.root.join("aw.json")).unwrap(), original);
}

#[test]
fn ordinary_validation_keeps_its_output_and_does_not_require_a_baseline() {
    let fixture = Fixture::new();
    let output = validate(&fixture, &fixture.document(), false);
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(output.stdout, b"configuration valid\n");
    assert!(output.stderr.is_empty());
    assert!(!fixture.root.join("aw.json").exists());
}

#[test]
fn policy_scope_validation_does_not_launch_agents_or_providers_or_modify_files() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    fixture.save(&baseline);
    let original = fs::read(fixture.root.join("aw.json")).unwrap();
    let mut candidate = baseline;
    fixture.provider(&mut candidate);
    candidate["spec"]["providers"]["policy"]["transport"]["argv"] =
        json!(["/not-installed/provider"]);
    candidate["spec"]["providers"]["policy"]["config"] = json!({"secret":"never-print-this"});
    let output = validate(&fixture, &candidate, true);
    assert!(output.status.success(), "{:?}", output);
    assert_eq!(
        output.stdout,
        b"policy update scope valid; runtime activation is not checked\n"
    );
    assert!(output.stderr.is_empty());
    assert_eq!(fs::read(fixture.root.join("aw.json")).unwrap(), original);
    assert!(!fixture.root.join("environment-bytes-agent.json").exists());
}

#[test]
fn policy_scope_validation_rejects_launch_edits_without_printing_values() {
    let fixture = Fixture::new();
    let baseline = fixture.document();
    fixture.save(&baseline);
    let mut candidate = baseline;
    candidate["spec"]["agents"]["qoder"]["argv"] = json!(["/private/never-print-this"]);
    let output = validate(&fixture, &candidate, true);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(error.contains("/spec/agents"), "{error}");
    assert!(!error.contains("never-print-this"));
}

#[test]
fn baseline_and_candidate_both_require_complete_configuration_validation() {
    let fixture = Fixture::new();
    let mut candidate = fixture.document();
    fs::write(fixture.root.join("aw.json"), b"spec: {}\nspec: {}\n").unwrap();
    let output = validate(&fixture, &candidate, true);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate mapping key"));

    fixture.save(&candidate);
    candidate["spec"]["events"]["tool.before"] = json!({"enabled":true,"steps":[
        {"id":"missing","provider":"unknown","operation":"check",
         "effects":["observe"],"on_error":"report"}]});
    let output = validate(&fixture, &candidate, true);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn baseline_is_bounded_and_missing_paths_are_errors() {
    let fixture = Fixture::new();
    let candidate = fixture.document();
    assert_eq!(validate(&fixture, &candidate, true).status.code(), Some(1));
    fs::write(
        fixture.root.join("aw.json"),
        vec![b' '; aw_config::MAX_DOCUMENT_BYTES + 1],
    )
    .unwrap();
    let output = validate(&fixture, &candidate, true);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("limit"));
}

#[test]
fn comparison_errors_identify_candidate_and_baseline_inputs() {
    let fixture = Fixture::new();
    let document = fixture.document();
    for invalid in [
        b"spec: {}\nspec: {}\n".to_vec(),
        b"{}".to_vec(),
        vec![b' '; aw_config::MAX_DOCUMENT_BYTES + 1],
    ] {
        fixture.save(&document);
        let plain = validate_bytes(&fixture, &invalid, false);
        assert_eq!(plain.status.code(), Some(1));
        assert!(plain.stdout.is_empty());
        assert!(!plain.stderr.starts_with(b"aw: candidate:"));
        assert!(!plain.stderr.starts_with(b"aw: baseline:"));

        let candidate = validate_bytes(&fixture, &invalid, true);
        assert_eq!(candidate.status.code(), Some(1));
        assert!(candidate.stdout.is_empty());
        assert!(candidate.stderr.starts_with(b"aw: candidate:"));

        fs::write(fixture.root.join("aw.json"), &invalid).unwrap();
        let baseline = validate(&fixture, &document, true);
        assert_eq!(baseline.status.code(), Some(1));
        assert!(baseline.stdout.is_empty());
        assert!(baseline.stderr.starts_with(b"aw: baseline:"));
    }

    fixture.save(&document);
    let missing = fixture.root.join("missing.json");
    let mut command = fixture.command();
    command.args(["validate", "--config"]).arg(&missing);
    let plain = fixture.run(command);
    assert_eq!(plain.status.code(), Some(1));

    let mut command = fixture.command();
    command.args(["validate", "--config"]).arg(&missing);
    command.arg("--against").arg(fixture.root.join("aw.json"));
    let candidate = fixture.run(command);
    assert_eq!(candidate.status.code(), Some(1));
    let original = plain.stderr.strip_prefix(b"aw: ").unwrap();
    assert_eq!(candidate.stderr, [b"aw: candidate: ", original].concat());

    fs::remove_file(fixture.root.join("aw.json")).unwrap();
    let baseline = validate(&fixture, &document, true);
    assert_eq!(baseline.status.code(), Some(1));
    assert_eq!(baseline.stderr, [b"aw: baseline: ", original].concat());
}
