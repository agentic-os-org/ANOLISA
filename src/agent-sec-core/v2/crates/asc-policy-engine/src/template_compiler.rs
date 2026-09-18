use std::collections::HashSet;

use asc_pap::PolicyCompiler;
use asc_policy_types::Validate;
use asc_policy_types::authoring::{PolicyTemplate, TemplateEnvelope};
use asc_policy_types::error::ValidationError;
use asc_policy_types::identifiers::{ProfileId, ResourceSetId, RuleId};
use asc_policy_types::ir::{
    ActivationRequirement, CanonicalPolicyIr, DecisionTiming, EvidenceRequirement, Expression,
    FailurePolicy, Obligation, ResourceOperation, ResourceTarget, RestrictiveDecision,
    RuleEnforcement, RuleIr, RuleOutcome, RuntimeFailurePolicy, SemanticAtom, SubjectRemediation,
    UpdateFailurePolicy,
};
use asc_policy_types::policy::PolicyEnvelope;
use asc_policy_types::profile::{IR_SCHEMA_VERSION_V1, PROFILE_V1ALPHA1_DEMO1};
use asc_policy_types::resource::{
    FileMatcher, FileResolution, PathMatcher, ResourceSelector, ResourceSet,
};

/// Deterministic compiler for the currently frozen product Policy subset.
#[derive(Debug, Default, Clone, Copy)]
pub struct PolicyTemplateCompiler;

impl PolicyCompiler for PolicyTemplateCompiler {
    fn lower(&self, template: &TemplateEnvelope) -> Result<PolicyEnvelope, ValidationError> {
        let PolicyTemplate::PreventFileDeletion { files } = &template.template else {
            return Err(ValidationError::new(
                "template.kind",
                "the current compiler supports only prevent_file_deletion",
            ));
        };
        if files.is_empty() {
            return Err(ValidationError::new("template.files", "must not be empty"));
        }

        let resource_id = ResourceSetId::new("protected-file-entries")
            .map_err(|message| ValidationError::new("payload.resources[0].id", message))?;
        let mut unique_matchers = HashSet::with_capacity(files.len());
        let matchers = files
            .iter()
            .enumerate()
            .map(|(index, path)| {
                let matcher = FileMatcher {
                    path: if path.contains(['*', '?']) {
                        PathMatcher::Glob {
                            pattern: path.clone(),
                        }
                    } else {
                        PathMatcher::Exact { path: path.clone() }
                    },
                    resolution: FileResolution::PathEntry,
                };
                matcher.validate().map_err(|error| {
                    ValidationError::new(format!("template.files[{index}]"), error.message)
                })?;
                if !unique_matchers.insert(matcher.clone()) {
                    return Err(ValidationError::new(
                        format!("template.files[{index}]"),
                        "duplicate matcher",
                    ));
                }
                Ok(matcher)
            })
            .collect::<Result<Vec<_>, ValidationError>>()?;
        let policy = PolicyEnvelope {
            ir_schema_version: IR_SCHEMA_VERSION_V1,
            profile_id: ProfileId::new(PROFILE_V1ALPHA1_DEMO1)
                .map_err(|message| ValidationError::new("profileId", message))?,
            policy_id: template.policy_id.clone(),
            revision: template.revision,
            payload: CanonicalPolicyIr {
                resources: vec![ResourceSet {
                    id: resource_id.clone(),
                    selector: ResourceSelector::File { matchers },
                }],
                rules: vec![RuleIr {
                    id: RuleId::new("deny-protected-file-deletion")
                        .map_err(|message| ValidationError::new("payload.rules[0].id", message))?,
                    when: Expression::Atom {
                        atom: SemanticAtom::ResourceOperation {
                            operation: ResourceOperation::Delete,
                            target: ResourceTarget::In {
                                resource_set: resource_id,
                            },
                        },
                    },
                    outcome: RuleOutcome {
                        decision: RestrictiveDecision::Deny,
                        obligations: vec![Obligation::Audit, Obligation::EmitReceipt],
                        remediation: SubjectRemediation::None,
                    },
                    enforcement: RuleEnforcement {
                        decision_timing: DecisionTiming::PreEffect,
                        required_evidence: vec![
                            EvidenceRequirement::BindingReady,
                            EvidenceRequirement::OperationDenied,
                        ],
                    },
                }],
                activation: ActivationRequirement::PostAttachAllowed,
                failure_policy: FailurePolicy {
                    runtime: RuntimeFailurePolicy::FailClosed,
                    update: UpdateFailurePolicy::KeepLastKnownGood,
                },
            },
        };
        policy.validate()?;
        Ok(policy)
    }
}
