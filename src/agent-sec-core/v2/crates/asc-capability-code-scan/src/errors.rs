//! Errors surfaced by the Code Scan capability.
//!
//! Both the message text and the numeric code mirror V1's `CodeScanError`
//! hierarchy. The message is part of the product contract: the result summary
//! is rendered as `scan error: {message}` and asserted by end-to-end goldens,
//! so the wording must not drift.
//!
//! Code ranges, with prefix `1` identifying the code scanner: `100` base,
//! `110-119` input layer, `120-129` rule layer, `130-139` engine layer.
//! `140-149` is reserved for the LLM engine, which this crate does not carry.

/// A scan failure that the caller must still be able to turn into a verdict.
///
/// Every variant maps one-to-one onto a V1 error class, including the case
/// where no context is available and V1 falls back to a bare message: V1
/// appends `: {detail}` only for a non-empty detail, and a message-carrying
/// class uses the message verbatim, falling back to its class default.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CodeScanError {
    /// V1 `CodeScanError`: an unclassified failure.
    #[error("internal error")]
    Internal,
    /// V1 `ErrInputEmpty`: the input was empty or whitespace only.
    #[error("empty input code")]
    InputEmpty,
    /// V1 `ErrUnsupportedLang`: the language is outside the supported set.
    #[error("{}", with_detail("unsupported language", .0))]
    UnsupportedLanguage(String),
    /// V1 `ErrRuleYamlParse`: the rule document could not be parsed.
    ///
    /// Carries the YAML file stem, which is what V1 reports; it is not
    /// necessarily equal to the `rule_id` declared inside the file.
    #[error("{}", with_detail("rule file YAML parse error", .0))]
    RuleYamlParse(String),
    /// V1 `ErrRuleValidation`: a field was missing, empty or of a wrong type.
    #[error("{}", with_detail("rule validation failed", .0))]
    RuleValidation(String),
    /// V1 `ErrRuleRefResolve`: `target_regexes_ref` named an absent shared list.
    #[error("{}", with_detail("rule reference resolve failed", .0))]
    RuleRefResolve(String),
    /// V1 `ErrRegexCompile`: a rule pattern was rejected by the regex engine.
    #[error("{}", with_detail("regex compile failed", .0))]
    RegexCompile(String),
    /// V1 `ErrEngineResource`: the engine ran out of a bounded resource.
    #[error("engine resource exhausted")]
    EngineResource,
    /// V1 `ErrLlmUnavailable`: the local model cannot serve this scan.
    #[error("{}", message_or("LLM model not available", .0))]
    LlmUnavailable(String),
    /// V1 `ErrLlmUnparsable`: the model did not produce an unambiguous verdict.
    #[error("{}", message_or("LLM response unparsable", .0))]
    LlmUnparsable(String),
}

fn with_detail(prefix: &str, detail: &str) -> String {
    if detail.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}: {detail}")
    }
}

fn message_or(default: &str, message: &str) -> String {
    if message.is_empty() {
        default.to_owned()
    } else {
        message.to_owned()
    }
}

impl CodeScanError {
    /// Returns the V1 numeric error code for this failure.
    pub const fn code(&self) -> u16 {
        match self {
            Self::Internal => 100,
            Self::InputEmpty => 110,
            Self::UnsupportedLanguage(_) => 111,
            Self::RuleYamlParse(_) => 121,
            Self::RuleValidation(_) => 122,
            Self::RuleRefResolve(_) => 123,
            Self::RegexCompile(_) => 124,
            Self::EngineResource => 131,
            Self::LlmUnavailable(_) => 140,
            Self::LlmUnparsable(_) => 141,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CodeScanError;

    #[test]
    fn messages_follow_v1_detail_rules() {
        for (error, message) in [
            (
                CodeScanError::UnsupportedLanguage("ruby".into()),
                "unsupported language: ruby",
            ),
            (
                CodeScanError::UnsupportedLanguage(String::new()),
                "unsupported language",
            ),
            (
                CodeScanError::RegexCompile(String::new()),
                "regex compile failed",
            ),
            (
                CodeScanError::LlmUnavailable(String::new()),
                "LLM model not available",
            ),
            (
                CodeScanError::LlmUnparsable("raw output: maybe".into()),
                "raw output: maybe",
            ),
            (
                CodeScanError::LlmUnparsable(String::new()),
                "LLM response unparsable",
            ),
        ] {
            assert_eq!(error.to_string(), message);
        }
    }
}
