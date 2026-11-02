//! Renders unapproved candidates and re-verifies provenance, digests and artifacts at the client
//! trust boundary.

use asc_daemon_protocol::intent::{INTENT_RESULT_BYTES, IntentProposeResult};
use asc_intent::{
    Artifact, CANDIDATE_VERSION, DRAFT_VERSION, PROMPT_VERSION, prepare_policy,
    validate_policy_response,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::io::{self, Write as _};

use super::{Format, IntentFailure};

pub(super) fn validate(value: &Value, task: &str, ttl: u32) -> Result<(), IntentFailure> {
    let bad = || IntentFailure::Response;
    if serde_json::to_vec(value).map_err(|_| bad())?.len() > INTENT_RESULT_BYTES
        || value["format_version"] != CANDIDATE_VERSION
        || value["status"] != "CANDIDATE"
        || value["authorization_required"] != true
        || !fields_exact(
            value,
            &[
                "format_version",
                "status",
                "authorization_required",
                "task_id",
                "created_at",
                "expires_at",
                "artifact",
                "draft",
                "notes",
                "candidate_hash",
            ],
        )
    {
        return Err(bad());
    }
    check_draft(&value["draft"], task)?;
    let mut content = value.clone();
    content
        .as_object_mut()
        .ok_or_else(bad)?
        .remove("candidate_hash");
    if hash(&content)? != string(value, "candidate_hash")? {
        return Err(bad());
    }
    let created = value["created_at"].as_i64().ok_or_else(bad)?;
    if created <= 0 || created.checked_add(i64::from(ttl)) != Some(expiry(value)?) {
        return Err(bad());
    }
    let id = string(value, "task_id")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(bad());
    }
    let artifact: Artifact =
        serde_json::from_value(value["artifact"].clone()).map_err(|_| bad())?;
    // A digest is not a signature; only rebuilding the artifact reveals model output that was
    // swapped after its hash was recomputed.
    let expected = prepare_policy(string(&value["draft"], "policy")?).map_err(|_| bad())?;
    if artifact.language != expected.language
        || artifact.text != expected.text
        || artifact.schema_sha256 != expected.schema_sha256
        || value["notes"].as_array().is_none_or(|notes| {
            notes.is_empty() || notes.iter().any(|s| s.as_str().is_none_or(str::is_empty))
        })
    {
        return Err(bad());
    }
    Ok(())
}

fn fields_exact(value: &Value, names: &[&str]) -> bool {
    value.as_object().is_some_and(|fields| {
        fields.len() == names.len() && fields.keys().all(|key| names.contains(&key.as_str()))
    })
}

fn cedar_text(candidate: &Value) -> Result<&str, IntentFailure> {
    if candidate["artifact"]["language"] != "cedar" {
        return Err(IntentFailure::Response);
    }
    string(&candidate["artifact"], "text")
}

pub(super) fn expiry(candidate: &Value) -> Result<i64, IntentFailure> {
    candidate["expires_at"]
        .as_i64()
        .ok_or(IntentFailure::Response)
}

pub(super) fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, IntentFailure> {
    value[key].as_str().ok_or(IntentFailure::Response)
}

pub(super) fn render(
    result: &IntentProposeResult,
    task: &str,
    format: Format,
) -> Result<(), IntentFailure> {
    let mut out = io::stdout().lock();
    match (format, result) {
        (Format::Json, _) => {
            serde_json::to_writer(&mut out, result).map_err(|_| IntentFailure::Output)?;
            writeln!(out)?;
        }
        (Format::Cedar, IntentProposeResult::Candidate { candidate }) => {
            writeln!(
                out,
                "// CANDIDATE: not approved; not effective.\n// unresolved: {}\n{}",
                terminal_json(&candidate["draft"]["unresolved"])?,
                cedar_text(candidate)?
            )?;
        }
        (Format::Cedar, _) => writeln!(
            io::stderr(),
            "{}",
            escape(&serde_json::to_string(result).map_err(|_| IntentFailure::Response)?)
        )?,
        (Format::Text, IntentProposeResult::Candidate { candidate }) => {
            writeln!(out, "Candidate generated; not approved or effective.")?;
            understanding(&mut out, &candidate["draft"], task)?;
            writeln!(out, "\nValidated Cedar (not approved)")?;
            for line in cedar_text(candidate)?.lines() {
                writeln!(out, "{}", escape(line))?;
            }
            for note in candidate["notes"]
                .as_array()
                .ok_or(IntentFailure::Response)?
            {
                writeln!(
                    out,
                    "  {}",
                    escape(note.as_str().ok_or(IntentFailure::Response)?)
                )?;
            }
            writeln!(out, "Expires: {} (Unix seconds)\n", expiry(candidate)?)?;
        }
        (Format::Text, IntentProposeResult::NeedsClarification { draft, reasons }) => {
            writeln!(
                out,
                "Candidate not generated: the current model and schema cannot express this task."
            )?;
            understanding(&mut out, draft, task)?;
            writeln!(out, "\nReasons")?;
            for reason in reasons {
                writeln!(out, "  - {}", escape(reason))?;
            }
            writeln!(
                out,
                "\nNo Cedar is emitted, so nothing can be approved from this run."
            )?;
        }
    }
    out.flush()?;
    Ok(())
}

