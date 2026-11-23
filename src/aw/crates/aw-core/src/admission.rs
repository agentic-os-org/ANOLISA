//! Pure Hook policy admission over one exact configuration snapshot.
//!
//! This module owns the event, effect, selector and failure-action admission
//! rules, and binds their output to the parsed configuration, target, Adapter
//! evidence and exact-byte revision that produced it. Admission starts no
//! process, opens no socket and writes no journal. Native Hook readiness, Agent
//! authorization and tool dispatch checks remain caller responsibilities.
//!
//! Admitted types have private fields and no public constructor or
//! deserializer:
//!
//! ```compile_fail
//! let policy = aw_core::admission::AdmittedPolicy {
//!     config: unimplemented!(),
//!     revision: String::new(),
//!     target: String::new(),
//!     capabilities: unimplemented!(),
//!     providers: unimplemented!(),
//!     steps: Vec::new(),
//! };
//! ```
//!
//! ```compile_fail
//! let policy: aw_core::admission::AdmittedPolicy =
//!     serde_json::from_value(serde_json::json!({})).unwrap();
//! ```
//!
//! Steps are exposed as a read-only slice:
//!
//! ```compile_fail
//! fn inject(policy: &mut aw_core::admission::AdmittedPolicy) {
//!     let step = policy.steps()[0].clone();
//!     policy.steps().push(step);
//! }
//! ```

mod rules;
mod selection;

pub use aw_provider::ProviderEvidence;
pub use rules::{admit, preflight};
pub use selection::{matches_tool_selectors, projected_out_steps, ProjectedOutStep};

use aw_config::{Configuration, Validator};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// Trusted Adapter evidence for a specific version and interaction entrypoint.
///
/// The embedding application supplies this evidence independently of Provider
/// claims. Admission does not probe the Agent or certify the supplied version.
#[derive(Clone, Debug)]
pub struct AdapterCapabilities {
    /// One of the adapters declared by the configuration contract.
    pub adapter: String,
    /// The actual Agent version whose capabilities were established by the caller.
    pub version: String,
    /// The validated interaction mode, such as a print or interactive entrypoint.
    pub entrypoint: String,
    /// Supported effects by event, restricted to this library's initial surface.
    pub events: BTreeMap<String, Vec<String>>,
    /// Additional failure actions supported by each event at this exact entrypoint.
    pub failure_actions: BTreeMap<String, Vec<String>>,
}

/// One configured step checked against the implementation and Adapter.
///
/// Structured steps returned by [`admit`] have also passed Provider admission;
/// native steps require no Provider handshake. Steps returned by [`preflight`]
/// remain candidates, and public fields carry no proof
/// of admission. This is not an executable Core plan or a Ready status. Runtime
/// binding, budgets, process ownership and adoption remain Host concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdmittedStep {
    /// The event that selects this step.
    pub event: String,
    /// The step identifier within its event.
    pub step_id: String,
    /// The named Provider in the desired configuration.
    pub provider: String,
    /// Structured Provider evaluation or uninterpreted native hook execution.
    pub execution: StepExecution,
    /// The Host action required when Provider execution fails.
    pub on_error: String,
    /// Event-level tool selectors; intersected with the step selectors.
    pub event_tools: Vec<String>,
    /// Step-level tool selectors; omitted configuration admits all tools.
    pub step_tools: Vec<String>,
}

impl AdmittedStep {
    /// Match a validated normalized event against both configured tool selectors.
    ///
    /// Standard selectors compare `tool.name`; exact native selectors compare the
    /// Adapter ID and retained `tool.native_name`. No aliases or globs are inferred.
    /// A mismatch still consumes the step and participates in ordered completion.
    pub fn matches_tool(&self, event: &Value) -> bool {
        selection::matches_tool_selectors(
            &self.event_tools,
            &self.step_tools,
            event["agent"]["adapter"].as_str(),
            event["tool"]["name"].as_str(),
            event["tool"]["native_name"].as_str(),
        )
    }
}

/// Execution contract selected by the configured protocol or its documented default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepExecution {
    /// Invoke a boolean command once; the Host projects configured effects.
    Hook {
        /// Effects to request when the command returns true.
        effects: Vec<String>,
        /// Optional configured explanation for a match.
        reason_code: Option<String>,
    },
    /// Validate requests and candidate effects with aw-provider/v1alpha1.
    Provider {
        /// Operation declared by the Provider.
        operation: String,
        /// Requested effects checked against Provider and Adapter evidence.
        effects: Vec<String>,
    },
    /// Preserve raw callback input, output and process status for the Adapter.
    Native,
}

impl StepExecution {
    /// Whether this structured step requests a model-visible text replacement.
    pub fn replaces_result(&self) -> bool {
        matches!(self, Self::Provider { effects, .. } if effects.iter().any(|effect| effect == "replace_result"))
    }
}

