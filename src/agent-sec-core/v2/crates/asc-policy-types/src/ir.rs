//! Canonical Policy IR rule, Atom, outcome, and guarantee contracts.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::error::{Validate, ValidationError};
use crate::identifiers::{ResourceSetId, RuleId};
use crate::profile::{MAX_ATOMS_PER_RULE, MAX_RESOURCE_SETS, MAX_RULES};
use crate::resource::{ResourceKind, ResourceSet};

/// Canonical, backend-independent security semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CanonicalPolicyIr {
    /// Resource sets referenced by rules.
    pub resources: Vec<ResourceSet>,
    /// Monotonic restrictive rules.
    pub rules: Vec<RuleIr>,
    /// Binding activation barrier.
    pub activation: ActivationRequirement,
    /// Runtime and update failure behavior.
    pub failure_policy: FailurePolicy,
}

impl Validate for CanonicalPolicyIr {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.resources.is_empty() {
            return Err(ValidationError::new("resources", "must not be empty"));
        }
        if self.resources.len() > MAX_RESOURCE_SETS {
            return Err(ValidationError::new(
                "resources",
                format!("must contain at most {MAX_RESOURCE_SETS} resource sets"),
            ));
        }
        if self.rules.is_empty() {
            return Err(ValidationError::new("rules", "must not be empty"));
        }
        if self.rules.len() > MAX_RULES {
            return Err(ValidationError::new(
                "rules",
                format!("must contain at most {MAX_RULES} rules"),
            ));
        }
        if self.failure_policy.runtime == RuntimeFailurePolicy::AuditOnly {
            return Err(ValidationError::new(
                "failurePolicy.runtime",
                "audit_only cannot satisfy a restrictive V1 policy",
            ));
        }

        let mut resources = HashMap::with_capacity(self.resources.len());
        for (index, resource) in self.resources.iter().enumerate() {
            resource.validate().map_err(|error| {
                ValidationError::new(format!("resources[{index}].{}", error.path), error.message)
            })?;
            if resources
                .insert(&resource.id, resource.selector.kind())
                .is_some()
            {
                return Err(ValidationError::new(
                    format!("resources[{index}].id"),
                    "duplicate resource-set id",
                ));
            }
        }

        let mut rule_ids = HashSet::with_capacity(self.rules.len());
        for (index, rule) in self.rules.iter().enumerate() {
            if !rule_ids.insert(&rule.id) {
                return Err(ValidationError::new(
                    format!("rules[{index}].id"),
                    "duplicate rule id",
                ));
            }
            validate_rule(rule, &resources).map_err(|error| {
                ValidationError::new(format!("rules[{index}].{}", error.path), error.message)
            })?;
        }
        Ok(())
    }
}

/// One monotonic restrictive rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleIr {
    /// Policy-local rule identity.
    pub id: RuleId,
    /// Conditions evaluated against one decision event.
    pub when: Expression,
    /// Restrictive decision, obligations, and optional remediation.
    pub outcome: RuleOutcome,
    /// Timing and evidence requirements.
    pub enforcement: RuleEnforcement,
}

/// Bounded V1 expression tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Expression {
    /// One semantic atom.
    Atom {
        /// Typed condition.
        atom: SemanticAtom,
    },
    /// Conjunction over atoms from the same decision event.
    All {
        /// Direct Atom children. Nested expressions are rejected by V1.
        expressions: Vec<Expression>,
    },
    /// Reserved for a later profile.
    Any {
        /// Child expressions.
        expressions: Vec<Expression>,
    },
    /// Reserved for a later profile.
    Not {
        /// Negated expression.
        expression: Box<Expression>,
    },
}

impl Expression {
    /// Applies a resource-set ID remapping in place.
    pub fn remap_resources<F>(&mut self, mut remap: F)
    where
        F: FnMut(&ResourceSetId) -> ResourceSetId,
    {
        self.visit_atoms_mut(&mut |atom| atom.remap_resources(&mut remap));
    }

