use super::*;
use crate::provider::mock::MockProvider;
use crate::provider::Message;

fn store(temp: &tempfile::TempDir) -> SessionStore {
    SessionStore::for_workspace(temp.path().join("sessions").to_str().unwrap(), temp.path())
        .expect("session store")
}

/// Builds a transcript of `runs` complete Agent runs with bulky content.
fn bulky_messages(runs: usize) -> Vec<Message> {
    let filler = "log line with useful diagnostics 诊断输出 ".repeat(120);
    let mut messages = Vec::new();
    for run in 0..runs {
        messages.push(Message::user(&format!("prompt {run}: {filler}")));
        messages.push(Message::assistant(&format!("answer {run}: {filler}")));
    }
    messages
}

fn persisted(store: &SessionStore, runs: usize) -> PersistedSession {
    let mut session = PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        bulky_messages(runs),
    );
    store.persist(&mut session).expect("persist fixture");
    session
}

fn summary_provider() -> MockProvider {
    MockProvider::text_only("## Objective and constraints\n- keep diagnosing memory usage")
}

async fn compact(
    store: &SessionStore,
    session: &PersistedSession,
    provider: &dyn ContentGenerator,
    config: &CoreConfig,
) -> Result<(CompactionReport, PersistedSession), CompactionError> {
    compact_session(
        store,
        &session.session_id,
        provider,
        config,
        1_000,
        None,
        CompactionTrigger::Manual,
        None,
    )
    .await
}

#[tokio::test]
async fn compact_persist_reload_preserves_identity_and_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let before = store.inspect(&session.session_id).expect("summary before");

    let provider = summary_provider();
    let config = CoreConfig::default();
    let (report, updated) = compact(&store, &session, &provider, &config)
        .await
        .expect("compaction committed");

    assert_eq!(report.session_id, session.session_id.to_string());
    assert!(report.compacted_through > 0);
    assert!(report.tokens_after.value < report.tokens_before.value);
    assert_eq!(updated.messages.len(), session.messages.len());

    // Simulated process restart: reload from disk.
    let reloaded = store.load(&session.session_id).expect("reload");
    let projection = reloaded.compaction.as_ref().expect("projection persisted");
    assert_eq!(projection.revision, 1);
    assert_eq!(projection.compacted_through, report.compacted_through);
    assert_eq!(reloaded.session_id, session.session_id);
    assert_eq!(reloaded.created_at_ms, session.created_at_ms);
    assert_eq!(reloaded.messages.len(), session.messages.len());

    // Picker metadata stays stable: first prompt and message count.
    let after = store.inspect(&session.session_id).expect("summary after");
    assert_eq!(after.first_prompt, before.first_prompt);
    assert_eq!(after.message_count, before.message_count);

    // The effective context replaces the prefix with one snapshot.
    let effective = effective_messages(&reloaded.messages, reloaded.compaction.as_ref());
    assert!(effective.len() < reloaded.messages.len());
    assert!(effective[0].content.as_text().contains("compacted-history"));
}

#[tokio::test]
async fn manual_compaction_can_summarize_one_complete_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 1);
    let provider = summary_provider();

    let (report, updated) = compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect("manual compaction committed");

    assert_eq!(report.compacted_through, session.messages.len());
    assert_eq!(updated.messages.len(), session.messages.len());
    assert_eq!(
        updated
            .compaction
            .as_ref()
            .expect("projection")
            .compacted_through,
        session.messages.len()
    );
}

#[tokio::test]
async fn sessions_without_projection_load_compatibly() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 2);
    let loaded = store.load(&session.session_id).expect("load");
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.messages.len(), 4);
}

#[tokio::test]
async fn provider_error_leaves_previous_projection_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let config = CoreConfig::default();

    // Commit a first projection.
    let provider = summary_provider();
    compact(&store, &session, &provider, &config)
        .await
        .expect("first compaction");
    let committed = store.load(&session.session_id).unwrap();

    // Grow the transcript so another cut becomes available, then fail.
    let mut grown = committed.clone();
    grown.messages.extend(bulky_messages(3));
    store.persist(&mut grown).expect("grow transcript");
    let failing = MockProvider::partial_error();
    let error = compact(&store, &grown, &failing, &config)
        .await
        .expect_err("provider failure");
    assert_eq!(error.code(), "provider_error");

    let after = store.load(&session.session_id).unwrap();
    assert_eq!(after.compaction, committed.compaction);
}

