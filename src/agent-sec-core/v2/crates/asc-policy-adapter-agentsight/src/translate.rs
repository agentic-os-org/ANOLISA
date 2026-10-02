//! Deterministic Canonical IR to `ActPlane` DSL translation.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use asc_policy_types::Validate;
use asc_policy_types::binding::PreparedBinding;
use asc_policy_types::ir::{
    ActivationRequirement, DecisionTiming, EvidenceRequirement, Expression, Obligation,
    ResourceOperation, ResourceTarget, RestrictiveDecision, RuleIr, RuntimeFailurePolicy,
    SemanticAtom, SubjectRemediation, UpdateFailurePolicy,
};
use asc_policy_types::policy::PolicyEnvelope;
use asc_policy_types::resource::{FileResolution, PathMatcher, ResourceSelector};
use asc_policy_types::scope::{PreparedScope, ScopeSelector};
use asc_policy_types::target::{
    AdapterFault, TargetBindingPlan, TranslationOutcome, TranslationRejection,
};

use crate::plan::{
    ACTPLANE_POLICY_MEDIA_TYPE, AGENTSIGHT_BINDING_PLAN_FORMAT,
    AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION, AgentSightBindingPlan, AgentSightPolicyPlan,
    AgentSightScopePlan, AgentSightSourceBinding,
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
        let policy_dsl = match compile_policy(binding) {
            Ok(policy_dsl) => policy_dsl,
            Err(rejection) => return Ok(TranslationOutcome::Rejected(rejection)),
        };

        let target_plan = AgentSightBindingPlan {
            schema_version: AGENTSIGHT_BINDING_PLAN_SCHEMA_VERSION,
            source: AgentSightSourceBinding {
                binding_id: binding.binding_id.clone(),
                binding_revision: binding.binding_revision,
                policy_id: binding.policy.policy_id.clone(),
                policy_revision: binding.policy.revision,
                scope_id: binding.scope.scope_id.clone(),
                scope_revision: binding.scope.revision,
            },
            policy: AgentSightPolicyPlan {
                media_type: ACTPLANE_POLICY_MEDIA_TYPE.to_owned(),
                content: policy_dsl,
            },
            scope,
        };
        let content =
            serde_json::to_vec(&target_plan).map_err(|_| fault("ADAPTER_SERIALIZATION_FAILED"))?;
        let plan = TargetBindingPlan {
            format: AGENTSIGHT_BINDING_PLAN_FORMAT.to_owned(),
            content,
        };
        Ok(TranslationOutcome::Translated(plan))
    }
}

fn translate_scope(scope: &PreparedScope) -> Result<AgentSightScopePlan, TranslationRejection> {
    let ScopeSelector::Pid { pid } = &scope.selector else {
        return Err(rejection("UNSUPPORTED_SCOPE_SELECTOR"));
    };
    let root_pid = i32::try_from(*pid).map_err(|_| rejection("UNSUPPORTED_SCOPE_PID_RANGE"))?;
    Ok(AgentSightScopePlan::ProcessTree { root_pid })
}

fn compile_policy(binding: &PreparedBinding) -> Result<String, TranslationRejection> {
    let policy = &binding.policy.canonical_policy;
    if !guarantees_supported(&policy.payload) {
        return Err(rejection("UNSUPPORTED_GUARANTEE"));
    }
    let mut rules: Vec<_> = policy.payload.rules.iter().collect();
    rules.sort_by(|left, right| left.id.as_str().cmp(right.id.as_str()));

    let mut patterns = BTreeSet::new();
    for rule in rules {
        let translated_patterns = translate_rule(policy, rule)?;
        patterns.extend(translated_patterns);
    }

    if patterns.len() > ACTPLANE_MAX_RULES {
        return Err(rejection("ACTPLANE_RULE_LIMIT_EXCEEDED"));
    }

    Ok(render_dsl(&patterns))
}

