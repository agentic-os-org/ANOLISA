//! Project bounded Cedar resource regions into coarse, process-tree-wide requests.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Arc;

use asc_daemon_protocol::authorization::{
    ExecutionPermissions, ExecutionSpec, FileRight, FilesystemDefault, FilesystemPermissions,
    FilesystemRule, LaunchSpec, NetworkMode, NetworkPermissions, OsPermissionSet, PathFact,
    RuleEffect, Validate,
};
use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::pst::{
    ActionConstraint, BinaryOp, Clause, Expr, Literal, PatternElem, PrincipalConstraint,
    ResourceConstraint, UnaryOp, Var,
};
use cedar_policy::{Authorizer, Context, Entities, EntityUid, PolicySet, Request, Schema};
use serde_json::{Value, json};

use super::PermissionInputError as Error;

type Result<T> = std::result::Result<T, Error>;
type Facts<'a> = BTreeMap<&'a str, &'a PathFact>;
const OPERATIONS: [&str; 5] = ["write", "create", "delete", "rename", "setattr"];
const REGION_BUDGET: usize = 16_384;
const PATH_BYTE_LIMIT: usize = 4095;
const CAPTURE_BYTE_BUDGET: usize = 4 * 1024 * 1024;

pub(super) fn bind(
    source: &CedarPolicySource,
    launch: &LaunchSpec,
) -> std::result::Result<ExecutionSpec, crate::PepError> {
    let parsed = parse(source)?;
    // Retain every boundary, including exclusions that lexical rule compaction would discard.
    let paths = parsed.paths.iter().cloned().collect::<Vec<_>>();
    let mut capture = BTreeSet::new();
    let mut capture_bytes = 0;
    for path in &paths {
        for ancestor in Path::new(path).ancestors() {
            let ancestor = ancestor
                .to_str()
                .ok_or_else(|| unsupported("non-UTF-8 parent"))?;
            if !capture.contains(ancestor) {
                if ancestor.len() > CAPTURE_BYTE_BUDGET - capture_bytes {
                    return Err(unsupported("Cedar path capture exceeds its byte budget").into());
                }
                add_path(&mut capture, ancestor)?;
                capture_bytes += ancestor.len();
            }
        }
    }
    let empty = OsPermissionSet {
        version: 1,
        filesystem: FilesystemPermissions {
            default: FilesystemDefault::Deny,
            rules: vec![],
        },
        execution: ExecutionPermissions::default(),
        network: NetworkPermissions {
            mode: NetworkMode::Off,
        },
    };
    let facts =
        crate::prepare_path_facts(launch, &empty, &capture.into_iter().collect::<Vec<_>>())?;
    let index = fact_index(&facts);
    for path in [&launch.resolved_program, &launch.cwd] {
        if canonical(path, &index)? != path {
            return Err(
                unsupported(format!("launch path changed during preparation: {path}")).into(),
            );
        }
    }
    for path in &paths {
        let metadata = std::fs::metadata(path)?;
        // Canonicalization cannot identify other hardlink names. Reject before a
        // redundant exclusion disappears and bypasses the backend's hardlink check.
        if metadata.is_file() && metadata.nlink() > 1 {
            return Err(unsupported(format!("hardlinked permission path: {path}")).into());
        }
    }
    let permissions = project_parsed(source, launch, parsed, Some(&facts))?;
    permissions.validate()?;
    let mut mount_aliases: Vec<String> = Vec::new();
    for path in paths {
        let target = canonical(&path, &index)?;
        if path != target {
            // A writable host-backed ancestor could retarget the link after installation.
            // Bind parents too; checking only the final link misses intermediate directory links.
            for parent in Path::new(&path).ancestors().skip(1) {
                let resolved = canonical(
                    parent
                        .to_str()
                        .ok_or_else(|| unsupported("non-UTF-8 parent"))?,
                    &index,
                )?;
                if rules_allow(&permissions.filesystem.rules, resolved, FileRight::Write) {
                    return Err(unsupported(format!(
                        "permission alias has a writable ancestor: {path}"
                    ))
                    .into());
                }
            }
            let mut covered = false;
            for alias in &mount_aliases {
                if let Some(suffix) = path.strip_prefix(alias.as_str())
                    && suffix.starts_with('/')
                    && format!(
                        "{}{suffix}",
                        canonical(alias, &index)?.trim_end_matches('/')
                    ) == target
                {
                    covered = true;
                    break;
                }
            }
            if !covered && rules_allow(&permissions.filesystem.rules, target, FileRight::Read) {
                mount_aliases.push(path);
            }
        }
    }
    Ok(ExecutionSpec {
        version: 1,
        launch: launch.clone(),
        permissions,
        path_facts: facts,
        mount_aliases,
    })
}

