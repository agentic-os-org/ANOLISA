//! V1 matching priorities over candidates already restricted to the observation owner.

use asc_security_events::{CorrelationCandidate, SecurityEvent, extract_verdict};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{QueryControl, QueryError, QueryObservation};

use super::ZERO_RUN_ID;

pub(super) fn correlate(
    record: &QueryObservation,
    candidates: &[CorrelationCandidate],
    control: &QueryControl,
) -> Result<Vec<Value>, QueryError> {
    if record.session_id.trim().is_empty() {
        return Ok(Vec::new());
    }
    let categories: &[&str] = match record.hook.as_str() {
        "before_tool_call" => &["code_scan", "skill_ledger", "pii_scan"],
        "before_agent_run" => &["prompt_scan", "pii_scan"],
        "after_tool_call" => &["pii_scan"],
        _ => return Ok(Vec::new()),
    };
    let real_run = !record.run_id.trim().is_empty() && record.run_id != ZERO_RUN_ID;
    let exact = real_run
        && record
            .tool_call_id
            .as_deref()
            .is_some_and(|v| !v.trim().is_empty());
    let run_match = real_run && record.hook == "before_agent_run";
    let reason = if exact {
        "tool_call_id"
    } else if run_match {
        "run_id"
    } else {
        "field+time"
    };
    let items = categories
        .iter()
        .filter_map(|category| {
            let selected = candidates
                .iter()
                .take_while(|_| !control.is_cancelled())
                .filter_map(|candidate| {
                    let event = &candidate.event;
                    if Some(event.uid) != record.uid
                        || event.category != *category
                        || event.session_id.as_deref() != Some(record.session_id.as_str())
                        || (real_run && event.run_id.as_deref() != Some(record.run_id.as_str()))
                        || (exact && event.tool_call_id != record.tool_call_id)
                    {
                        return None;
                    }
                    let delta = candidate.timestamp_epoch - record.timestamp_epoch;
                    let rank = if exact || run_match {
                        exact_rank(record, event)
                    } else if delta.abs() <= 10.0 {
                        field_rank(record, event)
                    } else {
                        None
                    }?;
                    Some((candidate, rank, delta))
                })
                .min_by(|(a, ar, ad), (b, br, bd)| {
                    ar.cmp(br)
                        .then_with(|| ad.abs().total_cmp(&bd.abs()))
                        .then_with(|| a.timestamp_epoch.total_cmp(&b.timestamp_epoch))
                        .then_with(|| a.event.event_id.cmp(&b.event.event_id))
                });
            selected.map(|(candidate, rank, delta)| {
                let mut event = json!(candidate.event);
                if let Some(verdict) = extract_verdict(&candidate.event.details) {
                    event["verdict"] = json!(verdict);
                }
                if candidate.event.category == "skill_ledger" {
                    project_skill(&mut event);
                }
                json!({"kind":"security", "uid":record.uid,
                "observability_event_id":record.id, "observability":record,
                "hook":record.hook, "session_id":record.session_id, "run_id":record.run_id,
                "call_id":record.call_id, "tool_call_id":record.tool_call_id,
                "timestamp":candidate.event.timestamp, "timestamp_epoch":candidate.timestamp_epoch,
                "event":event, "match":{"reason":reason, "rank":rank, "time_delta_seconds":delta}})
            })
        })
        .collect();
    control.check()?;
    Ok(items)
}

fn project_skill(event: &mut Value) {
    let result = &event["details"]["result"];
    let request = &event["details"]["request"];
    let command = [&result["command"], &request["command"]]
        .into_iter()
        .filter_map(Value::as_str)
        .find(|s| !s.is_empty())
        .map(str::to_owned);
    let name = if result["results"].is_array() {
        None
    } else {
        result["skill_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                request["skill_dir"]
                    .as_str()
                    .and_then(|path| std::path::Path::new(path).file_name())
                    .and_then(|name| name.to_str())
                    .map(str::to_owned)
            })
    };
    if let Some(command) = command {
        event["command"] = json!(command);
    }
    if let Some(name) = name {
        event["skill_name"] = json!(name);
    }
}

fn exact_rank(record: &QueryObservation, event: &SecurityEvent) -> Option<u8> {
    if event.category != "pii_scan" {
        return Some(0);
    }
    let expected = match record.hook.as_str() {
        "before_tool_call" => "tool_input",
        "after_tool_call" => "tool_output",
        _ => return Some(0),
    };
    let request = event.details.get("request")?.as_object()?;
    if let Some(source) = request
        .get("source")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    {
        return (source == expected).then_some(0);
    }
    hash_rank(record, event)
}

