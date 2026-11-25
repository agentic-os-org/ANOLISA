//! Project explicit alternatives on file.write's required, typed operation field.

use std::sync::Arc;

use cedar_policy::pst::{ActionConstraint, BinaryOp, Clause, Expr, Literal, StaticPolicy, Var};

use super::{OperationBranch, SelectedStatement};

fn file_write(rule: &StaticPolicy) -> bool {
    let matches = |id: &cedar_policy::pst::EntityUID| {
        id.ty.to_string() == "SecCore::Action" && id.eid == "file.write"
    };
    match &rule.body().action {
        ActionConstraint::Eq(id) => matches(id),
        ActionConstraint::In(ids) => !ids.is_empty() && ids.iter().all(matches),
        ActionConstraint::Any => false,
    }
}

fn operation_field(expr: &Expr) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == "operation" &&
        matches!(expr.as_ref(), Expr::GetAttr { expr, attr } if attr == "input" && matches!(expr.as_ref(), Expr::Var(Var::Context))))
}

fn operation(expr: &Expr) -> Option<&str> {
    let Expr::BinaryOp {
        op: BinaryOp::Eq,
        left,
        right,
    } = expr
    else {
        return None;
    };
    let (field, value) = if matches!(left.as_ref(), Expr::Literal(_)) {
        (right, left)
    } else {
        (left, right)
    };
    match value.as_ref() {
        Expr::Literal(Literal::EntityUID(id))
            if operation_field(field) && id.ty.to_string() == "SecCore::WriteOperation" =>
        {
            Some(id.eid.as_str())
        }
        _ => None,
    }
}

fn uses_operation(expr: &Expr) -> bool {
    expr.reduce(
        &|node| operation_field(node).then_some(true),
        &|a, b| a || b,
        false,
    )
}

// Only claim totality for the fields the file.write request guarantees or a
// principal attribute just checked with `has`. Keep other predicates intact.
fn guaranteed(expr: &Expr) -> bool {
    fn attr_on(expr: &Expr, var: &Var, name: &str) -> bool {
        matches!(expr, Expr::GetAttr { expr, attr } if attr == name && matches!(expr.as_ref(), Expr::Var(v) if v == var))
    }
    fn guarded(expr: &Expr, guard: &Expr) -> bool {
        let Expr::HasAttr { expr: base, attrs } = guard else {
            return false;
        };
        if !matches!(base.as_ref(), Expr::Var(Var::Principal)) || attrs.len() != 1 {
            return false;
        }
        matches!(expr, Expr::BinaryOp {op: BinaryOp::Eq, left, right}
            if attr_on(left, &Var::Principal, &attrs.head) && matches!(right.as_ref(), Expr::Literal(_))
                || attr_on(right, &Var::Principal, &attrs.head) && matches!(left.as_ref(), Expr::Literal(_)))
    }
    match expr {
        Expr::Literal(_) => true,
        Expr::HasAttr { expr, .. } => matches!(expr.as_ref(), Expr::Var(Var::Principal)),
        Expr::GetAttr { .. } => attr_on(expr, &Var::Resource, "path") || operation_field(expr),
        Expr::Like { expr, .. } => attr_on(expr, &Var::Resource, "path"),
        Expr::BinaryOp {
            op: BinaryOp::Eq | BinaryOp::Or,
            left,
            right,
        } => guaranteed(left) && guaranteed(right),
        Expr::BinaryOp {
            op: BinaryOp::And,
            left,
            right,
        } => guaranteed(left) && (guaranteed(right) || guarded(right, left)),
        _ => false,
    }
}

struct Projected {
    // None denotes a discarded, definitely-false arm for the allowed operations.
    expr: Option<Arc<Expr>>,
    // Every successful evaluation of the source entails an explicit operation.
    bounded: bool,
    // Evaluating this subtree cannot encounter an optional field or other Cedar error.
    total: bool,
}

struct Projector<'a> {
    allowed: &'a [&'a str],
    branches: Vec<OperationBranch>,
    removed: bool,
}

