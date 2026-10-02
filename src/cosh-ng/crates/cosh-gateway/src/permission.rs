//! Fail-closed local handling and redacted evidence for ACP once-only choices.

mod evidence;

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::runtime::{AcpV1PermissionDecision, AcpV1PermissionOptionKind, AcpV1PermissionRequest};

pub use evidence::{FilePermissionEvidenceSink, PermissionEvidenceError};

const MAX_DISPLAY_BYTES: usize = 512;

/// Stable local decision class. It never creates a durable trust rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OncePermissionDecision {
    /// Select an Agent-provided `allow_once` option.
    AllowOnce,
    /// Select an Agent-provided `reject_once` option.
    RejectOnce,
    /// Refuse to select an option because interaction could not complete.
    Cancelled,
}

/// Redacted interaction presented to a local actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OncePermissionPrompt {
    /// Bounded untrusted display title derived from the tool call.
    pub title: String,
    /// Whether the Agent offered an `allow_once` option.
    pub can_allow_once: bool,
    /// Whether the Agent offered a `reject_once` option.
    pub can_reject_once: bool,
}

/// Durable, redacted evidence written before replying to the Agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionEvidence {
    /// Evidence schema version.
    pub schema_version: u16,
    /// Milliseconds since the Unix epoch.
    pub occurred_at_ms: u64,
    /// Stable built-in profile name.
    pub profile: String,
    /// SHA-256 of the canonical workspace path bytes.
    pub workspace_digest: String,
    /// SHA-256 of the opaque ACP session identifier.
    pub session_digest: String,
    /// SHA-256 of the JSON-RPC request identifier.
    pub request_digest: String,
    /// SHA-256 of the complete validated tool-call value.
    pub tool_call_digest: String,
    /// Effective local user that made the decision.
    pub actor_uid: u32,
    /// Once-only decision class; never an Agent option label.
    pub decision: OncePermissionDecision,
}

/// Writes permission evidence at a security boundary.
pub trait PermissionEvidenceSink {
    /// Persists one record before the caller responds to the Agent.
    fn record(&mut self, evidence: &PermissionEvidence) -> Result<(), PermissionEvidenceError>;
}

impl<T: PermissionEvidenceSink + ?Sized> PermissionEvidenceSink for &mut T {
    fn record(&mut self, evidence: &PermissionEvidence) -> Result<(), PermissionEvidenceError> {
        (**self).record(evidence)
    }
}

/// Presents one local permission interaction.
pub trait PermissionPresenter {
    /// Returns a once-only choice. EOF and unavailable interaction must cancel.
    fn decide(
        &mut self,
        prompt: &OncePermissionPrompt,
    ) -> Result<OncePermissionDecision, PermissionProxyError>;
}

/// Presenter used when no trusted local terminal is available or prompting is disabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct CancelPermissionPresenter;

impl PermissionPresenter for CancelPermissionPresenter {
    fn decide(
        &mut self,
        _prompt: &OncePermissionPrompt,
    ) -> Result<OncePermissionDecision, PermissionProxyError> {
        Ok(OncePermissionDecision::Cancelled)
    }
}

/// Line-oriented presenter used only with a trusted local terminal.
#[derive(Debug)]
pub struct TextPermissionPresenter<R, W> {
    input: R,
    output: W,
}

impl<R, W> TextPermissionPresenter<R, W> {
    /// Creates a presenter over caller-owned terminal streams.
    pub fn new(input: R, output: W) -> Self {
        Self { input, output }
    }
}

impl<R: BufRead, W: Write> PermissionPresenter for TextPermissionPresenter<R, W> {
    fn decide(
        &mut self,
        prompt: &OncePermissionPrompt,
    ) -> Result<OncePermissionDecision, PermissionProxyError> {
        writeln!(self.output, "Agent requests permission: {}", prompt.title)?;
        match (prompt.can_allow_once, prompt.can_reject_once) {
            (true, true) => writeln!(self.output, "Choose [a]llow once / [r]eject once:")?,
            (true, false) => writeln!(self.output, "Choose [a]llow once / [c]ancel:")?,
            (false, true) => writeln!(self.output, "Choose [r]eject once / [c]ancel:")?,
            (false, false) => return Ok(OncePermissionDecision::Cancelled),
        }
        self.output.flush()?;
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Ok(OncePermissionDecision::Cancelled);
        }
        Ok(match line.trim().to_ascii_lowercase().as_str() {
            "a" | "allow" | "allow_once" if prompt.can_allow_once => {
                OncePermissionDecision::AllowOnce
            }
            "r" | "reject" | "reject_once" if prompt.can_reject_once => {
                OncePermissionDecision::RejectOnce
            }
            _ => OncePermissionDecision::Cancelled,
        })
    }
}

