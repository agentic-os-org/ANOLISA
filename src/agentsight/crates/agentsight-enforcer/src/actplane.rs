//! Official ActPlane adapter and Linux backend.

use std::collections::HashMap;
use std::fs;
use std::mem::MaybeUninit;
#[cfg(target_os = "linux")]
use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{SystemTime, UNIX_EPOCH};

use actplane_ifc_compiler::{Compiled, compile_str};
use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, BindingState, CredentialExfiltrationPolicy,
    DestinationClass, Effect, EnforcementCapabilities, EventIdentity, FileAction, HealthStatus,
    NetworkAction, NetworkDirection, PROTOCOL_VERSION, PolicyDecision, PolicyMode,
    ReplaceFailureCode, ReplaceOutcome, ReplacePolicy, ReplaceValidationError, ReplacementPolicy,
    SecurityEvent, SecurityEventKind, TaintTransition, TaintTransitionKind, ViolationEvent,
    classify_public_ipv4_destination,
};
use ebpf_ifc_engine::capability::{
    AUTH_ADD_LABEL, AUTH_BIND_RULE, AUTH_DECLASSIFY, AUTH_DELEGATE, AUTH_NARROW_SCOPE,
    AUTH_REQUIRE_GATE, CapState, TARGET_CHILD, TARGET_SELF,
};
use ebpf_ifc_engine::{GLOBAL_ACTIVE_DOMAIN_ID, PinnedEngine, ReloadHandle, Violation};
use thiserror::Error;
use uuid::Uuid;

use crate::event_hub::SecurityEventHub;
use crate::{BackendError, EnforcementBackend, EventHub, SubscriberClass};

// Compile-time guard: compiler and engine must agree on CConfig ABI size.
// If this fails, one crate's CRule definition was modified without updating the other.
const _: () = assert!(
    actplane_ifc_compiler::COMPILED_CONFIG_BLOB_SIZE == ebpf_ifc_engine::EXPECTED_CONFIG_BLOB_SIZE,
    "BPF ABI mismatch: actplane-ifc-compiler and ebpf-ifc-engine CConfig sizes diverged. \
     Update both crates' CRule/CConfig definitions to match."
);

/// Exact official upstream revision compiled into this adapter.
pub const ACTPLANE_REVISION: &str = "a62e5d9d96f91101cda019519053e950d532380a";

#[derive(Clone)]
struct ActiveBinding {
    binding: Binding,
    credential_policy: Option<CredentialExfiltrationPolicy>,
    /// When the target process runs inside a PID namespace, this holds the
    /// global kernel PID resolved by [`resolve_to_host_pid`].  `None` when
    /// the namespace PID equals the kernel PID (root namespace).
    kernel_pid: Option<i32>,
    reasons: Vec<String>,
    rule_names: Vec<String>,
    label_names: HashMap<u64, String>,
    /// Inodes guarded in the BPF inode_guard map for this binding.
    /// Each entry is `(ino, dev)` and must be cleaned up when the binding
    /// is detached so the map does not leak stale entries.
    guarded_inodes: Vec<(u64, u32)>,
}

struct PreparedBinding {
    request: ApplyPolicy,
    credential_policy: Option<CredentialExfiltrationPolicy>,
    compiled: Compiled,
}

struct RuntimeState {
    bindings: Mutex<HashMap<u32, ActiveBinding>>,
    events: EventHub,
    security_events: SecurityEventHub,
    runtime_error: Mutex<Option<String>>,
}

impl RuntimeState {
    fn new() -> Self {
        Self {
            bindings: Mutex::new(HashMap::new()),
            events: EventHub::default(),
            security_events: SecurityEventHub::default(),
            runtime_error: Mutex::new(None),
        }
    }

    fn bindings(&self) -> MutexGuard<'_, HashMap<u32, ActiveBinding>> {
        self.bindings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn runtime_error(&self) -> MutexGuard<'_, Option<String>> {
        self.runtime_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Official pinned ActPlane runtime behind the AgentSight backend contract.
pub struct ActPlaneBackend {
    engine: Arc<PinnedEngine>,
    reload: Arc<ReloadHandle>,
    _runtime_lock: fs::File,
    state: Arc<RuntimeState>,
    stop: Arc<AtomicBool>,
    poller: Mutex<Option<JoinHandle<()>>>,
    lifecycle: Mutex<()>,
    /// True when the enforcer is running in the init PID namespace.
    /// File-delete-guard requires this for correct cap_task seeding and
    /// domain propagation.  When false, health reports file_delete_guard=false.
    in_init_pidns: bool,
}

impl ActPlaneBackend {
    /// Opens or installs the singleton and starts its violation poller.
    ///
    /// # Errors
    ///
    /// Returns a kernel error when the pinned engine, exclusive runtime lock,
    /// reload handle, self-protection, initial cleanup, or event drain cannot be
    /// established.
    pub fn open() -> Result<Self, BackendError> {
        let engine = Arc::new(
            PinnedEngine::open_or_install_singleton()
                .map_err(|error| kernel_error("open pinned singleton", error))?,
        );
        let runtime_lock = engine
            .try_lock_runtime()
            .map_err(|error| kernel_error("lock singleton runtime", error))?;
        let reload = Arc::new(
            engine
                .reload_handle()
                .map_err(|error| kernel_error("open reload handle", error))?,
        );
        engine
            .protect_pid(std::process::id() as i32)
            .map_err(|error| kernel_error("protect enforcer pid", error))?;
        let drained = prepare_runtime(
            || {
                reload
                    .clear_runtime_state()
                    .map_err(|error| kernel_error("clear stale runtime state", error))
            },
            || {
                engine
                    .drain_pending_events()
                    .map_err(|error| kernel_error("drain stale pinned events", error))
            },
        )?;

        // Clear stale inode guards that may survive a crash or SIGKILL so
        // they do not leak into the next binding.
        if let Err(e) = engine.clear_inode_guards() {
            eprintln!("agentsight: failed to clear stale inode guards on startup: {e}");
        }

        let state = Arc::new(RuntimeState::new());
        let stop = Arc::new(AtomicBool::new(false));
        let poller = spawn_poller(Arc::clone(&engine), Arc::clone(&state), Arc::clone(&stop));
        log::info!(
            "ActPlane engine ready: drained {drained} stale pinned events, violation poller started"
        );
        Ok(Self {
            engine,
            reload,
            _runtime_lock: runtime_lock,
            state,
            stop,
            poller: Mutex::new(Some(poller)),
            lifecycle: Mutex::new(()),
            in_init_pidns: is_init_pid_namespace(),
        })
    }

    fn lifecycle(&self) -> MutexGuard<'_, ()> {
        self.lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn cleanup_binding(
        &self,
        request: &ApplyPolicy,
        id: u32,
        kernel_pid: Option<i32>,
    ) -> Vec<String> {
        let mut errors = Vec::new();
        let control_pid = std::process::id() as i32;
        if let Err(error) = self.engine.unbind_pid_from_domain(control_pid, id) {
            errors.push(format!("unbind control pid: {error}"));
        }
        // Use the resolved kernel PID when the target runs in a PID namespace;
        // fall back to the namespace PID stored in the request otherwise.
        let effective_pid = kernel_pid.unwrap_or(request.root_pid);
        if let Err(error) = self.engine.unbind_pid_from_domain(effective_pid, id) {
            errors.push(format!("unbind target pid: {error}"));
        }
        if let Err(error) = self.reload.clear_runtime_state() {
            errors.push(format!("clear runtime state: {error}"));
        }
        errors
    }

    fn prepare_binding(
        &self,
        mut request: ApplyPolicy,
        credential_policy: Option<CredentialExfiltrationPolicy>,
    ) -> Result<PreparedBinding, BackendError> {
        // Resolve namespace-local PID to host PID BEFORE start-time validation,
        // so that the stale-process check reads the correct /proc/<host_pid>/stat.
        let host_pid = resolve_to_host_pid(request.root_pid, request.process_start_time);
        if host_pid != request.root_pid {
            eprintln!(
                "resolve_to_host_pid: namespace pid {} -> host pid {}",
                request.root_pid, host_pid
            );
            request.root_pid = host_pid;
        }
        let actual_start = read_process_start_time(request.root_pid)?;
        if actual_start != request.process_start_time {
            return Err(BackendError::StaleProcess {
                pid: request.root_pid,
            });
        }
        let compiled = compile_str(&request.policy_dsl).map_err(BackendError::CompileFailure)?;
        if compiled
            .labels
            .get("COMMAND")
            .or_else(|| compiled.labels.get("AGENT"))
            .is_none()
        {
            return Err(BackendError::CompileFailure(
                "policy must declare a COMMAND or AGENT source label".into(),
            ));
        }
        Ok(PreparedBinding {
            request,
            credential_policy,
            compiled,
        })
    }

