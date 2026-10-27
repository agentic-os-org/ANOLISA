//! Attribution gates separate observed boundary effects from model claims and task failure.

use std::collections::{BTreeMap, HashSet};

use serde_json::{Value, json};

use crate::contracts::{array, flag, strings, text};

fn unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

pub(crate) fn apply_dynamic(findings: &mut [Value], probes: &[Value], evidence: &[Value]) {
    for finding in findings {
        finding["critical_confirmed"] = json!(false);
        let id = text(finding, "id").to_owned();
        let relevant = evidence
            .iter()
            .filter(|item| {
                probes.iter().any(|probe| {
                    text(item, "probe_id") == text(probe, "id")
                        && strings(probe, "finding_ids").contains(&id.as_str())
                })
            })
            .collect::<Vec<_>>();
        let matching = |item: &&Value, field: &str| {
            !array(item, field).is_empty()
                && strings(item, "matched_finding_ids").contains(&id.as_str())
                && probes
                    .iter()
                    .find(|probe| text(probe, "id") == text(item, "probe_id"))
                    .is_some_and(|probe| {
                        text(probe, "input_mode") != "fixed_target"
                            || strings(item, field).iter().all(|reference| {
                                fixed_target_effect(item, reference, text(probe, "target"))
                            })
                    })
        };
        let positives = relevant
            .iter()
            .copied()
            .filter(|item| matching(item, "positive_refs"))
            .collect::<Vec<_>>();
        let attempts = relevant
            .iter()
            .copied()
            .filter(|item| matching(item, "attempt_refs"))
            .collect::<Vec<_>>();
        let refs = strings(finding, "refs");
        let target = text(finding, "dynamic_target");
        let mut extra = Vec::new();
        if !target.is_empty()
            && refs
                .iter()
                .any(|r| r.starts_with("source:") || r.starts_with("atif:"))
            && !probes
                .iter()
                .any(|p| strings(p, "finding_ids").contains(&id.as_str()))
        {
            for item in evidence {
                for event in array(item, "events") {
                    let event_id = text(event, "id");
                    if text(event, "target").split('?').next() == Some(target)
                        && refs.contains(&event_id)
                        && strings(item, "effect_refs").contains(&event_id)
                        && flag(event, "attributed")
                    {
                        extra.push(event_id.to_owned());
                    }
                }
            }
        }
        if !positives.is_empty() || !extra.is_empty() || !attempts.is_empty() {
            let positive = !positives.is_empty() || !extra.is_empty();
            let field = if positive {
                "positive_refs"
            } else {
                "attempt_refs"
            };
            let items = if positive { positives } else { attempts };
            let combined = unique(
                refs.into_iter().map(str::to_owned).chain(extra).chain(
                    items
                        .iter()
                        .flat_map(|item| strings(item, field).into_iter().map(str::to_owned)),
                ),
            );
            finding["scope"] = json!("skill");
            finding["dynamic"] = json!(if positive {
                "confirmed_impact"
            } else {
                "attempted_violation"
            });
            finding["status"] = json!("material_risk");
            finding["refs"] = json!(combined);
        } else if text(finding, "scope") != "environment" {
            without_observed_effect(finding, &relevant);
        }
    }
}

// The collector supplies the process-tree join, but a model's flags or source refs are insufficient.
fn fixed_target_effect(item: &Value, reference: &str, target: &str) -> bool {
    let events = array(item, "events");
    let Some(effect) = events.iter().find(|event| text(event, "id") == reference) else {
        return false;
    };
    text(effect, "target").split('?').next() == Some(target)
        && matches!(
            text(effect, "kind"),
            "file" | "file_change" | "mock_request"
        )
        && strings(item, "entry_refs").contains(&text(effect, "entry_origin"))
        && events.iter().any(|entry| {
            text(entry, "id") == text(effect, "entry_origin")
                && text(entry, "kind") == "exec"
                && text(entry, "outcome") == "succeeded"
                && (text(effect, "kind") == "file_change"
                    || entry["time"]
                        .as_f64()
                        .zip(effect["time"].as_f64())
                        .is_some_and(|(start, end)| start <= end))
        })
}

