//! Recognizes the finite path partitions that can become persistent OS constraints.

use std::collections::BTreeSet;

use asc_policy_types::cedar::CedarSchemaReference;
use cedar_policy::{Policy, ResourceConstraint};
use cedar_policy_core::ast::{BinaryOp, Expr, ExprKind, Literal, PatternElem, Var};

use crate::PdpError;

pub(crate) fn collect_boundaries(
    policy: &Policy,
    schema: &CedarSchemaReference,
    paths: &mut BTreeSet<String>,
) -> Result<(), PdpError> {
    if !asc_policy_cedar::deployment::header_may_match(
        &policy.action_constraint(),
        &policy.resource_constraint(),
        &["file.read", "file.write"],
        "SecCore::File",
    ) {
        return Ok(());
    }
    if !matches!(
        policy.resource_constraint(),
        ResourceConstraint::Any | ResourceConstraint::Is(_)
    ) {
        return Err(unsupported(
            policy,
            "filesystem policy resources must use path predicates",
        ));
    }
    let ast: &cedar_policy_core::ast::Policy = policy.as_ref();
    if let Some(condition) = ast.non_scope_constraints() {
        inspect(
            policy,
            condition,
            paths,
            asc_policy_cedar::schema::uses_principal_targeting(schema),
        )?;
    }
    Ok(())
}

fn inspect(
    policy: &Policy,
    expression: &Expr,
    paths: &mut BTreeSet<String>,
    ordinary_write: bool,
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
        // Schema v3 shares rules with other consumers. Cedar evaluates operation
        // against an ordinary write, while sibling path predicates still partition it.
        ExprKind::GetAttr { expr, attr }
            if ordinary_write
                && attr == "operation"
                && matches!(expr.expr_kind(), ExprKind::GetAttr { expr, attr } if attr == "input"
                && matches!(expr.expr_kind(), ExprKind::Var(Var::Context))) =>
        {
            return Ok(());
        }
        ExprKind::Var(Var::Context) => {
            return Err(unsupported(
                policy,
                "runtime file-operation context cannot be fixed at sandbox launch",
            ));
        }
        _ => {}
    }
    visit_children(expression, |child| {
        inspect(policy, child, paths, ordinary_write)
    })
}

// Traverse immediate children because subexpressions() includes the root itself.
fn visit_children(
    expression: &Expr,
    mut visit: impl FnMut(&Expr) -> Result<(), PdpError>,
) -> Result<(), PdpError> {
    match expression.expr_kind() {
        ExprKind::If {
            test_expr,
            then_expr,
            else_expr,
        } => {
            visit(test_expr)?;
            visit(then_expr)?;
            visit(else_expr)?;
        }
        ExprKind::And { left, right } | ExprKind::Or { left, right } => {
            visit(left)?;
            visit(right)?;
        }
        ExprKind::UnaryApp { arg, .. } => visit(arg)?,
        ExprKind::BinaryApp { arg1, arg2, .. } => {
            visit(arg1)?;
            visit(arg2)?;
        }
        ExprKind::ExtensionFunctionApp { args, .. } | ExprKind::Set(args) => {
            for argument in args.iter() {
                visit(argument)?;
            }
        }
        ExprKind::GetAttr { expr, .. }
        | ExprKind::HasAttr { expr, .. }
        | ExprKind::ExtHasAttr { expr, .. }
        | ExprKind::Like { expr, .. }
        | ExprKind::Is { expr, .. } => visit(expr)?,
        ExprKind::Record(fields) => {
            for expression in fields.values() {
                visit(expression)?;
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
