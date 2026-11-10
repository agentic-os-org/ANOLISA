//! Generates unapproved Cedar candidates from a task or a hook command/cwd.
//! Base policy is never updated; permission mapping, PDP intersection and execution happen
//! downstream. The generation flow lives in `policy_generator.rs`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod intent_extraction;
mod policy_generator;
mod policy_summary;
mod policy_validation;

pub use intent_extraction::{
    CommandInput, DRAFT_VERSION, INTENT_BUDGET, IntentError, MAX_COMMAND_BYTES, MAX_CWD_BYTES,
    PROMPT_VERSION, PolicyDraft, PolicyResponse, TaskInput, generate_policy_draft, reasons_valid,
    validate_policy_response,
};
pub use policy_generator::{
    CANDIDATE_VERSION, CandidatePolicy, CommandPolicyOutcome, MAX_CANDIDATE_BYTES,
    MAX_PROPOSAL_TTL, ProposalContext, ProposalError, ProposalOutcome, propose_command,
    propose_task,
};
pub use policy_summary::{ActionCategory, PathScope, Permission, PolicySummary, summarize};
pub use policy_validation::{
    Artifact, CEDAR_SCHEMA, MAX_POLICY_BYTES, PolicyError, prepare_policy,
};
