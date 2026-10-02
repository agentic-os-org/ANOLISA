//! Transport-agnostic mechanics behind the reuse-label endpoints, shared by the
//! Linux and macOS handlers.
//!
//! Everything here works on stores and plain data — no actix types — for the
//! same reason [`crate::preferences::api`] does: the two servers must not drift
//! apart in what a label means, and a rule that lives in one handler inevitably
//! does.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use agentsight_atif::AtifTrajectory;
use agentsight_opt::llm::LlmClient;
use agentsight_trajectory_collector::TrajectoryStore;

use super::label::{ConfirmState, LabelAction, SessionLabel, TrajectoryIdentity, TrajectoryLabel};
use super::store::{LabelFilter, ReuseStore, ReuseStoreError, RuleOverrideStat};
use super::summarize::summarize_trajectory;
use super::triage::{TriageConfig, triage};

/// Trajectories examined by one triage request when the caller does not say.
pub const DEFAULT_TRIAGE_LIMIT: i64 = 500;
/// Hard ceiling on one triage request.
///
/// Each trajectory is read and parsed individually, so a large run is slow
/// rather than memory-hungry; the cap exists to keep a single HTTP request from
/// running unbounded, not to protect memory.
pub const MAX_TRIAGE_LIMIT: i64 = 5_000;
/// Label rows returned by one listing request when the caller does not say.
pub const DEFAULT_SESSIONS_LIMIT: i64 = 200;
/// Hard ceiling on one listing request.
pub const MAX_SESSIONS_LIMIT: i64 = 2_000;

/// Failure modes the handlers need to tell apart.
#[derive(Debug, thiserror::Error)]
pub enum ReuseApiError {
    /// No `trajectories.db` yet — collection has not run. A configuration
    /// state, not a fault, so handlers report it distinctly from a real error.
    #[error("trajectories.db not found; run `agentsight trace` with trajectory collection enabled")]
    TrajectoriesUnavailable,
    /// `reuse.db` could not be opened, which means labels cannot be served at
    /// all. Kept separate from a per-request failure for the same reason.
    #[error("reuse.db unavailable: {0}")]
    ReuseUnavailable(String),
    #[error("label store: {0}")]
    Store(#[from] ReuseStoreError),
    #[error("trajectory store: {0}")]
    Trajectories(String),
    #[error("unknown {field} {value:?}")]
    BadParameter { field: &'static str, value: String },
    /// Kept apart from [`Self::BadParameter`] so the caller learns whether to
    /// add the field or correct it.
    #[error("missing required {field}")]
    MissingParameter { field: &'static str },
}

type Result<T> = std::result::Result<T, ReuseApiError>;

impl ReuseApiError {
    /// HTTP status this condition warrants.
    ///
    /// Decided here rather than in each handler so the two servers cannot answer
    /// differently for the same cause — a client that has to special-case the
    /// platform has no contract at all.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::TrajectoriesUnavailable => 404,
            // The caller named a trajectory that has not been triaged. That is
            // their mistake, not a fault here, and reporting it as a server error
            // leaves them unable to tell a wrong id from a broken database.
            Self::Store(ReuseStoreError::UnknownSession(_)) => 404,
            Self::ReuseUnavailable(_) => 503,
            Self::BadParameter { .. } | Self::MissingParameter { .. } => 400,
            Self::Store(_) | Self::Trajectories(_) => 500,
        }
    }

    /// Stable machine-readable code for the response body.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TrajectoriesUnavailable => "trajectories_unavailable",
            Self::ReuseUnavailable(_) => "reuse_unavailable",
            Self::BadParameter { .. } => "bad_parameter",
            Self::MissingParameter { .. } => "missing_parameter",
            Self::Store(ReuseStoreError::UnknownSession(_)) => "session_not_labelled",
            Self::Store(_) => "label_store_error",
            Self::Trajectories(_) => "trajectory_store_error",
        }
    }

    /// Body both handlers return, so the shape is one thing too.
    pub fn body(&self) -> serde_json::Value {
        serde_json::json!({ "error": self.code(), "message": self.to_string() })
    }
}

// ─── Requests ────────────────────────────────────────────────────────────────

