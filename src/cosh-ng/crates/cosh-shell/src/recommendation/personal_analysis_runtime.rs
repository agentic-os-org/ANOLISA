use std::fs;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::adapter::CoshCoreAdapter;

use super::personal_analyzer::{
    apply_merge_patch, build_fixed_prompt, build_input, prepare_merge_patch,
    validate_provider_budget, LocalIdSource, MAX_PROVIDER_INPUT_BYTES,
};
use super::personal_crypto::random_hex;
use super::personal_model::{
    ActivityPayload, AttemptPhase, FeedbackAction, RecommendationState, DISCLOSURE_VERSION,
};
use super::personal_process::{
    analyzer_process_is_gone, process_start_identity_token, verified_terminate_process_group,
    CoshCoreAnalyzerProcess, ProcessGroupIdentity,
};
use super::personal_runner::{
    analyzer_command, run_initialized_with_body_hooks, AnalyzerProcess, ProcessFailure,
    RunnerCommand, RunnerError,
};
use super::personal_scheduler::{
    check_dispatch, finish_attempt, mark_body_sent, mark_body_write_started, reserve_attempt,
    rollback_zero_body, LeaseLiveness, Reservation, SchedulerBlock, SessionGate,
};
use super::personal_store::{
    read_analyzer_guard, AnalyzerGuardHeader, PersonalStore, StateVersion,
};

const ANALYZER_SCHEMA: &str = r##"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"required":["discarded_activities","recent_tasks","frequent_patterns"],"properties":{"discarded_activities":{"type":"array","items":{"$ref":"#/$defs/discarded"}},"recent_tasks":{"type":"array","items":{"$ref":"#/$defs/recent"}},"frequent_patterns":{"type":"array","items":{"$ref":"#/$defs/frequent"}}},"$defs":{"nullable_id":{"type":["string","null"]},"ids":{"type":"array","items":{"type":"string"}},"entity":{"type":"object","additionalProperties":false,"required":["kind","value","volatility"],"properties":{"kind":{"enum":["namespace","workload","service","repo","branch","relative_path","test_target","process","package","host","url"]},"value":{"type":"string"},"volatility":{"enum":["stable","ephemeral"]}}},"discarded":{"type":"object","additionalProperties":false,"required":["activity_id","reason"],"properties":{"activity_id":{"type":"string"},"reason":{"const":"no_recommendation_value"}}},"recent":{"type":"object","additionalProperties":false,"required":["prior_task_id","summary","entities","evidence_activity_ids","prior_snapshot_ids","prompt_text"],"properties":{"prior_task_id":{"$ref":"#/$defs/nullable_id"},"summary":{"type":"string"},"entities":{"type":"array","items":{"$ref":"#/$defs/entity"}},"evidence_activity_ids":{"$ref":"#/$defs/ids"},"prior_snapshot_ids":{"$ref":"#/$defs/ids"},"prompt_text":{"type":"string"}}},"frequent":{"type":"object","additionalProperties":false,"required":["prior_pattern_id","summary","stable_entities","evidence_activity_ids","prior_snapshot_ids","prompt_text"],"properties":{"prior_pattern_id":{"$ref":"#/$defs/nullable_id"},"summary":{"type":"string"},"stable_entities":{"type":"array","items":{"$ref":"#/$defs/entity"}},"evidence_activity_ids":{"$ref":"#/$defs/ids"},"prior_snapshot_ids":{"$ref":"#/$defs/ids"},"prompt_text":{"type":"string"}}}}}"##;
const WRITER_FLUSH_POLLS: usize = 40;
const WRITER_FLUSH_POLL_INTERVAL: Duration = Duration::from_millis(25);
// High bit owns the body-write boundary; low bits retain the monotonic foreground epoch.
const BODY_WRITE_CLAIMED: u64 = 1 << 63;
const FOREGROUND_EPOCH_MASK: u64 = !BODY_WRITE_CLAIMED;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnalyzerTriggerContext {
    pub(crate) has_eligible_trigger: bool,
    pub(crate) foreground_idle: bool,
    pub(crate) foreground_activity_epoch: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct AnalyzerWorkerRequest {
    pub(crate) enabled: bool,
    pub(crate) store_root: PathBuf,
    pub(crate) adapter: CoshCoreAdapter,
    pub(crate) session_gate: SessionGate,
    pub(crate) session_scope_id: String,
    pub(crate) now_unix_secs: u64,
    pub(crate) trigger: AnalyzerTriggerContext,
    pub(crate) model: String,
    pub(crate) cancellation: AnalyzerCancellation,
}

impl AnalyzerWorkerRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        enabled: bool,
        store_root: PathBuf,
        adapter: CoshCoreAdapter,
        session_gate: SessionGate,
        session_scope_id: String,
        now_unix_secs: u64,
        trigger: AnalyzerTriggerContext,
        model: String,
    ) -> Self {
        Self {
            enabled,
            store_root,
            adapter,
            session_gate,
            session_scope_id,
            now_unix_secs,
            trigger,
            model,
            cancellation: AnalyzerCancellation::new(),
        }
    }

    pub(crate) fn with_cancellation(mut self, cancellation: AnalyzerCancellation) -> Self {
        self.cancellation = cancellation;
        self
    }
}

