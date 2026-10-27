//! Deterministic Policy template to `ActPlane` DSL translation.

use std::fmt::Write as _;

use asc_policy_types::Validate;
use asc_policy_types::authoring::{
    Action, Category, Comparison, Condition, Effect, FileProtectionExclusionKind,
    FileProtectionPolicy, FileProtectionTargetKind, FileProtectionTopology, Resource, Scalar,
    Value,
};
use asc_policy_types::binding::BindingScope;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::scope::ScopeSelector;
use asc_policy_types::target::{
    AdapterFault, TargetBindingPlan, TranslationOutcome, TranslationRejection,
};

use crate::plan::{
    ACTPLANE_POLICY_MEDIA_TYPE, AGENTSIGHT_BINDING_PLAN_FORMAT,
    AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION, AGENTSIGHT_FILE_PROTECTION_POLICY_VERSION,
    AGENTSIGHT_STRUCTURED_BINDING_PLAN_FORMAT, AGENTSIGHT_STRUCTURED_BINDING_PLAN_SCHEMA_VERSION,
    AgentSightBindingPlan, AgentSightFileProtectionExclusion,
    AgentSightFileProtectionExclusionKind, AgentSightFileProtectionPolicy,
    AgentSightFileProtectionTarget, AgentSightFileProtectionTargetKind,
    AgentSightFileProtectionTopologyMode, AgentSightFileProtections, AgentSightPolicyPlan,
    AgentSightScopePlan, AgentSightSourceBinding, AgentSightStructuredBindingPlan,
};

// ActPlane's PAT=64 kernel ABI reserves one byte for the trailing NUL.
const ACTPLANE_MAX_LOWERED_PATTERN_BYTES: usize = 63;
const ACTPLANE_MAX_RULES: usize = 128;
const DSL_REASON: &str = "AgentSecCore file deletion policy";

/// Pure AgentSight/ActPlane Adapter for file-deletion policies and PID Scopes.
#[derive(Debug, Default, Clone, Copy)]
pub struct AgentSightAdapter;

impl AgentSightAdapter {
    /// Translates one complete immutable Binding without persistence or I/O.
    ///
    /// # Errors
    /// Returns an internal Adapter fault. A deterministic semantic mismatch is
    /// represented by [`TranslationOutcome::Rejected`].
    pub fn translate(&self, binding: &PreparedBinding) -> Result<TranslationOutcome, AdapterFault> {
        if binding.validate().is_err() {
            return Ok(rejected("INVALID_BINDING"));
        }

        let scope = match translate_scope(&binding.scope) {
            Ok(scope) => scope,
            Err(rejection) => return Ok(TranslationOutcome::Rejected(rejection)),
        };
        match binding.policy.template.spec_version.as_str() {
            "0.1" => translate_legacy_binding(binding, scope),
            "0.2" => translate_structured_binding(binding, scope),
            _ => Ok(rejected("INVALID_BINDING")),
        }
    }
}

fn translate_legacy_binding(
    binding: &PreparedBinding,
    scope: AgentSightScopePlan,
) -> Result<TranslationOutcome, AdapterFault> {
    let policy_dsl = match compile_policy(binding) {
        Ok(policy_dsl) => policy_dsl,
        Err(rejection) => return Ok(TranslationOutcome::Rejected(rejection)),
    };
    let target_plan = AgentSightBindingPlan {
        schema_version: AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION,
        source: translate_source(binding),
        policy: AgentSightPolicyPlan {
            media_type: ACTPLANE_POLICY_MEDIA_TYPE.to_owned(),
            content: policy_dsl,
        },
        scope,
    };
    serialize_plan(AGENTSIGHT_BINDING_PLAN_FORMAT, &target_plan)
}

fn translate_structured_binding(
    binding: &PreparedBinding,
    scope: AgentSightScopePlan,
) -> Result<TranslationOutcome, AdapterFault> {
    let Some(file_protection) = binding.policy.template.file_protection.as_ref() else {
        return Ok(rejected("INVALID_BINDING"));
    };
    let target_plan = AgentSightStructuredBindingPlan {
        schema_version: AGENTSIGHT_STRUCTURED_BINDING_PLAN_SCHEMA_VERSION,
        source: translate_source(binding),
        scope,
        file_protection_policy: translate_file_protection(file_protection),
    };
    serialize_plan(AGENTSIGHT_STRUCTURED_BINDING_PLAN_FORMAT, &target_plan)
}

fn serialize_plan(
    format: &str,
    plan: &impl serde::Serialize,
) -> Result<TranslationOutcome, AdapterFault> {
    let content = serde_json::to_vec(plan).map_err(|_| fault("ADAPTER_SERIALIZATION_FAILED"))?;
    Ok(TranslationOutcome::Translated(TargetBindingPlan {
        format: format.to_owned(),
        content,
    }))
}

fn translate_source(binding: &PreparedBinding) -> AgentSightSourceBinding {
    AgentSightSourceBinding {
        binding_id: binding.binding_id.clone(),
        binding_revision: binding.binding_revision,
        policy_id: binding.policy.policy_id.clone(),
        policy_revision: binding.policy.revision,
        scope_id: binding.scope.scope_id.clone(),
    }
}