/// Query of `POST /api/reuse/triage`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TriageQuery {
    /// Label one trajectory instead of a batch. Useful right after a session
    /// ends, and the only way to force a specific one to the front.
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Body of `POST /api/reuse/sessions/{id}/label`.
///
/// `confirm` endorses the automatic verdict as it stands; `override` replaces
/// it. Both are recorded as human decisions, because a person who read the
/// trajectory and agreed has told us something the rules could not.
#[derive(Debug, Clone, Deserialize)]
pub struct LabelDecisionRequest {
    /// `confirm` or `override`.
    pub action: String,
    /// Required for `override`; ignored for `confirm`.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// Who decided. Falls back to `dashboard` when the caller does not say, so
    /// the audit row is never blank.
    #[serde(default)]
    pub decided_by: Option<String>,
}

/// Body of `POST /api/reuse/sessions/labels:batch-confirm`.
#[derive(Debug, Clone, Deserialize)]
pub struct BatchConfirmRequest {
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub decided_by: Option<String>,
}

/// Body of `POST /api/reuse/judge`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct JudgeQuery {
    /// Judge these trajectories. When absent, picks unjudged ones the rules
    /// could not place.
    #[serde(default)]
    pub session_ids: Option<Vec<String>>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Outcome of one judging run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JudgeReport {
    pub examined: usize,
    pub judged: usize,
    /// Already judged, or the trajectory could not be read.
    pub skipped: usize,
    /// Calls that failed. Reported rather than aborting the run, so one bad
    /// response does not waste the requests already paid for.
    pub failed: usize,
    /// Verdicts reduced to `unknown` for citing no step.
    pub downgraded: usize,
    pub judged_good: usize,
    pub judged_bad: usize,
    pub judged_unclear: usize,
}

/// Query of `GET /api/reuse/sessions`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SessionsQuery {
    /// Comma-separated effective labels to keep, e.g. `bad,useless`.
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub confirm_state: Option<String>,
    /// Only rows whose automatic verdict drifted away from a human decision.
    #[serde(default)]
    pub changed_since_decision: Option<bool>,
    #[serde(default)]
    pub limit: Option<i64>,
}

// ─── Responses ───────────────────────────────────────────────────────────────

/// What one triage run did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TriageReport {
    /// Trajectories considered, including the ones skipped.
    pub examined: usize,
    /// Labels written or refreshed.
    pub labelled: usize,
    /// Already labelled by this rule version from identical content, so the
    /// result could only have been the same.
    pub unchanged: usize,
    /// Rows whose `atif_json` would not parse.
    pub unparsable: usize,
    /// Rows that vanished between listing and reading.
    pub missing: usize,
    /// Automatic verdicts produced, by label. Counts the automatic result, not
    /// the effective one, so a human decision does not hide what the rules said.
    pub auto_good: usize,
    pub auto_bad: usize,
    pub auto_useless: usize,
    pub auto_unknown: usize,
    /// Rows where a human decision is in force and disagrees with the automatic
    /// verdict just computed.
    pub human_overrides_in_force: usize,
    /// Rule version the run recorded; see [`TriageConfig::version`].
    pub triage_version: String,
    /// True when `limit` cut the run short, so the caller knows to continue.
    pub truncated: bool,
}

/// One label row as the dashboard sees it.
///
/// `effective_label` is materialised rather than left to the client: it is a
/// function of two columns, and every consumer re-deriving that rule is how
/// they start disagreeing about which label is in force.
#[derive(Debug, Clone, Serialize)]
pub struct SessionLabelView {
    pub session_id: String,
    /// The opening request, the closest thing to a conversation's title. Absent
    /// for tool-only transcripts, where the client falls back to the id.
    pub title: Option<String>,
    /// Working directory the session ran in.
    pub project: String,
    /// Which product wrote it: `qoder`, `claude-code`, `codex`.
    pub source: String,
    pub agent_name: String,
    pub started_at: Option<String>,
    pub is_subagent: bool,
    pub effective_label: String,
    pub confirm_state: String,
    /// Whether a person actually signed off. Distinct from having a label:
    /// an unconfirmed automatic label is in force but is not an endorsement.
    pub human_backed: bool,
    pub auto_label: String,
    pub auto_reason: String,
    pub auto_rules: Vec<String>,
    pub human_label: Option<String>,
    pub human_reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at_ns: Option<i64>,
    pub auto_changed_since_decision: bool,
    /// The model judge's verdict, absent until second-level labelling runs.
    pub llm_label: Option<String>,
    pub llm_reason: Option<String>,
    pub llm_cited_steps: Vec<usize>,
    pub llm_downgraded: bool,
    pub n_steps: usize,
    pub n_user_turns: usize,
    pub n_tool_calls: usize,
    pub max_agent_len: usize,
    pub n_findings: usize,
    pub triage_version: String,
    pub updated_at_ns: i64,
}

