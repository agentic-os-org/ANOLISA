//! Typed current and historical predicates with bounded, domain-aware validation.

use serde::{Deserialize, Serialize};

use super::{Action, Category, Resource, validate_file_pattern, validate_operation};
use crate::ValidationError;

/// Scalar operands never coerce between strings, integers and booleans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    /// Text or a registered enum/path value.
    String(String),
    /// Exact integer operand.
    Integer(i64),
    /// Boolean operand.
    Boolean(bool),
}

/// A scalar or flat scalar array, subject to the attribute's operand contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// One scalar.
    Scalar(Scalar),
    /// Flat operands for membership or parameter-prefix comparison.
    Array(Vec<Scalar>),
}

/// Exactly one comparison per attribute node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    /// Equality, or full-token prefix matching for argsPrefix.
    Eq(Value),
    /// Inequality, or negated prefix matching for argsPrefix.
    Ne(Value),
    /// Strict numeric lower bound.
    Gt(Value),
    /// Inclusive numeric lower bound.
    Ge(Value),
    /// Strict numeric upper bound.
    Lt(Value),
    /// Inclusive numeric upper bound.
    Le(Value),
    /// Membership in a homogeneous scalar array.
    In(Vec<Scalar>),
}

/// One logical node or one registered current-operation attribute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Condition {
    /// All child predicates must hold.
    And(Vec<Condition>),
    /// Any child predicate may hold.
    Or(Vec<Condition>),
    /// Negate one predicate; missing facts remain unknown.
    Not(Box<Condition>),
    /// Requesting executable path for read/write operations.
    Program(Comparison),
    /// Read target's exact path value.
    File(Comparison),
    /// Write operation: write, create, delete, rename or setattr.
    Operation(Comparison),
    /// Requested permission bits for setattr.
    Mode(Comparison),
    /// Requested owner for setattr.
    OwnerUid(Comparison),
    /// Requested group for setattr.
    OwnerGid(Comparison),
    /// Complete argv token prefix, excluding `argv[0]`.
    ArgsPrefix(Comparison),
    /// Working directory at exec.
    Cwd(Comparison),
}

/// An event predicate or logical composition; events cannot nest history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HistoryCondition {
    /// A matching historical event in the policy's defined history domain.
    Event(HistoryEvent),
    /// Logical combination of event predicates.
    Logic(HistoryLogic),
}

/// Logical historical-event operators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryLogic {
    /// All predicates must hold, without implied event order.
    And(Vec<HistoryCondition>),
    /// At least one predicate must hold.
    Or(Vec<HistoryCondition>),
    /// Negate a predicate; incomplete history remains unknown.
    Not(Box<HistoryCondition>),
}

/// Historical operation facts, without a decision or nested previous clause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistoryEvent {
    /// Historical operation domain.
    pub category: Category,
    /// Historical action.
    pub action: Action,
    /// Historical operation target.
    pub target: Resource,
    /// Predicate on this event's own attributes.
    #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
}

fn count_node(path: &str, depth: usize, nodes: &mut usize) -> Result<(), ValidationError> {
    *nodes += 1;
    if depth > 16 || *nodes > 4096 {
        return Err(ValidationError::new(
            path,
            "condition exceeds depth 16 or 4096 total nodes",
        ));
    }
    Ok(())
}

pub(super) fn validate(
    condition: &Condition,
    action: Action,
    path: &str,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), ValidationError> {
    count_node(path, depth, nodes)?;
    match condition {
        Condition::And(children) | Condition::Or(children) => {
            let name = if matches!(condition, Condition::And(_)) {
                "and"
            } else {
                "or"
            };
            if children.is_empty() {
                return Err(ValidationError::new(
                    path,
                    "logical groups must not be empty",
                ));
            }
            for (i, child) in children.iter().enumerate() {
                validate(
                    child,
                    action,
                    &format!("{path}.{name}[{i}]"),
                    depth + 1,
                    nodes,
                )?;
            }
            Ok(())
        }
        Condition::Not(child) => validate(child, action, &format!("{path}.not"), depth + 1, nodes),
        _ => {
            let (name, comparison, allowed) = match condition {
                Condition::Program(c) => ("program", c, action != Action::Exec),
                Condition::File(c) => ("file", c, action == Action::Read),
                Condition::Operation(c) => ("operation", c, action == Action::Write),
                Condition::Mode(c) => ("mode", c, action == Action::Write),
                Condition::OwnerUid(c) => ("ownerUid", c, action == Action::Write),
                Condition::OwnerGid(c) => ("ownerGid", c, action == Action::Write),
                Condition::ArgsPrefix(c) => ("argsPrefix", c, action == Action::Exec),
                Condition::Cwd(c) => ("cwd", c, action == Action::Exec),
                _ => unreachable!("logical nodes returned above"),
            };
            let field = format!("{path}.{name}");
            if !allowed {
                return Err(ValidationError::new(
                    field,
                    "attribute is not defined for this action",
                ));
            }
            validate_comparison(comparison, name, &field)
        }
    }
}

