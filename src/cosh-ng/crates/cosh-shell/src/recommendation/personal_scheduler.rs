use super::personal_model::{AnalyzerAttempt, AnalyzerLease, AnalyzerSchedulerState, AttemptPhase};

pub(crate) const ANALYZER_COOLDOWN_SECS: u64 = 30 * 60;
pub(crate) const ANALYZER_ROLLING_WINDOW_SECS: u64 = 24 * 60 * 60;
pub(crate) const MAX_ATTEMPTS_PER_WINDOW: usize = 3;
pub(crate) const ANALYZER_LEASE_SECS: u64 = 22;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SessionGate {
    child_started: bool,
    body_sent: bool,
}

impl SessionGate {
    pub(crate) fn can_attempt(self) -> bool {
        !self.child_started
    }

    pub(crate) fn mark_child_started(&mut self) {
        self.child_started = true;
    }

    pub(crate) fn mark_body_sent(&mut self) {
        self.child_started = true;
        self.body_sent = true;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reservation {
    pub(crate) attempt_id: String,
    pub(crate) owner_session_id: String,
    pub(crate) lease_nonce: String,
    pub(crate) owner_pid: u32,
    pub(crate) owner_start_identity: String,
    pub(crate) core_leader_pid: Option<u32>,
    pub(crate) core_leader_start_identity: Option<String>,
    pub(crate) core_process_group_id: Option<u32>,
    pub(crate) base_epoch: String,
    pub(crate) base_generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SchedulerBlock {
    Disabled,
    NoTrigger,
    ForegroundBusy,
    SessionConsumed,
    ClockRollback,
    Cooldown,
    DailyLimit,
    LeaseHeld,
    DuplicateAttempt,
    AttemptMissing,
    InvalidPhase,
    LeaseMismatch,
}

pub(crate) trait LeaseLiveness {
    fn owner_alive(&self, lease: &AnalyzerLease) -> bool;
    fn core_alive(&self, lease: &AnalyzerLease) -> bool;
}

pub(crate) fn check_dispatch(
    enabled: bool,
    has_eligible_trigger: bool,
    foreground_idle: bool,
    session: SessionGate,
) -> Result<(), SchedulerBlock> {
    if !enabled {
        return Err(SchedulerBlock::Disabled);
    }
    if !has_eligible_trigger {
        return Err(SchedulerBlock::NoTrigger);
    }
    if !foreground_idle {
        return Err(SchedulerBlock::ForegroundBusy);
    }
    if !session.can_attempt() {
        return Err(SchedulerBlock::SessionConsumed);
    }
    Ok(())
}

pub(crate) fn reserve_attempt(
    state: &mut AnalyzerSchedulerState,
    now_unix_secs: u64,
    reservation: Reservation,
    liveness: &impl LeaseLiveness,
) -> Result<(), SchedulerBlock> {
    let mut next = state.clone();
    if next
        .last_attempt_unix_secs
        .is_some_and(|last| now_unix_secs < last)
        || next
            .attempts
            .iter()
            .any(|attempt| attempt.reserved_unix_secs > now_unix_secs)
    {
        return Err(SchedulerBlock::ClockRollback);
    }
    if let Some(lease) = &next.lease {
        let reclaimable = now_unix_secs >= lease.expires_unix_secs
            && !liveness.owner_alive(lease)
            && !liveness.core_alive(lease);
        if !reclaimable {
            return Err(SchedulerBlock::LeaseHeld);
        }
        let reserved_unix_secs = lease.expires_unix_secs.saturating_sub(ANALYZER_LEASE_SECS);
        next.attempts.retain(|attempt| {
            attempt.phase != AttemptPhase::Reserved
                || attempt.reserved_unix_secs != reserved_unix_secs
        });
        next.last_attempt_unix_secs = next
            .attempts
            .iter()
            .map(|attempt| attempt.reserved_unix_secs)
            .max();
        next.lease = None;
    }
    if next
        .last_attempt_unix_secs
        .is_some_and(|last| now_unix_secs - last < ANALYZER_COOLDOWN_SECS)
    {
        return Err(SchedulerBlock::Cooldown);
    }
    next.attempts.retain(|attempt| {
        now_unix_secs - attempt.reserved_unix_secs < ANALYZER_ROLLING_WINDOW_SECS
    });
    if next.attempts.len() >= MAX_ATTEMPTS_PER_WINDOW {
        return Err(SchedulerBlock::DailyLimit);
    }
    if next
        .attempts
        .iter()
        .any(|attempt| attempt.attempt_id == reservation.attempt_id)
    {
        return Err(SchedulerBlock::DuplicateAttempt);
    }

    next.attempts.push(AnalyzerAttempt {
        attempt_id: reservation.attempt_id,
        reserved_unix_secs: now_unix_secs,
        phase: AttemptPhase::Reserved,
    });
    next.last_attempt_unix_secs = Some(now_unix_secs);
    next.lease = Some(AnalyzerLease {
        owner_session_id: reservation.owner_session_id,
        lease_nonce: reservation.lease_nonce,
        owner_pid: reservation.owner_pid,
        owner_start_identity: reservation.owner_start_identity,
        core_leader_pid: reservation.core_leader_pid,
        core_leader_start_identity: reservation.core_leader_start_identity,
        core_process_group_id: reservation.core_process_group_id,
        base_epoch: reservation.base_epoch,
        base_generation: reservation.base_generation,
        expires_unix_secs: now_unix_secs.saturating_add(ANALYZER_LEASE_SECS),
    });
    *state = next;
    Ok(())
}

pub(crate) fn mark_body_write_started(
    state: &mut AnalyzerSchedulerState,
    attempt_id: &str,
) -> Result<(), SchedulerBlock> {
    let attempt = state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .ok_or(SchedulerBlock::AttemptMissing)?;
    if attempt.phase != AttemptPhase::Reserved {
        return Err(SchedulerBlock::InvalidPhase);
    }
    attempt.phase = AttemptPhase::BodyWriteStarted;
    Ok(())
}

pub(crate) fn mark_body_sent(
    state: &mut AnalyzerSchedulerState,
    attempt_id: &str,
) -> Result<(), SchedulerBlock> {
    let attempt = state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .ok_or(SchedulerBlock::AttemptMissing)?;
    if attempt.phase != AttemptPhase::BodyWriteStarted {
        return Err(SchedulerBlock::InvalidPhase);
    }
    attempt.phase = AttemptPhase::BodySent;
    Ok(())
}

pub(crate) fn finish_attempt(
    state: &mut AnalyzerSchedulerState,
    attempt_id: &str,
    lease_nonce: &str,
) -> Result<(), SchedulerBlock> {
    ensure_lease(state, lease_nonce)?;
    let attempt = state
        .attempts
        .iter_mut()
        .find(|attempt| attempt.attempt_id == attempt_id)
        .ok_or(SchedulerBlock::AttemptMissing)?;
    if attempt.phase != AttemptPhase::BodySent {
        return Err(SchedulerBlock::InvalidPhase);
    }
    attempt.phase = AttemptPhase::Finished;
    state.lease = None;
    Ok(())
}

pub(crate) fn rollback_zero_body(
    state: &mut AnalyzerSchedulerState,
    attempt_id: &str,
    lease_nonce: &str,
) -> Result<(), SchedulerBlock> {
    ensure_lease(state, lease_nonce)?;
    let Some(index) = state
        .attempts
        .iter()
        .position(|attempt| attempt.attempt_id == attempt_id)
    else {
        return Err(SchedulerBlock::AttemptMissing);
    };
    if state.attempts[index].phase != AttemptPhase::Reserved {
        return Err(SchedulerBlock::InvalidPhase);
    }
    state.attempts.remove(index);
    state.last_attempt_unix_secs = state
        .attempts
        .iter()
        .map(|attempt| attempt.reserved_unix_secs)
        .max();
    state.lease = None;
    Ok(())
}

fn ensure_lease(state: &AnalyzerSchedulerState, lease_nonce: &str) -> Result<(), SchedulerBlock> {
    if state
        .lease
        .as_ref()
        .is_some_and(|lease| lease.lease_nonce == lease_nonce)
    {
        Ok(())
    } else {
        Err(SchedulerBlock::LeaseMismatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recommendation::personal_model::{AnalyzerSchedulerState, AttemptPhase};

    #[test]
    fn session_gate_is_consumed_after_analyzer_child_starts() {
        let mut gate = SessionGate::default();

        assert!(gate.can_attempt());
        assert!(gate.can_attempt());
        gate.mark_child_started();
        assert!(!gate.can_attempt());
        assert_eq!(
            check_dispatch(true, true, true, gate),
            Err(SchedulerBlock::SessionConsumed)
        );
        assert_eq!(
            check_dispatch(true, true, false, SessionGate::default()),
            Err(SchedulerBlock::ForegroundBusy)
        );
    }

    #[test]
    fn reserve_enforces_cooldown_rolling_limit_and_clock_rollback() {
        let mut state = AnalyzerSchedulerState::default();
        let liveness = DeadProcesses;
        reserve_attempt(&mut state, 10_000, request("a"), &liveness).expect("first");
        rollback_zero_body(&mut state, "a", "nonce-a").expect("preflight rollback");
        reserve_attempt(&mut state, 10_000, request("a2"), &liveness).expect("retry");
        mark_body_write_started(&mut state, "a2").expect("body write started");
        mark_body_sent(&mut state, "a2").expect("body sent");
        finish_attempt(&mut state, "a2", "nonce-a2").expect("finished");

        assert_eq!(
            reserve_attempt(&mut state, 11_799, request("b"), &liveness),
            Err(SchedulerBlock::Cooldown)
        );
        reserve_attempt(&mut state, 11_800, request("b"), &liveness).expect("boundary");
        mark_body_write_started(&mut state, "b").expect("body write started");
        mark_body_sent(&mut state, "b").expect("body sent");
        finish_attempt(&mut state, "b", "nonce-b").expect("finished");
        reserve_attempt(&mut state, 13_600, request("c"), &liveness).expect("third");
        mark_body_write_started(&mut state, "c").expect("body write started");
        mark_body_sent(&mut state, "c").expect("body sent");
        finish_attempt(&mut state, "c", "nonce-c").expect("finished");

        assert_eq!(
            reserve_attempt(&mut state, 15_400, request("d"), &liveness),
            Err(SchedulerBlock::DailyLimit)
        );
        assert_eq!(
            reserve_attempt(&mut state, 13_599, request("rollback"), &liveness),
            Err(SchedulerBlock::ClockRollback)
        );
    }

    #[test]
    fn attempt_at_exact_rolling_window_boundary_is_pruned() {
        let mut state = AnalyzerSchedulerState {
            attempts: vec![
                attempt("old", 100),
                attempt("middle", 1_900),
                attempt("recent", 3_700),
            ],
            last_attempt_unix_secs: Some(3_700),
            lease: None,
        };

        reserve_attempt(&mut state, 86_500, request("new"), &DeadProcesses)
            .expect("window boundary");

        assert!(!state
            .attempts
            .iter()
            .any(|attempt| attempt.attempt_id == "old"));
        assert_eq!(state.attempts.len(), 3);
    }

    #[test]
    fn lease_reclaim_requires_expiry_and_both_processes_dead() {
        let mut state = AnalyzerSchedulerState::default();
        reserve_attempt(&mut state, 100, request("a"), &DeadProcesses).expect("reserve");
        assert_eq!(state.lease.as_ref().expect("lease").expires_unix_secs, 122);

        assert_eq!(
            reserve_attempt(&mut state, 121, request("b"), &DeadProcesses),
            Err(SchedulerBlock::LeaseHeld)
        );
        assert_eq!(
            reserve_attempt(&mut state, 122, request("b"), &LiveOwner),
            Err(SchedulerBlock::LeaseHeld)
        );

        reserve_attempt(&mut state, 122, request("b"), &DeadProcesses).expect("reclaim");
        assert_eq!(state.lease.as_ref().expect("lease").lease_nonce, "nonce-b");
        assert_eq!(state.attempts.len(), 1);
        assert_eq!(state.attempts[0].attempt_id, "b");
        assert_eq!(state.last_attempt_unix_secs, Some(122));
    }

    #[test]
    fn attempt_phases_include_body_write_linearization() {
        let mut state = AnalyzerSchedulerState::default();
        reserve_attempt(&mut state, 100, request("a"), &DeadProcesses).expect("reserve");
        assert_eq!(state.attempts[0].phase, AttemptPhase::Reserved);
        mark_body_write_started(&mut state, "a").expect("body write started");
        assert_eq!(state.attempts[0].phase, AttemptPhase::BodyWriteStarted);
        mark_body_sent(&mut state, "a").expect("body sent");
        assert_eq!(state.attempts[0].phase, AttemptPhase::BodySent);
        finish_attempt(&mut state, "a", "nonce-a").expect("finished");
        assert_eq!(state.attempts[0].phase, AttemptPhase::Finished);
        assert!(state.lease.is_none());
    }

    #[test]
    fn preflight_lease_has_no_core_identity_and_reclaims_as_core_dead() {
        let mut state = AnalyzerSchedulerState::default();
        let mut preflight = request("preflight");
        preflight.core_leader_pid = None;
        preflight.core_leader_start_identity = None;
        preflight.core_process_group_id = None;
        reserve_attempt(&mut state, 100, preflight, &DeadProcesses).expect("reserve preflight");

        let lease = state.lease.as_ref().expect("preflight lease");
        assert!(lease.core_leader_pid.is_none());
        assert!(lease.core_leader_start_identity.is_none());
        assert!(lease.core_process_group_id.is_none());

        reserve_attempt(
            &mut state,
            100 + ANALYZER_COOLDOWN_SECS,
            request("next"),
            &DeadProcesses,
        )
        .expect("expired preflight lease is reclaimable");
        assert_eq!(state.attempts.last().unwrap().attempt_id, "next");
    }

    fn request(id: &str) -> Reservation {
        Reservation {
            attempt_id: id.to_string(),
            owner_session_id: "session".to_string(),
            lease_nonce: format!("nonce-{id}"),
            owner_pid: 10,
            owner_start_identity: "owner-start".to_string(),
            core_leader_pid: Some(20),
            core_leader_start_identity: Some("core-start".to_string()),
            core_process_group_id: Some(20),
            base_epoch: "epoch".to_string(),
            base_generation: 3,
        }
    }

    fn attempt(id: &str, reserved_unix_secs: u64) -> AnalyzerAttempt {
        AnalyzerAttempt {
            attempt_id: id.to_string(),
            reserved_unix_secs,
            phase: AttemptPhase::Finished,
        }
    }

    struct DeadProcesses;

    impl LeaseLiveness for DeadProcesses {
        fn owner_alive(&self, _lease: &AnalyzerLease) -> bool {
            false
        }

        fn core_alive(&self, _lease: &AnalyzerLease) -> bool {
            false
        }
    }

    struct LiveOwner;

    impl LeaseLiveness for LiveOwner {
        fn owner_alive(&self, _lease: &AnalyzerLease) -> bool {
            true
        }

        fn core_alive(&self, _lease: &AnalyzerLease) -> bool {
            false
        }
    }
}
