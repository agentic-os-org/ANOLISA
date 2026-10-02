//! Emergency preflight ladder tests.
//!
//! The ladder degrades `preserve_recent_runs` → 1 → "latest completed run may
//! be summarized" and must never cut the active run, silently lose transcript
//! messages, or misreport a summarizer failure as a missing split point.

use super::*;
use crate::compaction::projection::source_digest;
use crate::config::CoreConfig;
use crate::provider::mock::MockProvider;
use crate::provider::Message;

/// Runtime prefix used by every scenario; small enough to leave the window to
/// the transcript.
const PREFIX_TOKENS: u64 = 1_000;
/// Explicit window so the thresholds are independent of the model tables.
const WINDOW: u64 = 40_000;

/// Config with an explicit window and the requested recent-run protection.
fn config(preserve_recent_runs: usize) -> CoreConfig {
    let mut config = CoreConfig::default();
    config.session.compaction.model_context_window = Some(WINDOW);
    config.session.compaction.preserve_recent_runs = preserve_recent_runs;
    config
}

fn budget_for(config: &CoreConfig) -> ContextBudget {
    let policy = &config.session.compaction;
    let capability =
        ModelCapability::resolve(policy, config.agent.session_token_limit, "mock-model");
    ContextBudget::compute(capability, PREFIX_TOKENS, policy)
}

/// ASCII payload whose conservative estimate is about `tokens` tokens.
///
/// [`super::super::budget::estimate_messages_tokens`] charges four tokens of
/// role overhead plus one token per four ASCII bytes.
fn sized_text(label: &str, tokens: u64) -> String {
    let filler = "x".repeat((tokens.saturating_sub(8) * 4) as usize);
    format!("{label} {filler}")
}

/// A complete Agent run: user prompt plus a final assistant reply.
fn complete_run(label: &str, tokens: u64) -> Vec<Message> {
    let half = tokens / 2;
    vec![
        Message::user(&sized_text(label, half)),
        Message::assistant(&sized_text(label, tokens - half)),
    ]
}

/// An in-flight run: a user prompt whose assistant reply has not arrived.
fn active_run(label: &str, tokens: u64) -> Vec<Message> {
    vec![Message::user(&sized_text(label, tokens))]
}

fn summarizer() -> MockProvider {
    // `repeat_text` never exhausts, so one provider can serve every rung.
    MockProvider::repeat_text("## Objective and constraints\n- keep diagnosing")
}

struct Preflight {
    runtime: CompactionRuntime,
    result: Result<(), String>,
    statuses: Vec<String>,
}

impl Preflight {
    fn status_codes(&self) -> Vec<&str> {
        self.statuses.iter().map(String::as_str).collect()
    }

    fn error(&self) -> &str {
        self.result.as_ref().expect_err("preflight failed closed")
    }
}

/// Runs the preflight over `messages` and captures the emitted status lines.
async fn preflight(
    messages: &[Message],
    config: &CoreConfig,
    provider: &dyn ContentGenerator,
) -> Preflight {
    preflight_resuming(messages, config, provider, None).await
}

/// Same as [`preflight`] but starts from an already committed projection, as a
/// resumed session does.
async fn preflight_resuming(
    messages: &[Message],
    config: &CoreConfig,
    provider: &dyn ContentGenerator,
    state: Option<crate::compaction::CompactionState>,
) -> Preflight {
    let mut runtime = CompactionRuntime::default();
    let revision = state.as_ref().map(|state| state.revision).unwrap_or(0);
    runtime.load_state(state, revision);
    let mut writer: Vec<u8> = Vec::new();
    let result = run_context_preflight(
        &mut runtime,
        messages,
        provider,
        "mock-model",
        config,
        PREFIX_TOKENS,
        &mut writer,
    )
    .await;
    let statuses = String::from_utf8_lossy(&writer)
        .lines()
        .filter_map(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .ok()?
                .get("status")?
                .as_str()
                .map(ToOwned::to_owned)
        })
        .collect();
    Preflight {
        runtime,
        result,
        statuses,
    }
}

/// Asserts the projection covers exactly what it claims and stays consistent.
fn assert_projection_is_consistent(
    runtime: &CompactionRuntime,
    messages: &[Message],
    expected_revision: u64,
) {
    let state = runtime.state().expect("projection committed");
    assert_eq!(state.revision, expected_revision, "projection revision");
    assert_eq!(runtime.revision(), expected_revision, "runtime clock");
    assert_eq!(
        state.source_digest,
        source_digest(&messages[..state.compacted_through]),
        "digest must cover exactly the summarized prefix"
    );
    assert!(super::super::boundary::is_safe_split_point(
        messages,
        state.compacted_through
    ));
}