/// Fail-closed orchestration for one correlated ACP permission callback.
#[derive(Debug)]
pub struct OncePermissionProxy<P, E> {
    presenter: P,
    evidence: E,
}

impl<P, E> OncePermissionProxy<P, E> {
    /// Creates a proxy whose evidence sink is authoritative for replies.
    pub fn new(presenter: P, evidence: E) -> Self {
        Self {
            presenter,
            evidence,
        }
    }
}

impl<P: PermissionPresenter, E: PermissionEvidenceSink> OncePermissionProxy<P, E> {
    /// Resolves one request and persists redacted evidence before returning a reply.
    ///
    /// # Errors
    ///
    /// Returns presentation, serialization, or durable evidence failures. The
    /// caller must cancel the callback when an error is returned.
    pub fn resolve(
        &mut self,
        context: PermissionEvidenceContext<'_>,
        request: &AcpV1PermissionRequest,
    ) -> Result<AcpV1PermissionDecision, PermissionProxyError> {
        let allow = request
            .options
            .iter()
            .find(|option| option.kind == AcpV1PermissionOptionKind::AllowOnce);
        let reject = request
            .options
            .iter()
            .find(|option| option.kind == AcpV1PermissionOptionKind::RejectOnce);
        let prompt = OncePermissionPrompt {
            title: bounded_title(&request.tool_call),
            can_allow_once: allow.is_some(),
            can_reject_once: reject.is_some(),
        };
        let decision = self.presenter.decide(&prompt)?;
        let selected = match decision {
            OncePermissionDecision::AllowOnce => allow.map(|option| option.option_id.clone()),
            OncePermissionDecision::RejectOnce => reject.map(|option| option.option_id.clone()),
            OncePermissionDecision::Cancelled => None,
        };
        let effective = if selected.is_some() {
            decision
        } else {
            OncePermissionDecision::Cancelled
        };
        let evidence = evidence(context, request, effective)?;
        self.evidence.record(&evidence)?;
        Ok(
            selected.map_or(AcpV1PermissionDecision::Cancelled, |option_id| {
                AcpV1PermissionDecision::Selected { option_id }
            }),
        )
    }
}

/// Trusted fields bound by the local entrypoint for one evidence record.
#[derive(Debug, Clone, Copy)]
pub struct PermissionEvidenceContext<'a> {
    /// Stable built-in profile name.
    pub profile: &'a str,
    /// Canonical workspace bytes used only as digest input.
    pub canonical_workspace: &'a [u8],
    /// Effective local actor UID.
    pub actor_uid: u32,
    /// Current wall-clock milliseconds.
    pub occurred_at_ms: u64,
}

/// Failure at the once-only permission boundary.
#[derive(Debug, Error)]
pub enum PermissionProxyError {
    /// Local interaction stream failed.
    #[error("local permission interaction failed: {0}")]
    Interaction(#[from] io::Error),
    /// The validated tool call could not be encoded for correlation.
    #[error("permission correlation encoding failed: {0}")]
    Correlation(#[from] serde_json::Error),
    /// Durable evidence could not be written; no Agent reply is authorized.
    #[error(transparent)]
    Evidence(#[from] PermissionEvidenceError),
}

fn bounded_title(tool_call: &serde_json::Value) -> String {
    let title = tool_call
        .get("title")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("Agent tool request");
    let mut title = title
        .chars()
        .filter(|value| !value.is_control())
        .collect::<String>();
    if title.len() > MAX_DISPLAY_BYTES {
        let mut boundary = MAX_DISPLAY_BYTES;
        while !title.is_char_boundary(boundary) {
            boundary -= 1;
        }
        title.truncate(boundary);
    }
    title
}

fn evidence(
    context: PermissionEvidenceContext<'_>,
    request: &AcpV1PermissionRequest,
    decision: OncePermissionDecision,
) -> Result<PermissionEvidence, serde_json::Error> {
    let request_id = match &request.request_id {
        crate::runtime::AcpV1RequestId::Number(value) => format!("number:{value}"),
        crate::runtime::AcpV1RequestId::String(value) => format!("string:{value}"),
    };
    let tool_call = serde_json::to_vec(&request.tool_call)?;
    Ok(PermissionEvidence {
        schema_version: 1,
        occurred_at_ms: context.occurred_at_ms,
        profile: context.profile.to_owned(),
        workspace_digest: digest(context.canonical_workspace),
        session_digest: digest(request.session_id.as_bytes()),
        request_digest: digest(request_id.as_bytes()),
        tool_call_digest: digest(&tool_call),
        actor_uid: context.actor_uid,
        decision,
    })
}

fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests;
