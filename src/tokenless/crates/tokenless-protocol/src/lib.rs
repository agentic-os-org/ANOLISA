//! Protocol v2 lifecycle transport shared by Tokenless frontends.
//!
//! In-process callers use the operation-specific payload types directly.
//! [`RequestEnvelope`] and [`ResponseEnvelope`] exist only for CLI and other
//! cross-process transports.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The protocol version implemented by this crate.
pub const PROTOCOL_VERSION: u32 = 2;
/// Identity of the normalized token estimator used by Tokenless.
pub const TOKENIZER_ID: &str = "heuristic-v1";
/// Identity of the byte-length fallback used before a text scan is possible.
pub const BYTE_ESTIMATOR_ID: &str = "byte-length-v1";
/// Maximum diagnostic length emitted by lifecycle operations.
pub const DIAGNOSTIC_MAX_BYTES: usize = 4096;

/// Estimates normalized tokens using [`TOKENIZER_ID`].
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for character in text.chars() {
        if is_cjk(character) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    cjk + other.div_ceil(4)
}

/// Estimates normalized tokens from a byte count.
#[must_use]
pub fn estimate_tokens_from_bytes(bytes: usize) -> usize {
    bytes.div_ceil(4)
}

/// Counts Unicode scalar values in `text`.
#[must_use]
pub fn count_chars(text: &str) -> usize {
    text.chars().count()
}

fn is_cjk(character: char) -> bool {
    matches!(character,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2A6DF}'
        | '\u{2A700}'..='\u{2B73F}'
        | '\u{2B740}'..='\u{2B81F}'
        | '\u{2B820}'..='\u{2CEAF}'
        | '\u{2CEB0}'..='\u{2EBEF}'
        | '\u{30000}'..='\u{3134F}'
        | '\u{3100}'..='\u{312F}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{3040}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
    )
}

/// Error returned for an invalid protocol transport.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The payload declares an unsupported protocol version.
    #[error("unsupported protocol_version {found} (supported: {PROTOCOL_VERSION})")]
    UnsupportedVersion {
        /// Version found in the payload.
        found: u32,
    },
    /// The JSON does not match the selected operation shape.
    #[error("malformed protocol payload: {0}")]
    Malformed(#[from] serde_json::Error),
    /// The response operation differs from the request operation.
    #[error("response operation {found:?} does not match request operation {expected:?}")]
    OperationMismatch {
        /// Operation selected by the request.
        expected: Operation,
        /// Operation returned in the response.
        found: Operation,
    },
    /// Serialization failed.
    #[error("protocol serialization failed: {0}")]
    Serialize(#[source] serde_json::Error),
    /// A host recovery-tool identifier cannot safely appear in an instruction.
    #[error("recovery tool name must contain 1–64 ASCII letters, digits, '_' or '-': {0:?}")]
    InvalidRecoveryToolName(String),
}

/// Lifecycle operation carried by a transport envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    /// Transform model-bound tool declarations.
    BeforeModel,
    /// Rewrite tool arguments before execution.
    PreTool,
    /// Process one completed tool result.
    PostTool,
    /// Restore one visible stash marker.
    Retrieve,
}

impl Operation {
    /// Returns the stable wire name.
    #[must_use]
    pub fn wire_str(self) -> &'static str {
        match self {
            Self::BeforeModel => "before_model",
            Self::PreTool => "pre_tool",
            Self::PostTool => "post_tool",
            Self::Retrieve => "retrieve",
        }
    }
}

/// Attribution shared by lifecycle operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attribution {
    /// Stable agent or adapter identifier.
    pub agent_id: String,
    /// Conversation identifier when supplied by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Tool-call identifier when supplied by the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl Attribution {
    /// Creates attribution with only an agent identifier.
    #[must_use]
    pub fn new(agent_id: impl Into<String>) -> Self {
        Self {
            agent_id: agent_id.into(),
            session_id: None,
            tool_use_id: None,
        }
    }
}

/// Recovery entry point available to the model, independent of compression policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RecoveryMethod {
    /// No reachable recovery entry point.
    #[default]
    None,
    /// The host can run the trusted local CLI through its existing shell tool.
    Shell,
    /// A statically registered tool enforces current-reference authorization.
    Tool {
        /// Actual host tool name; Core does not register or rename it.
        name: RecoveryToolName,
    },
}

impl<'de> Deserialize<'de> for RecoveryMethod {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Empty struct variants reject fields that Serde's tagged unit variants ignore.
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
        enum Wire {
            None {},
            Shell {},
            Tool { name: RecoveryToolName },
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::None {} => Self::None,
            Wire::Shell {} => Self::Shell,
            Wire::Tool { name } => Self::Tool { name },
        })
    }
}

