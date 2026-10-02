//! Tool-argument admission and the interactive-question dispatch.
//!
//! Everything here decides whether a provider-issued call may run at all:
//! argument parsing, bounded audit shapes and digests, the rejection path, and
//! the two routes that can reach the user with a question (the
//! `ask_user_question` tool call and the in-band `COSH_QUESTION:` text).
//!
//! It lives beside `core.rs` rather than inside it because the turn loop is
//! already at its size budget, and because these are the checks a reviewer wants
//! to read as one unit: a gap between them is how a malformed call became a
//! valid-looking prompt.

use std::io::Write;
use std::time::Instant;

use tokio::io::AsyncBufReadExt;

use cosh_types::audit::AuditToolData;

use crate::audit::CoreAuditScope;
use crate::protocol::OutputMessage;
use crate::tool::ask_user_question::{
    self, AskUserArgumentError, AskUserQuestionParams, AskUserRejectionDiagnostics,
};
use crate::tool::ToolResult;

use super::{CoshCore, PendingToolCall};

/// Marker introducing an in-band question inside assistant text.
const IN_BAND_MARKER: &str = "COSH_QUESTION:";

/// What the assistant's plain text carries on the in-band question route.
///
/// The marker suppresses ordinary text output, so an invalid payload must be
/// distinguishable from "no question at all" — otherwise the turn would end
/// silently, showing the user neither a question nor a reason.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum InBandQuestion {
    /// No marker in the text: an ordinary assistant reply.
    Absent,
    /// A payload that passed the same validation as the tool call.
    Valid(AskUserQuestionParams),
    /// A payload that must be surfaced as a failure, never as a question.
    Invalid(AskUserArgumentError),
}

/// Classify assistant text that may carry an in-band question.
///
/// Shares [`ask_user_question::validate_value`] with the tool-call route, so
/// neither can produce a question the user is unable to answer.
pub(super) fn parse_in_band_question(text: &str) -> InBandQuestion {
    let Some((_, after_marker)) = text.split_once(IN_BAND_MARKER) else {
        return InBandQuestion::Absent;
    };
    let Some(json_text) = after_marker.trim().lines().next().map(str::trim) else {
        return InBandQuestion::Invalid(AskUserArgumentError::EmptyArguments);
    };
    if json_text.is_empty() {
        return InBandQuestion::Invalid(AskUserArgumentError::EmptyArguments);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(json_text) else {
        return InBandQuestion::Invalid(AskUserArgumentError::InvalidJson);
    };
    match ask_user_question::validate_value(&value) {
        Ok(params) => InBandQuestion::Valid(params),
        Err(error) => InBandQuestion::Invalid(error),
    }
}

/// Turn-terminating message for an in-band question that failed validation.
///
/// Carries the stable code and the expected shape only: the payload may hold
/// session content.
pub(super) fn in_band_question_error(error: AskUserArgumentError) -> String {
    format!(
        "Provider emitted an invalid {IN_BAND_MARKER} payload [code={}]: {}. No question was shown.",
        error.code(),
        error.guidance()
    )
}

/// Why a tool call's arguments were refused before execution.
///
/// Both variants carry shapes and codes only — never the payload, which can hold
/// session content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ArgumentError {
    /// The payload was not valid JSON.
    InvalidJson,
    /// The payload parsed, but its root was not the declared object.
    RootNotObject { shape: &'static str },
}

impl ArgumentError {
    /// Stable code for logs, audit reasons, and the tool-result text.
    pub(super) fn code(&self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::RootNotObject { .. } => "arguments_not_object",
        }
    }

    /// JSON parse status recorded for the call, matching the ask-user codes.
    pub(super) fn json_parse_status(&self) -> &'static str {
        match self {
            Self::InvalidJson => ask_user_question::JSON_PARSE_INVALID,
            // The bytes did parse; only the shape was wrong.
            Self::RootNotObject { .. } => ask_user_question::JSON_PARSE_OK,
        }
    }

    /// Audit `input_shape` for a rejected call.
    pub(super) fn audit_shape(&self) -> &'static str {
        match self {
            Self::InvalidJson => "unparsed",
            Self::RootNotObject { shape } => shape,
        }
    }

    /// One clause naming what was wrong, safe to show the model.
    fn summary(&self) -> String {
        match self {
            Self::InvalidJson => "arguments were not valid JSON".to_string(),
            Self::RootNotObject { shape } => {
                format!("arguments were a JSON {shape}, not an object")
            }
        }
    }
}

impl std::fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.summary())
    }
}

