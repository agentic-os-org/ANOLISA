/// Maximum number of choices carried by one Runtime input request.
pub const MAX_RUNTIME_INPUT_OPTIONS: usize = 32;
/// Maximum aggregate UTF-8 bytes across a question and all choice presentation.
pub const MAX_RUNTIME_INPUT_REQUEST_TEXT_BYTES: usize = 16 * 1024;
/// Maximum number of selected choice indices in one Runtime input response.
pub const MAX_RUNTIME_INPUT_SELECTIONS: usize = 32;

/// Invalid bounded Runtime input request or response.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeInputError {
    /// A non-free-text question has no selectable answer.
    #[error("input request must allow free text or provide at least one option")]
    NoAnswerMode,
    /// Multiple selection requires at least two choices.
    #[error("multi-select input request must provide at least two options")]
    InvalidMultiSelect,
    /// The option list exceeds the contract count bound.
    #[error("input request exceeds the {max} option limit")]
    TooManyOptions {
        /// Maximum accepted option count.
        max: usize,
    },
    /// The complete presentation exceeds its aggregate UTF-8 bound.
    #[error("input request presentation exceeds the {max_bytes}-byte limit")]
    RequestTextTooLarge {
        /// Maximum aggregate UTF-8 byte count.
        max_bytes: usize,
    },
    /// Choice labels must be unique so indices and display remain unambiguous.
    #[error("input request contains duplicate option labels")]
    DuplicateOption,
    /// An option response must select at least one choice.
    #[error("input response must select at least one option")]
    EmptySelection,
    /// The selection list exceeds the contract count bound.
    #[error("input response exceeds the {max} selection limit")]
    TooManySelections {
        /// Maximum accepted selection count.
        max: usize,
    },
    /// Repeated indices could otherwise be misread as multiple answers.
    #[error("input response contains duplicate option indices")]
    DuplicateSelection,
}

/// One bounded user-presentable choice for a Runtime input request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeInputOption {
    label: BoundedText,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<BoundedText>,
}

impl RuntimeInputOption {
    /// Creates one bounded choice.
    #[must_use]
    pub fn new(label: BoundedText, description: Option<BoundedText>) -> Self {
        Self { label, description }
    }

    /// Returns the user-visible choice label.
    #[must_use]
    pub fn label(&self) -> &BoundedText {
        &self.label
    }

    /// Returns the optional user-visible explanation.
    #[must_use]
    pub fn description(&self) -> Option<&BoundedText> {
        self.description.as_ref()
    }
}

/// Exact bounded question emitted by one Runtime turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeInputRequest {
    request_id: InputRequestId,
    run_id: RunId,
    turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<ToolUseId>,
    question: BoundedText,
    options: Vec<RuntimeInputOption>,
    allow_free_text: bool,
    multi_select: bool,
}

impl RuntimeInputRequest {
    /// Builds a request after enforcing count, aggregate byte, and answer-mode bounds.
    pub fn new(
        request_id: InputRequestId,
        run_id: RunId,
        turn_id: TurnId,
        tool_use_id: Option<ToolUseId>,
        question: BoundedText,
        options: Vec<RuntimeInputOption>,
        allow_free_text: bool,
        multi_select: bool,
    ) -> Result<Self, RuntimeInputError> {
        if options.len() > MAX_RUNTIME_INPUT_OPTIONS {
            return Err(RuntimeInputError::TooManyOptions {
                max: MAX_RUNTIME_INPUT_OPTIONS,
            });
        }
        if !allow_free_text && options.is_empty() {
            return Err(RuntimeInputError::NoAnswerMode);
        }
        if multi_select && options.len() < 2 {
            return Err(RuntimeInputError::InvalidMultiSelect);
        }
        let text_bytes = options
            .iter()
            .try_fold(question.as_str().len(), |total, option| {
                total
                    .checked_add(option.label().as_str().len())?
                    .checked_add(option.description().map_or(0, |value| value.as_str().len()))
            });
        if text_bytes.is_none_or(|bytes| bytes > MAX_RUNTIME_INPUT_REQUEST_TEXT_BYTES) {
            return Err(RuntimeInputError::RequestTextTooLarge {
                max_bytes: MAX_RUNTIME_INPUT_REQUEST_TEXT_BYTES,
            });
        }
        if options.iter().enumerate().any(|(index, option)| {
            options[..index]
                .iter()
                .any(|prior| prior.label() == option.label())
        }) {
            return Err(RuntimeInputError::DuplicateOption);
        }
        Ok(Self {
            request_id,
            run_id,
            turn_id,
            tool_use_id,
            question,
            options,
            allow_free_text,
            multi_select,
        })
    }

