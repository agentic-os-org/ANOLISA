//! Explicit local Skill assessment: native planning/scoring and a separately installed Harbor worker.

mod bounded_json;
mod catalog;
mod contracts;
mod evidence;
mod fixtures;
mod judge;
mod masb;
mod redaction;
mod scanners;
mod score;
mod source;
mod worker;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use asc_model_client::remote::RemoteModel;
use serde_json::{Value, json};
use thiserror::Error;

use contracts::{array, flag, strings, text};
use source::{Snapshot, catalog, private_json};

/// Errors preserve input failures separately from execution failures and cancellation.
#[derive(Debug, Error)]
pub enum Error {
    /// The task, input tree, or evidence binding violates the public contract.
    #[error("invalid scoring contract: {0}")]
    Contract(String),
    /// A required execution or assessment operation failed.
    #[error("scoring unavailable or incomplete: {0}")]
    Runtime(String),
    /// Rejected raw evidence could not be removed; no assessment report may be published.
    #[error(
        "unsafe evidence retained in a private request directory; do not distribute evidence; request={request_id}, compose_project={compose_project}"
    )]
    UnsafeEvidence {
        /// Controller-issued request identity, usable for manual recovery.
        request_id: String,
        /// Exact owned resource scope, never a wildcard cleanup selector.
        compose_project: String,
    },
    /// Local filesystem or subprocess operation failed.
    #[error("scoring I/O: {0}")]
    Io(#[from] std::io::Error),
    /// A descriptor-relative filesystem operation failed.
    #[error("scoring filesystem: {0}")]
    Unix(#[from] rustix::io::Errno),
    /// A filesystem or process operation failed at a known assessment stage and path.
    #[error("scoring I/O: stage={stage} path={path:?}: {source}")]
    File {
        /// Operation that failed, without task contents or process arguments.
        stage: &'static str,
        /// File, directory, or executable involved in the failed operation.
        path: std::path::PathBuf,
        /// Original operating-system error, including its errno when available.
        #[source]
        source: std::io::Error,
    },
    /// Malformed JSON at a contract boundary.
    #[error("invalid scoring JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Shared remote configuration or sanitized provider failure.
    #[error(transparent)]
    Model(#[from] asc_model_client::remote::RemoteError),
    /// A caller requested cancellation.
    #[error("skill assessment cancelled")]
    Cancelled,
    /// The overall assessment budget expired.
    #[error("skill assessment deadline exceeded")]
    Deadline,
}

impl Error {
    /// Attach a stage and path to an unlocated I/O error, preserving other error categories.
    pub fn at_path(stage: &'static str, path: &Path, error: impl Into<Self>) -> Self {
        let source = match error.into() {
            Self::Io(source) => source,
            Self::Unix(source) => source.into(),
            other => return other,
        };
        Self::File {
            stage,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Result type for native assessment operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Public report and its independently checked process exit status.
pub struct RunOutcome {
    /// Report-v1, including completion and cleanup state.
    pub report: Value,
    /// Zero only after complete execution, adjudication, and cleanup.
    pub exit_code: u8,
}

fn judge(cancelled: Arc<AtomicBool>) -> Result<judge::Judge> {
    let model = RemoteModel::from_environment()?;
    if model.uses_legacy_configuration() {
        eprintln!(
            "[skill-score] ANTHROPIC_API_KEY/ANTHROPIC_BASE_URL are deprecated; configure the shared ASC model service"
        );
    }
    Ok(judge::Judge {
        model,
        calls: Vec::new(),
        deadline: Instant::now() + Duration::from_secs(45 * 60),
        cancelled,
    })
}

/// Plan against a private snapshot; no worker, Docker, or daemon is required.
///
/// # Errors
/// Rejects unsupported input trees, failed scanner coverage, and invalid model adjudication.
pub fn plan(skill: &Path, cancelled: Arc<AtomicBool>) -> Result<Value> {
    let skill = skill.canonicalize()?;
    let mut snapshot = Snapshot::create(&skill)?;
    let mut judge = judge(cancelled)?;
    eprintln!("[skill-score] static scan");
    snapshot.validate_paths(&skill, &judge.model)?;
    let signals = scanners::scan(&mut snapshot.files, &judge.model, true)?;
    let catalog = catalog(&snapshot.files)?;
    let environment: Value = serde_json::from_str(include_str!("../assets/tool-inventory.json"))?;
    let payload = json!({"catalog":catalog,"static":signals,"source_redaction":signals["source_redaction"],"omitted":snapshot.omitted,"missing_script_references":missing_scripts(&snapshot.files),"risk_catalog":contracts::risk_catalog()?,"hygiene_checks":contracts::rules()?,"environment":environment});
    let output = tempfile::tempdir()?;
    eprintln!("[skill-score] semantic audit");
    let mut audit = judge.call("audit", &payload, &output.path().join("audit.json"))?;
    audit["omitted_sources"] = json!(snapshot.omitted);
    let mut limitations = array(&audit, "limitations").to_vec();
    if let Some(checks) = audit["pattern_checks"].as_object() {
        for (id, check) in checks {
            if text(check, "status") == "insufficient_evidence" {
                limitations.push(format!("{id}: {}", text(check, "reason")).into());
            }
        }
    }
    audit["limitations"] = limitations.into();
    if let Some(findings) = audit["findings"].as_array_mut() {
        for finding in findings {
            finding["critical_confirmed"] = false.into();
            if text(finding, "dynamic") != "unsupported" {
                finding["dynamic"] = "no_proof".into();
            }
        }
    }
    let mut task = contracts::public_task(&skill, &snapshot.hash, &audit);
    redaction::redact_value(&mut task, &judge.model)?;
    let mut checked = parse_task(task.clone(), &skill)?;
    contracts::validate_claims(&checked["audit"], &catalog, true)?;
    contracts::validate_probes(&mut checked["audit"], &snapshot.files, false)?;
    for probe in array(&checked["audit"], "probes") {
        redaction::validate_source_values(probe, &signals["source_redaction"])?;
    }
    Ok(task)
}

fn missing_scripts(files: &BTreeMap<String, String>) -> Vec<String> {
    files.get("SKILL.md").map_or_else(Vec::new, |markdown| {
        markdown
            .split(|c: char| c.is_whitespace() || matches!(c, '`' | '(' | ')' | '[' | ']'))
            .filter(|s| {
                s.starts_with("scripts/")
                    && [".py", ".sh", ".js", ".ts"]
                        .iter()
                        .any(|suffix| s.ends_with(suffix))
                    && !files.contains_key(*s)
            })
            .map(str::to_owned)
            .collect()
    })
}

/// Validate the public task shape and resolve relative Skill paths against the task's directory.
///
/// # Errors
/// Caller-supplied dynamic verdicts, private confirmation fields, and unknown task fields fail.
pub fn parse_task(mut task: Value, base: &Path) -> Result<Value> {
    let object = task
        .as_object()
        .ok_or_else(|| Error::Contract("task input must be one JSON object".into()))?;
    if object.len() != 4
        || !["schema_version", "skill_path", "content_hash", "audit"]
            .iter()
            .all(|k| object.contains_key(*k))
        || text(&task, "schema_version") != "agent-sec-skill-score/task-v1"
    {
        return Err(Error::Contract("unsupported task-v1 fields/schema".into()));
    }
    if text(&task, "content_hash").len() != 64
        || !text(&task, "content_hash")
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Contract(
            "content_hash must be lowercase SHA256".into(),
        ));
    }
    let audit = task["audit"]
        .as_object()
        .ok_or_else(|| Error::Contract("audit must be an object".into()))?;
    if !["objective", "allowed_boundary", "findings", "probes"]
        .iter()
        .all(|k| audit.contains_key(*k))
        || audit.keys().any(|k| {
            !matches!(
                k.as_str(),
                "objective"
                    | "allowed_boundary"
                    | "findings"
                    | "probes"
                    | "control_gaps"
                    | "limitations"
                    | "omitted_sources"
                    | "planning_errors"
                    | "missing_implementation"
            )
        })
    {
        return Err(Error::Contract("unsupported public audit fields".into()));
    }
    for finding in array(&task["audit"], "findings") {
        if ["critical_confirmed", "control_test_refs", "dynamic_target"]
            .iter()
            .any(|k| finding.get(k).is_some())
            || finding
                .get("dynamic")
                .is_some_and(|v| v != "no_proof" && v != "unsupported")
        {
            return Err(Error::Contract(
                "task findings cannot supply dynamic evidence or Critical confirmation".into(),
            ));
        }
    }
    if array(&task["audit"], "probes")
        .iter()
        .any(|p| p.get("target").is_some() || p.get("required").is_some())
    {
        return Err(Error::Contract(
            "task probes cannot supply derived target/required fields".into(),
        ));
    }
    let skill = text(&task, "skill_path");
    if skill.is_empty() {
        return Err(Error::Contract("skill_path is required".into()));
    }
    let skill_path = base.join(skill);
    task["skill_path"] = skill_path
        .canonicalize()
        .map_err(|error| Error::at_path("task.skill.resolve", &skill_path, error))?
        .to_string_lossy()
        .into_owned()
        .into();
    // task-v1 omits empty derived/default probe values in the existing public producer.
    if let Some(probes) = task["audit"]["probes"].as_array_mut() {
        for probe in probes {
            if let Some(fields) = probe.as_object_mut() {
                fields.entry("finding_ids").or_insert(json!([]));
                fields.entry("input_path").or_insert(json!(""));
            }
        }
    }
    redaction::value(&mut task)?;
    let schema = contracts::schema("audit")?;
    contracts::normalize(&mut task["audit"], &schema, &schema, "task.audit")?;
    if let Some(probes) = task["audit"]["probes"].as_array_mut() {
        for probe in probes {
            fixtures::materialize(probe)?;
        }
    }
    Ok(task)
}

/// Execute a validated task using fixed installed materials and preserve partial reports on failure.
///
/// # Errors
/// Input/content binding failures return errors; execution failures are retained in a nonzero outcome.
pub fn run(
    task: &Value,
    output: &Path,
    prefix: &Path,
    cancelled: Arc<AtomicBool>,
) -> Result<RunOutcome> {
    let started = Instant::now();
    let skill = Path::new(text(task, "skill_path"));
    let mut snapshot = Snapshot::create(skill)?;
    if snapshot.hash != text(task, "content_hash") {
        return Err(Error::Contract(
            "Skill content changed or uses the old digest scheme; create a new task with plan"
                .into(),
        ));
    }
    let runtime = worker::Runtime::installed(prefix)?;
    let judge = judge(cancelled)?;
    snapshot.validate_paths(skill, &judge.model)?;
    let signals = scanners::scan(&mut snapshot.files, &judge.model, true)?;
    let source_redaction = signals["source_redaction"].clone();
    let catalog = catalog(&snapshot.files)?;
    let mut audit = task["audit"].clone();
    redaction::redact_value(&mut audit, &judge.model)?;
    contracts::validate_claims(&audit, &catalog, true)?;
    contracts::validate_probes(&mut audit, &snapshot.files, false)?;
    for probe in array(&audit, "probes") {
        redaction::validate_source_values(probe, &source_redaction)?;
    }
    private_json(&output.join("source-redaction.json"), &source_redaction)?;
    private_json(
        &output.join("task.json"),
        &contracts::public_task(skill, &snapshot.hash, &audit),
    )?;
    let failures = planning_failures(&audit);
    let mut limitations = failures.clone();
    limitations.extend(
        snapshot.omitted.iter().map(|entry| {
            format!("source review omitted {entry}; execution digest still includes it")
        }),
    );
    let mut run = Run {
        task,
        output,
        skill,
        started,
        snapshot,
        judge,
        runtime,
        catalog,
        source_redaction,
        probes: array(&audit, "probes").to_vec(),
        findings: array(&audit, "findings").to_vec(),
        gaps: array(&audit, "control_gaps").to_vec(),
        audit,
        failures,
        limitations,
        observed: Vec::new(),
        environment: json!({}),
        final_value: json!({}),
        final_available: false,
        execution_count: 0,
    };
    if let Some(missing) = run.audit["missing_implementation"].as_object() {
        run.failures.extend(
            missing
                .keys()
                .map(|path| format!("missing implementation: {path}")),
        );
    }
    if let Err(error) = run.execute() {
        if matches!(error, Error::UnsafeEvidence { .. }) {
            return Err(error);
        }
        run.failures
            .push(run.judge.model.redact_diagnostic(&error.to_string()));
    }
    run.adjudicate_and_finish()
}

fn planning_failures(audit: &Value) -> Vec<String> {
    audit["planning_errors"]
        .as_object()
        .into_iter()
        .flat_map(|errors| errors.iter())
        .map(|(id, reason)| {
            format!(
                "probe planning/{id}: {}",
                reason.as_str().unwrap_or_default()
            )
        })
        .collect()
}

struct Run<'a> {
    task: &'a Value,
    output: &'a Path,
    skill: &'a Path,
    started: Instant,
    snapshot: Snapshot,
    judge: judge::Judge,
    runtime: worker::Runtime,
    catalog: BTreeMap<String, String>,
    source_redaction: Value,
    audit: Value,
    probes: Vec<Value>,
    findings: Vec<Value>,
    gaps: Vec<Value>,
    failures: Vec<String>,
    limitations: Vec<String>,
    observed: Vec<Value>,
    environment: Value,
    final_value: Value,
    final_available: bool,
    execution_count: u64,
}

impl Run<'_> {
    fn execute(&mut self) -> Result<()> {
        self.environment = self.runtime.invoke(worker::Invocation {
            operation: "environment",
            snapshot: self.snapshot.path(),
            probe: None,
            root: &self.output.join("environment"),
            model: &self.judge.model,
            deadline: self
                .judge
                .deadline
                .min(Instant::now() + Duration::from_secs(60)),
            cancelled: &self.judge.cancelled,
        })?;
        private_json(&self.output.join("environment.json"), &self.environment)?;
        for probe in self.probes.clone() {
            if self.judge.cancelled.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if self.execution_count >= 24
                || Instant::now() + Duration::from_secs(90) >= self.judge.deadline
            {
                return Err(Error::Deadline);
            }
            let item = self.execute_probe(&probe)?;
            if let Some(entries) = item["catalog"].as_object() {
                for (id, value) in entries {
                    if id.starts_with("source:") || self.catalog.contains_key(id) {
                        return Err(Error::Runtime(
                            "worker evidence overwrote an existing reference".into(),
                        ));
                    }
                    let text = value.as_str().ok_or_else(|| {
                        Error::Runtime("evidence catalog value is not text".into())
                    })?;
                    self.catalog
                        .insert(id.clone(), self.judge.model.redact(text));
                }
            }
            let critical = flag(&item, "critical_canary");
            self.observed.push(item);
            if critical {
                break;
            }
        }
        Ok(())
    }

    fn execute_probe(&mut self, probe: &Value) -> Result<Value> {
        let mut attempts = Vec::new();
        for attempt in 1..=if flag(probe, "required") { 2 } else { 1 } {
            if self.execution_count >= 24 {
                break;
            }
            self.execution_count += 1;
            let root = self
                .output
                .join("probes")
                .join(text(probe, "id"))
                .join(format!("attempt-{attempt}"));
            eprintln!(
                "[skill-score] Harbor {}, attempt {attempt}",
                text(probe, "id")
            );
            let deadline = self
                .judge
                .deadline
                .checked_sub(Duration::from_secs(90))
                .ok_or(Error::Deadline)?
                .min(
                    Instant::now()
                        + Duration::from_secs(600 * array(probe, "turns").len() as u64 + 180),
                );
            let item = match self.runtime.invoke(worker::Invocation {
                operation: "run",
                snapshot: self.snapshot.path(),
                probe: Some(probe),
                root: &root,
                model: &self.judge.model,
                deadline,
                cancelled: &self.judge.cancelled,
            }) {
                Ok(item) => item,
                Err(error) => {
                    if matches!(
                        error,
                        Error::Cancelled | Error::Deadline | Error::UnsafeEvidence { .. }
                    ) {
                        return Err(error);
                    }
                    json!({"probe_id":text(probe,"id"),"complete":false,"error":self.judge.model.redact_diagnostic(&error.to_string())})
                }
            };
            let stop = flag(&item, "complete")
                || flag(&item, "critical_canary")
                || text(&item, "failure_kind") == "setup";
            private_json(&root.join("evidence.json"), &item)?;
            attempts.push(item);
            if stop {
                break;
            }
        }
        if attempts.is_empty() {
            return Err(Error::Deadline);
        }
        Ok(evidence::merge_attempts(&attempts))
    }

    fn adjudicate_and_finish(mut self) -> Result<RunOutcome> {
        if let Err(error) = self.adjudicate() {
            self.failures
                .push(self.judge.model.redact_diagnostic(&error.to_string()));
            evidence::apply_dynamic(&mut self.findings, &self.probes, &self.observed);
        }
        self.finish()
    }

    fn adjudicate(&mut self) -> Result<()> {
        if self.judge.cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut dynamic: BTreeMap<_, _> = self
            .catalog
            .iter()
            .filter(|(r, _)| !r.starts_with("source:"))
            .map(|(r, t)| (r.clone(), t.clone()))
            .collect();
        let signals = scanners::scan(&mut dynamic, &self.judge.model, false)?;
        self.catalog.extend(dynamic);
        private_json(&self.output.join("runtime-signals.json"), &signals)?;
        let required = array(&signals, "signals")
            .iter()
            .map(|s| text(s, "ref").to_owned())
            .chain(
                array(&self.audit, "findings")
                    .iter()
                    .chain(self.gaps.iter())
                    .flat_map(|f| {
                        strings(f, "refs")
                            .into_iter()
                            .chain(strings(f, "control_refs"))
                            .map(str::to_owned)
                    }),
            )
            .chain(self.observed.iter().flat_map(|e| {
                strings(e, "entry_exit_refs")
                    .into_iter()
                    .chain(strings(e, "entry_failure_refs"))
                    .map(str::to_owned)
            }))
            .collect();
        let (model_catalog, catalog_stats) = catalog::judge_view(&self.catalog, &required)?;
        private_json(
            &self.output.join("judge-catalog.json"),
            &json!({"catalog":model_catalog,"stats":catalog_stats}),
        )?;
        let coverage = coverage(&self.observed);
        self.final_value = self.judge.call("final", &json!({"catalog":model_catalog,"source_redaction":self.source_redaction,"audit":self.audit,"runtime_signals":signals["signals"],"runtime_tools":self.environment,"hygiene_checks":contracts::rules()?,"coverage":coverage}), &self.output.join("adjudication.json"))?;
        self.findings = array(&self.final_value, "findings").to_vec();
        self.gaps = array(&self.final_value, "control_gaps").to_vec();
        evidence::apply_dynamic(&mut self.findings, &self.probes, &self.observed);
        self.review_critical(&model_catalog)?;
        self.final_available = true;
        Ok(())
    }

    fn review_critical(&mut self, model_catalog: &BTreeMap<String, String>) -> Result<()> {
        let critical_ids = critical_ids(&self.audit, &self.findings);
        if critical_ids.is_empty() || !evidence::canary_findings(&self.observed).is_empty() {
            return Ok(());
        }
        let claims = self.findings.iter().filter(|f| critical_ids.contains(text(f, "id")))
            .map(|f| json!({"id":f["id"],"entry":f["entry"],"operation":f["operation"],"boundary":f["boundary"],"refs":f["refs"]})).collect::<Vec<_>>();
        let independent = self.judge.call(
            "critical",
            &json!({"catalog":model_catalog,"source_redaction":self.source_redaction,"claims":claims,"coverage":coverage(&self.observed)}),
            &self.output.join("critical.json"),
        )?;
        apply_critical_review(&mut self.findings, &critical_ids, &independent);
        Ok(())
    }

    fn report_data(&mut self) -> Result<Value> {
        let baseline = self
            .observed
            .iter()
            .find(|e| text(e, "probe_id") == "baseline")
            .cloned()
            .unwrap_or(json!({}));
        let (assessable, reason) = self.probes.first().map_or_else(
            || (false, "no baseline was executed".into()),
            |probe| {
                evidence::baseline_eligibility(&self.final_value, probe, &baseline, &self.catalog)
            },
        );
        let baseline_task = json!({"completed":flag(&self.final_value,"baseline_completed"),"failure_kind":self.final_value["baseline_failure"],"assessable":assessable,"reason":self.final_value["baseline_reason"],"refs":self.final_value["baseline_refs"],"eligibility_reason":reason});
        if !assessable {
            self.limitations.push(format!("baseline: {reason}"));
        }
        let data = json!({"schema_version":"agent-sec-skill-score/report-v1","content_hash":self.task["content_hash"],"model":self.judge.model.model(),"judge_model":self.judge.model.model(),"skill":self.skill.file_name().and_then(|s|s.to_str()),"findings":self.findings,"control_gaps":self.gaps,"probes":self.probes,"coverage":coverage(&self.observed),"failures":self.failures,"limitations":self.limitations,"planning_notes":self.audit["limitations"],"audit_omitted_sources":self.audit["omitted_sources"],"final_available":self.final_available,"baseline_task":baseline_task,"task_results":self.final_value["task_results"].as_array().cloned().unwrap_or_default(),"task_checks":self.final_value["task_checks"],"hygiene_review":self.final_value["hygiene_review"],"corrections":self.final_value["corrections"],"environment":self.environment,"elapsed_seconds":self.started.elapsed().as_secs_f64(),"risk_catalog":serde_json::from_str::<Value>(include_str!("../assets/risk-provenance.json"))?,"usage":judge_usage(&self.judge.calls,self.execution_count)});
        Ok(data)
    }

    fn score_report(&self, data: &mut Value) -> Result<()> {
        let score = score::score_findings(&self.findings, &self.failures, &self.gaps)?;
        if let (Some(data), Some(score)) = (data.as_object_mut(), score.as_object()) {
            data.extend(score.clone());
        }
        let neutral = self
            .findings
            .iter()
            .map(|finding| {
                let mut finding = finding.clone();
                finding["dynamic"] = "no_proof".into();
                finding
            })
            .collect::<Vec<_>>();
        let neutral_score = score::score_findings(&neutral, &self.failures, &self.gaps)?;
        data["dynamic_diagnostic"] = json!({"scope":"Final roots, severity and existence fixed; noncritical dynamic factor only. Not a causal experiment.","neutral_noncritical_score":neutral_score["noncritical_score"],"observed_noncritical_score":data["noncritical_score"],"delta":((data["noncritical_score"].as_f64().unwrap_or(0.0)-neutral_score["noncritical_score"].as_f64().unwrap_or(0.0))*10.0).round()/10.0,"critical_gate":matches!(text(data,"decision"),"blocked"|"needs_review")});
        for (field, source) in [
            ("runtime_prompt_tokens", "total_prompt_tokens"),
            ("runtime_completion_tokens", "total_completion_tokens"),
        ] {
            data["usage"][field] = self
                .observed
                .iter()
                .flat_map(|item| array(item, "attempts"))
                .map(|attempt| attempt["usage"][source].as_u64().unwrap_or(0))
                .sum::<u64>()
                .into();
        }
        Ok(())
    }

    fn finish(mut self) -> Result<RunOutcome> {
        let canary = evidence::canary_findings(&self.observed);
        if !canary.is_empty() {
            self.findings.retain(|f| text(f, "id") != "critical-canary");
            self.findings.extend(canary);
        }
        let dynamic_complete = dynamic_complete(&self.audit, &self.observed);
        if !dynamic_complete {
            self.limitations.push("dynamic execution is incomplete; unexecuted or incomplete probes are not successful validation".into());
        }
        let mut data = self.report_data()?;
        data["planning_errors"] = self.audit["planning_errors"].clone();
        self.score_report(&mut data)?;
        private_json(
            &self.output.join("events.json"),
            &json!(
                self.observed
                    .iter()
                    .flat_map(|item| array(item, "events"))
                    .collect::<Vec<_>>()
            ),
        )?;
        data["dynamic_complete"] = dynamic_complete.into();
        data["cleanup_status"] = self.runtime.cleanup_status.clone().into();
        data["resources"] = json!(self.runtime.resources);
        private_json(
            &self.output.join("evidence-catalog.json"),
            &json!(self.catalog),
        )?;
        // Snapshot cancellation once after evidence writes, as we commit the final report.
        let exit_code = finalize_execution(&mut data, self.judge.cancelled.load(Ordering::Relaxed));
        private_json(&self.output.join("internal-report.json"), &data)?;
        let mut report = evidence::public_report(&data, &self.catalog);
        for field in [
            "execution_status",
            "dynamic_complete",
            "cleanup_status",
            "failure_stage",
            "planning_errors",
        ] {
            if let Some(value) = data.get(field) {
                report[field] = value.clone();
            }
        }
        redaction::redact_value(&mut report, &self.judge.model)?;
        Ok(RunOutcome { report, exit_code })
    }
}

fn finalize_execution(data: &mut Value, cancelled: bool) -> u8 {
    let failure_stage = if cancelled {
        "cancellation"
    } else if data["planning_errors"]
        .as_object()
        .is_some_and(|errors| !errors.is_empty())
    {
        "planning"
    } else if !flag(data, "dynamic_complete") {
        "execution"
    } else if !flag(data, "final_available") {
        "adjudication"
    } else if text(data, "cleanup_status") != "complete" {
        "cleanup"
    } else if !array(data, "failures").is_empty() {
        "assessment"
    } else {
        data["execution_status"] = "complete".into();
        return 0;
    };
    data["failure_stage"] = failure_stage.into();
    data["execution_status"] = if cancelled {
        "cancelled"
    } else if array(data, "coverage").is_empty() {
        "failed"
    } else {
        "incomplete"
    }
    .into();
    1
}

fn critical_ids(audit: &Value, findings: &[Value]) -> BTreeSet<String> {
    array(audit, "findings")
        .iter()
        .chain(findings)
        .filter(|f| text(f, "severity") == "Critical" && text(f, "scope") == "skill")
        .map(|f| text(f, "id").to_owned())
        .collect()
}

fn apply_critical_review(findings: &mut [Value], ids: &BTreeSet<String>, independent: &Value) {
    for finding in findings.iter_mut().filter(|f| ids.contains(text(f, "id"))) {
        let reviewed = array(independent, "findings").iter().find(|f| {
            text(f, "id") == text(finding, "id")
                && text(f, "severity") == "Critical"
                && text(f, "scope") == "skill"
                && matches!(text(f, "status"), "material_risk" | "unresolved")
        });
        let confirmed = reviewed.is_some_and(|r| {
            let refs = strings(finding, "refs");
            strings(r, "refs").iter().any(|id| refs.contains(id))
                && text(r, "status") == "material_risk"
                && text(finding, "severity") == "Critical"
                && text(finding, "scope") == "skill"
                && text(finding, "status") == "material_risk"
        });
        finding["critical_confirmed"] = confirmed.into();
        if let Some(challenge) = reviewed
            && !confirmed
        {
            finding["reason"] = format!(
                "Primary verdict ({}/{}/{}): {} Independent Skill Critical verdict: {}",
                text(finding, "scope"),
                text(finding, "severity"),
                text(finding, "status"),
                text(finding, "reason"),
                text(challenge, "reason")
            )
            .into();
            let mut refs = strings(finding, "refs");
            for reference in strings(challenge, "refs") {
                if !refs.contains(&reference) {
                    refs.push(reference);
                }
            }
            finding["refs"] = json!(refs);
            // An unresolved Skill attribution must reach the Critical gate and the public report.
            finding["scope"] = "skill".into();
            finding["severity"] = "Critical".into();
            finding["status"] = "unresolved".into();
        }
    }
}

fn judge_usage(calls: &[Value], runtime_attempts: u64) -> Value {
    let reported = calls
        .iter()
        .filter_map(|call| {
            Some((
                call["usage"]["input_tokens"].as_u64()?,
                call["usage"]["output_tokens"].as_u64()?,
            ))
        })
        .collect::<Vec<_>>();
    json!({"judge_calls":calls.len(),
        "judge_tokens":reported.iter().map(|(input, output)|input + output).sum::<u64>(),
        "judge_tokens_scope":"reported_usage_only",
        "judge_usage_complete":reported.len() == calls.len(),
        "judge_calls_without_usage":calls.len() - reported.len(),
        "runtime_attempts":runtime_attempts})
}

fn dynamic_complete(audit: &Value, observed: &[Value]) -> bool {
    !array(audit, "probes").is_empty()
        && audit["planning_errors"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty)
        && array(audit, "probes").iter().all(|probe| {
            observed
                .iter()
                .any(|e| text(e, "probe_id") == text(probe, "id") && flag(e, "complete"))
        })
}

fn coverage(evidence: &[Value]) -> Vec<Value> {
    evidence
        .iter()
        .map(|item| {
            let mut item = item.clone();
            if let Some(object) = item.as_object_mut() {
                object.remove("catalog");
                object.remove("events");
            }
            item
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_at_report_commit_overrides_completed_execution_and_adjudication() {
        let completed = json!({"dynamic_complete":true,"final_available":true,"cleanup_status":"complete",
            "failures":[],"coverage":[{"probe_id":"baseline","complete":true}]});
        let mut normal = completed.clone();
        assert_eq!(finalize_execution(&mut normal, false), 0);
        assert_eq!(normal["execution_status"], "complete");
        assert!(normal.get("failure_stage").is_none());
        let mut late = completed;
        assert_eq!(finalize_execution(&mut late, true), 1);
        assert_eq!(late["execution_status"], "cancelled");
        assert_eq!(late["failure_stage"], "cancellation");
    }

    #[test]
    fn independent_critical_keeps_disputed_skill_attribution_visible_and_unranked() -> Result<()> {
        let source = "source:SKILL.md:1";
        let critical = json!({"id":"transfer","title":"Untrusted destination","severity":"Critical",
            "scope":"skill","status":"material_risk","entry":"run","operation":"transfer",
            "boundary":"network","dynamic":"no_proof","reason":"Skill selects the destination",
            "refs":[source]});
        let audit = json!({"findings":[critical]});
        let mut primary = critical.clone();
        primary["scope"] = "environment".into();
        primary["severity"] = "Low".into();
        primary["reason"] = "Runtime selects the destination".into();
        for (reference, expected_evidence) in [(source, 1), ("source:SKILL.md:2", 2)] {
            let mut challenge = critical.clone();
            challenge["refs"] = json!([reference]);
            let mut findings = [primary.clone()];
            let ids = critical_ids(&audit, &findings);
            apply_critical_review(&mut findings, &ids, &json!({"findings":[challenge]}));
            let mut data = score::score_findings(&findings, &[], &[])?;
            assert_eq!(data["rankable"], false);
            data["findings"] = json!(findings);
            let catalog = BTreeMap::from([
                (source.into(), "send".into()),
                (reference.into(), "send".into()),
            ]);
            let report = evidence::public_report(&data, &catalog);
            assert_eq!(report["decision"], "needs_review");
            assert!(report["score"].is_null());
            let public = &report["findings"][0];
            assert_eq!(public["status"], "unresolved");
            assert_eq!(array(public, "evidence").len(), expected_evidence);
            assert!(text(public, "reason").contains("environment"));
            assert!(text(public, "reason").contains("Skill selects the destination"));
        }
        let mut findings = [critical];
        let ids = critical_ids(&audit, &findings);
        apply_critical_review(&mut findings, &ids, &audit);
        assert_eq!(
            score::score_findings(&findings, &[], &[])?["decision"],
            "blocked"
        );
        Ok(())
    }

    #[test]
    fn missing_judge_usage_is_reported_as_unknown_not_zero_cost() {
        let success = json!({"usage":{"input_tokens":7,"output_tokens":3}});
        let usage = judge_usage(std::slice::from_ref(&success), 2);
        assert_eq!(usage["judge_usage_complete"], true);
        assert_eq!(usage["judge_calls_without_usage"], 0);
        let usage = judge_usage(&[success, json!({"code":"timeout"})], 2);
        assert_eq!(usage["judge_calls"], 2);
        assert_eq!(usage["judge_tokens"], 10);
        assert_eq!(usage["judge_tokens_scope"], "reported_usage_only");
        assert_eq!(usage["judge_usage_complete"], false);
        assert_eq!(usage["judge_calls_without_usage"], 1);
        assert_eq!(usage["runtime_attempts"], 2);
    }

    fn planned_audit() -> Value {
        json!({"objective":"Describe the supplied Skill","allowed_boundary":"Read SKILL.md only","findings":[],"probes":[{"id":"baseline","finding_ids":[],"template":"baseline","turns":["Use the Skill to describe its purpose."],"entry_path":"SKILL.md","activation":"Read the Skill instructions","expected_result":"The Skill purpose is described","input_path":""}]})
    }

    #[test]
    fn public_task_roundtrip_restores_defaults_and_run_requires_optional_component() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let skill = directory.path().join("skill");
        std::fs::create_dir(&skill)?;
        std::fs::write(skill.join("SKILL.md"), "# Example\nDescribe this Skill.")?;
        let snapshot = Snapshot::create(&skill)?;
        let mut audit = planned_audit();
        let schema = contracts::schema("audit")?;
        contracts::normalize(&mut audit, &schema, &schema, "audit")?;
        contracts::validate_claims(&audit, &catalog(&snapshot.files)?, true)?;
        contracts::validate_probes(&mut audit, &snapshot.files, true)?;
        let mut task = contracts::public_task(&skill, &snapshot.hash, &audit);
        assert!(task["audit"].get("hygiene_review").is_none());
        let probe = task["audit"]["probes"][0]
            .as_object_mut()
            .ok_or_else(|| Error::Contract("fixture probe missing".into()))?;
        probe.remove("finding_ids");
        probe.remove("input_path");
        let mut parsed = parse_task(task, directory.path())?;
        contracts::validate_probes(&mut parsed["audit"], &snapshot.files, false)?;
        assert_eq!(parsed["audit"]["probes"][0]["finding_ids"], json!([]));
        assert_eq!(parsed["audit"]["probes"][0]["required"], true);
        let outcome = run(
            &parsed,
            directory.path(),
            directory.path(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(outcome,Err(Error::Runtime(message)) if message.contains("sec-core-skill-score"))
        );
        Ok(())
    }

    #[test]
    fn a_successful_baseline_cannot_hide_failed_probe_planning() {
        let mut audit = planned_audit();
        let evidence = [json!({"probe_id":"baseline","complete":true})];
        assert!(dynamic_complete(&audit, &evidence));
        audit["planning_errors"] = json!({"probe-risk":"prerequisite unavailable"});
        assert!(!dynamic_complete(&audit, &evidence));
    }

    #[test]
    fn edited_private_verdict_or_changed_tree_cannot_be_reused() -> Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("SKILL.md"), "# Example")?;
        let snapshot = Snapshot::create(directory.path())?;
        let mut task = contracts::public_task(directory.path(), &snapshot.hash, &planned_audit());
        task["audit"]["probes"][0]["required"] = true.into();
        assert!(matches!(
            parse_task(task, directory.path()),
            Err(Error::Contract(_))
        ));
        let task = parse_task(
            contracts::public_task(directory.path(), &snapshot.hash, &planned_audit()),
            directory.path(),
        )?;
        std::fs::write(directory.path().join("binary"), [0, 42])?;
        let outcome = run(
            &task,
            directory.path(),
            directory.path(),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(
            matches!(outcome,Err(Error::Contract(message)) if message.contains("new task with plan"))
        );
        Ok(())
    }
}