impl RecoveryMethod {
    /// Whether a caller has declared a reachable recovery entry point.
    #[must_use]
    pub fn is_available(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// Declares an existing, marker-authorized host tool.
    ///
    /// # Errors
    /// Returns an error when the name is not 1–64 ASCII letters, digits, `_`, or `-`.
    pub fn tool(name: impl Into<String>) -> Result<Self, ProtocolError> {
        Ok(Self::Tool {
            name: name.into().try_into()?,
        })
    }
}

/// Validated host tool identifier, safe to embed in a recovery instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RecoveryToolName(String);

impl RecoveryToolName {
    /// Returns the host tool's unchanged name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RecoveryToolName {
    type Error = ProtocolError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        if !(1..=64).contains(&name.len())
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        {
            return Err(ProtocolError::InvalidRecoveryToolName(name));
        }
        Ok(Self(name))
    }
}

impl From<RecoveryToolName> for String {
    fn from(name: RecoveryToolName) -> Self {
        name.0
    }
}

/// Host capabilities relevant to BeforeModel.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeforeModelCapabilities {
    /// The host can replace tool declarations.
    #[serde(default)]
    pub replace_tools: bool,
    /// Only an authorized static tool permits recoverable schema truncation.
    pub recovery: RecoveryMethod,
}

/// Input for the BeforeModel lifecycle operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeforeModelRequest {
    /// Tool declarations visible to the next model call.
    pub tools: Vec<Value>,
    /// Model-visible request context with tool declarations removed.
    pub visible_context: Value,
    /// Host capabilities for applying the result.
    pub capabilities: BeforeModelCapabilities,
}

/// Result of the BeforeModel lifecycle operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeforeModelResponse {
    /// Tool declarations to send to the model.
    pub tools: Vec<Value>,
    /// Sorted, deduplicated lowercase markers visible to the model.
    pub visible_markers: Vec<String>,
}

/// Host capabilities relevant to PreTool.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreToolCapabilities {
    /// The host can replace arguments before execution.
    #[serde(default)]
    pub replace_arguments: bool,
    /// The host can block this call and suggest a retry.
    #[serde(default)]
    pub block_and_suggest: bool,
}

/// Input for the PreTool lifecycle operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreToolRequest {
    /// Name of the tool about to run.
    pub tool_name: String,
    /// Original host arguments.
    pub arguments: Value,
    /// Object field that contains the command string.
    pub command_field: String,
    /// Host capabilities for applying a rewrite.
    pub capabilities: PreToolCapabilities,
}

/// Action the adapter must take after PreTool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreToolAction {
    /// Execute the original arguments.
    Passthrough,
    /// Replace the call arguments directly.
    ReplaceArguments,
    /// Block the call and ask the model to retry with the returned arguments.
    BlockAndSuggest,
}

/// Optimization already applied to a tool's eventual output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputOptimization {
    /// No earlier optimization is known.
    None,
    /// RTK rewrote the command and owns the resulting output shape.
    Rtk,
}

/// Result of the PreTool lifecycle operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreToolResponse {
    /// Arguments to execute or suggest.
    pub arguments: Value,
    /// Host action selected from declared capabilities.
    pub action: PreToolAction,
    /// Optimization state to carry into PostTool.
    pub output_optimization: OutputOptimization,
}

/// Whether PostTool is processing an ordinary or Retrieve result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultKind {
    /// An ordinary tool result.
    Tool,
    /// Output returned by an agent-facing Retrieve operation.
    Retrieve,
}

/// Host-reported status of a tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    /// The tool completed successfully.
    Success,
    /// The tool completed with an error result.
    Error,
    /// The host interrupted execution.
    Interrupted,
    /// The host denied execution.
    Denied,
}

/// Origin of model-visible PostTool content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentOrigin {
    /// Output produced by executing a command.
    CommandOutput,
    /// A copy of authoritative file content.
    FileContent,
    /// A service or framework response.
    ApiResponse,
}

impl ContentOrigin {
    /// Returns the stable wire name.
    #[must_use]
    pub fn wire_str(self) -> &'static str {
        match self {
            Self::CommandOutput => "command_output",
            Self::FileContent => "file_content",
            Self::ApiResponse => "api_response",
        }
    }
}

/// Host capabilities relevant to PostTool.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostToolCapabilities {
    /// The host can replace the model-visible output.
    #[serde(default)]
    pub replace_output: bool,
    /// Recovery entry point used by the final candidate's instructions.
    pub recovery: RecoveryMethod,
    /// The replacement slot accepts arbitrary text.
    #[serde(default)]
    pub replace_with_text: bool,
}