#[tokio::test]
async fn empty_summary_is_never_committed() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let provider = MockProvider::text_only("   \n ");
    let error = compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect_err("empty summary");
    assert_eq!(error.code(), "empty_summary");
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_none());
}

#[tokio::test]
async fn inflating_summary_is_rejected_as_not_reducing() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    // Short transcript: three tiny runs, preserve one.
    let mut session = PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![
            Message::user("a"),
            Message::assistant("b"),
            Message::user("c"),
            Message::assistant("d"),
            Message::user("e"),
            Message::assistant("f"),
        ],
    );
    store.persist(&mut session).unwrap();
    let mut config = CoreConfig::default();
    config.session.compaction.preserve_recent_runs = 1;
    let provider = MockProvider::text_only(&"inflated summary ".repeat(1_000));
    let error = compact(&store, &session, &provider, &config)
        .await
        .expect_err("token inflation");
    assert_eq!(error.code(), "not_reducing");
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_none());
}

#[tokio::test]
async fn cancelled_stream_is_never_committed() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    // Stream ends without MessageEnd, as after a cancel().
    let provider = MockProvider::new(vec![vec![crate::provider::GenerateEvent::TextDelta(
        "partial".to_string(),
    )]]);
    let error = compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect_err("cancelled stream");
    assert_eq!(error.code(), "cancelled");
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_none());
}

/// Provider that concurrently advances the session while summarizing.
struct ConflictingProvider {
    persist_dir: String,
    workspace: std::path::PathBuf,
    session_id: ProviderSessionId,
    inner: MockProvider,
}

#[async_trait::async_trait]
impl ContentGenerator for ConflictingProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[crate::provider::ToolDeclaration],
        config: &crate::provider::GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        let store = SessionStore::for_workspace(&self.persist_dir, &self.workspace)
            .expect("conflict store");
        let mut session = store.load(&self.session_id).expect("conflict load");
        session.messages.push(Message::user("concurrent turn"));
        session
            .messages
            .push(Message::assistant("concurrent answer"));
        store.persist(&mut session).expect("conflict persist");
        self.inner.generate(messages, tools, config).await
    }

    fn cancel(&self) {}
}

#[tokio::test]
async fn generation_conflict_discards_candidate() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions").to_str().unwrap().to_string();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let provider = ConflictingProvider {
        persist_dir,
        workspace: temp.path().to_path_buf(),
        session_id: session.session_id.clone(),
        inner: summary_provider(),
    };
    let error = compact_session(
        &store,
        &session.session_id,
        &provider,
        &CoreConfig::default(),
        1_000,
        None,
        CompactionTrigger::Auto,
        None,
    )
    .await
    .expect_err("generation conflict");
    assert_eq!(error.code(), "conflict");
    let after = store.load(&session.session_id).unwrap();
    assert!(after.compaction.is_none());
    assert_eq!(after.messages.len(), session.messages.len() + 2);
}

/// Panics if the summarizer is ever invoked; proves stale attempts fail
/// closed before any provider work.
struct PanicProvider;

#[async_trait::async_trait]
impl ContentGenerator for PanicProvider {
    async fn generate(
        &self,
        _messages: &[Message],
        _tools: &[crate::provider::ToolDeclaration],
        _config: &crate::provider::GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        panic!("provider must not be called for a stale automatic compaction");
    }

    fn cancel(&self) {}
}

async fn compact_expecting(
    store: &SessionStore,
    session: &PersistedSession,
    provider: &dyn ContentGenerator,
    expected: ExpectedRevision,
) -> Result<(CompactionReport, PersistedSession), CompactionError> {
    compact_session(
        store,
        &session.session_id,
        provider,
        &CoreConfig::default(),
        1_000,
        None,
        CompactionTrigger::Auto,
        Some(expected),
    )
    .await
}

#[tokio::test]
async fn stale_generation_fails_closed_without_calling_provider() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let error = compact_expecting(
        &store,
        &session,
        &PanicProvider,
        ExpectedRevision {
            generation: session.generation + 5,
            projection_revision: 0,
        },
    )
    .await
    .expect_err("stale generation");
    assert_eq!(error.code(), "conflict");
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_none());
}

#[tokio::test]
async fn stale_projection_revision_fails_closed_without_calling_provider() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    // The session currently has no projection (revision 0); expecting a
    // later revision means the recommendation is out of date.
    let error = compact_expecting(
        &store,
        &session,
        &PanicProvider,
        ExpectedRevision {
            generation: session.generation,
            projection_revision: 3,
        },
    )
    .await
    .expect_err("stale revision");
    assert_eq!(error.code(), "conflict");
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_none());
}

