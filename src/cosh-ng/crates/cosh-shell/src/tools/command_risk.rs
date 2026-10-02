use super::broker::can_run_approved_bash_tool;
use super::command_risk_build::{
    apply_null_redirection_policy, assessment, basename, command_requires_tty, dedupe_reasons,
    downloaded_program_file, has_interpreter_inline_code, has_tty_arg, high_risk_program,
    high_shell_syntax, interpreter_consumes_stdin_as_program, max_output_exposure,
    max_output_stability, min_confidence, network_download_effect,
};
use super::command_risk_compound::{assess_stripped_compound, compound_segments, finalize_complex};
use super::command_risk_parser::{is_env_assignment, parse_command, ParsedCommand};
use super::command_risk_pipeline::assess_pipeline;
use super::command_risk_verdict::high_risk_program_assessment;
use super::guarded_diagnostic::validate_guarded_diagnostic;
use super::is_sensitive_target;
use super::readonly_compound::build_readonly_compound_plan;
use super::readonly_pipeline::validate_readonly_pipeline;

pub use super::command_risk_model::{
    is_high_risk_explanation, AssessmentConfidence, AssessmentPolicy, AssessmentSource,
    AssessmentSummary, AutoAllowEvidence, AutoExecutionPolicy, AutoExecutionRoute,
    CommandAssessment, CommandShape, ExecutionDecision, InteractionRequirement, OutputExposure,
    OutputStability, ReadonlyEvidence, RiskImpact, RiskReason, SideEffectClass,
    HIGH_RISK_EXPLANATION_REASONS,
};

pub fn assess_shell_command(command: &str, policy: AssessmentPolicy) -> CommandAssessment {
    let command = command.trim();
    let parsed = parse_command(command);
    if command.is_empty() {
        return assessment(
            policy.source,
            command,
            parsed.shape,
            ExecutionDecision::AskUser,
            RiskImpact::Medium,
            AssessmentConfidence::Low,
            InteractionRequirement::None,
            OutputStability::StableSnapshot,
            OutputExposure::Normal,
            vec![SideEffectClass::Unknown],
            vec!["empty-command"],
            None,
        );
    }
    if command.contains('\0') {
        return assessment(
            policy.source,
            command,
            parsed.shape,
            ExecutionDecision::Block,
            RiskImpact::High,
            AssessmentConfidence::High,
            InteractionRequirement::None,
            OutputStability::StableSnapshot,
            OutputExposure::Normal,
            vec![SideEffectClass::Unknown],
            vec!["unsafe-binding"],
            None,
        );
    }
    if parsed.shape == CommandShape::Unparseable {
        return assessment(
            policy.source,
            command,
            parsed.shape,
            ExecutionDecision::AskUser,
            RiskImpact::High,
            AssessmentConfidence::Low,
            InteractionRequirement::None,
            OutputStability::StableSnapshot,
            OutputExposure::Normal,
            vec![SideEffectClass::Unknown],
            vec!["parse-failed"],
            None,
        );
    }
    if parsed.shape == CommandShape::CommandSubstitution {
        return high_shell_syntax(policy.source, command, parsed.shape, "command-substitution");
    }
    if parsed.shape == CommandShape::RedirectionWrite {
        return high_shell_syntax(policy.source, command, parsed.shape, "redirection-write");
    }

    let null_redirections = parsed.null_redirections;
    if !null_redirections.is_empty() && parsed.shape == CommandShape::Complex {
        // Subshells, brace groups, and background syntax cannot be
        // reliably segmented; keep the pre-fix fail-closed classification
        // for redirection-carrying complex commands.
        return high_shell_syntax(
            policy.source,
            command,
            CommandShape::RedirectionWrite,
            "redirection-write",
        );
    }
    let mut result = if let Some(segments) = compound_segments(&parsed) {
        // Compound commands are assessed per segment and aggregated so
        // high-risk tails keep their full stage assessment (issue #1785);
        // non-compound shapes keep the shape-specific paths below.
        assess_stripped_compound(command, parsed.shape, &segments, policy)
    } else {
        match parsed.shape {
            CommandShape::Simple | CommandShape::EnvSimple => {
                assess_simple_command(command, parsed, policy)
            }
            CommandShape::Pipeline => assess_pipeline(command, parsed, policy),
            CommandShape::AndOrList | CommandShape::Sequence | CommandShape::RedirectionRead => {
                let mut simple = assess_first_stage(command, &parsed, policy);
                simple.shape = parsed.shape;
                simple.execution = ExecutionDecision::AskUser;
                simple.confidence = min_confidence(simple.confidence, AssessmentConfidence::Medium);
                insert_structural_reason(
                    &mut simple.reasons,
                    match parsed.shape {
                        CommandShape::AndOrList => "and-or-list-not-auto-executable",
                        CommandShape::Sequence => "sequence-not-auto-executable",
                        CommandShape::RedirectionRead => "read-redirection-not-auto-executable",
                        _ => "complex-shell-not-auto-executable",
                    },
                );
                simple
            }
            CommandShape::Complex => {
                let mut simple = assess_first_stage(command, &parsed, policy);
                simple.shape = parsed.shape;
                finalize_complex(&mut simple, &parsed);
                simple
            }
            CommandShape::Empty
            | CommandShape::Unparseable
            | CommandShape::CommandSubstitution
            | CommandShape::RedirectionWrite => unreachable!("handled above"),
        }
    };
    if !null_redirections.is_empty() {
        apply_null_redirection_policy(&mut result);
    }
    result
}

