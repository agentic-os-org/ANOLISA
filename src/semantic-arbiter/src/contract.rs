//! Closed input and answer types; asserted provenance is not authentication.

use serde::{Deserialize, Serialize};

/// Correlation retained across evidence, judgments and execution receipts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Consumer-owned session identity.
    pub session_id: String,
    /// Stable instance identity, never a bare PID.
    pub instance_id: String,
    /// Consumer-verified workspace identity, not a path.
    pub workspace_id: String,
    /// Current task version, retained independently of history.
    pub task_version: String,
    /// Consumer policy revision.
    pub config_revision: String,
    /// Immutable candidate identity.
    pub candidate_id: String,
    /// Candidate revision changes whenever its inputs change.
    pub candidate_revision: String,
}

/// Authoritative task ingress; historical reports cannot select either variant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskOrigin {
    /// Explicit operator input.
    Operator,
    /// Verified native user input supplied by the consumer.
    NativeUser,
}

/// Explicit structured task constraints; free text never grants authority.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Session to which this task belongs.
    pub session_id: String,
    /// Version associated with the candidate binding.
    pub version: String,
    /// Consumer-verified ingress, synthetic in check fixtures.
    pub origin: TaskOrigin,
    /// Authoritative task text; the semantic units judge against it.
    pub text: String,
    /// Optional ceiling: when non-empty, every omitted resource is out of scope.
    pub allowed_resource_ids: Vec<String>,
    /// Optional retention limit overriding allowances; checked before any unit.
    pub preserve_resource_ids: Vec<String>,
}

/// Candidate operations understood by the offline reference evaluator.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Replace an existing resource.
    Replace,
    /// Remove an existing resource.
    Delete,
}

/// Frozen operation projection; check never resolves the resource on disk.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    /// Consumer-owned operation type.
    pub operation: Operation,
    /// Opaque resource identity under the bound workspace.
    pub resource_id: String,
    /// Full native call retained as untrusted data, never instructions or authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_tool: Option<NativeTool>,
}

/// Native tool invocation data; the arbiter never executes or resolves its arguments.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeTool {
    /// Nonblank native name, at most 128 UTF-8 bytes.
    pub name: String,
    /// Complete argument object, not an extracted command or path summary.
    pub input: serde_json::Value,
}

/// Maximum compact JSON byte length of the complete native argument object.
pub const MAX_NATIVE_INPUT_BYTES: usize = 65_536;

/// Validate and hash the complete candidate as domain-separated, recursively key-sorted JSON.
pub fn candidate_revision(candidate: &Candidate) -> Result<String, crate::protocol::ContractError> {
    use sha2::{Digest, Sha256};

    crate::validation::candidate(candidate)?;
    let value = serde_json::to_value(candidate)
        .map_err(|_| crate::protocol::ContractError::InvalidInput)?;
    let mut bytes = Vec::new();
    canonical_json(&value, &mut bytes)?;
    let mut hash = Sha256::new();
    hash.update(b"semantic-arbiter/v1alpha1\0candidate\0");
    hash.update(bytes);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn canonical_json(
    value: &serde_json::Value,
    bytes: &mut Vec<u8>,
) -> Result<(), crate::protocol::ContractError> {
    use crate::protocol::ContractError;
    use serde_json::Value;

    match value {
        Value::Object(object) => {
            // Explicit sorting keeps revisions independent of serde_json's map feature flags.
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
            bytes.push(b'{');
            for (index, (key, value)) in entries.into_iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                serde_json::to_writer(&mut *bytes, key).map_err(|_| ContractError::InvalidInput)?;
                bytes.push(b':');
                canonical_json(value, bytes)?;
            }
            bytes.push(b'}');
        }
        Value::Array(array) => {
            bytes.push(b'[');
            for (index, value) in array.iter().enumerate() {
                if index != 0 {
                    bytes.push(b',');
                }
                canonical_json(value, bytes)?;
            }
            bytes.push(b']');
        }
        _ => serde_json::to_writer(bytes, value).map_err(|_| ContractError::InvalidInput)?,
    }
    Ok(())
}