pub(super) fn project(
    source: &CedarPolicySource,
    launch: &LaunchSpec,
    facts: Option<&[PathFact]>,
) -> Result<OsPermissionSet> {
    project_parsed(source, launch, parse(source)?, facts)
}

fn project_parsed(
    source: &CedarPolicySource,
    launch: &LaunchSpec,
    mut parsed: Parsed,
    facts: Option<&[PathFact]>,
) -> Result<OsPermissionSet> {
    if let Some(facts) = facts {
        normalize_policy_paths(&mut parsed, facts)?;
    }
    let policies =
        PolicySet::from_pst(parsed.pst).map_err(|error| unsupported(error.to_string()))?;
    let evaluator = Evaluator {
        policies,
        schema: asc_policy_cedar::schema::load_schema(&source.schema)?,
    };
    if parsed.invocation {
        evaluator.check_invocation(launch)?;
    }
    let paths = parsed.paths;
    let programs = parsed.programs;
    let network = evaluator.query("network.use", "Network", "all", &json!({}), json!({}))?;
    let mut permissions = OsPermissionSet {
        version: 1,
        filesystem: FilesystemPermissions {
            default: FilesystemDefault::Deny,
            rules: vec![],
        },
        execution: ExecutionPermissions::default(),
        network: NetworkPermissions {
            mode: if network.allowed {
                NetworkMode::Full
            } else {
                NetworkMode::Off
            },
        },
    };
    let mut ordered: Vec<_> = paths.iter().collect();
    ordered.sort_by_key(|path| path.len());
    let mut exclusions = Vec::new();
    let mut readable = BTreeSet::new();
    let mut writable = BTreeSet::new();
    for path in ordered {
        let (access, mut forbidden) = evaluator.access(path, &programs)?;
        let (children, child_forbidden) = evaluator.access(&probe(path, &paths)?, &programs)?;
        if access != children && !is_file(path, facts)? {
            return Err(unsupported(format!(
                "point-only or descendants-only directory permissions at {path}"
            )));
        }
        if access.read {
            readable.insert(path.as_str());
        }
        if access.write {
            writable.insert(path.as_str());
        }
        // Descendant-only forbids must retain the backend's directory hardlink checks.
        forbidden.read |= child_forbidden.read;
        forbidden.write |= child_forbidden.write;
        exclusions.push((path.as_str(), forbidden));
        for (right, allowed) in [
            (FileRight::Read, access.read),
            (FileRight::Write, access.write),
        ] {
            if allowed != rules_allow(&permissions.filesystem.rules, path, right) {
                if allowed
                    && permissions.filesystem.rules.iter().any(|rule| {
                        rule.effect == RuleEffect::Deny
                            && contains(&rule.path, path)
                            && (rule.rights.contains(&right)
                                || rule.rights.contains(&FileRight::Read))
                    })
                {
                    return Err(unsupported(format!(
                        "grant below a denied ancestor at {path}"
                    )));
                }
                permissions.filesystem.rules.push(FilesystemRule {
                    effect: if allowed {
                        RuleEffect::Allow
                    } else {
                        RuleEffect::Deny
                    },
                    path: path.clone(),
                    rights: vec![right],
                });
            }
        }
    }
    retain_exclusions(
        &mut permissions.filesystem.rules,
        exclusions,
        &readable,
        &writable,
    );
    Ok(permissions)
}

