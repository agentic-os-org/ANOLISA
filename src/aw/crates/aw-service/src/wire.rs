//! Versioned local control messages; native identities are data, not authentication.

use aw_provider::admission::AdapterCapabilities;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, ffi::OsString};

mod environment;

/// Experimental local protocol, separate from the Provider stdio protocol.
pub const VERSION: &str = "aw-service/v1alpha7";

/// Bootstrap configuration and daemon lifetime to which mutable requests bind.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Fresh Linux UUID for each service start, preventing stale handle reuse.
    pub generation: String,
    /// SHA-256 of the exact bootstrap document; instance policy revisions are separate.
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
    /// Additional failure actions supported at the exact launch entrypoint.
    #[serde(default)]
    pub failure_actions: BTreeMap<String, Vec<String>>,
}

impl From<AdapterCapabilities> for Capabilities {
    fn from(value: AdapterCapabilities) -> Self {
        Self {
            adapter: value.adapter,
            version: value.version,
            entrypoint: value.entrypoint,
            events: value.events,
            failure_actions: value.failure_actions,
        }
    }
}

impl From<Capabilities> for AdapterCapabilities {
    fn from(value: Capabilities) -> Self {
        Self {
            adapter: value.adapter,
            version: value.version,
            entrypoint: value.entrypoint,
            events: value.events,
            failure_actions: value.failure_actions,
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
    /// Digest of the original event candidate; completion views never replace it.
    pub candidate_revision: String,
}

/// Same-user launcher report; a PID is descriptive data, never process authentication.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunOutcome {
    /// The foreground child was successfully spawned.
    Started {
        /// Child PID reported by its owning launcher.
        pid: u32,
    },
    /// The owning launcher reaped the child with exactly one exit representation.
    Exited {
        /// Ordinary exit status, or null for a signal.
        code: Option<i32>,
        /// Terminating signal, or null for ordinary exit.
        signal: Option<i32>,
    },
    /// Launch or native readiness failed; no successful completion is implied.
    Failed,
}

/// Native bridge lifecycle; only Adapters interpret framework event names.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    /// Stable bridge has registered and can accept requests.
    Ready,
    /// A whole Agent request is entering the shared admission gate.
    RequestStart,
    /// The native runtime confirms the whole request has finished.
    RequestEnd,
    /// Tool execution is entering, before its policy callbacks.
    ToolStart,
    /// Tool execution and its policy callbacks have terminated.
    ToolEnd,
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

impl Request {
    pub(crate) fn admit(&self, identity: &Identity) -> crate::Result<std::time::Instant> {
        if self.api_version != VERSION {
            return Err(crate::rejected("protocol_version"));
        }
        if self.identity.as_ref() != Some(identity)
            && !(self.identity.is_none() && matches!(self.operation, Operation::Status))
        {
            return Err(crate::rejected("stale_service"));
        }
        crate::deadline::decode(self.deadline_ns)
    }
}

/// No operation grants native permission or certifies effect adoption.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Discover identity and current bounded resource counts.
    Status,
    /// Read at most 20 completed step metadata entries from this service start.
    RecentExecutions,
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
    /// Tie a prepared root binding to its same-user launcher lifetime.
    OwnInstance {
        /// Prepared root instance; ownership is immutable after registration.
        instance_id: String,
        /// Launcher PID, corroborated with start time, UID and PID namespace.
        pid: u32,
    },
    /// Prepare an isolated task binding under an owned root's immutable policy.
    OpenScope {
        /// Owned root; nested task scopes and restricted reload roots are rejected.
        parent_id: String,
        /// Task worker PID: the root launcher itself or one of its descendants.
        pid: u32,
        /// Absolute task working directory.
        cwd: String,
        /// Complete explicit task environment, with no daemon inheritance.
        #[serde(with = "environment")]
        environment: BTreeMap<OsString, OsString>,
    },
    /// Read the immutable task policy before preparing and acknowledging native routes.
    ScopePolicy {
        /// Prepared task scope under an owned resident reload root.
        instance_id: String,
    },
    /// Confirm that the native owner received its task handle within five seconds.
    AcknowledgeScope {
        /// Prepared task binding; callbacks remain closed until acknowledgement.
        instance_id: String,
    },

