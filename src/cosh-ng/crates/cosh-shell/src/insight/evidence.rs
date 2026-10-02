use crate::types::CommandBlock;

pub(crate) const TARGET_FACTS_MAX_BYTES: usize = 4 * 1024;
pub(crate) const EXCERPT_MAX_BYTES: usize = 12 * 1024;
pub(crate) const RELATED_FACTS_MAX_BYTES: usize = 8 * 1024;
pub(crate) const PROVIDER_CONTEXT_MAX_BYTES: usize = 24 * 1024;
pub(crate) const BUILD_TEST_SIDE_BYTES: usize = 6 * 1024;
const PROVIDER_ENVELOPE_MAX_BYTES: usize = 4 * 1024;

const MAX_EXCERPT_LINES: usize = 120;
const MAX_RELATED_FACTS: usize = 3;
const TRUNCATION_MARKER: &str = "... <truncated>";
const OPTIONAL_CONTEXT_MAX_BYTES: usize = PROVIDER_CONTEXT_MAX_BYTES
    - PROVIDER_ENVELOPE_MAX_BYTES
    - TARGET_FACTS_MAX_BYTES
    - EXCERPT_MAX_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundInsightMetadata {
    pub(crate) evidence_status: Option<String>,
    pub(crate) severity: String,
    pub(crate) confidence: String,
    pub(crate) evidence: Vec<String>,
}

pub(crate) fn take_bound_insight_metadata(
    hints: &mut Vec<String>,
    default_severity: &str,
    default_confidence: &str,
    default_evidence: Vec<String>,
) -> BoundInsightMetadata {
    let mut metadata = BoundInsightMetadata {
        evidence_status: None,
        severity: default_severity.to_string(),
        confidence: default_confidence.to_string(),
        evidence: default_evidence,
    };
    hints.retain(|hint| {
        if let Some(value) = hint.strip_prefix("__cosh_insight_evidence_status=") {
            metadata.evidence_status = Some(normalize_evidence_status(value).to_string());
            false
        } else if let Some(value) = hint.strip_prefix("__cosh_insight_severity=") {
            metadata.severity = value.to_string();
            false
        } else if let Some(value) = hint.strip_prefix("__cosh_insight_confidence=") {
            metadata.confidence = value.to_string();
            false
        } else if let Some(value) = hint.strip_prefix("__cosh_insight_evidence=") {
            if !metadata.evidence.iter().any(|existing| existing == value) {
                metadata.evidence.push(value.to_string());
            }
            false
        } else {
            true
        }
    });
    metadata
}

fn normalize_evidence_status(value: &str) -> &str {
    match value {
        "Available" => "available",
        "Truncated" | "truncated_at_capture" => "truncated",
        "Empty" => "empty",
        "Unavailable" => "unavailable",
        "Expired" => "expired",
        "ReadFailed" => "read_failed",
        "SanitizeFailed" => "sanitize_failed",
        "RedactionFailed" => "redaction_failed",
        _ => value,
    }
}

pub(crate) fn trim_optional_context_hints(hints: &mut Vec<String>) {
    while hints.iter().map(String::len).sum::<usize>() + hints.len().saturating_sub(1)
        > OPTIONAL_CONTEXT_MAX_BYTES
    {
        if hints.pop().is_none() {
            break;
        }
    }
}

