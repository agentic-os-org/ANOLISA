//! Versioned, workspace-scoped persistence for provider conversation sessions.

use std::fmt;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::compaction::CompactionState;
use crate::provider::Message;

mod io;
mod listing;
mod scoped;
mod store;
mod summary;

pub use store::SessionStore;
pub(crate) use summary::bounded_summary_text;

pub(super) const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
/// Canonical provider conversation identifier accepted by persistence paths.
pub struct ProviderSessionId(String);

impl ProviderSessionId {
    /// Parses a lowercase canonical UUID before any filesystem path is built.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidId`] for malformed or non-canonical UUIDs.
    pub fn parse(value: &str) -> Result<Self, SessionError> {
        let parsed = uuid::Uuid::parse_str(value).map_err(|_| SessionError::InvalidId {
            value: value.to_string(),
        })?;
        if parsed.to_string() != value {
            return Err(SessionError::InvalidId {
                value: value.to_string(),
            });
        }
        Ok(Self(value.to_string()))
    }

    /// Generates a new provider conversation identifier.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Returns the canonical UUID text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ProviderSessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ProviderSessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Health reported during session discovery.
pub enum SessionHealth {
    /// The envelope can be loaded and resumed.
    Ready,
    /// The stored JSON or envelope contract is malformed.
    Corrupt,
    /// The envelope uses a schema version this binary cannot load.
    Incompatible,
    /// The envelope belongs to another canonical workspace.
    ScopeMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Versioned provider conversation envelope stored on disk.
pub struct PersistedSession {
    /// Persistence schema version.
    pub schema_version: u32,
    /// Immutable provider conversation identity.
    pub session_id: ProviderSessionId,
    /// Canonical workspace that owns the session.
    pub workspace_scope: String,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last committed timestamp in Unix milliseconds.
    pub updated_at_ms: u64,
    /// Model associated with the conversation.
    pub model: String,
    /// Optimistic concurrency generation.
    pub generation: u64,
    /// Model-visible conversation history.
    pub messages: Vec<Message>,
    /// Optional compaction projection over the transcript prefix.
    ///
    /// Absent for pre-compaction envelopes; ignoring it always yields the
    /// complete transcript, so older readers remain correct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<CompactionState>,
    /// Highest committed compaction revision, retained even when the active
    /// projection is rejected during loading.
    ///
    /// This is a monotonic clock, not a property of the live projection: a
    /// projection dropped by sanitization must not let the next commit reuse
    /// a revision an earlier commit already published.
    #[serde(default)]
    pub compaction_revision: u64,
}

impl PersistedSession {
    /// Builds an uncommitted schema-v1 session envelope.
    pub fn new(
        session_id: ProviderSessionId,
        workspace_scope: String,
        model: String,
        messages: Vec<Message>,
    ) -> Self {
        let now = now_ms();
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            session_id,
            workspace_scope,
            created_at_ms: now,
            updated_at_ms: now,
            model,
            generation: 0,
            messages,
            compaction: None,
            compaction_revision: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Bounded metadata exposed to session-management clients.
pub struct SessionSummary {
    /// Provider conversation identity.
    pub session_id: ProviderSessionId,
    /// Canonical owning workspace.
    pub workspace_scope: String,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last update timestamp in Unix milliseconds.
    pub updated_at_ms: u64,
    /// Model name when recoverable from the envelope.
    pub model: Option<String>,
    /// Model-visible message count.
    pub message_count: usize,
    /// First user prompt used as a picker preview.
    pub first_prompt: Option<String>,
    /// Detected schema version, when available.
    pub schema_version: Option<u32>,
    /// Discovery health.
    pub health: SessionHealth,
}

#[derive(Debug, Clone)]
/// Stable persistence and recovery failure categories.
pub enum SessionError {
    /// A caller supplied a non-canonical provider ID.
    InvalidId {
        /// Rejected value.
        value: String,
    },
    /// A pagination cursor does not match the supported opaque format.
    InvalidCursor {
        /// Rejected cursor.
        cursor: String,
    },
    /// A management request exceeds a bounded protocol contract.
    InvalidRequest {
        /// Rejected request detail.
        message: String,
    },
    /// No stored session exists for the requested ID.
    NotFound {
        /// Requested provider ID.
        session_id: String,
    },
    /// A filesystem operation failed.
    Io {
        /// Operation being attempted.
        operation: &'static str,
        /// Affected path.
        path: PathBuf,
        /// Underlying I/O error.
        message: String,
    },
    /// Stored content could not be decoded safely.
    Corrupt {
        /// Affected provider ID.
        session_id: String,
        /// Decode or invariant failure.
        message: String,
    },
    /// Stored content uses an unsupported schema.
    IncompatibleVersion {
        /// Affected provider ID.
        session_id: String,
        /// Unsupported schema version.
        version: u32,
    },
    /// Stored content belongs to another workspace.
    ScopeMismatch {
        /// Affected provider ID.
        session_id: String,
        /// Requested canonical workspace.
        expected: String,
        /// Workspace recorded in the envelope.
        actual: String,
    },
    /// Another writer changed or locked the session.
    Conflict {
        /// Affected provider ID.
        session_id: String,
    },
    /// The caller attempted to clear a protected session.
    ActiveSession {
        /// Protected provider ID.
        session_id: String,
    },
}

impl SessionError {
    /// Returns the stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidId { .. } => "invalid_id",
            Self::InvalidCursor { .. } => "invalid_cursor",
            Self::InvalidRequest { .. } => "invalid_request",
            Self::NotFound { .. } => "not_found",
            Self::Io { .. } => "io",
            Self::Corrupt { .. } => "corrupt",
            Self::IncompatibleVersion { .. } => "incompatible_version",
            Self::ScopeMismatch { .. } => "scope_mismatch",
            Self::Conflict { .. } => "conflict",
            Self::ActiveSession { .. } => "active_session",
        }
    }