fn normalize_policy_paths(parsed: &mut Parsed, facts: &[PathFact]) -> Result<()> {
    let index = fact_index(facts);
    for (_, policy) in &mut parsed.pst.policies {
        let body = policy.body().clone();
        if !file_actions(&body.action) {
            continue;
        }
        let mut clauses = body.clauses().clone();
        for clause in &mut clauses {
            let (Clause::When(expr) | Clause::Unless(expr)) = clause;
            normalize(Arc::make_mut(expr), &index)?;
        }
        let body = body
            .try_with_clauses(clauses)
            .map_err(|error| unsupported(error.to_string()))?;
        *policy = cedar_policy::pst::StaticPolicy::try_from(body)
            .map_err(|error| unsupported(error.to_string()))?;
    }
    parsed.paths = parsed
        .paths
        .iter()
        .map(|path| canonical(path, &index).map(str::to_owned))
        .collect::<Result<_>>()?;
    Ok(())
}

fn retain_exclusions(
    rules: &mut Vec<FilesystemRule>,
    exclusions: Vec<(&str, Access)>,
    readable: &BTreeSet<&str>,
    writable: &BTreeSet<&str>,
) {
    // Redundant denies still carry the backend's hardlink checks. A complement
    // containing a permitted subtree cannot be installed as an ancestor deny.
    for (path, forbidden) in exclusions {
        for (right, denied, grants) in [
            (FileRight::Read, forbidden.read, readable),
            (FileRight::Write, forbidden.write, writable),
        ] {
            let prefix = format!("{}/", path.trim_end_matches('/'));
            if denied
                && !grants.contains(path)
                && !grants
                    .range(prefix.as_str()..)
                    .next()
                    .is_some_and(|child| child.starts_with(&prefix))
                && !grants.is_empty()
                && !rules.iter().any(|rule| {
                    rule.effect == RuleEffect::Deny
                        && contains(&rule.path, path)
                        && (rule.rights.contains(&right) || rule.rights.contains(&FileRight::Read))
                })
            {
                rules.push(FilesystemRule {
                    effect: RuleEffect::Deny,
                    path: path.into(),
                    rights: vec![right],
                });
            }
        }
    }
}

struct Parsed {
    pst: cedar_policy::pst::PolicySet,
    paths: BTreeSet<String>,
    programs: BTreeSet<String>,
    invocation: bool,
}

fn file_actions(action: &ActionConstraint) -> bool {
    match action {
        ActionConstraint::Any => true,
        ActionConstraint::Eq(id) => matches!(id.eid.as_str(), "file.read" | "file.write"),
        ActionConstraint::In(ids) => ids
            .iter()
            .any(|id| matches!(id.eid.as_str(), "file.read" | "file.write")),
    }
}

