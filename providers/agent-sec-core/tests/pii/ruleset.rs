//! Ruleset identity across tool-output scans.

use super::*;

#[test]
fn nested_mask_requires_the_same_ruleset_for_both_risky_json_reports() {
    let result = json!({"stdout": SLOT, "metadata": "kept"});
    let raw = result.to_string();
    let mut req = mask_request(SLOT);
    req["event"]["tool"]["result"] = result;
    for (first, second) in [
        (Some("a"), Some("a")),
        (Some("a"), Some("b")),
        (Some("a"), None),
        (None, Some("a")),
    ] {
        let mut report = json!({"ok":true,"verdict":"deny","summary":{
            "source":"tool_output","truncated":false,"bytes_scanned":raw.len()},
            "redacted_text":raw.replace("SENSITIVE","[MASKED]")});
        if let Some(id) = second {
            report["summary"]["ruleset_id"] = json!(id);
        }
        let mut case = json!({"pii":true, "redactions_by_text":{&raw:{"stdout":report}}});
        if let Some(id) = first {
            case["ruleset_id"] = json!(id);
        } else {
            case["v1_ruleset_sha256"] = json!("a".repeat(64));
        }
        let d = support::Directory::new(case);
        let response = run(req.clone(), &d);
        if first == second {
            assert_eq!(
                response["effects"],
                json!([{"type":"replace_result",
                "reason_code":"pii_risk","text":"contact [MASKED] for delivery"}])
            );
        } else {
            let error = if first.is_none() || second.is_none() {
                "missing_ruleset_id"
            } else {
                "ruleset_changed"
            };
            assert_eq!(response["error_code"], error);
            assert!(response.get("effects").is_none());
        }
    }
}

#[test]
fn mask_requires_ruleset_id_even_when_v1_reports_a_custom_rules_hash() {
    let first_hash = "a".repeat(64);
    for (slot, redaction_hash) in [
        ("public", first_hash.clone()),
        (SLOT, first_hash.clone()),
        (SLOT, "b".repeat(64)),
    ] {
        let d = support::Directory::new(json!({
            "pii": true, "v1_ruleset_sha256": first_hash, "v1_redaction_sha256": redaction_hash
        }));
        let response = run(mask_request(slot), &d);
        assert_eq!(response["status"], "error");
        assert_eq!(response["error_code"], "missing_ruleset_id");
        assert!(response.get("effects").is_none());
        assert_eq!(scans(&d).len(), 1);
    }
}

#[test]
fn ruleset_id_must_be_a_nonempty_string_when_present() {
    for id in [json!(""), json!(1), Value::Null] {
        let d = support::Directory::new(json!({"pii":true,"ruleset_id":id}));
        let response = run(mask_request(SLOT), &d);
        assert_eq!(response["error_code"], "invalid_scan_result");
        assert!(response.get("effects").is_none());
    }
}