#[tokio::test]
async fn degrades_from_two_preserved_runs_to_one() {
    // The two most recent runs dominate the context, so preserving both cannot
    // reclaim enough; dropping to one protected run can. #2240.
    let mut messages = complete_run("first", 5_000);
    messages.extend(complete_run("second", 31_000));
    messages.extend(active_run("third", 300));
    let config = config(2);
    let budget = budget_for(&config);

    let preflight = preflight(&messages, &config, &summarizer()).await;
    preflight.result.as_ref().expect("emergency recovered");

    // Two rungs committed: run 0 first, then run 1.
    assert_projection_is_consistent(&preflight.runtime, &messages, 2);
    let state = preflight.runtime.state().expect("projection");
    assert_eq!(
        state.compacted_through, 4,
        "the cut must stop at the active run's first message"
    );
    let history = preflight
        .runtime
        .effective_history_tokens(&messages, PREFIX_TOKENS);
    assert!(
        !budget.over_emergency(history),
        "history {history} must be under the emergency threshold {}",
        budget.emergency_tokens
    );
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_completed"
        ]
    );
}

#[tokio::test]
async fn degrades_to_summarizing_the_latest_completed_run() {
    // No active run: one protected run still leaves an oversized context, so
    // the last rung must summarize the latest completed run too.
    let mut messages = complete_run("first", 5_000);
    messages.extend(complete_run("second", 31_000));
    let config = config(2);
    let budget = budget_for(&config);

    let preflight = preflight(&messages, &config, &summarizer()).await;
    preflight.result.as_ref().expect("emergency recovered");

    // Rung 1 (preserve 2) had no candidate at all, rung 2 compacted run 0 and
    // rung 3 compacted run 1, so exactly two projections were committed.
    assert_projection_is_consistent(&preflight.runtime, &messages, 2);
    assert_eq!(
        preflight
            .runtime
            .state()
            .expect("projection")
            .compacted_through,
        messages.len()
    );
    assert!(!budget.over_emergency(
        preflight
            .runtime
            .effective_history_tokens(&messages, PREFIX_TOKENS)
    ));
}

#[tokio::test]
async fn preserve_two_never_fails_a_transcript_manual_compaction_could_save() {
    // A single oversized complete run: `preserve_recent_runs = 2` finds no cut,
    // but `/session compact` would summarize it. The emergency path must reach
    // the same conclusion instead of reporting context_limit.
    let messages = complete_run("only", 33_000);
    let config = config(2);
    let budget = budget_for(&config);
    assert!(budget.over_emergency(super::super::budget::estimate_messages_tokens(&messages)));

    let preflight = preflight(&messages, &config, &summarizer()).await;
    preflight
        .result
        .as_ref()
        .expect("the last rung must summarize the single completed run");
    assert_projection_is_consistent(&preflight.runtime, &messages, 1);
    assert_eq!(
        preflight
            .runtime
            .state()
            .expect("projection")
            .compacted_through,
        messages.len()
    );
}

#[tokio::test]
async fn repeats_a_policy_when_the_summary_input_only_allows_partial_progress() {
    // Each run is about 160 KiB, so the 192 KiB summary-input ceiling can
    // represent only one run per attempt. A fixed three-call ladder therefore
    // stops while safe completed-run cuts still remain.
    let mut messages = Vec::new();
    for index in 0..30 {
        messages.extend(complete_run(&format!("run {index}"), 40_000));
    }
    let mut config = config(2);
    config.session.compaction.model_context_window = Some(1_000_000);
    let budget = budget_for(&config);
    assert!(budget.over_emergency(super::super::budget::estimate_messages_tokens(&messages)));

    let preflight = preflight(&messages, &config, &summarizer()).await;

    assert!(
        preflight.result.is_ok(),
        "preflight must keep advancing while safe cuts remain: {:?}",
        preflight.result
    );
    let state = preflight.runtime.state().expect("projection committed");
    assert!(
        state.revision
            > emergency_ladder(config.session.compaction.preserve_recent_runs).len() as u64,
        "recovery must require more than one call per ladder rung"
    );
    assert_projection_is_consistent(&preflight.runtime, &messages, state.revision);
    assert!(!budget.over_emergency(
        preflight
            .runtime
            .effective_history_tokens(&messages, PREFIX_TOKENS)
    ));
}