impl Projector<'_> {
    fn visit(
        &mut self,
        expr: &Arc<Expr>,
        clause: usize,
        path: &mut Vec<bool>,
    ) -> Option<Projected> {
        if let Some(name) = operation(expr) {
            let keep = self.allowed.contains(&name);
            if keep {
                self.branches.push(OperationBranch {
                    clause,
                    path: path.clone(),
                    operation: name.into(),
                });
            } else {
                self.removed = true;
            }
            return Some(Projected {
                expr: keep.then(|| Arc::clone(expr)),
                bounded: true,
                total: true,
            });
        }
        if let Expr::BinaryOp {
            op: op @ (BinaryOp::And | BinaryOp::Or),
            left,
            right,
        } = expr.as_ref()
        {
            let before = self.branches.len();
            path.push(false);
            let a = self.visit(left, clause, path)?;
            path.pop();
            path.push(true);
            let b = self.visit(right, clause, path)?;
            path.pop();
            let and = matches!(op, BinaryOp::And);
            let bounded = if and {
                a.bounded || b.bounded
            } else {
                a.bounded && b.bounded
            };
            let mut total = (a.total && b.total) || guaranteed(expr);
            let projected = match (a.expr, b.expr) {
                (Some(left), Some(right)) => Some(Arc::new(Expr::BinaryOp {
                    op: *op,
                    left,
                    right,
                })),
                (None, _) if and => {
                    total = true;
                    None
                }
                (Some(_), None) if and && !a.total => return None,
                (_, None) if and => None,
                (left, right) => left.or(right),
            };
            if projected.is_none() {
                self.branches.truncate(before);
            }
            return Some(Projected {
                expr: projected,
                bounded,
                total,
            });
        }
        // Negation, set membership and computed expressions need whole-rule evaluation.
        (!uses_operation(expr)).then(|| Projected {
            expr: Some(Arc::clone(expr)),
            bounded: false,
            total: guaranteed(expr),
        })
    }
}

pub(super) fn branches(rule: &StaticPolicy) -> Vec<OperationBranch> {
    fn collect(expr: &Expr, index: usize, path: &mut Vec<bool>, output: &mut Vec<OperationBranch>) {
        if let Some(name) = operation(expr) {
            output.push(OperationBranch {
                clause: index,
                path: path.clone(),
                operation: name.into(),
            });
        } else if let Expr::BinaryOp {
            op: BinaryOp::And | BinaryOp::Or,
            left,
            right,
        } = expr
        {
            path.push(false);
            collect(left, index, path, output);
            path.pop();
            path.push(true);
            collect(right, index, path, output);
            path.pop();
        }
    }
    let mut output = Vec::new();
    if file_write(rule) {
        for (i, clause) in rule.body().clauses().iter().enumerate() {
            if let Clause::When(expr) = clause {
                collect(expr, i, &mut Vec::new(), &mut output);
            }
        }
    }
    output
}

pub(super) fn project(rule: &StaticPolicy, allowed: &[&str]) -> SelectedStatement {
    if !file_write(rule) {
        return SelectedStatement::Undetermined;
    }
    let mut projector = Projector {
        allowed,
        branches: Vec::new(),
        removed: false,
    };
    let mut bounded = false;
    let mut prior_total = true;
    let mut clauses = Vec::new();
    for (index, clause) in rule.body().clauses().iter().enumerate() {
        match clause {
            Clause::When(expr) => {
                let Some(projected) = projector.visit(expr, index, &mut Vec::new()) else {
                    return SelectedStatement::Undetermined;
                };
                bounded |= projected.bounded;
                let total = projected.total;
                match projected.expr {
                    Some(expr) => clauses.push(Clause::When(expr)),
                    None if prior_total => return SelectedStatement::Unrelated,
                    None => return SelectedStatement::Undetermined,
                }
                prior_total &= total;
            }
            Clause::Unless(expr) if uses_operation(expr) => return SelectedStatement::Undetermined,
            Clause::Unless(expr) => {
                prior_total &= guaranteed(expr);
                clauses.push(clause.clone());
            }
        }
    }
    if !bounded {
        return SelectedStatement::Undetermined;
    }
    if !projector.removed {
        return SelectedStatement::Whole;
    }
    let Ok(body) = rule.body().clone().try_with_clauses(clauses) else {
        return SelectedStatement::Undetermined;
    };
    match StaticPolicy::try_from(body) {
        Ok(fragment) => SelectedStatement::Fragment {
            cedar: fragment.to_string(),
            branches: projector.branches,
        },
        Err(_) => SelectedStatement::Undetermined,
    }
}
