//! Provider-free JSON protocol for session discovery, validation, and deletion.

use std::io::{self, Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::CoreConfig;
use crate::session::{
    bounded_summary_text, ProviderSessionId, SessionError, SessionStore, SessionSummary,
};

const MAX_CLEAR_PLAN_LIMIT: usize = 4096;
const MAX_CLEAR_REQUEST_IDS: usize = 128;
const MAX_ERROR_MESSAGE_BYTES: usize = 2048;
const MAX_REPORTED_SESSION_ID_BYTES: usize = 128;
const MAX_SESSION_CONTROL_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_SESSION_CONTROL_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum SessionControlRequest {
    List {
        workspace_scope: String,
        #[serde(default = "default_list_limit")]
        limit: usize,
        #[serde(default)]
        cursor: Option<String>,
        #[serde(default)]
        all_workspaces: bool,
    },
    Inspect {
        workspace_scope: String,
        session_id: String,
    },
    Validate {
        workspace_scope: String,
        session_id: String,
    },
    PrepareClearAll {
        workspace_scope: String,
        protected_session_ids: Vec<String>,
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        cursor: Option<String>,
    },
    Clear {
        workspace_scope: String,
        session_ids: Vec<String>,
        protected_session_ids: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
struct SessionControlResponse<T> {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<SessionErrorInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Stable JSON representation of a core session error.
pub struct SessionErrorInfo {
    /// Machine-readable error code.
    pub code: String,
    /// Developer-oriented failure detail.
    pub message: String,
    /// Whether an interactive caller can continue.
    pub recoverable: bool,
    /// Optional recovery guidance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl From<&SessionError> for SessionErrorInfo {
    fn from(error: &SessionError) -> Self {
        Self {
            code: error.code().to_string(),
            message: bounded_summary_text(&error.to_string(), MAX_ERROR_MESSAGE_BYTES),
            recoverable: error.recoverable(),
            hint: error.hint().map(str::to_string),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum SessionControlData {
    List {
        sessions: Vec<SessionSummary>,
        next_cursor: Option<String>,
    },
    Inspect {
        session: SessionSummary,
    },
    Validate {
        session: SessionSummary,
    },
    PrepareClearAll {
        session_ids: Vec<String>,
        protected_session_ids: Vec<String>,
        next_cursor: Option<String>,
    },
    Clear {
        deleted: Vec<String>,
        skipped: Vec<SessionClearFailure>,
    },
}

#[derive(Debug, Serialize)]
struct SessionClearFailure {
    session_id: String,
    error: SessionErrorInfo,
}

/// Serves one management request from standard input and writes one JSON response.
pub fn run() -> i32 {
    let response = read_request().and_then(|request| {
        let config = CoreConfig::load_for_workspace(Path::new(request.workspace_scope()));
        handle_request(&config, request)
    });
    let mut envelope = match response {
        Ok(data) => SessionControlResponse {
            ok: true,
            data: Some(data),
            error: None,
        },
        Err(error) => SessionControlResponse {
            ok: false,
            data: None,
            error: Some(SessionErrorInfo::from(&error)),
        },
    };
    let mut encoded = match serde_json::to_vec(&envelope) {
        Ok(encoded) => encoded,
        Err(_) => return 1,
    };
    if encoded.len().saturating_add(1) > MAX_SESSION_CONTROL_RESPONSE_BYTES {
        envelope = SessionControlResponse {
            ok: false,
            data: None,
            error: Some(SessionErrorInfo {
                code: "invalid_request".to_string(),
                message: "session-control response exceeded the protocol byte budget".to_string(),
                recoverable: true,
                hint: Some("Retry with a smaller page or request batch.".to_string()),
            }),
        };
        encoded = match serde_json::to_vec(&envelope) {
            Ok(encoded) => encoded,
            Err(_) => return 1,
        };
    }
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());
    if writer.write_all(&encoded).is_err() || writeln!(writer).is_err() {
        return 1;
    }
    if envelope.ok {
        0
    } else {
        1
    }
}

impl SessionControlRequest {
    fn workspace_scope(&self) -> &str {
        match self {
            Self::List {
                workspace_scope, ..
            }
            | Self::Inspect {
                workspace_scope, ..
            }
            | Self::Validate {
                workspace_scope, ..
            }
            | Self::PrepareClearAll {
                workspace_scope, ..
            }
            | Self::Clear {
                workspace_scope, ..
            } => workspace_scope,
        }
    }
}

fn read_request() -> Result<SessionControlRequest, SessionError> {
    let mut input = Vec::new();
    io::stdin()
        .lock()
        .take(MAX_SESSION_CONTROL_REQUEST_BYTES.saturating_add(1) as u64)
        .read_to_end(&mut input)
        .map_err(|error| SessionError::Io {
            operation: "read session-control request",
            path: "<stdin>".into(),
            message: error.to_string(),
        })?;
    if input.len() > MAX_SESSION_CONTROL_REQUEST_BYTES {
        return Err(SessionError::InvalidRequest {
            message: format!(
                "session-control request exceeds the {MAX_SESSION_CONTROL_REQUEST_BYTES} byte limit"
            ),
        });
    }
    serde_json::from_slice(&input).map_err(|error| SessionError::InvalidRequest {
        message: format!("invalid session-control request: {error}"),
    })
}

fn handle_request(
    config: &CoreConfig,
    request: SessionControlRequest,
) -> Result<SessionControlData, SessionError> {
    match request {
        SessionControlRequest::List {
            workspace_scope,
            limit,
            cursor,
            all_workspaces,
        } => {
            let store = store(config, &workspace_scope)?;
            let (sessions, next_cursor) = store.list(limit, cursor.as_deref(), all_workspaces)?;
            Ok(SessionControlData::List {
                sessions,
                next_cursor,
            })
        }
        SessionControlRequest::Inspect {
            workspace_scope,
            session_id,
        } => {
            let store = store(config, &workspace_scope)?;
            let session_id = ProviderSessionId::parse(&session_id)?;
            Ok(SessionControlData::Inspect {
                session: store.inspect(&session_id)?,
            })
        }
        SessionControlRequest::Validate {
            workspace_scope,
            session_id,
        } => {
            let store = store(config, &workspace_scope)?;
            let session_id = ProviderSessionId::parse(&session_id)?;
            Ok(SessionControlData::Validate {
                session: store.validate(&session_id)?,
            })
        }
        SessionControlRequest::PrepareClearAll {
            workspace_scope,
            protected_session_ids,
            limit,
            cursor,
        } => {
            ensure_bounded_ids("protected_session_ids", &protected_session_ids)?;
            let store = store(config, &workspace_scope)?;
            let protected = protected_session_ids
                .iter()
                .map(|value| ProviderSessionId::parse(value))
                .collect::<Result<Vec<_>, _>>()?;
            let session_ids = store.session_ids()?;
            if limit.is_none() && session_ids.len() > MAX_CLEAR_PLAN_LIMIT {
                return Err(SessionError::InvalidRequest {
                    message: format!(
                        "prepare_clear_all requires pagination above \
                         {MAX_CLEAR_PLAN_LIMIT} stored IDs"
                    ),
                });
            }
            let start = match cursor {
                Some(cursor) => {
                    let parsed = ProviderSessionId::parse(&cursor)
                        .map_err(|_| SessionError::InvalidCursor { cursor })?;
                    session_ids.partition_point(|session_id| session_id.as_str() <= parsed.as_str())
                }
                None => 0,
            };
            let end = start
                .saturating_add(
                    limit
                        .unwrap_or(MAX_CLEAR_PLAN_LIMIT)
                        .clamp(1, MAX_CLEAR_PLAN_LIMIT),
                )
                .min(session_ids.len());
            let next_cursor = (end < session_ids.len())
                .then(|| session_ids.get(end.saturating_sub(1)))
                .flatten()
                .map(ToString::to_string);
            let (session_ids, protected_session_ids) = session_ids[start..end]
                .iter()
                .cloned()
                .partition::<Vec<_>, _>(|session_id| !protected.contains(session_id));
            Ok(SessionControlData::PrepareClearAll {
                session_ids: session_ids
                    .into_iter()
                    .map(|session_id| session_id.to_string())
                    .collect(),
                protected_session_ids: protected_session_ids
                    .into_iter()
                    .map(|session_id| session_id.to_string())
                    .collect(),
                next_cursor,
            })
        }
        SessionControlRequest::Clear {
            workspace_scope,
            session_ids,
            protected_session_ids,
        } => {
            ensure_bounded_ids("session_ids", &session_ids)?;
            ensure_bounded_ids("protected_session_ids", &protected_session_ids)?;
            let store = store(config, &workspace_scope)?;
            let protected = protected_session_ids
                .iter()
                .map(|value| ProviderSessionId::parse(value))
                .collect::<Result<Vec<_>, _>>()?;
            let mut deleted = Vec::new();
            let mut skipped = Vec::new();
            for value in session_ids {
                let session_id = match ProviderSessionId::parse(&value) {
                    Ok(session_id) => session_id,
                    Err(error) => {
                        skipped.push(SessionClearFailure {
                            session_id: bounded_summary_text(&value, MAX_REPORTED_SESSION_ID_BYTES),
                            error: SessionErrorInfo::from(&error),
                        });
                        continue;
                    }
                };
                match store.clear(&session_id, &protected) {
                    Ok(()) => deleted.push(session_id.to_string()),
                    Err(error) => skipped.push(SessionClearFailure {
                        session_id: session_id.to_string(),
                        error: SessionErrorInfo::from(&error),
                    }),
                }
            }
            Ok(SessionControlData::Clear { deleted, skipped })
        }
    }
}

fn store(config: &CoreConfig, workspace_scope: &str) -> Result<SessionStore, SessionError> {
    SessionStore::for_workspace(&config.session.persist_dir, Path::new(workspace_scope))
}

fn default_list_limit() -> usize {
    20
}

fn ensure_bounded_ids(field: &str, ids: &[String]) -> Result<(), SessionError> {
    if ids.len() > MAX_CLEAR_REQUEST_IDS {
        return Err(SessionError::InvalidRequest {
            message: format!(
                "{field} contains {} IDs; at most {MAX_CLEAR_REQUEST_IDS} are allowed",
                ids.len()
            ),
        });
    }
    Ok(())
}