fn draft_of(result: &IntentProposeResult) -> &Value {
    match result {
        IntentProposeResult::Candidate { candidate } => &candidate["draft"],
        IntentProposeResult::NeedsClarification { draft, .. } => draft,
    }
}

pub(super) fn validate_draft(
    result: &IntentProposeResult,
    task: &str,
) -> Result<(), IntentFailure> {
    check_draft(draft_of(result), task)?;
    if let IntentProposeResult::NeedsClarification { reasons, .. } = result
        && (reasons.is_empty()
            || reasons.len() > 128
            || reasons
                .iter()
                .any(|r| r.trim().is_empty() || r.len() > 4096 || r.chars().any(char::is_control)))
    {
        return Err(IntentFailure::Response);
    }
    Ok(())
}

fn check_draft(draft: &Value, task: &str) -> Result<(), IntentFailure> {
    let source = &draft["source"];
    // The source digest proves only that the input is unchanged, not that the model understood
    // every requirement.
    if !fields_exact(
        draft,
        &[
            "format_version",
            "status",
            "prompt_version",
            "source",
            "policy",
            "unresolved",
        ],
    ) || !fields_exact(source, &["source_id", "kind", "byte_length", "sha256"])
        || validate_policy_response(
            &json!({"policy":draft["policy"], "unresolved":draft["unresolved"]}).to_string(),
        )
        .is_err()
        || draft["prompt_version"] != PROMPT_VERSION
        || draft["format_version"] != DRAFT_VERSION
        || draft["status"] != "DRAFT"
        || source["kind"] != "task_text"
        || source["sha256"] != digest(task.as_bytes())
        || source["byte_length"].as_u64() != Some(task.len() as u64)
        || source["source_id"]
            .as_str()
            .is_none_or(|s| s.is_empty() || s.len() > 256 || s.chars().any(char::is_control))
    {
        return Err(IntentFailure::Response);
    }
    Ok(())
}

fn understanding(out: &mut impl io::Write, draft: &Value, task: &str) -> Result<(), IntentFailure> {
    writeln!(
        out,
        "\nOriginal task\n  {}\n\nModel Cedar draft (unconfirmed)\n{}\n  unresolved: {}",
        escape(task),
        escape(string(draft, "policy")?),
        terminal_json(&draft["unresolved"])?
    )?;
    writeln!(
        out,
        "  Strict validation is not a task-completeness proof and does not grant authorization."
    )?;
    Ok(())
}

