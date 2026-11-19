//! Versioned deterministic-check protocol, deliberately distinct from AW Provider invocation.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::contract::{
    Binding, Budget, Candidate, Evidence, EvidenceGap, Scenario, Task, UnitAnswer,
};
use crate::outcome::{CandidateControl, Outcome, Receipt};

/// Independent experimental check contract; no AW handshake is implied.
pub const CHECK_API_VERSION: &str = "semantic-arbiter/v1alpha1";
/// Hard UTF-8 message bound, enforced before deserialization.
pub const MAX_MESSAGE_BYTES: usize = 262_144;
/// Hard evidence item bound, independent of caller budgets.
pub const MAX_EVIDENCE_ITEMS: usize = 64;

/// Stable errors without echoing task text, source content or credentials.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContractError {
    /// Message exceeded the hard protocol bound.
    #[error("message_too_large")]
    MessageTooLarge,
    /// Invalid JSON, duplicate/unknown fields or invalid enum representation.
    #[error("invalid_json")]
    InvalidJson,
    /// Unsupported independent protocol version.
    #[error("unsupported_version")]
    UnsupportedVersion,
    /// A typed field violated its semantic or size constraint.
    #[error("invalid_input")]
    InvalidInput,
    /// Correlation does not match the immutable input.
    #[error("binding_mismatch")]
    BindingMismatch,
    /// A result violated the recommendation or answer contract.
    #[error("invalid_response")]
    InvalidResponse,
}

/// Only deterministic checking of synthetic/offline input is implemented.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckMode {
    /// Recorded clock and asserted provenance, never live authorization.
    DeterministicCheck,
}

/// Frozen reference input; absence of a task is explicit rather than inferred.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckRequest {
    /// Exact independent protocol version.
    pub api_version: String,
    /// Consumer-owned request identifier.
    pub request_id: String,
    /// Explicitly distinguishes deterministic checking from live evaluation.
    pub mode: CheckMode,
    /// Immutable task/candidate/workspace/policy association.
    pub binding: Binding,
    /// Original event limits.
    pub budget: Budget,
    /// Recorded evaluation instant, not the current wall clock.
    pub check_at_ms: u64,
    /// Enabled reference scenarios; never inferred from available evidence.
    pub scenarios: Vec<Scenario>,
    /// Explicit authoritative task projection, or null when missing.
    pub task: Option<Task>,
    /// Structured resource operation.
    pub candidate: Candidate,
    /// Frozen evidence array delivered inline, not through implicit RPC.
    pub evidence: Vec<Evidence>,
    /// Explicit coverage omissions.
    pub gaps: Vec<EvidenceGap>,
}

/// Valid judgments may block; malformed envelopes are protocol errors instead.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResponse {
    /// Exact independent protocol version.
    pub api_version: String,
    /// Echoed immutable request identity.
    pub request_id: String,
    /// Explicit deterministic-check result.
    pub mode: CheckMode,
    /// Echoed context binding.
    pub binding: Binding,
    /// Digest of the typed input, not a provenance or authorization credential.
    pub input_digest: String,
    /// One validated answer for every enabled reference scenario.
    pub answers: Vec<UnitAnswer>,
    /// Recommendations only, without execution receipts.
    pub outcome: Outcome,
}

/// Machine-readable CLI capability statement; vocabulary is not native support.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Model-evaluation protocol version.
    pub evaluation_api_version: String,
    /// Deterministic checking retains its independent recorded-clock protocol.
    pub check_api_version: String,
    /// Implemented operation names.
    pub operations: Vec<String>,
    /// Implemented execution modes, neither of which executes recommendations.
    pub modes: Vec<String>,
    /// HTTP model backends available through explicit configuration.
    pub model_backends: Vec<String>,
    /// Implemented reference scenarios.
    pub scenarios: Vec<Scenario>,
    /// Complete public candidate-control contract vocabulary in restriction order.
    pub candidate_controls: Vec<CandidateControl>,
    /// Side-effect templates emitted as recommendations only.
    pub obligation_kinds: Vec<String>,
    /// Bound enforced before parsing any message.
    pub max_message_bytes: usize,
}

/// Return implemented capabilities and the complete public contract vocabulary.
pub fn capabilities() -> Capabilities {
    Capabilities {
        evaluation_api_version: crate::evaluation::EVALUATION_API_VERSION.into(),
        check_api_version: CHECK_API_VERSION.into(),
        operations: vec!["check".into(), "evaluate".into()],
        modes: vec!["deterministic_check".into(), "model_evaluation".into()],
        model_backends: vec!["bailian".into(), "typesafe".into()],
        scenarios: vec![Scenario::Preservation, Scenario::TaskScope],
        candidate_controls: vec![
            CandidateControl::Nop,
            CandidateControl::Allow,
            CandidateControl::Ask,
            CandidateControl::Block,
        ],
        obligation_kinds: vec!["snapshot".into(), "audit".into()],
        max_message_bytes: MAX_MESSAGE_BYTES,
    }
}

/// Decode one bounded request, rejecting unsupported versions and invalid bindings.
pub fn decode_request(bytes: &[u8]) -> Result<CheckRequest, ContractError> {
    let request = decode(bytes)?;
    validate_request(&request)?;
    Ok(request)
}

/// Decode and bind a response to the exact original request.
pub fn decode_response(
    bytes: &[u8],
    request: &CheckRequest,
) -> Result<CheckResponse, ContractError> {
    let response = decode(bytes)?;
    validate_response(&response, request)?;
    Ok(response)
}

/// Decode a receipt for a directive previously retained by the consumer.
pub fn decode_receipt(
    bytes: &[u8],
    request: &CheckRequest,
    outcome: &Outcome,
) -> Result<Receipt, ContractError> {
    let receipt = decode(bytes)?;
    crate::validation::receipt(&receipt, request, outcome)?;
    Ok(receipt)
}

/// Validate in-memory requests as strictly as decoded input.
pub fn validate_request(request: &CheckRequest) -> Result<(), ContractError> {
    crate::validation::request(request)
}

/// Validate response identity, answers, evidence references and typed outcomes.
pub fn validate_response(
    response: &CheckResponse,
    request: &CheckRequest,
) -> Result<(), ContractError> {
    crate::validation::response(response, request)
}

/// Validate a recommendation independently of the narrower check capabilities.
pub fn validate_outcome(outcome: &Outcome, request: &CheckRequest) -> Result<(), ContractError> {
    validate_request(request)?;
    crate::validation::outcome(outcome, request)
}

/// Hash declaration-order typed JSON with a versioned domain separator.
pub fn input_digest(request: &CheckRequest) -> Result<String, ContractError> {
    let bytes = serde_json::to_vec(request).map_err(|_| ContractError::InvalidInput)?;
    let mut hash = Sha256::new();
    hash.update(b"semantic-arbiter/v1alpha1\0check-input\0");
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ContractError> {
    crate::wire::decode(bytes)
}
