//! `ask_user_question` argument contract: schema declaration, strict validation,
//! and the bounded diagnostics emitted when a call is rejected.
//!
//! The interactive question path is the one place where a malformed tool call
//! used to become a valid-looking prompt, so declaration and validation live
//! together here: a schema change that is not mirrored by a validation rule is
//! visible in one file.

use serde_json::Value;

use crate::provider::ToolDeclaration;

/// Tool name as declared to providers and matched in the Core tool loop.
pub const TOOL_NAME: &str = "ask_user_question";

const TOOL_DESCRIPTION: &str = "Ask the user a question. Use this when you need clarification or want the user to choose between options.";

/// Validated arguments for one `ask_user_question` call.
///
/// Construction goes through [`inspect_arguments`] or [`validate_value`], so a
/// value of this type is a proof that the question is answerable: `question` is
/// non-empty after trimming, and at least one of free-text input or a non-empty
/// option list is available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserQuestionParams {
    /// Question text, trimmed and guaranteed non-empty.
    pub question: String,
    /// Selectable options; may be empty only when `allow_free_text` is true.
    pub options: Vec<AskUserQuestionOption>,
    /// Whether free-text answers are accepted. Defaults to true when omitted.
    pub allow_free_text: bool,
    /// Whether multiple options may be selected. Defaults to false when omitted.
    pub multi_select: bool,
}

/// One selectable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserQuestionOption {
    /// Display label, trimmed and guaranteed non-empty.
    pub label: String,
    /// Optional longer explanation.
    pub description: Option<String>,
}

/// Why a set of `ask_user_question` arguments cannot be turned into a question.
///
/// The variants are a stable diagnostic surface: [`AskUserArgumentError::code`]
/// strings appear in tool-result text, audit records, and tests, so rename them
/// only together with the tests that pin them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskUserArgumentError {
    /// The provider produced no argument bytes at all.
    EmptyArguments,
    /// Argument bytes were present but not parseable as JSON.
    InvalidJson,
    /// Valid JSON whose root is not an object.
    RootNotObject,
    /// No `question` key at all.
    MissingQuestion,
    /// `question` present but not a JSON string, including an explicit `null`.
    QuestionWrongType,
    /// `question` is a string that is empty after trimming.
    EmptyQuestion,
    /// `options` present but not a JSON array, including an explicit `null`.
    OptionsWrongType,
    /// An `options` entry is not an object with a non-empty string `label`,
    /// or carries a `description` that is present but not a string.
    OptionInvalid,
    /// `allow_free_text` present but not a boolean, including an explicit `null`.
    AllowFreeTextWrongType,
    /// `multi_select` present but not a boolean, including an explicit `null`.
    MultiSelectWrongType,
    /// Free text disabled with no selectable option — the user could not answer.
    NoAnswerPath,
    /// Claude-style nested `questions` array, which this tool does not accept.
    UnsupportedNestedQuestions,
}

impl AskUserArgumentError {
    /// Stable machine-readable error code.
    pub fn code(self) -> &'static str {
        match self {
            Self::EmptyArguments => "empty_arguments",
            Self::InvalidJson => "invalid_json",
            Self::RootNotObject => "root_not_object",
            Self::MissingQuestion => "missing_question",
            Self::QuestionWrongType => "question_wrong_type",
            Self::EmptyQuestion => "empty_question",
            Self::OptionsWrongType => "options_wrong_type",
            Self::OptionInvalid => "option_invalid",
            Self::AllowFreeTextWrongType => "allow_free_text_wrong_type",
            Self::MultiSelectWrongType => "multi_select_wrong_type",
            Self::NoAnswerPath => "no_answer_path",
            Self::UnsupportedNestedQuestions => "unsupported_nested_questions",
        }
    }

    /// Remediation hint describing the expected shape, never echoing the input.
    pub fn guidance(self) -> &'static str {
        match self {
            Self::EmptyArguments => {
                "the tool call carried no arguments; send a JSON object with a \"question\" string"
            }
            Self::InvalidJson => {
                "arguments were not valid JSON; send one complete JSON object, not a fragment"
            }
            Self::RootNotObject => "arguments must be a JSON object",
            Self::MissingQuestion => "arguments must contain a \"question\" string",
            Self::QuestionWrongType => "\"question\" must be a string; null is not accepted",
            Self::EmptyQuestion => "\"question\" must not be empty or whitespace only",
            Self::OptionsWrongType => {
                "\"options\" must be an array when present; omit the key instead of sending null"
            }
            Self::OptionInvalid => {
                "each option must be an object with a non-empty string \"label\" and an optional string \"description\"; omit \"description\" instead of sending null"
            }
            Self::AllowFreeTextWrongType => {
                "\"allow_free_text\" must be a boolean when present; omit the key instead of sending null"
            }
            Self::MultiSelectWrongType => {
                "\"multi_select\" must be a boolean when present; omit the key instead of sending null"
            }
            Self::NoAnswerPath => {
                "\"allow_free_text\": false requires at least one valid option, otherwise the user cannot answer"
            }
            Self::UnsupportedNestedQuestions => {
                "nested \"questions\" arrays are not supported; ask one question per tool call using the top-level \"question\" field"
            }
        }
    }

    /// Tool-result text returned to the model so it can retry with valid input.
    ///
    /// Deliberately carries only the error code and the expected shape: the
    /// rejected payload may contain user or session content.
    pub fn tool_error_message(self) -> String {
        format!(
            "{TOOL_NAME} arguments rejected [code={}]: {}. No question was shown to the user; re-issue the call with arguments matching the declared schema.",
            self.code(),
            self.guidance()
        )
    }
}

