//! Application-owned registration, with generation-bound recovery and native acknowledgement.

use super::{environment, Binding, Capabilities};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsString};

/// Stable JSON lifecycle envelope exposed by `aw instance`.
pub const REGISTRATION_VERSION: &str = "aw-instance/v1";

/// Caller-selected live process, corroborated against the Unix socket peer.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RegistrationOwner {
    /// Application process, or the direct parent of its CLI transport.
    pub pid: u32,
    /// Linux /proc/PID/stat start time (field 22), protecting against PID reuse.
    pub start_ticks: String,
}

/// Recovery key retained for this owner and daemon generation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationKey {
    /// Client-generated, nonempty identifier of at most 128 ASCII characters.
    pub request_id: String,
    /// Exact application process identity.
    pub owner: RegistrationOwner,
}

/// Instance handle; every field is checked against the retained registration.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationHandle {
    /// Original recovery key.
    pub key: RegistrationKey,
    /// Prepared target and daemon identity.
    pub binding: Binding,
}

/// Independent lifecycle commands. Ready acknowledges installed native routes;
/// registration alone establishes no native interception or effect adoption.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum RegistrationCommand {
    /// Prepare an application-owned root without starting or adopting its process.
    Register {
        /// Recovery key; repeating identical input returns the retained state.
        key: RegistrationKey,
        /// Existing configured target.
        target: String,
        /// Adapter-verified capabilities at the application's native entrypoint.
        capabilities: Capabilities,
        /// Absolute Provider working directory supplied by the application.
        cwd: String,
        /// Complete explicit Provider environment; never inherited from the daemon.
        #[serde(with = "environment")]
        environment: BTreeMap<OsString, OsString>,
    },
    /// Recover the state after a missing response without repeating native work.
    Query {
        /// Original recovery key.
        key: RegistrationKey,
    },
    /// Confirm native routes before opening the first task scope.
    Ready {
        /// Exact prepared handle returned by registration or query.
        handle: RegistrationHandle,
        /// Immutable revision whose returned routes the Adapter installed.
        policy_revision: String,
    },
    /// Cancel and drain only this instance's scopes and Provider resources.
    Release {
        /// Exact prepared handle; repeated release returns the retained state.
        handle: RegistrationHandle,
    },
}

/// Explicit service selection plus an independent lifecycle action.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrationRequest {
    /// Must equal [`REGISTRATION_VERSION`].
    pub api_version: String,
    /// Identity discovered before registration; retained across retries and queries.
    pub identity: super::Identity,
    /// Action and its typed input.
    pub command: RegistrationCommand,
}

impl RegistrationRequest {
    /// Decode bounded JSON without discarding duplicate fields or invalid integers.
    ///
    /// # Errors
    /// Rejects malformed, ambiguous, oversized or unknown input fields.
    pub fn from_json(bytes: &[u8]) -> crate::Result<Self> {
        let request: Self = crate::ipc::decode(bytes)?;
        if request.api_version != REGISTRATION_VERSION {
            return Err(crate::rejected("registration_protocol_version"));
        }
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn registration_input_rejects_ambiguous_fields_and_unknown_versions() {
        let input = json!({"api_version":REGISTRATION_VERSION,
            "identity":{"generation":"generation","config_revision":"revision"},
            "command":{"action":"query","key":{"request_id":"one","owner":{"pid":123,"start_ticks":"456"}}}});
        let encoded = serde_json::to_vec(&input).unwrap();
        assert!(RegistrationRequest::from_json(&encoded).is_ok());
        let mut wrong_version = input.clone();
        wrong_version["api_version"] = json!("aw-instance/v0");
        assert!(
            RegistrationRequest::from_json(&serde_json::to_vec(&wrong_version).unwrap()).is_err()
        );
        let mut unknown = input;
        unknown["command"]["unknown"] = json!(true);
        assert!(RegistrationRequest::from_json(&serde_json::to_vec(&unknown).unwrap()).is_err());
        let duplicate = String::from_utf8(encoded)
            .unwrap()
            .replace("\"pid\":123", "\"pid\":123,\"pid\":456");
        assert!(RegistrationRequest::from_json(duplicate.as_bytes()).is_err());
    }
}