fn parse(source: &CedarPolicySource) -> Result<Parsed> {
    let pst = asc_policy_cedar::pst(source)?;
    let mut paths = BTreeSet::from(["/".to_owned()]);
    let mut programs = BTreeSet::new();
    if pst.policies.len() > 1024 {
        return Err(unsupported("more than 1024 Cedar policies"));
    }
    let mut invocation = false;
    let mut terms = pst.policies.len();
    for policy in pst.policies.values() {
        let body = policy.body();
        match &body.principal {
            PrincipalConstraint::Any => {}
            PrincipalConstraint::Is(ty) if ty.to_string() == "SecCore::Subject" => {}
            _ => return Err(unsupported("subject-specific permission inputs")),
        }
        if body.annotations.keys().any(|name| name.as_str() != "id") {
            return Err(unsupported("permission inputs cannot discharge advice"));
        }
        let actions = match &body.action {
            ActionConstraint::Any => vec![
                "file.read",
                "file.write",
                "file.exec",
                "process.spawn",
                "network.use",
            ],
            ActionConstraint::Eq(id) => vec![id.eid.as_str()],
            ActionConstraint::In(ids) => ids.iter().map(|id| id.eid.as_str()).collect(),
        };
        invocation |= actions
            .iter()
            .any(|a| matches!(*a, "file.exec" | "process.spawn"));
        if actions
            .iter()
            .any(|a| matches!(*a, "file.read" | "file.write"))
        {
            match &body.resource {
                ResourceConstraint::Any => {}
                ResourceConstraint::Is(ty) if ty.to_string() == "SecCore::File" => {}
                _ => {
                    return Err(unsupported(
                        "file resources must use literal path predicates",
                    ));
                }
            }
            for clause in body.clauses() {
                let (Clause::When(expr) | Clause::Unless(expr)) = clause;
                terms = terms.saturating_add(inspect(expr, &mut paths, &mut programs, 0)?);
            }
        }
    }
    let policies =
        PolicySet::from_pst(pst.clone()).map_err(|error| unsupported(error.to_string()))?;
    for policy in policies.policies() {
        if references_principal(
            &policy
                .to_json()
                .map_err(|error| unsupported(error.to_string()))?,
        ) {
            return Err(unsupported(
                "subject conditions cannot become permission requests",
            ));
        }
    }
    // Reserve the extra program class before generating any representatives or evaluating Cedar.
    if paths
        .len()
        .saturating_mul(programs.len() + 1)
        .saturating_mul(terms)
        > REGION_BUDGET
    {
        return Err(unsupported(
            "Cedar permission projection exceeds its region budget",
        ));
    }
    programs.insert(probe("/", &programs)?);
    Ok(Parsed {
        pst,
        paths,
        programs,
        invocation,
    })
}

fn is_file(path: &str, facts: Option<&[PathFact]>) -> Result<bool> {
    if let Some(facts) = facts {
        // A point grant must remain a file in the identities pinned for the backend.
        return facts
            .iter()
            .find(|fact| fact.path == path)
            .map(|fact| !fact.is_directory)
            .ok_or_else(|| unsupported(format!("point permission lacks a pinned object: {path}")));
    }
    Ok(std::fs::metadata(path)?.is_file())
}

fn fact_index(facts: &[PathFact]) -> Facts<'_> {
    facts
        .iter()
        .map(|fact| (fact.path.as_str(), fact))
        .collect()
}

fn canonical<'a>(path: &str, facts: &Facts<'a>) -> Result<&'a str> {
    facts
        .get(path)
        .map(|fact| fact.canonical_path.as_str())
        .ok_or_else(|| unsupported(format!("permission path lacks a pinned object: {path}")))
}

