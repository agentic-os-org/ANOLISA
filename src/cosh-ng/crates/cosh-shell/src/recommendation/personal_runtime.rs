use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, TryLockError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::adapter::CoshCoreAdapter;

use super::personal_context::{build_activity_context, build_host_id};
use super::personal_crypto::{hex, hmac_sha256, random_hex, CryptoError};
use super::personal_feedback::{FeedbackEvent, FeedbackLifecycle, FrozenPromptBinding};
use super::personal_history::{
    sync_native_bash_history, HistoryControl, HistorySyncResult, HistorySyncState,
    LiveShellCommand, NativeBashHistoryMarker,
};
use super::personal_model::{
    resolve_recommendations_enabled, ActivityContext, ActivityOutcome, ActivityPayload,
    ActivityRecord, ActivitySource, AnalyzerLease, CachedPromptCandidate, FeedbackAction,
    RecommendationCache, RecommendationFeedbackState, RecommendationState, UserWorkProfile,
    DISCLOSURE_VERSION,
};
use super::personal_planner::PlannerCandidate;
use super::personal_process::{
    analyzer_process_is_gone, verified_terminate_process_group, ProcessGroupIdentity,
};
use super::personal_store::{PersonalStore, StateVersion, StoreError};

const MAX_QUEUE_ITEMS: usize = 20;
const MAX_QUEUE_BYTES: usize = 128 * 1024;
const IMPRESSION_SUPPRESSION_HOURS: u64 = 8;
const SUBMITTED_SUPPRESSION_HOURS: u64 = 2;
const MAX_FEEDBACK_STATES: usize = 20;
const FINGERPRINT_RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CLEAR_CAS_ATTEMPTS: usize = 3;

#[derive(Debug)]
pub(crate) enum PersonalRuntimeError {
    Store(StoreError),
    Crypto(CryptoError),
    Operation(String),
    Serialize,
}

impl fmt::Display for PersonalRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => error.fmt(formatter),
            Self::Crypto(error) => error.fmt(formatter),
            Self::Operation(error) => formatter.write_str(error),
            Self::Serialize => formatter.write_str("recommendation activity serialization failed"),
        }
    }
}

impl std::error::Error for PersonalRuntimeError {}

