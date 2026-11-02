//! Orchestrates direct Cedar candidate generation; a candidate is neither an approval nor an
//! installation into an execution backend.
//!
//! 1. task text + shared schema -> model Cedar draft and model-reported unresolved items.
//! 2. official parser + canonical absolute-path check + Strict validation -> Cedar artifact.
//! 3. artifact + raw draft + TTL and hash -> unapproved candidate.
//!
//! A draft rejected as an unsupported feature or a non-canonical path becomes
//! `NeedsClarification` carrying its reasons instead of an error; any other failure aborts the
//! run, and every stage stops at the earliest of the caller deadline, the candidate TTL and
//! [`INTENT_BUDGET`].
//!
//! Generation covers a task before it starts only: `intent.propose` always yields a fresh
//! candidate and tracks no policy revision, so a running task cannot have its policy updated.
//! TODO(asc-intent maintainers): add that update path together with a trusted current revision;
//! approval, publication and enforcement stay downstream.

use std::time::{Duration, Instant};

use asc_model_client::provider::ModelProvider;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    Artifact, INTENT_BUDGET, IntentError, PolicyDraft, PolicyError, TaskInput,
    generate_policy_draft, prepare_policy,
};

/// Maximum candidate TTL; it is not the lifetime of an issued grant.
pub const MAX_PROPOSAL_TTL: u32 = 1800;
/// Serialization budget for a complete candidate.
pub const MAX_CANDIDATE_BYTES: usize = 512 * 1024;
/// Direct Cedar candidate protocol version.
pub const CANDIDATE_VERSION: &str = "asc.intent.candidate.v1";

/// Built from server identity and clock; matching fields in user JSON are never accepted.
#[derive(Clone, Serialize)]
pub struct ProposalContext {
    pub(crate) task_id: String,
    pub(crate) created_at: i64,
    pub(crate) expires_at: i64,
    #[serde(skip)]
    started: Instant,
    #[serde(skip)]
    ttl: Duration,
}

impl ProposalContext {
    /// Fixes the candidate identity and validity window; never renewed automatically.
    ///
    /// # Errors
    /// Rejects invalid identities, timestamp overflow and TTLs outside 1..=1800.
    pub fn new(task_id: String, created_at: i64, ttl: u32) -> Result<Self, ProposalError> {
        let expires_at = created_at
            .checked_add(i64::from(ttl))
            .ok_or(ProposalError::InvalidContext)?;
        if !stable_id(&task_id) || created_at <= 0 || !(1..=MAX_PROPOSAL_TTL).contains(&ttl) {
            return Err(ProposalError::InvalidContext);
        }
        Ok(Self {
            task_id,
            created_at,
            expires_at,
            started: Instant::now(),
            ttl: Duration::from_secs(u64::from(ttl)),
        })
    }
}

#[derive(Serialize)]
struct CandidateContent {
    format_version: &'static str,
    status: &'static str,
    authorization_required: bool,
    #[serde(flatten)]
    context: ProposalContext,
    artifact: Artifact,
    draft: PolicyDraft,
    notes: Vec<String>,
}

/// A candidate that passed Cedar validation but is not approved; the model may still report
/// unresolved requirements.
#[derive(Serialize)]
pub struct CandidatePolicy {
    #[serde(flatten)]
    content: CandidateContent,
    candidate_hash: String,
}

impl CandidatePolicy {
    /// The model policy text.
    pub fn policy_text(&self) -> &str {
        &self.content.artifact.text
    }

    /// Validated Cedar artifact; it does not prove task completeness.
    pub fn artifact(&self) -> &Artifact {
        &self.content.artifact
    }

    /// Requirements the model reported as unresolved; do not hand them to approval as is.
    pub fn unresolved(&self) -> &[String] {
        self.content.draft.unresolved()
    }

    /// Digest over all serialized content except itself; not a signature.
    pub fn candidate_hash(&self) -> &str {
        &self.candidate_hash
    }
}