/// Conditional head-insert for structural reasons (ARP SDD design.md §1):
/// structural verdicts outrank fallback/neutral observations from the first
/// stage, but must not displace a high-risk explanation as the primary reason.
pub(super) fn insert_structural_reason(reasons: &mut Vec<&'static str>, structural: &'static str) {
    let index = match reasons.first() {
        Some(first) if is_high_risk_explanation(first) => 1,
        _ => 0,
    };
    reasons.insert(index.min(reasons.len()), structural);
}

pub fn blocked_shell_binding_assessment(
    source: AssessmentSource,
    command: &str,
    reason: &'static str,
) -> CommandAssessment {
    assessment(
        source,
        command.trim(),
        CommandShape::Unparseable,
        ExecutionDecision::Block,
        RiskImpact::High,
        AssessmentConfidence::High,
        InteractionRequirement::None,
        OutputStability::StableSnapshot,
        OutputExposure::Normal,
        vec![SideEffectClass::Unknown],
        vec![reason],
        None,
    )
}

pub(super) fn assess_simple_command(
    command: &str,
    parsed: ParsedCommand,
    policy: AssessmentPolicy,
) -> CommandAssessment {
    let tokens = parsed.stages.first().cloned().unwrap_or_default();
    let program_index = tokens
        .iter()
        .position(|token| !is_env_assignment(token))
        .unwrap_or(0);
    let command_tokens = &tokens[program_index..];
    let Some(program) = command_tokens
        .first()
        .map(|token| basename(token).to_string())
    else {
        return assessment(
            policy.source,
            command,
            parsed.shape,
            ExecutionDecision::AskUser,
            RiskImpact::Medium,
            AssessmentConfidence::Low,
            InteractionRequirement::None,
            OutputStability::StableSnapshot,
            OutputExposure::Normal,
            vec![SideEffectClass::Unknown],
            vec!["empty-command"],
            None,
        );
    };
    let sensitive = command_tokens
        .iter()
        .any(|token| is_sensitive_target(token));
    if sensitive {
        return assessment(
            policy.source,
            command,
            parsed.shape,
            ExecutionDecision::AskUser,
            RiskImpact::High,
            AssessmentConfidence::High,
            InteractionRequirement::None,
            OutputStability::StableSnapshot,
            OutputExposure::MayContainSecrets,
            vec![SideEffectClass::SensitiveDataRead],
            vec!["sensitive-path"],
            None,
        );
    }

    if let Some(high) = high_risk_program_assessment(
        policy.source,
        command,
        parsed.shape,
        &program,
        command_tokens,
    ) {
        return high;
    }

    let mut stage = stage_assessment(&program, command_tokens);
    if stage.impact != RiskImpact::High
        && stage.interaction == InteractionRequirement::None
        && parsed.null_redirections.is_stderr_only()
        && build_readonly_compound_plan(command).is_some()
    {
        let evidence = AutoAllowEvidence::StderrSuppressedReadonly;
        stage.impact = RiskImpact::Low;
        stage.confidence = AssessmentConfidence::High;
        stage.reasons.insert(0, evidence.reason_code());
        return finalize_simple(policy, command, parsed.shape, stage, Some(evidence));
    }
    if let Some(readonly) = direct_readonly_evidence(command) {
        stage.impact = RiskImpact::Low;
        stage.confidence = AssessmentConfidence::High;
        stage.reasons.insert(0, readonly.reason_code());
        return finalize_simple(
            policy,
            command,
            parsed.shape,
            stage,
            Some(readonly.auto_allow()),
        );
    }

    if is_safe_diagnostic_family(&program) {
        let guarded_evidence =
            policy.guarded_diagnostic_executor && validate_guarded_diagnostic(command).is_ok();
        stage.impact = if policy.auto_mode && guarded_evidence {
            RiskImpact::Low
        } else {
            RiskImpact::Medium
        };
        stage.confidence = AssessmentConfidence::High;
        stage.reasons.insert(0, "safe-diagnostic-family");
        return finalize_simple(
            policy,
            command,
            parsed.shape,
            stage,
            (policy.auto_mode && guarded_evidence).then_some(AutoAllowEvidence::GuardedDiagnostic),
        );
    }

    finalize_simple(policy, command, parsed.shape, stage, None)
}

