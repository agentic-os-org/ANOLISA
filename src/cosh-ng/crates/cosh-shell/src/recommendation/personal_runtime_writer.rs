use super::*;
pub(super) enum WriterCommand {
    Wake,
    Clear {
        now_hour_bucket: u64,
        reply: SyncSender<Result<(), String>>,
    },
    SetEnabled {
        enabled: bool,
        now_hour_bucket: u64,
        reply: SyncSender<Result<bool, String>>,
    },
    MarkNoticeSeen {
        notice_version: u16,
        now_hour_bucket: u64,
        reply: SyncSender<Result<(), String>>,
    },
    SyncHistory {
        marker: NativeBashHistoryMarker,
        expected_owner_uid: u32,
        now_unix_secs: u64,
        host_identity: String,
        live_commands: Vec<LiveShellCommand>,
        reply: SyncSender<Result<(), String>>,
    },
    Shutdown {
        now_hour_bucket: u64,
        budget: Duration,
        reply: SyncSender<Result<usize, String>>,
    },
}

pub(crate) struct PersonalRuntimeWriter {
    pub(super) runtime: Arc<Mutex<PersonalRuntime>>,
    commands: SyncSender<WriterCommand>,
    worker: Option<JoinHandle<()>>,
    contention_drops: SourceCounts,
    enabled: bool,
    feedback_lifecycle: Option<FeedbackLifecycle>,
}

impl PersonalRuntimeWriter {
    pub(super) fn new(
        runtime: Arc<Mutex<PersonalRuntime>>,
        commands: SyncSender<WriterCommand>,
        worker: JoinHandle<()>,
        enabled: bool,
        feedback_lifecycle: Option<FeedbackLifecycle>,
    ) -> Self {
        Self {
            runtime,
            commands,
            worker: Some(worker),
            contention_drops: SourceCounts::default(),
            enabled,
            feedback_lifecycle,
        }
    }

    pub(crate) fn try_sync_native_bash_history(
        &self,
        marker: NativeBashHistoryMarker,
        expected_owner_uid: u32,
        now_unix_secs: u64,
        host_identity: String,
        live_commands: Vec<LiveShellCommand>,
    ) -> Result<Receiver<Result<(), String>>, PersonalRuntimeError> {
        let (reply, receiver) = mpsc::sync_channel(1);
        self.commands
            .try_send(WriterCommand::SyncHistory {
                marker,
                expected_owner_uid,
                now_unix_secs,
                host_identity,
                live_commands,
                reply,
            })
            .map_err(writer_command_error)?;
        Ok(receiver)
    }

    pub(crate) fn session_scope_id(&self) -> Option<String> {
        self.runtime
            .try_lock()
            .ok()?
            .session_scope_id()
            .map(str::to_string)
    }

    pub(crate) fn activity_identity(
        &mut self,
        source: ActivitySource,
        opaque_event_identity: &[u8],
    ) -> Result<Option<ActivityIdentity>, PersonalRuntimeError> {
        if !self.enabled {
            return Ok(None);
        }
        match self.runtime.try_lock() {
            Ok(runtime) => runtime.activity_identity(source, opaque_event_identity),
            Err(TryLockError::WouldBlock) => {
                self.contention_drops.increment(source);
                Ok(None)
            }
            Err(TryLockError::Poisoned(_)) => Ok(None),
        }
    }

    pub(crate) fn build_context(
        &self,
        host_identity: &str,
        cwd: &Path,
        repo_root: Option<&Path>,
        normalized_remote: Option<&str>,
        home: &Path,
    ) -> Option<ActivityContext> {
        if !self.enabled {
            return None;
        }
        self.runtime.try_lock().ok()?.build_context(
            host_identity,
            cwd,
            repo_root,
            normalized_remote,
            home,
        )
    }

    #[cfg(test)]
    pub(crate) fn try_enqueue(&mut self, record: ActivityRecord) -> EnqueueOutcome {
        self.try_enqueue_inner(record, None)
    }

    pub(crate) fn try_enqueue_for_epoch(
        &mut self,
        record: ActivityRecord,
        store_epoch: &str,
    ) -> EnqueueOutcome {
        self.try_enqueue_inner(record, Some(store_epoch))
    }

