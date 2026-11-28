//! Validated deployment hints for SDK startup, without runtime admission proof.

use crate::{canonical, instrumentation::Runtime, Error, Registry};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::OnceLock};

/// Version of the deployment hint document understood by this build.
pub const VERSION: &str = "aw-deployment/v1alpha1";

/// Version of the public semantic SDK callbacks.
pub const SDK_PROTOCOL: &str = "aw-hook-source/v1";
/// Entrypoint for application-owned SDK callbacks.
pub const SDK_ENTRYPOINT: &str = "application";
/// Maximum semantic effects carried by public SDK callbacks.
pub const SDK_EVENTS: &[(&str, &[&str])] = &[
    ("session.start", &["observe"]),
    ("turn.stop", &["observe", "block"]),
    ("session.end", &["observe"]),
    ("input.submit", &["observe", "block"]),
    ("tool.before", &["observe", "block", "replace_input"]),
    ("tool.after", &["observe", "replace_result"]),
];

/// Canonical profile digests and the resulting SDK capability ceiling.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentationBinding {
    /// Language runtime of the process binding.
    pub runtime: Runtime,
    /// SHA-256 of each profile's canonical JSON bytes, indexed by builtin ID.
    pub profiles: BTreeMap<String, String>,
    /// Supported effects per semantic Hook.
    pub events: BTreeMap<String, Vec<String>>,
    /// Additional failure actions (blocking and reporting use event effects).
    pub failure_actions: BTreeMap<String, Vec<String>>,
}

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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    observe_session_start: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow_plaintext_http_hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model_transport: Option<String>,
    availability: Availability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    instrumentation_profiles: Option<InstrumentationBinding>,
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
        let fields: Fields = serde_json::from_value(value)
            .map_err(|_| Error::SchemaMismatch("deployment-descriptor-v1alpha1".into()))?;
        if let Some(binding) = &fields.instrumentation_profiles {
            if fields.profile.adapter != "sdk"
                || fields.profile.version != SDK_PROTOCOL
                || fields.profile.entrypoint != SDK_ENTRYPOINT
                || binding.events.iter().any(|(event, effects)| {
                    !effects.iter().any(|effect| effect == "observe")
                        || !SDK_EVENTS.iter().any(|(name, allowed)| {
                            *name == event
                                && effects
                                    .iter()
                                    .all(|effect| allowed.contains(&effect.as_str()))
                        })
                })
                || binding
                    .failure_actions
                    .keys()
                    .any(|event| !binding.events.contains_key(event))
            {
                return Err(Error::Invariant(
                    "invalid SDK instrumentation descriptor binding",
                ));
            }
        }
        Ok(Self(fields))
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

    /// Whether the selected target requests session-start observation.
    pub fn observe_session_start(&self) -> bool {
        self.0.observe_session_start
    }

    /// Optional builtin profile binding for SDK startup.
    pub fn instrumentation_profiles(&self) -> Option<&InstrumentationBinding> {
        self.0.instrumentation_profiles.as_ref()
    }

    /// Existing service availability contract selected during compilation.
    pub fn availability(&self) -> Availability {
        self.0.availability
    }
}