fn translate_file_protection(policy: &FileProtectionPolicy) -> AgentSightFileProtectionPolicy {
    AgentSightFileProtectionPolicy {
        version: AGENTSIGHT_FILE_PROTECTION_POLICY_VERSION,
        topology_mode: match policy.topology {
            FileProtectionTopology::StaticAtApply => {
                AgentSightFileProtectionTopologyMode::StaticAtApply
            }
        },
        targets: policy
            .targets
            .iter()
            .map(|target| AgentSightFileProtectionTarget {
                path: target.path.clone(),
                kind: match target.kind {
                    FileProtectionTargetKind::ExactFile => {
                        AgentSightFileProtectionTargetKind::ExactFile
                    }
                    FileProtectionTargetKind::DirectoryTree => {
                        AgentSightFileProtectionTargetKind::DirectoryTree
                    }
                },
                protections: AgentSightFileProtections {
                    delete: target.protections.delete,
                    ordinary_write: target.protections.ordinary_write,
                },
                exclusions: target
                    .exclusions
                    .iter()
                    .map(|exclusion| AgentSightFileProtectionExclusion {
                        path: exclusion.path.clone(),
                        kind: match exclusion.kind {
                            FileProtectionExclusionKind::ExactEntry => {
                                AgentSightFileProtectionExclusionKind::ExactEntry
                            }
                            FileProtectionExclusionKind::DirectorySubtree => {
                                AgentSightFileProtectionExclusionKind::DirectorySubtree
                            }
                        },
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn translate_scope(scope: &BindingScope) -> Result<AgentSightScopePlan, TranslationRejection> {
    if matches!(scope.selector, ScopeSelector::CgroupId { .. }) {
        return Err(rejection("UNSUPPORTED_SCOPE_SELECTOR"));
    }
    let pid = scope.process.pid;
    let root_pid = i32::try_from(pid).map_err(|_| rejection("UNSUPPORTED_SCOPE_PID_RANGE"))?;
    Ok(AgentSightScopePlan::ProcessTree {
        root_pid,
        process: scope.process.clone(),
    })
}

fn compile_policy(binding: &PreparedBinding) -> Result<String, TranslationRejection> {
    let rules = &binding.policy.template.rules;
    if rules.len() > ACTPLANE_MAX_RULES {
        return Err(rejection("ACTPLANE_RULE_LIMIT_EXCEEDED"));
    }
    // Validate the whole policy before returning a plan; never apply a subset.
    for (index, rule) in rules.iter().enumerate() {
        let reject = |code| rejection(&format!("RULE_{index}_{code}"));
        if rule.effect != Effect::Block {
            return Err(reject("UNSUPPORTED_EFFECT"));
        }
        if rule.category != Category::File || rule.action != Action::Write {
            return Err(reject("UNSUPPORTED_ACTION"));
        }
        if rule.previous.is_some() {
            return Err(reject("UNSUPPORTED_HISTORY"));
        }
        if !matches!(&rule.condition, Some(Condition::Operation(Comparison::Eq(Value::Scalar(Scalar::String(operation))))) if operation == "delete")
        {
            return Err(reject("UNSUPPORTED_CONDITION"));
        }
        let Resource::File { path: pattern } = &rule.target;
        if !actplane_can_represent_glob(pattern) {
            return Err(reject("UNSUPPORTED_ACTPLANE_GLOB"));
        }
        if actplane_lowered_literal_len(pattern) > ACTPLANE_MAX_LOWERED_PATTERN_BYTES {
            return Err(reject("ACTPLANE_PATTERN_LIMIT_EXCEEDED"));
        }
        if !is_safe_dsl_pattern(pattern) {
            return Err(reject("UNSUPPORTED_ACTPLANE_PATTERN"));
        }
        if !is_safe_dsl_pattern(rule.because.as_deref().unwrap_or(DSL_REASON)) {
            // ActPlane's lexer has no quoted-string escape syntax.
            return Err(reject("UNSUPPORTED_ACTPLANE_REASON"));
        }
    }
    let mut dsl = String::from("source AGENT = exec \"**\"\n");
    for (index, rule) in rules.iter().enumerate() {
        let Resource::File { path: pattern } = &rule.target;
        let reason = rule.because.as_deref().unwrap_or(DSL_REASON);
        // ActPlane still maps unlink and write to OP_WRITE; translation alone
        // does not establish delete-only kernel enforcement.
        writeln!(dsl, "rule agentseccore-unlink-{index:04}:\n  block unlink file \"{pattern}\" if AGENT\n  because \"{reason}\"")
            .unwrap_or_else(|_| unreachable!("writing formatted text into String cannot fail"));
    }
    Ok(dsl)
}

fn actplane_can_represent_glob(pattern: &str) -> bool {
    if pattern.contains('?') {
        return false;
    }

    if !pattern.contains('*') {
        return true;
    }

    pattern
        .strip_suffix("/**")
        .is_some_and(|prefix| !prefix.contains('*'))
}

// Only literal paths and trailing /** reach this helper after the glob check.
fn actplane_lowered_literal_len(pattern: &str) -> usize {
    if let Some(prefix) = pattern.strip_suffix("/**") {
        return prefix.len() + 1;
    }
    pattern.len()
}

fn is_safe_dsl_pattern(pattern: &str) -> bool {
    // Paths are already nonempty and normalized; this is the DSL boundary.
    !pattern
        .chars()
        .any(|character| matches!(character, '"' | '\\') || character.is_control())
}

fn rejected(code: &str) -> TranslationOutcome {
    TranslationOutcome::Rejected(rejection(code))
}

fn rejection(code: &str) -> TranslationRejection {
    TranslationRejection {
        code: code.to_owned(),
    }
}

fn fault(code: &str) -> AdapterFault {
    AdapterFault {
        code: code.to_owned(),
    }
}