// Substitute only the already validated file-path literals in Cedar's own syntax tree.
// Boolean logic, permit/forbid precedence and conditions stay with the official evaluator.
fn normalize(expr: &mut Expr, facts: &Facts<'_>) -> Result<()> {
    match expr {
        Expr::UnaryOp { expr, .. } => normalize(Arc::make_mut(expr), facts)?,
        Expr::BinaryOp {
            op: BinaryOp::And | BinaryOp::Or,
            left,
            right,
        } => {
            normalize(Arc::make_mut(left), facts)?;
            normalize(Arc::make_mut(right), facts)?;
        }
        Expr::BinaryOp {
            op: BinaryOp::Eq | BinaryOp::NotEq,
            left,
            right,
        } => {
            if path_field(left) {
                normalize_literal(Arc::make_mut(right), facts)?;
            } else if path_field(right) {
                normalize_literal(Arc::make_mut(left), facts)?;
            }
        }
        Expr::BinaryOp {
            op: BinaryOp::Contains,
            left,
            right,
        } if path_field(right) => {
            if let Expr::Set(values) = Arc::make_mut(left) {
                for value in values {
                    normalize_literal(Arc::make_mut(value), facts)?;
                }
            }
        }
        Expr::Like { expr, pattern } if path_field(expr) => {
            let wildcard = matches!(pattern.last(), Some(PatternElem::Wildcard));
            let literal: String = pattern
                .iter()
                .filter_map(|part| match part {
                    PatternElem::Char(ch) => Some(*ch),
                    PatternElem::Wildcard => None,
                })
                .collect();
            let path = if wildcard {
                literal.trim_end_matches('/')
            } else {
                &literal
            };
            let target = canonical(if path.is_empty() { "/" } else { path }, facts)?;
            *pattern = target.chars().map(PatternElem::Char).collect();
            if wildcard {
                if target != "/" {
                    pattern.push(PatternElem::Char('/'));
                }
                pattern.push(PatternElem::Wildcard);
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_literal(expr: &mut Expr, facts: &Facts<'_>) -> Result<()> {
    if let Expr::Literal(Literal::String(value)) = expr {
        *value = canonical(value, facts)?.into();
    }
    Ok(())
}

fn inspect(
    expr: &Expr,
    paths: &mut BTreeSet<String>,
    programs: &mut BTreeSet<String>,
    depth: usize,
) -> Result<usize> {
    if depth > 64 {
        return Err(unsupported("Cedar permission condition exceeds depth 64"));
    }
    match expr {
        Expr::Literal(Literal::Bool(_)) => Ok(1),
        Expr::UnaryOp {
            op: UnaryOp::Not,
            expr,
        } => Ok(1 + inspect(expr, paths, programs, depth + 1)?),
        Expr::BinaryOp {
            op: BinaryOp::And | BinaryOp::Or,
            left,
            right,
        } => Ok(1
            + inspect(left, paths, programs, depth + 1)?
            + inspect(right, paths, programs, depth + 1)?),
        Expr::BinaryOp {
            op: BinaryOp::Eq | BinaryOp::NotEq,
            left,
            right,
        } => {
            if matches!(left.as_ref(), Expr::Literal(_)) {
                inspect_equality(right, left, paths, programs)?;
            } else {
                inspect_equality(left, right, paths, programs)?;
            }
            Ok(1)
        }
        Expr::BinaryOp {
            op: BinaryOp::Contains,
            left,
            right,
        } => {
            let Expr::Set(values) = left.as_ref() else {
                return Err(unsupported("nonliteral permission set"));
            };
            for value in values {
                inspect_equality(right, value, paths, programs)?;
            }
            Ok(1 + values.len())
        }
        Expr::Like { expr, pattern } if path_field(expr) => {
            let mut prefix = String::new();
            for (index, part) in pattern.iter().enumerate() {
                match part {
                    PatternElem::Char(ch) => prefix.push(*ch),
                    PatternElem::Wildcard
                        if index + 1 == pattern.len() && prefix.ends_with('/') =>
                    {
                        if prefix != "/" {
                            prefix.pop();
                        }
                    }
                    PatternElem::Wildcard => {
                        return Err(unsupported(
                            "only literal paths and trailing directory /* patterns are supported",
                        ));
                    }
                }
            }
            add_path(paths, &prefix)?;
            Ok(1)
        }
        _ => Err(unsupported(
            "unsupported file condition; expected path, program or write-operation predicates",
        )),
    }
}

fn inspect_equality(
    field: &Expr,
    literal: &Expr,
    paths: &mut BTreeSet<String>,
    programs: &mut BTreeSet<String>,
) -> Result<()> {
    if let Expr::Literal(Literal::String(value)) = literal {
        if path_field(field) {
            return add_path(paths, value);
        }
        if input_field(field, "program") {
            return add_path(programs, value);
        }
    }
    if let Expr::Literal(Literal::EntityUID(id)) = literal
        && input_field(field, "operation")
        && id.ty.to_string() == "SecCore::WriteOperation"
        && OPERATIONS.contains(&id.eid.as_str())
    {
        return Ok(());
    }
    Err(unsupported("unsupported file predicate or nonliteral path"))
}

fn path_field(expr: &Expr) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == "path" && matches!(expr.as_ref(), Expr::Var(Var::Resource)))
}

fn input_field(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == name &&
        matches!(expr.as_ref(), Expr::GetAttr { expr, attr } if attr == "input" && matches!(expr.as_ref(), Expr::Var(Var::Context))))
}

fn add_path(paths: &mut BTreeSet<String>, path: &str) -> Result<()> {
    if path.len() > PATH_BYTE_LIMIT {
        return Err(unsupported("Cedar path exceeds 4095 bytes"));
    }
    asc_daemon_protocol::authorization::validate_os_path(path)?;
    if paths.contains(path) {
        return Ok(());
    }
    if paths.len() >= REGION_BUDGET {
        return Err(unsupported(
            "Cedar permission projection exceeds its region budget",
        ));
    }
    paths.insert(path.to_owned());
    Ok(())
}

fn references_principal(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            fields.get("Var").is_some_and(|value| value == "principal")
                || fields.values().any(references_principal)
        }
        Value::Array(items) => items.iter().any(references_principal),
        _ => false,
    }
}

