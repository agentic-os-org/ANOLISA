//! Immutable extension runtime snapshots and safe-point activation.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::skill::SkillConfig;
use crate::tool::ToolRegistry;

use super::{
    AgentRegistry, ExtensionContextSnapshot, ExtensionDiagnostic, ExtensionHealth, ExtensionHooks,
    McpRuntime,
};

/// Immutable metadata identifying one complete extension runtime snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeGeneration {
    /// Monotonic generation ID.
    pub id: u64,
    /// Stable catalog and package-content fingerprint.
    pub fingerprint: String,
    /// Whether all required contributions validated.
    pub healthy: bool,
    /// Whether a linked source changed after candidate construction.
    pub stale: bool,
}

impl RuntimeGeneration {
    /// Creates healthy metadata for a fully validated snapshot.
    pub fn healthy(id: u64, fingerprint: impl Into<String>) -> Self {
        Self {
            id,
            fingerprint: fingerprint.into(),
            healthy: true,
            stale: false,
        }
    }
}

/// Complete immutable capability set consumed by one Agent run.
pub struct RuntimeSnapshot {
    /// Generation metadata used by management and safe reload.
    pub generation: RuntimeGeneration,
    /// Fully loaded skill definitions, including prompt bodies.
    pub skills: Vec<SkillConfig>,
    /// Package identities enabled in this immutable generation.
    pub active_extensions: BTreeSet<String>,
    /// Per-package health after every runtime contribution has been validated.
    pub extension_health: BTreeMap<String, ExtensionHealth>,
    /// Tool registry bound to this generation, including MCP tools.
    pub tools: Arc<ToolRegistry>,
    /// Validated extension hooks bound to this generation.
    pub hooks: ExtensionHooks,
    /// Bounded extension context captured for this generation.
    pub context: ExtensionContextSnapshot,
    /// MCP child processes and discovered tools owned by this generation.
    pub mcp: Arc<McpRuntime>,
    /// Strict, policy-intersected extension agent declarations.
    pub agents: AgentRegistry,
    /// Redaction-safe diagnostics captured during candidate construction.
    pub diagnostics: Vec<ExtensionDiagnostic>,
}

impl RuntimeSnapshot {
    /// Builds a snapshot from already validated, generation-owned contributions.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        generation: RuntimeGeneration,
        skills: Vec<SkillConfig>,
        active_extensions: BTreeSet<String>,
        extension_health: BTreeMap<String, ExtensionHealth>,
        tools: Arc<ToolRegistry>,
        hooks: ExtensionHooks,
        context: ExtensionContextSnapshot,
        mcp: Arc<McpRuntime>,
        agents: AgentRegistry,
        diagnostics: Vec<ExtensionDiagnostic>,
    ) -> Self {
        Self {
            generation,
            skills,
            active_extensions,
            extension_health,
            tools,
            hooks,
            context,
            mcp,
            agents,
            diagnostics,
        }
    }

    /// Creates a base snapshot for tests and extension-free startup.
    pub fn bootstrap(generation: RuntimeGeneration, tools: Arc<ToolRegistry>) -> RuntimeSnapshot {
        Self::new(
            generation,
            Vec::new(),
            BTreeSet::new(),
            BTreeMap::new(),
            tools,
            ExtensionHooks::default(),
            ExtensionContextSnapshot::default(),
            Arc::new(McpRuntime::default()),
            AgentRegistry::default(),
            Vec::new(),
        )
    }
}

impl fmt::Debug for RuntimeSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSnapshot")
            .field("generation", &self.generation)
            .field("skills", &self.skills.len())
            .field("active_extensions", &self.active_extensions)
            .field("extension_health", &self.extension_health)
            .field("tools", &self.tools.names())
            .field("hooks_empty", &self.hooks.is_empty())
            .field(
                "context_bytes",
                &self.context.rendered().map(str::len).unwrap_or(0),
            )
            .field("mcp_servers", &self.mcp.statuses().len())
            .field("agents", &self.agents.list().len())
            .field("diagnostics", &self.diagnostics)
            .finish()
    }
}

/// Result of requesting a generation switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// Candidate became current atomically.
    Activated,
    /// Active runs keep the old generation pinned until their safe point.
    PendingSafeReload,
    /// Candidate failed required validation.
    CandidateUnhealthy,
    /// Candidate source changed after validation.
    CandidateStale,
    /// No candidate is available.
    NoCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationStatus {
    /// Current generation ID.
    pub current: u64,
    /// Staged candidate generation ID, if any.
    pub candidate: Option<u64>,
    /// Number of Agent runs pinned to the current snapshot.
    pub active_runs: usize,
    /// Whether activation is waiting for the next safe point.
    pub pending: bool,
}

#[derive(Debug)]
struct State {
    current: Arc<RuntimeSnapshot>,
    candidate: Option<Arc<RuntimeSnapshot>>,
    retired: Vec<Arc<RuntimeSnapshot>>,
    active_runs: usize,
    pending: bool,
}

/// Long-lived owner of current, candidate, retired, and active-run snapshots.
#[derive(Debug, Clone)]
pub struct GenerationController {
    state: Arc<Mutex<State>>,
}