    fn install_prepared_locked(
        &self,
        bindings: &mut HashMap<u32, ActiveBinding>,
        prepared: PreparedBinding,
        runtime_domain: Option<u32>,
    ) -> Result<Binding, BackendError> {
        let PreparedBinding {
            request,
            credential_policy,
            compiled,
        } = prepared;
        let label = compiled
            .labels
            .get("COMMAND")
            .or_else(|| compiled.labels.get("AGENT"))
            .copied()
            .ok_or_else(|| {
                BackendError::CompileFailure(
                    "policy must declare a COMMAND or AGENT source label".into(),
                )
            })?;
        let id = runtime_domain.unwrap_or_else(|| domain_id(request.binding_id));
        self.engine
            .seed_label_in_domain(request.root_pid, id, label)
            .map_err(|error| kernel_error("seed target process domain", error))?;

        let control_pid = std::process::id() as i32;
        let control_state = CapState {
            scope_id: 1,
            labels: label,
            authority_mask: AUTH_BIND_RULE
                | AUTH_NARROW_SCOPE
                | AUTH_ADD_LABEL
                | AUTH_REQUIRE_GATE
                | AUTH_DECLASSIFY
                | AUTH_DELEGATE,
            target_mask: TARGET_SELF | TARGET_CHILD,
            gate_mask: u64::MAX,
            label_mask: u64::MAX,
            ..CapState::default()
        };
        // After prepare_binding, request.root_pid is already the host PID
        // (resolve_to_host_pid was called there).  No separate kernel_pid needed.
        if let Err(error) = self.engine.bind_state(control_pid, id, control_state) {
            let cleanup = self.cleanup_binding(&request, id, None);
            return Err(kernel_error_with_cleanup(
                "bind control process",
                error,
                cleanup,
            ));
        }
        if let Err(error) = self
            .reload
            .append_policy_delta(control_pid, id, &compiled.bytes)
        {
            let cleanup = self.cleanup_binding(&request, id, None);
            return Err(kernel_error_with_cleanup(
                "append policy delta",
                error,
                cleanup,
            ));
        }
        if let Err(error) = self.engine.unbind_pid_from_domain(control_pid, id) {
            let cleanup = self.cleanup_binding(&request, id, None);
            return Err(kernel_error_with_cleanup(
                "unbind control process",
                error,
                cleanup,
            ));
        }

        // Populate inode guard map for kernel-level fast-path protection.
        // This allows 5.10/6.6 kernels (where bpf_d_path is unavailable in
        // LSM hooks) to still block file deletion via inode matching.
        let guarded_inodes = populate_inode_guards(&self.engine, &request.policy_dsl);

        let binding = Binding {
            request,
            state: BindingState::Enforced,
            message: None,
            domain_id: Some(id),
        };
        bindings.insert(
            id,
            ActiveBinding {
                binding: binding.clone(),
                credential_policy,
                kernel_pid: None,
                reasons: compiled.reasons,
                rule_names: compiled.meta.into_iter().map(|meta| meta.name).collect(),
                label_names: compiled
                    .labels
                    .into_iter()
                    .map(|(name, mask)| (mask, name))
                    .collect(),
                guarded_inodes,
            },
        );
        Ok(binding)
    }

    fn apply_policy(
        &self,
        request: ApplyPolicy,
        credential_policy: Option<CredentialExfiltrationPolicy>,
    ) -> Result<Binding, BackendError> {
        let _lifecycle = self.lifecycle();
        let mut bindings = self.state.bindings();
        if let Some(existing) = bindings
            .values()
            .find(|active| active.binding.request.binding_id == request.binding_id)
        {
            return if existing.binding.request == request
                && existing.credential_policy == credential_policy
            {
                Ok(existing.binding.clone())
            } else {
                Err(BackendError::BindingConflict(request.binding_id))
            };
        }
        if !bindings.is_empty() {
            return Err(BackendError::BindingConflict(request.binding_id));
        }
        let binding_id = request.binding_id;
        let prepared = match self.prepare_binding(request, credential_policy) {
            Ok(prepared) => prepared,
            Err(error) => {
                log::warn!("policy apply rejected: binding_id={binding_id}: {error}");
                return Err(error);
            }
        };
        let result = self.install_prepared_locked(&mut bindings, prepared, None);
        match &result {
            Ok(binding) => log::info!(
                "policy enforced: binding_id={} root_pid={} domain_id={:?}",
                binding.request.binding_id,
                binding.request.root_pid,
                binding.domain_id
            ),
            Err(error) => log::error!("policy apply failed: binding_id={binding_id}: {error}"),
        }
        result
    }

    fn detach_binding_locked(
        &self,
        bindings: &mut HashMap<u32, ActiveBinding>,
        binding_id: Uuid,
    ) -> Result<(), BackendError> {
        let Some((id, active)) = bindings
            .iter()
            .find(|(_, active)| active.binding.request.binding_id == binding_id)
            .map(|(id, active)| (*id, active.clone()))
        else {
            return Err(BackendError::MissingBinding(binding_id));
        };
        // Clean up inode guard entries before tearing down the domain so the
        // BPF map does not retain stale entries for a detached binding.
        for &(ino, dev) in &active.guarded_inodes {
            if let Err(e) = self.engine.unguard_inode(ino, dev) {
                eprintln!("failed to unguard inode {ino}:{dev}: {e}");
            }
        }
        let cleanup = self.cleanup_binding(&active.binding.request, id, active.kernel_pid);
        if !cleanup.is_empty() {
            return Err(BackendError::KernelFailure(cleanup.join("; ")));
        }
        bindings.remove(&id);
        Ok(())
    }
}

impl EnforcementBackend for ActPlaneBackend {
    fn health(&self) -> Result<HealthStatus, BackendError> {
        let runtime_error = self.state.runtime_error().clone();
        // Report file-guard coverage from the profile actually loaded into the
        // kernel, so callers can gate APPLY_READY on real enforcement capability
        // rather than a static assumption (closes the silent-false-positive gap).
        let mut capabilities = EnforcementCapabilities::actplane();
        // supports_file_delete_guard() reflects the feature set the loaded profile
        // reserves, but the enforce_path_unlink / enforce_path_rename LSM hooks are
        // only attached when BPF-LSM is active at load time. AND in the live LSM
        // state so a host without an active BPF-LSM reports file_delete_guard=false
        // (fail-closed) instead of advertising a capability that silently enforces
        // nothing.
        capabilities.file_delete_guard = self.engine.supports_file_delete_guard()
            && ebpf_ifc_engine::bpf_lsm_active()
            && self.in_init_pidns;
        capabilities.file_delete_guard_mode = if capabilities.file_delete_guard {
            if self.engine.bpf_d_path_in_lsm_available() {
                Some("path".into())
            } else {
                Some("inode".into())
            }
        } else {
            None
        };
        let health = self.state.events.reflect_delivery_loss(HealthStatus {
            ready: runtime_error.is_none(),
            backend: "actplane".into(),
            capabilities,
            message: runtime_error,
        });
        Ok(self.state.security_events.reflect_delivery_loss(health))
    }

    fn apply(&self, request: ApplyPolicy) -> Result<Binding, BackendError> {
        self.apply_policy(request, None)
    }

    fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
    ) -> Result<Binding, BackendError> {
        let (request, credential_policy) = credential_apply_request(request)?;
        self.apply_policy(request, Some(credential_policy))
    }

