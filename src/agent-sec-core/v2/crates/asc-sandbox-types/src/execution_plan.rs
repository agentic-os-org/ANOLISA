//! Backend selection pinned at approval and evidence returned after installation.
use super::{ExecutionSpec, LandlockMode, LandlockStatus, LinuxExecutionSpec, LinuxSandboxOptions};
use serde::{Deserialize, Serialize, Serializer};

/// Backend and settings that must survive an approval handoff together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendSelection {
    /// Execute using the local Linux sandbox helper.
    LinuxSandbox(LinuxSandboxOptions),
    /// Place the workload through the configured Docker backend.
    Docker {},
}

/// Accepted permissions and the specific backend that preflighted them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(from = "PlanWire<ExecutionSpec>")]
pub struct ExecutionPlan {
    /// Permission and path bindings validated against the PDP decision.
    pub spec: ExecutionSpec,
    /// Deployment selection retained through approval.
    pub backend: BackendSelection,
}

#[derive(Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
enum PlanWire<S> {
    Selected { backend: BackendSelection, spec: S },
    Linux(LinuxExecutionSpec<S>),
}

impl From<PlanWire<ExecutionSpec>> for ExecutionPlan {
    fn from(wire: PlanWire<ExecutionSpec>) -> Self {
        match wire {
            PlanWire::Selected { backend, spec } => Self { spec, backend },
            PlanWire::Linux(LinuxExecutionSpec { spec, landlock }) => Self {
                spec,
                backend: BackendSelection::LinuxSandbox(LinuxSandboxOptions { landlock }),
            },
        }
    }
}

impl Serialize for ExecutionPlan {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Existing Cosh clients exchange the flat Linux helper spec with the daemon.
        let wire = match self.backend {
            BackendSelection::LinuxSandbox(options) => PlanWire::Linux(LinuxExecutionSpec {
                spec: &self.spec,
                landlock: options.landlock,
            }),
            backend @ BackendSelection::Docker {} => PlanWire::Selected {
                backend,
                spec: &self.spec,
            },
        };
        wire.serialize(serializer)
    }
}

/// Evidence supplied by the backend in addition to the generic process lifecycle.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "EvidenceWire", into = "EvidenceWire")]
pub enum BackendEvidence {
    /// No additional restrictions were reported.
    #[default]
    None,
    /// Linux sandbox installed or explicitly skipped the requested write ceiling.
    LinuxSandbox { landlock: LandlockStatus },
}

impl BackendEvidence {
    /// Validate the installation handshake against the approved backend, when the
    /// daemon holds a handoff. Direct PEP reports have no retained selection.
    pub fn matches_installation(
        &self,
        accepted: Option<BackendSelection>,
        installed: bool,
    ) -> bool {
        match (self, accepted) {
            (Self::None, Some(BackendSelection::LinuxSandbox(options))) => {
                !installed || options.landlock.is_off()
            }
            (Self::None, _) => true,
            (Self::LinuxSandbox { landlock }, Some(BackendSelection::LinuxSandbox(options))) => {
                installed && landlock.satisfies(options.landlock)
            }
            (Self::LinuxSandbox { landlock }, None) => {
                installed && landlock.satisfies(LandlockMode::Auto)
            }
            (Self::LinuxSandbox { .. }, Some(BackendSelection::Docker {})) => false,
        }
    }
}

// Keep existing audit consumers' result.landlock field and omit it for other backends.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvidenceWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    landlock: Option<LandlockStatus>,
}

impl From<EvidenceWire> for BackendEvidence {
    fn from(wire: EvidenceWire) -> Self {
        match wire.landlock {
            Some(landlock) => Self::LinuxSandbox { landlock },
            None => Self::None,
        }
    }
}

impl From<BackendEvidence> for EvidenceWire {
    fn from(evidence: BackendEvidence) -> Self {
        Self {
            landlock: match evidence {
                BackendEvidence::LinuxSandbox { landlock } => Some(landlock),
                BackendEvidence::None => None,
            },
        }
    }
}
