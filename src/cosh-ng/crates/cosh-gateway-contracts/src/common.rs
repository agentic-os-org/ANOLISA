//! Shared bounded values, headers, actors, and runtime context.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{
    external::ExternalRef,
    ids::{
        ActorId, AgentSessionId, ApprovalId, ExecutionId, InstallationId, MessageId, PermitId,
        RunId, RuntimeBindingId, RuntimeInstanceId, TaskId,
    },
};

/// Maximum UTF-8 byte length of user-facing contract text.
pub const MAX_TEXT_BYTES: usize = 4096;
/// Maximum UTF-8 byte length of names used for authorities and operations.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum UTF-8 byte length of opaque external values.
pub const MAX_OPAQUE_BYTES: usize = 1024;
/// Maximum UTF-8 byte length of an idempotency key.
pub const MAX_IDEMPOTENCY_KEY_BYTES: usize = 256;
/// Current Gateway command schema version.
pub const CONTRACT_SCHEMA_VERSION: u16 = 1;
/// Durable Task event payload schema version.
///
/// Keep this independent from ingress and Runtime wire versions. A bump
/// requires a SQLite schema migration that rewrites every persisted Task event
/// and projection before current readers can open the database.
pub const TASK_EVENT_SCHEMA_VERSION: u16 = 1;
/// Current Runtime command and event schema version.
pub const RUNTIME_CONTRACT_SCHEMA_VERSION: u16 = 4;

/// Failure returned when a bounded string violates its construction contract.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BoundedStringError {
    /// Empty values do not carry usable contract meaning.
    #[error("value must not be empty")]
    Empty,
    /// The UTF-8 representation exceeds the type-specific byte limit.
    #[error("value exceeds the {max_bytes}-byte limit")]
    TooLong {
        /// Maximum accepted UTF-8 byte count.
        max_bytes: usize,
    },
    /// NUL bytes are forbidden at transport and operating-system boundaries.
    #[error("value must not contain a NUL character")]
    ContainsNul,
}

fn validate_bounded(value: &str, max_bytes: usize) -> Result<(), BoundedStringError> {
    if value.is_empty() {
        return Err(BoundedStringError::Empty);
    }
    if value.len() > max_bytes {
        return Err(BoundedStringError::TooLong { max_bytes });
    }
    if value.contains('\0') {
        return Err(BoundedStringError::ContainsNul);
    }
    Ok(())
}

macro_rules! bounded_string {
    ($name:ident, $max:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Constructs a validated bounded value.
            pub fn new(value: impl Into<String>) -> Result<Self, BoundedStringError> {
                let value = value.into();
                validate_bounded(&value, $max)?;
                Ok(Self(value))
            }

            /// Returns the validated text value.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
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
                Self::new(value).map_err(de::Error::custom)
            }
        }
    };
}

bounded_string!(
    BoundedText,
    MAX_TEXT_BYTES,
    "User-facing text whose serialized size is bounded."
);
bounded_string!(
    BoundedName,
    MAX_NAME_BYTES,
    "A bounded authority, operation, runtime, or profile name."
);
bounded_string!(
    BoundedOpaque,
    MAX_OPAQUE_BYTES,
    "An opaque external value with a strict serialized-size limit."
);
bounded_string!(
    IdempotencyKey,
    MAX_IDEMPOTENCY_KEY_BYTES,
    "A caller-scoped key used to replay command admission safely."
);

/// Error returned when a digest is not canonical lowercase SHA-256 text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("digest must contain exactly 64 lowercase hexadecimal characters")]
pub struct DigestError;

/// Canonical lowercase hexadecimal representation of a SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(String);

impl Digest {
    /// Parses a lowercase 64-character SHA-256 digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, DigestError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(DigestError);
        }
        Ok(Self(value))
    }

    /// Returns the canonical hexadecimal representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// Stable schema discriminator for a contract envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContractSchema {
    /// Gateway ingress command schema.
    #[serde(rename = "cosh.gateway.command")]
    GatewayCommand,
    /// Durable Task lifecycle event schema.
    #[serde(rename = "cosh.task.event")]
    TaskEvent,
    /// Neutral Agent Runtime command schema.
    #[serde(rename = "cosh.runtime.command")]
    RuntimeCommand,
    /// Neutral Agent Runtime event schema.
    #[serde(rename = "cosh.runtime.event")]
    RuntimeEvent,
}

/// Failure returned when an envelope declares an unsupported schema version.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unsupported contract schema version {actual}; expected {expected}")]
pub struct SchemaVersionError {
    /// Version accepted by this crate.
    pub expected: u16,
    /// Version declared by the envelope.
    pub actual: u16,
}

/// Failure returned when an envelope carries another contract schema.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("envelope schema {actual:?} does not match expected schema {expected:?}")]
pub struct EnvelopeSchemaError {
    /// Schema required by the envelope type.
    pub expected: ContractSchema,
    /// Schema declared in the decoded header.
    pub actual: ContractSchema,
}

/// Metadata common to every Gateway domain envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ContractHeader {
    /// Domain schema carried by the envelope.
    pub schema: ContractSchema,
    /// Version of the domain schema, independent from ACP and Core versions.
    pub schema_version: u16,
    /// Unique identity of this command or event.
    pub message_id: MessageId,
    /// Milliseconds since the Unix epoch recorded by the producer.
    pub occurred_at_ms: u64,
    /// Lifecycle identities propagated with the message.
    pub correlation: Correlation,
}

impl ContractHeader {
    /// Creates a header at the current supported domain schema version.
    #[must_use]
    pub fn new(
        schema: ContractSchema,
        message_id: MessageId,
        occurred_at_ms: u64,
        correlation: Correlation,
    ) -> Self {
        Self {
            schema,
            schema_version: expected_schema_version(schema),
            message_id,
            occurred_at_ms,
            correlation,
        }
    }