#[tokio::test]
async fn bounded_progress_stops_at_the_per_preflight_attempt_limit() {
    // This window requires more bounded chunks than one preflight may request.
    // The guard must preserve every committed cut, report the limit precisely,
    // and leave another safe cut available for a subsequent retry.
    let mut messages = Vec::new();
    for index in 0..30 {
        messages.extend(complete_run(&format!("run {index}"), 40_000));
    }
    let mut config = config(2);
    config.session.compaction.model_context_window = Some(500_000);
    let budget = budget_for(&config);

    let preflight = preflight(&messages, &config, &summarizer()).await;

    let error = preflight.error();
    assert!(
        error.contains(&format!(
            "per-request limit of {MAX_EMERGENCY_COMPACTION_ATTEMPTS} compaction attempts"
        )),
        "{error}"
    );
    assert!(error.contains("retry to continue compaction"), "{error}");
    assert_eq!(
        preflight.runtime.revision(),
        MAX_EMERGENCY_COMPACTION_ATTEMPTS as u64
    );
    let state = preflight.runtime.state().expect("latest safe projection");
    assert_projection_is_consistent(
        &preflight.runtime,
        &messages,
        MAX_EMERGENCY_COMPACTION_ATTEMPTS as u64,
    );
    assert!(budget.over_emergency(
        preflight
            .runtime
            .effective_history_tokens(&messages, PREFIX_TOKENS)
    ));
    assert!(
        super::super::boundary::select_compacted_through_after(
            &messages,
            PreservationPolicy::ThroughLatestCompletedRun,
            budget.target_tokens,
            state.compacted_through,
        )
        .expect("valid transcript")
        .is_some(),
        "the attempt limit, not boundary exhaustion, must stop this preflight"
    );
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:attempt_limit"
        ]
    );
}

#[tokio::test]
async fn the_active_run_is_never_cut_or_summarized() {
    let mut messages = complete_run("first", 5_000);
    messages.extend(complete_run("second", 31_000));
    let active = active_run("in flight", 400);
    messages.extend(active.clone());
    let config = config(2);

    let preflight = preflight(&messages, &config, &summarizer()).await;
    preflight.result.as_ref().expect("emergency recovered");

    let state = preflight.runtime.state().expect("projection");
    assert!(
        state.compacted_through <= messages.len() - active.len(),
        "cut {} reached into the active run",
        state.compacted_through
    );
    // The active prompt survives verbatim in the provider-visible context.
    let effective = preflight.runtime.effective_messages(&messages);
    assert_eq!(
        effective
            .last()
            .expect("effective context")
            .content
            .as_text(),
        active[0].content.as_text()
    );
}

#[tokio::test]
async fn single_oversized_active_run_fails_closed() {
    // Nothing is complete, so no rung has a safe cut: the guard must refuse the
    // request rather than send an oversized one.
    let messages = active_run("huge in-flight prompt", 33_000);
    let config = config(2);

    let preflight = preflight(&messages, &config, &summarizer()).await;
    let error = preflight.error();
    assert!(error.starts_with(CONTEXT_LIMIT_ERROR_PREFIX), "{error}");
    assert!(error.contains("active run alone exceeds"), "{error}");
    assert!(
        preflight.runtime.state().is_none(),
        "no projection may be committed"
    );
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:active_run_too_large"
        ]
    );
}

#[tokio::test]
async fn summarizer_failure_is_reported_as_a_provider_error() {
    // Regression: a provider failure used to collapse into "no safe split".
    let mut messages = complete_run("first", 5_000);
    messages.extend(complete_run("second", 31_000));
    messages.extend(active_run("third", 300));
    let config = config(2);

    let preflight = preflight(&messages, &config, &MockProvider::partial_error()).await;
    let error = preflight.error();
    assert!(error.starts_with(CONTEXT_LIMIT_ERROR_PREFIX), "{error}");
    assert!(error.contains("summary generation failed"), "{error}");
    assert!(!error.contains("no safe Agent-run split"), "{error}");
    assert!(preflight.runtime.state().is_none());
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:provider_error"
        ]
    );
}