/// Input for the PostTool lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostToolRequest {
    /// Whether this is an ordinary tool or Retrieve result.
    pub result_kind: ResultKind,
    /// Name of the tool that produced the result.
    /// The exact name `Grep` restricts Core to search path sharing.
    pub tool_name: String,
    /// Model-visible result content.
    pub content: String,
    /// Host-reported execution status.
    pub status: ToolResultStatus,
    /// Authoritative origin selected by the adapter.
    pub content_origin: ContentOrigin,
    /// Optimization state returned by PreTool.
    pub output_optimization: OutputOptimization,
    /// Host capabilities for applying the result.
    pub capabilities: PostToolCapabilities,
}

/// Final PostTool disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// A candidate replaced the original result.
    Applied,
    /// A candidate was measured but the original result was returned.
    DryRun,
    /// Policy did not select a compressor.
    Passthrough,
    /// A candidate did not save both characters and tokens.
    NoSavings,
    /// A lossy candidate was rejected because no marker-authorized recovery path exists.
    RecoverabilityUnavailable,
    /// The pipeline exhausted its time budget.
    Timeout,
    /// The operation produced a bounded tool-error diagnostic.
    ToolError,
}

impl Disposition {
    /// Returns the stable wire name.
    #[must_use]
    pub fn wire_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::DryRun => "dry_run",
            Self::Passthrough => "passthrough",
            Self::NoSavings => "no_savings",
            Self::RecoverabilityUnavailable => "recoverability_unavailable",
            Self::Timeout => "timeout",
            Self::ToolError => "tool_error",
        }
    }
}

/// Detected PostTool content domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentType {
    /// JSON object or array.
    Json,
    /// Search-result listing.
    SearchResults,
    /// Compiler, package-manager, or test output.
    BuildLog,
    /// Stack trace or panic report.
    StackTrace,
    /// Unified diff.
    Diff,
    /// Complete HTML document.
    Html,
    /// Delimiter-consistent table.
    Tabular,
    /// Program source code.
    SourceCode,
    /// Readable text without a more specific domain.
    PlainText,
    /// Empty, binary-like, or unclassified content.
    Unknown,
}

impl ContentType {
    /// Returns the stable wire name.
    #[must_use]
    pub fn wire_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::SearchResults => "search_results",
            Self::BuildLog => "build_log",
            Self::StackTrace => "stack_trace",
            Self::Diff => "diff",
            Self::Html => "html",
            Self::Tabular => "tabular",
            Self::SourceCode => "source_code",
            Self::PlainText => "plain_text",
            Self::Unknown => "unknown",
        }
    }
}

/// Concrete transformations applied to an emitted result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppliedOperation {
    /// Function schema descriptions were compacted.
    SchemaCompression,
    /// ANSI SGR presentation sequences were removed from a build log.
    TerminalCleanup,
    /// Repeated build/test progress was reduced behind retrieval markers.
    BuildLogReduction,
    /// CSV/TSV quoting and record terminators changed with all cells retained.
    TabularCompaction,
    /// CSV/TSV rows were omitted behind a reference to the original text.
    TabularRowReduction,
    /// Consecutive search records share a full path without removing source text.
    SearchPathSharing,
    /// Unmodified Git diff context was reduced with the original available in Stash.
    DiffReduction,
    /// Empty and diagnostic JSON fields were removed.
    JsonCleanup,
    /// A JSON record collection was reduced with retrievable omissions.
    JsonRecordReduction,
    /// Oversized JSON values were truncated.
    JsonTruncation,
    /// JSON was encoded as TOON.
    Toon,
}

impl AppliedOperation {
    /// Returns the stable wire name.
    #[must_use]
    pub fn wire_str(self) -> &'static str {
        match self {
            Self::SchemaCompression => "schema_compression",
            Self::TerminalCleanup => "terminal_cleanup",
            Self::BuildLogReduction => "build_log_reduction",
            Self::TabularCompaction => "tabular_compaction",
            Self::TabularRowReduction => "tabular_row_reduction",
            Self::SearchPathSharing => "search_path_sharing",
            Self::DiffReduction => "diff_reduction",
            Self::JsonCleanup => "json_cleanup",
            Self::JsonRecordReduction => "json_record_reduction",
            Self::JsonTruncation => "json_truncation",
            Self::Toon => "toon",
        }
    }
}

