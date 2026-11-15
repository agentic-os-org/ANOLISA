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
//! Tasks and single commands only create new candidates; existing policies are neither read nor
//! rewritten, and permission mapping, intersection and execution stay downstream.

use std::time::{Duration, Instant};

use asc_model_client::provider::ModelProvider;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    Artifact, INTENT_BUDGET, IntentError, PolicyDraft, PolicyError, TaskInput, annotated_forbids,
    intent_extraction::generate_policy_draft_after, launch_view_gaps, point_aliases,
    prepare_policy,
};

/// Maximum candidate TTL; it is not the lifetime of an issued grant.
pub const MAX_PROPOSAL_TTL: u32 = 1800;
/// Serialization budget for a complete candidate.
pub const MAX_CANDIDATE_BYTES: usize = 512 * 1024;
/// Direct Cedar candidate protocol version.
pub const CANDIDATE_VERSION: &str = "asc.intent.candidate.v1";
/// Prefix of the note listing permits that the launch-time authorization view drops.
pub const LAUNCH_VIEW_NOTE: &str = "LAUNCH VIEW:";
/// Prefix of the note listing point grants that name a link on the generating host.
pub const POINT_ALIAS_NOTE: &str = "POINT ALIAS:";
/// Prefix of the note listing forbids that the runtime protector refuses to lower.
pub const RUNTIME_VIEW_NOTE: &str = "RUNTIME VIEW:";

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

/// Cedar derived for one command; not an approval or an effective permission.
#[derive(Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum CommandPolicyOutcome {
    /// Passed the Cedar syntax, path and Strict-type checks; completeness is still unproven.
    Candidate {
        /// Raw Cedar text with no added rules; an empty string requests no permission.
        policy: String,
        /// Known but uncovered parts, such as an unresolved PATH lookup or script behavior; never
        /// treat them as authorized.
        caveats: Vec<String>,
    },
    /// Required information or expressiveness is missing; no partial policy is returned, so it
    /// cannot be mistaken for a complete request.
    NeedsClarification {
        /// Reasons reported by the model or found by the program checks.
        reasons: Vec<String>,
    },
}

/// Maximum model attempts for one command proposal; a single retry repairs the syntax and
/// Strict-typing mistakes models make often enough that failing the hook is worse.
const COMMAND_ATTEMPTS: u8 = 2;