fn translate_rule(
    policy: &PolicyEnvelope,
    rule: &RuleIr,
) -> Result<BTreeSet<String>, TranslationRejection> {
    let Expression::Atom {
        atom:
            SemanticAtom::ResourceOperation {
                operation: ResourceOperation::Delete,
                target: ResourceTarget::In { resource_set },
            },
    } = &rule.when
    else {
        return Err(rejection("UNSUPPORTED_CANONICAL_RULE"));
    };

    let Some(resource) = policy
        .payload
        .resources
        .iter()
        .find(|resource| &resource.id == resource_set)
    else {
        return Err(rejection("MISSING_RESOURCE_SET"));
    };
    let ResourceSelector::File { matchers } = &resource.selector else {
        return Err(rejection("UNSUPPORTED_RESOURCE_KIND"));
    };

    let mut patterns = BTreeSet::new();
    for matcher in matchers {
        if matcher.resolution != FileResolution::PathEntry {
            return Err(rejection("UNSUPPORTED_FILE_RESOLUTION"));
        }
        for pattern in target_patterns(&matcher.path)? {
            if !is_safe_dsl_pattern(&pattern) {
                return Err(rejection("UNSUPPORTED_ACTPLANE_PATTERN"));
            }
            patterns.insert(pattern);
        }
    }

    Ok(patterns)
}

fn guarantees_supported(policy: &asc_policy_types::ir::CanonicalPolicyIr) -> bool {
    policy.activation == ActivationRequirement::PostAttachAllowed
        && policy.failure_policy.runtime == RuntimeFailurePolicy::FailClosed
        && policy.failure_policy.update == UpdateFailurePolicy::KeepLastKnownGood
        && policy.rules.iter().all(|rule| {
            rule.outcome.decision == RestrictiveDecision::Deny
                && same_set(
                    &rule.outcome.obligations,
                    &[Obligation::Audit, Obligation::EmitReceipt],
                )
                && rule.outcome.remediation == SubjectRemediation::None
                && rule.enforcement.decision_timing == DecisionTiming::PreEffect
                && same_set(
                    &rule.enforcement.required_evidence,
                    &[
                        EvidenceRequirement::BindingReady,
                        EvidenceRequirement::OperationDenied,
                    ],
                )
        })
}

fn same_set<T: PartialEq>(actual: &[T], expected: &[T]) -> bool {
    actual.len() == expected.len() && expected.iter().all(|value| actual.contains(value))
}

fn target_patterns(path: &PathMatcher) -> Result<Vec<String>, TranslationRejection> {
    let patterns = match path {
        PathMatcher::Exact { path } => vec![path.clone()],
        PathMatcher::Glob { pattern } if actplane_can_represent_glob(pattern) => {
            vec![pattern.clone()]
        }
        PathMatcher::Glob { .. } => return Err(rejection("UNSUPPORTED_ACTPLANE_GLOB")),
        PathMatcher::Prefix { path } if path == "/" => vec!["/**".to_owned()],
        PathMatcher::Prefix { path } => vec![path.clone(), format!("{path}/**")],
    };
    if patterns
        .iter()
        .any(|pattern| actplane_lowered_literal_len(pattern) > ACTPLANE_MAX_LOWERED_PATTERN_BYTES)
    {
        return Err(rejection("ACTPLANE_PATTERN_LIMIT_EXCEEDED"));
    }
    Ok(patterns)
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

// Only literal paths and trailing /** reach this helper: Binding validation
// excludes wildcards from Exact/Prefix and target_patterns restricts Glob.
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

fn render_dsl(patterns: &BTreeSet<String>) -> String {
    // TODO: ActPlane currently lowers both `unlink` and `write` to OP_WRITE. Keep
    // the explicit unlink DSL while landing the Adapter-to-Client path, then
    // split the backend operation so delete-only enforcement does not also
    // block content mutation or other namespace mutation.
    let mut dsl = String::from("source AGENT = exec \"**\"\n");
    for (index, pattern) in patterns.iter().enumerate() {
        write!(
            dsl,
            "rule agentseccore-unlink-{index:04}:\n  block unlink file \"{pattern}\" if AGENT\n  because \"{DSL_REASON}\"\n"
        )
        .unwrap_or_else(|_| unreachable!("writing formatted text into String cannot fail"));
    }
    dsl
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