impl From<&SessionLabel> for SessionLabelView {
    fn from(label: &SessionLabel) -> Self {
        Self {
            session_id: label.session_id.clone(),
            title: label.identity.title.clone(),
            project: label.identity.project.clone(),
            source: label.identity.source.clone(),
            agent_name: label.identity.agent_name.clone(),
            started_at: label.identity.started_at.clone(),
            is_subagent: label.identity.is_subagent,
            effective_label: label.effective_label().as_str().to_string(),
            confirm_state: label.confirm_state.as_str().to_string(),
            human_backed: label.is_human_backed(),
            auto_label: label.auto_label.as_str().to_string(),
            auto_reason: label.auto_reason.clone(),
            auto_rules: label.auto_rules.clone(),
            human_label: label.human_label.map(|l| l.as_str().to_string()),
            human_reason: label.human_reason.clone(),
            decided_by: label.decided_by.clone(),
            decided_at_ns: label.decided_at_ns,
            auto_changed_since_decision: label.auto_changed_since_decision,
            llm_label: label.llm_label.map(|l| l.as_str().to_string()),
            llm_reason: label.llm_reason.clone(),
            llm_cited_steps: label.llm_cited_steps.clone(),
            llm_downgraded: label.llm_downgraded,
            n_steps: label.metrics.n_steps,
            n_user_turns: label.metrics.n_user_turns,
            n_tool_calls: label.metrics.n_tool_calls,
            max_agent_len: label.metrics.max_agent_len,
            n_findings: label.n_findings,
            triage_version: label.triage_version.clone(),
            updated_at_ns: label.updated_at_ns,
        }
    }
}

// ─── Operations ──────────────────────────────────────────────────────────────

