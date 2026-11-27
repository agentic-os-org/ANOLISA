//! Lower the supported Cedar forbid subset to shared file-protection intent.

mod expressions;
mod paths;

use std::collections::BTreeMap;

use asc_file_policy::{
    FILE_PROTECTION_POLICY_VERSION, FilePlan, FileProtectionPolicy, FileProtectionTarget,
    FileProtectionTopologyMode, FileProtections,
};
use asc_policy_types::{cedar::CedarPolicySource, target::TranslationRejection};
use cedar_policy::pst::{
    ActionConstraint, Clause, Effect, PrincipalConstraint, ResourceConstraint,
};

pub use expressions::FileRule;
pub use paths::PathScope;

use expressions::{DELETE, READ, WRITE, WRITE_OPERATIONS, and, expression};

pub(super) const MAX_TARGETS: usize = 64;
type Result<T> = std::result::Result<T, &'static str>;

/// Process target established by a file statement's subject predicates.
#[must_use]
pub fn placement(rule: &crate::deployment::StatementTarget) -> crate::deployment::Placement<'_> {
    use crate::deployment::Placement;
    match &rule.subject {
        Ok(Some(subject)) => Placement::Bind(subject),
        Ok(None) => Placement::Ignore,
        Err(error) => Placement::Unassigned(error.to_string()),
    }
}

/// Select the file operations shared by process file backends before target-specific lowering.
#[must_use]
pub fn select_operations(
    input: &crate::selection::ValidatedStatement<'_>,
) -> crate::selection::SelectedStatement {
    use crate::selection::SelectedStatement;
    let rule = input.rule;
    if !rule.may_match(&["file.read", "file.write", "file.exec"], "SecCore::File") {
        return SelectedStatement::Unrelated;
    }
    if let crate::deployment::Placement::Unassigned(reason) = placement(rule) {
        return SelectedStatement::Unsupported(reason);
    }
    if !rule.may_match(&["file.write"], "SecCore::File") {
        return SelectedStatement::Whole;
    }
    match input.select_operations(&["write", "delete"]) {
        SelectedStatement::Unrelated => SelectedStatement::Unsupported(
            "ActPlane supports only ordinary write and delete for file.write".into(),
        ),
        selected => selected,
    }
}

/// Compiles a source snapshot using the selected Translator's target lowering.
///
/// # Errors
/// Rejects invalid source or a file rule that cannot be expressed by the target.
pub fn translate(
    source: &CedarPolicySource,
    lower_target: fn(&FileRule) -> Result<Option<FileProtectionTarget>>,
) -> std::result::Result<FilePlan, TranslationRejection> {
    let policies = crate::pst(source).map_err(|_| reject("INVALID_CEDAR_POLICY"))?;
    let files = crate::project_pst(&policies, crate::PolicyDomain::File)
        .ok_or_else(|| reject("CEDAR_EMPTY_EFFECTIVE_POLICY"))?;
    translate_pst(&files, lower_target)
}

/// Extracts each file condition from a validated PST, then calls the Translator
/// to select its target. A rejection retains the original statement index.
///
/// # Errors
/// Rejects the entire projection if extraction or target lowering fails.
pub fn translate_pst(
    policies: &cedar_policy::pst::PolicySet,
    lower_target: fn(&FileRule) -> Result<Option<FileProtectionTarget>>,
) -> std::result::Result<FilePlan, TranslationRejection> {
    translate_with_origins(policies, lower_target, 0, |_, _| {})
}

