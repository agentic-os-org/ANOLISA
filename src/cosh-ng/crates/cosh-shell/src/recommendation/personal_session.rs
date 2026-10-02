use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::adapter::AdapterInstance;
use crate::recommendation::personal_analysis_runtime::{
    spawn_analyzer_worker, AnalyzerTriggerContext, AnalyzerWorkerRequest,
};
use crate::recommendation::personal_history::{LiveShellCommand, NativeBashHistoryMarker};
use crate::recommendation::personal_model::{ActivityPayload, FeedbackAction, DISCLOSURE_VERSION};
use crate::recommendation::personal_scheduler::SessionGate;
use crate::runtime::state::InlineState;
use crate::types::CommandOrigin;

const ANALYZER_IDLE_DELAY: Duration = Duration::from_secs(3);

pub(crate) fn request_retry_after_auth(state: &mut InlineState) {
    state.personalization.request_analyzer_retry();
}

pub(crate) fn poll_personal_session(
    state: &mut InlineState,
    adapter: &AdapterInstance,
    foreground_idle: bool,
) {
    if let Some(cancellation) = state.personalization.analyzer_cancellation.as_ref() {
        cancellation.set_foreground_idle(foreground_idle);
    }
    state.personalization.poll_ready();
    state.personalization.poll_history_file();
    poll_history_sync(state);
    poll_analyzer_worker(state);
    if !state.rendered_startup_banner {
        state.personalization.idle_since = None;
        return;
    }
    if !continuous_idle_ready(
        &mut state.personalization.idle_since,
        foreground_idle,
        Instant::now(),
    ) {
        return;
    }
    if state.analysis_mode == crate::runtime::state::AnalysisMode::Manual {
        return;
    }
    sync_history_once(state);
    start_analyzer_once(state, adapter);
}

fn sync_history_once(state: &mut InlineState) {
    if !state.personalization.bash_history {
        return;
    }
    let (Some(path), Some(writer)) = (
        state.personalization.history_file.clone(),
        state.personalization.writer.as_ref(),
    ) else {
        return;
    };
    if state.personalization.history_sync_pending.is_some()
        || state.personalization.history_synced_path.as_ref() == Some(&path)
        || state
            .personalization
            .history_retry_after
            .as_ref()
            .is_some_and(|(retry_path, retry_at)| retry_path == &path && Instant::now() < *retry_at)
    {
        return;
    }
    let live_commands = state
        .session_blocks
        .iter()
        .filter(|block| {
            matches!(
                block.origin,
                CommandOrigin::UserInteractive
                    | CommandOrigin::UserSendToShell
                    | CommandOrigin::UserAnalysisAction
            )
        })
        .map(|block| LiveShellCommand {
            command: block.command.clone(),
            observed_hour_bucket: block.ended_at_ms / 3_600_000,
        })
        .collect();
    let host_identity = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default();
    if let Ok(receiver) = writer.try_sync_native_bash_history(
        NativeBashHistoryMarker::new(path.clone()),
        unsafe { nix::libc::geteuid() },
        now_unix_secs(),
        host_identity,
        live_commands,
    ) {
        state.personalization.history_sync_pending = Some((path, receiver));
    }
}

fn poll_history_sync(state: &mut InlineState) {
    let Some((path, receiver)) = state.personalization.history_sync_pending.take() else {
        return;
    };
    match receiver.try_recv() {
        Ok(Ok(())) => {
            state.personalization.history_synced_path = Some(path);
            state.personalization.history_retry_after = None;
        }
        Ok(Err(_)) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            state.personalization.history_retry_after =
                Some((path, Instant::now() + Duration::from_secs(300)));
        }
        Err(std::sync::mpsc::TryRecvError::Empty) => {
            state.personalization.history_sync_pending = Some((path, receiver));
        }
    }
}

fn poll_analyzer_worker(state: &mut InlineState) {
    let Some(worker) = state.personalization.analyzer_worker.take() else {
        return;
    };
    if !worker.is_finished() {
        state.personalization.analyzer_worker = Some(worker);
        return;
    }
    match worker.join() {
        Ok(result) if !result.session_gate.can_attempt() => {
            state.personalization.analyzer_started = true;
        }
        Ok(_) => {}
        Err(_) => {
            state.personalization.analyzer_started = true;
        }
    }
}