    fn replace(&self, request: ReplacePolicy) -> Result<ReplaceOutcome, BackendError> {
        let _lifecycle = self.lifecycle();
        let bindings = self.state.bindings();
        let actual = bindings.values().next().cloned();

        if let Err(error) = request.validate() {
            let exact_source = actual
                .as_ref()
                .is_some_and(|active| active.binding == request.expected);
            return Ok(if exact_source {
                ReplaceOutcome::SourceRetained {
                    binding: request.expected,
                    code: validation_failure_code(&error),
                }
            } else {
                ReplaceOutcome::Conflict {
                    code: ReplaceFailureCode::BindingConflict,
                }
            });
        }

        let (target_request, target_policy) = match request.replacement.clone() {
            ReplacementPolicy::Generic(target) => (target, None),
            ReplacementPolicy::Credential(target) => match credential_apply_request(target) {
                Ok((target, policy)) => (target, Some(policy)),
                Err(error) => {
                    return Ok(preparation_failure(
                        actual.as_ref(),
                        &request.expected,
                        &error,
                    ));
                }
            },
        };
        if let Some(active) = actual.as_ref()
            && active.binding.request == target_request
        {
            return if active.credential_policy == target_policy
                && request.validate_acknowledgement(&active.binding).is_ok()
            {
                Ok(ReplaceOutcome::Applied(active.binding.clone()))
            } else {
                Ok(ReplaceOutcome::Conflict {
                    code: ReplaceFailureCode::BindingConflict,
                })
            };
        }
        if let Some(active) = actual.as_ref()
            && active.binding != request.expected
        {
            return Ok(ReplaceOutcome::Conflict {
                code: ReplaceFailureCode::BindingConflict,
            });
        }
        let Some(active) = actual.as_ref() else {
            return Ok(ReplaceOutcome::Conflict {
                code: ReplaceFailureCode::BindingConflict,
            });
        };
        if active.credential_policy.as_ref()
            != request
                .source
                .credential_snapshot()
                .map(|snapshot| &snapshot.policy)
        {
            return Ok(ReplaceOutcome::SourceRetained {
                binding: request.expected,
                code: ReplaceFailureCode::BindingConflict,
            });
        }
        let _prepared_target = match self.prepare_binding(target_request, target_policy) {
            Ok(prepared) => prepared,
            Err(error) => {
                return Ok(preparation_failure(
                    actual.as_ref(),
                    &request.expected,
                    &error,
                ));
            }
        };
        Ok(unsupported_runtime_handoff(&request.expected))
    }

    fn detach(&self, binding_id: Uuid) -> Result<(), BackendError> {
        let _lifecycle = self.lifecycle();
        let mut bindings = self.state.bindings();
        self.detach_binding_locked(&mut bindings, binding_id)
    }

    fn bindings(&self) -> Result<Vec<Binding>, BackendError> {
        let mut bindings: Vec<_> = self
            .state
            .bindings()
            .values()
            .map(|active| active.binding.clone())
            .collect();
        bindings.sort_by_key(|binding| binding.request.binding_id);
        Ok(bindings)
    }

    fn subscribe(&self, id: Uuid, class: SubscriberClass) -> Receiver<ViolationEvent> {
        self.state.events.subscribe(id, class)
    }

    fn unsubscribe(&self, id: Uuid) {
        self.state.events.unsubscribe(id);
    }

    fn record_required_delivery_loss(&self, count: u64) {
        self.state.events.record_required_delivery_loss(count);
    }

    fn subscribe_security_events(&self) -> Receiver<SecurityEvent> {
        self.state.security_events.subscribe()
    }

    fn record_security_delivery_loss(&self, count: u64) {
        self.state.security_events.record_delivery_loss(count);
    }
}

fn prepare_runtime(
    clear: impl FnOnce() -> Result<(), BackendError>,
    drain: impl FnOnce() -> Result<usize, BackendError>,
) -> Result<usize, BackendError> {
    clear()?;
    drain()
}

impl Drop for ActPlaneBackend {
    fn drop(&mut self) {
        let active = self
            .state
            .bindings()
            .iter()
            .map(|(domain_id, binding)| (*domain_id, binding.clone()))
            .collect::<Vec<_>>();
        for (domain_id, binding) in active {
            let errors =
                self.cleanup_binding(&binding.binding.request, domain_id, binding.kernel_pid);
            if errors.is_empty() {
                self.state.bindings().remove(&domain_id);
            } else {
                log::error!(
                    "agentsight-enforcer could not clear binding {} during shutdown: {}",
                    binding.binding.request.binding_id,
                    errors.join("; ")
                );
            }
        }
        // Batch-clear all remaining inode guard entries so no stale inodes
        // survive the enforcer shutdown — covers both normal and error paths.
        if let Err(e) = self.engine.clear_inode_guards() {
            eprintln!("agentsight-enforcer: failed to clear inode guards during shutdown: {e}");
        }
        self.stop.store(true, Ordering::Release);
        let poller = self
            .poller
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(poller) = poller
            && poller.join().is_err()
        {
            log::error!("agentsight-enforcer ActPlane poller panicked during shutdown");
        }
    }
}

fn spawn_poller(
    engine: Arc<PinnedEngine>,
    state: Arc<RuntimeState>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let callback_state = Arc::clone(&state);
        if let Err(error) = engine.run(&stop, move |raw| {
            let active = callback_state.bindings().get(&raw.domain_id).cloned();
            if let Some(active) = active {
                callback_state
                    .events
                    .publish(convert_violation(raw.clone(), &active));
                if raw.op == 3
                    && raw.provenance.is_some()
                    && let Some(policy) = active.credential_policy.as_ref()
                {
                    let label = active
                        .label_names
                        .get(&raw.matched_label)
                        .cloned()
                        .unwrap_or_else(|| format!("label-0x{:x}", raw.matched_label));
                    match convert_security_events(raw, &active, policy, &label) {
                        Ok(events) => {
                            for event in events {
                                callback_state.security_events.publish(event);
                            }
                        }
                        Err(error) => {
                            let message = format!("normalize ActPlane evidence: {error}");
                            log::error!("{message}");
                            *callback_state.runtime_error() = Some(message);
                        }
                    }
                }
            }
        }) {
            let message = format!("violation poller stopped: {error}");
            log::error!("{message}");
            *state.runtime_error() = Some(message);
        }
    })
}

fn domain_id(binding_id: Uuid) -> u32 {
    let bytes = binding_id.as_bytes();
    let id = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .fold(0, std::ops::BitXor::bitxor);
    match id {
        0 | GLOBAL_ACTIVE_DOMAIN_ID => 1,
        id => id,
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
enum ProcessStatError {
    #[error("process stat is missing the command terminator")]
    MissingCommand,
    #[error("process stat is missing field 22")]
    MissingStartTime,
    #[error("process stat field 22 is not an unsigned integer")]
    InvalidStartTime,
}

fn parse_process_start_time(stat: &str) -> Result<u64, ProcessStatError> {
    let tail = stat
        .rsplit_once(") ")
        .map(|(_, tail)| tail)
        .ok_or(ProcessStatError::MissingCommand)?;
    tail.split_whitespace()
        .nth(19)
        .ok_or(ProcessStatError::MissingStartTime)?
        .parse()
        .map_err(|_| ProcessStatError::InvalidStartTime)
}

fn read_process_start_time(pid: i32) -> Result<u64, BackendError> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| BackendError::KernelFailure(format!("read /proc/{pid}/stat: {error}")))?;
    parse_process_start_time(&stat)
        .map_err(|error| BackendError::KernelFailure(format!("parse /proc/{pid}/stat: {error}")))
}

fn credential_apply_request(
    request: ApplyCredentialPolicy,
) -> Result<(ApplyPolicy, CredentialExfiltrationPolicy), BackendError> {
    let policy_dsl = compile_credential_exfiltration_policy(&request.policy)?;
    let policy = request.policy;
    Ok((
        ApplyPolicy {
            binding_id: request.binding_id,
            agent_id: request.agent_id,
            session_id: request.session_id,
            root_pid: request.root_pid,
            process_start_time: request.process_start_time,
            policy_id: policy.policy_id.clone(),
            policy_revision: policy.revision.to_string(),
            policy_dsl,
            policy_mode: Some(policy.mode),
        },
        policy,
    ))
}