impl From<StoreError> for PersonalRuntimeError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<CryptoError> for PersonalRuntimeError {
    fn from(error: CryptoError) -> Self {
        Self::Crypto(error)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SourceCounts {
    pub(crate) shell_command: u64,
    pub(crate) agent_request: u64,
    pub(crate) agent_run: u64,
    pub(crate) recommendation_feedback: u64,
    pub(crate) bash_history: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersonalRuntimeStatus {
    pub(crate) enabled: bool,
    pub(crate) accepting_records: bool,
    pub(crate) persisted_records: usize,
    pub(crate) queued_records: usize,
    pub(crate) queued_bytes: usize,
    pub(crate) dropped_records: SourceCounts,
    pub(crate) store_errors: u64,
    pub(crate) profile_generation: u64,
    pub(crate) cached_candidates: usize,
    pub(crate) last_summary_hour_bucket: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnqueueOutcome {
    Accepted,
    Dropped,
    Inactive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlushOutcome {
    Idle,
    Persisted(usize),
    Deferred,
    StaleEpochDropped(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityIdentity {
    pub(crate) activity_id: String,
    pub(crate) source_fingerprint: String,
    pub(crate) store_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IdentifiedActivityRecord {
    pub(crate) record: ActivityRecord,
    pub(crate) store_epoch: String,
}

#[derive(Debug, Clone)]
struct QueuedRecord {
    record: ActivityRecord,
    serialized_bytes: usize,
}

pub(crate) struct PersonalRuntime {
    enabled: bool,
    configured_enabled: bool,
    environment_override: Option<bool>,
    accepting_records: bool,
    store: Option<PersonalStore>,
    state: Option<RecommendationState>,
    epoch_key: Option<[u8; 32]>,
    session_scope_id: Option<String>,
    queue: VecDeque<QueuedRecord>,
    queue_bytes: usize,
    dropped_records: SourceCounts,
    store_errors: u64,
    feedback_lifecycle: Option<FeedbackLifecycle>,
}

#[path = "personal_runtime_helpers.rs"]
mod helpers;
#[path = "personal_runtime_lifecycle.rs"]
mod lifecycle;
#[path = "personal_runtime_writer.rs"]
mod writer;
use helpers::*;
#[cfg(test)]
use writer::current_hour_bucket;
use writer::writer_loop;
pub(crate) use writer::PersonalRuntimeWriter;

impl Default for PersonalRuntime {
    fn default() -> Self {
        Self::inert()
    }
}

impl PersonalRuntime {
    pub(crate) fn inert() -> Self {
        Self {
            enabled: false,
            configured_enabled: false,
            environment_override: None,
            accepting_records: false,
            store: None,
            state: None,
            epoch_key: None,
            session_scope_id: None,
            queue: VecDeque::new(),
            queue_bytes: 0,
            dropped_records: SourceCounts::default(),
            store_errors: 0,
            feedback_lifecycle: None,
        }
    }

    pub(crate) fn activity_identity(
        &self,
        source: ActivitySource,
        opaque_event_identity: &[u8],
    ) -> Result<Option<ActivityIdentity>, PersonalRuntimeError> {
        if !self.enabled || !self.accepting_records {
            return Ok(None);
        }
        let key = self
            .epoch_key
            .as_ref()
            .ok_or(PersonalRuntimeError::Serialize)?;
        let mut material = Vec::with_capacity(1 + opaque_event_identity.len());
        material.push(source_tag(source));
        material.extend_from_slice(opaque_event_identity);
        Ok(Some(ActivityIdentity {
            activity_id: random_hex(16)?,
            source_fingerprint: hex(&hmac_sha256(key, &material)),
            store_epoch: self
                .state
                .as_ref()
                .map(|state| state.store_epoch.clone())
                .ok_or(PersonalRuntimeError::Serialize)?,
        }))
    }

    pub(crate) fn enqueue(&mut self, record: ActivityRecord) -> EnqueueOutcome {
        if !self.enabled || !self.accepting_records {
            return EnqueueOutcome::Inactive;
        }
        let source = record.source;
        let Ok(bytes) = serde_json::to_vec(&record).map(|bytes| bytes.len()) else {
            self.dropped_records.increment(source);
            return EnqueueOutcome::Dropped;
        };
        if bytes > MAX_QUEUE_BYTES {
            self.dropped_records.increment(source);
            return EnqueueOutcome::Dropped;
        }
        self.queue.push_back(QueuedRecord {
            record,
            serialized_bytes: bytes,
        });
        self.queue_bytes = self.queue_bytes.saturating_add(bytes);
        let mut incoming_retained = true;
        while self.queue.len() > MAX_QUEUE_ITEMS || self.queue_bytes > MAX_QUEUE_BYTES {
            let index = self
                .queue
                .iter()
                .position(|item| is_weak(&item.record))
                .unwrap_or(0);
            let removed = self.queue.remove(index).expect("queue index must exist");
            if index == self.queue.len() {
                incoming_retained = false;
            }
            self.queue_bytes = self.queue_bytes.saturating_sub(removed.serialized_bytes);
            self.dropped_records.increment(removed.record.source);
        }
        if incoming_retained {
            EnqueueOutcome::Accepted
        } else {
            EnqueueOutcome::Dropped
        }
    }

    pub(crate) fn flush_once(
        &mut self,
        now_hour_bucket: u64,
    ) -> Result<FlushOutcome, PersonalRuntimeError> {
        if self.queue.is_empty() || self.store.is_none() || self.state.is_none() {
            return Ok(FlushOutcome::Idle);
        }
        let batch = self
            .queue
            .iter()
            .map(|item| item.record.clone())
            .collect::<Vec<_>>();
        let base = StateVersion::of(self.state.as_ref().expect("state checked above"));
        let store = self.store.as_ref().expect("store checked above");
        match store.merge(&base, now_hour_bucket, |state| {
            merge_records(state, &batch, now_hour_bucket)
        }) {
            Ok(state) => {
                let persisted = batch.len();
                for _ in 0..persisted {
                    if let Some(record) = self.queue.pop_front() {
                        self.queue_bytes = self.queue_bytes.saturating_sub(record.serialized_bytes);
                    }
                }
                self.state = Some(state);
                Ok(FlushOutcome::Persisted(persisted))
            }
            Err(StoreError::StaleState) => self.handle_stale_state(now_hour_bucket),
            Err(error) => {
                self.store_errors = self.store_errors.saturating_add(1);
                Err(error.into())
            }
        }
    }

    pub(crate) fn flush_bounded(
        &mut self,
        now_hour_bucket: u64,
        budget: Duration,
    ) -> Result<usize, PersonalRuntimeError> {
        let deadline = Instant::now() + budget;
        let mut persisted = 0usize;
        while !self.queue.is_empty() && Instant::now() < deadline {
            match self.flush_once(now_hour_bucket)? {
                FlushOutcome::Persisted(count) => persisted = persisted.saturating_add(count),
                FlushOutcome::StaleEpochDropped(_) | FlushOutcome::Idle => break,
                FlushOutcome::Deferred => continue,
            }
        }
        Ok(persisted)
    }

    pub(crate) fn reload(&mut self, now_hour_bucket: u64) -> Result<(), PersonalRuntimeError> {
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        let Some(next) = store.load(now_hour_bucket)? else {
            return Ok(());
        };
        let epoch_changed = self
            .state
            .as_ref()
            .is_some_and(|current| current.store_epoch != next.store_epoch);
        let next_epoch_key = if epoch_changed {
            Some(store.epoch_key(&next.store_epoch)?)
        } else {
            None
        };
        if epoch_changed {
            self.drop_all_queued();
            self.feedback_lifecycle = None;
            self.epoch_key = next_epoch_key;
        }
        self.state = Some(next);
        Ok(())
    }

    pub(crate) fn clear(&mut self, now_hour_bucket: u64) -> Result<(), PersonalRuntimeError> {
        self.drop_all_queued();
        self.feedback_lifecycle = None;
        let Some(store) = self.store.as_ref() else {
            return Ok(());
        };
        let state = clear_store_with_retry(store, now_hour_bucket, stop_observed_analyzer)?;
        self.epoch_key = Some(store.epoch_key(&state.store_epoch)?);
        self.state = Some(state);
        Ok(())
    }

    pub(crate) fn set_user_enabled(
        &mut self,
        requested: bool,
        now_hour_bucket: u64,
    ) -> Result<bool, PersonalRuntimeError> {
        if requested && self.environment_override == Some(false) {
            return Err(PersonalRuntimeError::Operation(
                "COSH_RECOMMENDATIONS_ENABLED=0 forces recommendations off".into(),
            ));
        }
        let Some(store) = self.store.as_ref() else {
            return Err(PersonalRuntimeError::Operation(
                "recommendation runtime is inactive".into(),
            ));
        };
        let state = store.set_user_enabled(requested, now_hour_bucket)?;
        let enabled = resolve_recommendations_enabled(
            self.environment_override,
            state.preferences.user_enabled,
            self.configured_enabled,
        );
        let epoch_changed = self
            .state
            .as_ref()
            .is_some_and(|current| current.store_epoch != state.store_epoch);
        let next_epoch_key = if epoch_changed {
            Some(store.epoch_key(&state.store_epoch)?)
        } else {
            None
        };
        if epoch_changed || !enabled {
            self.drop_all_queued();
            self.feedback_lifecycle = None;
        }
        if epoch_changed {
            self.epoch_key = next_epoch_key;
        }
        self.enabled = enabled;
        self.accepting_records =
            enabled && state.preferences.notice_version_seen >= DISCLOSURE_VERSION;
        self.state = Some(state);
        Ok(enabled)
    }

    pub(crate) fn mark_notice_seen(
        &mut self,
        notice_version: u16,
        now_hour_bucket: u64,
    ) -> Result<(), PersonalRuntimeError> {
        let Some(store) = self.store.as_ref() else {
            return Err(PersonalRuntimeError::Operation(
                "recommendation runtime is inactive".into(),
            ));
        };
        self.state = Some(store.mark_notice_seen(notice_version, now_hour_bucket)?);
        self.accepting_records = self.enabled && notice_version >= DISCLOSURE_VERSION;
        Ok(())
    }

    pub(crate) fn shutdown(
        &mut self,
        now_hour_bucket: u64,
        budget: Duration,
    ) -> Result<usize, PersonalRuntimeError> {
        let persisted = self.flush_bounded(now_hour_bucket, budget)?;
        self.accepting_records = false;
        self.feedback_lifecycle = None;
        Ok(persisted)
    }

    #[cfg(test)]
    pub(crate) fn freeze_prompt(&mut self, binding: FrozenPromptBinding) -> Option<FeedbackEvent> {
        if !self.enabled || !self.accepting_records {
            return None;
        }
        let mut lifecycle = FeedbackLifecycle::new(binding);
        let impression = lifecycle.impression();
        self.feedback_lifecycle = Some(lifecycle);
        impression
    }

    #[cfg(test)]
    pub(crate) fn accept_frozen_prompt(&mut self) -> Option<FeedbackEvent> {
        self.feedback_lifecycle.as_mut()?.accept()
    }

    pub(crate) fn planner_candidates(&self, now_hour_bucket: u64) -> Vec<PlannerCandidate> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        state
            .cache
            .candidates
            .iter()
            .map(|candidate| planner_candidate(candidate, &state.feedback, now_hour_bucket))
            .collect()
    }

    pub(crate) fn apply_history_result(
        &mut self,
        result: &HistorySyncResult,
        records: &[ActivityRecord],
        now_hour_bucket: u64,
    ) -> Result<(), PersonalRuntimeError> {
        if result.delete_history_derived {
            self.remove_queued_source(ActivitySource::BashHistory);
        }
        self.update_state(now_hour_bucket, |state| {
            if result.delete_history_derived {
                clear_history_derived(state);
            }
            merge_records(state, records, now_hour_bucket);
            state.journal.history_cursor = result.cursor.clone();
            state.journal.history_baseline_pending = result.baseline_pending;
        })
    }

    fn sync_native_bash_history(
        &mut self,
        marker: &NativeBashHistoryMarker,
        expected_owner_uid: u32,
        now_unix_secs: u64,
        host_identity: &str,
        live_commands: &[LiveShellCommand],
    ) -> Result<(), PersonalRuntimeError> {
        if !self.enabled || !self.accepting_records {
            return Ok(());
        }
        let Some(state) = self.state.as_ref() else {
            return Ok(());
        };
        let sync_state = HistorySyncState {
            cursor: state.journal.history_cursor.clone(),
            baseline_pending: state.journal.history_baseline_pending,
        };
        let key = self.epoch_key.ok_or(PersonalRuntimeError::Serialize)?;
        let host_id = build_host_id(&key, host_identity);
        let result = sync_native_bash_history(
            HistoryControl::Enabled,
            Some(marker),
            expected_owner_uid,
            now_unix_secs,
            &sync_state,
            live_commands,
            |material| hex(&hmac_sha256(&key, material)),
        )
        .map_err(|error| {
            PersonalRuntimeError::Operation(format!("history sync failed: {error:?}"))
        })?;
        let now_hour_bucket = now_unix_secs / 3600;
        let mut records = Vec::with_capacity(result.imported.len());
        for entry in &result.imported {
            let material = format!(
                "{}\0{}",
                entry.execution_hour_bucket.unwrap_or_default(),
                entry.command
            );
            let Some(identity) =
                self.activity_identity(ActivitySource::BashHistory, material.as_bytes())?
            else {
                return Ok(());
            };
            records.push(ActivityRecord {
                activity_id: identity.activity_id,
                session_scope_id: None,
                source_fingerprint: identity.source_fingerprint,
                observed_hour_bucket: entry.execution_hour_bucket.unwrap_or(now_hour_bucket),
                source: ActivitySource::BashHistory,
                context: ActivityContext {
                    host_id: host_id.clone(),
                    ..ActivityContext::default()
                },
                payload: ActivityPayload::BashHistoryCommand {
                    command: entry.command.clone(),
                    origin_unverified: true,
                    execution_hour_bucket: entry.execution_hour_bucket,
                    time_unverified: entry.time_unverified,
                },
                redaction: Default::default(),
                summarized_generation: None,
            });
        }
        self.apply_history_result(&result, &records, now_hour_bucket)
    }

    #[cfg(test)]
    pub(crate) fn reset_history_derived(
        &mut self,
        now_hour_bucket: u64,
    ) -> Result<(), PersonalRuntimeError> {
        self.remove_queued_source(ActivitySource::BashHistory);
        self.update_state(now_hour_bucket, |state| {
            clear_history_derived(state);
            state.journal.history_cursor = None;
            state.journal.history_baseline_pending = true;
        })
    }

    fn update_state(
        &mut self,
        now_hour_bucket: u64,
        update: impl Fn(&mut RecommendationState),
    ) -> Result<(), PersonalRuntimeError> {
        let (Some(store), Some(state)) = (self.store.as_ref(), self.state.as_ref()) else {
            return Ok(());
        };
        let base = StateVersion::of(state);
        match store.merge(&base, now_hour_bucket, update) {
            Ok(next) => {
                self.state = Some(next);
                Ok(())
            }
            Err(error) => {
                self.store_errors = self.store_errors.saturating_add(1);
                Err(error.into())
            }
        }
    }

    fn handle_stale_state(
        &mut self,
        now_hour_bucket: u64,
    ) -> Result<FlushOutcome, PersonalRuntimeError> {
        self.store_errors = self.store_errors.saturating_add(1);
        let prior_epoch = self
            .state
            .as_ref()
            .map(|state| state.store_epoch.clone())
            .unwrap_or_default();
        let store = self.store.as_ref().expect("store exists while flushing");
        let Some(next) = store.load(now_hour_bucket)? else {
            return Ok(FlushOutcome::Deferred);
        };
        if next.store_epoch == prior_epoch {
            self.state = Some(next);
            return Ok(FlushOutcome::Deferred);
        }
        let next_epoch_key = store.epoch_key(&next.store_epoch)?;
        let dropped = self.queue.len();
        self.drop_all_queued();
        self.feedback_lifecycle = None;
        self.epoch_key = Some(next_epoch_key);
        self.state = Some(next);
        Ok(FlushOutcome::StaleEpochDropped(dropped))
    }

    fn drop_all_queued(&mut self) {
        while let Some(item) = self.queue.pop_front() {
            self.dropped_records.increment(item.record.source);
        }
        self.queue_bytes = 0;
    }

    fn remove_queued_source(&mut self, source: ActivitySource) {
        let mut retained = VecDeque::with_capacity(self.queue.len());
        while let Some(item) = self.queue.pop_front() {
            if item.record.source == source {
                self.queue_bytes = self.queue_bytes.saturating_sub(item.serialized_bytes);
                self.dropped_records.increment(source);
            } else {
                retained.push_back(item);
            }
        }
        self.queue = retained;
    }
}

fn stop_observed_analyzer(lease: Option<&AnalyzerLease>) -> Result<(), PersonalRuntimeError> {
    let Some(lease) = lease else {
        return Ok(());
    };
    if let (Some(leader_pid), Some(leader_start_identity), Some(process_group_id)) = (
        lease.core_leader_pid,
        lease.core_leader_start_identity.clone(),
        lease.core_process_group_id,
    ) {
        let identity = ProcessGroupIdentity {
            owner_pid: lease.owner_pid,
            owner_start_identity: lease.owner_start_identity.clone(),
            leader_pid,
            leader_start_identity,
            process_group_id,
        };
        if !verified_terminate_process_group(&identity) && !analyzer_process_is_gone(&identity) {
            return Err(PersonalRuntimeError::Operation(
                "active recommendation analyzer could not be stopped".into(),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "personal_runtime_tests.rs"]
mod tests;