/// Candidate generation result; no branch means approval or effect.
#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ProposalOutcome {
    /// A valid Cedar candidate that keeps the raw draft and model-reported issues.
    Candidate {
        /// Unapproved candidate.
        candidate: Box<CandidatePolicy>,
    },
    /// The paths or unsupported policy features need rework; there is no approvable candidate.
    NeedsClarification {
        /// The raw model output is still preserved.
        draft: Box<PolicyDraft>,
        /// Reasons reported by the program checks.
        reasons: Vec<String>,
    },
}

/// Orchestration errors carry no task body or model output.
#[derive(Debug, Error)]
pub enum ProposalError {
    /// The server identity or timestamp is invalid.
    #[error("invalid proposal identity, timestamp or TTL")]
    InvalidContext,
    /// The overall deadline or the candidate TTL expired.
    #[error("proposal deadline exceeded")]
    DeadlineExceeded,
    /// The model call or the response wrapper is invalid.
    #[error("proposal generation failed: {0}")]
    Intent(#[from] IntentError),
    /// Real Cedar validation or candidate encoding failed.
    #[error("proposal validation failed: {0}")]
    Policy(#[from] PolicyError),
}

/// Generates a Cedar candidate directly from a task.
///
/// The model receives the task text only; no file contents are sent and no path metadata is
/// accepted. The draft keeps unresolved items; Strict validation does not prove that task and
/// policy are semantically equivalent.
///
/// # Errors
/// Returns no candidate when input, provider, Cedar syntax/types, budgets or the deadline fail.
pub fn propose_task(
    provider: &dyn ModelProvider,
    model: &str,
    task: &TaskInput,
    context: &ProposalContext,
    deadline: Instant,
) -> Result<ProposalOutcome, ProposalError> {
    let deadline = deadline
        .min(context.started + context.ttl)
        .min(Instant::now() + INTENT_BUDGET);
    check_deadline(deadline)?;
    let draft = generate_policy_draft(provider, model, task, deadline)?;
    let artifact = match prepare_policy(draft.policy()) {
        Ok(artifact) => artifact,
        Err(error @ (PolicyError::InvalidPath | PolicyError::UnsupportedPolicy)) => {
            check_deadline(deadline)?;
            return Ok(ProposalOutcome::NeedsClarification {
                draft: Box::new(draft),
                reasons: vec![error.to_string()],
            });
        }
        Err(error) => return Err(error.into()),
    };
    check_deadline(deadline)?;
    let payload = CandidateContent {
        format_version: CANDIDATE_VERSION,
        status: "CANDIDATE",
        authorization_required: true,
        context: context.clone(),
        artifact,
        draft,
        notes: vec![
            "CANDIDATE: not approved, not effective; not connected to enforcement.".into(),
            "Cedar Strict validates syntax and types, not task completeness. Review the policy against the original task; unresolved is model-reported, not a completeness proof.".into(),
            "Default deny without a matching permit; forbid takes precedence. A plain permit does not restrict other policies' allows.".into(),
            "file.read is file-content read, not listing names only. There is no implicit workspace or directory grant.".into(),
            "Enforcement must normalize paths, handle links and namespaces, and adjudicate both rename endpoints. Only the task text is sent to the model.".into(),
            "Scope/Binding, approval, expiry revocation and execution remain downstream; executable access does not grant its sub-operations.".into(),
        ],
    };
    let candidate_hash = canonical_hash(&payload)?;
    let candidate = CandidatePolicy {
        content: payload,
        candidate_hash,
    };
    if serde_json::to_vec(&candidate)
        .map_err(|_| PolicyError::Encoding)?
        .len()
        > MAX_CANDIDATE_BYTES - 128
    {
        return Err(PolicyError::TooLarge.into());
    }
    check_deadline(deadline)?;
    Ok(ProposalOutcome::Candidate {
        candidate: Box::new(candidate),
    })
}

fn check_deadline(deadline: Instant) -> Result<(), ProposalError> {
    if Instant::now() >= deadline {
        Err(ProposalError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn stable_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn canonical_hash(value: &impl Serialize) -> Result<String, PolicyError> {
    fn sort(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(fields) => {
                fields.sort_keys();
                for value in fields.values_mut() {
                    sort(value);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    sort(value);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(value).map_err(|_| PolicyError::Encoding)?;
    sort(&mut value);
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&value).map_err(|_| PolicyError::Encoding)?)
    ))
}