    pub(crate) fn try_enqueue_identified_deferred(
        &mut self,
        identified: IdentifiedActivityRecord,
    ) -> EnqueueOutcome {
        if !self.enabled {
            return EnqueueOutcome::Inactive;
        }
        let source = identified.record.source;
        match self.runtime.try_lock() {
            Ok(mut runtime)
                if runtime
                    .snapshot()
                    .is_none_or(|state| state.store_epoch != identified.store_epoch) =>
            {
                runtime.dropped_records.increment(source);
                EnqueueOutcome::Dropped
            }
            Ok(mut runtime) => runtime.enqueue(identified.record),
            Err(TryLockError::WouldBlock) => {
                self.contention_drops.increment(source);
                EnqueueOutcome::Dropped
            }
            Err(TryLockError::Poisoned(_)) => {
                self.contention_drops.increment(source);
                EnqueueOutcome::Inactive
            }
        }
    }

    fn try_enqueue_inner(
        &mut self,
        record: ActivityRecord,
        expected_epoch: Option<&str>,
    ) -> EnqueueOutcome {
        if !self.enabled {
            return EnqueueOutcome::Inactive;
        }
        let source = record.source;
        let outcome = match self.runtime.try_lock() {
            Ok(mut runtime)
                if expected_epoch.is_some_and(|expected| {
                    runtime
                        .snapshot()
                        .is_none_or(|state| state.store_epoch != expected)
                }) =>
            {
                runtime.dropped_records.increment(source);
                EnqueueOutcome::Dropped
            }
            Ok(mut runtime) => runtime.enqueue(record),
            Err(TryLockError::WouldBlock) => {
                self.contention_drops.increment(source);
                return EnqueueOutcome::Dropped;
            }
            Err(TryLockError::Poisoned(_)) => {
                self.contention_drops.increment(source);
                return EnqueueOutcome::Inactive;
            }
        };
        if outcome == EnqueueOutcome::Accepted {
            match self.commands.try_send(WriterCommand::Wake) {
                Ok(()) | Err(TrySendError::Full(WriterCommand::Wake)) => {}
                Err(TrySendError::Disconnected(WriterCommand::Wake)) => {
                    return EnqueueOutcome::Inactive;
                }
                Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => unreachable!(),
            }
        }
        outcome
    }

    pub(crate) fn poll_status(&self) -> Option<PersonalRuntimeStatus> {
        let mut status = self.runtime.try_lock().ok()?.status();
        status.enabled = self.enabled;
        status.accepting_records &= self.enabled;
        status.dropped_records.shell_command = status
            .dropped_records
            .shell_command
            .saturating_add(self.contention_drops.shell_command);
        status.dropped_records.agent_request = status
            .dropped_records
            .agent_request
            .saturating_add(self.contention_drops.agent_request);
        status.dropped_records.agent_run = status
            .dropped_records
            .agent_run
            .saturating_add(self.contention_drops.agent_run);
        status.dropped_records.recommendation_feedback = status
            .dropped_records
            .recommendation_feedback
            .saturating_add(self.contention_drops.recommendation_feedback);
        status.dropped_records.bash_history = status
            .dropped_records
            .bash_history
            .saturating_add(self.contention_drops.bash_history);
        Some(status)
    }

    pub(crate) fn poll_snapshot(&self) -> Option<RecommendationState> {
        self.runtime.try_lock().ok()?.snapshot().cloned()
    }

    pub(crate) fn poll_planner_candidates(
        &self,
        now_hour_bucket: u64,
    ) -> Option<Vec<PlannerCandidate>> {
        if !self.enabled {
            return Some(Vec::new());
        }
        let mut runtime = self.runtime.try_lock().ok()?;
        if runtime.reload(now_hour_bucket).is_err() {
            return None;
        }
        Some(runtime.planner_candidates(now_hour_bucket))
    }

    pub(crate) fn arm_frozen_prompt(&mut self, binding: FrozenPromptBinding) -> bool {
        if !self.enabled {
            return false;
        }
        self.feedback_lifecycle = Some(FeedbackLifecycle::new(binding));
        true
    }

    pub(crate) fn accept_frozen_prompt(&mut self) -> Option<FeedbackEvent> {
        self.feedback_lifecycle.as_mut()?.accept()
    }

    pub(crate) fn submit_frozen_prompt(&mut self, final_text: &str) -> Option<FeedbackEvent> {
        let event = self.feedback_lifecycle.as_mut()?.submit(final_text);
        self.feedback_lifecycle = None;
        event
    }

    pub(crate) fn dismiss_frozen_prompt(&mut self) -> Option<FeedbackEvent> {
        let event = self.feedback_lifecycle.as_mut()?.explicit_dismiss();
        if event.is_some() {
            self.feedback_lifecycle = None;
        }
        event
    }