/// Admission failures; diagnostics omit configuration values.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    /// The bytes are not a valid AW configuration document.
    #[error(transparent)]
    Configuration(#[from] aw_config::Error),
    /// Static or evidence-based capability admission rejected the policy.
    #[error("policy admission rejected: {0}")]
    Rejected(&'static str),
}

/// Stateless admission entrypoint holding the compiled configuration schema.
pub struct AdmissionEngine {
    validator: Validator,
}

impl AdmissionEngine {
    /// Compiles the bundled configuration schema without network access.
    ///
    /// # Errors
    /// Returns [`AdmissionError::Configuration`] if the bundled schema is invalid.
    pub fn new() -> Result<Self, AdmissionError> {
        Ok(Self {
            validator: Validator::new()?,
        })
    }

    /// Parses the exact configuration bytes and checks them statically for one target.
    ///
    /// `capabilities` must come from the trusted Adapter integration. The revision
    /// is the lowercase hexadecimal SHA-256 of `bytes`. No Provider or Agent
    /// executable is started.
    ///
    /// # Errors
    /// Returns [`AdmissionError::Configuration`] for an invalid document and
    /// [`AdmissionError::Rejected`] for an unsupported target, event, effect,
    /// selector, transport or failure action.
    pub fn preflight(
        &self,
        bytes: &[u8],
        target: &str,
        capabilities: AdapterCapabilities,
    ) -> Result<PolicyCandidate, AdmissionError> {
        let config = self.validator.parse(bytes)?;
        let requirements = preflight(&config, target, &capabilities)?;
        Ok(PolicyCandidate {
            config,
            revision: format!("{:x}", Sha256::digest(bytes)),
            target: target.to_owned(),
            capabilities,
            requirements,
        })
    }

    /// Admits a candidate with Provider evidence discovered from the same snapshot.
    ///
    /// The caller's trusted Host must have protocol-checked each evidence entry
    /// for the named configured Provider. Native and boolean Hook steps need no
    /// evidence. The candidate is consumed so its snapshot stays fixed.
    ///
    /// # Errors
    /// Returns [`AdmissionError::Rejected`] for missing or mismatched evidence.
    pub fn admit(
        &self,
        candidate: PolicyCandidate,
        providers: &BTreeMap<String, ProviderEvidence>,
    ) -> Result<AdmittedPolicy, AdmissionError> {
        let PolicyCandidate {
            config,
            revision,
            target,
            capabilities,
            requirements,
        } = candidate;
        rules::admit_requirements(&config, &requirements, &capabilities.adapter, providers)?;
        Ok(AdmittedPolicy {
            config,
            revision,
            target,
            capabilities,
            providers: providers.clone(),
            steps: requirements,
        })
    }
}

/// Statically checked policy awaiting Provider evidence for one configuration snapshot.
pub struct PolicyCandidate {
    config: Configuration,
    revision: String,
    target: String,
    capabilities: AdapterCapabilities,
    requirements: Vec<AdmittedStep>,
}

impl PolicyCandidate {
    /// The validated configuration parsed from the revision bytes.
    pub fn configuration(&self) -> &Configuration {
        &self.config
    }

    /// Lowercase hexadecimal SHA-256 of the exact configuration bytes.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The configured Agent target that was checked.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Trusted Adapter evidence supplied by the caller.
    pub fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    /// Enabled applicable steps in configured order before Provider evidence checks.
    pub fn requirements(&self) -> &[AdmittedStep] {
        &self.requirements
    }
}

/// Immutable admitted Hook policy bound to one configuration snapshot.
///
/// Runtime binding, native Hook readiness, Provider process identity and
/// per-call checks remain caller responsibilities.
pub struct AdmittedPolicy {
    config: Configuration,
    revision: String,
    target: String,
    capabilities: AdapterCapabilities,
    providers: BTreeMap<String, ProviderEvidence>,
    steps: Vec<AdmittedStep>,
}

impl AdmittedPolicy {
    /// The validated configuration whose bytes produced [`Self::revision`].
    pub fn configuration(&self) -> &Configuration {
        &self.config
    }

    /// Lowercase hexadecimal SHA-256 of the exact configuration bytes.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// The configured Agent target that was admitted.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Trusted Adapter evidence used for admission.
    pub fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    /// Protocol-checked Provider evidence bound to this admitted policy.
    pub fn providers(&self) -> &BTreeMap<String, ProviderEvidence> {
        &self.providers
    }

    /// Admitted steps in configured order within each event.
    pub fn steps(&self) -> &[AdmittedStep] {
        &self.steps
    }
}

impl fmt::Debug for PolicyCandidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Configuration may contain Provider-private values; omit it.
        f.debug_struct("PolicyCandidate")
            .field("revision", &self.revision)
            .field("target", &self.target)
            .field("capabilities", &self.capabilities)
            .field("requirements", &self.requirements)
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AdmittedPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdmittedPolicy")
            .field("revision", &self.revision)
            .field("target", &self.target)
            .field("capabilities", &self.capabilities)
            .field("steps", &self.steps)
            .finish_non_exhaustive()
    }
}
