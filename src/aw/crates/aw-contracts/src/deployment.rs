//! Validated deployment hints for SDK startup, without runtime admission proof.

use crate::{canonical, Error, Registry};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Version of the deployment hint document understood by this build.
pub const VERSION: &str = "aw-deployment/v1alpha1";

/// Service availability behavior selected by the integration profile and policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// Wait for AW admission before protected work.
    Required,
    /// Permit native operation only under the existing fallback contract.
    NativeFallback,
}

/// Identity of the trusted capability profile used during offline compilation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentProfile {
    /// Configured Adapter implementation.
    pub adapter: String,
    /// Version of the verified integration surface.
    pub version: String,
    /// Native interaction mode for which capabilities were supplied.
    pub entrypoint: String,
}

/// Strictly decoded SDK startup hints bound to configuration bytes and a target.
///
/// The document establishes shape and identity only. SDKs must verify the
/// configuration digest and expected target/profile before using its hints;
/// Service still performs authoritative admission and Ready handling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DeploymentDescriptor(Fields);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fields {
    version: String,
    configuration_sha256: String,
    target: String,
    profile: DeploymentProfile,
    endpoint: String,
    wait_for_input_submit: bool,
    availability: Availability,
}

impl DeploymentDescriptor {
    /// Parses bounded JSON, rejecting duplicate keys, unknown fields and versions.
    ///
    /// # Errors
    /// Returns a contract error when encoding or document shape is invalid.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        static REGISTRY: OnceLock<Result<Registry, Error>> = OnceLock::new();
        let registry = REGISTRY
            .get_or_init(Registry::new)
            .as_ref()
            .map_err(|_| Error::InvalidSchema("deployment-descriptor-v1alpha1".into()))?;
        let value = canonical::parse(bytes)?;
        registry.validate("deployment-descriptor-v1alpha1", &value)?;
        serde_json::from_value(value)
            .map(Self)
            .map_err(|_| Error::SchemaMismatch("deployment-descriptor-v1alpha1".into()))
    }

    /// Exact version of the deployment hint contract.
    pub fn version(&self) -> &str {
        &self.0.version
    }

    /// SHA-256 of the original configuration bytes, including whitespace.
    pub fn configuration_sha256(&self) -> &str {
        &self.0.configuration_sha256
    }

    /// Configured Agent target selected at deployment time.
    pub fn target(&self) -> &str {
        &self.0.target
    }

    /// Trusted profile identity used for static capability admission.
    pub fn profile(&self) -> &DeploymentProfile {
        &self.0.profile
    }

    /// Absolute service socket path resolved at deployment time.
    pub fn endpoint(&self) -> &str {
        &self.0.endpoint
    }

    /// Whether input submission must wait for AW before reaching the model.
    pub fn wait_for_input_submit(&self) -> bool {
        self.0.wait_for_input_submit
    }

    /// Existing service availability contract selected during compilation.
    pub fn availability(&self) -> Availability {
        self.0.availability
    }
}
