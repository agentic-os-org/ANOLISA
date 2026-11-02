//! Recognizes the finite path partitions that can become persistent OS constraints.

use std::collections::BTreeSet;

use cedar_policy::{ActionConstraint, Policy, PolicySet, ResourceConstraint};
use cedar_policy_core::ast::{BinaryOp, Expr, ExprKind, Literal, PatternElem, Var};

use crate::PdpError;

pub(crate) fn boundaries(policies: &PolicySet) -> Result<BTreeSet<String>, PdpError> {
    let mut paths = BTreeSet::new();
    for policy in policies
        .policies()
        .filter(|policy| applies_to_files(policy))
    {
        match policy.resource_constraint() {
            ResourceConstraint::Any | ResourceConstraint::Is(_) => {}
            _ => {
                return Err(unsupported(
                    policy,
                    "filesystem policy resources must use path predicates",
                ));
            }
        }
        let ast: &cedar_policy_core::ast::Policy = policy.as_ref();
        if let Some(condition) = ast.non_scope_constraints() {
            inspect(policy, condition, &mut paths)?;
        }
    }
    Ok(paths)
}

fn applies_to_files(policy: &Policy) -> bool {
    let is_file_action = |action: &cedar_policy::EntityUid| {
        matches!(action.id().as_ref(), "file.read" | "file.write")
    };
    match policy.action_constraint() {
        ActionConstraint::Any => true,
        ActionConstraint::Eq(action) => is_file_action(&action),
        ActionConstraint::In(actions) => actions.iter().any(is_file_action),
    }
}

fn inspect(
    policy: &Policy,
    expression: &Expr,
    paths: &mut BTreeSet<String>,
) -> Result<(), PdpError> {
    match expression.expr_kind() {
        ExprKind::BinaryApp {
            op: BinaryOp::Eq,
            arg1,
            arg2,
        } => {
            if let Some(path) = path_equality(arg1, arg2).or_else(|| path_equality(arg2, arg1)) {
                check_path(path)?;
                paths.insert(path.to_owned());
                return Ok(());
            }
        }
        ExprKind::Like { expr, pattern } if is_resource_path(expr) => {
            let mut value = String::new();
            for (index, element) in pattern.get_elems().iter().enumerate() {
                match element {
                    PatternElem::Char(character) => value.push(*character),
                    PatternElem::Wildcard if index + 1 == pattern.len() && value.ends_with('/') => {
                        if value.len() > 1 {
                            value.pop();
                        }
                        check_path(&value)?;
                        paths.insert(value);
                        return Ok(());
                    }
                    PatternElem::Wildcard => {
                        return Err(unsupported(
                            policy,
                            "only directory-prefix path patterns can become OS permissions",
                        ));
                    }
                }
            }
            check_path(&value)?;
            paths.insert(value);
            return Ok(());
        }
        ExprKind::Is { expr, .. } | ExprKind::HasAttr { expr, .. }
            if matches!(expr.expr_kind(), ExprKind::Var(Var::Resource)) =>
        {
            return Ok(());
        }
        ExprKind::Var(Var::Resource) => {
            return Err(unsupported(
                policy,
                "resource may only appear in exact or directory-prefix path predicates",
            ));
        }
        ExprKind::Var(Var::Context) => {
            return Err(unsupported(
                policy,
                "runtime file-operation context cannot be fixed at sandbox launch",
            ));
        }
        _ => {}
    }
    // Process each immediate child once; subexpressions() includes the root.
    match expression.expr_kind() {
        ExprKind::If {
            test_expr,
            then_expr,
            else_expr,
        } => {
            inspect(policy, test_expr, paths)?;
            inspect(policy, then_expr, paths)?;
            inspect(policy, else_expr, paths)?;
        }
        ExprKind::And { left, right } | ExprKind::Or { left, right } => {
            inspect(policy, left, paths)?;
            inspect(policy, right, paths)?;
        }
        ExprKind::UnaryApp { arg, .. } => inspect(policy, arg, paths)?,
        ExprKind::BinaryApp { arg1, arg2, .. } => {
            inspect(policy, arg1, paths)?;
            inspect(policy, arg2, paths)?;
        }
        ExprKind::ExtensionFunctionApp { args, .. } | ExprKind::Set(args) => {
            for argument in args.iter() {
                inspect(policy, argument, paths)?;
            }
        }
        ExprKind::GetAttr { expr, .. }
        | ExprKind::HasAttr { expr, .. }
        | ExprKind::ExtHasAttr { expr, .. }
        | ExprKind::Like { expr, .. }
        | ExprKind::Is { expr, .. } => inspect(policy, expr, paths)?,
        ExprKind::Record(fields) => {
            for expression in fields.values() {
                inspect(policy, expression, paths)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn path_equality<'a>(expression: &Expr, literal: &'a Expr) -> Option<&'a str> {
    if !is_resource_path(expression) {
        return None;
    }
    match literal.expr_kind() {
        ExprKind::Lit(Literal::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn is_resource_path(expression: &Expr) -> bool {
    matches!(expression.expr_kind(), ExprKind::GetAttr { expr, attr }
        if attr == "path" && matches!(expr.expr_kind(), ExprKind::Var(Var::Resource)))
}

pub(crate) fn check_path(path: &str) -> Result<(), PdpError> {
    if !path.starts_with('/')
        || path.contains('\0')
        || path != "/"
            && (path.ends_with('/')
                || path
                    .split('/')
                    .skip(1)
                    .any(|part| matches!(part, "" | "." | "..")))
    {
        return Err(PdpError::Unsupported(format!(
            "path must be absolute and normalized: {path}"
        )));
    }
    Ok(())
}

pub(crate) fn contains(parent: &str, child: &str) -> bool {
    child == parent
        || parent == "/"
        || child
            .strip_prefix(parent)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub(crate) fn probe(directory: &str, boundaries: &BTreeSet<String>) -> String {
    let mut index = 0_u64;
    loop {
        let candidate = format!(
            "{}/__asc_permission_region_{index}__",
            directory.trim_end_matches('/')
        );
        if !boundaries.iter().any(|path| contains(&candidate, path)) {
            return candidate;
        }
        index += 1;
    }
}

fn unsupported(policy: &Policy, reason: &str) -> PdpError {
    PdpError::Unsupported(format!("{}: {reason}", policy.id()))
}