fn start_analyzer_once(state: &mut InlineState, adapter: &AdapterInstance) {
    if state.personalization.analyzer_started
        || state.personalization.analyzer_worker.is_some()
        || state.analysis_mode == crate::runtime::state::AnalysisMode::Manual
        || state.personalization.ai_disabled
    {
        return;
    }
    let AdapterInstance::CoshCore(core) = adapter else {
        return;
    };
    let Some(writer) = state.personalization.writer.as_ref() else {
        return;
    };
    let Some(status) = writer.poll_status() else {
        return;
    };
    if !status.enabled {
        return;
    }
    let Some(session_scope_id) = writer.session_scope_id() else {
        return;
    };
    let Some(snapshot) = writer.poll_snapshot() else {
        return;
    };
    if snapshot.preferences.notice_version_seen < DISCLOSURE_VERSION {
        return;
    }
    if state.personalization.analyzer_last_attempt_generation == Some(snapshot.generation) {
        return;
    }
    if !snapshot
        .journal
        .records
        .iter()
        .any(|record| eligible_analyzer_trigger(record, &session_scope_id))
    {
        return;
    }
    let Some(root) = state.personalization.store_root.clone() else {
        return;
    };
    let Some(model) = state.personalization.foreground_model.clone() else {
        return;
    };
    let cancellation = state
        .personalization
        .analyzer_cancellation
        .clone()
        .unwrap_or_default();
    let foreground_activity_epoch = cancellation.foreground_activity_epoch();
    let request = AnalyzerWorkerRequest::new(
        true,
        root,
        core.clone(),
        SessionGate::default(),
        session_scope_id,
        now_unix_secs(),
        AnalyzerTriggerContext {
            has_eligible_trigger: true,
            foreground_idle: true,
            foreground_activity_epoch,
        },
        model,
    )
    .with_cancellation(cancellation);
    state.personalization.analyzer_last_attempt_generation = Some(snapshot.generation);
    state.personalization.analyzer_worker = Some(spawn_analyzer_worker(request));
}

fn continuous_idle_ready(since: &mut Option<Instant>, idle: bool, now: Instant) -> bool {
    if !idle {
        *since = None;
        return false;
    }
    let started = since.get_or_insert(now);
    now.saturating_duration_since(*started) >= ANALYZER_IDLE_DELAY
}

