//! Bounded model adjudication with frozen oracle prompts and deterministic post-call validation.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};

use asc_model_client::remote::{RemoteError, RemoteModel};
use serde_json::{Value, json};

use crate::{Error, Result, contracts, fixtures, redaction::redact_value, source::private_json};

pub(crate) struct Judge {
    pub model: RemoteModel,
    pub calls: Vec<Value>,
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
}

impl Judge {
    pub fn call(&mut self, stage: &str, payload: &Value, path: &Path) -> Result<Value> {
        let first_call = self.calls.len();
        let result = self.call_inner(stage, payload, path);
        self.persist_attempts(path, first_call)?;
        result
    }

    fn persist_attempts(&self, path: &Path, first_call: usize) -> Result<()> {
        let attempts = self.calls[first_call..]
            .iter()
            .map(|call| {
                json!({"code":call["code"],"seconds":call["seconds"],
                    "reasoning_effort":call["reasoning_effort"]})
            })
            .collect::<Vec<_>>();
        private_json(&path.with_extension("attempts.json"), &json!(attempts))
    }

    fn call_inner(&mut self, stage: &str, payload: &Value, path: &Path) -> Result<Value> {
        let schema = contracts::schema(stage)?;
        let mut payload = payload.clone();
        if stage == "final" {
            remove_prior_verdicts(&mut payload);
        }
        redact_payload(&mut payload, &self.model)?;
        let prompt = format!(
            "{}{}",
            include_str!("../assets/principles.txt"),
            match stage {
                "audit" => include_str!("../assets/planning.txt"),
                "critical" =>
                    "Independently audit for Critical consequences from raw evidence. Return exactly one finding for every requested claim ID, including any you refute, and no other IDs. No previous verdict is supplied. Use baseline_completed=true if not relevant.",
                _ => include_str!("../assets/final.txt"),
            }
        );
        let mut message = format!(
            "{}\nRequired JSON schema:\n{}",
            serde_json::to_string(&payload)?,
            schema
        );
        let catalog: BTreeMap<String, String> = serde_json::from_value(payload["catalog"].clone())?;
        let mut last = String::new();
        for attempt in 1..=3 {
            let response = self.transport(&prompt, &message, stage != "audit")?;
            let raw = contracts::text(&response, "text").trim();
            let raw = if raw.starts_with("```") {
                raw.split_once('\n')
                    .map_or(raw, |(_, rest)| rest.trim_end_matches("```").trim())
            } else {
                raw
            };
            let result = (|| {
                let mut result: Value = serde_json::from_str(raw)?;
                redact_value(&mut result, &self.model)?;
                let candidate_probes = if stage == "audit" {
                    let candidate = result["probes"].clone();
                    result["probes"] = json!([]);
                    Some(candidate)
                } else {
                    None
                };
                contracts::normalize(&mut result, &schema, &schema, stage)?;
                contracts::validate_claims(&result, &catalog, stage == "audit")?;
                if stage == "critical" {
                    validate_critical_coverage(&result, &payload)?;
                } else {
                    contracts::validate_reviews(&result, &catalog, stage == "audit")?;
                }
                if let Some(probes) = candidate_probes {
                    result["probes"] = probes;
                }
                if stage == "final" {
                    validate_final(
                        &mut result,
                        payload.get("audit").unwrap_or(&Value::Null),
                        &payload,
                        &catalog,
                    )?;
                }
                Ok::<_, Error>(result)
            })();
            match result {
                Ok(mut value) => {
                    if stage == "audit" {
                        self.repair_probes(
                            &mut value,
                            &catalog,
                            &payload["source_redaction"],
                            &prompt,
                        )?;
                    }
                    private_json(path, &json!({"result":value,"calls":self.calls}))?;
                    return Ok(value);
                }
                Err(error) => {
                    last = self.model.redact(&error.to_string());
                    let _ = write!(
                        message,
                        "\nValidation error (attempt {attempt}): {last}. Return corrected complete JSON, preserving all claims."
                    );
                }
            }
        }
        Err(Error::Runtime(format!(
            "Judge {stage} output invalid after three attempts: {last}"
        )))
    }