#[tokio::test]
async fn inflating_summary_is_reported_as_not_reducing() {
    let mut messages = complete_run("first", 5_000);
    messages.extend(complete_run("second", 31_000));
    messages.extend(active_run("third", 300));
    let config = config(2);
    // A summary larger than the ~5 000-token prefix it replaces, but still
    // inside the 32 KiB persisted bound, so `not_reducing` is what rejects it.
    let provider = MockProvider::repeat_text(&"inflated ".repeat(3_000));

    let preflight = preflight(&messages, &config, &provider).await;
    let error = preflight.error();
    assert!(error.starts_with(CONTEXT_LIMIT_ERROR_PREFIX), "{error}");
    assert!(error.contains("did not reduce the context"), "{error}");
    // Deterministic: the deepest rung already used manual-compaction semantics,
    // so `/session compact` would reproduce this exact failure.
    assert!(!error.contains("compact manually"), "{error}");
    assert!(!error.contains("retry"), "{error}");
    assert!(error.contains("start a new session"), "{error}");
    assert!(preflight.runtime.state().is_none());
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:not_reducing"
        ]
    );
}

#[tokio::test]
async fn oversized_input_is_deterministic_and_does_not_suggest_manual_compaction() {
    // One completed run larger than the model-aware summarizer input budget:
    // no rung can render it, and a longer prefix only makes the input bigger,
    // so manual compaction cannot help either.
    let mut messages = complete_run("huge completed", 34_000);
    messages.extend(active_run("in flight", 300));
    let config = config(2);

    let preflight = preflight(&messages, &config, &summarizer()).await;

    let error = preflight.error();
    assert!(error.starts_with(CONTEXT_LIMIT_ERROR_PREFIX), "{error}");
    assert!(error.contains("summarized without loss"), "{error}");
    assert!(!error.contains("compact manually"), "{error}");
    assert!(!error.contains("retry"), "{error}");
    assert!(error.contains("start a new session"), "{error}");
    assert!(preflight.runtime.state().is_none());
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:oversized_input"
        ]
    );
}

#[tokio::test]
async fn a_transient_provider_failure_still_suggests_retry_and_manual_compaction() {
    // A summarizer hiccup is not a property of the transcript, so retrying or
    // running `/session compact` explicitly is genuinely worth suggesting.
    let mut messages = complete_run("first", 5_000);
    messages.extend(complete_run("second", 25_000));
    messages.extend(active_run("third", 300));
    let config = config(2);

    let preflight = preflight(&messages, &config, &MockProvider::partial_error()).await;

    let error = preflight.error();
    assert!(error.contains("summary generation failed"), "{error}");
    assert!(error.contains("retry, compact manually"), "{error}");
}

#[tokio::test]
async fn not_reducing_at_a_conservative_cut_still_degrades_and_recovers() {
    // Regression: `NotReducing` is specific to the cut that was tried, not a
    // verdict on the transcript. Summarizing only the tiny first run costs more
    // than it reclaims, but a deeper rung absorbs the large second run and wins.
    // Treating the first rung's `NotReducing` as terminal reported
    // `context_limit` for a request the ladder could still save.
    // Sized so the deep cut (first + second ≈ 30 000 tokens) still fits the
    // model-aware summarizer input budget for this 40 000-token window, while
    // the transcript as a whole (≈ 30 300) is over the emergency threshold.
    let mut messages = complete_run("first", 1_200);
    messages.extend(complete_run("second", 28_800));
    messages.extend(active_run("third", 300));
    let config = config(2);
    // ~2 000 tokens: more than the 1 200-token first run (so the conservative
    // rung inflates), far less than the 30 000-token prefix a deeper rung takes.
    let provider = MockProvider::repeat_text(&"inflated ".repeat(900));

    let preflight = preflight(&messages, &config, &provider).await;

    assert!(
        preflight.result.is_ok(),
        "ladder must recover past a cut-specific NotReducing: {:?}",
        preflight.result
    );
    // A projection was committed and it covers the large second run, not just
    // the first one that could not pay for its own summary.
    let state = preflight.runtime.state().expect("projection committed");
    assert!(
        state.compacted_through > 2,
        "deeper rung must summarize past the first run: {}",
        state.compacted_through
    );
    assert_projection_is_consistent(&preflight.runtime, &messages, 1);
    // The effective context is back under the emergency threshold.
    let budget = budget_for(&config);
    assert!(!budget.over_emergency(
        preflight
            .runtime
            .effective_history_tokens(&messages, PREFIX_TOKENS)
    ));
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_completed"
        ]
    );
}