/// Whether raw argument bytes could be parsed, used for bounded diagnostics.
pub const JSON_PARSE_EMPTY: &str = "empty";
/// Argument bytes were present but not valid JSON.
pub const JSON_PARSE_INVALID: &str = "invalid";
/// Argument bytes parsed into a JSON value.
pub const JSON_PARSE_OK: &str = "parsed";

/// Bounded facts about one argument payload, plus the validated params when the
/// payload is acceptable.
///
/// Every field is safe to log: shapes, byte counts, and codes only — never the
/// question text, option labels, or the raw arguments.
pub struct AskUserArgumentReport {
    /// Byte length of the raw argument string as received from the provider.
    pub argument_bytes: usize,
    /// One of [`JSON_PARSE_EMPTY`], [`JSON_PARSE_INVALID`], [`JSON_PARSE_OK`].
    pub json_parse_status: &'static str,
    /// Coarse shape of the `question` field, e.g. `missing`, `empty_string`.
    pub question_shape: &'static str,
    /// Parsed root value, kept only so the caller can derive audit shape/hash.
    pub root: Option<Value>,
    /// Validated params, or the first validation failure.
    pub outcome: Result<AskUserQuestionParams, AskUserArgumentError>,
}

/// Parse and validate raw provider argument bytes.
///
/// Empty or whitespace-only input is rejected as [`AskUserArgumentError::EmptyArguments`]
/// rather than treated as an empty object, because a question with no text is
/// exactly the argument-loss case this validation exists to surface.
pub fn inspect_arguments(raw: &str) -> AskUserArgumentReport {
    let argument_bytes = raw.len();

    if raw.trim().is_empty() {
        return AskUserArgumentReport {
            argument_bytes,
            json_parse_status: JSON_PARSE_EMPTY,
            question_shape: "unparsed",
            root: None,
            outcome: Err(AskUserArgumentError::EmptyArguments),
        };
    }

    let root: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => {
            return AskUserArgumentReport {
                argument_bytes,
                json_parse_status: JSON_PARSE_INVALID,
                question_shape: "unparsed",
                root: None,
                outcome: Err(AskUserArgumentError::InvalidJson),
            };
        }
    };

    let question_shape = question_shape(&root);
    let outcome = validate_value(&root);
    AskUserArgumentReport {
        argument_bytes,
        json_parse_status: JSON_PARSE_OK,
        question_shape,
        root: Some(root),
        outcome,
    }
}

/// Validate an already-parsed argument object.
///
/// Used both by [`inspect_arguments`] and by the in-band `COSH_QUESTION:` text
/// path, so both routes enforce identical rules.
///
/// # Errors
///
/// Returns the first violated rule as an [`AskUserArgumentError`].
pub fn validate_value(root: &Value) -> Result<AskUserQuestionParams, AskUserArgumentError> {
    let Some(obj) = root.as_object() else {
        return Err(AskUserArgumentError::RootNotObject);
    };

    // Claude's native payload nests every question under `questions`. Taking the
    // first entry would silently drop the rest, so the shape is rejected instead.
    // `questions` is not part of the declared schema, so a null carries nothing to
    // drop and is ignored like any other unknown key.
    if obj.get("questions").is_some_and(|v| !v.is_null()) {
        return Err(AskUserArgumentError::UnsupportedNestedQuestions);
    }

    // A present `null` is a schema violation, not an omission: the declared
    // types are string/array/boolean, so defaults apply only to absent keys.
    let question = match obj.get("question") {
        None => return Err(AskUserArgumentError::MissingQuestion),
        Some(Value::String(text)) => text.trim(),
        Some(_) => return Err(AskUserArgumentError::QuestionWrongType),
    };
    if question.is_empty() {
        return Err(AskUserArgumentError::EmptyQuestion);
    }

    let options = match obj.get("options") {
        None => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(parse_option)
            .collect::<Result<Vec<_>, _>>()?,
        Some(_) => return Err(AskUserArgumentError::OptionsWrongType),
    };

    let allow_free_text = match obj.get("allow_free_text") {
        None => true,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(AskUserArgumentError::AllowFreeTextWrongType),
    };
    let multi_select = match obj.get("multi_select") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(AskUserArgumentError::MultiSelectWrongType),
    };

    if !allow_free_text && options.is_empty() {
        return Err(AskUserArgumentError::NoAnswerPath);
    }

    Ok(AskUserQuestionParams {
        question: question.to_string(),
        options,
        allow_free_text,
        multi_select,
    })
}