fn values<'a>(value: &'a Value, keys: &[&str]) -> Vec<&'a str> {
    keys.iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|v| !v.trim().is_empty())
        .collect()
}

fn observation_values(record: &QueryObservation, category: &str) -> Vec<String> {
    let values = if record.hook == "before_agent_run" {
        values(
            &record.metrics,
            &[
                "pii_scan_input_sha256",
                "prompt",
                "user_input",
                "text",
                "input",
            ],
        )
    } else if category == "pii_scan" {
        values(&record.metrics, &["pii_scan_input_sha256"])
    } else if record.hook == "before_tool_call" && category == "code_scan" {
        match &record.metrics["parameters"] {
            Value::String(s) if !s.trim().is_empty() => vec![s.as_str()],
            v => values(v, &["command", "cmd", "code", "script", "input"]),
        }
    } else {
        Vec::new()
    };
    values.into_iter().map(str::to_owned).collect()
}

fn hash_rank(record: &QueryObservation, event: &SecurityEvent) -> Option<u8> {
    let hash = event.details.get("request")?.get("text_sha256")?.as_str()?;
    observation_values(record, "pii_scan")
        .iter()
        .any(|value| {
            (value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                && value == hash)
                || format!("{:x}", Sha256::digest(value.as_bytes())) == hash
        })
        .then_some(0)
}

fn field_rank(record: &QueryObservation, event: &SecurityEvent) -> Option<u8> {
    if event.category == "skill_ledger" {
        return None;
    }
    if event.category == "pii_scan" {
        return hash_rank(record, event);
    }
    let request = event.details.get("request")?;
    let keys: &[&str] = match event.category.as_str() {
        "prompt_scan" => &["text", "prompt", "user_input", "input"],
        "code_scan" => &["code", "command", "cmd", "script"],
        _ => return None,
    };
    let event_values = values(request, keys);
    observation_values(record, &event.category)
        .iter()
        .flat_map(|a| event_values.iter().filter_map(move |b| string_rank(a, b)))
        .min()
}

fn string_rank(a: &str, b: &str) -> Option<u8> {
    let a = a.split_whitespace().collect::<Vec<_>>().join(" ");
    let b = b.split_whitespace().collect::<Vec<_>>().join(" ");
    if a.is_empty() || b.is_empty() {
        None
    } else if a == b {
        Some(0)
    } else if a.ends_with(&b) || b.ends_with(&a) {
        Some(1)
    } else if a.starts_with(&b) || b.starts_with(&a) {
        Some(2)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_defaults(defaults: &Value, fields: &Value) -> Value {
        let mut object = defaults.as_object().unwrap().clone();
        object.extend(fields.as_object().unwrap().clone());
        Value::Object(object)
    }

    /// QRY-008: executable query-contract coverage.
    #[test]
    fn frozen_v1_single_and_batch_matches_agree_with_core() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/query/v1-correlation.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let record: QueryObservation =
                serde_json::from_value(with_defaults(&fixture["record_defaults"], &case["record"]))
                    .unwrap();
            let mut candidates: Vec<CorrelationCandidate> = case["candidates"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| CorrelationCandidate {
                    event: serde_json::from_value(with_defaults(
                        &fixture["event_defaults"],
                        &row["event"],
                    ))
                    .unwrap(),
                    timestamp_epoch: row["timestamp_epoch"].as_f64().unwrap(),
                })
                .collect();
            // Even an incorrectly broad adapter result cannot cross ownership during matching.
            let mut foreign = candidates.clone();
            for candidate in &mut foreign {
                candidate.event.uid = 2000;
            }
            candidates.extend(foreign);
            let actual: Vec<Value> = correlate(&record,&candidates,&QueryControl::new(std::time::Instant::now()+std::time::Duration::from_secs(5), || false)).unwrap().iter().map(|item| json!({
                "event_id":item["event"]["event_id"], "reason":item["match"]["reason"],
                "rank":item["match"]["rank"], "time_delta_seconds":item["match"]["time_delta_seconds"]
            })).collect();
            assert_eq!(json!(actual), case["expected"], "{}", case["name"]);
        }
    }
}