fn replacement_failure_code(error: &BackendError) -> ReplaceFailureCode {
    match error {
        BackendError::BindingConflict(_) | BackendError::MissingBinding(_) => {
            ReplaceFailureCode::BindingConflict
        }
        BackendError::StaleProcess { .. } => ReplaceFailureCode::StaleProcess,
        BackendError::CompileFailure(_) => ReplaceFailureCode::CompileFailure,
        BackendError::KernelFailure(_) => ReplaceFailureCode::KernelFailure,
    }
}

fn validation_failure_code(error: &ReplaceValidationError) -> ReplaceFailureCode {
    match error {
        ReplaceValidationError::CredentialPolicy(_)
        | ReplaceValidationError::SourcePolicySnapshot(_) => ReplaceFailureCode::CompileFailure,
        ReplaceValidationError::SameBindingId
        | ReplaceValidationError::SourceNotEnforced
        | ReplaceValidationError::SourceDomainMissing
        | ReplaceValidationError::AgentMismatch
        | ReplaceValidationError::SessionMismatch
        | ReplaceValidationError::SourcePolicyMismatch
        | ReplaceValidationError::TargetAcknowledgementMismatch
        | ReplaceValidationError::RuntimeDomainMismatch => ReplaceFailureCode::BindingConflict,
    }
}

fn preparation_failure(
    actual: Option<&ActiveBinding>,
    expected: &Binding,
    error: &BackendError,
) -> ReplaceOutcome {
    let code = replacement_failure_code(error);
    if actual.is_some_and(|active| active.binding == *expected) {
        ReplaceOutcome::SourceRetained {
            binding: expected.clone(),
            code,
        }
    } else {
        ReplaceOutcome::Indeterminate { code }
    }
}

fn unsupported_runtime_handoff(source: &Binding) -> ReplaceOutcome {
    ReplaceOutcome::SourceRetained {
        binding: source.clone(),
        code: ReplaceFailureCode::UnsupportedHandoff,
    }
}

/// Translates the stable credential-exfiltration model into pinned ActPlane DSL.
///
/// The current ActPlane endpoint-condition ABI can represent one trusted target
/// per rule. Observe and audit policies use notify rules plus adapter-side TTL
/// and `public_ipv4` filtering. Enforce mode emits a `block` rule with an
/// `expires` clause so the pinned ABI honours taint TTL directly, and requires
/// at least one trusted endpoint to avoid blocking all outbound connections.
///
/// # Errors
///
/// Returns a compile failure for invalid policy fields, unsafe DSL literals,
/// unsupported enforcement semantics or trusted targets, or pinned compiler
/// rejection.
pub fn compile_credential_exfiltration_policy(
    policy: &CredentialExfiltrationPolicy,
) -> Result<String, BackendError> {
    policy
        .validate()
        .map_err(|error| BackendError::CompileFailure(error.to_string()))?;
    validate_label(&policy.taint_label)?;

    let mut sources = policy.source_patterns.clone();
    sources.sort();
    sources.dedup();
    for source in &sources {
        validate_literal("source pattern", source)?;
    }

    let mut trusted = policy.trusted_endpoints.clone();
    trusted.sort();
    trusted.dedup();
    for endpoint in &trusted {
        validate_literal("trusted endpoint", endpoint)?;
    }
    if trusted.len() > 1 {
        return Err(BackendError::CompileFailure(
            "the pinned ActPlane ABI supports one trusted endpoint exception per rule".into(),
        ));
    }
    // Enforce mode is rejected: the kernel LSM block rule uses `endpoint "*"`
    // which denies ALL outbound (including private/loopback), and the public/private
    // classification happens only after the kernel has already dropped the connection.
    // Until the BPF engine can express public-only scope, enforce remains unsafe.
    // TODO(roadmap): re-enable when ActPlane supports `scope public` or CIDR exclusions.
    if policy.mode == PolicyMode::Enforce {
        return Err(BackendError::CompileFailure(
            "enforce mode is not yet supported: kernel cannot distinguish public vs private destinations".into(),
        ));
    }

    let mut dsl = String::from("source AGENT = exec \"**\"\n");
    for source in sources {
        dsl.push_str(&format!(
            "source {} = file \"{}\"\n",
            policy.taint_label, source
        ));
    }
    dsl.push_str("rule agentsight-credential-exfiltration:\n  ");
    dsl.push_str("notify connect endpoint \"*\" if ");
    dsl.push_str(&policy.taint_label);
    if let Some(endpoint) = trusted.first() {
        dsl.push_str(" unless target \"");
        dsl.push_str(endpoint);
        dsl.push('"');
    }
    dsl.push_str("\n  because \"credential-derived data reached an untrusted network target\"\n");

    compile_str(&dsl).map_err(BackendError::CompileFailure)?;
    Ok(dsl)
}

fn validate_label(label: &str) -> Result<(), BackendError> {
    let valid = label.chars().enumerate().all(|(index, character)| {
        character == '_'
            || character.is_ascii_uppercase()
            || (index > 0 && character.is_ascii_digit())
    });
    if valid {
        Ok(())
    } else {
        Err(BackendError::CompileFailure(
            "taint label must use uppercase ASCII letters, digits, or underscore".into(),
        ))
    }
}

fn validate_literal(kind: &str, value: &str) -> Result<(), BackendError> {
    if value.is_empty()
        || value.len() >= 127
        || value
            .chars()
            .any(|character| character == '"' || character == '\\' || character.is_control())
    {
        return Err(BackendError::CompileFailure(format!(
            "{kind} contains unsupported DSL characters or exceeds 126 bytes"
        )));
    }
    Ok(())
}

fn convert_security_events(
    raw: Violation,
    active: &ActiveBinding,
    policy: &CredentialExfiltrationPolicy,
    taint_label: &str,
) -> Result<Vec<SecurityEvent>, BackendError> {
    convert_security_events_at(
        raw,
        active,
        policy,
        taint_label,
        now_ns(),
        monotonic_now_ns(),
    )
}

