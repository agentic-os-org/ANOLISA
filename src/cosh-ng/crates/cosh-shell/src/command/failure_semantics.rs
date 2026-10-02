use crate::types::{CommandBlock, CommandOrigin, ShellEvent, ShellEventKind};

use super::{classify_exit, first_program_token, ExitCodeCategory};

const CLASSIFIER_MAX_LINES: usize = 120;
const CLASSIFIER_MAX_BYTES: usize = 8 * 1024;
const CLASSIFIER_SIDE_LINES: usize = CLASSIFIER_MAX_LINES / 2;
const CLASSIFIER_SIDE_BYTES: usize = CLASSIFIER_MAX_BYTES / 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FailureSemantics {
    pub(crate) class: FailureClass,
    pub(crate) confidence: FailureConfidence,
    pub(crate) auto_eligibility: FailureAutoEligibility,
    pub(crate) reasons: Vec<FailureReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureClass {
    Success,
    ExpectedNoResult,
    UsageOrHelp,
    InteractiveCancel,
    UserInterrupt,
    PipelineNormal,
    CommandNotFound,
    PermissionDenied,
    AbnormalSignal,
    BuildOrTestFailure,
    RuntimeException,
    GenericRuntimeFailure,
    ProviderOrInternalArtifact,
    UnknownFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureConfidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureAutoEligibility {
    LegacyAllowlisted,
    SuggestOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FailureReason {
    ExitCode(i32),
    Program(String),
    SignalLikeExit,
    CommandSpecificExpectedNonZero,
    UsageLine,
    HelpSuggestion,
    OptionParseError,
    MissingArgument,
    BuildFailureOutput,
    TestFailureOutput,
    CommandNotFound,
    PermissionDenied,
    InteractiveCancelOutput,
    UserInterruptEvent,
    OutputUnavailable,
    CommandFamily(BuildOrTestFamily),
    TerminalSignature(FailureTerminalSignature),
    ExcerptDirection(FailureExcerptDirection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildOrTestFamily {
    Cargo,
    Make,
    Ninja,
    Maven,
    Gradle,
    Npm,
    Pytest,
    GoTest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureTerminalSignature {
    CargoTest,
    CargoTestRerun,
    CargoBuild,
    Make,
    Ninja,
    Maven,
    Gradle,
    Npm,
    Pytest,
    GoTest,
    PythonTraceback,
    RustPanic,
    PermissionDenied,
    SegmentationFault,
    CoreDumped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailureExcerptDirection {
    Head,
    Tail,
}

pub(crate) fn classify_failure(
    block: &CommandBlock,
    events: &[ShellEvent],
    output_excerpt: Option<&str>,
) -> FailureSemantics {
    let mut reasons = vec![
        FailureReason::ExitCode(block.exit_code),
        FailureReason::Program(first_program_token(&block.command).to_string()),
    ];

    if block.exit_code == 0 {
        return semantics(FailureClass::Success, FailureConfidence::High, reasons);
    }

    if command_has_user_interrupt_event(events, block) {
        reasons.push(FailureReason::UserInterruptEvent);
        return semantics(
            FailureClass::UserInterrupt,
            FailureConfidence::High,
            reasons,
        );
    }

    if matches!(
        block.origin,
        CommandOrigin::ShellInternal | CommandOrigin::ProviderTool
    ) {
        return semantics(
            FailureClass::ProviderOrInternalArtifact,
            FailureConfidence::Medium,
            reasons,
        );
    }

    if block.exit_code > 128
        && !matches!(
            block.exit_code,
            130 | 132 | 134 | 135 | 136 | 137 | 139 | 141
        )
    {
        reasons.push(FailureReason::SignalLikeExit);
        return semantics(
            FailureClass::UnknownFailure,
            FailureConfidence::Low,
            reasons,
        );
    }

    let bounded_output = output_excerpt.map(BoundedOutput::new);
    let category = classify_exit(block.exit_code, &block.command);
    match category {
        ExitCodeCategory::Success => {
            return semantics(FailureClass::Success, FailureConfidence::High, reasons);
        }
        ExitCodeCategory::UserInterrupt => {
            reasons.push(FailureReason::SignalLikeExit);
            return semantics(
                FailureClass::UserInterrupt,
                FailureConfidence::High,
                reasons,
            );
        }
        ExitCodeCategory::PipelineNormal => {
            return semantics(
                FailureClass::PipelineNormal,
                FailureConfidence::High,
                reasons,
            );
        }
        ExitCodeCategory::CommandSpecificNormal => {
            reasons.push(FailureReason::CommandSpecificExpectedNonZero);
            return semantics(
                FailureClass::ExpectedNoResult,
                FailureConfidence::High,
                reasons,
            );
        }
        ExitCodeCategory::CommandNotFound => {
            reasons.push(FailureReason::CommandNotFound);
            return semantics(
                FailureClass::CommandNotFound,
                FailureConfidence::High,
                reasons,
            );
        }
        ExitCodeCategory::PermissionDenied => {
            reasons.push(FailureReason::PermissionDenied);
            if let Some(output) = &bounded_output {
                push_terminal_signature_reason(&mut reasons, output_permission_signature(output));
            }
            return semantics(
                FailureClass::PermissionDenied,
                FailureConfidence::High,
                reasons,
            );
        }
        ExitCodeCategory::AbnormalSignal => {
            reasons.push(FailureReason::SignalLikeExit);
            if let Some(output) = &bounded_output {
                push_terminal_signature_reason(
                    &mut reasons,
                    fatal_signal_signature(block.exit_code, output),
                );
            }
            return semantics(
                FailureClass::AbnormalSignal,
                FailureConfidence::High,
                reasons,
            );
        }
        ExitCodeCategory::GenericError => {}
    }

    let Some(output) = bounded_output else {
        reasons.push(FailureReason::OutputUnavailable);
        return semantics(
            FailureClass::UnknownFailure,
            FailureConfidence::Low,
            reasons,
        );
    };
    let normalized = output.text();
    if interactive_cancel_output(&normalized) {
        reasons.push(FailureReason::InteractiveCancelOutput);
        return semantics(
            FailureClass::InteractiveCancel,
            FailureConfidence::High,
            reasons,
        );
    }

    let usage = usage_help_signals(&normalized, block.exit_code);
    let family = build_or_test_family(&block.command);
    let build_or_test = family.and_then(|family| {
        terminal_summary_for_family(&output, family).map(|signature| (family, signature))
    });
    if let Some(family) = family {
        reasons.push(FailureReason::CommandFamily(family));
    }
    if !usage.explicit_option_parse_error() {
        if let Some((_, signature)) = build_or_test {
            reasons.push(FailureReason::TerminalSignature(signature));
            reasons.push(FailureReason::ExcerptDirection(
                FailureExcerptDirection::Tail,
            ));
            return semantics(
                FailureClass::BuildOrTestFailure,
                FailureConfidence::High,
                reasons,
            );
        }
    }

    if usage.high_confidence() {
        reasons.extend(usage.reasons);
        return semantics(FailureClass::UsageOrHelp, FailureConfidence::High, reasons);
    }
    if usage.medium_confidence() {
        reasons.extend(usage.reasons);
        return semantics(
            FailureClass::UsageOrHelp,
            FailureConfidence::Medium,
            reasons,
        );
    }

    if let Some(signature) = output_permission_signature(&output) {
        reasons.push(FailureReason::PermissionDenied);
        push_terminal_signature_reason(&mut reasons, Some(signature));
        return semantics(
            FailureClass::PermissionDenied,
            FailureConfidence::High,
            reasons,
        );
    }

    if family.is_none() {
        if let Some((signature, direction)) = runtime_exception_signature(&output) {
            reasons.push(FailureReason::TerminalSignature(signature));
            reasons.push(FailureReason::ExcerptDirection(direction));
            return semantics(
                FailureClass::RuntimeException,
                FailureConfidence::High,
                reasons,
            );
        }
    }

    semantics(
        FailureClass::GenericRuntimeFailure,
        FailureConfidence::Medium,
        reasons,
    )
}

fn semantics(
    class: FailureClass,
    confidence: FailureConfidence,
    reasons: Vec<FailureReason>,
) -> FailureSemantics {
    let auto_eligibility = failure_auto_eligibility(class, &reasons);
    FailureSemantics {
        class,
        confidence,
        auto_eligibility,
        reasons,
    }
}

fn failure_auto_eligibility(
    class: FailureClass,
    reasons: &[FailureReason],
) -> FailureAutoEligibility {
    let exit_code = reasons.iter().find_map(|reason| match reason {
        FailureReason::ExitCode(exit_code) => Some(*exit_code),
        _ => None,
    });
    match class {
        FailureClass::PermissionDenied if exit_code == Some(126) => {
            FailureAutoEligibility::LegacyAllowlisted
        }
        FailureClass::AbnormalSignal if matches!(exit_code, Some(134 | 136 | 137 | 139)) => {
            FailureAutoEligibility::LegacyAllowlisted
        }
        FailureClass::BuildOrTestFailure if legacy_build_or_test_signature(reasons) => {
            FailureAutoEligibility::LegacyAllowlisted
        }
        _ => FailureAutoEligibility::SuggestOnly,
    }
}

fn legacy_build_or_test_signature(reasons: &[FailureReason]) -> bool {
    let family = reasons.iter().find_map(|reason| match reason {
        FailureReason::CommandFamily(family) => Some(*family),
        _ => None,
    });
    let signature = reasons.iter().find_map(|reason| match reason {
        FailureReason::TerminalSignature(signature) => Some(*signature),
        _ => None,
    });
    matches!(
        (family, signature),
        (
            Some(BuildOrTestFamily::Cargo),
            Some(FailureTerminalSignature::CargoTest | FailureTerminalSignature::CargoBuild)
        ) | (
            Some(BuildOrTestFamily::Make),
            Some(FailureTerminalSignature::Make)
        ) | (
            Some(BuildOrTestFamily::Npm),
            Some(FailureTerminalSignature::Npm)
        ) | (
            Some(BuildOrTestFamily::Pytest),
            Some(FailureTerminalSignature::Pytest)
        )
    )
}

fn command_has_user_interrupt_event(events: &[ShellEvent], block: &CommandBlock) -> bool {
    events.iter().any(|event| {
        event.kind == ShellEventKind::UserInputIntercepted
            && event.component.as_deref() == Some("control")
            && event.input.as_deref() == Some("ctrl_c")
            && event.started_at_ms.is_some_and(|timestamp| {
                timestamp >= block.started_at_ms && timestamp <= block.ended_at_ms
            })
    })
}

fn normalize_output(output: &str) -> String {
    strip_ansi(output).to_ascii_lowercase()
}

fn strip_ansi(output: &str) -> String {
    let mut cleaned = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        cleaned.push(ch);
    }
    cleaned
}

#[derive(Default)]
struct UsageHelpSignals {
    usage_line: bool,
    help_suggestion: bool,
    option_parse_error: bool,
    missing_argument: bool,
    option_list_ratio_high: bool,
    reasons: Vec<FailureReason>,
}

impl UsageHelpSignals {
    fn explicit_option_parse_error(&self) -> bool {
        self.option_parse_error || self.missing_argument
    }

    fn high_confidence(&self) -> bool {
        (self.usage_line
            && (self.option_parse_error || self.missing_argument || self.help_suggestion))
            || (self.option_parse_error && self.help_suggestion)
            || self.option_list_ratio_high
    }

    fn medium_confidence(&self) -> bool {
        self.usage_line || self.option_parse_error || self.missing_argument
    }
}

fn usage_help_signals(output: &str, exit_code: i32) -> UsageHelpSignals {
    let mut signals = UsageHelpSignals::default();
    let lines = output.lines().filter(|line| !line.trim().is_empty());
    let mut total_lines = 0usize;
    let mut option_lines = 0usize;

    for line in lines {
        total_lines += 1;
        let trimmed = line.trim_start();
        if trimmed.starts_with("usage:") {
            signals.usage_line = true;
            push_reason_once(&mut signals.reasons, FailureReason::UsageLine);
        }
        if line.contains("try ") && line.contains("--help")
            || line.contains("for more information") && line.contains("--help")
            || line.contains("run ") && line.contains("--help")
        {
            signals.help_suggestion = true;
            push_reason_once(&mut signals.reasons, FailureReason::HelpSuggestion);
        }
        if [
            "unexpected argument",
            "unrecognized option",
            "unknown option",
            "no such option",
            "unrecognized arguments:",
            "unknown flag:",
            "unknown shorthand flag:",
            "flag provided but not defined",
            "invalid choice:",
        ]
        .iter()
        .any(|needle| line.contains(needle))
        {
            signals.option_parse_error = true;
            push_reason_once(&mut signals.reasons, FailureReason::OptionParseError);
        }
        if [
            "missing required",
            "missing argument",
            "required arguments were not provided",
        ]
        .iter()
        .any(|needle| line.contains(needle))
        {
            signals.missing_argument = true;
            push_reason_once(&mut signals.reasons, FailureReason::MissingArgument);
        }
        if is_option_list_line(trimmed) {
            option_lines += 1;
        }
    }

    if matches!(exit_code, 1 | 2 | 64)
        && total_lines >= 5
        && option_lines >= 3
        && option_lines * 10 >= total_lines * 3
    {
        signals.option_list_ratio_high = true;
        push_reason_once(&mut signals.reasons, FailureReason::UsageLine);
    }

    signals
}

fn is_option_list_line(line: &str) -> bool {
    line.starts_with("--")
        || line
            .strip_prefix('-')
            .and_then(|rest| rest.chars().next())
            .is_some_and(|ch| ch.is_ascii_alphanumeric())
}

#[path = "failure_signatures.rs"]
mod failure_signatures;

use failure_signatures::{
    build_or_test_family, fatal_signal_signature, output_permission_signature,
    push_terminal_signature_reason, runtime_exception_signature, terminal_summary_for_family,
    BoundedOutput,
};

/// Shared cancel-output matcher for failure classification and the
/// failure-analysis skip gate (`hooks::interrupt`). Needles are tiered by
/// specificity so real failures whose output merely mentions these words
/// stay analyzable (issue #2090). Input is normalized here (ANSI stripped,
/// lowercased) because raw callers pass terminal output with control
/// sequences; the pass is idempotent for pre-normalized callers.
pub(crate) fn interactive_cancel_output(output: &str) -> bool {
    let normalized = normalize_output(output);
    let phrase_hit = [
        "sudo: a password is required",
        "a password is required",
        "a terminal is required",
        "operation cancelled",
        "operation canceled",
        "keyboardinterrupt",
    ]
    .iter()
    .any(|needle| normalized.contains(needle));
    if phrase_hit {
        return true;
    }

    normalized.lines().any(|line| {
        let line = line.trim();
        // Single-word cancel markers must stand alone on their own line so
        // failures like "transfer was interrupted by peer" stay analyzable.
        if line == "interrupted" {
            return true;
        }
        // Credential needles only signal a cancel when the line itself is the
        // prompt label, optionally with a bracketed default, ending with ':'
        // (e.g. "access key id []:"); error headers that merely mention the
        // needle and end with ':' stay analyzable.
        ["access key id", "access key secret", "default region id"]
            .iter()
            .any(|needle| {
                line.strip_prefix(needle)
                    .and_then(|rest| rest.strip_suffix(':'))
                    .is_some_and(|middle| {
                        let middle = middle.trim();
                        middle.is_empty() || (middle.starts_with('[') && middle.ends_with(']'))
                    })
            })
    })
}

fn push_reason_once(reasons: &mut Vec<FailureReason>, reason: FailureReason) {
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

#[cfg(test)]
#[path = "failure_semantics_tests.rs"]
mod tests;