    fn repair_probes(
        &mut self,
        audit: &mut Value,
        catalog: &BTreeMap<String, String>,
        source_redaction: &Value,
        prompt: &str,
    ) -> Result<()> {
        let mut candidates = contracts::array(audit, "probes").to_vec();
        let (mut valid, mut errors) =
            valid_probes(audit, &mut candidates, catalog, source_redaction)?;
        for _ in 0..3 {
            if errors.is_empty() {
                break;
            }
            let message = json!({"objective":audit["objective"],"allowed_boundary":audit["allowed_boundary"],"findings":audit["findings"],"catalog":catalog,"source_redaction":source_redaction,"valid_probes":valid,"failed_probes":errors,"instruction":"Repair only the failed probe IDs. Keep validated tasks unchanged. Return {probes:[...]}.","probe_schema":serde_json::from_str::<Value>(include_str!("../assets/probe-schema.json"))?});
            let repair = (|| {
                let response = self.transport(prompt, &message.to_string(), false)?;
                let mut candidate: Value =
                    serde_json::from_str(unfence(contracts::text(&response, "text")))?;
                redact_value(&mut candidate, &self.model)?;
                merge_repairs(&mut candidates, &valid, &errors, &candidate)
            })();
            match repair {
                Ok(()) => {
                    (valid, errors) =
                        valid_probes(audit, &mut candidates, catalog, source_redaction)?;
                }
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(error) => {
                    eprintln!(
                        "[skill-score] probe repair incomplete: {}",
                        self.model.redact(&error.to_string())
                    );
                    if matches!(error, Error::Deadline | Error::Model(_)) {
                        break;
                    }
                }
            }
        }
        audit["probes"] = json!(valid);
        if !errors.is_empty() {
            audit["planning_errors"] = json!(errors);
            let covered = valid
                .iter()
                .flat_map(|p| {
                    contracts::strings(p, "finding_ids")
                        .into_iter()
                        .map(str::to_owned)
                })
                .collect::<std::collections::BTreeSet<_>>();
            if let Some(findings) = audit["findings"].as_array_mut() {
                for finding in findings {
                    if !covered.contains(contracts::text(finding, "id")) {
                        finding["dynamic"] = "unsupported".into();
                    }
                }
            }
        }
        Ok(())
    }

    fn transport(&mut self, prompt: &str, message: &str, high: bool) -> Result<Value> {
        let client = self.model.clone();
        self.transport_with(high, |high, timeout| {
            let client = client.clone();
            let system = prompt.to_owned();
            let user = message.to_owned();
            let (send, receive) = mpsc::sync_channel(1);
            std::thread::spawn(move || {
                let _ = send.send(client.messages(&system, &user, 24_000, high, timeout));
            });
            receive
        })
    }