#[tokio::test]
async fn matching_expected_revision_compacts_normally() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let provider = summary_provider();
    let (report, _) = compact_expecting(
        &store,
        &session,
        &provider,
        ExpectedRevision {
            generation: session.generation,
            projection_revision: 0,
        },
    )
    .await
    .expect("matching revision compacts");
    assert_eq!(report.revision, 1);
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_some());
}

#[tokio::test]
async fn compact_in_memory_reduces_and_respects_disabled_policy() {
    let messages = bulky_messages(5);
    let provider = summary_provider();
    let config = CoreConfig::default();
    let candidate = compact_in_memory(
        &messages,
        None,
        0,
        &provider,
        "mock-model",
        &config,
        0,
        PreservationPolicy::RecentRuns(config.session.compaction.preserve_recent_runs),
    )
    .await
    .expect("in-memory candidate");
    assert!(candidate.compacted_through > 0);
    assert_eq!(candidate.revision, 1);

    let mut disabled = CoreConfig::default();
    disabled.session.compaction.enabled = false;
    let provider = summary_provider();
    let error = compact_in_memory(
        &messages,
        None,
        0,
        &provider,
        "mock-model",
        &disabled,
        0,
        PreservationPolicy::RecentRuns(2),
    )
    .await
    .expect_err("disabled policy");
    assert_eq!(error.code(), "disabled");
}

#[tokio::test]
async fn single_oversized_run_returns_oversized_input_and_keeps_transcript() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    // Run 0 opens with a user message larger than the whole summarizer
    // input budget, so not even one complete run can be rendered without
    // loss. Run 1 is preserved.
    let giant = "x".repeat(200 * 1024);
    let mut session = PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![
            Message::user(&giant),
            Message::assistant("ok"),
            Message::user("second"),
            Message::assistant("done"),
        ],
    );
    store.persist(&mut session).unwrap();
    let mut config = CoreConfig::default();
    config.session.compaction.preserve_recent_runs = 1;
    let provider = summary_provider();
    let error = compact(&store, &session, &provider, &config)
        .await
        .expect_err("oversized single run");
    assert_eq!(error.code(), "oversized_input");
    // The provider is never allowed to commit a lossy projection.
    let after = store.load(&session.session_id).unwrap();
    assert!(after.compaction.is_none());
    assert_eq!(after.messages.len(), session.messages.len());
}

#[tokio::test]
async fn run_exceeding_small_model_window_fails_closed_without_byte_cap_help() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    // ~48 KiB per run: far below the 192 KiB byte ceiling, but well over
    // the token budget of a 4K-window model — only the model-aware bound
    // can reject this request.
    let session = persisted(&store, 4);
    let mut config = CoreConfig::default();
    config.session.compaction.preserve_recent_runs = 1;
    config.session.compaction.model_context_window = Some(4_000);
    let provider = summary_provider();
    let error = compact(&store, &session, &provider, &config)
        .await
        .expect_err("run larger than the model window");
    assert_eq!(error.code(), "oversized_input");
    let after = store.load(&session.session_id).unwrap();
    assert!(after.compaction.is_none());
    assert_eq!(after.messages.len(), session.messages.len());
}

#[test]
fn cut_shrinks_to_largest_run_boundary_inside_the_token_budget() {
    // Three bulky runs (six messages, ~1.5K tokens per message); a budget
    // that fits only four messages must shrink the cut to the run
    // boundary at message 4 — never mid-run, never mid-message.
    let messages = bulky_messages(3);
    let (cut, input) = bounded_cut_and_input(&messages, None, 0, 6, 6_500).expect("shrunk cut");
    assert_eq!(cut, 4);
    // The rendered input covers exactly the committed range.
    assert!(input.contains("prompt 1:"));
    assert!(!input.contains("prompt 2:"));
}

#[test]
fn single_run_over_token_budget_is_oversized_input() {
    let messages = bulky_messages(2);
    let error = bounded_cut_and_input(&messages, None, 0, 2, 1_000).expect_err("one run too large");
    assert!(matches!(error, CompactionError::OversizedInput));
}

/// Records the estimated token size of every summary request it serves.
struct CapturingProvider {
    inner: MockProvider,
    captured: std::sync::Arc<std::sync::Mutex<Vec<u64>>>,
}