/// Recovery state of an applied transformation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Recoverability {
    /// Nothing task-relevant was removed.
    Lossless,
    /// Removed bytes are available through emitted stash markers.
    Retrievable,
    /// Removed bytes have no recovery path.
    Unrecoverable,
}

/// Result of the PostTool lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PostToolResponse {
    /// Text the adapter must emit.
    pub output: String,
    /// Pipeline or routing decision.
    pub disposition: Disposition,
    /// Detected domain when content detection ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<ContentType>,
    /// Transformations that shaped `output`, in execution order.
    #[serde(default)]
    pub applied_operations: Vec<AppliedOperation>,
    /// Recovery state of `output`.
    pub recoverability: Recoverability,
    /// Estimated tokens in the input.
    pub before_tokens: u64,
    /// Estimated tokens in the emitted or measured candidate.
    pub after_tokens: u64,
    /// Stash keys referenced by the emitted output.
    #[serde(default)]
    pub stash_keys: Vec<String>,
    /// Token estimator identity for both counts.
    pub tokenizer_id: String,
    /// Additive context for a tool error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_context: Option<String>,
}

impl PostToolResponse {
    /// Builds the canonical unchanged response.
    #[must_use]
    pub fn passthrough(request: &PostToolRequest, before_tokens: u64) -> Self {
        Self {
            output: request.content.clone(),
            disposition: Disposition::Passthrough,
            content_type: None,
            applied_operations: Vec::new(),
            recoverability: Recoverability::Lossless,
            before_tokens,
            after_tokens: before_tokens,
            stash_keys: Vec::new(),
            tokenizer_id: TOKENIZER_ID.to_owned(),
            additional_context: None,
        }
    }

    /// Returns whether the output replaced the input.
    #[must_use]
    pub fn is_applied(&self) -> bool {
        self.disposition == Disposition::Applied
    }
}

/// Input for an authorized Retrieve lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieveRequest {
    /// Bare hash or text containing one stash marker.
    pub hash_or_marker: String,
    /// Markers visible to the model at the time of retrieval.
    pub visible_markers: Vec<String>,
}

/// Result of an authorized Retrieve lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetrieveResponse {
    /// Normalized lowercase stash hash.
    pub hash: String,
    /// Byte-exact stashed payload.
    pub payload: String,
}

/// Operation-specific request carried by [`RequestEnvelope`].
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// BeforeModel payload.
    BeforeModel(BeforeModelRequest),
    /// PreTool payload.
    PreTool(PreToolRequest),
    /// PostTool payload.
    PostTool(PostToolRequest),
    /// Retrieve payload.
    Retrieve(RetrieveRequest),
}

impl Request {
    /// Returns the operation selected by this payload.
    #[must_use]
    pub fn operation(&self) -> Operation {
        match self {
            Self::BeforeModel(_) => Operation::BeforeModel,
            Self::PreTool(_) => Operation::PreTool,
            Self::PostTool(_) => Operation::PostTool,
            Self::Retrieve(_) => Operation::Retrieve,
        }
    }
}

/// Operation-specific response carried by [`ResponseEnvelope`].
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    /// BeforeModel result.
    BeforeModel(BeforeModelResponse),
    /// PreTool result.
    PreTool(PreToolResponse),
    /// PostTool result.
    PostTool(PostToolResponse),
    /// Retrieve result.
    Retrieve(RetrieveResponse),
}

impl Response {
    /// Returns the operation selected by this result.
    #[must_use]
    pub fn operation(&self) -> Operation {
        match self {
            Self::BeforeModel(_) => Operation::BeforeModel,
            Self::PreTool(_) => Operation::PreTool,
            Self::PostTool(_) => Operation::PostTool,
            Self::Retrieve(_) => Operation::Retrieve,
        }
    }
}

/// Protocol v2 request transport.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestEnvelope {
    /// Request attribution.
    pub attribution: Attribution,
    /// Operation-specific input.
    pub request: Request,
}

impl RequestEnvelope {
    /// Parses a strict v2 request envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] for unsupported versions, unknown fields, or
    /// a payload that does not match its operation.
    pub fn from_json(json: &str) -> Result<Self, ProtocolError> {
        check_version(json)?;
        let raw: RawRequestEnvelope = serde_json::from_str(json)?;
        let request = match raw.operation {
            Operation::BeforeModel => Request::BeforeModel(serde_json::from_value(raw.input)?),
            Operation::PreTool => Request::PreTool(serde_json::from_value(raw.input)?),
            Operation::PostTool => Request::PostTool(serde_json::from_value(raw.input)?),
            Operation::Retrieve => Request::Retrieve(serde_json::from_value(raw.input)?),
        };
        Ok(Self {
            attribution: raw.attribution,
            request,
        })
    }