    fn visit_atoms_mut<F>(&mut self, visitor: &mut F)
    where
        F: FnMut(&mut SemanticAtom),
    {
        match self {
            Self::Atom { atom } => visitor(atom),
            Self::All { expressions } | Self::Any { expressions } => {
                for expression in expressions {
                    expression.visit_atoms_mut(visitor);
                }
            }
            Self::Not { expression } => expression.visit_atoms_mut(visitor),
        }
    }
}

/// Smallest closed semantic condition understood by an adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SemanticAtom {
    /// One normalized operation on one resource set.
    ResourceOperation {
        /// Normalized resource operation.
        operation: ResourceOperation,
        /// Included or excluded resource set.
        target: ResourceTarget,
    },
    /// Information flow between two resource sets.
    InformationFlow {
        /// Flow source.
        source: ResourceTarget,
        /// Flow destination.
        destination: ResourceTarget,
        /// Required propagation coverage.
        propagation: FlowPropagation,
    },
}

impl SemanticAtom {
    fn remap_resources<F>(&mut self, remap: &mut F)
    where
        F: FnMut(&ResourceSetId) -> ResourceSetId,
    {
        match self {
            Self::ResourceOperation { target, .. } => target.remap(remap),
            Self::InformationFlow {
                source,
                destination,
                ..
            } => {
                source.remap(remap);
                destination.remap(remap);
            }
        }
    }

    fn event_key(&self) -> EventKey {
        match self {
            Self::ResourceOperation { operation, .. } => EventKey::ResourceOperation(*operation),
            Self::InformationFlow { propagation, .. } => EventKey::InformationFlow(*propagation),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventKey {
    ResourceOperation(ResourceOperation),
    InformationFlow(FlowPropagation),
}

/// Resource-set membership used by a semantic Atom.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ResourceTarget {
    /// Target belongs to the named set.
    In {
        /// Referenced policy-local resource set.
        resource_set: ResourceSetId,
    },
    /// Target belongs to the same resource domain but not the named set.
    Except {
        /// Referenced policy-local resource set whose complement is selected.
        resource_set: ResourceSetId,
    },
}

impl ResourceTarget {
    /// Returns the referenced resource-set identity.
    pub const fn resource_set(&self) -> &ResourceSetId {
        match self {
            Self::In { resource_set } | Self::Except { resource_set } => resource_set,
        }
    }

    fn remap<F>(&mut self, remap: &mut F)
    where
        F: FnMut(&ResourceSetId) -> ResourceSetId,
    {
        match self {
            Self::In { resource_set } | Self::Except { resource_set } => {
                *resource_set = remap(resource_set);
            }
        }
    }
}

/// V1 normalized operation vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceOperation {
    /// Read file content into the protected execution domain.
    Read,
    /// Remove one named filesystem directory entry.
    Delete,
    /// Change resource content.
    ContentMutation,
    /// Change filesystem namespace relationships.
    NamespaceMutation,
    /// Execute an image.
    Execute,
    /// Initiate a network connection.
    Connect,
    /// Use a product-level credential.
    UseCredential,
}

/// Information-flow propagation coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowPropagation {
    /// Direct source-to-destination transfer.
    Direct,
    /// Transfer after copying, encoding, joining, or computation.
    Derived,
}

/// Restrictive rule outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleOutcome {
    /// Access decision. V1 contains only `deny`.
    pub decision: RestrictiveDecision,
    /// Required side effects independent of the access decision.
    pub obligations: Vec<Obligation>,
    /// Optional action against the protected subject.
    pub remediation: SubjectRemediation,
}

/// Restrictive access decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestrictiveDecision {
    /// Deny the matched effect.
    Deny,
}

/// Evidence or notification obligation attached to a decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Obligation {
    /// Persist a normalized audit record.
    Audit,
    /// Notify an operator-facing channel.
    Notify,
    /// Emit a cursor-addressable enforcement receipt.
    EmitReceipt,
}

/// Subject-level remediation after a rule match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectRemediation {
    /// Do not change subject lifecycle.
    None,
    /// Freeze execution while preserving state.
    Freeze,
    /// Isolate the subject from untrusted resources.
    Quarantine,
    /// Terminate the subject.
    Kill,
}

