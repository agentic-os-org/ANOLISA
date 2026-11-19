//! Nonsecret backend configuration with explicit transport and threshold policy.

use std::net::IpAddr;

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::model::{BackendIdentity, BackendKind, BackendMode};
use crate::protocol::{ContractError, MAX_MESSAGE_BYTES};

/// Configured support thresholds in millionths, not calibrated quality guarantees.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
    /// Minimum support for a decisive preservation answer.
    #[serde(default = "default_threshold")]
    pub preservation_ppm: u32,
    /// Minimum support for a decisive task-scope answer.
    #[serde(default = "default_threshold")]
    pub task_scope_ppm: u32,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            preservation_ppm: default_threshold(),
            task_scope_ppm: default_threshold(),
        }
    }
}

fn default_threshold() -> u32 {
    900_000
}

/// Plain HTTP is restricted to explicitly labeled, literal-loopback tests.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TransportMode {
    /// Certificate-verified HTTPS for production providers.
    #[default]
    Https,
    /// HTTP on a literal loopback address, never a hostname.
    LoopbackTest,
}

/// Provider-specific URLs and credential environment variable names.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendConfig {
    /// Bailian uses the complete endpoint without appending a path.
    Bailian {
        /// Exact configured inference URL.
        endpoint_url: String,
        /// Provider model name or alias.
        #[serde(default = "bailian_model")]
        model: String,
        /// Name only; validation never reads the environment.
        #[serde(default = "bailian_key_env")]
        key_env: String,
    },
    /// TypeSafe appends `/v1/systemone` to its base URL.
    Typesafe {
        /// API base URL, optionally including a gateway path.
        #[serde(default = "typesafe_base")]
        base_url: String,
        /// Provider model name or alias.
        #[serde(default = "typesafe_model")]
        model: String,
        /// Name only; validation never reads the environment.
        #[serde(default = "typesafe_key_env")]
        key_env: String,
    },
}

fn bailian_model() -> String {
    "decision-model-preview".into()
}
fn bailian_key_env() -> String {
    "DASHSCOPE_API_KEY".into()
}
fn typesafe_base() -> String {
    "https://api.typesafe.ai".into()
}
fn typesafe_model() -> String {
    "jev-latest".into()
}
fn typesafe_key_env() -> String {
    "TYPESAFE_API_KEY".into()
}

/// Closed, bounded model configuration containing no credential values.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    /// Consumer policy revision bound to the evaluation input.
    pub config_revision: String,
    /// Remote provider selection; fixtures are not selectable here.
    pub backend: BackendConfig,
    /// Maximum inference duration, which can only shorten the event deadline.
    pub timeout_ms: u64,
    /// Streaming response byte limit, at most the protocol limit.
    pub max_response_bytes: usize,
    /// Versioned units use independent support thresholds.
    #[serde(default)]
    pub thresholds: Thresholds,
    /// Explicit production or loopback-test transport.
    #[serde(default)]
    pub transport: TransportMode,
}

impl ModelConfig {
    /// Reject invalid limits, thresholds, URLs and environment variable names.
    pub fn validate(&self) -> Result<(), ContractError> {
        let (url, model, key_env) = match &self.backend {
            BackendConfig::Bailian {
                endpoint_url,
                model,
                key_env,
            } => (endpoint_url, model, key_env),
            BackendConfig::Typesafe {
                base_url,
                model,
                key_env,
            } => (base_url, model, key_env),
        };
        if !bounded_text(&self.config_revision, 128)
            || !bounded_text(model, 256)
            || !(1..=120_000).contains(&self.timeout_ms)
            || !(1..=MAX_MESSAGE_BYTES).contains(&self.max_response_bytes)
            || ![
                self.thresholds.preservation_ppm,
                self.thresholds.task_scope_ppm,
            ]
            .iter()
            .all(|n| (500_001..=1_000_000).contains(n))
            || !valid_env_name(key_env)
        {
            return Err(ContractError::InvalidInput);
        }
        validate_url(url, self.transport)?;
        Ok(())
    }

    /// Decode bounded JSON with duplicate-key rejection, then validate policy.
    pub fn decode(bytes: &[u8]) -> Result<Self, ContractError> {
        let config: Self = crate::wire::decode(bytes)?;
        config.validate()?;
        Ok(config)
    }

    /// Hash validated, nonsecret typed configuration with a versioned domain.
    pub fn digest(&self) -> Result<String, ContractError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| ContractError::InvalidInput)?;
        let mut hash = Sha256::new();
        hash.update(b"semantic-arbiter/v1alpha2\0model-config\0");
        hash.update(bytes);
        Ok(format!("sha256:{:x}", hash.finalize()))
    }

    /// Describe the configured provider without exposing its URL or credentials.
    pub fn identity(&self) -> BackendIdentity {
        let (kind, model) = match &self.backend {
            BackendConfig::Bailian { model, .. } => (BackendKind::Bailian, model),
            BackendConfig::Typesafe { model, .. } => (BackendKind::Typesafe, model),
        };
        BackendIdentity {
            kind,
            mode: match self.transport {
                TransportMode::Https => BackendMode::Remote,
                TransportMode::LoopbackTest => BackendMode::LoopbackTest,
            },
            model: model.clone(),
        }
    }

    pub(crate) fn endpoint(&self) -> String {
        match &self.backend {
            BackendConfig::Bailian { endpoint_url, .. } => endpoint_url.clone(),
            BackendConfig::Typesafe { base_url, .. } => {
                format!("{}/v1/systemone", base_url.trim_end_matches('/'))
            }
        }
    }

    pub(crate) fn key_env(&self) -> &str {
        match &self.backend {
            BackendConfig::Bailian { key_env, .. } | BackendConfig::Typesafe { key_env, .. } => {
                key_env
            }
        }
    }
}

fn bounded_text(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name.len() <= 128
        && bytes
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn validate_url(raw: &str, transport: TransportMode) -> Result<(), ContractError> {
    let invalid = ContractError::InvalidInput;
    if raw.len() > 2048
        || raw
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return Err(invalid);
    }
    let prefix = match transport {
        TransportMode::Https => "https://",
        TransportMode::LoopbackTest => "http://",
    };
    let authority = raw
        .strip_prefix(prefix)
        .ok_or(invalid)?
        .split(['/', '?', '#'])
        .next()
        .ok_or(invalid)?;
    let url = Url::parse(raw).map_err(|_| invalid)?;
    if authority.is_empty()
        || authority.contains('@')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.host_str().is_none()
    {
        return Err(invalid);
    }
    if transport == TransportMode::LoopbackTest {
        // Check the original spelling: URL parsers also accept integer/octal IPv4.
        let host = if let Some(bracketed) = authority.strip_prefix('[') {
            bracketed.split_once(']').ok_or(invalid)?.0
        } else {
            authority.split(':').next().ok_or(invalid)?
        };
        if !host.parse::<IpAddr>().map_err(|_| invalid)?.is_loopback() {
            return Err(invalid);
        }
    }
    Ok(())
}
