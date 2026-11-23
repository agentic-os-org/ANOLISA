//! Summarizes validated candidate Cedar for display; it grants nothing and adds no rules.

use cedar_policy::PolicySet;
use serde_json::Value;
use std::str::FromStr as _;

use crate::policy_validation::pattern_scope;

/// Action category of a summarized statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionCategory {
    /// `file.read`.
    Read,
    /// `file.write`.
    Write,
    /// `file.exec`.
    Exec,
    /// `process.spawn`.
    Spawn,
    /// `network.use`.
    Network,
}

impl ActionCategory {
    /// Short stable label used by text renderers.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Exec => "exec",
            Self::Spawn => "spawn",
            Self::Network => "network",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "file.read" => Self::Read,
            "file.write" => Self::Write,
            "file.exec" => Self::Exec,
            "process.spawn" => Self::Spawn,
            "network.use" => Self::Network,
            _ => return None,
        })
    }

    const fn resource_type(self) -> &'static str {
        match self {
            Self::Read | Self::Write | Self::Exec => "SecCore::File",
            Self::Spawn => "SecCore::Process",
            Self::Network => "SecCore::Network",
        }
    }
}

/// Target scope of one statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathScope {
    /// One canonical absolute path.
    Exact(String),
    /// Every path under this canonical absolute directory, as written with `/*`.
    Subtree(String),
}

impl PathScope {
    /// Display form; subtree scopes keep the wildcard that produced them.
    pub fn display(&self) -> String {
        match self {
            Self::Exact(path) => path.clone(),
            Self::Subtree(prefix) if prefix == "/" => "/*".to_owned(),
            Self::Subtree(prefix) => format!("{prefix}/*"),
        }
    }
}

/// One permit or forbid reduced to the fields a reader compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Permission {
    /// Action category.
    pub action: ActionCategory,
    /// Target path or subtree; `None` when the statement leaves the path unconstrained.
    pub path: Option<PathScope>,
    /// Requesting program from `context.input.program`.
    pub program: Option<String>,
    /// Working directory from `context.input.cwd`.
    pub cwd: Option<String>,
    /// Write operation from `context.input.operation`.
    pub operation: Option<String>,
}

/// Permits and forbids of a validated candidate, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySummary {
    /// Statements that grant.
    pub permits: Vec<Permission>,
    /// Statements that deny.
    pub forbids: Vec<Permission>,
}

/// Summarizes validated candidate Cedar as comparable permissions.
///
/// Returns `None` when the text does not parse or uses a construct the summary cannot
/// represent, so callers show the exact Cedar instead of a partial view. The result is a
/// reading aid: it is neither approval nor an authorization decision.
pub fn summarize(text: &str) -> Option<PolicySummary> {
    let policies = PolicySet::from_str(text).ok()?;
    let mut summary = PolicySummary {
        permits: Vec::new(),
        forbids: Vec::new(),
    };
    for policy in policies.policies() {
        let statement = policy.to_json().ok()?;
        let found = permission(&statement)?;
        match statement["effect"].as_str()? {
            "permit" => summary.permits.push(found),
            "forbid" => summary.forbids.push(found),
            _ => return None,
        }
    }
    Some(summary)
}

/// Summarizes one statement; every `when` condition must describe the same scope, so a
/// condition the conjunction rejects keeps the exact Cedar.
fn permission(statement: &Value) -> Option<Permission> {
    let principal = statement["principal"].as_object()?;
    let principal_supported = principal["op"] == "All"
        || (principal["op"] == "is" && principal["entity_type"] == "SecCore::Subject");
    if !principal_supported {
        return None;
    }
    let action_scope = statement["action"].as_object()?;
    if action_scope["op"] != "==" || action_scope["entity"]["type"] != "SecCore::Action" {
        return None;
    }
    let action = ActionCategory::from_id(action_scope["entity"]["id"].as_str()?)?;
    let resource = statement["resource"].as_object()?;
    let resource_matches = resource["op"] == "All"
        || (resource["op"] == "is" && resource["entity_type"] == action.resource_type())
        // `resource == SecCore::Network::"all"` names the only enum value, so it states the
        // same whole-network scope as an unconstrained resource.
        || (action == ActionCategory::Network
            && resource["entity"]["type"] == "SecCore::Network"
            && resource["entity"]["id"] == "all");
    if !resource_matches {
        return None;
    }
    let mut constraints = Constraints::default();
    for condition in statement["conditions"].as_array()? {
        if condition["kind"] != "when" {
            return None;
        }
        conjunction(&condition["body"], &mut constraints)?;
    }
    Some(Permission {
        action,
        path: constraints.path,
        program: constraints.program,
        cwd: constraints.cwd,
        operation: constraints.operation,
    })
}