/// Rule-specific timing and evidence requirements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleEnforcement {
    /// Required decision point.
    pub decision_timing: DecisionTiming,
    /// Evidence that must be produced for the rule.
    pub required_evidence: Vec<EvidenceRequirement>,
}

/// Binding activation barrier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationRequirement {
    /// The binding must be ready before worker execution is released.
    BeforeWorkerStart,
    /// Attaching to an already running worker is allowed.
    PostAttachAllowed,
}

impl ActivationRequirement {
    /// Returns the stronger of two activation requirements.
    #[must_use]
    pub const fn stricter(self, other: Self) -> Self {
        if matches!(self, Self::BeforeWorkerStart) || matches!(other, Self::BeforeWorkerStart) {
            Self::BeforeWorkerStart
        } else {
            Self::PostAttachAllowed
        }
    }
}

/// Time at which a restrictive decision must be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionTiming {
    /// Before the protected effect commits.
    PreEffect,
    /// After the effect, for observation-only semantics.
    PostEffect,
}

/// Runtime and policy-update failure behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FailurePolicy {
    /// Runtime assurance loss behavior.
    pub runtime: RuntimeFailurePolicy,
    /// Policy replacement failure behavior.
    pub update: UpdateFailurePolicy,
}

/// Runtime assurance loss behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeFailurePolicy {
    /// Do not allow unprotected effects to proceed.
    FailClosed,
    /// Freeze the binding until assurance returns.
    FreezeBinding,
    /// Record the failure without blocking. Invalid for V1 hard rules.
    AuditOnly,
}

/// Policy update failure behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateFailurePolicy {
    /// Retain the last target policy proven active.
    KeepLastKnownGood,
    /// Disable and drain the binding.
    DisableBinding,
}

/// Evidence required from the control or enforcement plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceRequirement {
    /// Policy revision was accepted and stored.
    PolicyInstalled,
    /// Binding and every required enforcement point are ready.
    BindingReady,
    /// One rule matched.
    RuleMatched,
    /// The requested operation was denied synchronously.
    OperationDenied,
    /// Independent evidence of the resulting system effect.
    EffectReceipt,
}

fn validate_rule(
    rule: &RuleIr,
    resources: &HashMap<&ResourceSetId, ResourceKind>,
) -> Result<(), ValidationError> {
    validate_expression(&rule.when, resources)
        .map_err(|error| ValidationError::new(format!("when.{}", error.path), error.message))?;

    let mut obligations = HashSet::with_capacity(rule.outcome.obligations.len());
    for (index, obligation) in rule.outcome.obligations.iter().copied().enumerate() {
        if !obligations.insert(obligation) {
            return Err(ValidationError::new(
                format!("outcome.obligations[{index}]"),
                "duplicate obligation",
            ));
        }
    }

    if rule.enforcement.decision_timing != DecisionTiming::PreEffect {
        return Err(ValidationError::new(
            "enforcement.decisionTiming",
            "V1 restrictive rules require pre_effect timing",
        ));
    }
    if rule.enforcement.required_evidence.is_empty() {
        return Err(ValidationError::new(
            "enforcement.requiredEvidence",
            "must not be empty",
        ));
    }
    let mut evidence = HashSet::with_capacity(rule.enforcement.required_evidence.len());
    for (index, requirement) in rule
        .enforcement
        .required_evidence
        .iter()
        .copied()
        .enumerate()
    {
        if !evidence.insert(requirement) {
            return Err(ValidationError::new(
                format!("enforcement.requiredEvidence[{index}]"),
                "duplicate evidence requirement",
            ));
        }
    }
    Ok(())
}

