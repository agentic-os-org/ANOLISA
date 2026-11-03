//! Lower the supported Cedar forbid subset to Sight file protection.

mod expressions;
mod paths;

use asc_agentsight_protocol::{
    FILE_PROTECTION_POLICY_VERSION, FileProtectionPolicy, FileProtectionTarget,
    FileProtectionTopologyMode, FileProtections,
};
use asc_policy_types::{cedar::CedarPolicySource, target::TranslationRejection};
use cedar_policy::pst::{ActionConstraint, Clause, Effect, ResourceConstraint};

use expressions::{Branch, DELETE, WRITE, and, expression};
use paths::target;

pub(super) const MAX_TARGETS: usize = 64;
type Result<T> = std::result::Result<T, &'static str>;

pub(super) fn translate(
    source: &CedarPolicySource,
) -> std::result::Result<FileProtectionPolicy, TranslationRejection> {
    let policies = asc_policy_cedar::pst(source).map_err(|_| reject("INVALID_CEDAR_POLICY"))?;
    let mut targets = Vec::new();
    for (index, policy) in policies.policies.values().enumerate() {
        let body = policy.body();
        let translated = (|| {
            if body.effect != Effect::Forbid {
                return Err("UNSUPPORTED_EFFECT");
            }
            let is_write = |id: &cedar_policy::pst::EntityUID| {
                id.ty.to_string() == "SecCore::Action" && id.eid == "file.write"
            };
            match &body.action {
                ActionConstraint::Eq(id) if is_write(id) => {}
                ActionConstraint::In(ids) if !ids.is_empty() && ids.iter().all(is_write) => {}
                _ => return Err("UNSUPPORTED_ACTION"),
            }
            match &body.resource {
                ResourceConstraint::Any => {}
                ResourceConstraint::Is(ty) if ty.to_string() == "SecCore::File" => {}
                _ => return Err("UNSUPPORTED_RESOURCE_CONSTRAINT"),
            }
            let mut branches = vec![Branch::default()];
            for clause in body.clauses() {
                let (expr, negate) = match clause {
                    Clause::When(expr) => (expr, false),
                    Clause::Unless(expr) => (expr, true),
                };
                branches = and(branches, &expression(expr, negate, 0)?)?;
            }
            for branch in branches {
                if branch.operations & !(DELETE | WRITE) != 0 {
                    return Err("UNSUPPORTED_OPERATION");
                }
                if let Some(mut item) = target(branch)? {
                    // Identical scopes may share operation bits. Different exclusions
                    // on the same root are rejected by Sight's structured protocol.
                    item.exclusions.sort_by(|a, b| a.path.cmp(&b.path));
                    item.exclusions.dedup();
                    merge(&mut targets, item)?;
                }
            }
            Ok(())
        })();
        translated.map_err(|code| reject(&format!("CEDAR_RULE_{index}_{code}")))?;
    }
    if targets.is_empty() {
        return Err(reject("CEDAR_EMPTY_EFFECTIVE_POLICY"));
    }
    targets.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(FileProtectionPolicy {
        version: FILE_PROTECTION_POLICY_VERSION,
        topology_mode: FileProtectionTopologyMode::StaticAtApply,
        targets,
    })
}

fn merge(targets: &mut Vec<FileProtectionTarget>, item: FileProtectionTarget) -> Result<()> {
    if let Some(existing) = targets.iter_mut().find(|target| target.path == item.path) {
        if existing.kind != item.kind || existing.exclusions != item.exclusions {
            return Err("CONFLICTING_TARGET_SCOPES");
        }
        existing.protections.delete |= item.protections.delete;
        existing.protections.ordinary_write |= item.protections.ordinary_write;
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
    }
}

fn reject(code: &str) -> TranslationRejection {
    TranslationRejection { code: code.into() }
}