fn validate_comparison(
    comparison: &Comparison,
    attribute: &str,
    path: &str,
) -> Result<(), ValidationError> {
    let numeric = matches!(attribute, "mode" | "ownerUid" | "ownerGid");
    let prefix = attribute == "argsPrefix";
    let invalid = || ValidationError::new(path, "invalid operator or operand type for attribute");
    let validate_scalar = |value: &Scalar| -> Result<(), ValidationError> {
        match value {
            Scalar::Integer(n) if numeric && u32::try_from(*n).is_ok() => Ok(()),
            Scalar::String(text) if !numeric => {
                if attribute == "operation" {
                    if !matches!(
                        text.as_str(),
                        "write" | "create" | "delete" | "rename" | "setattr"
                    ) {
                        return Err(invalid());
                    }
                } else if !prefix {
                    validate_file_pattern(text)
                        .map_err(|message| ValidationError::new(path, message))?;
                    if text.contains(['*', '?']) {
                        return Err(ValidationError::new(
                            path,
                            "path comparisons require literal paths",
                        ));
                    }
                } else if text.contains('\0') {
                    return Err(invalid());
                }
                Ok(())
            }
            _ => Err(invalid()),
        }
    };
    match comparison {
        Comparison::In(values) if !prefix => {
            if values.is_empty() {
                return Err(invalid());
            }
            for value in values {
                validate_scalar(value)?;
            }
        }
        Comparison::Eq(value) | Comparison::Ne(value) => {
            if prefix {
                let Value::Array(values) = value else {
                    return Err(invalid());
                };
                if values.is_empty() {
                    return Err(invalid());
                }
                for value in values {
                    validate_scalar(value)?;
                }
            } else {
                let Value::Scalar(value) = value else {
                    return Err(invalid());
                };
                validate_scalar(value)?;
            }
        }
        Comparison::Lt(Value::Scalar(value))
        | Comparison::Le(Value::Scalar(value))
        | Comparison::Gt(Value::Scalar(value))
        | Comparison::Ge(Value::Scalar(value))
            if numeric =>
        {
            validate_scalar(value)?;
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

pub(super) fn validate_history(
    condition: &HistoryCondition,
    path: &str,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), ValidationError> {
    count_node(path, depth, nodes)?;
    match condition {
        HistoryCondition::Event(event) => {
            validate_operation(event.category, &event.target, path)?;
            if let Some(condition) = &event.condition {
                validate(
                    condition,
                    event.action,
                    &format!("{path}.where"),
                    depth + 1,
                    nodes,
                )?;
            }
            Ok(())
        }
        HistoryCondition::Logic(HistoryLogic::And(children) | HistoryLogic::Or(children)) => {
            let name = if matches!(condition, HistoryCondition::Logic(HistoryLogic::And(_))) {
                "and"
            } else {
                "or"
            };
            if children.is_empty() {
                return Err(ValidationError::new(
                    path,
                    "logical groups must not be empty",
                ));
            }
            for (i, child) in children.iter().enumerate() {
                validate_history(child, &format!("{path}.{name}[{i}]"), depth + 1, nodes)?;
            }
            Ok(())
        }
        HistoryCondition::Logic(HistoryLogic::Not(child)) => {
            validate_history(child, &format!("{path}.not"), depth + 1, nodes)
        }
    }
}
