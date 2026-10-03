//! Typed client for cosh-core's provider-free session management protocol.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::CoshCoreAdapter;

const DEFAULT_SESSION_CONTROL_TIMEOUT: Duration = Duration::from_secs(10);
const CLEAR_PLAN_PAGE_SIZE: usize = 4096;
const CLEAR_REQUEST_BATCH_SIZE: usize = 128;

mod process;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Health returned by the core-owned session protocol.
pub enum SessionHealth {
    /// Session can be selected for recovery.
    Ready,
    /// Stored data is malformed.
    Corrupt,
    /// Stored schema is newer or otherwise unsupported.
    Incompatible,
    /// Stored data belongs to another workspace.
    ScopeMismatch,
}

impl SessionHealth {
    /// Returns the core protocol label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Corrupt => "corrupt",
            Self::Incompatible => "incompatible",
            Self::ScopeMismatch => "scope_mismatch",
        }
    }

    /// Reports whether the picker may resume this session.
    pub fn can_resume(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// Provider-session metadata safe for the shell to display.
pub struct SessionSummary {
    /// Provider conversation ID.
    pub session_id: String,
    /// Canonical owning workspace.
    pub workspace_scope: String,
    /// Creation timestamp in Unix milliseconds.
    pub created_at_ms: u64,
    /// Last update timestamp in Unix milliseconds.
    pub updated_at_ms: u64,
    /// Model name when available.
    pub model: Option<String>,
    /// Model-visible message count.
    pub message_count: usize,
    /// First user prompt used as a picker preview.
    pub first_prompt: Option<String>,
    /// Detected persistence schema.
    pub schema_version: Option<u32>,
    /// Discovery health.
    pub health: SessionHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Stable, recoverable error returned by session management.
pub struct SessionErrorInfo {
    /// Machine-readable error code.
    pub code: String,
    /// Developer-oriented error detail.
    pub message: String,
    /// Whether the interactive shell can continue.
    pub recoverable: bool,
    /// Optional user recovery hint.
    pub hint: Option<String>,
}

impl SessionErrorInfo {
    fn transport(message: impl Into<String>) -> Self {
        Self {
            code: "transport".to_string(),
            message: message.into(),
            recoverable: true,
            hint: Some("Check the cosh-core path and retry.".to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
/// One session that a clear request did not delete.
pub struct SessionClearFailure {
    /// Provider ID that was skipped.
    pub session_id: String,
    /// Typed reason the item was skipped.
    pub error: SessionErrorInfo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One bounded page of session summaries.
pub struct SessionList {
    /// Newest-first summaries.
    pub sessions: Vec<SessionSummary>,
    /// Cursor for the next page, when one exists.
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Per-item outcome of a confirmed clear request.
pub struct SessionClearResult {
    /// Provider IDs deleted by core.
    pub deleted: Vec<String>,
    /// Provider IDs retained with typed errors.
    pub skipped: Vec<SessionClearFailure>,
    /// Batch failure after earlier mutations, when the full request did not finish.
    pub interruption: Option<SessionClearInterruption>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Partial-success boundary for a batched clear operation.
pub struct SessionClearInterruption {
    /// Error returned while the current batch had an unknown commit status.
    pub error: SessionErrorInfo,
    /// IDs sent in the failed batch whose deletion status is unknown.
    pub unknown_session_ids: Vec<String>,
    /// IDs not sent because processing stopped at the failed batch.
    pub unattempted_session_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Exact protected deletion plan prepared without loading session summaries.
pub struct SessionClearPlan {
    /// Canonical IDs eligible for confirmed deletion.
    pub session_ids: Vec<String>,
    /// Active or selected IDs excluded by the core.
    pub protected_session_ids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    ok: bool,
    data: Option<Value>,
    error: Option<SessionErrorInfo>,
}

#[derive(Debug, Deserialize)]
struct ListData {
    action: String,
    sessions: Vec<SessionSummary>,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SummaryData {
    action: String,
    session: SessionSummary,
}

#[derive(Debug, Deserialize)]
struct ClearData {
    action: String,
    deleted: Vec<String>,
    skipped: Vec<SessionClearFailure>,
}

#[derive(Debug, Deserialize)]
struct PrepareClearAllData {
    action: String,
    session_ids: Vec<String>,
    protected_session_ids: Vec<String>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Clone)]
/// One-shot client for cosh-core's provider-free management mode.
pub struct SessionManagementClient {
    program: String,
    timeout: Duration,
}

impl SessionManagementClient {
    /// Targets the supplied cosh-core executable.
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            timeout: DEFAULT_SESSION_CONTROL_TIMEOUT,
        }
    }

    /// Overrides the bounded lifetime of each one-shot management process.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Lists a bounded, cursor-addressed page.
    ///
    /// When `all_workspaces` is true, the request asks cosh-core to aggregate
    /// sessions from every workspace-scoped store.
    ///
    /// # Errors
    ///
    /// Returns a transport or core-owned typed session error.
    pub fn list(
        &self,
        workspace_scope: &str,
        limit: usize,
        cursor: Option<&str>,
        all_workspaces: bool,
    ) -> Result<SessionList, SessionErrorInfo> {
        let data: ListData = self.request(json!({
            "action": "list",
            "workspace_scope": workspace_scope,
            "limit": limit,
            "cursor": cursor,
            "all_workspaces": all_workspaces
        }))?;
        if data.action != "list" {
            return Err(SessionErrorInfo::transport(
                "cosh-core returned the wrong session action",
            ));
        }
        Ok(SessionList {
            sessions: data.sessions,
            next_cursor: data.next_cursor,
        })
    }

    /// Reads a summary even when its health prevents resume.
    ///
    /// # Errors
    ///
    /// Returns a transport or core-owned typed session error.
    pub fn inspect(
        &self,
        workspace_scope: &str,
        session_id: &str,
    ) -> Result<SessionSummary, SessionErrorInfo> {
        let data: SummaryData = self.request(json!({
            "action": "inspect",
            "workspace_scope": workspace_scope,
            "session_id": session_id
        }))?;
        if data.action != "inspect" {
            return Err(SessionErrorInfo::transport(
                "cosh-core returned the wrong session action",
            ));
        }
        Ok(data.session)
    }

    /// Validates that a session can be resumed in the workspace.
    ///
    /// # Errors
    ///
    /// Returns a transport or core-owned typed session error.
    pub fn validate(
        &self,
        workspace_scope: &str,
        session_id: &str,
    ) -> Result<SessionSummary, SessionErrorInfo> {
        let data: SummaryData = self.request(json!({
            "action": "validate",
            "workspace_scope": workspace_scope,
            "session_id": session_id
        }))?;
        if data.action != "validate" {
            return Err(SessionErrorInfo::transport(
                "cosh-core returned the wrong session action",
            ));
        }
        Ok(data.session)
    }

    /// Prepares an exact clear-all plan without loading persisted message summaries.
    ///
    /// # Errors
    ///
    /// Returns a transport or request-level core error.
    pub fn prepare_clear_all(
        &self,
        workspace_scope: &str,
        protected_session_ids: &[String],
    ) -> Result<SessionClearPlan, SessionErrorInfo> {
        if protected_session_ids.len() > CLEAR_REQUEST_BATCH_SIZE {
            return Err(SessionErrorInfo {
                code: "invalid_request".to_string(),
                message: format!(
                    "protected_session_ids contains {} IDs; at most \
                     {CLEAR_REQUEST_BATCH_SIZE} are allowed",
                    protected_session_ids.len()
                ),
                recoverable: true,
                hint: Some("Retry with a bounded protected-session set.".to_string()),
            });
        }
        let mut plan = SessionClearPlan {
            session_ids: Vec::new(),
            protected_session_ids: Vec::new(),
        };
        let mut cursor = None;
        loop {
            let data: PrepareClearAllData = self.request(json!({
                "action": "prepare_clear_all",
                "workspace_scope": workspace_scope,
                "protected_session_ids": protected_session_ids,
                "limit": CLEAR_PLAN_PAGE_SIZE,
                "cursor": cursor.as_deref()
            }))?;
            if data.action != "prepare_clear_all" {
                return Err(SessionErrorInfo::transport(
                    "cosh-core returned the wrong session action",
                ));
            }
            plan.session_ids.extend(data.session_ids);
            plan.protected_session_ids
                .extend(data.protected_session_ids);
            match data.next_cursor {
                Some(next_cursor)
                    if cursor
                        .as_deref()
                        .is_some_and(|previous| next_cursor.as_str() <= previous) =>
                {
                    return Err(SessionErrorInfo::transport(
                        "cosh-core returned a non-advancing clear-all pagination cursor",
                    ));
                }
                Some(next_cursor) => cursor = Some(next_cursor),
                None => break,
            }
        }
        Ok(plan)
    }

    /// Clears explicit IDs while forwarding protected IDs to core.
    ///
    /// # Errors
    ///
    /// Returns a transport or request-level core error; item failures are in the result.
    pub fn clear(
        &self,
        workspace_scope: &str,
        session_ids: &[String],
        protected_session_ids: &[String],
    ) -> Result<SessionClearResult, SessionErrorInfo> {
        if protected_session_ids.len() > CLEAR_REQUEST_BATCH_SIZE {
            return Err(SessionErrorInfo {
                code: "invalid_request".to_string(),
                message: format!(
                    "protected_session_ids contains {} IDs; at most \
                     {CLEAR_REQUEST_BATCH_SIZE} are allowed",
                    protected_session_ids.len()
                ),
                recoverable: true,
                hint: Some("Retry with a bounded protected-session set.".to_string()),
            });
        }
        let mut result = SessionClearResult {
            deleted: Vec::new(),
            skipped: Vec::new(),
            interruption: None,
        };
        let batches = session_ids.chunks(CLEAR_REQUEST_BATCH_SIZE);
        for (batch_index, batch) in batches.enumerate() {
            let data: ClearData = match self.request(json!({
                "action": "clear",
                "workspace_scope": workspace_scope,
                "session_ids": batch,
                "protected_session_ids": protected_session_ids
            })) {
                Ok(data) => data,
                Err(error) => {
                    let completed = (batch_index + 1) * CLEAR_REQUEST_BATCH_SIZE;
                    result.interruption = Some(SessionClearInterruption {
                        error,
                        unknown_session_ids: batch.to_vec(),
                        unattempted_session_ids: session_ids
                            .get(completed..)
                            .unwrap_or_default()
                            .to_vec(),
                    });
                    return Ok(result);
                }
            };
            if data.action != "clear" {
                let completed = (batch_index + 1) * CLEAR_REQUEST_BATCH_SIZE;
                result.interruption = Some(SessionClearInterruption {
                    error: SessionErrorInfo::transport(
                        "cosh-core returned the wrong session action",
                    ),
                    unknown_session_ids: batch.to_vec(),
                    unattempted_session_ids: session_ids
                        .get(completed..)
                        .unwrap_or_default()
                        .to_vec(),
                });
                return Ok(result);
            }
            result.deleted.extend(data.deleted);
            result.skipped.extend(data.skipped);
        }
        Ok(result)
    }

    fn request<T: for<'de> Deserialize<'de>>(&self, request: Value) -> Result<T, SessionErrorInfo> {
        let deadline = Instant::now() + self.timeout;
        let request = serde_json::to_vec(&request).map_err(|error| {
            SessionErrorInfo::transport(format!("failed to encode session request: {error}"))
        })?;
        let output =
            process::execute(&self.program, request, deadline, self.timeout).map_err(|error| {
                SessionErrorInfo::transport(format!("failed to run cosh-core: {error}"))
            })?;
        let envelope: Envelope = serde_json::from_slice(&output.stdout).map_err(|error| {
            let stderr = String::from_utf8_lossy(&output.stderr);
            SessionErrorInfo::transport(format!(
                "invalid cosh-core session response: {error}; {}",
                stderr.trim()
            ))
        })?;
        if !envelope.ok {
            return Err(envelope.error.unwrap_or_else(|| {
                SessionErrorInfo::transport("cosh-core session request failed without an error")
            }));
        }
        let data = envelope.data.ok_or_else(|| {
            SessionErrorInfo::transport("cosh-core session response omitted data")
        })?;
        serde_json::from_value(data).map_err(|error| {
            SessionErrorInfo::transport(format!("invalid cosh-core session response data: {error}"))
        })
    }
}

impl CoshCoreAdapter {
    /// Lists persisted sessions in a canonical workspace.
    ///
    /// When `all_workspaces` is true, summaries from every workspace-scoped
    /// store are returned; foreign workspaces are marked `scope_mismatch`.
    ///
    /// # Errors
    ///
    /// Returns a recoverable management protocol error.
    pub fn list_sessions(
        &self,
        workspace_scope: &str,
        all_workspaces: bool,
    ) -> Result<SessionList, SessionErrorInfo> {
        self.list_sessions_page(workspace_scope, 20, None, all_workspaces)
    }

    /// Lists one bounded page while preserving the core-owned opaque cursor.
    ///
    /// # Errors
    ///
    /// Returns a recoverable management protocol error.
    pub fn list_sessions_page(
        &self,
        workspace_scope: &str,
        limit: usize,
        cursor: Option<&str>,
        all_workspaces: bool,
    ) -> Result<SessionList, SessionErrorInfo> {
        SessionManagementClient::new(self.program.clone()).list(
            workspace_scope,
            limit,
            cursor,
            all_workspaces,
        )
    }
}

#[cfg(test)]
mod tests;
