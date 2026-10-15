//! Explicit read-only AgentSight queries, independent of Hook execution and Journal writes.
//! Source session names never establish a trusted AW binding or observation coverage.

mod transport;

use reqwest::Url;
use serde::Serialize;
use serde_json::Value;
use std::{
    path::Path,
    time::{Duration, Instant},
};
use transport::Transport;

/// Maximum combined response bytes for one query, including compatibility probes.
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Maximum steps across the selected trajectory and embedded subagent trajectories.
pub const MAX_STEPS: usize = 1000;

/// Query limits shared by all HTTP requests; never extend an existing Hook deadline.
#[derive(Clone, Copy)]
pub struct Limits {
    /// Total elapsed query budget, between one millisecond and sixty seconds.
    pub timeout: Duration,
    /// Combined response-body budget, between one byte and [`MAX_BYTES`].
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            bytes: 1024 * 1024,
        }
    }
}

/// Sanitized failures never include credentials, response bodies or source diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Caller supplied an invalid explicit setting.
    #[error("AgentSight query configuration: {0}")]
    Configuration(&'static str),
    /// Credential file is inaccessible, unsafe, or contains an invalid token.
    #[error("AgentSight credential unavailable or invalid")]
    Credential,
    /// Source authentication is disabled or the credential was rejected.
    #[error("AgentSight authentication required or rejected")]
    Authentication,
    /// Source cannot be reached or the response stream failed.
    #[error("AgentSight query unavailable")]
    Unavailable,
    /// The original query deadline expired.
    #[error("AgentSight query deadline exceeded")]
    Deadline,
    /// Response bodies or trajectory steps exceed the admitted limit.
    #[error("AgentSight query exceeds response limits")]
    Limit,
    /// A redirect would move the credential or widen the explicit query target.
    #[error("AgentSight redirects are not accepted")]
    Redirect,
    /// An unexpected HTTP status was returned without exposing its body.
    #[error("AgentSight query HTTP status {0}")]
    Http(u16),
    /// The source advertises an unsupported ATIF contract version.
    #[error("AgentSight query version unsupported")]
    UnsupportedVersion,
    /// The successful response does not satisfy the selected query contract.
    #[error("AgentSight query response invalid")]
    InvalidResponse,
}

/// Successful diagnostic projection; this is not an `aw.evidence/v1` assertion.
#[derive(Serialize)]
pub struct QueryResult {
    /// Independently versioned diagnostic output.
    pub schema_version: &'static str,
    /// Explicit source endpoint and observed compatibility information.
    pub source: Source,
    /// Caller-selected source session, with no implied AW session association.
    pub session_id: String,
    /// Always unassociated until a trusted Adapter establishes the AW mapping.
    pub association: &'static str,
    /// Always unknown: historical content cannot establish current probe coverage.
    pub coverage: &'static str,
    /// Distinguishes returned content from an ambiguous missing record/store.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Source claims and endpoint selection, not a verified AW runtime identity.
#[derive(Serialize)]
pub struct Source {
    /// Normalized explicit origin, without credentials or query parameters.
    pub endpoint: String,
    /// Product version is unknown: the common query API does not expose it.
    pub version: Option<String>,
    /// `loopback_unauthenticated` or `bearer_supplied`; neither verifies AW identity.
    pub authentication: &'static str,
}

/// Source content remains separate from canonical AW metadata and local audit.
#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    /// Valid selected trajectory, which can legitimately contain zero steps.
    Available {
        /// Total steps, including embedded subagent documents.
        step_count: usize,
        /// Bounded source JSON; no semantic judgment or persistence is implied.
        trajectory: Value,
    },
    /// Existing API cannot prove whether the record or its store is absent.
    Unknown {
        /// Stable non-sensitive diagnostic category.
        reason_code: &'static str,
    },
}

/// Reusable read-only connector owned by the AW composition boundary.
/// No discovery, automatic activation, retries, database access or writes occur.
pub struct Client {
    endpoint: Url,
    transport: Transport,
}

