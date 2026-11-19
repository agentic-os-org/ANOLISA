//! Bounded Boolean lowering that preserves each operation's path conditions.

use asc_file_policy::{FilePathPattern, PatternSyntax};
use cedar_policy::pst::{BinaryOp, Expr, Literal, PatternElem, UnaryOp, Var};

use super::{
    MAX_TARGETS, Result,
    paths::{PathScope, path_field, tree, validate_path},
};

pub(super) const DELETE: u8 = 1;
pub(super) const WRITE: u8 = 2;
// Unconstrained file.write includes create, rename and setattr as well.
pub(super) const WRITE_OPERATIONS: u8 = 31;
pub(super) const READ: u8 = 32;
const ALL_OPERATIONS: u8 = WRITE_OPERATIONS | READ;

#[derive(Clone)]
/// Conjunctive file conditions extracted from one Cedar statement.
pub struct FileRule {
    pub(super) operations: u8,
    /// Positive literal scope, when supplied by equality or a tree expression.
    pub range: Option<PathScope>,
    pub(super) excluded: Vec<PathScope>,
    /// All path predicates in this branch, including negated predicates.
    pub patterns: Vec<FilePathPattern>,
    pub(super) origins: Vec<usize>,
}

impl Default for FileRule {
    fn default() -> Self {
        Self {
            operations: ALL_OPERATIONS,
            range: None,
            excluded: Vec::new(),
            patterns: Vec::new(),
            origins: Vec::new(),
        }
    }
}

pub(super) fn and(left: Vec<FileRule>, right: &[FileRule]) -> Result<Vec<FileRule>> {
    let mut result = Vec::new();
    for a in left {
        for b in right {
            let operations = a.operations & b.operations;
            if operations == 0 {
                continue;
            }
            let range = match (&a.range, &b.range) {
                (Some(a), Some(b)) if a.contains(b) => Some(b.clone()),
                (Some(a), Some(b)) if b.contains(a) => Some(a.clone()),
                (Some(_), Some(_)) => continue,
                (a, b) => a.clone().or_else(|| b.clone()),
            };
            if result.len() == MAX_TARGETS {
                return Err("EXPRESSION_LIMIT_EXCEEDED");
            }
            let mut excluded = a.excluded.clone();
            excluded.extend(b.excluded.iter().cloned());
            if excluded.len() > MAX_TARGETS {
                return Err("EXPRESSION_LIMIT_EXCEEDED");
            }
            let mut patterns = a.patterns.clone();
            patterns.extend(b.patterns.iter().cloned());
            if patterns.len() > MAX_TARGETS {
                return Err("EXPRESSION_LIMIT_EXCEEDED");
            }
            result.push(FileRule {
                operations,
                range,
                excluded,
                patterns,
                origins: a.origins.iter().chain(&b.origins).copied().collect(),
            });
        }
    }
    Ok(result)
}

fn or(mut left: Vec<FileRule>, right: Vec<FileRule>) -> Result<Vec<FileRule>> {
    if left.len() + right.len() > MAX_TARGETS {
        return Err("EXPRESSION_LIMIT_EXCEEDED");
    }
    left.extend(right);
    Ok(left)
}

fn path(path: &str, is_tree: bool, negate: bool) -> Result<Vec<FileRule>> {
    validate_path(path)?;
    let range = if is_tree {
        PathScope::Tree(path.into())
    } else {
        PathScope::Exact(path.into())
    };
    let mut branch = FileRule::default();
    if negate {
        branch.excluded.push(range);
    } else {
        branch.range = Some(range);
    }
    Ok(vec![branch])
}

fn operation_field(expr: &Expr) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == "operation" &&
        matches!(expr.as_ref(), Expr::GetAttr { expr, attr } if attr == "input" && matches!(expr.as_ref(), Expr::Var(Var::Context))))
}

