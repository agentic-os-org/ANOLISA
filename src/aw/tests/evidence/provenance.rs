use super::*;

fn context(kind: &str, status: &str) -> Value {
    let mut value = fixtures()["evidence-context-v1"].clone();
    let evidence = &mut value["evidence"][0];
    evidence["kind"] = json!(kind);
    evidence["execution_ref"] = Value::Null;
    evidence["association"]["status"] = json!(status);
    evidence["association"]["basis"] = json!(if status == "verified" {
        "native_callback"
    } else {
        "none"
    });
    evidence["trust"] = json!(if status == "verified" {
        "trusted_adapter"
    } else {
        "unverified_native"
    });
    if status == "unassociated" {
        evidence["association"]["session_id"] = Value::Null;
        evidence["association"]["tool_call_id"] = Value::Null;
    }
    value
}

fn check(mut value: Value, accepted: bool) {
    let evidence = &mut value["evidence"][0];
    evidence["evidence_id"] =
        json!(
            canonical::document_digest(&json!([evidence["source"], evidence["content_ref"]]))
                .unwrap()
        );
    let execution = evidence["execution_ref"].as_str().map(str::to_owned);
    assert_eq!(
        REGISTRY.validate("evidence-v1", evidence).is_ok(),
        accepted,
        "record provenance: {evidence}"
    );
    value["execution_refs"] = json!(execution.into_iter().collect::<Vec<_>>());
    // Recompute every affected digest so a rejection must enforce provenance,
    // rather than merely detecting metadata changed since the fixture was hashed.
    resign(&mut value);
    assert_eq!(
        validate_context(
            &REGISTRY,
            &value,
            &value["subject"],
            value["candidate_revision"].as_str().unwrap(),
        )
        .is_ok(),
        accepted,
        "context provenance: {}",
        value["evidence"][0]
    );
}

#[test]
fn native_provenance_is_checked_without_execution_assertions() {
    for kind in ["native.callback", "native.tool_result"] {
        for status in ["verified", "unverified", "unassociated"] {
            let valid = context(kind, status);
            check(valid.clone(), true);
            for (pointer, replacement) in [
                ("/source/component", json!("agentsight")),
                ("/derivation", json!("source_history")),
                ("/derivation", json!("model_echo")),
                ("/trust", json!("source_reported")),
                (
                    "/trust",
                    json!(if status == "verified" {
                        "unverified_native"
                    } else {
                        "trusted_adapter"
                    }),
                ),
                ("/association/basis", json!("native_session")),
            ] {
                let mut invalid = valid.clone();
                *invalid["evidence"][0].pointer_mut(pointer).unwrap() = replacement;
                check(invalid, false);
            }
            if status == "verified" {
                let mut invalid = valid.clone();
                invalid["evidence"][0]["association"]["tool_call_id"] = Value::Null;
                check(invalid, false);
            }
        }
    }
}

#[test]
fn source_history_cannot_claim_native_provenance_or_call_association() {
    for derivation in ["source_history", "model_echo"] {
        for status in ["verified", "unverified", "unassociated"] {
            let mut valid = context("trajectory.step", status);
            let evidence = &mut valid["evidence"][0];
            evidence["source"] = json!({
                "component":"agentsight", "scope":"4".repeat(64),
                "instance_id":null, "version":null,
                "record_ref":"trajectory:session-1#/steps/0",
                "revision":evidence["source"]["revision"],
            });
            evidence["trust"] = json!("source_reported");
            evidence["derivation"] = json!(derivation);
            evidence["association"]["basis"] = json!(if status == "verified" {
                "native_session"
            } else {
                "none"
            });
            evidence["association"]["tool_call_id"] = Value::Null;
            check(valid.clone(), true);
            for (pointer, replacement) in [
                ("/source/component", json!("aw")),
                ("/trust", json!("trusted_adapter")),
                ("/trust", json!("unverified_native")),
                ("/derivation", json!("native_report")),
                ("/association/basis", json!("native_callback")),
                ("/association/tool_call_id", json!("call-1")),
                (
                    "/execution_ref",
                    fixtures()["evidence-v1"]["execution_ref"].clone(),
                ),
            ] {
                let mut invalid = valid.clone();
                *invalid["evidence"][0].pointer_mut(pointer).unwrap() = replacement;
                check(invalid, false);
            }
        }
    }
}
