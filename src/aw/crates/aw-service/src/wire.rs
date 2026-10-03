//! Versioned local control messages; native identities are data, not authentication.

use aw_provider::admission::AdapterCapabilities;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsString};

mod environment;

/// Experimental local protocol, separate from the Provider stdio protocol.
pub const VERSION: &str = "aw-service/v1alpha1";

/// Configuration snapshot and daemon lifetime to which every mutable request binds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Fresh Linux UUID for each service start, preventing stale handle reuse.
    pub generation: String,
    /// SHA-256 of the exact configuration document.
    pub config_revision: String,
}

/// Trusted Adapter evidence supplied by the same-user launcher, never a Provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Configured Adapter name.
    pub adapter: String,
    /// Validated native version; supplying it does not prove hook installation.
    pub version: String,
    /// Validated native interaction entrypoint.
    pub entrypoint: String,
    /// Supported effects by event.
    pub events: BTreeMap<String, Vec<String>>,
}

impl From<Capabilities> for AdapterCapabilities {
    fn from(value: Capabilities) -> Self {
        Self {
            adapter: value.adapter,
            version: value.version,
            entrypoint: value.entrypoint,
            events: value.events,
        }
    }
}

/// An admitted process context; lifetime is explicit and bounded by service capacity.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Service identity at preparation.
    pub identity: Identity,
    /// Service-issued instance identifier, also required in normalized event data.
    pub instance_id: String,
    /// Named target in the retained AW configuration.
    pub target: String,
    /// Durable preparation record key.
    pub audit_key: String,
}

/// One native event's immutable input, shared budget and once-only step claims.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventHandle {
    /// Unique service event identifier and durable audit key.
    pub event_id: String,
    /// Owning binding instance.
    pub instance_id: String,
    /// Admitted step identifiers; their order does not imply scheduling.
    pub steps: Vec<String>,
}

/// One bounded local RPC. The client never retries it automatically.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Must equal [`VERSION`].
    pub api_version: String,
    /// Required except for the initial status discovery.
    pub identity: Option<Identity>,
    /// Absolute Linux CLOCK_MONOTONIC nanoseconds, capped at 60 seconds from receipt.
    pub deadline_ns: u64,
    /// Requested operation.
    pub operation: Operation,
}

/// No operation grants native permission or certifies effect adoption.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Discover identity and current bounded resource counts.
    Status,
    /// Pin context and perform real Provider discovery/private configuration checks.
    Bind {
        /// Named target.
        target: String,
        /// Caller-verified native capabilities.
        capabilities: Capabilities,
        /// Absolute working directory for Provider processes.
        cwd: String,
        /// Complete explicit environment; no implicit daemon inheritance.
        #[serde(with = "environment")]
        environment: BTreeMap<OsString, OsString>,
    },
    /// Release a binding; active events must be closed first.
    Unbind {
        /// Service-issued identifier.
        instance_id: String,
    },
    /// Cancel this instance's events, wait for cleanup, then release its binding.
    ///
    /// A timed-out release keeps the instance draining and forbids new events;
    /// another release may finish cleanup without reopening the instance.
    ReleaseInstance {
        /// Service-issued identifier.
        instance_id: String,
    },
    /// Establish one shared event lifetime, without executing any step.
    OpenEvent {
        /// Prepared service instance.
        instance_id: String,
        /// Normalized Provider event, including the matching instance ID.
        event: Value,
    },
    /// Lease one native callback shared by all step processes for the same tool call.
    ///
    /// Nonempty session/tool call IDs identify the event within this instance.
    /// Repeated opens require identical input and reuse the first deadline. Closed
    /// correlations remain claimed until unbind; they cannot start a new budget.
    /// At most 1024 correlations are retained per instance; exhaustion is explicit.
    OpenHookEvent {
        /// Prepared service instance.
        instance_id: String,
        /// Normalized event, with matching instance and native correlation IDs.
        event: Value,
        /// Exact callback stdin, limited independently to one MiB.
        native_input: Vec<u8>,
    },
    /// Execute one step; concurrent RPCs preserve caller scheduling.
    InvokeStep {
        /// Open event identifier.
        event_id: String,
        /// Expected instance; prevents accidental cross-binding calls.
        instance_id: String,
        /// Admitted step; may be attempted once in this event.
        step_id: String,
        /// Complete native callback environment; only native-hook steps use it.
        /// Missing values preserve the bound context. Not event data or audit payload.
        #[serde(default, with = "environment::optional")]
        native_environment: Option<BTreeMap<OsString, OsString>>,
    },
    /// Cancel unfinished work, join children and release event resources.
    CloseEvent {
        /// Event to close. Expiry also closes abandoned events.
        event_id: String,
        /// Expected instance.
        instance_id: String,
    },
    /// Read one bounded, verified durable record chain, including previous starts.
    Audit {
        /// Preparation or event audit key.
        key: String,
    },
    /// Request cancellation and graceful shutdown; process exit completes cleanup.
    Stop,
}

impl Operation {
    /// Parse untrusted operation JSON before normalization can discard invalid data.
    ///
    /// # Errors
    /// Rejects messages over 8 MiB, overflowing integer tokens, duplicate fields,
    /// excessive nesting, trailing data and invalid operation fields. Native
    /// floating-point values and Unicode remain supported.
    pub fn from_json(bytes: &[u8]) -> crate::Result<Self> {
        Ok(crate::ipc::decode(bytes)?)
    }
}

/// Exactly one result or error code; native results explicitly carry callback bytes.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    /// Protocol version.
    pub api_version: String,
    /// Service identity that handled the request.
    pub identity: Identity,
    /// Successful operation result; effects are candidates only.
    pub result: Option<Value>,
    /// Stable rejection code on failure.
    pub error: Option<String>,
    /// Evidence for a failed preparation when no successful binding was returned.
    pub audit_key: Option<String>,
}