fn validate_expression(
    expression: &Expression,
    resources: &HashMap<&ResourceSetId, ResourceKind>,
) -> Result<(), ValidationError> {
    match expression {
        Expression::Atom { atom } => validate_atom(atom, resources),
        Expression::All { expressions } => {
            if expressions.is_empty() || expressions.len() > MAX_ATOMS_PER_RULE {
                return Err(ValidationError::new(
                    "expressions",
                    format!("all requires 1..={MAX_ATOMS_PER_RULE} direct atoms"),
                ));
            }
            let mut event_key = None;
            for (index, child) in expressions.iter().enumerate() {
                let Expression::Atom { atom } = child else {
                    return Err(ValidationError::new(
                        format!("expressions[{index}]"),
                        "V1 all expressions may contain only direct atoms",
                    ));
                };
                validate_atom(atom, resources).map_err(|error| {
                    ValidationError::new(
                        format!("expressions[{index}].{}", error.path),
                        error.message,
                    )
                })?;
                let current = atom.event_key();
                if event_key.is_some_and(|expected| expected != current) {
                    return Err(ValidationError::new(
                        format!("expressions[{index}]"),
                        "all atoms must describe the same decision event",
                    ));
                }
                event_key = Some(current);
            }
            Ok(())
        }
        Expression::Any { .. } | Expression::Not { .. } => Err(ValidationError::new(
            "type",
            "V1 profile allows only atom and non-nested all expressions",
        )),
    }
}

fn validate_atom(
    atom: &SemanticAtom,
    resources: &HashMap<&ResourceSetId, ResourceKind>,
) -> Result<(), ValidationError> {
    match atom {
        SemanticAtom::ResourceOperation { operation, target } => {
            let kind = referenced_kind(target, resources, "atom.target")?;
            let valid = matches!(
                (operation, kind),
                (
                    ResourceOperation::Read
                        | ResourceOperation::Delete
                        | ResourceOperation::ContentMutation
                        | ResourceOperation::NamespaceMutation,
                    ResourceKind::File
                ) | (ResourceOperation::Execute, ResourceKind::Executable)
                    | (ResourceOperation::Connect, ResourceKind::Endpoint)
                    | (ResourceOperation::UseCredential, ResourceKind::Credential)
            );
            if !valid {
                return Err(ValidationError::new(
                    "atom.operation",
                    "operation is incompatible with the referenced resource kind",
                ));
            }
            Ok(())
        }
        SemanticAtom::InformationFlow {
            source,
            destination,
            propagation,
        } => {
            if *propagation != FlowPropagation::Direct {
                return Err(ValidationError::new(
                    "atom.propagation",
                    "V1 profile allows only direct information flow",
                ));
            }
            let source_kind = referenced_kind(source, resources, "atom.source")?;
            let destination_kind = referenced_kind(destination, resources, "atom.destination")?;
            if !matches!(source_kind, ResourceKind::File | ResourceKind::Credential)
                || destination_kind != ResourceKind::Endpoint
            {
                return Err(ValidationError::new(
                    "atom",
                    "V1 information flow requires a file/credential source and endpoint destination",
                ));
            }
            Ok(())
        }
    }
}