impl Client {
    /// Open an explicit endpoint, using private credentials only for HTTPS.
    /// HTTPS verifies certificates/hostnames. Literal-loopback HTTP ignores `token_file`
    /// and sends no credential; the local listener's identity remains unverified.
    ///
    /// # Errors
    /// Rejects unsafe endpoint forms, credentials and HTTP client initialization failure.
    pub fn connect(endpoint: &str, token_file: &Path) -> Result<Self, Error> {
        let endpoint = transport::endpoint(endpoint)?;
        let transport = Transport::new(&endpoint, token_file)?;
        Ok(Self {
            endpoint,
            transport,
        })
    }

    /// Read one collected trajectory through the reviewed auth-status and ATIF-v1.7 APIs.
    /// HTTP and HTTPS use the same contract checks and shared deadline/byte budget.
    /// Missing store/record is unknown, never successful evidence of no activity.
    ///
    /// # Errors
    /// Returns sanitized admission, authentication, transport, limit or protocol failures.
    pub fn trajectory(&self, session_id: &str, limits: Limits) -> Result<QueryResult, Error> {
        if session_id.is_empty()
            || session_id.len() > 512
            || session_id.chars().any(char::is_control)
            || matches!(session_id, "." | "..")
        {
            return Err(Error::Configuration("invalid session ID"));
        }
        if limits.timeout < Duration::from_millis(1)
            || limits.timeout > Duration::from_secs(60)
            || limits.bytes == 0
            || limits.bytes > MAX_BYTES
        {
            return Err(Error::Configuration("invalid query limits"));
        }
        let deadline = Instant::now() + limits.timeout;
        let mut remaining = limits.bytes;
        let get = |segments: &[&str], remaining: &mut usize| {
            let mut url = self.endpoint.clone();
            url.path_segments_mut()
                .map_err(|_| Error::InvalidResponse)?
                .clear()
                .extend(segments);
            self.transport.get(url, deadline, remaining)
        };
        let (_, auth) = get(&["api", "auth", "status"], &mut remaining)?;
        match auth.get("auth_enabled").and_then(Value::as_bool) {
            Some(true) => {}
            Some(false) => return Err(Error::Authentication),
            None => return Err(Error::InvalidResponse),
        }
        let (status, trajectory) = get(&["api", "trajectories", session_id], &mut remaining)?;
        let outcome = if status == 404 {
            Outcome::Unknown {
                reason_code: "record_or_store_unavailable",
            }
        } else {
            if trajectory["session_id"] != session_id {
                return Err(Error::InvalidResponse);
            }
            Outcome::Available {
                step_count: validate(&trajectory)?,
                trajectory,
            }
        };
        if Instant::now() >= deadline {
            return Err(Error::Deadline);
        }
        Ok(QueryResult {
            schema_version: "aw.agentsight.query/v1alpha1",
            source: Source {
                endpoint: self.endpoint.to_string(),
                version: None,
                authentication: self.transport.authentication(),
            },
            session_id: session_id.into(),
            association: "unassociated",
            coverage: "unknown",
            outcome,
        })
    }
}

fn validate(root: &Value) -> Result<usize, Error> {
    let mut pending = vec![root];
    let mut count = 0;
    while let Some(doc) = pending.pop() {
        if doc["schema_version"] != "ATIF-v1.7" {
            return Err(Error::UnsupportedVersion);
        }
        if doc["agent"]["name"].as_str().is_none_or(str::is_empty) {
            return Err(Error::InvalidResponse);
        }
        let steps = doc["steps"].as_array().ok_or(Error::InvalidResponse)?;
        count += steps.len();
        if count > MAX_STEPS {
            return Err(Error::Limit);
        }
        for (index, step) in steps.iter().enumerate() {
            if step["step_id"].as_u64() != Some(index as u64 + 1) || !step.is_object() {
                return Err(Error::InvalidResponse);
            }
        }
        if let Some(children) = doc.get("subagent_trajectories") {
            pending.extend(children.as_array().ok_or(Error::InvalidResponse)?);
        }
    }
    Ok(count)
}