    pub(crate) fn ignore_frozen_prompt(&mut self) -> Option<FeedbackEvent> {
        let event = self.feedback_lifecycle.as_mut()?.ignore();
        self.feedback_lifecycle = None;
        event
    }

    pub(crate) fn clear_frozen_prompt(&mut self) {
        self.feedback_lifecycle = None;
    }

    pub(crate) fn feedback_record(
        &mut self,
        event: FeedbackEvent,
        observed_hour_bucket: u64,
        context: ActivityContext,
        opaque_event_identity: &[u8],
    ) -> Result<Option<IdentifiedActivityRecord>, PersonalRuntimeError> {
        let identity = match self.activity_identity(
            ActivitySource::RecommendationFeedback,
            opaque_event_identity,
        )? {
            Some(identity) => identity,
            None => return Ok(None),
        };
        Ok(Some(IdentifiedActivityRecord {
            store_epoch: identity.store_epoch,
            record: ActivityRecord {
                activity_id: identity.activity_id,
                session_scope_id: self.session_scope_id(),
                source_fingerprint: identity.source_fingerprint,
                observed_hour_bucket,
                source: ActivitySource::RecommendationFeedback,
                context,
                payload: ActivityPayload::RecommendationFeedback {
                    candidate_id: event.candidate_id,
                    candidate_source: event.candidate_source,
                    task_ref: event.task_ref,
                    profile_generation: event.profile_generation,
                    intent_lifecycle_id: event.intent_lifecycle_id,
                    action: event.action,
                    edit_bucket: event.edit_bucket,
                },
                redaction: Default::default(),
                summarized_generation: None,
            },
        }))
    }

    pub(crate) fn clear(
        &mut self,
        now_hour_bucket: u64,
        budget: Duration,
    ) -> Result<(), PersonalRuntimeError> {
        self.feedback_lifecycle = None;
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .try_send(WriterCommand::Clear {
                now_hour_bucket,
                reply,
            })
            .map_err(writer_command_error)?;
        response
            .recv_timeout(budget)
            .map_err(|_| PersonalRuntimeError::Operation("recommendation clear timed out".into()))?
            .map_err(PersonalRuntimeError::Operation)
    }

    pub(crate) fn set_user_enabled(
        &mut self,
        enabled: bool,
        now_hour_bucket: u64,
        budget: Duration,
    ) -> Result<(), PersonalRuntimeError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .try_send(WriterCommand::SetEnabled {
                enabled,
                now_hour_bucket,
                reply,
            })
            .map_err(writer_command_error)?;
        self.enabled = response
            .recv_timeout(budget)
            .map_err(|_| {
                PersonalRuntimeError::Operation("recommendation preference timed out".into())
            })?
            .map_err(PersonalRuntimeError::Operation)?;
        Ok(())
    }

    pub(crate) fn mark_notice_seen(
        &mut self,
        notice_version: u16,
        now_hour_bucket: u64,
        budget: Duration,
    ) -> Result<(), PersonalRuntimeError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .try_send(WriterCommand::MarkNoticeSeen {
                notice_version,
                now_hour_bucket,
                reply,
            })
            .map_err(writer_command_error)?;
        response
            .recv_timeout(budget)
            .map_err(|_| PersonalRuntimeError::Operation("recommendation notice timed out".into()))?
            .map_err(PersonalRuntimeError::Operation)
    }

    pub(crate) fn current_ai_configured(
        &self,
        adapter: &CoshCoreAdapter,
    ) -> Result<bool, PersonalRuntimeError> {
        resolve_auth_state_bounded(adapter)
    }

    pub(crate) fn shutdown(
        &mut self,
        now_hour_bucket: u64,
        budget: Duration,
    ) -> Result<usize, PersonalRuntimeError> {
        self.feedback_lifecycle = None;
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .try_send(WriterCommand::Shutdown {
                now_hour_bucket,
                budget,
                reply,
            })
            .map_err(writer_command_error)?;
        let result = response
            .recv_timeout(budget)
            .map_err(|_| {
                PersonalRuntimeError::Operation("recommendation writer shutdown timed out".into())
            })?
            .map_err(PersonalRuntimeError::Operation)?;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        Ok(result)
    }
}

impl Drop for PersonalRuntimeWriter {
    fn drop(&mut self) {
        self.worker.take();
    }
}

