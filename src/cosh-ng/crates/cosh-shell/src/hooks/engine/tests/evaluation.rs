use super::*;
use crate::types::{MemoryPressureFacts, MetricsConfidence};
use std::collections::BTreeSet;

struct FakeHook {
    matcher: HookMatcher,
    severity: FindingSeverity,
}

struct SpoofingBuiltinHook {
    matcher: HookMatcher,
}

struct TypedMemoryHook {
    matcher: HookMatcher,
}

impl BuiltinHook for TypedMemoryHook {
    fn id(&self) -> &str {
        &self.matcher.id
    }

    fn matcher(&self) -> &HookMatcher {
        &self.matcher
    }

    fn evaluate(&self, _input: &HookInput) -> Option<HookFinding> {
        Some(HookFinding {
            hook_id: "memory-pressure".to_string(),
            severity: FindingSeverity::Warning,
            title: "changed presentation".to_string(),
            description: "changed presentation".to_string(),
            suggestion: "diagnose".to_string(),
            skill: None,
            cli_hint: None,
            context_refs: Vec::new(),
        })
    }

    fn builtin_facts(&self, _input: &HookInput) -> Option<BuiltinFindingFacts> {
        Some(BuiltinFindingFacts::MemoryPressure(MemoryPressureFacts {
            confidence: MetricsConfidence::High,
            available_ratio: 0.08,
            swap_ratio: Some(0.25),
        }))
    }
}

impl BuiltinHook for SpoofingBuiltinHook {
    fn id(&self) -> &str {
        &self.matcher.id
    }

    fn matcher(&self) -> &HookMatcher {
        &self.matcher
    }

    fn evaluate(&self, _input: &HookInput) -> Option<HookFinding> {
        Some(HookFinding {
            hook_id: "payload-controlled-id".to_string(),
            severity: FindingSeverity::Warning,
            title: "test".to_string(),
            description: "desc".to_string(),
            suggestion: "fix it".to_string(),
            skill: None,
            cli_hint: None,
            context_refs: Vec::new(),
        })
    }
}

impl BuiltinHook for FakeHook {
    fn id(&self) -> &str {
        &self.matcher.id
    }
    fn matcher(&self) -> &HookMatcher {
        &self.matcher
    }
    fn evaluate(&self, _input: &HookInput) -> Option<HookFinding> {
        Some(HookFinding {
            hook_id: self.matcher.id.clone(),
            severity: self.severity,
            title: "test".to_string(),
            description: "desc".to_string(),
            suggestion: "fix it".to_string(),
            skill: None,
            cli_hint: None,
            context_refs: Vec::new(),
        })
    }
}

#[test]
fn evaluate_returns_sorted_findings() {
    let mut engine = HookEngine::new();
    engine.register(Box::new(FakeHook {
        matcher: HookMatcher {
            id: "info-hook".to_string(),
            commands: vec![],
            command_patterns: vec![],
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        severity: FindingSeverity::Info,
    }));
    engine.register(Box::new(FakeHook {
        matcher: HookMatcher {
            id: "critical-hook".to_string(),
            commands: vec![],
            command_patterns: vec![],
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        severity: FindingSeverity::Critical,
    }));
    engine.register(Box::new(FakeHook {
        matcher: HookMatcher {
            id: "warning-hook".to_string(),
            commands: vec![],
            command_patterns: vec![],
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        severity: FindingSeverity::Warning,
    }));

    let block = CommandBlock {
        id: "b1".to_string(),
        session_id: "s1".to_string(),
        command: "ls".to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 0,
        ended_at_ms: 100,
        duration_ms: 100,
        exit_code: 0,
        status: crate::types::CommandStatus::Completed,
        output: crate::types::OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };

    let findings = engine.evaluate(&block);
    assert_eq!(findings.len(), 3);
    assert_eq!(findings[0].severity, FindingSeverity::Critical);
    assert_eq!(findings[1].severity, FindingSeverity::Warning);
    assert_eq!(findings[2].severity, FindingSeverity::Info);
}

#[test]
fn evaluate_with_disabled_skips_matching_hook() {
    let mut engine = HookEngine::new();
    engine.register(Box::new(FakeHook {
        matcher: HookMatcher {
            id: "disabled-hook".to_string(),
            commands: vec![],
            command_patterns: vec![],
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
        severity: FindingSeverity::Warning,
    }));

    let block = CommandBlock {
        id: "b1".to_string(),
        session_id: "s1".to_string(),
        command: "ls".to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 0,
        ended_at_ms: 100,
        duration_ms: 100,
        exit_code: 0,
        status: crate::types::CommandStatus::Completed,
        output: crate::types::OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let disabled = HashSet::from(["disabled-hook".to_string()]);

    assert!(engine.evaluate_with_disabled(&block, &disabled).is_empty());
    assert_eq!(engine.evaluate(&block).len(), 1);
}
#[test]
fn builtin_provenance_uses_registered_hook_id_not_payload_hook_id() {
    let mut engine = HookEngine::new();
    engine.register(Box::new(SpoofingBuiltinHook {
        matcher: HookMatcher {
            id: "registered-builtin".to_string(),
            commands: vec![],
            command_patterns: vec![],
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
    }));

    let findings = engine.evaluate(&make_block("ls"));

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].hook_id, "payload-controlled-id");
    assert_eq!(
        findings[0].provenance(),
        &HookProvenance::Builtin {
            producer_registration_ids: BTreeSet::from(["registered-builtin".to_string()]),
        }
    );
}

#[test]
fn engine_attaches_builtin_facts_outside_external_payload() {
    let mut engine = HookEngine::new();
    engine.register(Box::new(TypedMemoryHook {
        matcher: HookMatcher {
            id: "memory-pressure".to_string(),
            commands: vec![],
            command_patterns: vec![],
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger: HookTrigger::OnComplete,
        },
    }));

    let findings = engine.evaluate(&make_block("free -m"));

    assert_eq!(findings.len(), 1);
    assert!(matches!(
        findings[0].builtin_facts.as_ref(),
        Some(BuiltinFindingFacts::MemoryPressure(MemoryPressureFacts {
            confidence: MetricsConfidence::High,
            available_ratio: 0.08,
            swap_ratio: Some(0.25),
        }))
    ));
}

#[test]
fn external_hook_boundaries_redact_input_and_findings() {
    let secret = "hook-boundary-secret";
    let mut block = make_block(&format!("curl --token {secret}"));
    block.cwd = format!("/tmp/token={secret}");
    let input = runtime::hook_input_from_block(&block);
    assert!(!input.command.contains(secret), "{}", input.command);
    assert!(!input.cwd.contains(secret), "{}", input.cwd);

    let mut finding = HookFinding {
        hook_id: format!("token={secret}"),
        severity: FindingSeverity::Warning,
        title: format!("Bearer {secret}"),
        description: format!("password={secret}"),
        suggestion: format!("--api-key {secret}"),
        skill: Some(format!("token={secret}")),
        cli_hint: Some(format!("--token {secret}")),
        context_refs: vec![format!("client_secret={secret}")],
    };
    runtime::redact_hook_finding(&mut finding);

    let serialized = serde_json::to_string(&finding).unwrap();
    assert!(!serialized.contains(secret), "{serialized}");
}