#[derive(Debug, Clone)]
pub(crate) struct AnalyzerCancellation {
    running: Arc<Mutex<Option<RunningAnalyzer>>>,
    foreground_idle: Arc<AtomicBool>,
    foreground_activity_epoch: Arc<AtomicU64>,
}

impl Default for AnalyzerCancellation {
    fn default() -> Self {
        Self {
            running: Arc::default(),
            foreground_idle: Arc::new(AtomicBool::new(true)),
            foreground_activity_epoch: Arc::new(AtomicU64::new(0)),
        }
    }
}

#[derive(Debug, Clone)]
struct RunningAnalyzer {
    owner_pid: u32,
    owner_start_identity: String,
    owner_session_id: String,
    lease_nonce: String,
    leader_pid: u32,
    leader_start_identity: String,
    process_group_id: u32,
    store_epoch: String,
}

impl AnalyzerCancellation {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn cancel_current(&self) -> bool {
        self.cancel_if(|_| true)
    }

    pub(crate) fn set_foreground_idle(&self, idle: bool) {
        if !idle {
            let mut current = self.foreground_activity_epoch.load(Ordering::Acquire);
            loop {
                let next = (current & BODY_WRITE_CLAIMED)
                    | ((current.wrapping_add(1)) & FOREGROUND_EPOCH_MASK);
                match self.foreground_activity_epoch.compare_exchange_weak(
                    current,
                    next,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
        self.foreground_idle.store(idle, Ordering::Release);
    }

    pub(crate) fn foreground_idle(&self) -> bool {
        self.foreground_idle.load(Ordering::Acquire)
    }

    pub(crate) fn foreground_activity_epoch(&self) -> u64 {
        self.foreground_activity_epoch.load(Ordering::Acquire) & FOREGROUND_EPOCH_MASK
    }

    pub(crate) fn claim_body_write(&self, expected_epoch: u64) -> bool {
        if !self.foreground_idle() {
            return false;
        }
        self.foreground_activity_epoch
            .compare_exchange(
                expected_epoch,
                expected_epoch | BODY_WRITE_CLAIMED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn release_body_write(&self) {
        self.foreground_activity_epoch
            .fetch_and(FOREGROUND_EPOCH_MASK, Ordering::AcqRel);
    }

    fn cancel_if(&self, predicate: impl FnOnce(&RunningAnalyzer) -> bool) -> bool {
        let running = self.running.lock().ok().and_then(|guard| guard.clone());
        let Some(running) = running.filter(predicate) else {
            return false;
        };
        verified_terminate_process_group(&running.process_identity())
    }

    fn cancel_if_guard_changed(&self, header: &AnalyzerGuardHeader) -> bool {
        self.cancel_if(|running| {
            if header.store_epoch != running.store_epoch {
                return true;
            }
            !header.lease.as_ref().is_some_and(|lease| {
                lease.owner_pid == running.owner_pid
                    && lease.owner_start_identity == running.owner_start_identity
                    && lease.owner_session_id == running.owner_session_id
                    && lease.lease_nonce == running.lease_nonce
                    && lease.core_leader_pid == Some(running.leader_pid)
                    && lease.core_leader_start_identity.as_deref()
                        == Some(running.leader_start_identity.as_str())
                    && lease.core_process_group_id == Some(running.process_group_id)
            })
        })
    }

    fn register(&self, running: RunningAnalyzer) {
        if let Ok(mut guard) = self.running.lock() {
            *guard = Some(running);
        }
    }

    fn clear(&self, leader_pid: u32, identity: &str) {
        if let Ok(mut guard) = self.running.lock() {
            if guard.as_ref().is_some_and(|running| {
                running.leader_pid == leader_pid && running.leader_start_identity == identity
            }) {
                *guard = None;
            }
        }
    }
}

impl RunningAnalyzer {
    fn process_identity(&self) -> ProcessGroupIdentity {
        ProcessGroupIdentity {
            owner_pid: self.owner_pid,
            owner_start_identity: self.owner_start_identity.clone(),
            leader_pid: self.leader_pid,
            leader_start_identity: self.leader_start_identity.clone(),
            process_group_id: self.process_group_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalyzerRunBlock {
    Scheduler(SchedulerBlock),
    NoticeRequired,
    AuthNotConfigured,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalyzerFailureStage {
    Store,
    Identity,
    Process,
    Input,
    StateTransition,
    Provider,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnalyzerRunOutcome {
    Completed,
    Blocked(AnalyzerRunBlock),
    Failed {
        stage: AnalyzerFailureStage,
        body_sent: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AnalyzerWorkerResult {
    pub(crate) outcome: AnalyzerRunOutcome,
    pub(crate) session_gate: SessionGate,
}

pub(crate) fn spawn_analyzer_worker(
    request: AnalyzerWorkerRequest,
) -> JoinHandle<AnalyzerWorkerResult> {
    std::thread::spawn(move || {
        let watcher_stop = Arc::new(AtomicBool::new(false));
        let watcher = spawn_epoch_watcher(
            request.store_root.clone(),
            request.cancellation.clone(),
            watcher_stop.clone(),
        );
        let mut gate = request.session_gate;
        let outcome = run_analyzer_once_with_cancellation(
            request.enabled,
            &request.store_root,
            &request.adapter,
            &mut gate,
            &request.session_scope_id,
            request.now_unix_secs,
            request.trigger,
            &request.model,
            request.cancellation,
        );
        watcher_stop.store(true, Ordering::Release);
        let _ = watcher.join();
        AnalyzerWorkerResult {
            outcome,
            session_gate: gate,
        }
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_analyzer_once_with_cancellation(
    enabled: bool,
    store_root: &Path,
    adapter: &CoshCoreAdapter,
    session_gate: &mut SessionGate,
    session_scope_id: &str,
    now_unix_secs: u64,
    trigger: AnalyzerTriggerContext,
    model: &str,
    cancellation: AnalyzerCancellation,
) -> AnalyzerRunOutcome {
    let store = match PersonalStore::open(store_root) {
        Ok(store) => ProductionStore(store),
        Err(_) => return failed(AnalyzerFailureStage::Store, false),
    };
    let mut dependencies = ProductionDependencies {
        adapter,
        cancellation,
        model,
    };
    orchestrate_once(
        enabled,
        &store,
        &mut dependencies,
        session_gate,
        session_scope_id,
        now_unix_secs,
        trigger,
    )
}

#[path = "personal_analysis_orchestration.rs"]
mod orchestration;
use orchestration::*;

struct EmptyDirectory {
    path: PathBuf,
}

impl EmptyDirectory {
    fn create(dependencies: &mut impl RuntimeDependencies) -> Result<Self, ()> {
        let name = dependencies.next_id("cosh-recommendation")?;
        let path = std::env::temp_dir().join(name);
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(&path).map_err(|_| ())?;
        let metadata = fs::symlink_metadata(&path).map_err(|_| ())?;
        if metadata.file_type().is_symlink()
            || !metadata.is_dir()
            || metadata.uid() != unsafe { nix::libc::geteuid() }
        {
            let _ = fs::remove_dir(&path);
            return Err(());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).map_err(|_| ())?;
        Ok(Self { path })
    }
}

impl Drop for EmptyDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn identity_matches(pid: u32, expected: &str) -> bool {
    !expected.is_empty() && process_start_identity(pid).as_deref() == Some(expected)
}

fn spawn_epoch_watcher(
    store_root: PathBuf,
    cancellation: AnalyzerCancellation,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            if let Ok(header) = read_analyzer_guard(&store_root) {
                cancellation.cancel_if_guard_changed(&header);
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
}

fn process_start_identity(pid: u32) -> Option<String> {
    process_start_identity_token(pid)
}

#[cfg(test)]
#[path = "personal_analysis_runtime_tests.rs"]
mod tests;