fn direct_readonly_evidence(command: &str) -> Option<ReadonlyEvidence> {
    can_run_approved_bash_tool(command)
        .is_ok()
        .then_some(ReadonlyEvidence::DirectReadonlyBroker)
}

fn assess_first_stage(
    command: &str,
    parsed: &ParsedCommand,
    policy: AssessmentPolicy,
) -> CommandAssessment {
    let simple = ParsedCommand {
        shape: if parsed.shape == CommandShape::EnvSimple {
            CommandShape::EnvSimple
        } else {
            CommandShape::Simple
        },
        stages: parsed.stages.first().cloned().into_iter().collect(),
        requires_shell_expansion: parsed.requires_shell_expansion,
        null_redirections: Default::default(),
        segments: Vec::new(),
        segment_connectors: Vec::new(),
    };
    assess_simple_command(command, simple, policy)
}

fn finalize_simple(
    policy: AssessmentPolicy,
    command: &str,
    shape: CommandShape,
    stage: StageAssessment,
    evidence: Option<AutoAllowEvidence>,
) -> CommandAssessment {
    let auto_allow = evidence.filter(|_| policy.auto_mode);
    let execution = if auto_allow.is_some() {
        ExecutionDecision::AutoAllow
    } else if stage.interaction == InteractionRequirement::TtyRequired {
        ExecutionDecision::ForegroundHandoffRequired
    } else {
        ExecutionDecision::AskUser
    };
    assessment(
        policy.source,
        command,
        shape,
        execution,
        stage.impact,
        stage.confidence,
        stage.interaction,
        stage.output_stability,
        stage.output_exposure,
        stage.side_effects,
        dedupe_reasons(stage.reasons),
        auto_allow,
    )
}

#[derive(Debug, Clone)]
pub(super) struct StageAssessment {
    pub(super) impact: RiskImpact,
    pub(super) confidence: AssessmentConfidence,
    pub(super) interaction: InteractionRequirement,
    pub(super) output_stability: OutputStability,
    pub(super) output_exposure: OutputExposure,
    pub(super) side_effects: Vec<SideEffectClass>,
    pub(super) reasons: Vec<&'static str>,
}

