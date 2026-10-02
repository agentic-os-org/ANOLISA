use super::*;

pub(super) trait RuntimeStore {
    fn initialize(&self, now_hour: u64) -> Result<RecommendationState, ()>;
    fn commit(
        &self,
        base: &StateVersion,
        next: RecommendationState,
        now_hour: u64,
    ) -> Result<RecommendationState, ()>;
}

pub(super) struct ProductionStore(pub(super) PersonalStore);

impl RuntimeStore for ProductionStore {
    fn initialize(&self, now_hour: u64) -> Result<RecommendationState, ()> {
        self.0.initialize(now_hour).map_err(|_| ())
    }

    fn commit(
        &self,
        base: &StateVersion,
        next: RecommendationState,
        now_hour: u64,
    ) -> Result<RecommendationState, ()> {
        self.0.commit(base, next, now_hour).map_err(|_| ())
    }
}

pub(super) trait RuntimeProcess: AnalyzerProcess {
    fn leader_pid(&self) -> u32;
    fn cancellation_failed(&self) -> bool;
}

impl RuntimeProcess for CoshCoreAnalyzerProcess {
    fn leader_pid(&self) -> u32 {
        self.process_group_id()
    }

    fn cancellation_failed(&self) -> bool {
        self.cancellation_failed()
    }
}

pub(super) trait RuntimeDependencies {
    type Process: RuntimeProcess;

    fn spawn(&mut self, command: RunnerCommand) -> Result<Self::Process, ProcessFailure>;
    fn next_id(&mut self, prefix: &str) -> Result<String, ()>;
    fn process_identity(&self, pid: u32) -> Option<String>;
    fn claim_body_write(&self, expected_epoch: u64) -> bool;
    fn release_body_write(&self);
    fn analyzer_model(&self) -> Option<&str> {
        None
    }
    fn wait_for_writer(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
    fn register_running(&self, _running: RunningAnalyzer) {}
    fn clear_running(&self, _leader_pid: u32, _identity: &str) {}
}

pub(super) struct ProductionDependencies<'a> {
    pub(super) adapter: &'a CoshCoreAdapter,
    pub(super) cancellation: AnalyzerCancellation,
    pub(super) model: &'a str,
}

impl RuntimeDependencies for ProductionDependencies<'_> {
    type Process = CoshCoreAnalyzerProcess;

    fn spawn(&mut self, mut command: RunnerCommand) -> Result<Self::Process, ProcessFailure> {
        command.program = self.adapter.program.clone();
        CoshCoreAnalyzerProcess::spawn(command)
    }

    fn next_id(&mut self, prefix: &str) -> Result<String, ()> {
        random_hex(16)
            .map(|value| format!("{prefix}-{value}"))
            .map_err(|_| ())
    }

    fn process_identity(&self, pid: u32) -> Option<String> {
        process_start_identity(pid)
    }

    fn claim_body_write(&self, expected_epoch: u64) -> bool {
        self.cancellation.claim_body_write(expected_epoch)
    }

    fn release_body_write(&self) {
        self.cancellation.release_body_write();
    }

    fn analyzer_model(&self) -> Option<&str> {
        Some(self.model)
    }

    fn register_running(&self, running: RunningAnalyzer) {
        self.cancellation.register(running);
    }

    fn clear_running(&self, leader_pid: u32, identity: &str) {
        self.cancellation.clear(leader_pid, identity);
    }
}

struct IdentityLiveness;

impl LeaseLiveness for IdentityLiveness {
    fn owner_alive(&self, lease: &crate::recommendation::personal_model::AnalyzerLease) -> bool {
        identity_matches(lease.owner_pid, &lease.owner_start_identity)
    }