    /// Returns the independently allocated request identity.
    #[must_use]
    pub fn request_id(&self) -> &InputRequestId {
        &self.request_id
    }

    /// Returns the owning Run.
    #[must_use]
    pub fn run_id(&self) -> &RunId {
        &self.run_id
    }

    /// Returns the exact prompt turn waiting for input.
    #[must_use]
    pub fn turn_id(&self) -> &TurnId {
        &self.turn_id
    }

    /// Returns the optional observed tool identity.
    #[must_use]
    pub fn tool_use_id(&self) -> Option<&ToolUseId> {
        self.tool_use_id.as_ref()
    }

    /// Returns the bounded user-visible question.
    #[must_use]
    pub fn question(&self) -> &BoundedText {
        &self.question
    }

    /// Returns the bounded selectable choices.
    #[must_use]
    pub fn options(&self) -> &[RuntimeInputOption] {
        &self.options
    }

    /// Returns whether bounded free text is accepted.
    #[must_use]
    pub fn allows_free_text(&self) -> bool {
        self.allow_free_text
    }

    /// Returns whether more than one choice may be selected.
    #[must_use]
    pub fn allows_multiple(&self) -> bool {
        self.multi_select
    }
}

impl<'de> Deserialize<'de> for RuntimeInputRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct WireRequest {
            request_id: InputRequestId,
            run_id: RunId,
            turn_id: TurnId,
            #[serde(default)]
            tool_use_id: Option<ToolUseId>,
            question: BoundedText,
            options: Vec<RuntimeInputOption>,
            allow_free_text: bool,
            multi_select: bool,
        }

        let wire = WireRequest::deserialize(deserializer)?;
        Self::new(
            wire.request_id,
            wire.run_id,
            wire.turn_id,
            wire.tool_use_id,
            wire.question,
            wire.options,
            wire.allow_free_text,
            wire.multi_select,
        )
        .map_err(de::Error::custom)
    }
}

/// Non-empty bounded unique indices selected from a Runtime input request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct RuntimeInputSelections(Vec<u16>);

impl RuntimeInputSelections {
    /// Builds a bounded selection list.
    pub fn new(selections: Vec<u16>) -> Result<Self, RuntimeInputError> {
        if selections.is_empty() {
            return Err(RuntimeInputError::EmptySelection);
        }
        if selections.len() > MAX_RUNTIME_INPUT_SELECTIONS {
            return Err(RuntimeInputError::TooManySelections {
                max: MAX_RUNTIME_INPUT_SELECTIONS,
            });
        }
        if selections
            .iter()
            .enumerate()
            .any(|(index, selection)| selections[..index].iter().any(|prior| prior == selection))
        {
            return Err(RuntimeInputError::DuplicateSelection);
        }
        Ok(Self(selections))
    }

    /// Returns selected zero-based option indices.
    #[must_use]
    pub fn as_slice(&self) -> &[u16] {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RuntimeInputSelections {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(Vec::<u16>::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Bounded answer supplied for one exact Runtime input request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuntimeInputResponse {
    /// Bounded free-text input.
    Text {
        /// Text delivered only to the waiting Runtime request.
        text: BoundedText,
    },
    /// One or more zero-based indices into the request's choices.
    Options {
        /// Non-empty bounded unique selections.
        selections: RuntimeInputSelections,
    },
}

/// Runtime-facing result for a provider-native permission callback.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RuntimePermissionDecision {
    /// Permit the provider to execute its own tool exactly once.
    ///
    /// This is observation-only authority. It must never create or consume a
    /// COSH execution permit because the side effect remains provider-owned.
    ProviderNativeAllowOnce,
    /// Permit a non-ACP Runtime to execute its own local effect exactly once.
    ///
    /// This is observation-only authority and is intentionally distinct from
    /// an ACP provider callback decision.
    RuntimeNativeAllowOnce,
    /// Policy denied the Runtime request.
    Deny {
        /// Stable reason for denial.
        code: DenialCode,
        /// Redacted explanation safe to send to the Runtime.
        safe_message: BoundedText,
    },
}