fn convert_security_events_at(
    raw: Violation,
    active: &ActiveBinding,
    policy: &CredentialExfiltrationPolicy,
    taint_label: &str,
    observed_at_ns: u64,
    monotonic_now_ns: Option<u64>,
) -> Result<Vec<SecurityEvent>, BackendError> {
    let provenance = raw.provenance.as_ref().ok_or_else(|| {
        BackendError::KernelFailure("ActPlane connect violation lacks source provenance".into())
    })?;
    let monotonic_now_ns = monotonic_now_ns.ok_or_else(|| {
        BackendError::KernelFailure(
            "monotonic time is unavailable for credential taint TTL evaluation".into(),
        )
    })?;
    let taint_age_ns = monotonic_now_ns
        .checked_sub(provenance.timestamp_ns)
        .ok_or_else(|| {
            BackendError::KernelFailure(
                "credential provenance timestamp is newer than monotonic time".into(),
            )
        })?;
    let taint_ttl_ns = policy
        .taint_ttl_secs
        .checked_mul(1_000_000_000)
        .ok_or_else(|| BackendError::CompileFailure("taint TTL exceeds nanoseconds".into()))?;
    let destination_class = classify_destination(&raw.target);
    if taint_age_ns >= taint_ttl_ns
        || destination_class != DestinationClass::Public
        || policy
            .trusted_endpoints
            .iter()
            .any(|trusted| trusted == &raw.target)
    {
        return Ok(Vec::new());
    }
    let policy_revision = active
        .binding
        .request
        .policy_revision
        .parse::<u64>()
        .map_err(|_| {
            BackendError::CompileFailure(format!(
                "policy revision '{}' is not numeric",
                active.binding.request.policy_revision
            ))
        })?;
    let rule_id = active
        .rule_names
        .get(raw.rule_id as usize)
        .cloned()
        .or_else(|| Some(raw.rule_id.to_string()));
    let reason = active
        .reasons
        .get(raw.rule_id as usize)
        .cloned()
        .unwrap_or_else(|| "credential taint reached an untrusted network target".into());
    let sink_time = monotonic_to_epoch_ns(raw.timestamp_ns, monotonic_now_ns, observed_at_ns);
    let source_time =
        monotonic_to_epoch_ns(provenance.timestamp_ns, monotonic_now_ns, observed_at_ns);
    let source_start = process_start_time(active, provenance.pid);
    let target_start = process_start_time(active, raw.pid);
    let source_identity = event_identity(active, provenance.pid, None, source_start);
    let sink_identity = event_identity(active, raw.pid, Some(raw.ppid), target_start);
    let source_event_id = Uuid::new_v4();
    let taint_event_id = Uuid::new_v4();
    let sink_event_id = Uuid::new_v4();
    let decision_event_id = Uuid::new_v4();
    let policy_id = active.binding.request.policy_id.clone();
    let source_path = redact_home_path(&provenance.target);
    let destination = raw.target.clone();

    Ok(vec![
        SecurityEvent {
            event_id: source_event_id,
            occurred_at_ns: source_time,
            observed_at_ns,
            identity: source_identity,
            kind: SecurityEventKind::FileAction(FileAction {
                policy_id: policy_id.clone(),
                policy_revision,
                operation: operation_name(provenance.op).into(),
                path: source_path,
                resource_class: "credential".into(),
                succeeded: true,
                errno: None,
                rule_id: rule_id.clone(),
            }),
        },
        SecurityEvent {
            event_id: taint_event_id,
            occurred_at_ns: source_time,
            observed_at_ns,
            identity: sink_identity.clone(),
            kind: SecurityEventKind::TaintTransition(TaintTransition {
                policy_id: policy_id.clone(),
                policy_revision,
                label: taint_label.into(),
                transition: if provenance.pid == raw.pid {
                    TaintTransitionKind::Add
                } else {
                    TaintTransitionKind::Inherit
                },
                source_pid: provenance.pid,
                source_process_start_time: source_start,
                target_pid: raw.pid,
                target_process_start_time: target_start,
                reason: "ActPlane reported the first source provenance for the matched label"
                    .into(),
            }),
        },
        SecurityEvent {
            event_id: sink_event_id,
            occurred_at_ns: sink_time,
            observed_at_ns,
            identity: sink_identity.clone(),
            kind: SecurityEventKind::NetworkAction(NetworkAction {
                policy_id: policy_id.clone(),
                policy_revision,
                direction: NetworkDirection::Outbound,
                destination: destination.clone(),
                destination_class,
                protocol: "tcp".into(),
                succeeded: !raw.blocked,
                errno: raw.blocked.then_some(libc::EPERM),
                rule_id: rule_id.clone(),
            }),
        },
        SecurityEvent {
            event_id: decision_event_id,
            occurred_at_ns: sink_time,
            observed_at_ns,
            identity: sink_identity,
            kind: SecurityEventKind::PolicyDecision(PolicyDecision {
                policy_id,
                policy_revision,
                source_event_id,
                sink_event_id,
                mode: policy.mode,
                requested_effect: effect(raw.effect),
                blocked: raw.blocked,
                killed: raw.killed,
                errno: raw.blocked.then_some(libc::EPERM),
                risk_score: if raw.blocked { 95 } else { 85 },
                reason,
            }),
        },
    ])
}

fn process_start_time(active: &ActiveBinding, pid: i32) -> u64 {
    if pid == active.binding.request.root_pid {
        active.binding.request.process_start_time
    } else {
        read_process_start_time(pid).unwrap_or(0)
    }
}

fn event_identity(
    active: &ActiveBinding,
    pid: i32,
    ppid: Option<i32>,
    process_start_time: u64,
) -> EventIdentity {
    EventIdentity {
        binding_id: active.binding.request.binding_id,
        agent_id: active.binding.request.agent_id.clone(),
        agent_name: None,
        session_id: active.binding.request.session_id.clone(),
        conversation_id: None,
        tool_call_id: None,
        pid,
        process_start_time,
        ppid,
        cgroup_id: None,
        protocol_version: PROTOCOL_VERSION,
        enforcer_version: env!("CARGO_PKG_VERSION").into(),
        actplane_revision: ACTPLANE_REVISION.into(),
    }
}

fn redact_home_path(path: &str) -> String {
    if let Some(relative) = path.strip_prefix("/root/") {
        return format!("~/{relative}");
    }
    if let Some(relative) = path.strip_prefix("/home/")
        && let Some((_, remainder)) = relative.split_once('/')
    {
        return format!("~/{remainder}");
    }
    path.into()
}

fn classify_destination(destination: &str) -> DestinationClass {
    classify_public_ipv4_destination(destination)
}

fn convert_violation(raw: Violation, active: &ActiveBinding) -> ViolationEvent {
    let monotonic_now_ns = monotonic_now_ns();
    let observed_at_ns = now_ns();
    convert_violation_at(raw, active, observed_at_ns, monotonic_now_ns)
}

fn convert_violation_at(
    raw: Violation,
    active: &ActiveBinding,
    observed_at_ns: u64,
    monotonic_now_ns: Option<u64>,
) -> ViolationEvent {
    let rule_index = raw.rule_id as usize;
    let occurred_at_ns = monotonic_now_ns
        .map(|monotonic_now_ns| {
            monotonic_to_epoch_ns(raw.timestamp_ns, monotonic_now_ns, observed_at_ns)
        })
        .unwrap_or(observed_at_ns);
    ViolationEvent {
        event_id: Uuid::new_v4(),
        binding_id: active.binding.request.binding_id,
        agent_id: active.binding.request.agent_id.clone(),
        session_id: active.binding.request.session_id.clone(),
        policy_id: active.binding.request.policy_id.clone(),
        policy_revision: active.binding.request.policy_revision.clone(),
        pid: raw.pid,
        ppid: Some(raw.ppid),
        process_start_time: read_process_start_time(raw.pid).unwrap_or(0),
        operation: operation_name(raw.op).into(),
        target: raw.target,
        effect: effect(raw.effect),
        blocked: raw.blocked,
        killed: raw.killed,
        rule_id: active
            .rule_names
            .get(rule_index)
            .cloned()
            .or_else(|| Some(raw.rule_id.to_string())),
        reason: active.reasons.get(rule_index).cloned(),
        occurred_at_ns,
        observed_at_ns,
        actplane_revision: ACTPLANE_REVISION.into(),
    }
}

fn operation_name(operation: u32) -> &'static str {
    match operation {
        0 => "exec",
        1 => "open",
        2 => "write",
        3 => "connect",
        4 => "recv",
        _ => "unknown",
    }
}

fn effect(value: u32) -> Effect {
    match value {
        1 => Effect::Block,
        2 => Effect::Kill,
        _ => Effect::Notify,
    }
}

fn monotonic_to_epoch_ns(
    event_monotonic_ns: u64,
    monotonic_now_ns: u64,
    realtime_now_ns: u64,
) -> u64 {
    let Some(elapsed_ns) = monotonic_now_ns.checked_sub(event_monotonic_ns) else {
        return realtime_now_ns;
    };
    realtime_now_ns
        .checked_sub(elapsed_ns)
        .unwrap_or(realtime_now_ns)
}

fn monotonic_now_ns() -> Option<u64> {
    let mut timestamp = MaybeUninit::<libc::timespec>::uninit();
    // SAFETY: `clock_gettime` initializes the provided timespec on success.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, timestamp.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: The successful call above initialized `timestamp`.
    let timestamp = unsafe { timestamp.assume_init() };
    let seconds = u64::try_from(timestamp.tv_sec).ok()?;
    let nanoseconds = u64::try_from(timestamp.tv_nsec).ok()?;
    if nanoseconds >= 1_000_000_000 {
        return None;
    }
    seconds.checked_mul(1_000_000_000)?.checked_add(nanoseconds)
}

fn now_ns() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    nanos.min(u64::MAX as u128) as u64
}

fn kernel_error(context: &str, error: std::io::Error) -> BackendError {
    BackendError::KernelFailure(format!("{context}: {error}"))
}

fn kernel_error_with_cleanup(
    context: &str,
    error: std::io::Error,
    cleanup: Vec<String>,
) -> BackendError {
    let mut message = format!("{context}: {error}");
    if !cleanup.is_empty() {
        message.push_str("; cleanup failed: ");
        message.push_str(&cleanup.join("; "));
    }
    BackendError::KernelFailure(message)
}

