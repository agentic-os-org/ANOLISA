//! Generates unapproved Cedar candidates directly from a task and the shared schema; never
//! approves or enforces a policy.
//! The generation flow lives in `policy_generator.rs`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod intent_extraction;
mod policy_generator;
mod policy_validation;

pub use intent_extraction::{
    DRAFT_VERSION, INTENT_BUDGET, IntentError, PROMPT_VERSION, PolicyDraft, PolicyResponse,
    TaskInput, generate_policy_draft, validate_policy_response,
};
pub use policy_generator::{
    CANDIDATE_VERSION, CandidatePolicy, MAX_CANDIDATE_BYTES, MAX_PROPOSAL_TTL, ProposalContext,
    ProposalError, ProposalOutcome, propose_task,
};
pub use policy_validation::{
    Artifact, CEDAR_SCHEMA, MAX_POLICY_BYTES, PolicyError, prepare_policy,
};