    /// Reports whether the shell can remain usable after this failure.
    pub fn recoverable(&self) -> bool {
        true
    }

    /// Returns a concise user recovery hint.
    pub fn hint(&self) -> Option<&'static str> {
        match self {
            Self::InvalidId { .. } => Some("Use the canonical session UUID shown by session list."),
            Self::InvalidCursor { .. } => Some("Refresh the session list and restart pagination."),
            Self::InvalidRequest { .. } => {
                Some("Retry with a bounded request that follows the session protocol.")
            }
            Self::NotFound { .. } => Some("Refresh the session list and choose an existing entry."),
            Self::Corrupt { .. } => Some("Clear the damaged session after confirming its ID."),
            Self::IncompatibleVersion { .. } => {
                Some("Upgrade cosh-core or clear the incompatible session.")
            }
            Self::ScopeMismatch { .. } => Some("Resume the session from its original workspace."),
            Self::Conflict { .. } => Some("Retry after the other session writer has completed."),
            Self::ActiveSession { .. } => {
                Some("Select another session before clearing this protected session.")
            }
            Self::Io { .. } => Some("Check the session directory permissions and retry."),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId { value } => write!(formatter, "invalid session ID: {value}"),
            Self::InvalidCursor { cursor } => {
                write!(formatter, "invalid session pagination cursor: {cursor}")
            }
            Self::InvalidRequest { message } => {
                write!(formatter, "invalid session management request: {message}")
            }
            Self::NotFound { session_id } => write!(formatter, "session not found: {session_id}"),
            Self::Io {
                operation,
                path,
                message,
            } => write!(
                formatter,
                "session {operation} failed for {}: {message}",
                path.display()
            ),
            Self::Corrupt {
                session_id,
                message,
            } => write!(formatter, "session {session_id} is corrupt: {message}"),
            Self::IncompatibleVersion {
                session_id,
                version,
            } => write!(
                formatter,
                "session {session_id} uses unsupported schema version {version}"
            ),
            Self::ScopeMismatch {
                session_id,
                expected,
                actual,
            } => write!(
                formatter,
                "session {session_id} belongs to {actual}, not {expected}"
            ),
            Self::Conflict { session_id } => {
                write!(formatter, "session {session_id} changed concurrently")
            }
            Self::ActiveSession { session_id } => {
                write!(formatter, "session {session_id} is protected")
            }
        }
    }
}

impl std::error::Error for SessionError {}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}