    fn core_alive(&self, lease: &crate::recommendation::personal_model::AnalyzerLease) -> bool {
        match (
            lease.core_leader_pid,
            lease.core_leader_start_identity.as_deref(),
            lease.core_process_group_id,
        ) {
            (Some(leader_pid), Some(leader_start_identity), Some(process_group_id)) => {
                !analyzer_process_is_gone(&ProcessGroupIdentity {
                    owner_pid: lease.owner_pid,
                    owner_start_identity: lease.owner_start_identity.clone(),
                    leader_pid,
                    leader_start_identity: leader_start_identity.to_string(),
                    process_group_id,
                })
            }
            _ => false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn orchestrate_once<S: RuntimeStore, D: RuntimeDependencies>(
    enabled: bool,
    store: &S,
    dependencies: &mut D,
    session_gate: &mut SessionGate,
    session_scope_id: &str,
    now_unix_secs: u64,
    trigger: AnalyzerTriggerContext,
) -> AnalyzerRunOutcome {
    if let Err(block) = check_dispatch(
        enabled,
        trigger.has_eligible_trigger,
        trigger.foreground_idle,
        *session_gate,
    ) {
        return AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::Scheduler(block));
    }
    if session_scope_id.is_empty() || session_scope_id.len() > 128 {
        return failed(AnalyzerFailureStage::Input, false);
    }

    let now_hour = now_unix_secs / 3600;
    let mut state = match store.initialize(now_hour) {
        Ok(state) => state,
        Err(()) => return failed(AnalyzerFailureStage::Store, false),
    };
    let initial_epoch = state.store_epoch.clone();
    for poll in 0..=WRITER_FLUSH_POLLS {
        if has_session_trigger(&state, session_scope_id) {
            break;
        }
        if poll == WRITER_FLUSH_POLLS {
            return AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::Scheduler(
                SchedulerBlock::NoTrigger,
            ));
        }
        dependencies.wait_for_writer(WRITER_FLUSH_POLL_INTERVAL);
        state = match store.initialize(now_hour) {
            Ok(state) if state.store_epoch == initial_epoch => state,
            Ok(_) => return failed(AnalyzerFailureStage::StateTransition, false),
            Err(()) => return failed(AnalyzerFailureStage::Store, false),
        };
    }
    if state.preferences.notice_version_seen < DISCLOSURE_VERSION {
        return AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::NoticeRequired);
    }
    let owner_pid = std::process::id();
    let owner_identity = match dependencies.process_identity(owner_pid) {
        Some(identity) => identity,
        None => return failed(AnalyzerFailureStage::Identity, false),
    };