#[tokio::test]
async fn an_oversized_summary_with_nothing_left_to_cut_reports_no_safe_split() {
    // Resumed session on a tiny window whose existing projection already covers
    // the whole transcript: no rung has a candidate and there is no active run,
    // so the snapshot itself is what exceeds the threshold.
    let messages = complete_run("summarized", 400);
    let mut config = config(2);
    config.session.compaction.model_context_window = Some(8_000);
    let state = crate::compaction::CompactionState {
        revision: 3,
        compacted_through: messages.len(),
        summary: "prior context ".repeat(1_400),
        model: "mock-model".to_string(),
        prompt_version: super::super::projection::COMPACTION_PROMPT_VERSION,
        source_digest: source_digest(&messages),
        tokens_before: None,
        tokens_after: None,
        created_at_ms: 0,
    };

    let preflight =
        preflight_resuming(&messages, &config, &summarizer(), Some(state.clone())).await;
    let error = preflight.error();
    assert!(error.starts_with(CONTEXT_LIMIT_ERROR_PREFIX), "{error}");
    assert!(error.contains("no safe Agent-run split"), "{error}");
    // The pre-existing projection is untouched by a failed emergency.
    assert_eq!(preflight.runtime.state(), Some(&state));
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:no_safe_split"
        ]
    );
}

#[tokio::test]
async fn a_summary_plus_a_modest_active_run_reports_an_irreducible_remainder() {
    // A large persisted summary and an active run that is well under the
    // threshold on its own exhaust the ladder together. Blaming the active run
    // merely because one exists would misdirect the operator, so the measured
    // remainder is classified as irreducible instead.
    let mut messages = complete_run("summarized", 400);
    let active_tokens = 23_000;
    messages.extend(active_run("in flight", active_tokens));
    let config = config(2);
    let budget = budget_for(&config);
    // ~8 000 tokens, just inside the 32 KiB persisted summary bound.
    let state = crate::compaction::CompactionState {
        revision: 4,
        compacted_through: 2,
        summary: "prior context ".repeat(2_285),
        model: "mock-model".to_string(),
        prompt_version: super::super::projection::COMPACTION_PROMPT_VERSION,
        source_digest: source_digest(&messages[..2]),
        tokens_before: None,
        tokens_after: None,
        created_at_ms: 0,
    };
    // Preconditions that make this case distinct from `active_run_too_large`:
    // the tail alone is under the threshold, but with the summary it is over.
    assert!(!budget.over_emergency(active_tokens));

    let preflight =
        preflight_resuming(&messages, &config, &summarizer(), Some(state.clone())).await;

    let error = preflight.error();
    assert!(error.starts_with(CONTEXT_LIMIT_ERROR_PREFIX), "{error}");
    assert!(
        error.contains("summary plus the remaining verbatim messages"),
        "{error}"
    );
    assert!(!error.contains("active run alone exceeds"), "{error}");
    // The deepest rung already used manual semantics, so manual compaction is
    // not offered as the recovery path.
    assert!(!error.contains("compact manually"), "{error}");
    assert!(error.contains("start a new session"), "{error}");
    assert_eq!(preflight.runtime.state(), Some(&state));
    assert_eq!(
        preflight.status_codes(),
        vec![
            "compaction_emergency_started",
            "compaction_emergency_failed:irreducible_remainder"
        ]
    );
}

#[tokio::test]
async fn a_context_under_the_threshold_is_left_alone() {
    let messages = complete_run("small", 500);
    let config = config(2);
    let preflight = preflight(&messages, &config, &summarizer()).await;
    preflight.result.as_ref().expect("no emergency");
    assert!(preflight.runtime.state().is_none());
    assert!(preflight.statuses.is_empty());
}

#[test]
fn the_ladder_degrades_monotonically_without_duplicate_rungs() {
    assert_eq!(
        emergency_ladder(2),
        vec![
            PreservationPolicy::RecentRuns(2),
            PreservationPolicy::RecentRuns(1),
            PreservationPolicy::ThroughLatestCompletedRun,
        ]
    );
    // A policy of 0 or 1 must not spend two identical summarizer calls.
    for preserve in [0, 1] {
        assert_eq!(
            emergency_ladder(preserve),
            vec![
                PreservationPolicy::RecentRuns(1),
                PreservationPolicy::ThroughLatestCompletedRun,
            ],
            "preserve_recent_runs = {preserve}"
        );
    }
    assert_eq!(emergency_ladder(5).len(), 3);
}

#[tokio::test]
async fn disabled_compaction_skips_the_guard_entirely() {
    let messages = complete_run("only", 33_000);
    let mut config = config(2);
    config.session.compaction.enabled = false;
    let preflight = preflight(&messages, &config, &summarizer()).await;
    preflight
        .result
        .as_ref()
        .expect("a disabled policy must not block the request");
    assert!(preflight.statuses.is_empty());
}
