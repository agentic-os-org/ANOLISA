//! Core-owned compaction lifecycle shared by manual and automatic triggers.
//!
//! Commit order: load snapshot → capture generation and source digest →
//! choose a safe prefix → generate the summary without holding the session
//! lock → validate the candidate → reload and revalidate generation and
//! digest → atomically commit through the session store.

use std::io::Write;

use serde::Serialize;

use crate::cli::CliArgs;
use crate::config::CoreConfig;
use crate::context::ContextBuilder;
use crate::provider::ContentGenerator;
use crate::session::{PersistedSession, ProviderSessionId, SessionError, SessionStore};

use super::boundary::{
    group_agent_runs, select_compacted_through_after, BoundaryError, PreservationPolicy,
};
use super::budget::{
    estimate_messages_tokens, estimate_text_tokens, measure_history, ContextBudget, ModelCapability,
};
use super::projection::{
    effective_messages, source_digest, CompactionState, TokenMeasurement, COMPACTION_PROMPT_VERSION,
};
use super::summarize::{
    generate_summary, render_summary_input, summary_input_token_budget, SummaryError,
};

/// Estimated tokens reserved for tool declarations in CLI-mode budgets.
const CLI_TOOL_DECLARATION_RESERVE: u64 = 2_048;

/// Expected pre-compaction context revision for an automatic attempt.
///
/// An automatic recommendation is emitted at an idle boundary and acted on
/// later by a separate process; by then the session may have advanced.
/// [`compact_session`] validates this against the freshly loaded session
/// *before* any provider work, so a stale recommendation never spends a model
/// call or rewrites a context that already moved on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedRevision {
    /// Session store generation captured when the recommendation was emitted.
    pub generation: u64,
    /// Compaction revision clock captured with the recommendation.
    ///
    /// Zero means the session has never committed a compaction. This tracks
    /// the durable clock rather than the live projection, so a projection
    /// rejected by sanitization does not make an in-flight recommendation
    /// look stale.
    pub projection_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
/// Why a compaction attempt was started.
pub enum CompactionTrigger {
    /// Explicit user request (`/session compact` or `--compact`).
    Manual,
    /// Idle-boundary automatic trigger after crossing the soft threshold.
    Auto,
    /// In-run emergency protection before an oversized provider request.
    Emergency,
}

impl CompactionTrigger {
    /// Returns the stable protocol label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
            Self::Emergency => "emergency",
        }
    }

    /// Default preservation policy for this trigger.
    ///
    /// Automatic and emergency compaction start from the configured recent-run
    /// protection so a normal soft-threshold compaction stays quality-first;
    /// the emergency preflight owns its own stricter fallbacks from there.
    fn preservation(&self, preserve_recent_runs: usize) -> PreservationPolicy {
        match self {
            Self::Manual => PreservationPolicy::ThroughLatestCompletedRun,
            Self::Auto | Self::Emergency => PreservationPolicy::RecentRuns(preserve_recent_runs),
        }
    }
}

#[derive(Debug, Clone)]
/// Failure classes that leave the previous projection untouched.
pub enum CompactionError {
    /// Compaction is disabled by configuration.
    Disabled,
    /// The transcript has no compactable complete Agent run.
    NothingToCompact,
    /// The transcript violates the tool protocol; compaction fails closed.
    Boundary(String),
    /// The summarizer provider request failed.
    Provider(String),
    /// The provider returned no usable summary text.
    EmptySummary,
    /// The provider exceeded the persisted summary bound.
    OversizedSummary,
    /// The summary stream was cancelled before completion.
    Cancelled,
    /// The candidate did not shrink the effective context.
    NotReducing,
    /// No safe cut fits inside the bounded summarizer input.
    OversizedInput,
    /// The session generation moved between snapshot and commit.
    Conflict,
    /// The summarized prefix changed between snapshot and commit.
    DigestMismatch,
    /// The persisted compaction revision cannot be incremented.
    RevisionExhausted,
    /// The session store rejected the operation.
    Session(SessionError),
}