/// Generates Cedar from a hook command/cwd; it reads no base policy and computes no effective
/// permission.
///
/// Reads no file and runs no command. Only a model-reported unresolved item yields no candidate;
/// caveats travel with the candidate. Syntax, Strict-typing and path-literal rejections are
/// retried once with the local reason; unsupported-policy features are never retried. After the
/// retry a syntax or typing failure is an error, while a persistent path rejection becomes a
/// clarification. Validation proves Cedar syntax and types only, never command completeness or
/// minimality.
///
/// # Errors
/// Returns an error for invalid input or model output, invalid Cedar syntax or types, and
/// timeouts or exceeded limits.
pub fn propose_command(
    provider: &dyn ModelProvider,
    model: &str,
    input: &crate::CommandInput,
    deadline: Instant,
) -> Result<CommandPolicyOutcome, ProposalError> {
    let deadline = deadline.min(Instant::now() + INTENT_BUDGET);
    check_deadline(deadline)?;
    let mut attempt = 1;
    let mut rejection: Option<String> = None;
    loop {
        let response = crate::intent_extraction::generate_command_response(
            provider,
            model,
            input,
            deadline,
            rejection.as_deref(),
        )?;
        if !response.unresolved.is_empty() {
            check_deadline(deadline)?;
            return Ok(CommandPolicyOutcome::NeedsClarification {
                reasons: response.unresolved,
            });
        }
        // An empty policy is the answer for a command with no file, process or network action:
        // it grants nothing, so `prepare_policy` admits it and the caveats stay attached.
        match prepare_policy(&response.policy) {
            Ok(artifact) => {
                check_deadline(deadline)?;
                return Ok(CommandPolicyOutcome::Candidate {
                    policy: artifact.text,
                    caveats: response.caveats,
                });
            }
            // A rejected draft is retried once with the local reason, so the model can repair a
            // non-canonical path, a wildcard path literal or a schema mismatch instead of the run
            // terminating on it.
            Err(
                error @ (PolicyError::InvalidPath
                | PolicyError::WildcardLiteral
                | PolicyError::Syntax
                | PolicyError::Validation),
            ) if attempt < COMMAND_ATTEMPTS => {
                check_deadline(deadline)?;
                rejection = Some(error.to_string());
                attempt += 1;
            }
            // After the single retry, a persistent path rejection describes the command, not a
            // transient mistake, so it becomes a clarification reason rather than an error.
            Err(
                error @ (PolicyError::InvalidPath
                | PolicyError::WildcardLiteral
                | PolicyError::UnsupportedPolicy),
            ) => {
                check_deadline(deadline)?;
                return Ok(CommandPolicyOutcome::NeedsClarification {
                    reasons: vec![error.to_string()],
                });
            }
            Err(error) => return Err(error.into()),
        }
    }
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
/// A valid draft whose permits test `context` is retried once with that reason, because the
/// launch authorizer reads the same revision without context and would grant those permits
/// nothing. A forbid carrying annotations is retried the same way, because the runtime protector
/// refuses it and fails the whole binding. The retry only replaces the first draft when it is
/// valid and has neither; otherwise the first draft stays the candidate and the findings travel
/// as `LAUNCH VIEW:` and `RUNTIME VIEW:` notes.
///
/// The exact paths the final permits name are then resolved on this host (metadata only; nothing
/// is sent). A point grant on a link authorizes nothing, but it is not retried: the task asked
/// for that name, and the repair is a wider grant only a reviewer may choose. It travels as a
/// `POINT ALIAS:` note instead.
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
    let mut draft = generate_policy_draft_after(provider, model, task, deadline, None)?;
    let mut artifact = match prepare_policy(draft.policy()) {
        Ok(artifact) => artifact,
        Err(
            error @ (PolicyError::InvalidPath
            | PolicyError::WildcardLiteral
            | PolicyError::UnsupportedPolicy),
        ) => {
            check_deadline(deadline)?;
            return Ok(ProposalOutcome::NeedsClarification {
                draft: Box::new(draft),
                reasons: vec![error.to_string()],
            });
        }
        Err(error) => return Err(error.into()),
    };
    let mut gaps = launch_view_gaps(&artifact.text)?;
    let mut annotated = annotated_forbids(&artifact.text)?;
    if !gaps.is_empty() || !annotated.is_empty() {
        let mut reasons = Vec::new();
        if !gaps.is_empty() {
            reasons.push(format!(
                "the launch authorizer reads no context, so these permits would grant nothing: \
                 {}; express permits with resource attributes only and keep \
                 context.input.operation for forbids",
                gaps.join("; ")
            ));
        }
        if !annotated.is_empty() {
            reasons.push(format!(
                "the runtime protector refuses annotated forbids and fails the whole binding: \
                 {}; write forbids without annotations and describe them in comments",
                annotated.join("; ")
            ));
        }
        let rejection = reasons.join(". ");
        // A failed or still-gapped retry must not cost the valid first draft.
        if let Ok(retry) =
            generate_policy_draft_after(provider, model, task, deadline, Some(&rejection))
            && let Ok(repaired) = prepare_policy(retry.policy())
            && launch_view_gaps(&repaired.text).is_ok_and(|left| left.is_empty())
            && annotated_forbids(&repaired.text).is_ok_and(|left| left.is_empty())
        {
            draft = retry;
            artifact = repaired;
            gaps.clear();
            annotated.clear();
        }
    }
    check_deadline(deadline)?;
    let aliases = point_aliases(&artifact.text, &|path| {
        std::fs::canonicalize(path)
            .ok()?
            .into_os_string()
            .into_string()
            .ok()
    })?;
    let notes = candidate_notes(&gaps, &annotated, &aliases);
    let payload = CandidateContent {
        format_version: CANDIDATE_VERSION,
        status: "CANDIDATE",
        authorization_required: true,
        context: context.clone(),
        artifact,
        draft,
        notes,
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

/// Fixed review notes plus one note per finding a consumer of this revision would trip over.
fn candidate_notes(gaps: &[String], annotated: &[String], aliases: &[String]) -> Vec<String> {
    let mut notes: Vec<String> = vec![
        "CANDIDATE: not approved, not effective; not connected to enforcement.".into(),
        "Cedar Strict validates syntax and types, not task completeness. Review the policy against the original task; unresolved is model-reported, not a completeness proof.".into(),
        "Default deny without a matching permit; forbid takes precedence. A plain permit does not restrict other policies' allows.".into(),
        "file.read is file-content read, not listing names only. There is no implicit workspace or directory grant.".into(),
        "Enforcement must normalize paths, handle links and namespaces, and adjudicate both rename endpoints. Only the task text is sent to the model.".into(),
        "Scope/Binding, approval, expiry revocation and execution remain downstream; executable access does not grant its sub-operations.".into(),
    ];
    if !gaps.is_empty() {
        notes.push(format!(
            "{LAUNCH_VIEW_NOTE} the launch authorizer drops permits that test context, so they \
             authorize no launch: {}.",
            gaps.join("; ")
        ));
    }
    if !annotated.is_empty() {
        notes.push(format!(
            "{RUNTIME_VIEW_NOTE} the runtime protector refuses annotated forbids, so a Scope on \
             this revision fails with UNSUPPORTED_ANNOTATION: {}.",
            annotated.join("; ")
        ));
    }
    if !aliases.is_empty() {
        notes.push(format!(
            "{POINT_ALIAS_NOTE} the authorizer compares canonical paths and the sandbox refuses \
             a point grant on a link, so on this host these grants match nothing and a launch \
             whose request covers them fails: {}. Grant the target as a directory pair or keep \
             the requirement unresolved.",
            aliases.join("; ")
        ));
    }
    notes
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