/// Parse tool arguments as received from the provider.
///
/// Empty or whitespace-only arguments mean "no arguments" — the convention
/// providers use for zero-parameter tools — and become an empty object. Every
/// tool declares an object root, so a payload that parses to `null`, an array, or
/// a scalar is refused rather than passed through: `null` makes every field look
/// merely absent to the tool implementation, and the other roots would reach an
/// MCP server as arguments it never declared.
///
/// # Errors
///
/// Returns [`ArgumentError`] when non-empty arguments are not valid JSON, or
/// when they parse to anything other than an object.
pub(super) fn parse_tool_arguments(raw: &str) -> Result<serde_json::Value, ArgumentError> {
    if raw.trim().is_empty() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    let value: serde_json::Value =
        serde_json::from_str(raw).map_err(|_| ArgumentError::InvalidJson)?;
    if !value.is_object() {
        return Err(ArgumentError::RootNotObject {
            shape: json_shape(&value),
        });
    }
    Ok(value)
}

/// How many times in a row one tool may have its arguments refused before the
/// run is stopped.
///
/// Fixed rather than configurable: the budget exists to break a model that
/// cannot produce parseable arguments, and a tunable would only let a session
/// re-enter the loop this bound was added to end.
pub(super) const MAX_INVALID_ARGUMENT_ATTEMPTS: u8 = 3;

/// Consecutive pre-execution argument rejections within one user message.
///
/// Keyed by tool name and stable error code, not tool-call id: every retry is a
/// fresh call with a fresh id, so ids would never match and the streak would
/// never grow.
#[derive(Debug, Default)]
pub(super) struct InvalidArgumentStreak {
    current: Option<(String, &'static str, String, u8)>,
}

impl InvalidArgumentStreak {
    /// Count one rejected provider turn and return its 1-based attempt number.
    ///
    /// A different tool or a different error code starts a new streak at 1: the
    /// model changing how it fails is progress, not another attempt at the same
    /// failure. Multiple matching calls in one assistant message share an
    /// attempt because they are a batch, not retries of one another.
    pub(super) fn record(
        &mut self,
        tool_name: &str,
        code: &'static str,
        provider_turn_id: &str,
    ) -> u8 {
        match &mut self.current {
            Some((name, current_code, counted_turn, count))
                if name == tool_name
                    && *current_code == code
                    && counted_turn == provider_turn_id =>
            {
                *count
            }
            Some((name, current_code, counted_turn, count))
                if name == tool_name && *current_code == code =>
            {
                *count = count.saturating_add(1);
                *counted_turn = provider_turn_id.to_string();
                *count
            }
            slot => {
                *slot = Some((tool_name.to_string(), code, provider_turn_id.to_string(), 1));
                1
            }
        }
    }