impl CompactionError {
    /// Returns the stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NothingToCompact => "nothing_to_compact",
            Self::Boundary(_) => "unsafe_boundary",
            Self::Provider(_) => "provider_error",
            Self::EmptySummary => "empty_summary",
            Self::OversizedSummary => "oversized_summary",
            Self::Cancelled => "cancelled",
            Self::NotReducing => "not_reducing",
            Self::OversizedInput => "oversized_input",
            Self::Conflict => "conflict",
            Self::DigestMismatch => "digest_mismatch",
            Self::RevisionExhausted => "revision_exhausted",
            Self::Session(error) => error.code(),
        }
    }
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disabled => write!(formatter, "session compaction is disabled"),
            Self::NothingToCompact => {
                write!(formatter, "no complete Agent run is available to compact")
            }
            Self::Boundary(detail) => {
                write!(formatter, "unsafe transcript boundary: {detail}")
            }
            Self::Provider(detail) => write!(formatter, "summary generation failed: {detail}"),
            Self::EmptySummary => write!(formatter, "provider returned an empty summary"),
            Self::OversizedSummary => {
                write!(formatter, "provider summary exceeded the size bound")
            }
            Self::Cancelled => write!(formatter, "compaction was cancelled"),
            Self::NotReducing => {
                write!(formatter, "candidate summary did not reduce the context")
            }
            Self::OversizedInput => write!(
                formatter,
                "no safe split fits the bounded summarizer input; history cannot be \
                 summarized without loss"
            ),
            Self::Conflict => write!(
                formatter,
                "session changed concurrently; candidate was discarded"
            ),
            Self::DigestMismatch => write!(
                formatter,
                "summarized transcript prefix changed; candidate was discarded"
            ),
            Self::RevisionExhausted => {
                write!(formatter, "compaction revision is exhausted")
            }
            Self::Session(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for CompactionError {}

impl From<BoundaryError> for CompactionError {
    fn from(error: BoundaryError) -> Self {
        Self::Boundary(error.to_string())
    }
}

impl From<SummaryError> for CompactionError {
    fn from(error: SummaryError) -> Self {
        match error {
            SummaryError::Provider(detail) => Self::Provider(detail),
            SummaryError::Empty => Self::EmptySummary,
            SummaryError::Oversized => Self::OversizedSummary,
            SummaryError::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
/// Committed compaction outcome reported over CLI and shell protocols.
pub struct CompactionReport {
    /// Session that owns the projection.
    pub session_id: String,
    /// Committed projection revision.
    pub revision: u64,
    /// Transcript index covered by the summary.
    pub compacted_through: usize,
    /// Complete transcript length; unchanged by compaction.
    pub transcript_messages: usize,
    /// Effective context measurement before compaction.
    pub tokens_before: TokenMeasurement,
    /// Estimated effective context after compaction.
    pub tokens_after: TokenMeasurement,
    /// Persisted summary size in bytes.
    pub summary_bytes: usize,
    /// Trigger that started this attempt.
    pub trigger: CompactionTrigger,
    /// Budget snapshot used for the decision.
    pub budget: ContextBudget,
}

/// Runs one full compaction attempt against a persisted session.
///
/// The session lock is only held inside the final `persist`; the provider
/// request runs lock-free. Any error leaves the stored projection unchanged.
///
/// # Errors
///
/// Returns a [`CompactionError`] describing why the candidate was not
/// committed.
pub async fn compact_session(
    store: &SessionStore,
    session_id: &ProviderSessionId,
    provider: &dyn ContentGenerator,
    config: &CoreConfig,
    prefix_tokens: u64,
    provider_reported_history: Option<u64>,
    trigger: CompactionTrigger,
    expected: Option<ExpectedRevision>,
) -> Result<(CompactionReport, PersistedSession), CompactionError> {
    let policy = &config.session.compaction;
    if !policy.enabled {
        return Err(CompactionError::Disabled);
    }

    // 1. Snapshot: capture generation and digest before any provider work.
    let session = store.load(session_id).map_err(CompactionError::Session)?;
    let base_generation = session.generation;

    // 1a. Fail closed on a stale automatic recommendation *before* spending a
    //     provider call: if the session generation or the projection revision
    //     moved since the recommendation was emitted, this attempt targets an
    //     out-of-date context and must be discarded with no model work.
    if let Some(expected) = expected {
        if base_generation != expected.generation {
            return Err(CompactionError::Conflict);
        }
        // Compare the durable revision clock, not whether a projection
        // survived sanitization: a recommendation emitted at revision `N`
        // stays valid when loading rejects the revision-`N` projection, and
        // the next successful commit is still `N + 1`.
        if session.compaction_revision != expected.projection_revision {
            return Err(CompactionError::Conflict);
        }
    }

    // 1b. Reserve the next revision from the durable clock before any provider
    //     work. The stored clock is untrusted disk input, so an exhausted
    //     counter fails closed here instead of wrapping at commit time.
    let next_revision = session
        .compaction_revision
        .checked_add(1)
        .ok_or(CompactionError::RevisionExhausted)?;

    let model = if session.model.is_empty() {
        config.resolve_provider().model
    } else {
        session.model.clone()
    };

    let capability = ModelCapability::resolve(policy, config.agent.session_token_limit, &model);
    let budget = ContextBudget::compute(capability, prefix_tokens, policy);

    let effective_before = effective_messages(&session.messages, session.compaction.as_ref());
    let estimated_before = estimate_messages_tokens(&effective_before);
    let tokens_before = measure_history(provider_reported_history, estimated_before);

    // 2. Choose a safe prefix over the complete transcript.
    let previous_cut = session
        .compaction
        .as_ref()
        .map(|state| state.compacted_through)
        .unwrap_or(0);
    let cut = select_compacted_through_after(
        &session.messages,
        trigger.preservation(policy.preserve_recent_runs),
        budget.target_tokens,
        previous_cut,
    )?
    .ok_or(CompactionError::NothingToCompact)?;

    // 3. Bound the summarizer input by the summarizer model's own context
    //    window (plus the byte-level memory ceiling). The committed cut may
    //    only cover messages the input actually rendered; otherwise omitted
    //    history would silently vanish from the effective context.
    let (cut, input) = bounded_cut_and_input(
        &session.messages,
        session.compaction.as_ref(),
        previous_cut,
        cut,
        summary_input_token_budget(&capability),
    )?;
    let digest = source_digest(&session.messages[..cut]);

    // 4. Summarize without holding the session lock.
    let summary = generate_summary(provider, &model, &input).await?;

    // 5. Validate the candidate before touching persistence.
    let candidate = CompactionState {
        revision: next_revision,
        compacted_through: cut,
        summary,
        model: model.clone(),
        prompt_version: COMPACTION_PROMPT_VERSION,
        source_digest: digest.clone(),
        tokens_before: Some(tokens_before),
        tokens_after: None,
        created_at_ms: now_ms(),
    };
    let effective_after = effective_messages(&session.messages, Some(&candidate));
    let estimated_after = estimate_messages_tokens(&effective_after);
    if estimated_after >= estimated_before {
        return Err(CompactionError::NotReducing);
    }
    let tokens_after = measure_history(None, estimated_after);
    let mut candidate = candidate;
    candidate.tokens_after = Some(tokens_after);

    // 6. Reacquire, revalidate, and commit atomically.
    let mut current = store.load(session_id).map_err(CompactionError::Session)?;
    if current.generation != base_generation {
        return Err(CompactionError::Conflict);
    }
    if current.messages.len() < cut || source_digest(&current.messages[..cut]) != digest {
        return Err(CompactionError::DigestMismatch);
    }
    // The generation check above proves `current` is the same persisted
    // version the snapshot read, so `next_revision` is still exactly one past
    // this session's durable clock. Advance the clock with the projection.
    current.compaction_revision = candidate.revision;
    current.compaction = Some(candidate.clone());
    match store.persist(&mut current) {
        Ok(()) => {}
        Err(SessionError::Conflict { .. }) => return Err(CompactionError::Conflict),
        Err(error) => return Err(CompactionError::Session(error)),
    }

    let report = CompactionReport {
        session_id: session_id.to_string(),
        revision: candidate.revision,
        compacted_through: cut,
        transcript_messages: current.messages.len(),
        tokens_before,
        tokens_after,
        summary_bytes: candidate.summary.len(),
        trigger,
        budget,
    };
    Ok((report, current))
}

/// In-memory emergency compaction for the active in-run context.
///
/// Used between complete model/tool exchanges inside one Agent run, where the
/// running process is the session's only writer; the resulting projection is
/// committed together with the run's transcript at the next persist.
///
/// `preservation` is supplied by the caller rather than read from the policy so
/// the emergency preflight can degrade its protection step by step over the
/// same selection and summarization engine the manual path uses.
///
/// `previous_revision` is the runtime's durable revision clock, which outlives
/// any projection dropped by sanitization; the candidate takes the next value.
///
/// # Errors
///
/// Returns the [`CompactionError`] that stopped the attempt so the caller can
/// distinguish "this policy protects every remaining run"
/// ([`CompactionError::NothingToCompact`]) from a summarizer failure. The
/// caller keeps its previous projection in every error case.
pub(crate) async fn compact_in_memory(
    messages: &[crate::provider::Message],
    previous: Option<&CompactionState>,
    previous_revision: u64,
    provider: &dyn ContentGenerator,
    model: &str,
    config: &CoreConfig,
    target_tokens: u64,
    preservation: PreservationPolicy,
) -> Result<CompactionState, CompactionError> {
    let policy = &config.session.compaction;
    if !policy.enabled {
        return Err(CompactionError::Disabled);
    }
    // Fail closed before any provider work.
    let revision = previous_revision
        .checked_add(1)
        .ok_or(CompactionError::RevisionExhausted)?;
    let previous_cut = previous.map(|state| state.compacted_through).unwrap_or(0);
    let cut = select_compacted_through_after(messages, preservation, target_tokens, previous_cut)?
        .ok_or(CompactionError::NothingToCompact)?;
    // Emergency compaction shares the exact model-aware input budget used by
    // the manual/automatic engine path, so the three triggers cannot drift.
    let capability = ModelCapability::resolve(policy, config.agent.session_token_limit, model);
    let (cut, input) = bounded_cut_and_input(
        messages,
        previous,
        previous_cut,
        cut,
        summary_input_token_budget(&capability),
    )?;
    let digest = source_digest(&messages[..cut]);
    let summary = generate_summary(provider, model, &input).await?;

    let estimated_before = estimate_messages_tokens(&effective_messages(messages, previous));
    let candidate = CompactionState {
        revision,
        compacted_through: cut,
        summary,
        model: model.to_string(),
        prompt_version: COMPACTION_PROMPT_VERSION,
        source_digest: digest,
        tokens_before: Some(measure_history(None, estimated_before)),
        tokens_after: None,
        created_at_ms: now_ms(),
    };
    let estimated_after = estimate_messages_tokens(&effective_messages(messages, Some(&candidate)));
    if estimated_after >= estimated_before {
        return Err(CompactionError::NotReducing);
    }
    let mut candidate = candidate;
    candidate.tokens_after = Some(measure_history(None, estimated_after));
    Ok(candidate)
}

/// Clamps a selected cut so the projection never covers messages that the
/// bounded summarizer input could not represent.
///
/// `max_input_tokens` is the model-aware budget from
/// [`summary_input_token_budget`]; the byte ceiling inside
/// [`render_summary_input`] stays as a memory-safety second layer. Returns
/// the final cut together with the input that renders exactly the committed
/// message range.
///
/// # Errors
///
/// Returns [`CompactionError::OversizedInput`] when not even one complete
/// Agent run fits inside the summarizer input bound, and propagates
/// boundary validation failures.
fn bounded_cut_and_input(
    messages: &[crate::provider::Message],
    previous: Option<&CompactionState>,
    previous_cut: usize,
    cut: usize,
    max_input_tokens: u64,
) -> Result<(usize, String), CompactionError> {
    let rendered = render_summary_input(previous, &messages[previous_cut..cut], max_input_tokens);
    if rendered.rendered_messages >= cut - previous_cut {
        return Ok((cut, rendered.input));
    }
    // Shrink to the largest run boundary whose whole span was rendered.
    let limit = previous_cut + rendered.rendered_messages;
    let adjusted = group_agent_runs(messages)?
        .iter()
        .map(|span| span.start)
        .filter(|start| *start > previous_cut && *start <= limit)
        .max()
        .ok_or(CompactionError::OversizedInput)?;
    let rerendered = render_summary_input(
        previous,
        &messages[previous_cut..adjusted],
        max_input_tokens,
    );
    if rerendered.rendered_messages < adjusted - previous_cut {
        // A strictly shorter slice must render fully; anything else means
        // one run alone exceeds the input bound.
        return Err(CompactionError::OversizedInput);
    }
    Ok((adjusted, rerendered.input))
}

/// Estimates the runtime prefix (`P`) for provider-free CLI budgets.
pub(crate) fn cli_prefix_tokens(config: &CoreConfig, workspace: &std::path::Path) -> u64 {
    let system_prompt = ContextBuilder::build_system_prompt(
        workspace,
        &[],
        &[],
        config.agent.approval_mode.label(),
        config.ai.output_language.as_deref(),
    );
    estimate_text_tokens(&system_prompt) + CLI_TOOL_DECLARATION_RESERVE
}

#[derive(Debug, Serialize)]
struct CompactCliEnvelope<'a> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<&'a CompactionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<CompactCliError>,
}

#[derive(Debug, Serialize)]
struct CompactCliError {
    code: String,
    message: String,
    recoverable: bool,
}

/// Serves `cosh-core --headless --workspace <ws> --resume <id> --compact`.
///
/// Emits exactly one JSON envelope on stdout and returns the exit code.
pub(crate) async fn run_compact_cli(args: &CliArgs, config: CoreConfig) -> i32 {
    let result = compact_cli_inner(args, &config).await;
    let stdout = std::io::stdout();
    let mut writer = std::io::BufWriter::new(stdout.lock());
    let (envelope, code) = match &result {
        Ok(report) => (
            CompactCliEnvelope {
                ok: true,
                data: Some(report),
                error: None,
            },
            0,
        ),
        Err(error) => (
            CompactCliEnvelope {
                ok: false,
                data: None,
                error: Some(CompactCliError {
                    code: error.code().to_string(),
                    message: error.to_string(),
                    recoverable: true,
                }),
            },
            1,
        ),
    };
    if let Ok(encoded) = serde_json::to_string(&envelope) {
        let _ = writeln!(writer, "{encoded}");
        let _ = writer.flush();
    }
    code
}

async fn compact_cli_inner(
    args: &CliArgs,
    config: &CoreConfig,
) -> Result<CompactionReport, CompactionError> {
    let Some(resume) = args.resume.as_deref() else {
        return Err(CompactionError::Session(SessionError::InvalidRequest {
            message: "--compact requires --resume <session-id>".to_string(),
        }));
    };
    let session_id = ProviderSessionId::parse(resume).map_err(CompactionError::Session)?;

    // Validate the manual/auto argument combination *before* any auth check,
    // provider creation, or engine call, so a malformed invocation fails
    // closed with no side effects. The only legal shapes are:
    //   - manual: neither --expect-generation nor --expect-revision;
    //   - auto (`--auto-compact`): both present, binding the run to an exact
    //     generation and projection revision.
    // Anything else (auto missing a bound, only one bound, or a manual run
    // carrying an expected bound) is a typed invalid_request. Leaving expected
    // unbound would let an automatic run compact without revision binding.
    let (trigger, expected) = match (
        args.auto_compact,
        args.expect_generation,
        args.expect_revision,
    ) {
        (false, None, None) => (CompactionTrigger::Manual, None),
        (true, Some(generation), Some(projection_revision)) => (
            CompactionTrigger::Auto,
            Some(ExpectedRevision {
                generation,
                projection_revision,
            }),
        ),
        _ => {
            return Err(CompactionError::Session(SessionError::InvalidRequest {
                message: "automatic compaction requires both --expect-generation and \
                              --expect-revision; manual compaction must supply neither"
                    .to_string(),
            }));
        }
    };

    let workspace = args
        .workspace
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
    let store = SessionStore::for_workspace(&config.session.persist_dir, &workspace)
        .map_err(CompactionError::Session)?;
    if crate::needs_auth(config) {
        return Err(CompactionError::Provider(
            "provider credentials are not configured; run /auth first".to_string(),
        ));
    }
    let provider = crate::create_provider(config);
    let prefix_tokens = cli_prefix_tokens(config, &workspace);
    let (report, _updated) = compact_session(
        &store,
        &session_id,
        provider.as_ref(),
        config,
        prefix_tokens,
        None,
        trigger,
        expected,
    )
    .await?;
    Ok(report)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