fn referenced_kind(
    target: &ResourceTarget,
    resources: &HashMap<&ResourceSetId, ResourceKind>,
    path: &str,
) -> Result<ResourceKind, ValidationError> {
    resources
        .get(target.resource_set())
        .copied()
        .ok_or_else(|| ValidationError::new(path, "referenced resource set does not exist"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::{
        EndpointMatcher, FileMatcher, FileResolution, PathMatcher, ResourceSelector,
    };
    use serde_json::Value;

    fn value_at_path<'a>(mut value: &'a Value, path: &str) -> Option<&'a Value> {
        for segment in path.split('.') {
            let field_end = segment.find('[').unwrap_or(segment.len());
            if field_end > 0 {
                value = value.get(&segment[..field_end])?;
            }

            let mut indexes = &segment[field_end..];
            while let Some(index) = indexes.strip_prefix('[') {
                let (index, remaining) = index.split_once(']')?;
                value = value.get(index.parse::<usize>().ok()?)?;
                indexes = remaining;
            }
            if !indexes.is_empty() {
                return None;
            }
        }
        Some(value)
    }

    fn rule(resource_set: &ResourceSetId, operation: ResourceOperation) -> RuleIr {
        RuleIr {
            id: RuleId::new("deny-operation").unwrap(),
            when: Expression::Atom {
                atom: SemanticAtom::ResourceOperation {
                    operation,
                    target: ResourceTarget::In {
                        resource_set: resource_set.clone(),
                    },
                },
            },
            outcome: RuleOutcome {
                decision: RestrictiveDecision::Deny,
                obligations: vec![Obligation::Audit],
                remediation: SubjectRemediation::None,
            },
            enforcement: RuleEnforcement {
                decision_timing: DecisionTiming::PreEffect,
                required_evidence: vec![EvidenceRequirement::OperationDenied],
            },
        }
    }

    fn policy(resource: ResourceSet, rule: RuleIr) -> CanonicalPolicyIr {
        CanonicalPolicyIr {
            resources: vec![resource],
            rules: vec![rule],
            activation: ActivationRequirement::BeforeWorkerStart,
            failure_policy: FailurePolicy {
                runtime: RuntimeFailurePolicy::FailClosed,
                update: UpdateFailurePolicy::KeepLastKnownGood,
            },
        }
    }

    #[test]
    fn delete_is_valid_only_for_an_existing_file_resource_set() {
        let resource_set = ResourceSetId::new("protected-files").unwrap();
        let file_resource = ResourceSet {
            id: resource_set.clone(),
            selector: ResourceSelector::File {
                matchers: vec![FileMatcher {
                    path: PathMatcher::Exact {
                        path: "/workspace/important".to_owned(),
                    },
                    resolution: FileResolution::PathEntry,
                }],
            },
        };
        assert!(
            policy(
                file_resource,
                rule(&resource_set, ResourceOperation::Delete)
            )
            .validate()
            .is_ok()
        );

        let endpoint_resource = ResourceSet {
            id: resource_set.clone(),
            selector: ResourceSelector::Endpoint {
                matchers: vec![EndpointMatcher::Host {
                    pattern: "example.com".to_owned(),
                    ports: vec![443],
                }],
            },
        };
        assert!(
            policy(
                endpoint_resource,
                rule(&resource_set, ResourceOperation::Delete)
            )
            .validate()
            .is_err()
        );
    }

    #[test]
    fn rules_cannot_reference_an_unknown_resource_set() {
        let existing = ResourceSetId::new("protected-files").unwrap();
        let missing = ResourceSetId::new("missing-files").unwrap();
        let resource = ResourceSet {
            id: existing,
            selector: ResourceSelector::File {
                matchers: vec![FileMatcher {
                    path: PathMatcher::Prefix {
                        path: "/workspace".to_owned(),
                    },
                    resolution: FileResolution::PathEntry,
                }],
            },
        };

        let policy = policy(resource, rule(&missing, ResourceOperation::Delete));
        let wire = serde_json::to_value(&policy).unwrap();
        let error = policy.validate().unwrap_err();
        assert_eq!(error.path, "rules[0].when.atom.target");
        assert!(value_at_path(&wire, &error.path).is_some());
    }

    #[test]
    fn resource_errors_point_to_fields_in_the_serialized_policy() {
        let resource_set = ResourceSetId::new("protected-files").unwrap();
        let resource = ResourceSet {
            id: resource_set.clone(),
            selector: ResourceSelector::File {
                matchers: vec![FileMatcher {
                    path: PathMatcher::Glob {
                        pattern: "/workspace/[secret]".to_owned(),
                    },
                    resolution: FileResolution::PathEntry,
                }],
            },
        };
        let policy = policy(resource, rule(&resource_set, ResourceOperation::Delete));
        let wire = serde_json::to_value(&policy).unwrap();
        let error = policy.validate().unwrap_err();

        assert_eq!(error.path, "resources[0].matchers[0].path.pattern");
        assert!(value_at_path(&wire, &error.path).is_some());
    }

    #[test]
    fn restrictive_policies_reject_audit_only_runtime_failure() {
        let resource_set = ResourceSetId::new("protected-files").unwrap();
        let resource = ResourceSet {
            id: resource_set.clone(),
            selector: ResourceSelector::File {
                matchers: vec![FileMatcher {
                    path: PathMatcher::Exact {
                        path: "/workspace/important".to_owned(),
                    },
                    resolution: FileResolution::PathEntry,
                }],
            },
        };
        let mut policy = policy(resource, rule(&resource_set, ResourceOperation::Delete));
        policy.failure_policy.runtime = RuntimeFailurePolicy::AuditOnly;

        let error = policy.validate().unwrap_err();
        assert_eq!(error.path, "failurePolicy.runtime");
    }
}