pub(super) fn stage_assessment(program: &str, tokens: &[String]) -> StageAssessment {
    if has_interpreter_inline_code(program, tokens) {
        return StageAssessment {
            impact: RiskImpact::High,
            confidence: AssessmentConfidence::High,
            interaction: InteractionRequirement::None,
            output_stability: OutputStability::PotentiallyLarge,
            output_exposure: OutputExposure::Normal,
            side_effects: vec![SideEffectClass::RemoteCodeExecution],
            reasons: vec!["remote-code-execution"],
        };
    }
    if command_requires_tty(program, tokens) {
        return StageAssessment {
            impact: RiskImpact::Medium,
            confidence: AssessmentConfidence::High,
            interaction: InteractionRequirement::TtyRequired,
            output_stability: OutputStability::UnstableInteractive,
            output_exposure: OutputExposure::Normal,
            side_effects: vec![SideEffectClass::None],
            reasons: vec!["requires-tty"],
        };
    }
    if program == "top" {
        return StageAssessment {
            impact: RiskImpact::Medium,
            confidence: AssessmentConfidence::High,
            interaction: if top_is_batch_snapshot(tokens) {
                InteractionRequirement::None
            } else {
                InteractionRequirement::TtyRequired
            },
            output_stability: if top_is_batch_snapshot(tokens) {
                OutputStability::StableSnapshot
            } else {
                OutputStability::Streaming
            },
            output_exposure: OutputExposure::MayContainCommandLine,
            side_effects: vec![SideEffectClass::None],
            reasons: vec!["streaming-diagnostic"],
        };
    }
    if program == "awk" {
        let high = tokens.iter().any(|token| {
            has_awk_system_call(token) || token.contains("getline") || token.contains('>')
        });
        return StageAssessment {
            impact: if high {
                RiskImpact::High
            } else {
                RiskImpact::Medium
            },
            confidence: AssessmentConfidence::Medium,
            interaction: InteractionRequirement::None,
            output_stability: OutputStability::PotentiallyLarge,
            output_exposure: OutputExposure::Normal,
            side_effects: if high {
                vec![SideEffectClass::RemoteCodeExecution]
            } else {
                vec![SideEffectClass::None]
            },
            reasons: vec![if high {
                "awk-shell-execution"
            } else {
                "awk-not-auto-allowlisted"
            }],
        };
    }
    if matches!(program, "curl" | "wget") {
        return StageAssessment {
            impact: RiskImpact::Medium,
            confidence: AssessmentConfidence::Medium,
            interaction: InteractionRequirement::None,
            output_stability: OutputStability::PotentiallyLarge,
            output_exposure: OutputExposure::Normal,
            side_effects: vec![SideEffectClass::NetworkRead],
            reasons: vec!["network-read"],
        };
    }
    if matches!(program, "cargo" | "npm" | "make") {
        return StageAssessment {
            impact: RiskImpact::Medium,
            confidence: AssessmentConfidence::Medium,
            interaction: InteractionRequirement::None,
            output_stability: OutputStability::PotentiallyLarge,
            output_exposure: OutputExposure::Normal,
            side_effects: vec![SideEffectClass::Unknown],
            reasons: vec!["build-or-test-command"],
        };
    }
    if matches!(program, "df" | "ps") {
        return StageAssessment {
            impact: RiskImpact::Medium,
            confidence: AssessmentConfidence::High,
            interaction: InteractionRequirement::None,
            output_stability: OutputStability::StableSnapshot,
            output_exposure: if program == "ps" {
                OutputExposure::MayContainCommandLine
            } else {
                OutputExposure::Normal
            },
            side_effects: vec![SideEffectClass::None],
            reasons: vec!["safe-diagnostic-family"],
        };
    }
    if matches!(program, "grep" | "rg" | "find" | "head" | "tail" | "cat")
        && tokens.iter().any(|token| is_secret_search_token(token))
    {
        return StageAssessment {
            impact: RiskImpact::High,
            confidence: AssessmentConfidence::High,
            interaction: InteractionRequirement::None,
            output_stability: OutputStability::StableSnapshot,
            output_exposure: OutputExposure::MayContainSecrets,
            side_effects: vec![SideEffectClass::SensitiveDataRead],
            reasons: vec!["sensitive-search"],
        };
    }
    if matches!(
        program,
        "grep" | "rg" | "head" | "tail" | "sort" | "uniq" | "cut" | "wc"
    ) {
        return StageAssessment {
            impact: RiskImpact::Low,
            confidence: AssessmentConfidence::Medium,
            interaction: InteractionRequirement::None,
            output_stability: if program == "tail" {
                OutputStability::PotentiallyLarge
            } else {
                OutputStability::StableSnapshot
            },
            output_exposure: OutputExposure::Normal,
            side_effects: vec![SideEffectClass::None],
            reasons: vec!["readonly-pipeline-stage"],
        };
    }
    if matches!(program, "docker" | "podman" | "kubectl") {
        return assess_container_or_cluster(program, tokens);
    }

    StageAssessment {
        impact: RiskImpact::Medium,
        confidence: AssessmentConfidence::Low,
        interaction: InteractionRequirement::None,
        output_stability: OutputStability::StableSnapshot,
        output_exposure: OutputExposure::Normal,
        side_effects: vec![SideEffectClass::Unknown],
        reasons: vec!["unknown-command"],
    }
}

