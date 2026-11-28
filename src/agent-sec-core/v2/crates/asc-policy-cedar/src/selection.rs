//! Static PEP selection over validated, revision-pinned Cedar statements.

mod operations;

use asc_policy_types::{
    cedar::{CedarPolicySource, CedarSchemaReference},
    identifiers::{ResourceId, Revision},
};

use crate::{CedarError, deployment::StatementTarget};

/// Parsed once for all consumers of a consistent PAP snapshot.
#[derive(Debug)]
pub struct ValidatedPolicy {
    /// Original statement identities are stable inside this source revision.
    pub statements: Vec<StatementTarget>,
    /// Request selection is independent of process deployment eligibility.
    pub request_eligible: Result<bool, String>,
}

impl ValidatedPolicy {
    /// Parse and validate source before invoking any consumer.
    ///
    /// # Errors
    /// Returns Cedar source validation and syntax representation errors.
    pub fn parse(source: &CedarPolicySource) -> Result<Self, CedarError> {
        let (policies, _) = crate::validation::parse_and_validate(source)?;
        let statements = crate::deployment::statements_from_policies(&policies)?;
        Ok(Self {
            request_eligible: crate::sandbox_policy_headers(&statements)
                .map_err(|error| error.diagnostic()),
            statements,
        })
    }
}

/// Complete input to an adapter. Process and request facts are supplied at execution.
pub struct ValidatedStatement<'a> {
    pub policy_id: &'a ResourceId,
    pub revision: Revision,
    pub schema: &'a CedarSchemaReference,
    pub rule: &'a StatementTarget,
}

/// Adapter-owned static selection callback.
pub type Select = fn(&ValidatedStatement<'_>) -> Selection;

/// A statement ID can be interpreted only within its immutable source revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementRef {
    pub policy_id: ResourceId,
    pub revision: Revision,
    pub statement_id: String,
}

/// Source location of an explicit equality on context.input.operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationBranch {
    /// Zero-based position in the source statement's when/unless clauses.
    pub clause: usize,
    /// From the clause root, false takes the left operand and true takes the right.
    pub path: Vec<bool>,
    /// Literal `SecCore::WriteOperation` identifier.
    pub operation: String,
}

/// Executable result and the extent to which static coverage is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectedStatement {
    /// Consume the complete original.
    Whole,
    /// Consume the complete original, whose partial coverage cannot be proven.
    Undetermined,
    /// Complete Cedar statement with its retained branch locations in the source.
    Fragment {
        cedar: String,
        branches: Vec<OperationBranch>,
    },
    /// Statement outside the consumer's domain.
    Unrelated,
    /// Relevant statement the adapter cannot consume.
    Unsupported(String),
}

impl SelectedStatement {
    /// Whether this outcome contributes executable input.
    #[must_use]
    pub fn is_selected(&self) -> bool {
        matches!(
            self,
            Self::Whole | Self::Undetermined | Self::Fragment { .. }
        )
    }
}

/// Direction of a conditional enforcement difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Stricter,
    Weaker,
}

/// A known difference in an adapter's capabilities; independent of runtime receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementDeviation {
    pub branch: OperationBranch,
    pub direction: Direction,
    pub code: &'static str,
    pub condition: &'static str,
    pub explanation: &'static str,
}

/// Source identity, executable input and adapter-provided capability differences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub source: StatementRef,
    pub selected: SelectedStatement,
    pub deviations: Vec<EnforcementDeviation>,
}

impl ValidatedStatement<'_> {
    /// Attribute an adapter's result to the exact source considered.
    #[must_use]
    pub fn selection(&self, selected: SelectedStatement) -> Selection {
        Selection {
            source: StatementRef {
                policy_id: self.policy_id.clone(),
                revision: self.revision,
                statement_id: self.rule.id.clone(),
            },
            selected,
            deviations: Vec::new(),
        }
    }

    /// Select positive file-write alternatives without moving any other predicate.
    #[must_use]
    pub fn select_operations(&self, operations: &[&str]) -> SelectedStatement {
        operations::project(&self.rule.syntax, operations)
    }

    /// Locate the original positive operation equalities for capability annotations.
    #[must_use]
    pub fn operation_branches(&self) -> Vec<OperationBranch> {
        operations::branches(&self.rule.syntax)
    }
}