fn equality(field: &Expr, value: &Expr, negate: bool) -> Result<Vec<FileRule>> {
    if let Expr::Literal(Literal::String(value)) = value
        && path_field(field)
    {
        return path(value, false, negate);
    }
    if let Expr::Literal(Literal::EntityUID(value)) = value
        && operation_field(field)
        && value.ty.to_string() == "SecCore::WriteOperation"
    {
        let operations = match value.eid.as_str() {
            "delete" => DELETE,
            "write" => WRITE,
            _ => return Err("UNSUPPORTED_OPERATION"),
        };
        return Ok(vec![FileRule {
            operations: if negate {
                ALL_OPERATIONS & !operations
            } else {
                operations
            },
            ..FileRule::default()
        }]);
    }
    Err("UNSUPPORTED_CONDITION")
}

fn like(pattern: &[PatternElem], negate: bool) -> FileRule {
    let mut value = String::new();
    for part in pattern {
        match part {
            PatternElem::Wildcard => value.push('*'),
            PatternElem::Char(c) => {
                if matches!(c, '*' | '\\') {
                    value.push('\\');
                }
                value.push(*c);
            }
        }
    }
    let pattern = FilePathPattern {
        syntax: PatternSyntax::CedarLike,
        value,
        negated: negate,
    };
    FileRule {
        patterns: vec![pattern],
        ..FileRule::default()
    }
}

pub(super) fn expression(
    expr: &Expr,
    negate: bool,
    depth: usize,
    origins: &mut std::ops::Range<usize>,
) -> Result<Vec<FileRule>> {
    if depth > 64 {
        return Err("EXPRESSION_LIMIT_EXCEEDED");
    }
    if let Some(root) = tree(expr) {
        return path(&root, true, negate);
    }
    match expr {
        Expr::Like { expr, pattern } if path_field(expr) => Ok(vec![like(pattern, negate)]),
        Expr::Literal(Literal::Bool(value)) => Ok(if *value == negate {
            vec![]
        } else {
            vec![FileRule::default()]
        }),
        Expr::UnaryOp {
            op: UnaryOp::Not,
            expr,
        } => expression(expr, !negate, depth + 1, &mut (0..0)),
        Expr::BinaryOp {
            op: BinaryOp::And | BinaryOp::Or,
            left,
            right,
        } => {
            let a = expression(left, negate, depth + 1, origins)?;
            let b = expression(right, negate, depth + 1, origins)?;
            let conjunction = matches!(
                expr,
                Expr::BinaryOp {
                    op: BinaryOp::And,
                    ..
                }
            ) != negate;
            if conjunction { and(a, &b) } else { or(a, b) }
        }
        Expr::BinaryOp {
            op: op @ (BinaryOp::Eq | BinaryOp::NotEq),
            left,
            right,
        } => {
            let (field, value) = if matches!(left.as_ref(), Expr::Literal(_)) {
                (right, left)
            } else {
                (left, right)
            };
            let mut branches = equality(field, value, negate != (*op == BinaryOp::NotEq))?;
            if *op == BinaryOp::Eq
                && operation_field(field)
                && let Some(origin) = origins.next()
            {
                for branch in &mut branches {
                    branch.origins.push(origin);
                }
            }
            Ok(branches)
        }
        Expr::BinaryOp {
            op: BinaryOp::Contains,
            left,
            right,
        } => {
            if !path_field(right) && !operation_field(right) {
                return Err("UNSUPPORTED_CONDITION");
            }
            let Expr::Set(values) = left.as_ref() else {
                return Err("UNSUPPORTED_CONDITION");
            };
            if values.len() > MAX_TARGETS {
                return Err("EXPRESSION_LIMIT_EXCEEDED");
            }
            let mut result = if negate {
                vec![FileRule::default()]
            } else {
                vec![]
            };
            for value in values {
                let next = equality(right, value, negate)?;
                result = if negate {
                    and(result, &next)?
                } else {
                    or(result, next)?
                };
            }
            Ok(result)
        }
        _ => Err("UNSUPPORTED_CONDITION"),
    }
}