#[derive(Default)]
struct Constraints {
    path: Option<PathScope>,
    program: Option<String>,
    cwd: Option<String>,
    operation: Option<String>,
}

/// Accepts only `&&` chains of single-attribute comparisons so contradictions and disjunctions
/// cannot be summarized into something broader than the policy states. The single exception is
/// `path == X || path like "X/*"`, which is exactly the subtree rooted at X.
fn conjunction(body: &Value, constraints: &mut Constraints) -> Option<()> {
    if let Some(and) = body.get("&&") {
        conjunction(&and["left"], constraints)?;
        return conjunction(&and["right"], constraints);
    }
    if let Some(or) = body.get("||") {
        return subtree_or(&or["left"], &or["right"], constraints);
    }
    if let Some(equal) = body.get("==") {
        return equality(&equal["left"], &equal["right"], constraints);
    }
    if let Some(like) = body.get("like") {
        if attribute(&like["left"]) != Some(("resource", "path")) {
            return None;
        }
        let (prefix, wildcard) = pattern_scope(&like["pattern"]).ok()?;
        let scope = if wildcard {
            PathScope::Subtree(prefix)
        } else {
            PathScope::Exact(prefix)
        };
        return set(&mut constraints.path, scope);
    }
    None
}

/// Folds `path == X || path like "X/*"` into the subtree rooted at X and rejects every other
/// disjunction, because any other union cannot be stated as one scope.
fn subtree_or(left: &Value, right: &Value, constraints: &mut Constraints) -> Option<()> {
    let (Some(equal), Some(like)) = (left.get("=="), right.get("like")) else {
        return None;
    };
    let literal = equality_path(equal)?;
    if attribute(&like["left"]) != Some(("resource", "path")) {
        return None;
    }
    let (prefix, wildcard) = pattern_scope(&like["pattern"]).ok()?;
    if !wildcard || prefix != literal {
        return None;
    }
    set(&mut constraints.path, PathScope::Subtree(prefix))
}

fn equality_path(equal: &Value) -> Option<String> {
    let ((receiver, name), literal) = match attribute(&equal["left"]) {
        Some(attribute) => (attribute, &equal["right"]),
        None => (attribute(&equal["right"])?, &equal["left"]),
    };
    if (receiver, name) != ("resource", "path") {
        return None;
    }
    Some(literal["Value"].as_str()?.to_owned())
}

fn equality(left: &Value, right: &Value, constraints: &mut Constraints) -> Option<()> {
    // `resource == SecCore::Network::"all"` names the only enum value, so it adds no scope to
    // the statement; `resource.path` comparisons carry the addresses this summary states.
    if whole_network(left, right) || whole_network(right, left) {
        return Some(());
    }
    let ((receiver, name), literal) = match attribute(left) {
        Some(attribute) => (attribute, right),
        None => (attribute(right)?, left),
    };
    match (receiver, name) {
        ("resource", "path") => set(
            &mut constraints.path,
            PathScope::Exact(literal["Value"].as_str()?.to_owned()),
        ),
        ("context.input", "program") => set(
            &mut constraints.program,
            literal["Value"].as_str()?.to_owned(),
        ),
        ("context.input", "cwd") => {
            set(&mut constraints.cwd, literal["Value"].as_str()?.to_owned())
        }
        ("context.input", "operation") => {
            let entity = &literal["Value"]["__entity"];
            if entity["type"] != "SecCore::WriteOperation" {
                return None;
            }
            set(
                &mut constraints.operation,
                entity["id"].as_str()?.to_owned(),
            )
        }
        _ => None,
    }
}

/// Accepts `resource == SecCore::Network::"all"`, the only enum value, which adds no scope.
fn whole_network(subject: &Value, object: &Value) -> bool {
    subject["Var"] == "resource"
        && object["Value"]["__entity"]["type"] == "SecCore::Network"
        && object["Value"]["__entity"]["id"] == "all"
}

/// Returns the receiver and attribute of a canonical projection such as `resource.path`.
fn attribute(value: &Value) -> Option<(&'static str, &str)> {
    let projection = value.get(".")?;
    let name = projection["attr"].as_str()?;
    if projection["left"] == serde_json::json!({"Var": "resource"}) && name == "path" {
        return Some(("resource", "path"));
    }
    let context_input = serde_json::json!({".": {"left": {"Var": "context"}, "attr": "input"}});
    if projection["left"] == context_input {
        return Some(("context.input", name));
    }
    None
}

/// Rejects repeated constraints on one attribute unless they agree; two different paths in one
/// statement would otherwise be shown as a single narrower scope.
fn set<T: PartialEq>(slot: &mut Option<T>, value: T) -> Option<()> {
    match slot {
        Some(existing) if *existing != value => None,
        _ => {
            *slot = Some(value);
            Some(())
        }
    }
}