    /// Rejects versions that this crate cannot interpret safely.
    pub fn validate_version(&self) -> Result<(), SchemaVersionError> {
        let expected = expected_schema_version(self.schema);
        if self.schema_version == expected {
            Ok(())
        } else {
            Err(SchemaVersionError {
                expected,
                actual: self.schema_version,
            })
        }
    }

    /// Rejects a header used with a different envelope type.
    pub fn validate_schema(&self, expected: ContractSchema) -> Result<(), EnvelopeSchemaError> {
        if self.schema == expected {
            Ok(())
        } else {
            Err(EnvelopeSchemaError {
                expected,
                actual: self.schema,
            })
        }
    }
}

const fn expected_schema_version(schema: ContractSchema) -> u16 {
    match schema {
        ContractSchema::RuntimeCommand | ContractSchema::RuntimeEvent => {
            RUNTIME_CONTRACT_SCHEMA_VERSION
        }
        ContractSchema::GatewayCommand => CONTRACT_SCHEMA_VERSION,
        ContractSchema::TaskEvent => TASK_EVENT_SCHEMA_VERSION,
    }
}

impl<'de> Deserialize<'de> for ContractHeader {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireHeader {
            schema: ContractSchema,
            schema_version: u16,
            message_id: MessageId,
            occurred_at_ms: u64,
            correlation: Correlation,
        }

        let wire = WireHeader::deserialize(deserializer)?;
        let header = Self {
            schema: wire.schema,
            schema_version: wire.schema_version,
            message_id: wire.message_id,
            occurred_at_ms: wire.occurred_at_ms,
            correlation: wire.correlation,
        };
        header.validate_version().map_err(de::Error::custom)?;
        Ok(header)
    }
}

/// Internal identities propagated across ingress, Task, Runtime, and execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Correlation {
    /// Gateway installation that allocated the identities.
    pub installation_id: InstallationId,
    /// Authenticated actor, when resolution has completed.
    pub actor_id: Option<ActorId>,
    /// Durable Task owning the lifecycle.
    pub task_id: Option<TaskId>,
    /// Current Task execution attempt.
    pub run_id: Option<RunId>,
    /// COSH-owned logical Agent session.
    pub agent_session_id: Option<AgentSessionId>,
    /// Fenced Runtime binding producing the message.
    pub runtime_binding_id: Option<RuntimeBindingId>,
    /// Approval relevant to this message.
    pub approval_id: Option<ApprovalId>,
    /// Permit relevant to this message.
    pub permit_id: Option<PermitId>,
    /// Governed execution relevant to this message.
    pub execution_id: Option<ExecutionId>,
    /// Direct accepted message that caused this message.
    pub causation_message_id: Option<MessageId>,
}

impl Correlation {
    /// Starts an empty lifecycle correlation for one installation.
    #[must_use]
    pub fn new(installation_id: InstallationId) -> Self {
        Self {
            installation_id,
            actor_id: None,
            task_id: None,
            run_id: None,
            agent_session_id: None,
            runtime_binding_id: None,
            approval_id: None,
            permit_id: None,
            execution_id: None,
            causation_message_id: None,
        }
    }
}

/// Source category of an authenticated actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// Interactive human principal.
    Human,
    /// Locally configured automation principal.
    Automation,
    /// Operating-system service principal.
    Service,
}

/// Authentication strength established by an ingress adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthAssurance {
    /// Local operating-system identity was verified.
    LocalOs,
    /// A channel or web identity assertion was verified.
    RemoteVerified,
    /// A configured automation credential was verified.
    AutomationCredential,
}

/// Authenticated actor identity supplied by an ingress identity resolver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRef {
    /// COSH-owned actor identity.
    pub actor_id: ActorId,
    /// Actor source category.
    pub actor_kind: ActorKind,
    /// Bounded identity issuer name.
    pub issuer: BoundedName,
    /// Assurance established by the adapter.
    pub assurance: AuthAssurance,
}

/// Opaque operating-system or remote environment selected for a Task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetRef {
    /// Target provider or environment kind.
    pub kind: BoundedName,
    /// Authority that owns the target namespace.
    pub authority: BoundedName,
    /// Opaque target identifier within the authority.
    pub identifier: BoundedOpaque,
}

/// Workspace supplied to a newly opened Agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    /// Digest of the canonical workspace scope.
    pub scope_digest: Digest,
    /// Optional safe display label.
    pub display_name: Option<BoundedText>,
}

/// Runtime choice requested by a Task command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSelector {
    /// Runtime adapter kind, such as an ACP or Core bridge.
    pub runtime: BoundedName,
    /// Optional configured runtime profile.
    pub profile: Option<BoundedName>,
}

/// Fenced binding between a Task Run and an external Agent session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeBindingRef {
    /// COSH binding identity.
    pub binding_id: RuntimeBindingId,
    /// Task owning the binding.
    pub task_id: TaskId,
    /// Run owning the binding.
    pub run_id: RunId,
    /// COSH logical Agent session.
    pub agent_session_id: AgentSessionId,
    /// Supervised child process identity.
    pub runtime_instance_id: RuntimeInstanceId,
    /// Process generation used to reject stale output.
    pub runtime_generation: u64,
    /// Scoped provider or ACP session reference.
    pub external_session: ExternalRef,
}

/// Content exchanged with an Agent Runtime without transport-specific types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContentPart {
    /// Bounded UTF-8 text.
    Text {
        /// Text content.
        text: BoundedText,
    },
    /// Link to a resource resolved outside the contract layer.
    ResourceLink {
        /// Opaque bounded resource locator.
        uri: BoundedOpaque,
        /// Optional safe display label.
        label: Option<BoundedText>,
    },
}