#[async_trait::async_trait]
impl ContentGenerator for CapturingProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[crate::provider::ToolDeclaration],
        config: &crate::provider::GenerateConfig,
    ) -> Result<crate::provider::GenerateStream, String> {
        self.captured
            .lock()
            .expect("captured requests lock")
            .push(estimate_messages_tokens(messages));
        self.inner.generate(messages, tools, config).await
    }

    fn cancel(&self) {}
}

#[tokio::test]
async fn summary_request_never_exceeds_a_small_model_window() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    // A large recovered session on a 12K-window model: the request that
    // carries the summary input must itself fit the window with the
    // output reserve subtracted.
    let session = persisted(&store, 8);
    let mut config = CoreConfig::default();
    config.session.compaction.preserve_recent_runs = 1;
    config.session.compaction.model_context_window = Some(12_000);
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let provider = CapturingProvider {
        inner: summary_provider(),
        captured: std::sync::Arc::clone(&captured),
    };
    let (report, _) = compact(&store, &session, &provider, &config)
        .await
        .expect("bounded compaction");
    assert!(report.compacted_through > 0);
    let requests = captured.lock().expect("captured requests lock");
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0] + 2_048 <= 12_000,
        "summary request of ~{} tokens exceeds the 12K window",
        requests[0]
    );
}

/// Locates the persisted envelope for `session_id` under the temp store.
fn session_file(temp: &tempfile::TempDir, session_id: &ProviderSessionId) -> std::path::PathBuf {
    let filename = format!("{session_id}.json");
    let mut pending = vec![temp.path().to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("read session directory") {
            let path = entry.expect("session directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().and_then(|name| name.to_str()) == Some(filename.as_str()) {
                return path;
            }
        }
    }
    panic!("no persisted envelope for session {session_id}");
}

/// Rewrites the persisted envelope through `edit`.
///
/// Tests own this so they can construct damaged or legacy on-disk shapes that
/// the engine itself must never produce; production code only ever sees the
/// typed [`PersistedSession`].
fn edit_envelope<F>(temp: &tempfile::TempDir, session_id: &ProviderSessionId, edit: F)
where
    F: FnOnce(&mut serde_json::Map<String, serde_json::Value>),
{
    let path = session_file(temp, session_id);
    let bytes = std::fs::read(&path).expect("read envelope");
    let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("parse envelope");
    edit(value.as_object_mut().expect("envelope object"));
    std::fs::write(&path, serde_json::to_vec(&value).expect("encode envelope"))
        .expect("write envelope");
}

/// Replaces one field of the stored projection, leaving the rest intact.
fn corrupt_projection_field(
    temp: &tempfile::TempDir,
    session_id: &ProviderSessionId,
    field: &str,
    value: serde_json::Value,
) {
    let field = field.to_string();
    edit_envelope(temp, session_id, move |envelope| {
        let projection = envelope
            .get_mut("compaction")
            .and_then(serde_json::Value::as_object_mut)
            .expect("stored projection");
        projection.insert(field, value);
    });
}

#[tokio::test]
async fn rejected_projection_preserves_revision_monotonicity() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 8);
    let config = CoreConfig::default();

    let provider = summary_provider();
    let (first, _) = compact(&store, &session, &provider, &config)
        .await
        .expect("first compaction");
    assert_eq!(first.revision, 1);

    // An externally rewritten prefix invalidates the digest; the projection is
    // no longer usable but its revision was published once.
    corrupt_projection_field(
        &temp,
        &session.session_id,
        "source_digest",
        serde_json::Value::String("0".repeat(64)),
    );

    let loaded = store
        .load(&session.session_id)
        .expect("load after corruption");
    assert!(
        loaded.compaction.is_none(),
        "an invalid projection must degrade to the complete transcript"
    );
    assert_eq!(
        loaded.compaction_revision, 1,
        "the revision floor must survive a rejected projection"
    );

    // The next successful commit continues the clock instead of reusing 1.
    let provider = summary_provider();
    let (second, _) = compact(&store, &loaded, &provider, &config)
        .await
        .expect("second compaction");
    assert_eq!(second.revision, 2);

    let reloaded = store.load(&session.session_id).expect("reload");
    assert_eq!(
        reloaded.compaction.expect("projection persisted").revision,
        2
    );
    assert_eq!(reloaded.compaction_revision, 2);
}