fn writer_command_error<T>(error: TrySendError<T>) -> PersonalRuntimeError {
    match error {
        TrySendError::Full(_) => {
            PersonalRuntimeError::Operation("recommendation writer busy".into())
        }
        TrySendError::Disconnected(_) => {
            PersonalRuntimeError::Operation("recommendation writer stopped".into())
        }
    }
}

fn resolve_auth_state_bounded(adapter: &CoshCoreAdapter) -> Result<bool, PersonalRuntimeError> {
    let adapter = adapter.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("cosh-recommendation-auth".to_string())
        .spawn(move || {
            let _ = sender.send(adapter.ai_configured());
        })
        .map_err(|error| {
            PersonalRuntimeError::Operation(format!("start auth resolver: {error}"))
        })?;
    receiver
        .recv_timeout(FINGERPRINT_RESOLVE_TIMEOUT)
        .map_err(|_| PersonalRuntimeError::Operation("provider state timed out".into()))?
        .map_err(PersonalRuntimeError::Operation)
}

pub(super) fn writer_loop(runtime: Arc<Mutex<PersonalRuntime>>, commands: Receiver<WriterCommand>) {
    let mut consecutive_failures = 0u8;
    let mut retry_at = Instant::now();
    loop {
        let mut should_flush = false;
        match commands.recv_timeout(Duration::from_millis(100)) {
            Ok(WriterCommand::Wake) => should_flush = true,
            Ok(WriterCommand::Clear {
                now_hour_bucket,
                reply,
            }) => {
                let result = runtime
                    .lock()
                    .map_err(|_| "recommendation writer poisoned".to_string())
                    .and_then(|mut runtime| {
                        runtime
                            .clear(now_hour_bucket)
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
                consecutive_failures = 0;
                retry_at = Instant::now();
            }
            Ok(WriterCommand::SetEnabled {
                enabled,
                now_hour_bucket,
                reply,
            }) => {
                let result = runtime
                    .lock()
                    .map_err(|_| "recommendation writer poisoned".to_string())
                    .and_then(|mut runtime| {
                        runtime
                            .set_user_enabled(enabled, now_hour_bucket)
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
                consecutive_failures = 0;
                retry_at = Instant::now();
            }
            Ok(WriterCommand::MarkNoticeSeen {
                notice_version,
                now_hour_bucket,
                reply,
            }) => {
                let result = runtime
                    .lock()
                    .map_err(|_| "recommendation writer poisoned".to_string())
                    .and_then(|mut runtime| {
                        runtime
                            .mark_notice_seen(notice_version, now_hour_bucket)
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
            }
            Ok(WriterCommand::SyncHistory {
                marker,
                expected_owner_uid,
                now_unix_secs,
                host_identity,
                live_commands,
                reply,
            }) => {
                let result = runtime
                    .lock()
                    .map_err(|_| "recommendation writer poisoned".to_string())
                    .and_then(|mut runtime| {
                        runtime
                            .sync_native_bash_history(
                                &marker,
                                expected_owner_uid,
                                now_unix_secs,
                                &host_identity,
                                &live_commands,
                            )
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
            }
            Ok(WriterCommand::Shutdown {
                now_hour_bucket,
                budget,
                reply,
            }) => {
                let result = runtime
                    .lock()
                    .map_err(|_| "recommendation writer poisoned".to_string())
                    .and_then(|mut runtime| {
                        runtime
                            .shutdown(now_hour_bucket, budget)
                            .map_err(|error| error.to_string())
                    });
                let _ = reply.send(result);
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => should_flush = true,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !should_flush || Instant::now() < retry_at {
            continue;
        }
        let now_hour_bucket = current_hour_bucket();
        let result = runtime
            .lock()
            .map_err(|_| ())
            .and_then(|mut runtime| runtime.flush_once(now_hour_bucket).map_err(|_| ()));
        match result {
            Ok(FlushOutcome::Persisted(_))
            | Ok(FlushOutcome::Idle)
            | Ok(FlushOutcome::StaleEpochDropped(_)) => {
                consecutive_failures = 0;
                retry_at = Instant::now();
            }
            Ok(FlushOutcome::Deferred) | Err(()) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                let delay = match consecutive_failures {
                    1 => Duration::from_millis(25),
                    2 => Duration::from_millis(100),
                    3 => Duration::from_millis(500),
                    _ => Duration::from_secs(5 * 60),
                };
                retry_at = Instant::now() + delay;
            }
        }
    }
}

pub(super) fn current_hour_bucket() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 3_600
}
