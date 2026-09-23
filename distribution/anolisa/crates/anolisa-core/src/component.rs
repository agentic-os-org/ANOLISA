//! Core component trait and metadata types.

use anolisa_env::EnvFacts;
use std::collections::HashMap;

/// Metadata describing an ANOLISA component.
#[derive(Debug, Clone)]
pub struct ComponentMeta {
    /// Stable component name.
    pub name: String,
    /// Component version declared by its manifest.
    pub version: String,
    /// Architectural layer this component belongs to.
    pub layer: Layer,
    /// Functional domain within the runtime layer.
    pub domain: Domain,
    /// Human-readable component summary.
    pub description: String,
}

/// Architecture layer a component belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer {
    /// OS base layer component.
    Osbase,
    /// Runtime layer component.
    Runtime,
    /// Adapter/encapsulation component.
    Encapsulation,
}

/// Functional domain within the runtime layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Domain {
    /// Tooling or command execution domain.
    Tools,
    /// Persistent state/checkpoint domain.
    State,
    /// Cost/token optimization domain.
    Cost,
    /// Security/audit domain.
    Security,
    /// Observability/telemetry domain.
    Observability,
}

/// Feature definition for a component.
#[derive(Debug, Clone)]
pub struct FeatureDef {
    /// Stable feature name.
    pub name: String,
    /// Display label.
    pub label: String,
    /// Whether the feature is enabled by default.
    pub default: bool,
    /// Environment requirements that gate this feature.
    pub requires_env: HashMap<String, String>,
    /// Feature names that cannot be enabled at the same time.
    pub conflicts_with: Vec<String>,
}

/// Health/status of a component.
#[derive(Debug, Clone)]
pub enum ComponentStatus {
    /// Component is healthy.
    Ok,
    /// Component is usable with a known degradation.
    Degraded {
        /// Why the component is not fully healthy.
        reason: String,
    },
    /// Component is installed but not running.
    Stopped,
    /// Component is absent from the host.
    NotInstalled,
    /// Component health probe returned an error.
    Error {
        /// Error detail from the health probe or manager.
        message: String,
    },
}

/// Pre-check result for environment compatibility.
#[derive(Debug)]
pub enum PreCheckResult {
    /// Host facts satisfy the requirement.
    Compatible,
    /// Host facts satisfy only part of the requirement.
    Partial {
        /// Requirement that only partially matched.
        reason: String,
        /// Suggested remediation.
        advice: String,
    },
    /// Host facts fail the requirement.
    Incompatible {
        /// Requirement that failed.
        reason: String,
        /// Suggested remediation.
        advice: String,
    },
}

/// The core trait every installable component implements.
pub trait Component {
    /// Static metadata used by planners and status renderers.
    fn metadata(&self) -> &ComponentMeta;
    /// Evaluate host facts before planning an install.
    fn check_env(&self, facts: &EnvFacts) -> PreCheckResult;
    /// Feature definitions exposed by this component.
    fn features(&self) -> &[FeatureDef];
    /// Current runtime status.
    fn status(&self) -> ComponentStatus;
}
