use crate::types::CommandBlock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObservedCommand<'a> {
    pub(crate) block: &'a CommandBlock,
    pub(crate) output_excerpt: Option<&'a str>,
    pub(crate) output_status: OutputExcerptStatus,
    pub(crate) scope: ExecutionScope,
    pub(crate) intent: CommandIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputExcerptStatus {
    Available,
    Truncated,
    Empty,
    Unavailable,
    Expired,
    ReadFailed,
    SanitizeFailed,
    RedactionFailed,
}

impl OutputExcerptStatus {
    pub(crate) fn is_usable(self, excerpt: Option<&str>) -> bool {
        if !matches!(self, Self::Available | Self::Truncated) {
            return false;
        }
        excerpt
            .map(str::trim)
            .map(|text| text.strip_suffix("... <truncated>").unwrap_or(text).trim())
            .is_some_and(|text| !text.is_empty())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ExecutionScope {
    pub(crate) session_id: String,
    pub(crate) kind: ExecutionScopeKind,
    pub(crate) identity: Option<String>,
}

impl ExecutionScope {
    pub(crate) fn local(session_id: impl Into<String>) -> Self {
        let session_id = session_id.into();
        Self {
            identity: Some(session_id.clone()),
            session_id,
            kind: ExecutionScopeKind::LocalHost,
        }
    }

    pub(crate) fn unknown(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            kind: ExecutionScopeKind::UnknownWrapper,
            identity: None,
        }
    }

    pub(crate) fn allows_correlation(&self) -> bool {
        self.kind == ExecutionScopeKind::LocalHost && self.identity.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ExecutionScopeKind {
    LocalHost,
    UnknownWrapper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum CommandIntent {
    RepairCommand,
    AnalyzeFailure,
    DiagnoseMemoryPressure,
    DiagnoseProcessMemory,
    DiagnoseMemoryRootCause,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum SuppressionTopic {
    CommandNotFound,
    PermissionDenied,
    BuildOrTestFailure,
    RuntimeException,
    AbnormalSignal,
    MemoryPressure,
    HighMemoryProcess,
    MemoryRootCause,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum EntityKey {
    Program(String),
    SystemMemory,
    Process(String),
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct SuppressionKey {
    pub(crate) version: u8,
    pub(crate) topic: SuppressionTopic,
    pub(crate) entity: EntityKey,
    pub(crate) scope: ExecutionScope,
    pub(crate) intent: CommandIntent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsightSource {
    FailedCommand,
    Free,
    Top,
    Ps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum InsightSeverity {
    Candidate,
    Warning,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InsightConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InsightEvidence {
    pub(crate) key: String,
    pub(crate) value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InsightTarget {
    pub(crate) insight_id: String,
    pub(crate) source_session_id: String,
    pub(crate) source_command_block_id: String,
    pub(crate) scope: ExecutionScope,
    pub(crate) evidence_handle: Option<String>,
    pub(crate) evidence_status: OutputExcerptStatus,
    pub(crate) severity: InsightSeverity,
    pub(crate) confidence: InsightConfidence,
    pub(crate) evidence: Vec<InsightEvidence>,
    pub(crate) created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InsightBinding {
    pub(crate) suggestion_id: String,
    pub(crate) target: InsightTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptSuggestion {
    ShellRewrite { text: String },
    AgentPrompt { binding: Box<InsightBinding> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InsightCandidate {
    pub(crate) source: InsightSource,
    pub(crate) topic: SuppressionTopic,
    pub(crate) entity: EntityKey,
    pub(crate) severity: InsightSeverity,
    pub(crate) confidence: InsightConfidence,
    pub(crate) evidence: Vec<InsightEvidence>,
    pub(crate) suggestion: Option<PromptSuggestion>,
    pub(crate) scope: ExecutionScope,
    pub(crate) suppression_key: SuppressionKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InlineInsight {
    pub(crate) topic: SuppressionTopic,
    pub(crate) entity: EntityKey,
    pub(crate) severity: InsightSeverity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterventionDecision {
    Silent,
    Suggest {
        insight: InlineInsight,
        suggestion: PromptSuggestion,
    },
    AutoAnalyze {
        activity: InlineInsight,
        target: InsightTarget,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_and_truncated_non_marker_content_is_usable() {
        assert!(OutputExcerptStatus::Available.is_usable(Some("error: build failed")));
        assert!(
            OutputExcerptStatus::Truncated.is_usable(Some("error: build failed\n... <truncated>"))
        );
    }

    #[test]
    fn empty_marker_only_and_failed_excerpt_states_are_not_usable() {
        assert!(!OutputExcerptStatus::Available.is_usable(None));
        assert!(!OutputExcerptStatus::Available.is_usable(Some(" \n\t")));
        assert!(!OutputExcerptStatus::Truncated.is_usable(Some("... <truncated>")));
        for status in [
            OutputExcerptStatus::Empty,
            OutputExcerptStatus::Unavailable,
            OutputExcerptStatus::Expired,
            OutputExcerptStatus::ReadFailed,
            OutputExcerptStatus::SanitizeFailed,
            OutputExcerptStatus::RedactionFailed,
        ] {
            assert!(!status.is_usable(Some("error: build failed")), "{status:?}");
        }
    }
}