    fn transport_with(
        &mut self,
        mut high: bool,
        mut request: impl FnMut(
            bool,
            Duration,
        ) -> mpsc::Receiver<std::result::Result<Value, RemoteError>>,
    ) -> Result<Value> {
        for attempt in 0..3 {
            if self.cancelled.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let remaining = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Deadline)?;
            if remaining < Duration::from_secs(1) {
                return Err(Error::Deadline);
            }
            let started = Instant::now();
            let receive = request(high, remaining.min(Duration::from_secs(300)));
            let response = loop {
                if self.cancelled.load(Ordering::Relaxed) {
                    self.record_failure(started, high, "cancelled");
                    return Err(Error::Cancelled);
                }
                if Instant::now() >= self.deadline {
                    self.record_failure(started, high, "deadline");
                    return Err(Error::Deadline);
                }
                match receive.recv_timeout(Duration::from_millis(100)) {
                    Ok(value) => break value,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => {
                        self.record_failure(started, high, "transport_terminated");
                        return Err(Error::Runtime("model transport terminated".into()));
                    }
                }
            };
            match response {
                Ok(value) => {
                    let mut call = json!({"code":"success","usage":value["usage"],
                        "seconds":started.elapsed().as_secs_f64(),
                        "reasoning_effort":if high { "high" } else { "medium" },
                        "stop_reason":value["stop_reason"]});
                    redact_value(&mut call, &self.model)?;
                    self.calls.push(call);
                    eprintln!(
                        "[skill-score] Judge call completed in {:.1}s",
                        started.elapsed().as_secs_f64()
                    );
                    return Ok(value);
                }
                Err(error) => {
                    let (code, retryable, retry_after) = match &error {
                        RemoteError::Provider {
                            code,
                            retryable,
                            retry_after,
                        } => (*code, *retryable, *retry_after),
                        RemoteError::Configuration(_) => ("configuration_error", false, None),
                    };
                    self.record_failure(started, high, code);
                    if !retryable || attempt == 2 {
                        return Err(Error::Model(error));
                    }
                    if code == "timeout" {
                        high = false;
                    }
                    let until = retry_until(
                        Instant::now(),
                        retry_after.unwrap_or(Duration::from_secs(1)),
                        self.deadline,
                    )?;
                    while Instant::now() < until {
                        if self.cancelled.load(Ordering::Relaxed) {
                            return Err(Error::Cancelled);
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        }
        Err(Error::Runtime("model retry budget exhausted".into()))
    }

    fn record_failure(&mut self, started: Instant, high: bool, code: &'static str) {
        let seconds = started.elapsed().as_secs_f64();
        let effort = if high { "high" } else { "medium" };
        self.calls
            .push(json!({"code":code,"seconds":seconds,"reasoning_effort":effort}));
        eprintln!(
            "[skill-score] Judge call failed: code={code} seconds={seconds:.1} reasoning_effort={effort}"
        );
    }
}

fn redact_payload(payload: &mut Value, model: &RemoteModel) -> Result<()> {
    let mut source = BTreeMap::new();
    if let Some(catalog) = payload.get_mut("catalog").and_then(Value::as_object_mut) {
        let references = catalog
            .keys()
            .filter(|reference| reference.starts_with("source:"))
            .cloned()
            .collect::<Vec<_>>();
        for reference in references {
            if let Some(value) = catalog.remove(&reference) {
                source.insert(reference, value);
            }
        }
    }
    redact_value(payload, model)?;
    if let Some(catalog) = payload.get_mut("catalog").and_then(Value::as_object_mut) {
        for (reference, mut value) in source {
            // The scanner already sanitized this source view with equality-preserving markers.
            // Reapplying generic credential-key rules would collapse distinct source values.
            if let Some(text) = value.as_str() {
                value = Value::String(model.redact(text));
            } else {
                redact_value(&mut value, model)?;
            }
            catalog.insert(reference, value);
        }
    }
    Ok(())
}

fn retry_until(now: Instant, delay: Duration, deadline: Instant) -> Result<Instant> {
    now.checked_add(delay)
        .filter(|until| *until < deadline)
        .ok_or(Error::Deadline)
}

fn remove_prior_verdicts(payload: &mut Value) {
    if let Some(audit) = payload["audit"].as_object_mut() {
        for key in [
            "hygiene_review",
            "pattern_checks",
            "advisory",
            "corrections",
            "limitations",
        ] {
            audit.remove(key);
        }
        for (field, remove) in [
            (
                "findings",
                &[
                    "status",
                    "severity",
                    "dynamic",
                    "critical_confirmed",
                    "reason",
                    "title",
                    "scope",
                ][..],
            ),
            ("control_gaps", &["status", "reason"][..]),
        ] {
            if let Some(items) = audit.get_mut(field).and_then(Value::as_array_mut) {
                for item in items {
                    if let Some(fields) = item.as_object_mut() {
                        for key in remove {
                            fields.remove(*key);
                        }
                    }
                }
            }
        }
    }
}

fn unfence(text: &str) -> &str {
    let raw = text.trim();
    if raw.starts_with("```") {
        raw.split_once('\n')
            .map_or(raw, |(_, rest)| rest.trim_end_matches("```").trim())
    } else {
        raw
    }
}

fn source_files(catalog: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (reference, content) in catalog {
        let Some(source) = reference.strip_prefix("source:") else {
            continue;
        };
        let (path, number) = source
            .rsplit_once(':')
            .ok_or_else(|| Error::Contract("source reference lacks a line number".into()))?;
        let line = number
            .parse::<usize>()
            .ok()
            .filter(|line| *line > 0 && *line <= crate::source::MAX_TEXT_BYTES)
            .ok_or_else(|| {
                Error::Contract("source reference line exceeds snapshot bounds".into())
            })?;
        if path.is_empty() || number != line.to_string() {
            return Err(Error::Contract("source reference is not canonical".into()));
        }
        let lines = files.entry(path.to_owned()).or_default();
        if lines.len() < line {
            // Source references retain their line numbers even in a sparse catalog.
            lines.resize(line, String::new());
        }
        lines[line - 1].clone_from(content);
    }
    Ok(files
        .into_iter()
        .map(|(path, lines)| (path, lines.join("\n")))
        .collect())
}

fn valid_probes(
    core: &Value,
    candidates: &mut [Value],
    catalog: &BTreeMap<String, String>,
    source_redaction: &Value,
) -> Result<(Vec<Value>, BTreeMap<String, String>)> {
    use contracts::{array, strings, text};
    let schema: Value = serde_json::from_str(include_str!("../assets/probe-schema.json"))?;
    let inventory: Value = serde_json::from_str(include_str!("../assets/tool-inventory.json"))?;
    let files = source_files(catalog)?;
    let mut errors = BTreeMap::new();
    let normalized = normalize_candidates(
        candidates,
        &schema,
        &inventory,
        &files,
        source_redaction,
        &mut errors,
    );
    let mut valid = Vec::new();
    if let Some(baseline) = normalized
        .iter()
        .find(|p| text(p, "template") == "baseline")
    {
        let mut test = core.clone();
        test["probes"] = json!([baseline]);
        match contracts::select_probes(&mut test, &files, true, false) {
            Ok(()) => valid = array(&test, "probes").to_vec(),
            Err(error) => {
                errors.insert("baseline".into(), error.to_string());
            }
        }
    }
    if valid.is_empty() {
        errors
            .entry("baseline".into())
            .or_insert_with(|| "a valid original normal task is required".into());
        return Ok((valid, errors));
    }
    for probe in normalized
        .iter()
        .filter(|p| text(p, "template") != "baseline")
    {
        let mut test = core.clone();
        let mut planned = valid.clone();
        planned.push(probe.clone());
        test["probes"] = planned.into();
        match contracts::select_probes(&mut test, &files, true, false) {
            Ok(()) => valid = array(&test, "probes").to_vec(),
            Err(error) => {
                errors.insert(text(probe, "id").to_owned(), error.to_string());
            }
        }
    }
    for finding in array(core, "findings") {
        if text(finding, "scope") == "skill"
            && matches!(text(finding, "status"), "material_risk" | "unresolved")
            && matches!(text(finding, "severity"), "Critical" | "High")
            && text(finding, "dynamic") != "unsupported"
            && !valid
                .iter()
                .any(|p| strings(p, "finding_ids").contains(&text(finding, "id")))
            && !candidates.iter().any(|p| {
                strings(p, "finding_ids").contains(&text(finding, "id"))
                    && errors.contains_key(text(p, "id"))
            })
        {
            errors.insert(
                format!("probe-{}", text(finding, "id")),
                "missing necessary probe; supply this ID or leave explicitly unverified".into(),
            );
        }
    }
    Ok((valid, errors))
}

fn normalize_candidates(
    candidates: &mut [Value],
    schema: &Value,
    inventory: &Value,
    files: &BTreeMap<String, String>,
    source_redaction: &Value,
    errors: &mut BTreeMap<String, String>,
) -> Vec<Value> {
    use contracts::{strings, text};
    let mut normalized = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for (index, candidate) in candidates.iter_mut().enumerate() {
        if text(candidate, "template") == "baseline" {
            candidate["id"] = "baseline".into();
        }
        let id = if text(candidate, "id").is_empty() {
            format!("invalid-{index}")
        } else {
            text(candidate, "id").to_owned()
        };
        let result = (|| {
            let mut probe = candidate.clone();
            contracts::normalize(&mut probe, schema, schema, "probe")?;
            fixtures::materialize(&mut probe)?;
            crate::redaction::validate_source_values(&probe, source_redaction)?;
            if !ids.insert(id.clone()) {
                return Err(Error::Contract("duplicate probe ID".into()));
            }
            if !files.contains_key(text(&probe, "entry_path")) {
                return Err(Error::Contract(
                    "missing original entry; do not rewrite the Skill".into(),
                ));
            }
            if strings(&probe, "required_tools").iter().any(|name| {
                inventory["executables"]
                    .get(*name)
                    .is_some_and(Value::is_null)
            }) {
                return Err(Error::Contract("prerequisite is absent from the image; use the original documented entry or leave the task unverified".into()));
            }
            Ok(probe)
        })();
        match result {
            Ok(probe) => normalized.push(probe),
            Err(error) => {
                errors.insert(id, error.to_string());
            }
        }
    }

    normalized
}

fn merge_repairs(
    candidates: &mut Vec<Value>,
    valid: &[Value],
    errors: &BTreeMap<String, String>,
    response: &Value,
) -> Result<()> {
    use contracts::{array, text};
    if response
        .as_object()
        .is_none_or(|fields| fields.len() != 1 || !fields.contains_key("probes"))
        || !response["probes"].is_array()
    {
        return Err(Error::Contract(
            "repair response must contain only the failed probes array".into(),
        ));
    }
    let schema: Value = serde_json::from_str(include_str!("../assets/probe-schema.json"))?;
    let mut replacements = Vec::new();
    for replacement in array(response, "probes") {
        let mut replacement = replacement.clone();
        if text(&replacement, "template") == "baseline" && errors.contains_key("baseline") {
            replacement["id"] = "baseline".into();
        }
        let id = text(&replacement, "id");
        if errors.contains_key(id) {
            replacements.push(replacement);
            continue;
        }
        let mut normalized = replacement.clone();
        contracts::normalize(&mut normalized, &schema, &schema, "repair")?;
        fixtures::materialize(&mut normalized)?;
        if let Some(fields) = normalized.as_object_mut() {
            fields.remove("target");
            fields.remove("required");
        }
        let unchanged = valid
            .iter()
            .find(|p| text(p, "id") == id)
            .is_some_and(|probe| {
                let mut probe = probe.clone();
                if let Some(fields) = probe.as_object_mut() {
                    fields.remove("target");
                    fields.remove("required");
                }
                probe == normalized
            });
        if !unchanged {
            return Err(Error::Contract(
                "repair may change only the requested failed probe IDs".into(),
            ));
        }
    }
    let replaced = replacements
        .iter()
        .map(|p| text(p, "id").to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    candidates.retain(|p| p.is_object() && !replaced.contains(text(p, "id")));
    candidates.extend(replacements);
    Ok(())
}

fn validate_final(
    value: &mut Value,
    prior: &Value,
    payload: &Value,
    catalog: &BTreeMap<String, String>,
) -> Result<()> {
    use contracts::{array, text};
    validate_preserved_claims(value, prior)?;
    let observed = array(payload, "coverage");
    let supplied_results = validate_task_coverage(value, observed, catalog)?;
    let mut results = Vec::new();
    for item in observed {
        let id = text(item, "probe_id");
        let probe = array(prior, "probes")
            .iter()
            .find(|p| text(p, "id") == id)
            .ok_or_else(|| Error::Contract("evidence references unplanned probe".into()))?;
        let checks = validate_task_checks(value, probe, id, catalog)?;
        if let Some(result) = apply_task_result(value, probe, item, &checks, &supplied_results)? {
            results.push(result);
        }
        value["task_checks"][id] = checks.into();
    }
    value["task_results"] = results.into();
    validate_failure_attribution(value, prior, observed, catalog);
    Ok(())
}

fn validate_critical_coverage(result: &Value, payload: &Value) -> Result<()> {
    use contracts::{array, text};
    let mut outstanding = array(payload, "claims")
        .iter()
        .map(|claim| text(claim, "id"))
        .collect::<std::collections::BTreeSet<_>>();
    for answer in array(result, "findings") {
        let id = text(answer, "id");
        if !outstanding.remove(id) {
            return Err(Error::Contract(format!(
                "unexpected or duplicate Critical review ID: {id}"
            )));
        }
    }
    if !outstanding.is_empty() {
        return Err(Error::Contract(format!(
            "Critical review omitted claim IDs: {}",
            outstanding.into_iter().collect::<Vec<_>>().join(", ")
        )));
    }
    Ok(())
}

fn validate_preserved_claims(value: &Value, prior: &Value) -> Result<()> {
    use contracts::{array, strings, text};
    for field in ["findings", "control_gaps"] {
        if array(prior, field).iter().any(|p| {
            !array(value, field)
                .iter()
                .any(|v| text(v, "id") == text(p, "id"))
        }) {
            return Err(Error::Contract(format!(
                "final Judge dropped prior {field}"
            )));
        }
    }
    for finding in array(value, "findings") {
        if let Some(old) = array(prior, "findings")
            .iter()
            .find(|f| text(f, "id") == text(finding, "id"))
            && matches!(
                text(finding, "status"),
                "refuted" | "accepted" | "control_gap"
            )
            && (text(finding, "reason").trim().is_empty()
                || !strings(old, "refs")
                    .iter()
                    .filter(|r| r.starts_with("source:"))
                    .any(|r| {
                        strings(finding, "refs").contains(r)
                            || strings(finding, "control_refs").contains(r)
                    }))
        {
            return Err(Error::Contract(
                "excluding a claim must address its original source evidence".into(),
            ));
        }
    }
    Ok(())
}

fn validate_task_coverage(
    value: &Value,
    observed: &[Value],
    catalog: &BTreeMap<String, String>,
) -> Result<Vec<Value>> {
    use contracts::{array, strings, text};
    let supplied = value["task_checks"]
        .as_object()
        .ok_or_else(|| Error::Contract("task_checks missing".into()))?;
    if supplied
        .keys()
        .any(|id| !observed.iter().any(|o| text(o, "probe_id") == id))
    {
        return Err(Error::Contract("task_checks claim unexecuted task".into()));
    }
    let supplied_results = array(value, "task_results").to_vec();
    let supplied_ids = supplied_results
        .iter()
        .map(|r| text(r, "task_id"))
        .collect::<std::collections::BTreeSet<_>>();
    let executed_ids = observed
        .iter()
        .map(|o| text(o, "probe_id"))
        .filter(|id| *id != "baseline")
        .collect::<std::collections::BTreeSet<_>>();
    if supplied_ids.len() != supplied_results.len() || supplied_ids != executed_ids {
        return Err(Error::Contract(
            "task_results must cover each executed non-baseline task exactly once".into(),
        ));
    }
    if supplied_results.iter().any(|r| {
        strings(r, "refs")
            .iter()
            .any(|reference| !catalog.contains_key(*reference))
    }) {
        return Err(Error::Contract(
            "task completion references unavailable evidence".into(),
        ));
    }
    Ok(supplied_results)
}

fn validate_task_checks(
    value: &Value,
    probe: &Value,
    id: &str,
    catalog: &BTreeMap<String, String>,
) -> Result<Vec<Value>> {
    use contracts::{strings, text};
    let expected = text(probe, "expected_result");
    let mut checks = value["task_checks"][id]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if checks.len() == 1 && text(&checks[0], "expected").is_empty() {
        checks[0]["expected"] = expected.into();
    }
    if checks.is_empty()
        || checks
            .iter()
            .map(|c| text(c, "expected"))
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            != expected.split_whitespace().collect::<Vec<_>>()
    {
        checks = vec![
            json!({"expected":expected,"observed":"Judge did not compare the entire original expectation","status":"unknown","refs":[]}),
        ];
    }
    for check in &checks {
        if strings(check, "refs")
            .iter()
            .any(|r| !catalog.contains_key(*r))
            || (text(check, "status") == "met"
                && !strings(check, "refs").iter().any(|r| {
                    ["atif", "event", "snapshot"]
                        .iter()
                        .any(|kind| r.starts_with(&format!("{kind}:{id}:")))
                }))
        {
            return Err(Error::Contract(
                "business result lacks matching execution evidence".into(),
            ));
        }
    }
    Ok(checks)
}

fn apply_task_result(
    value: &mut Value,
    probe: &Value,
    item: &Value,
    checks: &[Value],
    supplied_results: &[Value],
) -> Result<Option<Value>> {
    use contracts::{array, flag, strings, text};
    let id = text(item, "probe_id");
    let completed = checks.iter().all(|c| text(c, "status") == "met");
    let reason = checks
        .iter()
        .map(|c| {
            format!(
                "{}: {} -> {}",
                text(c, "status"),
                text(c, "expected"),
                text(c, "observed")
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let refs = checks
        .iter()
        .flat_map(|c| strings(c, "refs"))
        .collect::<Vec<_>>();
    if id == "baseline" {
        value["baseline_completed"] = completed.into();
        value["baseline_reason"] = reason.into();
        let mut refs = refs.iter().map(|r| Value::from(*r)).collect::<Vec<_>>();
        refs.extend(array(value, "baseline_refs").iter().cloned());
        value["baseline_refs"] = refs.into();
        if completed {
            value["baseline_failure"] = "none".into();
        } else if text(value, "baseline_failure") == "none" {
            value["baseline_failure"] = "unknown".into();
        }
    } else {
        let mut all_refs = supplied_results
            .iter()
            .find(|r| text(r, "task_id") == id)
            .map_or_else(Vec::new, |r| strings(r, "refs"))
            .into_iter()
            .chain(refs)
            .collect::<Vec<_>>();
        all_refs.sort_unstable();
        all_refs.dedup();
        if completed
            && !(flag(item, "complete")
                && flag(item, "skill_loaded")
                && (text(probe, "entry_path") == "SKILL.md"
                    || (flag(item, "entry_started")
                        && (text(probe, "input_path").is_empty() || flag(item, "input_observed"))))
                && all_refs.iter().any(|r| {
                    r.starts_with(&format!("atif:{id}:")) || r.starts_with(&format!("event:{id}:"))
                }))
        {
            return Err(Error::Contract(
                "completed task needs matching entry, input and business-result evidence".into(),
            ));
        }
        return Ok(Some(
            json!({"task_id":id,"completed":completed,"reason":reason,"refs":all_refs}),
        ));
    }
    Ok(None)
}

fn validate_failure_attribution(
    value: &mut Value,
    prior: &Value,
    observed: &[Value],
    catalog: &BTreeMap<String, String>,
) {
    use contracts::{array, flag, text};
    if !flag(value, "baseline_completed")
        && text(value, "baseline_failure") == "skill_failure"
        && let Some(probe) = array(prior, "probes")
            .iter()
            .find(|p| text(p, "id") == "baseline")
    {
        let empty = json!({});
        let baseline = observed
            .iter()
            .find(|e| text(e, "probe_id") == "baseline")
            .unwrap_or(&empty);
        let (eligible, reason) =
            crate::evidence::baseline_eligibility(value, probe, baseline, catalog);
        if !eligible {
            value["baseline_failure"] = "unknown".into();
            value["baseline_reason"] = format!(
                "{}; failure attribution unverified: {reason}",
                text(value, "baseline_reason")
            )
            .into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use contracts::{array, text};

    fn judge(budget: Duration) -> Result<Judge> {
        let environment = BTreeMap::from([
            ("ANTHROPIC_API_KEY".into(), "fixture-secret".into()),
            (
                "ANTHROPIC_BASE_URL".into(),
                "https://dashscope.aliyuncs.com/apps/anthropic".into(),
            ),
        ]);
        Ok(Judge {
            model: RemoteModel::resolve(Path::new("/nonexistent/asc-judge-fixture"), &environment)?,
            calls: Vec::new(),
            deadline: Instant::now() + budget,
            cancelled: Arc::new(AtomicBool::new(false)),
        })
    }

    #[test]
    fn each_requested_critical_claim_needs_one_answer_regardless_of_order() {
        let payload = json!({"claims":[{"id":"one"},{"id":"two"}]});
        for returned in [
            vec![],
            vec!["one"],
            vec!["one", "misspelled"],
            vec!["one", "two", "extra"],
            vec!["one", "one"],
        ] {
            let answers = returned
                .into_iter()
                .map(|id| json!({"id":id}))
                .collect::<Vec<_>>();
            assert!(validate_critical_coverage(&json!({"findings":answers}), &payload).is_err());
        }
        let reversed = json!({"findings":[{"id":"two"},{"id":"one"}]});
        assert!(validate_critical_coverage(&reversed, &payload).is_ok());
    }

    #[test]
    fn source_markers_survive_payload_redaction_without_exempting_runtime_values() -> Result<()> {
        let judge = judge(Duration::from_secs(1))?;
        let marker = "[REDACTED]__ASC_OPAQUE_00000000_00000001__";
        let source = format!(r#"config = {{"password":"{marker}"}}; output = "[REDACTED]""#);
        let mut payload = json!({
            "catalog": {
                "source:scripts/redact.py:1": source,
                "source:credential.txt:1": "fixture-secret",
                "event:1": "{\"password\":\"runtime-sensitive-value\",\"exit_code\":0}",
            },
            "source_redaction": {"markers":[{"marker":marker,"refs":["source:scripts/redact.py:1"]}]},
            "debug": "fixture-secret",
        });
        redact_payload(&mut payload, &judge.model)?;
        assert_eq!(payload["catalog"]["source:scripts/redact.py:1"], source);
        assert_eq!(payload["source_redaction"]["markers"][0]["marker"], marker);
        let serialized = payload.to_string();
        assert!(!serialized.contains("runtime-sensitive-value"));
        assert!(!serialized.contains("fixture-secret"));
        let runtime: Value = serde_json::from_str(
            payload["catalog"]["event:1"]
                .as_str()
                .expect("event remains JSON text"),
        )?;
        assert_eq!(runtime["exit_code"], 0);
        Ok(())
    }

    fn response(
        value: std::result::Result<Value, RemoteError>,
    ) -> mpsc::Receiver<std::result::Result<Value, RemoteError>> {
        let (send, receive) = mpsc::sync_channel(1);
        send.send(value).expect("fixture receiver is live");
        receive
    }

    fn failure(code: &'static str, retryable: bool, delay: Duration) -> RemoteError {
        RemoteError::Provider {
            code,
            retryable,
            retry_after: Some(delay),
        }
    }

    #[test]
    fn timeout_retries_reduce_effort_and_preserve_known_usage() -> Result<()> {
        let mut judge = judge(Duration::from_secs(900))?;
        let mut efforts = Vec::new();
        let result = judge.transport_with(true, |high, timeout| {
            efforts.push(high);
            assert_eq!(timeout, Duration::from_secs(300));
            response(match efforts.len() {
                1 => Err(failure("timeout", true, Duration::ZERO)),
                2 => Err(failure("server_error", true, Duration::ZERO)),
                _ => Ok(json!({"text":"{}","usage":{"input_tokens":7,"output_tokens":3}})),
            })
        })?;
        assert_eq!(efforts, [true, false, false]);
        assert_eq!(result["usage"]["input_tokens"], 7);
        assert_eq!(judge.calls.len(), 3);
        assert!(
            judge.calls[..2]
                .iter()
                .all(|call| call.get("usage").is_none())
        );
        assert_eq!(judge.calls[0]["reasoning_effort"], "high");
        assert_eq!(judge.calls[1]["reasoning_effort"], "medium");
        assert_eq!(judge.calls[2]["code"], "success");
        Ok(())
    }

    #[test]
    fn exhausted_and_nonretryable_calls_keep_safe_attempts() -> Result<()> {
        for (code, retryable, count) in [("timeout", true, 3), ("transport_failed", false, 1)] {
            let mut judge = judge(Duration::from_secs(10))?;
            let outcome = judge.transport_with(true, |_, _| {
                response(Err(failure(code, retryable, Duration::ZERO)))
            });
            assert!(matches!(outcome, Err(Error::Model(_))));
            assert_eq!(judge.calls.len(), count);
            assert!(judge.calls.iter().all(|call| call.get("usage").is_none()));
            let directory = tempfile::tempdir()?;
            let path = directory.path().join("final.json");
            judge.persist_attempts(&path, 0)?;
            let saved: Value =
                serde_json::from_slice(&std::fs::read(path.with_extension("attempts.json"))?)?;
            assert_eq!(saved.as_array().expect("attempt array").len(), count);
            assert_eq!(saved[0]["code"], code);
            assert!(saved[0]["seconds"].as_f64().is_some());
            assert!(!saved.to_string().contains("fixture-secret"));
            assert!(!path.exists());
        }
        Ok(())
    }

    #[test]
    fn retry_delay_and_request_timeout_share_the_remaining_budget() -> Result<()> {
        let budget = Duration::from_secs(2);
        let mut judge = judge(budget)?;
        let outcome = judge.transport_with(true, |high, timeout| {
            assert!(high);
            assert!(timeout <= budget);
            response(Err(failure("timeout", true, budget)))
        });
        assert!(matches!(outcome, Err(Error::Deadline)));
        assert_eq!(judge.calls.len(), 1);
        let mut calls = 0;
        judge.deadline = Instant::now();
        let outcome = judge.transport_with(true, |_, _| {
            calls += 1;
            response(Ok(json!({})))
        });
        assert!(matches!(outcome, Err(Error::Deadline)));
        assert_eq!(calls, 0);
        Ok(())
    }

    #[test]
    fn other_transient_failures_preserve_the_requested_effort() -> Result<()> {
        for initial_high in [false, true] {
            let mut judge = judge(Duration::from_secs(10))?;
            let mut efforts = Vec::new();
            let outcome = judge.transport_with(initial_high, |high, _| {
                efforts.push(high);
                response(Err(failure("rate_limited", true, Duration::ZERO)))
            });
            assert!(matches!(outcome, Err(Error::Model(_))));
            assert_eq!(efforts, [initial_high; 3]);
        }
        Ok(())
    }

    fn baseline() -> Value {
        json!({"id":"baseline","finding_ids":[],"template":"baseline","turns":["Run normally"],"entry_path":"SKILL.md","activation":"Read Skill","expected_result":"normal result","input_path":""})
    }
    fn targeted(id: &str, finding: &str) -> Value {
        json!({"id":id,"finding_ids":[finding],"template":"process_marker","turns":["Process supplied input"],"entry_path":"run.py","activation":"Pass supplied input to documented script","expected_result":"normal result","input_path":"/app/work/input.txt","fixtures":{"input.txt":"data"}})
    }

    #[test]
    fn source_reconstruction_preserves_reference_lines_and_bounds() -> Result<()> {
        let catalog = BTreeMap::from([
            ("source:run.py:3".into(), "guard.unlink()".into()),
            ("source:run.py:1".into(), "from pathlib import Path".into()),
            ("event:baseline:1".into(), "not source".into()),
        ]);
        assert_eq!(
            source_files(&catalog)?["run.py"],
            "from pathlib import Path\n\nguard.unlink()"
        );
        for line in ["0", "01", "600001", "not-a-line"] {
            assert!(
                source_files(&BTreeMap::from([(
                    format!("source:run.py:{line}"),
                    "guard.unlink()".into()
                )]))
                .is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn opaque_source_values_require_probe_repair_not_execution() -> Result<()> {
        let marker = "[REDACTED]__ASC_OPAQUE_00000000_00000001__";
        let provenance = json!({"markers":[{"marker":marker}]});
        let catalog = BTreeMap::from([("source:SKILL.md:1".into(), "Normal output".into())]);
        let mut probe = baseline();
        probe["expected_result"] = format!("Output {marker}").into();
        let (valid, errors) =
            valid_probes(&json!({"findings":[]}), &mut [probe], &catalog, &provenance)?;
        assert!(valid.is_empty());
        assert!(errors.contains_key("baseline"));
        let (valid, errors) = valid_probes(
            &json!({"findings":[]}),
            &mut [baseline()],
            &catalog,
            &provenance,
        )?;
        assert_eq!(valid.len(), 1);
        assert!(errors.is_empty());
        Ok(())
    }

    #[test]
    fn failed_probe_repair_preserves_independently_valid_work_and_source() -> Result<()> {
        let core = json!({"findings":[{"id":"f1","scope":"skill","status":"material_risk","severity":"High","dynamic":"no_proof","refs":["source:run.py:1"]},{"id":"f2","scope":"skill","status":"material_risk","severity":"High","dynamic":"no_proof","refs":["source:run.py:1"]}],"probes":[]});
        let catalog = BTreeMap::from([
            ("source:SKILL.md:1".into(), "Use run.py".into()),
            ("source:run.py:1".into(), "print('test')".into()),
        ]);
        let mut broken = targeted("bad", "f2");
        broken["entry_path"] = "missing.py".into();
        let mut candidates = vec![baseline(), targeted("good", "f1"), broken];
        let (valid, errors) = valid_probes(&core, &mut candidates, &catalog, &Value::Null)?;
        assert_eq!(
            valid.iter().map(|p| text(p, "id")).collect::<Vec<_>>(),
            vec!["baseline", "good"]
        );
        assert!(errors.contains_key("bad"));
        assert_eq!(array(&core, "findings").len(), 2);
        let mut malicious = targeted("good", "f1");
        malicious["expected_result"] = "altered frozen task".into();
        assert!(
            merge_repairs(
                &mut candidates,
                &valid,
                &errors,
                &json!({"probes":[malicious]})
            )
            .is_err()
        );
        merge_repairs(
            &mut candidates,
            &valid,
            &errors,
            &json!({"probes":[targeted("bad","f2")]}),
        )?;
        let (repaired, errors) = valid_probes(&core, &mut candidates, &catalog, &Value::Null)?;
        assert!(errors.is_empty());
        assert_eq!(repaired.len(), 3);
        assert_eq!(repaired[1], valid[1]);
        Ok(())
    }

    #[test]
    fn final_results_require_exact_coverage_and_proven_entry_for_business_success() -> Result<()> {
        let probe = targeted("targeted", "f1");
        let prior = json!({"findings":[],"control_gaps":[],"probes":[probe]});
        let mut payload = json!({"coverage":[{"probe_id":"targeted","complete":true,"skill_loaded":true,"entry_started":false,"input_observed":true}]});
        let catalog = BTreeMap::from([("event:targeted:1".into(), "observed result".into())]);
        let mut final_value = json!({"findings":[],"control_gaps":[],"task_checks":{"targeted":[{"expected":"normal result","observed":"matches","status":"met","refs":["event:targeted:1"]}]},"task_results":[]});
        assert!(validate_final(&mut final_value, &prior, &payload, &catalog).is_err());
        final_value["task_results"] = json!([{"task_id":"targeted","completed":true,"reason":"matches","refs":["event:targeted:1"]}]);
        assert!(validate_final(&mut final_value, &prior, &payload, &catalog).is_err());
        payload["coverage"][0]["entry_started"] = true.into();
        validate_final(&mut final_value, &prior, &payload, &catalog)?;
        assert_eq!(final_value["task_results"][0]["completed"], true);
        final_value["task_checks"]["targeted"][0]["status"] = "unmet".into();
        validate_final(&mut final_value, &prior, &payload, &catalog)?;
        assert_eq!(final_value["task_results"][0]["completed"], false);
        assert_eq!(payload["coverage"][0]["complete"], true);
        Ok(())
    }
    #[test]
    fn provider_retry_delay_cannot_overflow_or_outlive_the_assessment() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        assert!(retry_until(now, Duration::MAX, deadline).is_err());
        assert!(retry_until(now, Duration::from_secs(10), deadline).is_err());
        assert!(retry_until(now, Duration::from_secs(1), deadline).is_ok());
    }
}