struct Evaluator {
    policies: PolicySet,
    schema: Schema,
}
struct Outcome {
    allowed: bool,
    forbidden: bool,
}
#[derive(PartialEq, Eq)]
struct Access {
    read: bool,
    write: bool,
}

impl Evaluator {
    fn query(
        &self,
        action: &str,
        kind: &str,
        id: &str,
        attributes: &Value,
        context: Value,
    ) -> Result<Outcome> {
        let action = uid("Action", action)?;
        let principal = uid("Subject", "sandbox-input")?;
        let resource = uid(kind, id)?;
        let context = Context::from_json_value(context, Some((&self.schema, &action)))
            .map_err(|error| unsupported(error.to_string()))?;
        let entities = Entities::from_json_value(json!([
            {"uid": {"type": "SecCore::Subject", "id": "sandbox-input"}, "attrs": {}, "parents": []},
            {"uid": {"type": format!("SecCore::{kind}"), "id": id}, "attrs": attributes, "parents": []}
        ]), Some(&self.schema)).map_err(|error| unsupported(error.to_string()))?;
        let request = Request::new(principal, action, resource, context, Some(&self.schema))
            .map_err(|error| unsupported(error.to_string()))?;
        let response = Authorizer::new().is_authorized(&request, &self.policies, &entities);
        if let Some(error) = response.diagnostics().errors().next() {
            return Err(unsupported(format!(
                "Cedar permission evaluation failed: {error}"
            )));
        }
        Ok(Outcome {
            allowed: response.decision() == cedar_policy::Decision::Allow,
            forbidden: response.diagnostics().reason().any(|id| {
                self.policies
                    .policy(id)
                    .is_some_and(|policy| policy.effect() == cedar_policy::Effect::Forbid)
            }),
        })
    }

    fn access(&self, path: &str, programs: &BTreeSet<String>) -> Result<(Access, Access)> {
        let mut access = Access {
            read: false,
            write: false,
        };
        let mut forbidden = Access {
            read: false,
            write: false,
        };
        // shortcut: program-specific file grants become process-tree-wide until per-process enforcement exists.
        for program in programs {
            let read = self.query(
                "file.read",
                "File",
                path,
                &json!({"path": path}),
                json!({"input": {"program": program}}),
            )?;
            forbidden.read |= read.forbidden;
            forbidden.write |= read.forbidden;
            if read.forbidden {
                continue;
            }
            access.read |= read.allowed;
            // shortcut: any permitted mutation requests write, including deletion, until finer OS rights exist.
            let mut writes_forbidden = true;
            for operation in OPERATIONS {
                let write = self.query("file.write", "File", path, &json!({"path": path}), json!({"input": {
                    "program": program, "operation": {"__entity": {"type": "SecCore::WriteOperation", "id": operation}}
                }}))?;
                writes_forbidden &= write.forbidden;
                if write.allowed {
                    access.write = true;
                    access.read = true;
                    break;
                }
            }
            forbidden.write |= writes_forbidden;
        }
        Ok((access, forbidden))
    }

