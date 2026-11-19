//! Independent Noul probabilities and deadline-bound model backends.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::time::Instant;

pub mod http;
mod probability;

pub use probability::NoulProbability;

/// Version of exact-decimal, independent yes/no normalization.
pub const NORMALIZATION_VERSION: &str = "noul-floor-ppm/v2";

/// Actual provider implementation, including explicitly injected fixtures.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendKind {
    /// Bailian decision endpoint.
    Bailian,
    /// TypeSafe System One endpoint.
    Typesafe,
    /// Local deterministic test backend.
    Fixture,
}

/// Transport provenance retained independently of model answers.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum BackendMode {
    /// Certificate-verified HTTPS provider.
    Remote,
    /// Explicit literal-loopback HTTP test service.
    LoopbackTest,
    /// Injected in-process fixture, never remote inference.
    Fixture,
}

/// Nonsecret identity of the backend that was actually constructed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BackendIdentity {
    /// Concrete implementation.
    pub kind: BackendKind,
    /// Actual transport or fixture mode.
    pub mode: BackendMode,
    /// Configured model name or alias, not a provider response claim.
    pub model: String,
}

/// Closed yes/no question shape accepted by both adapters.
#[derive(Clone, Debug, Serialize)]
pub struct NoulQuestion {
    #[serde(rename = "type")]
    kind: &'static str,
    instructions: String,
}

impl NoulQuestion {
    /// Construct a Noul question without allowing a different answer type.
    pub fn new(instructions: String) -> Self {
        Self {
            kind: "noul",
            instructions,
        }
    }
}

/// Frozen model projection; answers cannot supply actions or evidence references.
#[derive(Clone, Debug)]
pub struct ModelRequest {
    /// Explicitly selected, provenance-labeled state.
    pub state: serde_json::Value,
    /// Independently evaluated questions keyed by their registered identifiers.
    pub questions: BTreeMap<String, NoulQuestion>,
}

/// Validated probabilities only; provider metadata grants no authority.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelReply {
    /// Exactly one answer per requested question.
    pub answers: BTreeMap<String, NoulProbability>,
}

/// Stable sanitized errors without URLs, credentials or server error bodies.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ModelError {
    /// The single absolute inference deadline expired.
    #[error("timeout")]
    Timeout,
    /// Provider rejected authentication or authorization.
    #[error("authentication")]
    Authentication,
    /// Provider rejected the request for rate limiting.
    #[error("rate_limited")]
    RateLimited,
    /// Connection, TLS or streaming transport failed.
    #[error("transport")]
    Transport,
    /// Provider returned another non-success status.
    #[error("http")]
    Http,
    /// Request/configuration or provider response violated the model contract.
    #[error("invalid_response")]
    InvalidResponse,
    /// Response exceeded the configured streaming byte limit.
    #[error("response_too_large")]
    ResponseTooLarge,
    /// No usable credential was supplied.
    #[error("missing_credentials")]
    MissingCredentials,
}

/// A single inference attempt sharing the caller's original absolute deadline.
pub trait ModelBackend: Send + Sync {
    /// Describe the implementation actually performing inference.
    fn identity(&self) -> BackendIdentity;

    /// Infer without retries or fallback, never extending the supplied deadline.
    fn infer<'a>(
        &'a self,
        request: &'a ModelRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<ModelReply, ModelError>> + Send + 'a>>;
}

/// Explicit in-process test injection, unavailable through backend configuration.
#[derive(Clone, Debug)]
pub struct FixtureBackend {
    reply: Result<ModelReply, ModelError>,
}

impl FixtureBackend {
    /// Retain a deterministic reply or failure with fixed fixture identity.
    pub fn new(reply: Result<ModelReply, ModelError>) -> Self {
        Self { reply }
    }
}

impl ModelBackend for FixtureBackend {
    fn identity(&self) -> BackendIdentity {
        BackendIdentity {
            kind: BackendKind::Fixture,
            mode: BackendMode::Fixture,
            model: "fixture".into(),
        }
    }

    fn infer<'a>(
        &'a self,
        _request: &'a ModelRequest,
        deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<ModelReply, ModelError>> + Send + 'a>> {
        Box::pin(async move {
            check_deadline(deadline)?;
            let reply = self.reply.clone();
            check_deadline(deadline)?;
            reply
        })
    }
}

pub(crate) fn check_deadline(deadline: Instant) -> Result<(), ModelError> {
    if Instant::now() >= deadline {
        Err(ModelError::Timeout)
    } else {
        Ok(())
    }
}