impl GenerationController {
    /// Creates a controller with one already-active snapshot.
    pub fn new(current: RuntimeSnapshot) -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                current: Arc::new(current),
                candidate: None,
                retired: Vec::new(),
                active_runs: 0,
                pending: false,
            })),
        }
    }

    /// Returns the current immutable snapshot.
    pub fn current(&self) -> Arc<RuntimeSnapshot> {
        Arc::clone(&self.state.lock().unwrap().current)
    }

    /// Returns redaction-safe generation state for runtime queries.
    pub fn status(&self) -> GenerationStatus {
        let state = self.state.lock().unwrap();
        GenerationStatus {
            current: state.current.generation.id,
            candidate: state
                .candidate
                .as_ref()
                .map(|snapshot| snapshot.generation.id),
            active_runs: state.active_runs,
            pending: state.pending,
        }
    }

    /// Stages a complete candidate without changing active runs.
    pub fn stage(&self, candidate: RuntimeSnapshot) -> Option<Arc<RuntimeSnapshot>> {
        let mut state = self.state.lock().unwrap();
        let previous = state.candidate.replace(Arc::new(candidate));
        state.pending = false;
        previous
    }

    /// Removes an unpublished candidate so its resources can be shut down after failure.
    pub fn discard_candidate(&self) -> Option<Arc<RuntimeSnapshot>> {
        let mut state = self.state.lock().unwrap();
        state.pending = false;
        state.candidate.take()
    }

    /// Marks a linked candidate stale without mutating the current generation.
    pub fn mark_candidate_stale(&self) {
        let mut state = self.state.lock().unwrap();
        if let Some(candidate) = state.candidate.as_mut() {
            if let Some(candidate) = Arc::get_mut(candidate) {
                candidate.generation.stale = true;
            }
        }
    }

    /// Pins the current snapshot for one active Agent run.
    pub fn pin(&self) -> GenerationPin {
        let mut state = self.state.lock().unwrap();
        state.active_runs += 1;
        GenerationPin {
            snapshot: Arc::clone(&state.current),
            state: Arc::clone(&self.state),
        }
    }

    /// Activates a healthy candidate now or at the next safe point.
    pub fn reload(&self) -> ReloadOutcome {
        let mut state = self.state.lock().unwrap();
        let Some(candidate) = state.candidate.as_ref() else {
            return ReloadOutcome::NoCandidate;
        };
        if !candidate.generation.healthy {
            return ReloadOutcome::CandidateUnhealthy;
        }
        if candidate.generation.stale {
            return ReloadOutcome::CandidateStale;
        }
        if state.active_runs != 0 {
            state.pending = true;
            return ReloadOutcome::PendingSafeReload;
        }
        activate_candidate(&mut state);
        ReloadOutcome::Activated
    }

    /// Takes snapshots that are no longer pinned so their resources can drain.
    pub fn take_retired(&self) -> Vec<Arc<RuntimeSnapshot>> {
        std::mem::take(&mut self.state.lock().unwrap().retired)
    }
}

/// Active-run binding released automatically at the safe point.
pub struct GenerationPin {
    snapshot: Arc<RuntimeSnapshot>,
    state: Arc<Mutex<State>>,
}

impl GenerationPin {
    /// Returns the complete immutable snapshot seen by this run.
    pub fn snapshot(&self) -> &RuntimeSnapshot {
        &self.snapshot
    }

    /// Returns generation metadata seen by this run.
    pub fn generation(&self) -> &RuntimeGeneration {
        &self.snapshot.generation
    }
}

impl Drop for GenerationPin {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.active_runs = state.active_runs.saturating_sub(1);
        if state.active_runs == 0 && state.pending {
            let eligible = state.candidate.as_ref().is_some_and(|candidate| {
                candidate.generation.healthy && !candidate.generation.stale
            });
            if eligible {
                activate_candidate(&mut state);
            }
        }
    }
}

fn activate_candidate(state: &mut State) {
    if let Some(candidate) = state.candidate.take() {
        let previous = std::mem::replace(&mut state.current, candidate);
        state.retired.push(previous);
    }
    state.pending = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: u64, fingerprint: &str) -> RuntimeSnapshot {
        RuntimeSnapshot::bootstrap(
            RuntimeGeneration::healthy(id, fingerprint),
            Arc::new(ToolRegistry::new()),
        )
    }

    #[test]
    fn idle_reload_switches_complete_snapshot_atomically() {
        let controller = GenerationController::new(snapshot(1, "one"));
        controller.stage(snapshot(2, "two"));
        assert_eq!(controller.reload(), ReloadOutcome::Activated);
        assert_eq!(controller.current().generation.id, 2);
        let retired = controller.take_retired();
        assert_eq!(retired.len(), 1);
        assert_eq!(retired[0].generation.id, 1);
    }

    #[test]
    fn active_run_stays_pinned_until_safe_point() {
        let controller = GenerationController::new(snapshot(1, "one"));
        let pin = controller.pin();
        controller.stage(snapshot(2, "two"));
        assert_eq!(controller.reload(), ReloadOutcome::PendingSafeReload);
        assert_eq!(pin.generation().id, 1);
        assert_eq!(controller.current().generation.id, 1);
        drop(pin);
        assert_eq!(controller.current().generation.id, 2);
        assert_eq!(controller.take_retired()[0].generation.id, 1);
    }

    #[test]
    fn unhealthy_or_stale_candidate_never_replaces_current() {
        let controller = GenerationController::new(snapshot(1, "one"));
        let mut unhealthy = snapshot(2, "bad");
        unhealthy.generation.healthy = false;
        controller.stage(unhealthy);
        assert_eq!(controller.reload(), ReloadOutcome::CandidateUnhealthy);
        assert_eq!(controller.current().generation.id, 1);

        controller.stage(snapshot(3, "linked"));
        controller.mark_candidate_stale();
        assert_eq!(controller.reload(), ReloadOutcome::CandidateStale);
        assert_eq!(controller.current().generation.id, 1);
        assert!(controller.take_retired().is_empty());
    }
}
