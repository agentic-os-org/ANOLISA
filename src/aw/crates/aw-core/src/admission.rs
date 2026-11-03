//! Pure Hook policy admission over one exact configuration snapshot.
//!
//! `aw_provider::admission` owns the event, effect, selector and failure-action
//! rules. This module binds their output to the parsed configuration, target,
//! Adapter evidence and exact-byte revision that produced it. Admission starts
//! no process, opens no socket and writes no journal. Native Hook readiness,
//! Agent authorization and tool dispatch checks remain caller responsibilities.
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

use aw_config::{Configuration, Validator};
use aw_provider::admission::{self, AdapterCapabilities, AdmittedStep, ProviderEvidence};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// Admission failures; diagnostics omit configuration values.
#[derive(Debug, thiserror::Error)]
pub enum AdmissionError {
    /// The bytes are not a valid AW configuration document.
    #[error(transparent)]
    Configuration(#[from] aw_config::Error),
    /// Static or evidence-based capability admission rejected the policy.
    #[error(transparent)]
    Provider(#[from] aw_provider::Error),
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
    /// [`AdmissionError::Provider`] for an unsupported target, event, effect,
    /// selector, transport or failure action.
    pub fn preflight(
        &self,
        bytes: &[u8],
        target: &str,
        capabilities: AdapterCapabilities,
    ) -> Result<PolicyCandidate, AdmissionError> {
        let config = self.validator.parse(bytes)?;
        let requirements = admission::preflight(&config, target, &capabilities)?;
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
    /// Returns [`AdmissionError::Provider`] for missing or mismatched evidence or
    /// any static rule rejected by the shared Provider admission.
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
            requirements: _,
        } = candidate;
        let steps = admission::admit(&config, &target, &capabilities, providers)?;
        Ok(AdmittedPolicy {
            config,
            revision,
            target,
            capabilities,
            providers: providers.clone(),
            steps,
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