fn parse_option(item: &Value) -> Result<AskUserQuestionOption, AskUserArgumentError> {
    // Bare strings are not part of the declared schema; accepting them here
    // would create an undocumented second format for the same field.
    let Some(obj) = item.as_object() else {
        return Err(AskUserArgumentError::OptionInvalid);
    };
    let label = match obj.get("label") {
        Some(Value::String(label)) => label.trim(),
        _ => return Err(AskUserArgumentError::OptionInvalid),
    };
    if label.is_empty() {
        return Err(AskUserArgumentError::OptionInvalid);
    }
    let description = match obj.get("description") {
        None => None,
        Some(Value::String(text)) => Some(text.to_string()),
        Some(_) => return Err(AskUserArgumentError::OptionInvalid),
    };
    Ok(AskUserQuestionOption {
        label: label.to_string(),
        description,
    })
}

/// Coarse shape of the `question` field for diagnostics.
fn question_shape(root: &Value) -> &'static str {
    let Some(obj) = root.as_object() else {
        return "root_not_object";
    };
    if obj.get("questions").is_some_and(|v| !v.is_null()) {
        return "nested_questions";
    }
    match obj.get("question") {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::String(text)) if text.trim().is_empty() => "empty_string",
        Some(Value::String(_)) => "nonempty_string",
        Some(Value::Bool(_)) => "wrong_type_boolean",
        Some(Value::Number(_)) => "wrong_type_number",
        Some(Value::Array(_)) => "wrong_type_array",
        Some(Value::Object(_)) => "wrong_type_object",
    }
}

/// Bounded provider/tool metadata recorded when Core rejects a call.
///
/// Holds no question text, option labels, raw arguments, SSE payload, or
/// credentials — only counts, shapes, and stable codes.
pub struct AskUserRejectionDiagnostics<'a> {
    /// Provider that produced the tool call, e.g. `aliyun`.
    pub provider_type: &'a str,
    /// Provider-assigned tool call id.
    pub tool_call_id: &'a str,
    /// Tool name as received; may differ from [`TOOL_NAME`] only in tests.
    pub tool_name: &'a str,
    /// Whether a `ToolCallStart` event was observed for this call.
    pub start_seen: bool,
    /// Number of `ToolCallDelta` events observed for this call.
    pub delta_count: u32,
    /// Whether a `ToolCallEnd` event was observed for this call.
    pub end_seen: bool,
    /// Accumulated argument byte count.
    pub argument_bytes: usize,
    /// JSON parse status from [`AskUserArgumentReport`].
    pub json_parse_status: &'static str,
    /// Stable code from [`AskUserArgumentError::code`].
    pub validation_error_code: &'static str,
    /// Coarse `question` field shape from [`AskUserArgumentReport`].
    pub question_shape: &'static str,
}

/// Record a rejected `ask_user_question` call at `warn` level.
pub fn log_rejection(diagnostics: &AskUserRejectionDiagnostics<'_>) {
    tracing::warn!(
        provider_type = diagnostics.provider_type,
        tool_call_id = diagnostics.tool_call_id,
        tool_name = diagnostics.tool_name,
        start_seen = diagnostics.start_seen,
        delta_count = diagnostics.delta_count,
        end_seen = diagnostics.end_seen,
        argument_bytes = diagnostics.argument_bytes,
        json_parse_status = diagnostics.json_parse_status,
        validation_error_code = diagnostics.validation_error_code,
        question_shape = diagnostics.question_shape,
        "rejected ask_user_question arguments"
    );
}

/// Public JSON schema advertised to providers.
///
/// `additionalProperties` is intentionally left unset: the validator ignores
/// unknown keys, so declaring `false` would promise a rejection Core does not
/// perform, and the keyword's acceptance across SysOM/DashScope has not been
/// verified against a live endpoint.
pub fn parameters_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "question": {
                "type": "string",
                "description": "The question to ask the user. Must be a non-empty string."
            },
            "options": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" },
                        "description": { "type": "string" }
                    },
                    "required": ["label"]
                },
                "description": "Options for the user to choose from"
            },
            "allow_free_text": {
                "type": "boolean",
                "description": "Whether to allow free-text input (default: true)"
            },
            "multi_select": {
                "type": "boolean",
                "description": "Whether to allow selecting multiple options (default: false)"
            }
        },
        "required": ["question"]
    })
}

/// Tool declaration sent to providers when the question tool is enabled.
pub fn declaration() -> ToolDeclaration {
    ToolDeclaration {
        name: TOOL_NAME.to_string(),
        description: TOOL_DESCRIPTION.to_string(),
        parameters: parameters_schema(),
    }
}

#[cfg(test)]
#[path = "ask_user_question/tests.rs"]
mod tests;