/// Lowers a projected file policy and records the target of each positive
/// operation equality before targets with the same path are merged.
///
/// # Errors
/// Returns the same extraction and target-lowering errors as [`translate_pst`].
pub fn translate_with_origins(
    policies: &cedar_policy::pst::PolicySet,
    lower_target: fn(&FileRule) -> Result<Option<FileProtectionTarget>>,
    origin_count: usize,
    mut record: impl FnMut(usize, &FileProtectionTarget),
) -> std::result::Result<FilePlan, TranslationRejection> {
    let mut origins = 0..origin_count;
    let mut targets = Vec::new();
    let mut patterned = Vec::new();
    for (index, (id, policy)) in policies.policies.iter().enumerate() {
        // Projection retains statement identities when HTTP rules precede file rules.
        let index = id
            .to_string()
            .strip_prefix("policy")
            .and_then(|position| position.parse::<usize>().ok())
            .unwrap_or(index);
        let translated = (|| {
            for branch in extract_rule(policy.body(), &mut origins)? {
                if let Some(mut item) = lower_target(&branch)? {
                    for &index in &branch.origins {
                        record(index, &item);
                    }
                    // Identical scopes may share operation bits. Different exclusions
                    // on the same root are rejected by the structured intent.
                    item.exclusions.sort_by(|a, b| a.path.cmp(&b.path));
                    item.exclusions.dedup();
                    if branch.patterns.is_empty() {
                        merge(&mut targets, item)?;
                    } else {
                        patterned.push((item, branch.patterns));
                    }
                    if targets.len() + patterned.len() > MAX_TARGETS {
                        return Err("TARGET_LIMIT_EXCEEDED");
                    }
                }
            }
            Ok(())
        })();
        translated.map_err(|code| reject(&format!("CEDAR_RULE_{index}_{code}")))?;
    }
    if targets.is_empty() && patterned.is_empty() {
        return Err(reject("CEDAR_EMPTY_EFFECTIVE_POLICY"));
    }
    targets.sort_by(|a, b| a.path.cmp(&b.path));
    let mut patterns = BTreeMap::new();
    for (target, predicates) in patterned {
        patterns.insert(targets.len(), predicates);
        targets.push(target);
    }
    Ok((
        FileProtectionPolicy {
            version: FILE_PROTECTION_POLICY_VERSION,
            topology_mode: FileProtectionTopologyMode::StaticAtApply,
            targets,
        },
        patterns,
    ))
}

fn extract_rule(
    body: &cedar_policy::pst::Template,
    origins: &mut std::ops::Range<usize>,
) -> Result<Vec<FileRule>> {
    if body.principal != PrincipalConstraint::Any {
        return Err("UNSUPPORTED_PRINCIPAL_CONSTRAINT");
    }
    if !body.annotations.is_empty() {
        return Err("UNSUPPORTED_ANNOTATION");
    }
    if body.effect != Effect::Forbid {
        return Err("UNSUPPORTED_EFFECT");
    }
    let all_actions = |name: &str| {
        let matches = |id: &cedar_policy::pst::EntityUID| {
            id.ty.to_string() == "SecCore::Action" && id.eid == name
        };
        match &body.action {
            ActionConstraint::Eq(id) => matches(id),
            ActionConstraint::In(ids) => !ids.is_empty() && ids.iter().all(matches),
            ActionConstraint::Any => false,
        }
    };
    let operations = if all_actions("file.write") {
        WRITE_OPERATIONS
    } else if all_actions("file.read") {
        READ
    } else {
        return Err("UNSUPPORTED_ACTION");
    };
    match &body.resource {
        ResourceConstraint::Any => {}
        ResourceConstraint::Is(ty) if ty.to_string() == "SecCore::File" => {}
        _ => return Err("UNSUPPORTED_RESOURCE_CONSTRAINT"),
    }
    let mut branches = vec![FileRule {
        operations,
        ..FileRule::default()
    }];
    for clause in body.clauses() {
        let lowered = match clause {
            Clause::When(expr) => expression(expr, false, 0, origins)?,
            Clause::Unless(expr) => expression(expr, true, 0, &mut (0..0))?,
        };
        branches = and(branches, &lowered)?;
    }
    if branches
        .iter()
        .any(|branch| branch.operations & !(DELETE | WRITE | READ) != 0)
    {
        return Err("UNSUPPORTED_OPERATION");
    }
    Ok(branches)
}

fn merge(targets: &mut Vec<FileProtectionTarget>, item: FileProtectionTarget) -> Result<()> {
    if let Some(existing) = targets.iter_mut().find(|target| target.path == item.path) {
        if existing.kind != item.kind || existing.exclusions != item.exclusions {
            return Err("CONFLICTING_TARGET_SCOPES");
        }
        existing.protections.delete |= item.protections.delete;
        existing.protections.ordinary_write |= item.protections.ordinary_write;
        existing.protections.read |= item.protections.read;
    } else {
        if targets.len() == MAX_TARGETS {
            return Err("TARGET_LIMIT_EXCEEDED");
        }
        targets.push(item);
    }
    Ok(())
}

fn protections(operations: u8) -> FileProtections {
    FileProtections {
        delete: operations & DELETE != 0,
        ordinary_write: operations & WRITE != 0,
        read: operations & READ != 0,
    }
}

fn reject(code: &str) -> TranslationRejection {
    TranslationRejection { code: code.into() }
}