/// Reference scenarios, not a dynamic registration API.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    /// Protect existing results.
    Preservation,
    /// Keep an operation within the authoritative task.
    TaskScope,
}

/// Evidence provenance retained without promotion to authorization.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Consumer-verified local byte comparison; only asserted in check.
    LocalVerifier,
    /// Unverified report from an external observation source.
    SourceReported,
    /// Copied or generated model content.
    ModelEcho,
}

/// Result of a controlled rebuild comparison, not a model prediction.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RebuildResult {
    /// Retained inputs and generator produced identical bytes.
    Identical,
    /// Existing bytes include results not reproduced by the retained recipe.
    Unique,
    /// Comparison could not establish either result.
    Unknown,
}

/// Small typed evidence vocabulary for the reference check path.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    tag = "kind",
    content = "parameters",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum EvidenceFact {
    /// Bound byte-comparison evidence.
    Rebuild {
        /// Resource compared by the local verifier.
        resource_id: String,
        /// Comparison outcome.
        result: RebuildResult,
    },
}

/// Immutable, bounded material with explicit coverage and lifetime.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// Unique reference used by answers and outcomes.
    pub id: String,
    /// Registered source identity.
    pub source_id: String,
    /// Source content revision.
    pub source_revision: String,
    /// Source/collection trust annotation, not a credential.
    pub provenance: Provenance,
    /// Exact task, candidate and policy association.
    pub binding: Binding,
    /// Collection time in the recorded clock domain.
    pub captured_at_ms: u64,
    /// Exclusive validity deadline in the same clock domain.
    pub valid_until_ms: u64,
    /// Typed fact; no executable text or commands.
    pub fact: EvidenceFact,
}

/// Stable, non-sensitive reasons suitable for audit metadata.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// All enabled reference checks completed without objection.
    Allowed,
    /// Explicit retention requirement prevents modification.
    Preserved,
    /// Unique results require a confirmed recovery point before execution.
    NeedsSnapshot,
    /// A declared allowance omits this resource, or a unit judged it out of scope.
    OutsideScope,
    /// No authoritative task was supplied.
    MissingTask,
    /// Required evidence was not supplied.
    MissingEvidence,
    /// Required evidence expired or was collected after the check instant.
    StaleEvidence,
    /// Reports or model echoes cannot prove local reconstruction.
    UntrustedEvidence,
    /// Qualified evidence gives incompatible results.
    ConflictingEvidence,
    /// Supplied material cannot establish the answer.
    Undetermined,
    /// The original event budget is exhausted.
    BudgetExceeded,
    /// The declared evidence budget is exhausted.
    EvidenceBudgetExceeded,
    /// Required coverage is incomplete.
    EvidenceGap,
}

/// A source coverage gap remains visible even when other evidence is available.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceGap {
    /// Source reporting the gap.
    pub source_id: String,
    /// Reference unit for which material is missing.
    pub scenario: Scenario,
}

/// Original event budget, never renewed by an evaluator.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Budget {
    /// Event opening time in the recorded clock domain.
    pub opened_at_ms: u64,
    /// Exclusive event deadline.
    pub deadline_ms: u64,
    /// Evidence item limit, at most the protocol maximum.
    pub max_evidence_items: usize,
    /// Serialized UTF-8 evidence-array limit.
    pub max_evidence_bytes: usize,
}

/// Closed reference answer vocabulary; models cannot produce actions.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerValue {
    /// No objection from this unit.
    Allow,
    /// Explicit retention constraint.
    Preserve,
    /// Permitted modification requires protection.
    NeedsSnapshot,
    /// Outside explicit task scope.
    OutsideScope,
    /// Required judgment could not be formed.
    Unknown,
}

/// Unit result kept separate from the selected action template.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UnitAnswer {
    /// Reference unit identity.
    pub unit: Scenario,
    /// Typed answer, not an action supplied by a model.
    pub value: AnswerValue,
    /// Integer millionths in 0..=1_000_000; no floating-point coercion.
    pub confidence_ppm: u32,
    /// Non-sensitive explanation code.
    pub reason: Reason,
    /// References into the current frozen evidence only.
    pub evidence_refs: Vec<String>,
}