fn has_awk_system_call(program: &str) -> bool {
    let bytes = program.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"system")
            && is_awk_identifier_boundary(bytes, index.checked_sub(1))
            && is_awk_identifier_boundary(bytes, index.checked_add(6))
        {
            let next = skip_awk_call_separators(bytes, index + 6);
            if bytes.get(next) == Some(&b'(') {
                return true;
            }
            index = next;
        } else {
            index += 1;
        }
    }

    false
}

fn skip_awk_call_separators(bytes: &[u8], mut index: usize) -> usize {
    loop {
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }

        let continuation_len = match (bytes.get(index + 1), bytes.get(index + 2)) {
            (Some(b'\n'), _) if bytes.get(index) == Some(&b'\\') => 2,
            (Some(b'\r'), Some(b'\n')) if bytes.get(index) == Some(&b'\\') => 3,
            _ => break,
        };
        index += continuation_len;
    }

    index
}

fn is_awk_identifier_boundary(bytes: &[u8], index: Option<usize>) -> bool {
    index
        .and_then(|index| bytes.get(index))
        .is_none_or(|byte| !matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_'))
}

fn assess_container_or_cluster(program: &str, tokens: &[String]) -> StageAssessment {
    let read_subcommands = if matches!(program, "kubectl") {
        &["get", "describe", "logs"][..]
    } else {
        &["ps", "images", "inspect", "logs"][..]
    };
    let write_subcommands = if matches!(program, "kubectl") {
        &["apply", "delete", "exec", "scale", "patch"][..]
    } else {
        &["run", "rm", "stop", "exec", "kill"][..]
    };
    let subcommand = tokens.get(1).map(String::as_str).unwrap_or("");
    if write_subcommands.contains(&subcommand) {
        return StageAssessment {
            impact: RiskImpact::High,
            confidence: AssessmentConfidence::High,
            interaction: if has_tty_arg(tokens) {
                InteractionRequirement::TtyRequired
            } else {
                InteractionRequirement::None
            },
            output_stability: OutputStability::PotentiallyLarge,
            output_exposure: OutputExposure::Normal,
            side_effects: vec![SideEffectClass::ServiceControl],
            reasons: vec!["service-or-container-control"],
        };
    }
    StageAssessment {
        impact: RiskImpact::Medium,
        confidence: if read_subcommands.contains(&subcommand) {
            AssessmentConfidence::High
        } else {
            AssessmentConfidence::Medium
        },
        interaction: InteractionRequirement::None,
        output_stability: OutputStability::PotentiallyLarge,
        output_exposure: OutputExposure::Normal,
        side_effects: vec![SideEffectClass::NetworkRead],
        reasons: vec!["cluster-or-container-read"],
    }
}

fn top_is_batch_snapshot(tokens: &[String]) -> bool {
    tokens.iter().any(|arg| arg == "-b" || arg == "-l")
}

fn is_safe_diagnostic_family(program: &str) -> bool {
    matches!(program, "df" | "ps" | "top")
}

fn is_secret_search_token(token: &str) -> bool {
    matches!(
        token.to_ascii_lowercase().as_str(),
        "token" | "secret" | "password" | "credential" | "apikey" | "api_key"
    )
}