#[tokio::test]
async fn empty_summary_on_disk_preserves_the_revision_floor() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let provider = summary_provider();
    compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect("first compaction");

    corrupt_projection_field(
        &temp,
        &session.session_id,
        "summary",
        serde_json::Value::String("   \n ".to_string()),
    );
    let loaded = store.load(&session.session_id).expect("load");
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.compaction_revision, 1);
}

#[tokio::test]
async fn future_prompt_version_preserves_the_revision_floor() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let provider = summary_provider();
    compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect("first compaction");

    corrupt_projection_field(
        &temp,
        &session.session_id,
        "prompt_version",
        serde_json::json!(COMPACTION_PROMPT_VERSION + 1),
    );
    let loaded = store.load(&session.session_id).expect("load");
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.compaction_revision, 1);
}

#[tokio::test]
async fn revision_floor_is_the_maximum_of_clock_and_projection() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let provider = summary_provider();
    compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect("first compaction");

    // Projection ahead of the clock: the projection wins.
    corrupt_projection_field(&temp, &session.session_id, "revision", serde_json::json!(7));
    let loaded = store.load(&session.session_id).expect("load");
    assert_eq!(loaded.compaction.expect("valid projection").revision, 7);
    assert_eq!(loaded.compaction_revision, 7);

    // Clock ahead of the projection: the clock wins.
    edit_envelope(&temp, &session.session_id, |envelope| {
        envelope.insert("compaction_revision".to_string(), serde_json::json!(9));
    });
    let loaded = store.load(&session.session_id).expect("load");
    assert_eq!(loaded.compaction.expect("valid projection").revision, 7);
    assert_eq!(loaded.compaction_revision, 9);
}

#[tokio::test]
async fn exhausted_revision_clock_fails_closed_without_calling_provider() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    edit_envelope(&temp, &session.session_id, |envelope| {
        envelope.insert(
            "compaction_revision".to_string(),
            serde_json::json!(u64::MAX),
        );
    });

    let error = compact(&store, &session, &PanicProvider, &CoreConfig::default())
        .await
        .expect_err("exhausted revision clock");
    assert_eq!(error.code(), "revision_exhausted");
    let after = store.load(&session.session_id).expect("load");
    assert!(after.compaction.is_none());
    assert_eq!(after.messages.len(), session.messages.len());
}

#[tokio::test]
async fn provider_failure_does_not_consume_a_revision() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let config = CoreConfig::default();
    let provider = summary_provider();
    compact(&store, &session, &provider, &config)
        .await
        .expect("first compaction");

    let mut grown = store.load(&session.session_id).expect("load committed");
    grown.messages.extend(bulky_messages(3));
    store.persist(&mut grown).expect("grow transcript");

    let failing = MockProvider::partial_error();
    let error = compact(&store, &grown, &failing, &config)
        .await
        .expect_err("provider failure");
    assert_eq!(error.code(), "provider_error");
    assert_eq!(
        store
            .load(&session.session_id)
            .expect("load")
            .compaction_revision,
        1
    );

    // The burned attempt did not advance the clock: the next commit is 2.
    let provider = summary_provider();
    let (report, _) = compact(&store, &grown, &provider, &config)
        .await
        .expect("retry compaction");
    assert_eq!(report.revision, 2);
}

#[tokio::test]
async fn generation_conflict_does_not_consume_a_revision() {
    let temp = tempfile::tempdir().unwrap();
    let persist_dir = temp.path().join("sessions").to_str().unwrap().to_string();
    let store = store(&temp);
    let session = persisted(&store, 4);
    let conflicting = ConflictingProvider {
        persist_dir,
        workspace: temp.path().to_path_buf(),
        session_id: session.session_id.clone(),
        inner: summary_provider(),
    };
    let error = compact(&store, &session, &conflicting, &CoreConfig::default())
        .await
        .expect_err("generation conflict");
    assert_eq!(error.code(), "conflict");
    assert_eq!(
        store
            .load(&session.session_id)
            .expect("load")
            .compaction_revision,
        0
    );

    let provider = summary_provider();
    let (report, _) = compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect("retry compaction");
    assert_eq!(report.revision, 1);
}