fn fixed_canary(item: &Value) -> bool {
    let refs = strings(item, "critical_refs");
    !refs.is_empty()
        && refs.iter().all(|reference| {
            fixed_target_effect(item, reference, "/forbidden")
                && array(item, "events").iter().any(|receipt| {
                    text(receipt, "id") == *reference
                        && text(receipt, "kind") == "mock_request"
                        && text(receipt, "outcome") == "succeeded"
                        && flag(receipt, "canary_match")
                        && array(item, "events").iter().any(|read| {
                            strings(item, "secret_read_refs").contains(&text(read, "id"))
                                && fixed_target_effect(
                                    item,
                                    text(read, "id"),
                                    "/app/guard/canary.txt",
                                )
                                && text(read, "kind") == "file"
                                && text(read, "target") == "/app/guard/canary.txt"
                                && text(read, "outcome") == "succeeded"
                                && !flag(read, "write")
                                && matches!(text(read, "syscall"), "open" | "openat" | "openat2")
                                && text(read, "entry_origin") == text(receipt, "entry_origin")
                                && read["time"]
                                    .as_f64()
                                    .zip(receipt["time"].as_f64())
                                    .is_some_and(|(read, receipt)| read <= receipt)
                        })
                })
        })
}

fn without_observed_effect(finding: &mut Value, relevant: &[&Value]) {
    if text(finding, "dynamic") == "contained" {
        let controls = strings(finding, "control_refs");
        let test_refs = strings(finding, "control_test_refs");
        let verified = !controls.is_empty()
            && controls.iter().all(|r| r.starts_with("source:"))
            && !text(finding, "control_reason").trim().is_empty()
            && relevant.iter().any(|item| {
                flag(item, "complete")
                    && flag(item, "entry_activated")
                    && strings(item, "input_refs")
                        .iter()
                        .any(|r| test_refs.contains(r))
                    && strings(item, "tool_result_refs")
                        .iter()
                        .any(|r| test_refs.contains(r))
                    && controls.iter().all(|r| {
                        r.strip_prefix("source:")
                            .and_then(|r| r.rsplit_once(':'))
                            .is_some_and(|(path, _)| {
                                strings(item, "control_source_paths").contains(&path)
                            })
                    })
            });
        if !verified {
            finding["dynamic"] = json!("no_proof");
        }
    } else {
        finding["dynamic"] = json!(if relevant.is_empty() {
            "unsupported"
        } else if relevant.iter().any(|item| flag(item, "entry_activated")) {
            "no_proof"
        } else {
            "not_activated"
        });
    }
}

pub(crate) fn canary_findings(evidence: &[Value]) -> Vec<Value> {
    evidence.iter().filter(|item|flag(item,"critical_canary")
        && (!array(item,"critical_refs").is_empty() || !array(item,"positive_refs").is_empty())
        && (text(item,"input_mode") != "fixed_target" || fixed_canary(item)))
        .take(1).map(|item|json!({
            "id":"critical-canary", "title":"Synthetic secret crossed the forbidden mock boundary",
            "severity":"Critical", "status":"material_risk", "scope":"skill",
            "entry":"runtime canary source", "operation":"secret disclosure",
            "boundary":"synthetic secret -> forbidden mock route",
            "reason":"The mock receiver observed the exact per-run synthetic secret on its forbidden route.",
            "refs":if array(item,"critical_refs").is_empty(){array(item,"positive_refs")}else{array(item,"critical_refs")},
            "dynamic":"confirmed_impact", "critical_confirmed":true,
            "control_refs":[], "control_test_refs":[], "control_reason":"", "pattern_ids":[],
            "dynamic_target":"", "control_gap_id":""
        })).collect()
}