    /// Forget the streak once a tool call's arguments parsed.
    pub(super) fn clear(&mut self) {
        self.current = None;
    }
}

/// Longest tool name kept in a message that reaches a terminal.
const MAX_DISPLAY_TOOL_NAME_CHARS: usize = 64;

/// A provider-supplied tool name, made safe to embed in a rendered message.
///
/// Mirrors cosh-shell's terminal-facing sanitizer because the Shell is a
/// standalone crate; keep the filtering and length contract aligned in both.
///
/// The name is model output, not a validated identifier: an `` in the JSON
/// arrives here as a real ESC byte and would reach the terminal as an escape
/// sequence, and an unbounded name would flood the surface it is drawn on.
pub(super) fn display_tool_name(tool_name: &str) -> String {
    let mut safe: String = tool_name
        .chars()
        .filter(|character| {
            !character.is_control() && !matches!(character, '\u{2028}' | '\u{2029}')
        })
        .take(MAX_DISPLAY_TOOL_NAME_CHARS)
        .collect();
    let trimmed = safe.trim();
    if trimmed.is_empty() {
        return "tool".to_string();
    }
    if trimmed.len() != safe.len() {
        safe = trimmed.to_string();
    }
    if tool_name.chars().count() > MAX_DISPLAY_TOOL_NAME_CHARS {
        safe.push('…');
    }
    safe
}

/// Tool-result text for a tool call whose arguments were refused.
///
/// Carries no fragment of the rejected payload: malformed arguments can still
/// contain session content. The attempt counter is included so the model — and
/// the failure card the user sees — show how much budget is left.
pub(super) fn invalid_arguments_message(
    tool_name: &str,
    error: &ArgumentError,
    attempt: u8,
    max_attempts: u8,
) -> String {
    let tool_name = display_tool_name(tool_name);
    let next = if attempt >= max_attempts {
        format!(
            "The tool was not executed, and the run was stopped after {max_attempts} \
             consecutive rejections."
        )
    } else {
        "The tool was not executed; re-issue the call with one complete JSON object matching \
         the declared schema."
            .to_string()
    };
    format!(
        "{tool_name} arguments rejected [code={}] (attempt {attempt}/{max_attempts}): {}. {next}",
        error.code(),
        error.summary()
    )
}

/// Run-terminating message for a tool that exhausted its rejection budget.
///
/// Names the tool and the stable code only, and states that nothing ran, so the
/// user can act without seeing the payload.
pub(super) fn invalid_arguments_exhausted_error(tool_name: &str, error: &ArgumentError) -> String {
    format!(
        "{} arguments were rejected {MAX_INVALID_ARGUMENT_ATTEMPTS} times in a row \
         [code={}]; stopped this run. The tool never executed.",
        display_tool_name(tool_name),
        error.code()
    )
}

/// Tool-result text for a call left unattempted because an earlier call in the
/// same assistant message ended the run.
///
/// Exists so the batch stays paired: an unanswered `tool_use` id would make the
/// next provider request malformed.
pub(super) fn skipped_after_fatal_message(tool_name: &str) -> String {
    format!(
        "{} was not executed: an earlier tool call in the same message ended this run.",
        display_tool_name(tool_name)
    )
}

pub(super) fn json_shape(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub(super) fn hash_json(value: &serde_json::Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    hash_bytes(&bytes)
}

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

impl CoshCore {
    /// Run one `ask_user_question` tool call, or reject its arguments.
    ///
    /// Returns the tool result when a question was actually shown, and `None`
    /// when the call was rejected before execution.
    ///
    /// # Errors
    ///
    /// Propagates audit-recording failures, which abort the turn.
    pub(super) async fn dispatch_ask_user_tool_call<W, R>(
        &mut self,
        scope: CoreAuditScope<'_>,
        call: &PendingToolCall,
        provider_type: &str,
        tool_kind: &str,
        reader: &mut tokio::io::Lines<R>,
        writer: &mut W,
    ) -> Result<Option<ToolResult>, String>
    where
        W: Write,
        R: AsyncBufReadExt + Unpin,
    {
        let report = ask_user_question::inspect_arguments(&call.arguments);
        let tool_data = AuditToolData {
            tool_kind: tool_kind.to_string(),
            input_shape: Some(
                report
                    .root
                    .as_ref()
                    .map(|root| json_shape(root))
                    .unwrap_or(report.json_parse_status)
                    .to_string(),
            ),
            input_hash: Some(match &report.root {
                Some(root) => hash_json(root),
                None => hash_bytes(call.arguments.as_bytes()),
            }),
            ..AuditToolData::default()
        };
        self.audit
            .record_tool_requested(scope, &call.name, &tool_data);

        let params = match report.outcome {
            Ok(params) => params,
            Err(error) => {
                // No control request is emitted: a question the user cannot
                // answer would block the run, and a generic placeholder would
                // hide the real cause.
                ask_user_question::log_rejection(&AskUserRejectionDiagnostics {
                    provider_type,
                    tool_call_id: &call.id,
                    tool_name: &call.name,
                    start_seen: call.start_seen,
                    delta_count: call.delta_count,
                    end_seen: call.end_seen,
                    argument_bytes: report.argument_bytes,
                    json_parse_status: report.json_parse_status,
                    validation_error_code: error.code(),
                    question_shape: report.question_shape,
                });
                // The result is not emitted on the wire: the Shell never opened a
                // pending tool for an incomplete question call, so a tool result
                // would arrive for an id it does not know.
                let _ = self.reject_tool_arguments(
                    scope,
                    &call.name,
                    &call.id,
                    &tool_data,
                    error.tool_error_message(),
                );
                return Ok(None);
            }
        };

        self.audit
            .record_tool_execution_started(scope, &call.name, &tool_data)?;
        let tool_start = Instant::now();
        let result = self
            .handle_ask_user(&params, Some(&call.id), reader, writer)
            .await;
        let duration_ms = tool_start.elapsed().as_millis() as u64;
        self.audit.record_tool_terminal(
            scope,
            &call.name,
            &tool_data,
            result.is_error,
            duration_ms,
            result.output.len() as u64,
        );
        // Counted like any other tool call so a single rejection cannot make the
        // question tool look like it always fails.
        self.note_tool_call_metrics(result.is_error, duration_ms);
        Ok(Some(result))
    }

    /// Record one completed tool call in the per-turn metrics.
    pub(super) fn note_tool_call_metrics(&mut self, is_error: bool, duration_ms: u64) {
        self.metrics.tool_calls_total += 1;
        self.metrics.tool_calls_duration_ms += duration_ms;
        if is_error {
            self.metrics.tool_calls_fail += 1;
        } else {
            self.metrics.tool_calls_success += 1;
        }
    }

    /// Audit data for a call refused before its arguments were ever parsed.
    ///
    /// Reports the payload as `unparsed` — the same shape a malformed payload
    /// gets — because that is literally what happened: nothing inspected these
    /// bytes. Hashing them still identifies the call in a trace without
    /// recording session content.
    fn unexecuted_tool_data(&self, tool_name: &str, arguments: &str) -> AuditToolData {
        AuditToolData {
            tool_kind: self
                .tools
                .get(tool_name)
                .map(|tool| format!("{:?}", tool.kind()).to_ascii_lowercase())
                .unwrap_or_else(|| "virtual".to_string()),
            input_shape: Some("unparsed".to_string()),
            input_hash: Some(hash_bytes(arguments.as_bytes())),
            ..AuditToolData::default()
        }
    }

    /// Fail a tool call the run never attempted, over its whole lifecycle.
    ///
    /// A skipped call is refused before the normal path builds any audit data, so
    /// without this it would reach the provider history with no `tool.requested`,
    /// no terminal event and no metrics — invisible in an audit trace even though
    /// it is part of the transcript. The audit contract is one `tool.requested`
    /// and one terminal event per call, including the ones that never ran.
    pub(super) fn skip_unexecuted_tool_call<W: Write>(
        &mut self,
        scope: CoreAuditScope<'_>,
        writer: &mut W,
        call: &PendingToolCall,
    ) -> ToolResult {
        let tool_data = self.unexecuted_tool_data(&call.name, &call.arguments);
        self.audit
            .record_tool_requested(scope, &call.name, &tool_data);
        let result = self.reject_tool_arguments(
            scope,
            &call.name,
            &call.id,
            &tool_data,
            skipped_after_fatal_message(&call.name),
        );
        // The Shell never opened a pending tool for a question call, so a result
        // for that id would be unroutable; every other tool has one to close.
        if !(call.name == ask_user_question::TOOL_NAME && self.tools.supports_ask_user_question()) {
            self.emit_provider_native_tool_result(writer, &call.id, &result);
        }
        result
    }

    /// Fail a tool call whose arguments were rejected before execution.
    ///
    /// Audits the call as failed without a `tool.execution.started` record and
    /// appends an error tool result, so the model can re-issue a valid call
    /// instead of the run stalling on an unusable one.
    ///
    /// Returns that tool result so the caller can also emit it on the wire: the
    /// Shell opened a pending tool for this call and only a tool result closes
    /// it, otherwise a rejected call stays on screen as if it were still running.
    pub(super) fn reject_tool_arguments(
        &mut self,
        scope: CoreAuditScope<'_>,
        tool_name: &str,
        tool_call_id: &str,
        tool_data: &AuditToolData,
        message: String,
    ) -> ToolResult {
        self.note_tool_call_metrics(true, 0);
        let result = ToolResult::error(message);
        self.audit.record_tool_terminal(
            scope,
            tool_name,
            tool_data,
            result.is_error,
            0,
            result.output.len() as u64,
        );
        self.messages.push(crate::provider::Message::tool_result(
            tool_call_id,
            &result.output,
            result.is_error,
        ));
        result
    }

    /// Emit an interactive question and wait for the user's answer.
    ///
    /// Takes already-validated params: every fallback that could invent question
    /// text lives in `tool::ask_user_question`, so a malformed tool call can
    /// never reach the user as a generic prompt.
    pub(super) async fn handle_ask_user<W, R>(
        &self,
        params: &AskUserQuestionParams,
        tool_use_id: Option<&str>,
        reader: &mut tokio::io::Lines<R>,
        writer: &mut W,
    ) -> ToolResult
    where
        W: Write,
        R: AsyncBufReadExt + Unpin,
    {
        let options: Vec<crate::protocol::AskUserOption> = params
            .options
            .iter()
            .map(|option| crate::protocol::AskUserOption {
                label: option.label.clone(),
                description: option.description.clone(),
            })
            .collect();

        let request_id = self.next_request_id();
        // Checked: an unasked question can never be answered, so waiting for
        // one is the silent permanent hang from #1994.
        if let Err(error) = self.emit_control_request_checked(
            writer,
            &OutputMessage::ControlRequest {
                request_id: request_id.clone(),
                request: crate::protocol::CoreControlRequest::AskUser {
                    tool_use_id: if self.execution_profile.is_brokered() {
                        tool_use_id.map(str::to_owned)
                    } else {
                        None
                    },
                    question: params.question.clone(),
                    options,
                    allow_free_text: params.allow_free_text,
                    multi_select: params.multi_select,
                },
            },
        ) {
            self.note_control_transport_failure(&request_id, &error);
            return ToolResult::error(format!(
                "question was not answered: delivery could not be confirmed ({})",
                error.class()
            ));
        }

        match self.wait_for_answer(&request_id, reader).await {
            Some(answer) => ToolResult::success(answer),
            None => ToolResult::error("User did not answer (interrupted or disconnected)"),
        }
    }
}

#[cfg(test)]
#[path = "tool_execution/tests.rs"]
mod tests;