    fn check_invocation(&self, launch: &LaunchSpec) -> Result<()> {
        let args: Vec<_> = launch
            .args
            .iter()
            .enumerate()
            .map(|(index, value)| json!({"index": index, "value": value}))
            .collect();
        let context = json!({"input": {"cwd": launch.cwd, "arg_count": args.len(), "args": args}});
        let executable = self.query(
            "file.exec",
            "File",
            &launch.resolved_program,
            &json!({"path": launch.resolved_program}),
            context.clone(),
        )?;
        let command = self.query(
            "process.spawn",
            "Process",
            &launch.program,
            &json!({"program": launch.program, "resolved_program": launch.resolved_program}),
            context,
        )?;
        if executable.forbidden || command.forbidden || !(executable.allowed || command.allowed) {
            return Err(unsupported(
                "Cedar input does not permit the initial invocation",
            ));
        }
        Ok(())
    }
}

fn uid(kind: &str, id: &str) -> Result<EntityUid> {
    format!("SecCore::{kind}::{}", serde_json::to_string(id)?)
        .parse()
        .map_err(|error: cedar_policy::ParseErrors| unsupported(error.to_string()))
}

fn contains(parent: &str, child: &str) -> bool {
    child == parent
        || parent == "/"
        || child
            .strip_prefix(parent)
            .is_some_and(|tail| tail.starts_with('/'))
}

fn probe(parent: &str, boundaries: &BTreeSet<String>) -> Result<String> {
    // Each rejected candidate consumes a distinct boundary. Ordered prefix lookup
    // replaces the former full scan, bounding N collisions to O(N log N) work.
    for index in 0..=boundaries.len() {
        let candidate = format!(
            "{}/__asc_permission_input_{index}__",
            parent.trim_end_matches('/')
        );
        let prefix = format!("{candidate}/");
        if !boundaries.contains(&candidate)
            && !boundaries
                .range(prefix.clone()..)
                .next()
                .is_some_and(|path| path.starts_with(&prefix))
        {
            return Ok(candidate);
        }
    }
    Err(unsupported("cannot select a permission representative"))
}

fn rules_allow(rules: &[FilesystemRule], path: &str, right: FileRight) -> bool {
    let mut allowed = false;
    for rule in rules.iter().filter(|rule| contains(&rule.path, path)) {
        if rule.effect == RuleEffect::Deny
            && (rule.rights.contains(&right) || rule.rights.contains(&FileRight::Read))
        {
            return false;
        }
        if rule.effect == RuleEffect::Allow
            && (rule.rights.contains(&right)
                || right == FileRight::Read && rule.rights.contains(&FileRight::Write))
        {
            allowed = true;
        }
    }
    allowed
}

fn unsupported(message: impl Into<String>) -> Error {
    Error::Unsupported(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn representative_handles_dense_collisions_and_component_boundaries() {
        let mut paths: BTreeSet<_> = (0..8_000)
            .map(|index| format!("/__asc_permission_input_{index}__/child"))
            .collect();
        // A neighboring name must not conceal the descendant in ordered prefix lookup.
        paths.insert("/__asc_permission_input_0__-neighbor".into());
        paths.insert("/__asc_permission_input_8000__-neighbor".into());
        assert_eq!(
            probe("/", &paths).unwrap(),
            "/__asc_permission_input_8000__"
        );
        assert_eq!(
            probe("/other", &paths).unwrap(),
            "/other/__asc_permission_input_0__"
        );
    }

    #[test]
    fn collection_budget_applies_to_paths_and_programs() {
        let mut paths = BTreeSet::new();
        for index in 0..REGION_BUDGET {
            add_path(&mut paths, &format!("/p{index}")).unwrap();
        }
        assert!(add_path(&mut paths, "/overflow").is_err());
        assert_eq!(paths.len(), REGION_BUDGET);
        add_path(&mut paths, "/p0").unwrap();
        assert_eq!(paths.len(), REGION_BUDGET);
    }
}
