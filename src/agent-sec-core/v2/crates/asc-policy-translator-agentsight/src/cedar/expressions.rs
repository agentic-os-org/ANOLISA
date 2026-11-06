//! Bounded Boolean lowering that preserves each operation's path conditions.

use cedar_policy::pst::{BinaryOp, Expr, Literal, UnaryOp, Var};

use super::{
    MAX_TARGETS, Result,
    paths::{Range, path_field, tree, validate_path},
};

pub(super) const DELETE: u8 = 1;
pub(super) const WRITE: u8 = 2;
// Unconstrained operation includes create, rename and setattr as well.
const ALL_OPERATIONS: u8 = 31;

#[derive(Clone)]
pub(super) struct Branch {
    pub(super) operations: u8,
    pub(super) range: Option<Range>,
    pub(super) excluded: Vec<Range>,
}

impl Default for Branch {
    fn default() -> Self {
        Self {
            operations: ALL_OPERATIONS,
            range: None,
            excluded: Vec::new(),
        }
    }
}

pub(super) fn and(left: Vec<Branch>, right: &[Branch]) -> Result<Vec<Branch>> {
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
            result.push(Branch {
                operations,
                range,
                excluded,
            });
        }
    }
    Ok(result)
}

fn or(mut left: Vec<Branch>, right: Vec<Branch>) -> Result<Vec<Branch>> {
    if left.len() + right.len() > MAX_TARGETS {
        return Err("EXPRESSION_LIMIT_EXCEEDED");
    }
    left.extend(right);
    Ok(left)
}

fn path(path: &str, is_tree: bool, negate: bool) -> Result<Vec<Branch>> {
    validate_path(path)?;
    let range = if is_tree {
        Range::Tree(path.into())
    } else {
        Range::Exact(path.into())
    };
    let mut branch = Branch::default();
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

fn equality(field: &Expr, value: &Expr, negate: bool) -> Result<Vec<Branch>> {
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
        return Ok(vec![Branch {
            operations: if negate {
                ALL_OPERATIONS & !operations
            } else {
                operations
            },
            ..Branch::default()
        }]);
    }
    Err("UNSUPPORTED_CONDITION")
}

pub(super) fn expression(expr: &Expr, negate: bool, depth: usize) -> Result<Vec<Branch>> {
    if depth > 64 {
        return Err("EXPRESSION_LIMIT_EXCEEDED");
    }
    if let Some(root) = tree(expr) {
        return path(&root, true, negate);
    }
    match expr {
        Expr::Literal(Literal::Bool(value)) => Ok(if *value == negate {
            vec![]
        } else {
            vec![Branch::default()]
        }),
        Expr::UnaryOp {
            op: UnaryOp::Not,
            expr,
        } => expression(expr, !negate, depth + 1),
        Expr::BinaryOp {
            op: BinaryOp::And | BinaryOp::Or,
            left,
            right,
        } => {
            let a = expression(left, negate, depth + 1)?;
            let b = expression(right, negate, depth + 1)?;
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
            op: BinaryOp::Eq | BinaryOp::NotEq,
            left,
            right,
        } => {
            let negate = negate
                != matches!(
                    expr,
                    Expr::BinaryOp {
                        op: BinaryOp::NotEq,
                        ..
                    }
                );
            if matches!(left.as_ref(), Expr::Literal(_)) {
                equality(right, left, negate)
            } else {
                equality(left, right, negate)
            }
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
                vec![Branch::default()]
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