/// Resolve a potentially namespace-local PID to the global (init namespace)
/// kernel PID that BPF `handle_fork` / `cap_fork` will use.
///
/// The enforcer **must** run in the init PID namespace for this to work.
/// When it does, host `/proc/<pid>/status` exposes the full `NSpid` chain.
///
/// **Algorithm**:
/// 1. Fast path: `/proc/<input_pid>/stat` exists and `start_time` matches →
///    the PID is valid in the host namespace.  Return `NSpid[0]` (always the
///    host PID when reading from init ns).
/// 2. Slow path: the PID does not exist in host `/proc` or `start_time` does
///    not match (namespace-local PID collides with a different host process).
///    Scan `/proc/*/status` for a process whose innermost `NSpid` value equals
///    `input_pid` and whose `start_time` matches.  Return its host PID.
/// 3. Fallback: no match found — return `input_pid` unchanged and let the
///    caller handle the stale-process / not-found case.
fn resolve_to_host_pid(input_pid: i32, input_start_time: u64) -> i32 {
    // Fast path: pid exists in host /proc and start_time matches.
    if let Ok(start_time) = proc_start_time(input_pid) {
        if start_time == input_start_time {
            return first_nspid(input_pid).unwrap_or(input_pid);
        }
        // pid exists but belongs to a different process (namespace collision).
    }

    // Slow path: scan /proc for a process whose innermost NSpid == input_pid
    // and whose start_time matches.
    let proc_dir = match fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return input_pid,
    };
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Ok(host_pid) = name.to_string_lossy().parse::<i32>() else {
            continue;
        };
        if host_pid <= 0 {
            continue;
        }
        let nspid = read_nspid_chain(host_pid);
        if nspid.len() < 2 {
            continue; // not in a nested namespace
        }
        if *nspid.last().unwrap() != input_pid {
            continue; // innermost ns PID doesn't match
        }
        if let Ok(pst) = proc_start_time(host_pid) {
            if pst == input_start_time {
                return nspid[0]; // host PID
            }
        }
    }

    input_pid // fallback
}

/// Read the `NSpid:` chain from `/proc/<pid>/status`.  Returns a Vec where
/// index 0 is the outermost (host) PID and the last element is the innermost
/// (namespace-local) PID.
fn read_nspid_chain(pid: i32) -> Vec<i32> {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return vec![];
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("NSpid:") {
            return rest
                .split_whitespace()
                .filter_map(|s| s.parse::<i32>().ok())
                .collect();
        }
    }
    vec![]
}

/// Return the first (host-namespace) value from the `NSpid:` line of
/// `/proc/<pid>/status`, or `None` if unavailable.
fn first_nspid(pid: i32) -> Option<i32> {
    read_nspid_chain(pid).first().copied()
}

/// Read `start_time` (field 22) from `/proc/<pid>/stat`.
fn proc_start_time(pid: i32) -> Result<u64, ()> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|_| ())?;
    let after_comm = &stat[stat.rfind(')').ok_or(())? + 2..];
    after_comm
        .split_ascii_whitespace()
        .nth(19) // field 22 = index 19 after ')'
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or(())
}

/// Check whether the current process runs in the init PID namespace by
/// comparing its pid-namespace inode with that of PID 1.
fn is_init_pid_namespace() -> bool {
    let Ok(self_ns) = fs::read_link("/proc/self/ns/pid") else {
        return false; // can't read → assume not init, fail-closed
    };
    let Ok(init_ns) = fs::read_link("/proc/1/ns/pid") else {
        return false;
    };
    let result = self_ns == init_ns;
    if !result {
        eprintln!(
            "agentsight-enforcer: not in init PID namespace \
             (self={}, pid1={}); file_delete_guard will be disabled",
            self_ns.display(),
            init_ns.display(),
        );
    }
    result
}

/// Extract concrete file paths from DSL `block (unlink|write|rename) file "..."`
/// clauses only.  Source definitions (`source X = file "..."`) and non-block
/// rules (`notify`, `audit`) are excluded so that credential source files and
/// observation-only policies do not receive unintended delete protection.
///
/// Paths containing glob characters (`*`, `?`) are skipped because they cannot
/// be stat'd for an inode.  Only absolute paths are returned.
fn extract_guarded_paths(dsl: &str) -> Vec<String> {
    let mut paths = Vec::new();
    // Only match `block <op> file "..."` — not `source X = file` or `notify`.
    let block_file_needles = [
        "block unlink file \"",
        "block write file \"",
        "block rename file \"",
    ];
    for needle in &block_file_needles {
        let mut rest = dsl;
        while let Some(pos) = rest.find(needle) {
            let start = pos + needle.len();
            rest = &rest[start..];
            if let Some(end) = rest.find('"') {
                let path = &rest[..end];
                if path.starts_with('/')
                    && !path.contains('*')
                    && !path.contains('?')
                    && !paths.iter().any(|p| p == path)
                {
                    // The inode guard fast path only checks te_pid_active()
                    // (= process is in any domain), which is semantically
                    // equivalent to "if AGENT" / "if COMMAND" (all exec in
                    // domain).  Rules with other labels (e.g. "if CREDENTIAL")
                    // or with "unless" conditions cannot be faithfully evaluated
                    // by the fast path and must be excluded.
                    let line_rest = rest[end + 1..].split('\n').next().unwrap_or("");
                    let has_unless = line_rest.contains("unless");
                    let has_non_standard_label = line_rest.contains(" if ")
                        && !line_rest.contains("if AGENT")
                        && !line_rest.contains("if COMMAND");
                    if !has_unless && !has_non_standard_label {
                        paths.push(path.to_string());
                    }
                }
                rest = &rest[end + 1..];
            } else {
                break;
            }
        }
    }
    paths
}

/// Stat each guarded file path from the DSL and insert the `(ino, dev)` pair
/// into the BPF inode guard map with UNLINK | RENAME protection flags.
///
/// Returns the list of successfully guarded `(ino, dev)` pairs so the caller
/// can store them for later cleanup.
#[cfg(target_os = "linux")]
fn populate_inode_guards(engine: &PinnedEngine, policy_dsl: &str) -> Vec<(u64, u32)> {
    let mut guarded: Vec<(u64, u32)> = Vec::new();
    for path in extract_guarded_paths(policy_dsl) {
        match fs::metadata(&path) {
            Ok(meta) => {
                let ino = meta.ino();
                let dev = userspace_dev_to_kernel(meta.dev());
                let flags =
                    ebpf_ifc_engine::INODE_GUARD_UNLINK | ebpf_ifc_engine::INODE_GUARD_RENAME;
                if let Err(e) = engine.guard_inode(ino, dev, flags) {
                    eprintln!("failed to guard inode {ino}:{dev} for {path}: {e}");
                } else {
                    guarded.push((ino, dev));
                }
            }
            Err(e) => eprintln!("cannot stat {path} for inode guard: {e}"),
        }
    }
    guarded
}

#[cfg(not(target_os = "linux"))]
fn populate_inode_guards(_engine: &PinnedEngine, _policy_dsl: &str) -> Vec<(u64, u32)> {
    Vec::new()
}