fn terminal_json(value: &Value) -> Result<String, IntentFailure> {
    Ok(serde_json::to_string(value)
        .map_err(|_| IntentFailure::Response)?
        .chars()
        .map(|ch| {
            if direction_control(ch) {
                format!("\\u{:04x}", u32::from(ch))
            } else {
                ch.to_string()
            }
        })
        .collect())
}
fn direction_control(ch: char) -> bool {
    matches!(ch, '\u{061c}' | '\u{200e}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}
pub(super) fn escape(text: &str) -> String {
    text.chars()
        .flat_map(|ch| {
            if ch.is_control() || direction_control(ch) {
                ch.escape_unicode().collect::<Vec<_>>()
            } else {
                vec![ch]
            }
        })
        .collect()
}
fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
fn hash(value: &Value) -> Result<String, IntentFailure> {
    fn sort(value: &mut Value) {
        match value {
            Value::Object(map) => {
                map.sort_keys();
                for value in map.values_mut() {
                    sort(value);
                }
            }
            Value::Array(values) => {
                for value in values {
                    sort(value);
                }
            }
            _ => {}
        }
    }
    let mut value = value.clone();
    sort(&mut value);
    Ok(digest(
        &serde_json::to_vec(&value).map_err(|_| IntentFailure::Response)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_checks_source_metadata_and_response() {
        let task = "Count names";
        let draft = json!({"format_version":DRAFT_VERSION, "status":"DRAFT", "prompt_version":PROMPT_VERSION,
            "source":{"source_id":"source", "kind":"task_text", "sha256":digest(task.as_bytes()), "byte_length":task.len()},
            "policy":"", "unresolved":["Listing is not modeled"]});
        let result = |draft| IntentProposeResult::NeedsClarification {
            draft,
            reasons: vec!["unsupported".into()],
        };
        assert!(validate_draft(&result(draft.clone()), task).is_ok());
        for (pointer, value) in [
            ("/format_version", json!("asc.intent.draft.v999")),
            ("/status", json!("APPROVED")),
            ("/source/sha256", json!("forged")),
            ("/source/byte_length", json!(0)),
            ("/source/source_id", Value::Null),
            ("/policy", Value::Null),
            ("/unresolved", json!([" "])),
        ] {
            let mut bad = draft.clone();
            *bad.pointer_mut(pointer).unwrap() = value;
            assert!(
                matches!(
                    validate_draft(&result(bad), task),
                    Err(IntentFailure::Response)
                ),
                "{pointer}"
            );
        }
    }

    #[test]
    fn terminal_sequences_and_bidi_are_visible_not_executed() {
        let value = escape("x\x1b[2J\n[approve]\u{202e}\u{2066}");
        assert!(!value.chars().any(char::is_control));
        assert!(!value.contains('\u{202e}'));
        assert!(value.contains("\\u{1b}"));
        let original = json!({"text":"x\u{202e}\u{2066}\u{1b}"});
        let encoded = terminal_json(&original).unwrap();
        assert!(!encoded.contains('\u{202e}'));
        assert_eq!(serde_json::from_str::<Value>(&encoded).unwrap(), original);
    }

    #[test]
    fn tampered_artifact_hash_and_versions_are_rejected() {
        let policy = "permit(principal, action == SecCore::Action::\"file.read\", resource is SecCore::File) when { resource.path == \"/work/report.csv\" };";
        let artifact = prepare_policy(policy).unwrap();
        let mut candidate = json!({"format_version":CANDIDATE_VERSION, "status":"CANDIDATE", "authorization_required":true,
            "draft":{"format_version":DRAFT_VERSION, "status":"DRAFT", "prompt_version":PROMPT_VERSION,
                "source":{"source_id":"source", "kind":"task_text", "sha256":digest(b"task"), "byte_length":4},
                "policy":policy, "unresolved":[]},
            "created_at":1, "expires_at":31, "task_id":"task", "artifact":artifact, "notes":["pending"]});
        candidate["candidate_hash"] = json!(hash(&candidate).unwrap());
        assert!(validate(&candidate, "task", 30).is_ok());
        for (field, value) in [
            ("format_version", json!("asc.intent.candidate.v2")),
            ("notes", json!([])),
            (
                "artifact",
                json!({"language":"cedar", "text":"", "schema_sha256":candidate["artifact"]["schema_sha256"]}),
            ),
            ("expires_at", json!(32)),
        ] {
            let mut bad = candidate.clone();
            bad[field] = value;
            bad.as_object_mut().unwrap().remove("candidate_hash");
            bad["candidate_hash"] = json!(hash(&bad).unwrap());
            assert!(validate(&bad, "task", 30).is_err(), "{field}");
        }
        // Recomputing the hash does not help: the client rebuilds the artifact from draft.policy.
        let mut swapped = candidate.clone();
        swapped["artifact"]["text"] = json!("tampered");
        swapped.as_object_mut().unwrap().remove("candidate_hash");
        swapped["candidate_hash"] = json!(hash(&swapped).unwrap());
        assert!(validate(&swapped, "task", 30).is_err());
    }
}