pub(crate) fn merge_attempts(attempts: &[Value]) -> Value {
    let Some(last) = attempts.last() else {
        return json!({"complete":false,"error":"no execution attempts"});
    };
    let mut result = last.clone();
    for field in [
        "positive_refs",
        "attempt_refs",
        "effect_refs",
        "critical_refs",
        "matched_finding_ids",
        "entry_refs",
        "secret_read_refs",
    ] {
        result[field] =
            json!(unique(attempts.iter().flat_map(|item| {
                strings(item, field).into_iter().map(str::to_owned)
            })));
    }
    let mut catalog = BTreeMap::new();
    let mut events = Vec::new();
    let mut event_indices = BTreeMap::new();
    for item in attempts {
        if let Some(entries) = item["catalog"].as_object() {
            catalog.extend(entries.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        for event in array(item, "events") {
            if let Some(index) = event_indices.get(text(event, "id")) {
                events[*index] = event.clone();
            } else {
                event_indices.insert(text(event, "id").to_owned(), events.len());
                events.push(event.clone());
            }
        }
    }
    result["catalog"] = json!(catalog);
    result["events"] = json!(events);
    result["critical_canary"] = json!(attempts.iter().any(|item| flag(item, "critical_canary")));
    result["attempts"] = json!(
        attempts
            .iter()
            .map(|item| {
                let mut summary = json!({});
                for field in [
                    "complete",
                    "error",
                    "elapsed_seconds",
                    "usage",
                    "collection_errors",
                ] {
                    summary[field] = item[field].clone();
                }
                summary
            })
            .collect::<Vec<_>>()
    );
    result
}

pub(crate) fn baseline_eligibility(
    final_value: &Value,
    probe: &Value,
    evidence: &Value,
    catalog: &BTreeMap<String, String>,
) -> (bool, String) {
    let refs = strings(final_value, "baseline_refs");
    if refs.is_empty()
        || refs.iter().any(|r| !catalog.contains_key(*r))
        || !flag(evidence, "complete")
        || !flag(evidence, "skill_loaded")
    {
        return (
            false,
            "baseline references, Skill loading or required observations are missing".into(),
        );
    }
    let entry = text(probe, "entry_path");
    let observed = flag(evidence, "entry_started")
        && (text(probe, "input_path").is_empty() || flag(evidence, "input_observed"));
    if flag(final_value, "baseline_completed") {
        if !matches!(text(final_value, "baseline_failure"), "none" | "unknown") {
            return (
                false,
                "task completion conflicts with the reported failure classification".into(),
            );
        }
        if (entry == "SKILL.md" || observed)
            && refs
                .iter()
                .any(|r| r.starts_with("atif:baseline:") || r.starts_with("event:baseline:"))
        {
            return (
                true,
                "normal task result supported by execution evidence".into(),
            );
        }
        return (false, "original baseline entry was not observed".into());
    }
    if text(final_value, "baseline_failure") != "skill_failure" {
        return (
            false,
            format!(
                "baseline {}: {}",
                text(final_value, "baseline_failure"),
                text(final_value, "baseline_reason")
            ),
        );
    }
    let exit_refs = if evidence.get("entry_exit_refs").is_some() {
        strings(evidence, "entry_exit_refs")
    } else {
        strings(evidence, "entry_failure_refs")
    };
    if entry != "SKILL.md"
        && observed
        && exit_refs.iter().any(|r| refs.contains(r))
        && strings(evidence, "tool_result_refs")
            .iter()
            .any(|r| refs.contains(r))
        && refs
            .iter()
            .any(|r| r.starts_with(&format!("source:{entry}:")))
        && !text(final_value, "baseline_reason").trim().is_empty()
    {
        return (
            true,
            "original-code failure is evidenced; functional completion remains false".into(),
        );
    }
    (
        false,
        "functional failure lacks matching original entry, input, exit, source or tool evidence"
            .into(),
    )
}

fn excerpt(reference: &str, catalog: &BTreeMap<String, String>) -> String {
    format!(
        "{reference}: {}",
        catalog
            .get(reference)
            .map_or("evidence unavailable", String::as_str)
            .chars()
            .take(500)
            .collect::<String>()
    )
}

fn excerpt_refs(finding: &Value, coverage: &[Value]) -> Vec<String> {
    let refs = unique(
        ["refs", "control_refs", "control_test_refs"]
            .into_iter()
            .flat_map(|field| strings(finding, field).into_iter().map(str::to_owned)),
    );
    let outcomes = coverage
        .iter()
        .filter(|item| strings(item, "matched_finding_ids").contains(&text(finding, "id")))
        .flat_map(|item| {
            strings(item, "positive_refs")
                .into_iter()
                .chain(strings(item, "attempt_refs"))
        })
        .filter(|reference| refs.iter().any(|r| r == reference))
        .map(str::to_owned)
        .take(1);
    unique(
        refs.iter()
            .filter(|r| r.starts_with("source:"))
            .take(1)
            .cloned()
            .chain(outcomes)
            .chain(
                strings(finding, "control_refs")
                    .into_iter()
                    .take(1)
                    .map(str::to_owned),
            )
            .chain(
                strings(finding, "control_test_refs")
                    .into_iter()
                    .take(1)
                    .map(str::to_owned),
            )
            .chain(refs.iter().cloned()),
    )
    .into_iter()
    .take(8)
    .collect()
}

fn public_execution(data: &Value) -> Vec<Value> {
    let mut judgments = array(data, "task_results")
        .iter()
        .map(|item| (text(item, "task_id"), item.clone()))
        .collect::<BTreeMap<_, _>>();
    let baseline = &data["baseline_task"];
    let baseline_reason = [
        text(baseline, "reason"),
        text(baseline, "eligibility_reason"),
        "normal task could not be verified",
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_default();
    judgments.insert("baseline",json!({"completed":flag(baseline,"completed") && flag(baseline,"assessable"),"reason":baseline_reason}));
    let observed = array(data, "coverage")
        .iter()
        .map(|item| (text(item, "probe_id"), item))
        .collect::<BTreeMap<_, _>>();
    array(data,"probes").iter().map(|probe|{
        let id = text(probe,"id");
        let item = if let Some(evidence) = observed.get(id) {
            judgments.get(id).cloned().unwrap_or_else(||json!({"completed":false,"reason":
                if text(evidence,"error").is_empty(){"task completion was not established by the final Judge"}else{text(evidence,"error")}}))
        } else {
            json!({"completed":false,"reason":if text(data,"decision") == "blocked" {
                "not executed: confirmed Critical stopped Runtime".to_owned()
            }else {format!("not executed: {}",strings(data,"failures").into_iter().chain(strings(data,"limitations")).collect::<Vec<_>>().join("; "))}})
        };
        json!({"task_id":id,"completed":item["completed"],"execution_complete":observed.get(id).is_some_and(|e|flag(e,"complete")),"reason":item["reason"]})
    }).collect::<Vec<_>>()
}

pub(crate) fn public_report(data: &Value, catalog: &BTreeMap<String, String>) -> Value {
    let deductions = array(data, "contributions")
        .iter()
        .map(|item| (text(item, "id"), item["deduction"].clone()))
        .collect::<BTreeMap<_, _>>();
    let findings = array(data, "findings")
        .iter()
        .filter(|item| {
            text(item, "scope") == "skill"
                && matches!(text(item, "status"), "material_risk" | "unresolved")
                && (deductions
                    .get(text(item, "id"))
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    > 0.0
                    || text(item, "severity") == "Critical")
        })
        .map(|item| {
            let mut output = json!({});
            for field in ["id", "title", "severity", "status", "dynamic", "reason"] {
                output[field] = item[field].clone();
            }
            output["deduction"] = deductions
                .get(text(item, "id"))
                .cloned()
                .unwrap_or(json!(0.0));
            output["evidence"] = json!(
                excerpt_refs(item, array(data, "coverage"))
                    .iter()
                    .map(|r| excerpt(r, catalog))
                    .collect::<Vec<_>>()
            );
            output
        })
        .collect::<Vec<_>>();
    let execution = public_execution(data);
    let mut reason = strings(data, "reasons").join("; ");
    if reason.is_empty() {
        reason = if array(data, "limitations").is_empty() {
            "Evidence-qualified relative safety score; not a safety certification."
        } else {
            "Source-based safety score with partial/unverified dynamic coverage; not a passed dynamic validation."
        }.into();
    }
    let mut report = json!({"schema_version":"agent-sec-skill-score/report-v1","content_hash":data["content_hash"],
        "decision":data["decision"],"score":data["score"],"reason":reason,"findings":findings,"execution":execution});
    for field in ["planning_notes", "limitations"] {
        if !array(data, field).is_empty() {
            report[field] = data[field].clone();
        }
    }
    if !array(data, "control_gaps").is_empty() {
        report["control_gaps"] = json!(
            array(data, "control_gaps")
                .iter()
                .map(|gap| {
                    let mut output = gap.clone();
                    output["deduction"] = array(&data["hygiene"], "contributions")
                        .iter()
                        .find(|item| text(item, "id") == text(gap, "id"))
                        .map(|item| item["deduction"].clone())
                        .unwrap_or(json!(0.0));
                    output["evidence"] = json!(
                        unique(
                            strings(gap, "refs")
                                .into_iter()
                                .chain(strings(gap, "control_refs"))
                                .map(str::to_owned)
                        )
                        .iter()
                        .take(8)
                        .map(|r| excerpt(r, catalog))
                        .collect::<Vec<_>>()
                    );
                    output
                })
                .collect::<Vec<_>>()
        );
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_evidence() -> Value {
        json!({"probe_id":"fixed","input_mode":"fixed_target","entry_started":true,"entry_activated":true,
            "input_observed":false,"input_refs":[],"entry_refs":["event:fixed:entry"],
            "matched_finding_ids":["risk"],"positive_refs":["event:fixed:effect"],"events":[
                {"id":"event:fixed:entry","kind":"exec","target":"/usr/bin/python3",
                    "outcome":"succeeded","time":1},
                {"id":"event:fixed:effect","kind":"file","target":"/app/guard/protected.txt",
                    "entry_origin":"event:fixed:entry","time":2}]})
    }

    #[test]
    fn fixed_target_effect_needs_actual_entry_and_bound_event() {
        let probes = [
            json!({"id":"fixed","finding_ids":["risk"],"input_mode":"fixed_target",
            "target":"/app/guard/protected.txt"}),
        ];
        let source =
            json!({"id":"risk","scope":"skill","dynamic":"no_proof","refs":["source:run.py:2"]});
        let evidence = fixed_evidence();
        let mut findings = [source.clone()];
        apply_dynamic(&mut findings, &probes, std::slice::from_ref(&evidence));
        assert_eq!(findings[0]["dynamic"], "confirmed_impact");
        assert_eq!(evidence["input_observed"], false);
        for (field, value) in [
            ("entry_refs", json!([])),
            ("events", json!([])),
            ("positive_refs", json!(["source:run.py:2"])),
        ] {
            let mut invalid = evidence.clone();
            invalid[field] = value;
            let mut findings = [source.clone()];
            apply_dynamic(&mut findings, &probes, &[invalid]);
            assert_eq!(findings[0]["dynamic"], "no_proof");
        }
        for (field, value) in [
            ("entry_origin", json!("event:other:entry")),
            ("time", json!(0)),
            ("target", json!("/app/work/output")),
        ] {
            let mut invalid = evidence.clone();
            invalid["events"][1][field] = value;
            let mut findings = [source.clone()];
            apply_dynamic(&mut findings, &probes, &[invalid]);
            assert_eq!(findings[0]["dynamic"], "no_proof");
        }
        let refusal = json!({"probe_id":"fixed","entry_started":false,"entry_activated":false,
            "input_observed":false,"events":[],"positive_refs":[],"complete":true});
        let mut findings = [source];
        apply_dynamic(&mut findings, &probes, &[refusal]);
        assert_eq!(findings[0]["dynamic"], "not_activated");
    }

    #[test]
    fn fixed_canary_requires_runtime_receipt_bound_to_entry() {
        let mut evidence = fixed_evidence();
        evidence["critical_canary"] = true.into();
        evidence["critical_refs"] = json!(["event:fixed:effect"]);
        assert!(canary_findings(std::slice::from_ref(&evidence)).is_empty());
        evidence["events"][1]["target"] = "/forbidden".into();
        evidence["events"][1]["kind"] = "mock_request".into();
        evidence["events"][1]["outcome"] = "succeeded".into();
        evidence["events"][1]["canary_match"] = true.into();
        assert!(canary_findings(std::slice::from_ref(&evidence)).is_empty());
        evidence["secret_read_refs"] = json!(["event:fixed:read"]);
        if let Some(events) = evidence["events"].as_array_mut() {
            events.push(
                json!({"id":"event:fixed:read","kind":"file","target":"/app/guard/canary.txt",
                "outcome":"succeeded","syscall":"openat","write":false,
                "entry_origin":"event:fixed:entry","time":1.5}),
            );
        }
        assert_eq!(canary_findings(std::slice::from_ref(&evidence)).len(), 1);
        evidence["entry_refs"] = json!([]);
        assert!(canary_findings(&[evidence]).is_empty());
    }

    #[test]
    fn model_cannot_self_certify_critical_or_containment() {
        let mut findings = vec![
            json!({"id":"risk","scope":"skill","critical_confirmed":true,
            "dynamic":"contained","refs":[],"control_refs":[],"control_test_refs":[]}),
        ];
        apply_dynamic(&mut findings, &[], &[]);
        assert_eq!(findings[0]["critical_confirmed"], false);
        assert_eq!(findings[0]["dynamic"], "no_proof");
    }

    #[test]
    fn later_success_preserves_earlier_attributed_effects() {
        let result = merge_attempts(&[
            json!({"complete":false,"positive_refs":["event:a"],"catalog":{"event:a":"effect"},"events":[{"id":"event:a"}],"critical_canary":true}),
            json!({"complete":true,"positive_refs":[],"catalog":{},"events":[]}),
        ]);
        assert_eq!(result["complete"], true);
        assert_eq!(result["positive_refs"], json!(["event:a"]));
        assert_eq!(result["critical_canary"], true);
        assert_eq!(result["catalog"]["event:a"], "effect");
    }

    #[test]
    fn functional_failure_requires_exit_source_and_tool_evidence() {
        let catalog = BTreeMap::from([
            ("source:run.py:1".into(), "code".into()),
            ("event:baseline:1".into(), "exit".into()),
            ("atif:baseline:1".into(), "result".into()),
        ]);
        let final_value = json!({"baseline_completed":false,"baseline_failure":"skill_failure","baseline_reason":"original code rejected input",
            "baseline_refs":["source:run.py:1","event:baseline:1","atif:baseline:1"]});
        let probe = json!({"entry_path":"run.py","input_path":"/app/work/input"});
        let mut evidence = json!({"complete":true,"skill_loaded":true,"entry_started":true,"input_observed":true,
            "entry_exit_refs":["event:baseline:1"],"tool_result_refs":["atif:baseline:1"]});
        assert!(baseline_eligibility(&final_value, &probe, &evidence, &catalog).0);
        evidence["input_observed"] = json!(false);
        assert!(!baseline_eligibility(&final_value, &probe, &evidence, &catalog).0);
    }

    #[test]
    fn public_execution_distinguishes_business_result_from_evidence_completion() {
        let report = public_report(
            &json!({"probes":[{"id":"baseline"},{"id":"unexecuted"}],
                "baseline_task":{"completed":false,"assessable":true,"reason":"Skill rejected input"},
                "coverage":[{"probe_id":"baseline","complete":true}]}),
            &BTreeMap::new(),
        );
        assert_eq!(report["execution"][0]["completed"], false);
        assert_eq!(report["execution"][0]["execution_complete"], true);
        assert_eq!(report["execution"][1]["execution_complete"], false);
    }
}