    let attempt_id = match dependencies.next_id("attempt") {
        Ok(id) => id,
        Err(()) => return failed(AnalyzerFailureStage::Identity, false),
    };
    let lease_nonce = match dependencies.next_id("lease") {
        Ok(id) => id,
        Err(()) => return failed(AnalyzerFailureStage::Identity, false),
    };
    let reservation = Reservation {
        attempt_id: attempt_id.clone(),
        owner_session_id: session_scope_id.to_string(),
        lease_nonce: lease_nonce.clone(),
        owner_pid,
        owner_start_identity: owner_identity.clone(),
        core_leader_pid: None,
        core_leader_start_identity: None,
        core_process_group_id: None,
        base_epoch: state.store_epoch.clone(),
        base_generation: state.generation,
    };
    if let Err(block) = reserve_attempt(
        &mut state.scheduler,
        now_unix_secs,
        reservation,
        &IdentityLiveness,
    ) {
        return AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::Scheduler(block));
    }
    if persist(store, &mut state, now_hour).is_err() {
        return failed(AnalyzerFailureStage::Store, false);
    }

    let input = match build_input(&state, session_scope_id, now_hour) {
        Ok(input) => input,
        Err(_) => {
            return rollback_failure(
                store,
                &mut state,
                now_hour,
                &attempt_id,
                &lease_nonce,
                AnalyzerFailureStage::Input,
            )
        }
    };
    let prompt = match build_fixed_prompt(ANALYZER_SCHEMA) {
        Ok(prompt) => prompt,
        Err(_) => {
            return rollback_failure(
                store,
                &mut state,
                now_hour,
                &attempt_id,
                &lease_nonce,
                AnalyzerFailureStage::Input,
            )
        }
    };
    if validate_provider_budget(&prompt, &input).is_err() {
        return rollback_failure(
            store,
            &mut state,
            now_hour,
            &attempt_id,
            &lease_nonce,
            AnalyzerFailureStage::Input,
        );
    }
    let body = format!("{prompt}\nINPUT:\n{}", input.json);
    if body.len() > MAX_PROVIDER_INPUT_BYTES {
        return rollback_failure(
            store,
            &mut state,
            now_hour,
            &attempt_id,
            &lease_nonce,
            AnalyzerFailureStage::Input,
        );
    }

    let temp = match EmptyDirectory::create(dependencies) {
        Ok(temp) => temp,
        Err(()) => {
            return rollback_failure(
                store,
                &mut state,
                now_hour,
                &attempt_id,
                &lease_nonce,
                AnalyzerFailureStage::Process,
            )
        }
    };
    let mut process = match dependencies.spawn(analyzer_command(
        temp.path.clone(),
        dependencies.analyzer_model(),
    )) {
        Ok(process) => process,
        Err(_) => {
            return rollback_failure(
                store,
                &mut state,
                now_hour,
                &attempt_id,
                &lease_nonce,
                AnalyzerFailureStage::Process,
            )
        }
    };
    session_gate.mark_child_started();
    let core_pid = process.leader_pid();
    let core_identity = match dependencies.process_identity(core_pid) {
        Some(identity) => identity,
        None => {
            return rollback_failure(
                store,
                &mut state,
                now_hour,
                &attempt_id,
                &lease_nonce,
                AnalyzerFailureStage::Identity,
            )
        }
    };
    let Some(lease) = state.scheduler.lease.as_mut().filter(|lease| {
        lease.lease_nonce == lease_nonce && lease.owner_session_id == session_scope_id
    }) else {
        return rollback_failure(
            store,
            &mut state,
            now_hour,
            &attempt_id,
            &lease_nonce,
            AnalyzerFailureStage::StateTransition,
        );
    };
    lease.core_leader_pid = Some(core_pid);
    lease.core_leader_start_identity = Some(core_identity.clone());
    lease.core_process_group_id = Some(core_pid);
    if persist(store, &mut state, now_hour).is_err() {
        return rollback_failure(
            store,
            &mut state,
            now_hour,
            &attempt_id,
            &lease_nonce,
            AnalyzerFailureStage::Store,
        );
    }

    dependencies.register_running(RunningAnalyzer {
        owner_pid,
        owner_start_identity: owner_identity,
        owner_session_id: session_scope_id.to_string(),
        lease_nonce: lease_nonce.clone(),
        leader_pid: core_pid,
        leader_start_identity: core_identity.clone(),
        process_group_id: core_pid,
        store_epoch: state.store_epoch.clone(),
    });
    let body_epoch = state.store_epoch.clone();
    let body_claimed = std::cell::Cell::new(false);
    let result = run_initialized_with_body_hooks(
        &mut process,
        &body,
        || {
            let mut latest = store.initialize(now_hour)?;
            if latest.store_epoch != body_epoch {
                return Err(());
            }
            if !dependencies.claim_body_write(trigger.foreground_activity_epoch) {
                return Err(());
            }
            body_claimed.set(true);
            mark_body_write_started(&mut latest.scheduler, &attempt_id).map_err(|_| ())?;
            persist(store, &mut latest, now_hour)
        },
        || {
            let mut latest = store.initialize(now_hour)?;
            if latest.store_epoch != body_epoch {
                return Err(());
            }
            mark_body_sent(&mut latest.scheduler, &attempt_id).map_err(|_| ())?;
            match persist(store, &mut latest, now_hour) {
                Ok(()) => {
                    state = latest;
                    session_gate.mark_body_sent();
                    Ok(())
                }
                Err(()) => Err(()),
            }
        },
    );
    if body_claimed.get() {
        dependencies.release_body_write();
    }
    if process.cancellation_failed() {
        return failed(
            AnalyzerFailureStage::Process,
            result.as_ref().err().is_some_and(|error| error.body_sent()),
        );
    }
    dependencies.clear_running(core_pid, &core_identity);
    let output = match result {
        Ok(output) => output,
        Err(error) if !error.body_sent() => {
            if attempt_phase(store, now_hour, &state.store_epoch, &attempt_id)
                == Some(AttemptPhase::BodyWriteStarted)
            {
                finish_after_body(store, &mut state, now_hour, &attempt_id, &lease_nonce);
                return failed(AnalyzerFailureStage::Provider, true);
            }
            if matches!(error, RunnerError::AuthRequired { .. }) {
                if let Ok(mut latest) = store.initialize(now_hour) {
                    if latest.store_epoch == state.store_epoch
                        && rollback_zero_body(&mut latest.scheduler, &attempt_id, &lease_nonce)
                            .is_ok()
                    {
                        let _ = persist(store, &mut latest, now_hour);
                    }
                }
                return AnalyzerRunOutcome::Blocked(AnalyzerRunBlock::AuthNotConfigured);
            }
            return rollback_failure(
                store,
                &mut state,
                now_hour,
                &attempt_id,
                &lease_nonce,
                AnalyzerFailureStage::Provider,
            );
        }
        Err(_) => {
            session_gate.mark_body_sent();
            finish_after_body(store, &mut state, now_hour, &attempt_id, &lease_nonce);
            return failed(AnalyzerFailureStage::Provider, true);
        }
    };

    let mut merge_state = match store.initialize(now_hour) {
        Ok(latest) if latest.store_epoch == input.base_epoch => latest,
        _ => return failed(AnalyzerFailureStage::StateTransition, true),
    };
    let mut ids = DependencyIds { dependencies };
    let patch = match prepare_merge_patch(&output, &input, &merge_state, &mut ids) {
        Ok(patch) => patch,
        Err(_) => {
            finish_after_body(store, &mut merge_state, now_hour, &attempt_id, &lease_nonce);
            return failed(AnalyzerFailureStage::Output, true);
        }
    };
    let merge_base = StateVersion::of(&merge_state);
    if apply_merge_patch(&mut merge_state, patch).is_err()
        || finish_attempt(&mut merge_state.scheduler, &attempt_id, &lease_nonce).is_err()
    {
        return failed(AnalyzerFailureStage::StateTransition, true);
    }
    merge_state = match store.commit(&merge_base, merge_state, now_hour) {
        Ok(state) => state,
        Err(()) => return failed(AnalyzerFailureStage::StateTransition, true),
    };
    debug_assert!(merge_state.scheduler.lease.is_none());
    AnalyzerRunOutcome::Completed
}