    /// Enable producer-sequenced reclamation on a fresh owned binding.
    EnableInvocations {
        /// Owned binding whose adapter supplies stable producer identities.
        instance_id: String,
    },
    /// Reserve one invocation before forwarding it to native dispatch.
    RegisterInvocation {
        /// Binding generation is carried by the outer request identity.
        instance_id: String,
        /// Strictly increasing producer sequence, starting at one.
        sequence: u64,
        /// Stable native identity reused by every callback phase.
        call_id: String,
        /// Native tool name before dialect normalization.
        tool_name: String,
        /// Immutable tool input observed at the producer.
        input: Value,
    },
    /// Cancel and drain both phases before reclaiming their details.
    FinishInvocation {
        /// Binding which owns the invocation.
        instance_id: String,
        /// Sequence returned by the owning producer.
        sequence: u64,
        /// Stable identity paired with the sequence.
        call_id: String,
    },
    /// Opt an AW-launched instance into stable bridge policy updates before spawning.
    EnableReload {
        /// Prepared instance whose Adapter supports the stable bridge.
        instance_id: String,
    },
    /// Corroborate the foreground process independently of optional observation.
    AssociateProcess {
        /// Instance owned by the launcher.
        instance_id: String,
        /// Foreground child PID; start time, UID and namespace are read locally.
        pid: u32,
    },
    /// Record an Adapter-confirmed native lifecycle boundary.
    BridgeLifecycle {
        /// Owning reloadable instance.
        instance_id: String,
        /// Native process invoking the bridge, corroborated against the root process.
        pid: u32,
        /// Adapter-normalized lifecycle phase.
        phase: LifecyclePhase,
        /// Request or tool identity; ready uses an empty identity.
        key: String,
    },
    /// Read the active Adapter callback plan; never activates a pending update.
    HookPlan {
        /// Owning instance with a ready stable bridge.
        instance_id: String,
        /// Native event name.
        event: String,
    },
    /// Prepare a candidate and activate only at an idle native request boundary.
    UpdatePolicy {
        /// Existing instance, located independently of the candidate digest.
        instance_id: String,
        /// Compare-and-swap guard for the current instance policy.
        expected_revision: String,
        /// Complete candidate document, bounded by aw-config before preparation.
        config: String,
    },
    /// Start optional instance observation once, probing the configured source within bounds.
    StartObservation {
        /// Prepared instance owned by this launcher.
        instance_id: String,
    },
    /// Record launcher-reported process state without claiming source process association.
    ReportRun {
        /// Instance with an active observation owner.
        instance_id: String,
        /// Actual foreground process outcome or a launch failure.
        outcome: RunOutcome,
    },
    /// Accept a pure observation callback without admitting or invoking any action.
    ObserveHookEvent {
        /// Instance with optional observation enabled.
        instance_id: String,
        /// Adapter-normalized tool event.
        event: Value,
        /// Original native callback bytes used to corroborate the identity.
        native_input: Vec<u8>,
    },
    /// Release an idle binding; reloadable instances reject active native requests/tools.
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
    /// Repeated opens require identical input and reuse the first deadline. Hermes
    /// completion observers and final replacement normalize only their seam name
    /// and observer-only middleware trace; the first native bytes are retained.
    /// OpenClaw binds compression and native completion to one original snapshot;
    /// its pinned completion view supplies native metadata without renewing claims.
    /// Sequenced bindings require a producer registration and reclaim both phases
    /// after FinishInvocation drains them. Unknown or retired identities fail closed.
    /// Legacy bindings retain at most 1024 claims until unbind; exhaustion is explicit.
    OpenHookEvent {
        /// Prepared service instance.
        instance_id: String,
        /// Normalized event, with matching instance and native correlation IDs.
        event: Value,
        /// Exact callback stdin, limited independently to one MiB.
        native_input: Vec<u8>,
    },
    /// Cancel one native correlation, wait for cleanup and forbid later opens.
    ///
    /// Closing before an in-flight open wins must also prevent that open from
    /// creating an abandoned event. Repeated closes are idempotent while the
    /// invocation is retained; sequenced bindings reject closes after retirement.
    CloseHookEvent {
        /// Owning binding instance.
        instance_id: String,
        /// Native event name.
        event: String,
        /// Session used by the normalized callback.
        session_id: String,
        /// Invocation-scoped normalized tool call identifier.
        call_id: String,
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
    /// Freeze one bounded analysis context for an active event, without invoking steps.
    /// Only configured sources and verified native sessions can be queried. Repeated
    /// requests reuse the snapshot; failures, closure and expiry cannot renew its budget.
    FreezeContext {
        /// Active service event.
        event_id: String,
        /// Expected owning instance.
        instance_id: String,
        /// Exact revision returned by the original event handle.
        candidate_revision: String,
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