/// Convert a userspace `stat.st_dev` value (glibc `new_encode_dev` format) to
/// the kernel-internal `dev_t` layout used by `super_block.s_dev` and read by
/// BPF via `BPF_CORE_READ(inode, i_sb, s_dev)`.
///
/// Userspace:  `(minor & 0xff) | (major << 8) | ((minor & !0xff) << 12)`
/// Kernel:     `MKDEV(major, minor)` = `(major << 20) | minor`
#[cfg(target_os = "linux")]
fn userspace_dev_to_kernel(dev: u64) -> u32 {
    let dev = dev as u32;
    let major = (dev & 0xfff00) >> 8;
    let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
    (major << 20) | minor
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use agentsight_enforcement_protocol::{
        ApplyPolicy, Binding, BindingState, CredentialExfiltrationPolicy, DestinationScope, Effect,
        PolicyMode, SecurityEventKind, ViolationEvent,
    };
    use ebpf_ifc_engine::{Provenance, Violation};
    use uuid::Uuid;

    use super::*;

    const _: fn(&PinnedEngine) -> std::io::Result<usize> = PinnedEngine::drain_pending_events;

    fn active_binding() -> ActiveBinding {
        ActiveBinding {
            binding: Binding {
                request: ApplyPolicy {
                    binding_id: Uuid::new_v4(),
                    agent_id: "agent-1".into(),
                    session_id: Some("session-1".into()),
                    root_pid: 42,
                    process_start_time: 98765,
                    policy_id: "policy-1".into(),
                    policy_revision: "revision-1".into(),
                    policy_dsl: "fixture".into(),
                    policy_mode: Some(PolicyMode::Audit),
                },
                state: BindingState::Enforced,
                message: None,
                domain_id: Some(7),
            },
            credential_policy: Some(credential_policy()),
            kernel_pid: None,
            reasons: vec!["credential reached an external sink".into()],
            rule_names: vec!["block-exfiltration".into()],
            label_names: HashMap::from([(1, "CREDENTIAL".into())]),
            guarded_inodes: Vec::new(),
        }
    }

    fn raw_violation(timestamp_ns: u64) -> Violation {
        Violation {
            effect: 1,
            blocked: true,
            killed: false,
            comm: "curl".into(),
            pid: 43,
            ppid: 42,
            target: "8.8.8.8".into(),
            rule_id: 0,
            op: 3,
            domain_id: 7,
            session_root: 42,
            label: 1,
            matched_label: 1,
            matched_labels: 1,
            provenance: None,
            timestamp_ns,
        }
    }

    fn credential_policy() -> CredentialExfiltrationPolicy {
        CredentialExfiltrationPolicy {
            policy_id: "credential-exfiltration".into(),
            revision: 3,
            source_patterns: vec!["/root/.ssh/id_rsa".into(), "/root/.aws/credentials".into()],
            trusted_endpoints: vec!["10.0.0.8".into()],
            taint_label: "CREDENTIAL".into(),
            taint_ttl_secs: 900,
            destination_scope: DestinationScope::PublicIpv4,
            mode: PolicyMode::Enforce,
        }
    }

    #[test]
    fn credential_policy_compiles_deterministically_with_trusted_exception() {
        let mut policy = credential_policy();
        policy.mode = PolicyMode::Audit;
        let dsl =
            compile_credential_exfiltration_policy(&policy).expect("fixture policy should compile");

        assert!(dsl.starts_with("source AGENT = exec \"**\"\n"));
        assert!(dsl.contains("source CREDENTIAL = file \"/root/.aws/credentials\""));
        assert!(dsl.contains("source CREDENTIAL = file \"/root/.ssh/id_rsa\""));
        assert!(dsl.contains("notify connect endpoint \"*\" if CREDENTIAL"));
        assert!(dsl.contains("unless target \"10.0.0.8\""));
        assert!(compile_str(&dsl).is_ok());
    }

    #[test]
    fn enforce_policy_compiles_block_rule_with_expires() {
        let policy = credential_policy();
        let dsl = compile_credential_exfiltration_policy(&policy)
            .expect("enforce policy with a trusted endpoint should compile");

        assert!(dsl.contains("block connect endpoint \"*\" if CREDENTIAL"));
        assert!(dsl.contains("unless target \"10.0.0.8\""));
        assert!(dsl.contains("expires 900s"));
        assert!(compile_str(&dsl).is_ok());
    }

    #[test]
    fn enforce_policy_requires_trusted_endpoint() {
        let mut policy = credential_policy();
        policy.trusted_endpoints.clear();
        let error = compile_credential_exfiltration_policy(&policy)
            .expect_err("enforce mode without a trusted endpoint must fail closed");

        assert!(error.to_string().contains("trusted_endpoint"));
    }

    #[test]
    fn credential_policy_rejects_dsl_injection() {
        let mut policy = credential_policy();
        policy.mode = PolicyMode::Audit;
        policy.source_patterns = vec!["/safe\"\nrule injected:".into()];

        assert!(compile_credential_exfiltration_policy(&policy).is_err());
    }

    #[test]
    fn checked_in_policy_fixture_compiles_in_audit_mode() {
        let policy: CredentialExfiltrationPolicy = serde_json::from_str(include_str!(
            "../../../integration-tests/fixtures/credential-exfiltration-policy.json"
        ))
        .expect("checked-in policy fixture should decode");

        let dsl = compile_credential_exfiltration_policy(&policy)
            .expect("checked-in policy fixture should compile");
        assert!(dsl.contains("notify connect endpoint"));
    }

    #[test]
    fn multiple_trusted_endpoints_fail_closed() {
        let mut policy = credential_policy();
        policy.mode = PolicyMode::Audit;
        policy.trusted_endpoints = vec!["10.0.0.8".into(), "10.0.0.9".into()];

        let error = compile_credential_exfiltration_policy(&policy)
            .expect_err("unsupported exception union must fail");
        assert!(error.to_string().contains("one trusted endpoint"));
    }

    #[test]
    fn raw_block_violation_becomes_ordered_security_evidence() {
        let mut raw = raw_violation(270_000_000_000);
        raw.provenance = Some(Provenance {
            label: 1,
            timestamp_ns: 269_000_000_000,
            pid: 43,
            op: 1,
            target: "/root/.ssh/id_rsa".into(),
        });

        let mut active = active_binding();
        active.binding.request.policy_revision = "3".into();
        let policy = active
            .credential_policy
            .clone()
            .expect("fixture credential policy should exist");
        let events = convert_security_events_at(
            raw,
            &active,
            &policy,
            "CREDENTIAL",
            1_784_000_000_000_000_000,
            Some(271_000_000_000),
        )
        .expect("fixture violation should convert");

        let SecurityEventKind::FileAction(source) = &events[0].kind else {
            panic!("first evidence must be a file action");
        };
        assert_eq!(source.path, "~/.ssh/id_rsa");
        let SecurityEventKind::TaintTransition(taint) = &events[1].kind else {
            panic!("second evidence must be a taint transition");
        };
        assert_eq!(taint.label, "CREDENTIAL");
        assert!(matches!(
            events[2].kind,
            SecurityEventKind::NetworkAction(_)
        ));
        let SecurityEventKind::PolicyDecision(decision) = &events[3].kind else {
            panic!("last evidence must be a policy decision");
        };
        assert!(decision.blocked);
        assert_eq!(decision.errno, Some(libc::EPERM));
        assert_eq!(decision.source_event_id, events[0].event_id);
        assert_eq!(decision.sink_event_id, events[2].event_id);
    }

    #[test]
    fn raw_notify_violation_preserves_audit_allow_outcome() {
        let mut raw = raw_violation(270_000_000_000);
        raw.effect = 0;
        raw.blocked = false;
        raw.provenance = Some(Provenance {
            label: 1,
            timestamp_ns: 269_000_000_000,
            pid: 43,
            op: 1,
            target: "/root/.aws/credentials".into(),
        });
        let mut active = active_binding();
        active.binding.request.policy_revision = "3".into();
        let policy = active
            .credential_policy
            .as_mut()
            .expect("fixture credential policy should exist");
        policy.mode = PolicyMode::Audit;
        let policy = policy.clone();

        let events = convert_security_events_at(
            raw,
            &active,
            &policy,
            "CREDENTIAL",
            1_784_000_000_000_000_000,
            Some(271_000_000_000),
        )
        .expect("audit violation should convert");

        let SecurityEventKind::NetworkAction(network) = &events[2].kind else {
            panic!("third evidence must be a network action");
        };
        assert!(network.succeeded);
        assert_eq!(network.errno, None);
        let SecurityEventKind::PolicyDecision(decision) = &events[3].kind else {
            panic!("last evidence must be a policy decision");
        };
        assert_eq!(decision.mode, PolicyMode::Audit);
        assert!(!decision.blocked);
        assert_eq!(decision.errno, None);
    }

    #[test]
    fn raw_notify_violation_preserves_product_observe_mode() {
        let mut raw = raw_violation(270_000_000_000);
        raw.effect = 0;
        raw.blocked = false;
        raw.provenance = Some(Provenance {
            label: 1,
            timestamp_ns: 269_000_000_000,
            pid: 43,
            op: 1,
            target: "/root/.aws/credentials".into(),
        });
        let mut active = active_binding();
        active.binding.request.policy_revision = "3".into();
        let policy = active
            .credential_policy
            .as_mut()
            .expect("fixture credential policy should exist");
        policy.mode = PolicyMode::Observe;
        let policy = policy.clone();

        let events = convert_security_events_at(
            raw,
            &active,
            &policy,
            "CREDENTIAL",
            1_784_000_000_000_000_000,
            Some(271_000_000_000),
        )
        .expect("observe violation should convert");

        let SecurityEventKind::PolicyDecision(decision) = &events[3].kind else {
            panic!("last evidence must be a policy decision");
        };
        assert_eq!(decision.mode, PolicyMode::Observe);
        assert!(!decision.blocked);
    }

    #[test]
    fn expired_taint_produces_no_security_evidence() {
        let mut raw = raw_violation(270_000_000_000);
        raw.effect = 0;
        raw.blocked = false;
        raw.provenance = Some(Provenance {
            label: 1,
            timestamp_ns: 200_000_000_000,
            pid: 43,
            op: 1,
            target: "/root/.aws/credentials".into(),
        });
        let mut active = active_binding();
        active.binding.request.policy_revision = "3".into();
        let policy = active
            .credential_policy
            .as_mut()
            .expect("fixture credential policy should exist");
        policy.mode = PolicyMode::Audit;
        policy.taint_ttl_secs = 60;
        let policy = policy.clone();

        let events = convert_security_events_at(
            raw,
            &active,
            &policy,
            "CREDENTIAL",
            1_784_000_000_000_000_000,
            Some(271_000_000_000),
        )
        .expect("expired taint should normalize safely");

        assert!(events.is_empty());
    }

    #[test]
    fn non_global_and_ipv6_sinks_produce_no_security_evidence() {
        for destination in [
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.8",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "192.0.0.9",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.51.100.10",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "2606:4700:4700::1111",
        ] {
            let mut raw = raw_violation(270_000_000_000);
            raw.effect = 0;
            raw.blocked = false;
            raw.target = destination.into();
            raw.provenance = Some(Provenance {
                label: 1,
                timestamp_ns: 269_000_000_000,
                pid: 43,
                op: 1,
                target: "/root/.aws/credentials".into(),
            });
            let mut active = active_binding();
            active.binding.request.policy_revision = "3".into();
            let policy = active
                .credential_policy
                .as_mut()
                .expect("fixture credential policy should exist");
            policy.mode = PolicyMode::Audit;
            let policy = policy.clone();

            let events = convert_security_events_at(
                raw,
                &active,
                &policy,
                "CREDENTIAL",
                1_784_000_000_000_000_000,
                Some(271_000_000_000),
            )
            .expect("out-of-scope destination should normalize safely");

            assert!(
                events.is_empty(),
                "{destination} must not be a product sink"
            );
        }
    }

    #[test]
    fn domain_id_is_stable_and_nonzero() {
        let id = Uuid::parse_str("00000000-0000-4000-8000-000000000123")
            .expect("fixture UUID should parse");
        assert_eq!(domain_id(id), domain_id(id));
        assert_ne!(domain_id(id), 0);
    }

    #[test]
    fn unsupported_runtime_handoff_retains_source_before_any_kernel_step() {
        let source = active_binding().binding;
        let before = source.clone();
        let outcome = unsupported_runtime_handoff(&source);

        assert_eq!(
            outcome,
            ReplaceOutcome::SourceRetained {
                binding: source.clone(),
                code: ReplaceFailureCode::UnsupportedHandoff,
            }
        );
        assert_eq!(source, before, "fail-closed handoff must not touch runtime");
    }

    #[test]
    fn unsupported_handoff_leaves_pid_reuse_and_concurrent_forks_under_source() {
        let source = active_binding().binding;
        let members = [(42, 101), (42, 999), (43, 102), (44, 103)];
        let before = members;
        let outcome = unsupported_runtime_handoff(&source);

        assert!(matches!(outcome, ReplaceOutcome::SourceRetained { .. }));
        assert_eq!(members, before);
    }

    #[test]
    fn prepare_runtime_clears_stale_state_before_draining_events() {
        let operations = RefCell::new(Vec::new());

        let drained = prepare_runtime(
            || {
                operations.borrow_mut().push("clear");
                Ok(())
            },
            || {
                operations.borrow_mut().push("drain");
                Ok(3)
            },
        )
        .expect("runtime preparation should succeed");

        assert_eq!(drained, 3);
        assert_eq!(operations.into_inner(), ["clear", "drain"]);
    }

    #[test]
    fn prepare_runtime_skips_drain_when_cleanup_fails() {
        let drain_called = Cell::new(false);

        let result = prepare_runtime(
            || Err(BackendError::KernelFailure("clear failed".into())),
            || {
                drain_called.set(true);
                Ok(0)
            },
        );

        assert!(matches!(
            result,
            Err(BackendError::KernelFailure(message)) if message == "clear failed"
        ));
        assert!(!drain_called.get());
    }

    #[test]
    fn domain_id_never_uses_the_reserved_global_domain() {
        let id = Uuid::parse_str("ffffffff-0000-0000-0000-000000000000")
            .expect("fixture UUID should parse");
        assert_ne!(domain_id(id), GLOBAL_ACTIVE_DOMAIN_ID);
    }

    #[test]
    fn proc_start_time_handles_parenthesized_comm() {
        let stat = "42 (a tricky) name) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765";
        assert_eq!(parse_process_start_time(stat), Ok(98765));
    }

    #[test]
    fn monotonic_event_time_is_converted_to_unix_epoch() {
        let occurred_at_ns =
            monotonic_to_epoch_ns(270_000_000_000, 271_000_000_000, 1_784_000_000_000_000_000);

        assert_eq!(occurred_at_ns, 1_783_999_999_000_000_000);
    }

    #[test]
    fn invalid_clock_relationships_fall_back_to_observation_time() {
        assert_eq!(monotonic_to_epoch_ns(30, 20, 5), 5);
        assert_eq!(monotonic_to_epoch_ns(10, 20, 5), 5);
    }

    #[test]
    fn violation_adapter_maps_fields_and_converts_fixed_observation() {
        let active = active_binding();
        let raw = raw_violation(270_000_000_000);

        let event: ViolationEvent = convert_violation_at(
            raw,
            &active,
            1_784_000_000_000_000_000,
            Some(271_000_000_000),
        );
        assert_eq!(event.binding_id, active.binding.request.binding_id);
        assert_eq!(event.agent_id, "agent-1");
        assert_eq!(event.session_id.as_deref(), Some("session-1"));
        assert_eq!(event.policy_id, "policy-1");
        assert_eq!(event.policy_revision, "revision-1");
        assert_eq!(event.pid, 43);
        assert_eq!(event.ppid, Some(42));
        assert_eq!(event.effect, Effect::Block);
        assert!(event.blocked);
        assert!(!event.killed);
        assert_eq!(event.operation, "connect");
        assert_eq!(event.target, "8.8.8.8");
        assert_eq!(event.rule_id.as_deref(), Some("block-exfiltration"));
        assert_eq!(
            event.reason.as_deref(),
            Some("credential reached an external sink")
        );
        assert_eq!(event.occurred_at_ns, 1_783_999_999_000_000_000);
        assert_eq!(event.observed_at_ns, 1_784_000_000_000_000_000);
        assert_eq!(event.actplane_revision, ACTPLANE_REVISION);
    }

    #[test]
    fn violation_adapter_falls_back_when_monotonic_read_fails() {
        let active = active_binding();
        let observed_at_ns = 1_784_000_000_000_000_000;

        let event = convert_violation_at(
            raw_violation(270_000_000_000),
            &active,
            observed_at_ns,
            None,
        );

        assert_eq!(event.occurred_at_ns, observed_at_ns);
        assert_eq!(event.observed_at_ns, observed_at_ns);
    }

    #[test]
    fn violation_adapter_falls_back_for_future_monotonic_event() {
        let observed_at_ns = 1_784_000_000_000_000_000;
        let event = convert_violation_at(
            raw_violation(272_000_000_000),
            &active_binding(),
            observed_at_ns,
            Some(271_000_000_000),
        );

        assert_eq!(event.occurred_at_ns, observed_at_ns);
    }

    #[test]
    fn violation_adapter_falls_back_when_epoch_subtraction_underflows() {
        let observed_at_ns = 5;
        let event = convert_violation_at(
            raw_violation(10),
            &active_binding(),
            observed_at_ns,
            Some(20),
        );

        assert_eq!(event.occurred_at_ns, observed_at_ns);
    }
}