fn persist(
    store: &impl RuntimeStore,
    state: &mut RecommendationState,
    now_hour: u64,
) -> Result<(), ()> {
    *state = store.commit(&StateVersion::of(state), state.clone(), now_hour)?;
    Ok(())
}

fn has_session_trigger(state: &RecommendationState, session_scope_id: &str) -> bool {
    state.journal.records.iter().any(|record| {
        record.summarized_generation.is_none()
            && record.session_scope_id.as_deref() == Some(session_scope_id)
            && matches!(
                record.payload,
                ActivityPayload::AgentRequest { .. }
                    | ActivityPayload::RecommendationFeedback {
                        action: FeedbackAction::Submitted,
                        ..
                    }
            )
    })
}

fn rollback_failure(
    store: &impl RuntimeStore,
    state: &mut RecommendationState,
    now_hour: u64,
    attempt_id: &str,
    lease_nonce: &str,
    stage: AnalyzerFailureStage,
) -> AnalyzerRunOutcome {
    let mut latest = match store.initialize(now_hour) {
        Ok(latest) if latest.store_epoch == state.store_epoch => latest,
        _ => return failed(AnalyzerFailureStage::StateTransition, false),
    };
    if rollback_zero_body(&mut latest.scheduler, attempt_id, lease_nonce).is_err()
        || persist(store, &mut latest, now_hour).is_err()
    {
        failed(AnalyzerFailureStage::StateTransition, false)
    } else {
        failed(stage, false)
    }
}

fn finish_after_body(
    store: &impl RuntimeStore,
    state: &mut RecommendationState,
    now_hour: u64,
    attempt_id: &str,
    lease_nonce: &str,
) {
    let Ok(mut latest) = store.initialize(now_hour) else {
        return;
    };
    let phase = latest
        .scheduler
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .map(|attempt| attempt.phase);
    let body_sent = match phase {
        Some(AttemptPhase::BodySent) => true,
        Some(AttemptPhase::BodyWriteStarted) => {
            mark_body_sent(&mut latest.scheduler, attempt_id).is_ok()
        }
        _ => false,
    };
    if latest.store_epoch == state.store_epoch
        && body_sent
        && finish_attempt(&mut latest.scheduler, attempt_id, lease_nonce).is_ok()
    {
        let _ = persist(store, &mut latest, now_hour);
    }
}

fn attempt_phase(
    store: &impl RuntimeStore,
    now_hour: u64,
    store_epoch: &str,
    attempt_id: &str,
) -> Option<AttemptPhase> {
    let state = store.initialize(now_hour).ok()?;
    (state.store_epoch == store_epoch).then_some(())?;
    state
        .scheduler
        .attempts
        .iter()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .map(|attempt| attempt.phase)
}

pub(super) fn failed(stage: AnalyzerFailureStage, body_sent: bool) -> AnalyzerRunOutcome {
    AnalyzerRunOutcome::Failed { stage, body_sent }
}

struct DependencyIds<'a, D> {
    dependencies: &'a mut D,
}

impl<D: RuntimeDependencies> LocalIdSource for DependencyIds<'_, D> {
    fn next_id(&mut self, prefix: &str) -> String {
        self.dependencies
            .next_id(prefix)
            .unwrap_or_else(|_| format!("{prefix}-unavailable"))
    }
}