#[tokio::test]
async fn emergency_compaction_continues_from_the_saved_revision_floor() {
    let messages = bulky_messages(5);
    let config = CoreConfig::default();

    // Sanitization dropped the projection, so `previous` is `None` while the
    // runtime clock still remembers revision 4.
    let provider = summary_provider();
    let candidate = compact_in_memory(
        &messages,
        None,
        4,
        &provider,
        "mock-model",
        &config,
        0,
        PreservationPolicy::RecentRuns(2),
    )
    .await
    .expect("in-memory candidate");
    assert_eq!(candidate.revision, 5);

    // An exhausted clock takes the existing fail-closed emergency path.
    let provider = summary_provider();
    let error = compact_in_memory(
        &messages,
        None,
        u64::MAX,
        &provider,
        "mock-model",
        &config,
        0,
        PreservationPolicy::RecentRuns(2),
    )
    .await
    .expect_err("exhausted revision clock");
    assert_eq!(error.code(), "revision_exhausted");
}

#[test]
fn runtime_revision_clock_outlives_a_rejected_projection() {
    let mut runtime = crate::compaction::CompactionRuntime::default();
    assert_eq!(runtime.revision(), 0);
    // Loading a session whose projection was rejected keeps the clock.
    runtime.load_state(None, 4);
    assert!(runtime.state().is_none());
    assert_eq!(runtime.revision(), 4);
}

#[tokio::test]
async fn legacy_envelope_recovers_the_floor_from_its_projection() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 8);
    let config = CoreConfig::default();
    let provider = summary_provider();
    compact(&store, &session, &provider, &config)
        .await
        .expect("first compaction");

    // Pre-`compaction_revision` shape: the clock exists only inside the
    // projection, and that projection is no longer valid.
    edit_envelope(&temp, &session.session_id, |envelope| {
        envelope.remove("compaction_revision");
        let projection = envelope
            .get_mut("compaction")
            .and_then(serde_json::Value::as_object_mut)
            .expect("stored projection");
        projection.insert(
            "source_digest".to_string(),
            serde_json::Value::String("0".repeat(64)),
        );
    });

    let loaded = store
        .load(&session.session_id)
        .expect("load legacy envelope");
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.compaction_revision, 1);
    let provider = summary_provider();
    let (report, _) = compact(&store, &loaded, &provider, &config)
        .await
        .expect("compaction after legacy load");
    assert_eq!(report.revision, 2);
}

#[tokio::test]
async fn legacy_envelope_without_projection_starts_at_zero() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 4);
    edit_envelope(&temp, &session.session_id, |envelope| {
        envelope.remove("compaction_revision");
    });

    let loaded = store
        .load(&session.session_id)
        .expect("load legacy envelope");
    assert!(loaded.compaction.is_none());
    assert_eq!(loaded.compaction_revision, 0);
    let provider = summary_provider();
    let (report, _) = compact(&store, &loaded, &provider, &CoreConfig::default())
        .await
        .expect("first compaction");
    assert_eq!(report.revision, 1);
}

#[tokio::test]
async fn normal_auto_trigger_still_protects_two_recent_runs() {
    // The soft-threshold path stays quality-first: with the default policy two
    // complete runs are both protected, so automatic compaction declines even
    // though the same transcript is compactable on request. Only the emergency
    // ladder degrades below this (see `compaction::preflight::tests`).
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let session = persisted(&store, 2);
    let config = CoreConfig::default();
    assert_eq!(config.session.compaction.preserve_recent_runs, 2);
    assert!(
        !super::super::boundary::has_new_compactable_prefix(&session.messages, 2, 0).unwrap(),
        "the idle-boundary preflight must see no eligible prefix"
    );

    let provider = summary_provider();
    let error = compact_session(
        &store,
        &session.session_id,
        &provider,
        &config,
        1_000,
        None,
        CompactionTrigger::Auto,
        None,
    )
    .await
    .expect_err("both runs are protected");
    assert_eq!(error.code(), "nothing_to_compact");
    assert!(store
        .load(&session.session_id)
        .unwrap()
        .compaction
        .is_none());

    // Manual compaction over the very same transcript keeps working.
    let provider = summary_provider();
    let (report, _) = compact(&store, &session, &provider, &config)
        .await
        .expect("manual compaction committed");
    assert!(report.compacted_through > 0);
}

#[tokio::test]
async fn nothing_to_compact_without_a_complete_run() {
    let temp = tempfile::tempdir().unwrap();
    let store = store(&temp);
    let mut session = PersistedSession::new(
        ProviderSessionId::new(),
        store.workspace_scope().to_string(),
        "mock-model".to_string(),
        vec![Message::user("unfinished request")],
    );
    store.persist(&mut session).unwrap();
    let provider = summary_provider();
    let error = compact(&store, &session, &provider, &CoreConfig::default())
        .await
        .expect_err("incomplete run");
    assert_eq!(error.code(), "nothing_to_compact");
}