    /// Serializes the request to the fixed v2 wire shape.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Serialize`] if serialization fails.
    pub fn to_json(&self) -> Result<String, ProtocolError> {
        serialize_envelope(
            self.request.operation(),
            &self.attribution,
            request_value(&self.request)?,
            "input",
        )
    }
}

/// Protocol v2 response transport.
#[derive(Debug, Clone, PartialEq)]
pub struct ResponseEnvelope {
    /// Response attribution copied from the request.
    pub attribution: Attribution,
    /// Operation-specific result.
    pub response: Response,
}

impl ResponseEnvelope {
    /// Parses a strict v2 response envelope.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] for unsupported versions, unknown fields, or
    /// a payload that does not match its operation.
    pub fn from_json(json: &str) -> Result<Self, ProtocolError> {
        check_version(json)?;
        let raw: RawResponseEnvelope = serde_json::from_str(json)?;
        let response = match raw.operation {
            Operation::BeforeModel => Response::BeforeModel(serde_json::from_value(raw.result)?),
            Operation::PreTool => Response::PreTool(serde_json::from_value(raw.result)?),
            Operation::PostTool => Response::PostTool(serde_json::from_value(raw.result)?),
            Operation::Retrieve => Response::Retrieve(serde_json::from_value(raw.result)?),
        };
        Ok(Self {
            attribution: raw.attribution,
            response,
        })
    }

    /// Serializes the response to the fixed v2 wire shape.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::Serialize`] if serialization fails.
    pub fn to_json(&self) -> Result<String, ProtocolError> {
        serialize_envelope(
            self.response.operation(),
            &self.attribution,
            response_value(&self.response)?,
            "result",
        )
    }

    /// Verifies that the response operation matches its request.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::OperationMismatch`] on a mismatch.
    pub fn ensure_operation(&self, expected: Operation) -> Result<(), ProtocolError> {
        let found = self.response.operation();
        if found == expected {
            Ok(())
        } else {
            Err(ProtocolError::OperationMismatch { expected, found })
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRequestEnvelope {
    #[serde(rename = "protocol_version", deserialize_with = "version_must_match")]
    _protocol_version: u32,
    operation: Operation,
    attribution: Attribution,
    input: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawResponseEnvelope {
    #[serde(rename = "protocol_version", deserialize_with = "version_must_match")]
    _protocol_version: u32,
    operation: Operation,
    attribution: Attribution,
    result: Value,
}

fn request_value(request: &Request) -> Result<Value, ProtocolError> {
    match request {
        Request::BeforeModel(value) => serde_json::to_value(value),
        Request::PreTool(value) => serde_json::to_value(value),
        Request::PostTool(value) => serde_json::to_value(value),
        Request::Retrieve(value) => serde_json::to_value(value),
    }
    .map_err(ProtocolError::Serialize)
}

fn response_value(response: &Response) -> Result<Value, ProtocolError> {
    match response {
        Response::BeforeModel(value) => serde_json::to_value(value),
        Response::PreTool(value) => serde_json::to_value(value),
        Response::PostTool(value) => serde_json::to_value(value),
        Response::Retrieve(value) => serde_json::to_value(value),
    }
    .map_err(ProtocolError::Serialize)
}

fn serialize_envelope(
    operation: Operation,
    attribution: &Attribution,
    payload: Value,
    payload_name: &str,
) -> Result<String, ProtocolError> {
    let mut object = serde_json::Map::new();
    object.insert("protocol_version".into(), Value::from(PROTOCOL_VERSION));
    object.insert(
        "operation".into(),
        serde_json::to_value(operation).map_err(ProtocolError::Serialize)?,
    );
    object.insert(
        "attribution".into(),
        serde_json::to_value(attribution).map_err(ProtocolError::Serialize)?,
    );
    object.insert(payload_name.into(), payload);
    serde_json::to_string(&object).map_err(ProtocolError::Serialize)
}

fn check_version(json: &str) -> Result<(), ProtocolError> {
    #[derive(Deserialize)]
    struct VersionOnly {
        protocol_version: u32,
    }
    let version: VersionOnly = serde_json::from_str(json)?;
    if version.protocol_version != PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedVersion {
            found: version.protocol_version,
        });
    }
    Ok(())
}

fn version_must_match<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let version = u32::deserialize(deserializer)?;
    if version == PROTOCOL_VERSION {
        Ok(version)
    } else {
        Err(serde::de::Error::custom(format_args!(
            "unsupported protocol_version {version} (supported: {PROTOCOL_VERSION})"
        )))
    }
}
