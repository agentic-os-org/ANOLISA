//! Strongly typed identities allocated by COSH.

use std::{fmt, str::FromStr};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;
use uuid::Uuid;

/// Failure returned when an internal identifier is not canonical for its type.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdError {
    /// The expected type prefix is absent or belongs to another ID type.
    #[error("identifier prefix must be `{expected}`")]
    WrongPrefix {
        /// Prefix required by the requested ID type.
        expected: &'static str,
    },
    /// The identifier body is not a canonical lowercase hyphenated UUID.
    #[error("identifier body must be a canonical lowercase hyphenated UUID")]
    InvalidUuid,
}

macro_rules! define_id {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Prefix used in the stable text representation.
            pub const PREFIX: &'static str = $prefix;

            /// Allocates a new identifier using the workspace UUID generator.
            #[must_use]
            pub fn new() -> Self {
                Self(format!("{}_{}", Self::PREFIX, Uuid::new_v4().hyphenated()))
            }

            /// Parses and validates a canonical identifier of this exact type.
            pub fn parse(value: impl AsRef<str>) -> Result<Self, IdError> {
                let value = value.as_ref();
                let expected_prefix = format!("{}_", Self::PREFIX);
                let body = value
                    .strip_prefix(&expected_prefix)
                    .ok_or(IdError::WrongPrefix {
                        expected: Self::PREFIX,
                    })?;
                let uuid = Uuid::parse_str(body).map_err(|_| IdError::InvalidUuid)?;
                if uuid.hyphenated().to_string() != body {
                    return Err(IdError::InvalidUuid);
                }
                Ok(Self(value.to_owned()))
            }

            /// Returns the canonical prefixed representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(de::Error::custom)
            }
        }
    };
}

define_id!(
    InstallationId,
    "ins",
    "Identifies one durable COSH Gateway installation."
);
define_id!(ActorId, "act", "Identifies an authenticated COSH actor.");
define_id!(TaskId, "tsk", "Identifies one durable user intent.");
define_id!(RunId, "run", "Identifies one attempt to execute a task.");
define_id!(
    TurnId,
    "trn",
    "Identifies one prompt turn within an Agent session."
);
define_id!(
    AgentSessionId,
    "ags",
    "Identifies one COSH-owned logical Agent session."
);
define_id!(
    ShellSessionId,
    "shs",
    "Identifies one COSH-owned Shell session."
);
define_id!(
    RuntimeInstanceId,
    "rti",
    "Identifies one supervised runtime process instance."
);
define_id!(
    RuntimeBindingId,
    "rtb",
    "Identifies a fenced binding between a run and runtime session."
);
define_id!(
    ApprovalId,
    "apr",
    "Identifies one durable approval request."
);
define_id!(PermitId, "prm", "Identifies one capability permit.");
define_id!(
    ExecutionId,
    "exe",
    "Identifies one attempted governed side effect."
);
define_id!(
    CheckpointId,
    "ckp",
    "Identifies one workspace checkpoint allocated by the capability broker."
);
define_id!(DeliveryId, "dlv", "Identifies one presentation delivery.");
define_id!(
    MessageId,
    "msg",
    "Identifies one COSH command or event envelope."
);
define_id!(RequestId, "req", "Identifies one COSH capability request.");
define_id!(
    InputRequestId,
    "inp",
    "Identifies one durable-eligible Runtime input request."
);
define_id!(ToolUseId, "tol", "Identifies one observed Agent tool call.");
define_id!(
    RuntimeMessageId,
    "rms",
    "Identifies one logical message emitted by an Agent runtime."
);

#[cfg(test)]
mod tests {
    use super::{CheckpointId, ExecutionId, RunId, TurnId};

    #[test]
    fn turn_ids_are_canonical_and_distinct_from_runs() {
        let turn_id = TurnId::new();

        assert!(turn_id.as_str().starts_with("trn_"));
        assert_eq!(
            TurnId::parse(turn_id.as_str()).expect("a generated turn ID is canonical"),
            turn_id
        );
        assert!(RunId::parse(turn_id.as_str()).is_err());
    }

    #[test]
    fn checkpoint_ids_are_canonical_and_distinct_from_executions() {
        let checkpoint_id = CheckpointId::new();

        assert!(checkpoint_id.as_str().starts_with("ckp_"));
        assert_eq!(
            CheckpointId::parse(checkpoint_id.as_str())
                .expect("a generated checkpoint ID is canonical"),
            checkpoint_id
        );
        assert!(ExecutionId::parse(checkpoint_id.as_str()).is_err());
    }
}
