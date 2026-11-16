//! Cooperative SDK control is consumed and confirmed by the application owner.

use super::{Availability, RegistrationHandle, RegistrationKey};
use serde::{Deserialize, Serialize};

/// Explicit discovery extension for application-mediated attach and detach.
pub const SDK_CONTROL_VERSION: &str = "aw-sdk-control/v1";

/// Requested change to an application's optional AW connection.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SdkIntent {
    /// Register installed callbacks and confirm Ready before admitting new tasks.
    Attach,
    /// Release an idle registration while keeping the native application alive.
    Detach,
}

/// Same-user control messages; only the exact SDK owner can announce or acknowledge.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum SdkControl {
    /// Publish a live application and retrieve its outstanding request.
    Poll {
        /// Stable application key, distinct from its replaceable registration key.
        key: RegistrationKey,
        /// Configured target; immutable for this application key.
        target: String,
        /// Application-selected mode; the controller cannot change it.
        availability: Availability,
        /// Current registration, if connected and Ready.
        handle: Option<RegistrationHandle>,
    },
    /// Stop publishing cooperative support without stopping the application.
    Withdraw {
        /// Exact application owner whose announcement is removed.
        key: RegistrationKey,
    },
    /// Discover live SDK applications without attaching or adopting them.
    List,
    /// Ask one discovered application to change its connection.
    Request {
        /// Exact application identity returned by List.
        application_id: String,
        /// Client-generated identifier retained across status queries.
        request_id: String,
        /// Requested lifecycle change.
        intent: SdkIntent,
        /// Application confirmation must arrive within this bounded interval.
        timeout_ms: u64,
    },
    /// Read the outcome after a delayed or lost response; never repeat the intent.
    Query {
        /// Selected live application.
        application_id: String,
        /// Original control request identifier.
        request_id: String,
    },
    /// Confirm the application completed its native readiness or idle release.
    Acknowledge {
        /// Authenticated application key.
        key: RegistrationKey,
        /// Original request identifier.
        request_id: String,
        /// Ready registration for attach; absent after detach or an error.
        handle: Option<RegistrationHandle>,
        /// Fixed failure code; no native payload or environment is exposed.
        error: Option<String>,
    },
}