/// Digest of the ATIF document a label was derived from.
///
/// Recorded so a re-collected trajectory recomputes its automatic verdict. The
/// digest is over the stored JSON rather than the parsed document: a change the
/// parser ignores today may matter to a later rule, and the cheap answer is to
/// treat any byte change as a change.
pub fn content_hash(atif_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(atif_json.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn clamp(requested: Option<i64>, default: i64, max: i64) -> i64 {
    match requested {
        Some(n) if n > 0 => n.min(max),
        _ => default,
    }
}

/// Labels trajectories and records the automatic verdicts.
///
/// Reads one trajectory at a time rather than fetching them in bulk:
/// `list_recent_atif_jsons` holds the store's mutex while collecting every
/// matched document, which its own documentation warns can be tens of
/// megabytes. A batch job has no reason to pay that.
///
/// # Errors
/// Returns [`ReuseApiError::TrajectoriesUnavailable`] when collection has not
/// run, or a store error on SQL failure.
pub fn run_triage(
    trajectories: Option<&TrajectoryStore>,
    labels: &ReuseStore,
    query: &TriageQuery,
    config: &TriageConfig,
) -> Result<TriageReport> {
    let trajectories = trajectories.ok_or(ReuseApiError::TrajectoriesUnavailable)?;
    let limit = clamp(query.limit, DEFAULT_TRIAGE_LIMIT, MAX_TRIAGE_LIMIT);
    let version = config.version();

    let session_ids: Vec<String> = match &query.session_id {
        Some(id) => vec![id.clone()],
        None => trajectories
            .list_summaries(None, None, None, limit)
            .map_err(|e| ReuseApiError::Trajectories(e.to_string()))?
            .into_iter()
            .map(|summary| summary.session_id)
            .collect(),
    };

    let mut report = TriageReport {
        triage_version: version.clone(),
        truncated: query.session_id.is_none() && session_ids.len() as i64 >= limit,
        ..TriageReport::default()
    };

    for session_id in session_ids {
        report.examined += 1;
        let Some(record) = trajectories
            .get(&session_id)
            .map_err(|e| ReuseApiError::Trajectories(e.to_string()))?
        else {
            report.missing += 1;
            continue;
        };
        let hash = content_hash(&record.atif_json);
        let identity = identity_from_record(&record);
        let Ok(doc) = serde_json::from_str::<AtifTrajectory>(&record.atif_json) else {
            report.unparsable += 1;
            continue;
        };

        // Same bytes and same rules can only produce the same verdict, so a
        // repeat run is free — this is what makes the endpoint safe to poll.
        // Identity is refreshed even on that path: it is denormalised display
        // data, so a row labelled before these columns existed would otherwise
        // keep showing a bare session id forever.
        if let Some(existing) = labels.get_label(&session_id)? {
            if existing.source_content_hash == hash && existing.triage_version == version {
                if existing.identity != identity {
                    labels.set_identity(&session_id, &identity)?;
                }
                report.unchanged += 1;
                tally_override(&existing, &mut report);
                continue;
            }
        }
        let outcome = triage(&doc, &summarize_trajectory(&doc), config);
        match outcome.label {
            TrajectoryLabel::Good => report.auto_good += 1,
            TrajectoryLabel::Bad => report.auto_bad += 1,
            TrajectoryLabel::Useless => report.auto_useless += 1,
            TrajectoryLabel::Unknown => report.auto_unknown += 1,
        }
        let stored = labels.upsert_auto_label(&session_id, identity, outcome, &hash, &version)?;
        report.labelled += 1;
        tally_override(&stored, &mut report);
    }

    Ok(report)
}

/// Counts a row whose human decision is in force and contradicts the rules.
fn tally_override(label: &SessionLabel, report: &mut TriageReport) {
    if label
        .human_label
        .is_some_and(|human| human != label.auto_label)
    {
        report.human_overrides_in_force += 1;
    }
}

/// Builds the display identity from a stored trajectory record.
///
/// The title is the first user message trimmed to one line; everything else is
/// copied as-is. Kept next to the triage loop because that is the only caller
/// and the mapping is not interesting enough to export.
fn identity_from_record(
    record: &agentsight_trajectory_collector::TrajectoryRecord,
) -> TrajectoryIdentity {
    TrajectoryIdentity {
        title: TrajectoryIdentity::title_from_message(record.first_user_message.as_deref()),
        project: record.project.clone(),
        source: record.source.clone(),
        agent_name: record.agent_name.clone(),
        started_at: record.start_time.clone(),
        is_subagent: record.is_subagent,
    }
}

/// Trajectories judged in one request when the caller does not say.
///
/// Each one is a paid call, so the default is small enough to be an
/// experiment rather than a bill.
pub const DEFAULT_JUDGE_LIMIT: i64 = 20;
/// Ceiling regardless of what the caller asks for.
pub const MAX_JUDGE_LIMIT: i64 = 200;

/// Asks the model to label trajectories the rules could not place.
///
/// Only `unknown` rows are candidates by default: that is where the
/// deterministic pass admitted it could not tell, and paying to re-confirm a
/// verdict the rules already reached with certainty buys nothing. A row already
/// carrying a judgement is skipped for the same reason.
///
/// One failed call does not end the run. The requests already issued have been
/// paid for, and their verdicts are worth keeping.
///
/// # Errors
/// Returns [`ReuseApiError::TrajectoriesUnavailable`] with no trajectory store,
/// or a store error while reading candidates.
pub async fn run_judgements(
    trajectories: &TrajectoryStore,
    labels: &ReuseStore,
    client: &LlmClient,
    query: &JudgeQuery,
) -> Result<JudgeReport> {
    let limit = clamp(query.limit, DEFAULT_JUDGE_LIMIT, MAX_JUDGE_LIMIT);

    let candidates: Vec<String> = match &query.session_ids {
        Some(ids) => ids.clone(),
        None => labels
            .list_labels(&LabelFilter::default())
            .map_err(ReuseApiError::Store)?
            .into_iter()
            .filter(|label| {
                label.llm_label.is_none()
                    && label.effective_label() == TrajectoryLabel::Unknown
                    && !label.is_human_backed()
            })
            .map(|label| label.session_id)
            .collect(),
    };

    let mut report = JudgeReport::default();
    for session_id in candidates.into_iter().take(limit as usize) {
        report.examined += 1;
        let Ok(Some(atif_json)) = trajectories.get_atif_json(&session_id) else {
            report.skipped += 1;
            continue;
        };
        let Ok(doc) = serde_json::from_str::<AtifTrajectory>(&atif_json) else {
            report.skipped += 1;
            continue;
        };
        match super::judge::judge_last_round(client, &doc).await {
            Ok(verdict) => {
                if verdict.downgraded {
                    report.downgraded += 1;
                }
                match verdict.label {
                    TrajectoryLabel::Good => report.judged_good += 1,
                    TrajectoryLabel::Bad => report.judged_bad += 1,
                    _ => report.judged_unclear += 1,
                }
                match labels.record_judgement(&session_id, &verdict) {
                    Ok(_) => report.judged += 1,
                    Err(error) => {
                        log::warn!("reuse: storing a judgement for {session_id} failed: {error}");
                        report.failed += 1;
                    }
                }
            }
            Err(error) => {
                log::warn!("reuse: judging {session_id} failed: {error}");
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

/// Default author recorded when the caller does not name one.
const DEFAULT_DECIDED_BY: &str = "dashboard";

/// Records a human decision on one trajectory's label.
///
/// This is the only way a trajectory reaches `bad` today. The deterministic
/// rules never accuse (see [`super::summarize`]), so an accusation is either a
/// person's call or, once the second-level path lands, a reviewed model verdict.
///
/// # Errors
/// Returns [`ReuseApiError::BadParameter`] for an unknown action or label, and
/// [`ReuseStoreError::UnknownSession`] via [`ReuseApiError::Store`] when the
/// trajectory has not been triaged — a human verdict with no automatic verdict
/// beside it cannot later be checked for rule misfires.
pub fn apply_label(
    labels: &ReuseStore,
    session_id: &str,
    request: &LabelDecisionRequest,
) -> Result<SessionLabelView> {
    let action = match request.action.trim() {
        "confirm" => LabelAction::Confirm,
        "override" => {
            let raw = request
                .label
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or(ReuseApiError::MissingParameter { field: "label" })?;
            let label = TrajectoryLabel::parse(raw).ok_or_else(|| ReuseApiError::BadParameter {
                field: "label",
                value: raw.to_string(),
            })?;
            // `unknown` means "the rules could not tell", which is not something
            // a person asserts about a trajectory they have read.
            if !TrajectoryLabel::user_assignable().contains(&label) {
                return Err(ReuseApiError::BadParameter {
                    field: "label",
                    value: raw.to_string(),
                });
            }
            LabelAction::Override(label)
        }
        other => {
            return Err(ReuseApiError::BadParameter {
                field: "action",
                value: other.to_string(),
            });
        }
    };

    let decided_by = request
        .decided_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_DECIDED_BY);
    let stored = labels.apply_decision(session_id, action, decided_by, request.reason.clone())?;
    Ok(SessionLabelView::from(&stored))
}

/// Confirms several labels at once, skipping ids that are not there.
///
/// Working through a list is the intended way to use this, so one stale id must
/// not fail the whole page. Returns the ids actually confirmed.
///
/// # Errors
/// Returns a store error on SQL failure.
pub fn confirm_labels(labels: &ReuseStore, request: &BatchConfirmRequest) -> Result<Vec<String>> {
    let decided_by = request
        .decided_by
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_DECIDED_BY);
    Ok(labels.confirm_batch(&request.session_ids, decided_by)?)
}

/// Per-rule counts of how often a person accepted or overturned the verdict it
/// contributed to.
///
/// The only continuously available measure of which rules misfire. The corpus
/// the rules were originally calibrated against is gone, and the one
/// investigation run by hand this cycle — which found that Markdown file links
/// were being read as fabricated paths — is not something to repeat manually.
///
/// # Errors
/// Returns a store error on SQL failure.
pub fn label_stats(labels: &ReuseStore) -> Result<Vec<RuleOverrideStat>> {
    Ok(labels.rule_override_stats()?)
}

/// Lists label rows for the dashboard.
///
/// # Errors
/// Returns [`ReuseApiError::BadParameter`] for an unrecognised label or
/// confirmation state — filtering on a typo would silently return nothing,
/// which reads like "no such trajectories".
pub fn list_sessions(labels: &ReuseStore, query: &SessionsQuery) -> Result<Vec<SessionLabelView>> {
    let mut filter = LabelFilter {
        limit: clamp(query.limit, DEFAULT_SESSIONS_LIMIT, MAX_SESSIONS_LIMIT),
        only_changed_since_decision: query.changed_since_decision.unwrap_or(false),
        ..LabelFilter::default()
    };
    if let Some(raw) = &query.label {
        for token in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
            let parsed =
                TrajectoryLabel::parse(token).ok_or_else(|| ReuseApiError::BadParameter {
                    field: "label",
                    value: token.to_string(),
                })?;
            filter.effective_labels.push(parsed);
        }
    }
    if let Some(raw) = &query.confirm_state {
        filter.confirm_state =
            Some(
                ConfirmState::parse(raw).ok_or_else(|| ReuseApiError::BadParameter {
                    field: "confirm_state",
                    value: raw.clone(),
                })?,
            );
    }
    Ok(labels
        .list_labels(&filter)?
        .iter()
        .map(SessionLabelView::from)
        .collect())
}

#[cfg(test)]
mod tests;