pub(crate) fn provider_target_facts(
    block: &CommandBlock,
    execution_scope: &str,
    origin: &str,
    evidence_status: &str,
    redaction_status: &str,
    truncation_status: &str,
    metadata: &BoundInsightMetadata,
) -> ProviderTargetFacts {
    let facts = crate::evidence::provider_safe_command_facts(block);
    let structured_evidence = truncate_head(&metadata.evidence.join(","), 1024);
    let command_id = truncate_head(&facts.id, 256);
    let execution_scope = truncate_head(execution_scope, 256);
    let origin = truncate_head(origin, 64);
    let cwd = truncate_head(&facts.cwd, 512);
    let end_cwd = truncate_head(&facts.end_cwd, 512);
    let output_id = truncate_head(&facts.output_id, 512);
    let before_truncation = format!(
        "command_id={}; exit_code={}; execution_scope={execution_scope}; origin={origin}; evidence_status={evidence_status}; redaction_status={redaction_status}; truncation_status=",
        command_id,
        facts.exit_code,
    );
    let after_truncation = format!(
        "; severity={}; confidence={}; structured_evidence={structured_evidence}; cwd={}; end_cwd={}; duration_ms={}; output_bytes={}; output_id={}; output_stability={}; command=",
        metadata.severity,
        metadata.confidence,
        cwd,
        end_cwd,
        facts.duration_ms,
        facts.output_bytes,
        output_id,
        facts.output_stability,
    );
    let truncation_status = TargetTruncationStatus::from_str(truncation_status);
    let command_budget = TARGET_FACTS_MAX_BYTES
        .saturating_sub(before_truncation.len())
        .saturating_sub(truncation_status.as_str().len())
        .saturating_sub(after_truncation.len());
    ProviderTargetFacts {
        before_truncation,
        truncation_status: Some(truncation_status),
        after_truncation: format!(
            "{after_truncation}{}",
            truncate_head(&facts.command, command_budget)
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetTruncationStatus {
    Complete,
    Truncated,
}

impl TargetTruncationStatus {
    fn from_str(value: &str) -> Self {
        if value == "truncated" {
            Self::Truncated
        } else {
            Self::Complete
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Truncated => "truncated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderTargetFacts {
    before_truncation: String,
    truncation_status: Option<TargetTruncationStatus>,
    after_truncation: String,
}

impl ProviderTargetFacts {
    fn render(&self, max_bytes: usize) -> String {
        let truncation_status = self
            .truncation_status
            .map(TargetTruncationStatus::as_str)
            .unwrap_or_default();
        truncate_head(
            &format!(
                "{}{}{}",
                self.before_truncation, truncation_status, self.after_truncation
            ),
            max_bytes,
        )
    }

    fn mark_excerpt_truncated(&mut self) {
        if self.truncation_status.is_some() {
            self.truncation_status = Some(TargetTruncationStatus::Truncated);
        }
    }

    #[cfg(test)]
    fn plain(value: impl Into<String>) -> Self {
        Self {
            before_truncation: value.into(),
            truncation_status: None,
            after_truncation: String::new(),
        }
    }

    #[cfg(test)]
    fn with_status_parts(before: &str, after: &str) -> Self {
        Self {
            before_truncation: before.to_string(),
            truncation_status: Some(TargetTruncationStatus::Complete),
            after_truncation: after.to_string(),
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.render(usize::MAX).len()
    }

    #[cfg(test)]
    fn contains(&self, pattern: &str) -> bool {
        self.render(usize::MAX).contains(pattern)
    }
}

impl std::fmt::Display for ProviderTargetFacts {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.render(usize::MAX))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidenceScenario {
    FailureClassifier,
    BuildOrTest,
    RuntimeException,
    CommandNotFound,
    PermissionDenied,
    AbnormalSignal,
    FreeMemory,
    TopProcesses,
    PsProcesses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExcerptDirection {
    Head,
    Tail,
    HeadTail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScenarioPolicy {
    pub(crate) direction: ExcerptDirection,
    pub(crate) max_lines: usize,
    pub(crate) max_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundedExcerpt {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

pub(crate) fn scenario_policy(scenario: EvidenceScenario) -> ScenarioPolicy {
    match scenario {
        EvidenceScenario::FailureClassifier => ScenarioPolicy {
            direction: ExcerptDirection::Head,
            max_lines: MAX_EXCERPT_LINES,
            max_bytes: 8 * 1024,
        },
        EvidenceScenario::BuildOrTest | EvidenceScenario::RuntimeException => ScenarioPolicy {
            direction: ExcerptDirection::HeadTail,
            max_lines: MAX_EXCERPT_LINES,
            max_bytes: EXCERPT_MAX_BYTES,
        },
        EvidenceScenario::CommandNotFound
        | EvidenceScenario::PermissionDenied
        | EvidenceScenario::AbnormalSignal => ScenarioPolicy {
            direction: ExcerptDirection::Tail,
            max_lines: MAX_EXCERPT_LINES,
            max_bytes: EXCERPT_MAX_BYTES,
        },
        EvidenceScenario::FreeMemory
        | EvidenceScenario::TopProcesses
        | EvidenceScenario::PsProcesses => ScenarioPolicy {
            direction: ExcerptDirection::Head,
            max_lines: MAX_EXCERPT_LINES,
            max_bytes: EXCERPT_MAX_BYTES,
        },
    }
}

pub(crate) fn bounded_excerpt(text: &str, scenario: EvidenceScenario) -> BoundedExcerpt {
    let policy = scenario_policy(scenario);
    match policy.direction {
        ExcerptDirection::Head => bounded_one_side(text, policy, true),
        ExcerptDirection::Tail => bounded_one_side(text, policy, false),
        ExcerptDirection::HeadTail => bounded_head_tail(text, policy),
    }
}

fn bounded_one_side(text: &str, policy: ScenarioPolicy, head: bool) -> BoundedExcerpt {
    let lines = text.lines().collect::<Vec<_>>();
    let line_truncated = lines.len() > policy.max_lines;
    let selected = if head {
        lines
            .iter()
            .take(policy.max_lines)
            .copied()
            .collect::<Vec<_>>()
    } else {
        lines
            .iter()
            .rev()
            .take(policy.max_lines)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
    };
    let selected = selected.join("\n");
    let byte_truncated = selected.len() > policy.max_bytes;
    let text = if byte_truncated {
        if head {
            truncate_head(&selected, policy.max_bytes)
        } else {
            truncate_tail(&selected, policy.max_bytes)
        }
    } else if line_truncated {
        if head {
            with_marker_after(&selected, policy.max_bytes)
        } else {
            with_marker_before(&selected, policy.max_bytes)
        }
    } else {
        selected
    };
    BoundedExcerpt {
        text,
        truncated: line_truncated || byte_truncated,
    }
}

fn bounded_head_tail(text: &str, policy: ScenarioPolicy) -> BoundedExcerpt {
    let lines = text.lines().collect::<Vec<_>>();
    if lines.len() <= policy.max_lines && text.len() <= policy.max_bytes {
        return BoundedExcerpt {
            text: text.to_string(),
            truncated: false,
        };
    }

    let side_lines = policy.max_lines / 2;
    let head_source = if lines.len() > policy.max_lines {
        lines
            .iter()
            .take(side_lines)
            .copied()
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text.to_string()
    };
    let tail_source = if lines.len() > policy.max_lines {
        lines
            .iter()
            .rev()
            .take(side_lines)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        text.to_string()
    };
    let separator = format!("\n{TRUNCATION_MARKER}\n");
    let head = utf8_prefix(&head_source, BUILD_TEST_SIDE_BYTES);
    let tail_budget = policy
        .max_bytes
        .saturating_sub(head.len())
        .saturating_sub(separator.len())
        .min(BUILD_TEST_SIDE_BYTES);
    let tail = utf8_suffix(&tail_source, tail_budget);

    BoundedExcerpt {
        text: format!("{head}{separator}{tail}"),
        truncated: true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceBundleInput {
    pub(crate) target_facts: ProviderTargetFacts,
    pub(crate) target_excerpt: String,
    pub(crate) related_facts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EvidenceBundle {
    pub(crate) target_facts: String,
    pub(crate) target_excerpt: String,
    pub(crate) related_facts: Vec<String>,
    pub(crate) removed_related_facts: usize,
    pub(crate) related_truncated: bool,
    pub(crate) target_excerpt_truncated: bool,
    pub(crate) serialized_bytes: usize,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BundleBudget {
    pub(crate) target_facts_bytes: usize,
    pub(crate) target_excerpt_bytes: usize,
    pub(crate) related_facts_bytes: usize,
    pub(crate) total_bytes: usize,
}

pub(crate) fn build_evidence_bundle(input: EvidenceBundleInput) -> EvidenceBundle {
    build_evidence_bundle_with_budget(
        input,
        BundleBudget {
            target_facts_bytes: TARGET_FACTS_MAX_BYTES,
            target_excerpt_bytes: EXCERPT_MAX_BYTES,
            related_facts_bytes: RELATED_FACTS_MAX_BYTES,
            total_bytes: PROVIDER_CONTEXT_MAX_BYTES,
        },
    )
}

pub(crate) fn build_provider_evidence_payload(
    input: EvidenceBundleInput,
    other_context_bytes: usize,
) -> String {
    let content_budget = PROVIDER_CONTEXT_MAX_BYTES
        .saturating_sub(other_context_bytes)
        .saturating_sub(PROVIDER_ENVELOPE_MAX_BYTES);
    let bundle = build_evidence_bundle_with_budget(
        input,
        BundleBudget {
            target_facts_bytes: TARGET_FACTS_MAX_BYTES.min(content_budget),
            target_excerpt_bytes: EXCERPT_MAX_BYTES.min(content_budget),
            related_facts_bytes: RELATED_FACTS_MAX_BYTES.min(content_budget),
            total_bytes: content_budget,
        },
    );
    let payload = serialize_evidence_bundle(&bundle);
    debug_assert!(payload.len() + other_context_bytes <= PROVIDER_CONTEXT_MAX_BYTES);
    payload
}

fn serialize_evidence_bundle(bundle: &EvidenceBundle) -> String {
    format!(
        "insight_evidence\n\
         bundle_status: target_excerpt_truncated={}; related_truncated={}; removed_related_facts={}\n\
         target_facts:\n{}\n\
         target_excerpt:\n{}\n\
         related_facts:\n{}",
        bundle.target_excerpt_truncated,
        bundle.related_truncated,
        bundle.removed_related_facts,
        bundle.target_facts,
        bundle.target_excerpt,
        bundle.related_facts.join("\n")
    )
}

pub(crate) fn build_evidence_bundle_with_budget(
    input: EvidenceBundleInput,
    budget: BundleBudget,
) -> EvidenceBundle {
    let mut target_facts_input = input.target_facts;
    let mut target_facts = target_facts_input.render(budget.target_facts_bytes);
    let mut target_excerpt = truncate_head(&input.target_excerpt, budget.target_excerpt_bytes);
    let mut target_excerpt_truncated = target_excerpt != input.target_excerpt;
    let mut related_facts = input.related_facts;
    let mut removed_related_facts = 0;
    let mut related_truncated = false;

    while related_facts.len() > MAX_RELATED_FACTS {
        related_facts.remove(0);
        removed_related_facts += 1;
    }
    while related_bytes(&related_facts) > budget.related_facts_bytes && related_facts.len() > 1 {
        related_facts.remove(0);
        removed_related_facts += 1;
    }
    if related_bytes(&related_facts) > budget.related_facts_bytes {
        related_facts[0] = truncate_head(&related_facts[0], budget.related_facts_bytes);
        related_truncated = true;
    }

    while bundle_bytes(&target_facts, &target_excerpt, &related_facts) > budget.total_bytes
        && related_facts.len() > 1
    {
        related_facts.remove(0);
        removed_related_facts += 1;
    }
    if bundle_bytes(&target_facts, &target_excerpt, &related_facts) > budget.total_bytes
        && !related_facts.is_empty()
    {
        let available = budget
            .total_bytes
            .saturating_sub(target_facts.len())
            .saturating_sub(target_excerpt.len());
        if available == 0 {
            related_facts.clear();
            removed_related_facts += 1;
        } else {
            related_facts[0] = truncate_head(&related_facts[0], available);
            related_truncated = true;
        }
    }
    if bundle_bytes(&target_facts, &target_excerpt, &related_facts) > budget.total_bytes {
        let available = budget
            .total_bytes
            .saturating_sub(target_facts.len())
            .saturating_sub(related_bytes(&related_facts));
        target_excerpt = truncate_head(&target_excerpt, available);
        target_excerpt_truncated = true;
    }
    if target_excerpt_truncated {
        target_facts_input.mark_excerpt_truncated();
        target_facts = target_facts_input.render(budget.target_facts_bytes);
        if bundle_bytes(&target_facts, &target_excerpt, &related_facts) > budget.total_bytes {
            let available = budget
                .total_bytes
                .saturating_sub(target_facts.len())
                .saturating_sub(related_bytes(&related_facts));
            target_excerpt = truncate_head(&target_excerpt, available);
        }
    }

    let serialized_bytes = bundle_bytes(&target_facts, &target_excerpt, &related_facts);
    EvidenceBundle {
        target_facts,
        target_excerpt,
        related_facts,
        removed_related_facts,
        related_truncated,
        target_excerpt_truncated,
        serialized_bytes,
    }
}

fn bundle_bytes(target_facts: &str, target_excerpt: &str, related_facts: &[String]) -> usize {
    target_facts.len() + target_excerpt.len() + related_bytes(related_facts)
}

fn related_bytes(facts: &[String]) -> usize {
    facts.iter().map(String::len).sum()
}

fn truncate_head(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes <= TRUNCATION_MARKER.len() {
        return TRUNCATION_MARKER[..max_bytes].to_string();
    }
    let content_bytes = max_bytes - TRUNCATION_MARKER.len();
    format!("{}{TRUNCATION_MARKER}", utf8_prefix(value, content_bytes))
}

fn truncate_tail(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    if max_bytes <= TRUNCATION_MARKER.len() {
        return TRUNCATION_MARKER[..max_bytes].to_string();
    }
    let content_bytes = max_bytes - TRUNCATION_MARKER.len();
    format!("{TRUNCATION_MARKER}{}", utf8_suffix(value, content_bytes))
}

fn with_marker_after(value: &str, max_bytes: usize) -> String {
    truncate_head(&format!("{value}\n{TRUNCATION_MARKER}"), max_bytes)
}

fn with_marker_before(value: &str, max_bytes: usize) -> String {
    truncate_tail(&format!("{TRUNCATION_MARKER}\n{value}"), max_bytes)
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn utf8_suffix(value: &str, max_bytes: usize) -> &str {
    let mut start = value.len().saturating_sub(max_bytes);
    while start < value.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    &value[start..]
}

#[cfg(test)]
#[path = "evidence/tests.rs"]
mod tests;