fn eligible_analyzer_trigger(
    record: &crate::recommendation::personal_model::ActivityRecord,
    session_scope_id: &str,
) -> bool {
    if record.summarized_generation.is_some()
        || record.session_scope_id.as_deref() != Some(session_scope_id)
    {
        return false;
    }
    match &record.payload {
        ActivityPayload::AgentRequest { .. } => true,
        ActivityPayload::RecommendationFeedback { action, .. } => {
            *action == FeedbackAction::Submitted
        }
        ActivityPayload::ShellCommand { .. }
        | ActivityPayload::AgentRun { .. }
        | ActivityPayload::BashHistoryCommand { .. } => false,
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommendation::personal_analysis_runtime::{
        AnalyzerRunBlock, AnalyzerRunOutcome, AnalyzerWorkerResult,
    };
    use crate::recommendation::personal_model::{
        ActivityContext, ActivityOutcome, ActivityRecord, ActivitySource, AgentRequestBindingKind,
        CandidateSource, RedactionReport, ShellActivityOrigin,
    };

    fn record(session: &str, payload: ActivityPayload) -> ActivityRecord {
        ActivityRecord {
            activity_id: "activity".to_string(),
            session_scope_id: Some(session.to_string()),
            source_fingerprint: "fingerprint".to_string(),
            observed_hour_bucket: 1,
            source: ActivitySource::AgentRequest,
            context: ActivityContext::default(),
            payload,
            redaction: RedactionReport::default(),
            summarized_generation: None,
        }
    }

    #[test]
    fn only_current_session_request_or_submitted_feedback_triggers_analysis() {
        let request = ActivityPayload::AgentRequest {
            text: "inspect service".to_string(),
            binding: AgentRequestBindingKind::FreeForm,
            context_command_activity_id: None,
            intent_lifecycle_id: "intent".to_string(),
            system_recommended_skill: None,
        };
        assert!(eligible_analyzer_trigger(
            &record("current", request.clone()),
            "current"
        ));
        assert!(!eligible_analyzer_trigger(
            &record("old", request),
            "current"
        ));

        for action in [
            FeedbackAction::Impression,
            FeedbackAction::TabAccepted,
            FeedbackAction::ExplicitDismissed,
            FeedbackAction::Overridden,
            FeedbackAction::Ignored,
        ] {
            assert!(!eligible_analyzer_trigger(
                &record("current", feedback(action)),
                "current"
            ));
        }
        assert!(eligible_analyzer_trigger(
            &record("current", feedback(FeedbackAction::Submitted)),
            "current"
        ));
    }

    #[test]
    fn shell_failure_never_triggers_analysis_by_itself() {
        let shell = ActivityPayload::ShellCommand {
            command: "make".to_string(),
            origin: ShellActivityOrigin::Interactive,
            parent_request_activity_id: None,
            outcome: ActivityOutcome::Failure,
        };
        assert!(!eligible_analyzer_trigger(
            &record("current", shell),
            "current"
        ));
    }

    #[test]
    fn analyzer_requires_three_continuous_idle_seconds() {
        let now = Instant::now();
        let mut since = None;

        assert!(!continuous_idle_ready(&mut since, true, now));
        assert!(!continuous_idle_ready(
            &mut since,
            true,
            now + Duration::from_millis(2_999)
        ));
        assert!(continuous_idle_ready(
            &mut since,
            true,
            now + Duration::from_secs(3)
        ));
        assert!(!continuous_idle_ready(
            &mut since,
            false,
            now + Duration::from_secs(4)
        ));
        assert!(since.is_none());
        assert!(!continuous_idle_ready(
            &mut since,
            true,
            now + Duration::from_secs(5)
        ));
    }

    #[test]
    fn zero_body_worker_waits_for_an_explicit_change_before_retry() {
        let mut state = InlineState {
            personalization: crate::recommendation::personal_state::PersonalizationState {
                analyzer_last_attempt_generation: Some(7),
                analyzer_worker: Some(std::thread::spawn(|| AnalyzerWorkerResult {
                    outcome: AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::AuthNotConfigured),
                    session_gate: SessionGate::default(),
                })),
                ..Default::default()
            },
            ..InlineState::default()
        };
        while state
            .personalization
            .analyzer_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            std::thread::yield_now();
        }

        poll_analyzer_worker(&mut state);

        assert!(!state.personalization.analyzer_started);
        assert_eq!(
            state.personalization.analyzer_last_attempt_generation,
            Some(7)
        );
        request_retry_after_auth(&mut state);
        assert_eq!(state.personalization.analyzer_last_attempt_generation, None);
    }

    #[test]
    fn explicit_retry_reopens_a_consumed_session_attempt() {
        let mut state = InlineState {
            personalization: crate::recommendation::personal_state::PersonalizationState {
                analyzer_started: true,
                analyzer_last_attempt_generation: Some(7),
                ..Default::default()
            },
            ..InlineState::default()
        };

        state.personalization.request_analyzer_retry();

        assert!(!state.personalization.analyzer_started);
        assert_eq!(state.personalization.analyzer_last_attempt_generation, None);
    }

    #[test]
    fn body_sent_worker_consumes_the_session_even_when_joined_later() {
        let mut gate = SessionGate::default();
        gate.mark_body_sent();
        let mut state = InlineState {
            personalization: crate::recommendation::personal_state::PersonalizationState {
                analyzer_worker: Some(std::thread::spawn(move || AnalyzerWorkerResult {
                    outcome: AnalyzerRunOutcome::Completed,
                    session_gate: gate,
                })),
                ..Default::default()
            },
            ..InlineState::default()
        };
        while state
            .personalization
            .analyzer_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            std::thread::yield_now();
        }

        poll_analyzer_worker(&mut state);

        assert!(state.personalization.analyzer_started);
    }

    #[test]
    fn history_sync_requires_a_successful_background_commit() {
        let history_file = std::path::PathBuf::from("/tmp/history");
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(Err("disk busy".to_string())).unwrap();
        let mut state = InlineState {
            personalization: crate::recommendation::personal_state::PersonalizationState {
                history_file: Some(history_file.clone()),
                history_sync_pending: Some((history_file.clone(), receiver)),
                ..Default::default()
            },
            ..InlineState::default()
        };

        poll_history_sync(&mut state);

        assert!(state.personalization.history_synced_path.is_none());
        assert!(state.personalization.history_retry_after.is_some());

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender.send(Ok(())).unwrap();
        state.personalization.history_sync_pending = Some((history_file.clone(), receiver));
        poll_history_sync(&mut state);
        assert_eq!(
            state.personalization.history_synced_path,
            Some(history_file)
        );
        assert!(state.personalization.history_retry_after.is_none());
    }

    #[test]
    fn history_sync_serializes_path_switches_until_pending_commit() {
        let first_path = std::path::PathBuf::from("/tmp/first-history");
        let pending_path = std::path::PathBuf::from("/tmp/pending-history");
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let mut state = InlineState {
            personalization: crate::recommendation::personal_state::PersonalizationState {
                history_file: Some(first_path.clone()),
                history_synced_path: Some(first_path.clone()),
                history_sync_pending: Some((pending_path.clone(), receiver)),
                ..Default::default()
            },
            ..InlineState::default()
        };

        poll_history_sync(&mut state);

        assert_eq!(
            state
                .personalization
                .history_sync_pending
                .as_ref()
                .map(|(path, _)| path),
            Some(&pending_path)
        );

        sender.send(Ok(())).unwrap();
        poll_history_sync(&mut state);

        assert_eq!(
            state.personalization.history_synced_path,
            Some(pending_path)
        );
        assert_ne!(
            state.personalization.history_synced_path,
            state.personalization.history_file
        );
        assert!(state.personalization.history_sync_pending.is_none());
    }

    fn feedback(action: FeedbackAction) -> ActivityPayload {
        ActivityPayload::RecommendationFeedback {
            candidate_id: "candidate".to_string(),
            candidate_source: CandidateSource::RecentTask,
            task_ref: "task".to_string(),
            profile_generation: 1,
            intent_lifecycle_id: "intent".to_string(),
            action,
            edit_bucket: None,
        }
    }
}
