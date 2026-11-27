//! Validates model-generated Cedar against the shared schema and task or command constraints.

use cedar_policy::{
    ActionConstraint, PolicySet, PrincipalConstraint, Schema, ValidationMode, ValidationWarning,
    Validator,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::str::FromStr as _;
use thiserror::Error;

/// Single embedded source of the shared policy type contract.
pub const CEDAR_SCHEMA: &str = asc_policy_cedar::schema::SCHEMA_TEXT;
/// Policy text budget.
pub const MAX_POLICY_BYTES: usize = 256 * 1024;
pub(crate) const MAX_POLICIES: usize = 1024;
/// Requirement key a statement without an `@id` shares with every other unlabelled statement.
const NO_REQUIREMENT: &str = "no @id";
/// The one action that covers its whole resource space and therefore needs no path.
const NETWORK_ACTION: &str = "network.use";

/// Validated Cedar text; validation is neither approval nor a completeness proof.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Always cedar.
    pub language: String,
    /// Original validated Cedar, possibly empty.
    pub text: String,
    /// Digest of the current schema; not a signature.
    pub schema_sha256: String,
}

/// Cedar validation errors carry no model text or task paths.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// The policy, the policy count or the final candidate exceeds its budget.
    #[error("candidate exceeds its size limit")]
    TooLarge,
    /// Candidate or Cedar AST encoding failed.
    #[error("candidate encoding failed")]
    Encoding,
    /// The official Cedar parser cannot parse the policy.
    #[error("generated candidate syntax is invalid")]
    Syntax,
    /// Official Strict type validation did not pass.
    #[error("generated candidate failed Cedar validation")]
    Validation,
    /// A path is not canonical and absolute, or uses an expression that cannot be checked.
    #[error("policy paths must use canonical absolute literals or directory subtree patterns")]
    InvalidPath,
    /// An equality literal ends with `/*`, which only a `like` pattern may express.
    #[error("equality compares a concrete path; write a directory subtree as like \"<dir>/*\"")]
    WildcardLiteral,
    /// Unbound templates, self-declared identities or contract-less annotations cannot become a
    /// candidate.
    #[error("policy contains unsupported templates, identity references or annotations")]
    UnsupportedPolicy,
    /// Task rules cannot be published together with mixed principal headers.
    #[error(
        "use consistent principal headers across all statements; launch approval requires principal is SecCore::Subject on every statement"
    )]
    PrincipalHeaders,
    /// A confirmation annotation is outside the one contract a task permit may carry.
    #[error(
        "advice must be a single escalation with a message, on a permit whose paths the policy \
         already grants"
    )]
    Advice,
}

/// Parses the model policy, checks path expressions and runs Strict validation once.
///
/// Absolute paths from the task may point into any directory. Only expressions that can be
/// verified precisely are checked; task completeness is not inferred, OS objects are not bound
/// and advice or approvals are not discharged.
///
/// A command proposal grants one launch and has its own approval flow, so a confirmation is not
/// admitted here; only a task draft carries that one annotation.
///
/// # Errors
/// Rejects invalid Cedar, non-canonical paths, unsupported policy features and oversized artifacts.
pub fn prepare_policy(model_text: &str) -> Result<Artifact, PolicyError> {
    prepare(model_text, &parse(model_text)?, false)
}

/// Renders a name-scoped launch rule for a recognized literal utility name.
pub(crate) fn spawn_rule(program: &str) -> String {
    format!(
        r#"permit(principal, action == SecCore::Action::"process.spawn", resource is SecCore::Process) when {{ resource.program == "{program}" }};"#
    )
}

/// Supplies a missing launch request only when neither launch action is present.
pub(crate) fn prepare_command_policy(
    model_text: &str,
    path_program: Option<&str>,
) -> Result<Artifact, PolicyError> {
    let policies = parse(model_text)?;
    if let Some(program) = path_program
        && policies.policies().next().is_some()
        && policies.policies().all(|policy| {
            matches!(
                policy.action_constraint(),
                ActionConstraint::Eq(action)
                    if !matches!(action.id().unescaped(), "process.spawn" | "file.exec")
            )
        })
    {
        return prepare_policy(&format!("{model_text}\n{}", spawn_rule(program)));
    }
    prepare(model_text, &policies, false)
}

/// Admits a task draft: the one place a `permit` may ask a human before it takes effect.
///
/// # Errors
/// Rejects invalid Cedar, paths, unsupported policy features and malformed confirmation advice.
pub fn prepare_task_policy(model_text: &str) -> Result<Artifact, PolicyError> {
    prepare(model_text, &parse(model_text)?, true)
}

/// Shared admission; `confirmations` decides whether an escalation advice is within contract.
fn prepare(
    model_text: &str,
    policies: &PolicySet,
    confirmations: bool,
) -> Result<Artifact, PolicyError> {
    for policy in policies.policies() {
        for (name, value) in policy.annotations() {
            match name {
                "id" => {}
                // The one annotation with an execution contract: it turns a permission the policy
                // already grants into one that asks first. It never adds a permission, and the
                // runtime protector rejects it on a forbid.
                "advice" if confirmations && policy.effect() == cedar_policy::Effect::Permit => {
                    escalation_message(value)?;
                }
                _ => return Err(PolicyError::UnsupportedPolicy),
            }
        }
        match policy.principal_constraint() {
            PrincipalConstraint::Any => {}
            PrincipalConstraint::Is(name) if name.to_string() == "SecCore::Subject" => {}
            _ => return Err(PolicyError::UnsupportedPolicy),
        }
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        check_expression(&est["conditions"], 0)?;
        // File/Subject UIDs are neither task paths nor trusted instance identities.
        check_entity_references(&est)?;
    }
    if confirmations {
        let eligible = asc_policy_cedar::request_policy_headers(policies)
            .map_err(|_| PolicyError::PrincipalHeaders)?;
        if !eligible
            && policies
                .policies()
                .any(|policy| policy.annotation("advice").is_some())
        {
            return Err(PolicyError::PrincipalHeaders);
        }
    }
    let (schema, _) =
        Schema::from_cedarschema_str(CEDAR_SCHEMA).map_err(|_| PolicyError::Validation)?;
    let validation = Validator::new(schema).validate(policies, ValidationMode::Strict);
    // Policy admission rejects an action that cannot apply to the resource type; the SDK reports
    // it as a warning, so `validation_passed` alone would emit a forbid that restricts nothing.
    let inapplicable_action = validation
        .validation_warnings()
        .any(|warning| matches!(warning, ValidationWarning::InvalidActionApplication(_)));
    if !validation.validation_passed() || inapplicable_action {
        return Err(PolicyError::Validation);
    }
    Ok(Artifact {
        language: "cedar".into(),
        text: model_text.to_owned(),
        schema_sha256: format!("sha256:{:x}", Sha256::digest(CEDAR_SCHEMA.as_bytes())),
    })
}

/// Reviews the complete candidate against the same policy-level checks as its consumers.
pub(crate) struct ConsumerReview {
    pub(crate) launch: Vec<String>,
    pub(crate) protection: Vec<String>,
    pub(crate) coverage: Vec<String>,
}

pub(crate) fn consumer_review(text: &str) -> Result<ConsumerReview, PolicyError> {
    let policies = parse(text)?;
    let eligible = asc_policy_cedar::request_policy_headers(&policies)
        .map_err(|_| PolicyError::PrincipalHeaders)?;
    let mut launch = Vec::new();
    if eligible && let Err(error) = asc_pdp_runtime::validate_os_policy_regions(&policies) {
        launch.push(error.to_string());
    }

    let syntax = policies
        .clone()
        .try_into_pst()
        .map_err(|_| PolicyError::Encoding)?;
    let files = asc_policy_cedar::project_pst(&syntax, asc_policy_cedar::PolicyDomain::File);
    let mut protection = Vec::new();
    if let Some(files) = &files
        && let Err(error) = asc_policy_translator_agentsight::validate_file_protection(files)
    {
        protection.push(error.code);
    }

    let sandbox_input = policies
        .policies()
        .all(|rule| rule.annotation("advice").is_none());
    let mut statements = Vec::new();
    for (index, policy) in policies.policies().enumerate() {
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        let mut consumers = Vec::new();
        let network = single_action(&est) == Some(NETWORK_ACTION);
        if sandbox_input {
            consumers.push(if network {
                "sandbox input (network mode)"
            } else {
                "sandbox input (projection required)"
            });
        }
        if eligible && launch.is_empty() {
            consumers.push(if policy.annotation("advice").is_some() {
                "PDP (launch approval)"
            } else {
                "PDP"
            });
        }
        if protection.is_empty()
            && files.as_ref().is_some_and(|view| {
                view.policies
                    .keys()
                    .any(|id| id.to_string() == policy.id().as_ref())
            })
        {
            consumers.push("AgentSight file protection");
        }
        if consumers.is_empty() {
            consumers.push("no compatible consumer identified");
        }
        statements.push((requirement_key(policy), consumers.join(", "), index + 1));
    }
    Ok(ConsumerReview {
        launch,
        protection,
        coverage: coverage_rows(&statements),
    })
}

/// Returns OS request-policy projection errors for this candidate.
///
/// # Errors
/// Rejects invalid policy sets.
pub fn launch_view_gaps(text: &str) -> Result<Vec<String>, PolicyError> {
    Ok(consumer_review(text)?.launch)
}

/// Returns the `AgentSight` file-view translation rejection for this candidate.
///
/// # Errors
/// Rejects invalid policy sets.
pub fn runtime_view_gaps(text: &str) -> Result<Vec<String>, PolicyError> {
    Ok(consumer_review(text)?.protection)
}

/// Lists potential consumers grouped by task requirement.
///
/// # Errors
/// Rejects invalid policy sets.
pub fn requirement_coverage(text: &str) -> Result<Vec<String>, PolicyError> {
    Ok(consumer_review(text)?.coverage)
}

fn coverage_rows(statements: &[(String, String, usize)]) -> Vec<String> {
    // Requirements and carriers keep the order in which they first appear.
    let mut keys: Vec<&str> = Vec::new();
    for (key, ..) in statements {
        if !keys.contains(&key.as_str()) {
            keys.push(key);
        }
    }
    let mut rows: Vec<(&str, String, &str)> = Vec::new();
    for key in keys {
        let mut carriers: Vec<&String> = Vec::new();
        for (_, carrier, _) in statements.iter().filter(|(other, ..)| other == key) {
            if !carriers.contains(&carrier) {
                carriers.push(carrier);
            }
        }
        for carrier in carriers {
            let positions: Vec<String> = statements
                .iter()
                .filter(|(other, kind, _)| other == key && kind == carrier)
                .map(|(.., position)| format!("#{position}"))
                .collect();
            rows.push((key, positions.join(" "), carrier.as_str()));
        }
    }
    let key_width = rows.iter().map(|(key, ..)| key.len()).max().unwrap_or(0);
    let position_width = rows
        .iter()
        .map(|(_, positions, _)| positions.len())
        .max()
        .unwrap_or(0);
    rows.into_iter()
        .map(|(key, positions, carrier)| {
            format!("{key:<key_width$}  {positions:<position_width$}  {carrier}")
        })
        .collect()
}

fn single_action(est: &Value) -> Option<&str> {
    let action = &est["action"];
    match action["op"].as_str()? {
        "==" | "in" if action.get("entity").is_some() => action["entity"]["id"].as_str(),
        "in" => {
            let entities = action["entities"].as_array()?;
            let first = entities.first()?["id"].as_str()?;
            entities
                .iter()
                .all(|entity| entity["id"] == first)
                .then_some(first)
        }
        _ => None,
    }
}

/// Lists the permits that stop for confirmation before they take effect.
///
/// A task permit may carry `@advice` with an escalation verdict, which turns an already granted
/// permission into one a human confirms before each execution. This lists every such permit with
/// the paths the prompt shows and the message it prints, so a reviewer sees the gates in the
/// revision instead of only at run time.
///
/// # Errors
/// Rejects text that `prepare_policy` would also reject as unparsable or oversized.
pub fn human_gates(text: &str) -> Result<Vec<HumanGate>, PolicyError> {
    let mut gates = Vec::new();
    for (index, policy) in parse(text)?.policies().enumerate() {
        let Some(value) = policy.annotation("advice") else {
            continue;
        };
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        let (action, points, trees) = statement_paths(&est);
        // A directory pair names the same location twice and the prompt lists it once.
        let mut paths = points;
        paths.extend(trees);
        dedupe(&mut paths);
        gates.push(HumanGate {
            action,
            requirement: requirement_key(policy),
            message: escalation_message(value)?,
            paths,
            position: index + 1,
        });
    }
    Ok(gates)
}

/// Lists confirmations that would widen authorization instead of only adding friction.
///
/// A confirmation is permission the policy already grants, made conditional on a human. A permit
/// whose paths no other permit of the same action grants is not a confirmation but a grant with
/// one click in front of it, so it is reported: the task has to name that location in its
/// writable scope first, and only then may a requirement ask about it.
///
/// `network.use` is exempt from that path rule: the action covers the whole network and has no
/// path or endpoint dimension, so there is nothing narrower to grant first and the gate is the
/// request for approval before launching a command with network access.
///
/// Command and executable approvals can use the invocation context. File-read and file-write
/// rules must be projectable to persistent OS permissions.
///
/// # Errors
/// Rejects text that `prepare_policy` would also reject as unparsable or oversized.
pub fn confirmation_gaps(text: &str) -> Result<Vec<String>, PolicyError> {
    let policies = parse(text)?;
    // What other statements already grant, per action; a confirmation may only repeat this.
    let mut granted: Vec<(String, Vec<String>, Vec<String>)> = Vec::new();
    for policy in policies.policies() {
        if policy.effect() != cedar_policy::Effect::Permit || policy.annotation("advice").is_some()
        {
            continue;
        }
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        granted.push(statement_paths(&est));
    }
    let mut gaps = Vec::new();
    for (index, policy) in policies.policies().enumerate() {
        if policy.annotation("advice").is_none() {
            continue;
        }
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        let at = label(index, policy);
        let (action, points, trees) = statement_paths(&est);
        if matches!(action.as_str(), "process.spawn" | "file.exec") {
            continue;
        }
        if matches!(action.as_str(), "file.read" | "file.write")
            && context_fields(&est["conditions"], &mut Vec::new())
        {
            gaps.push(format!(
                "permit {at} asks before acting but tests context, which the OS authorizer cannot project for a filesystem grant"
            ));
            continue;
        }
        if action.is_empty() {
            gaps.push(format!(
                "permit {at} asks before acting without naming exactly one action"
            ));
            continue;
        }
        // A whole-network gate names no path, and no other permit may grant the network first.
        if action == NETWORK_ACTION {
            if !est["conditions"].as_array().is_some_and(Vec::is_empty) {
                gaps.push(format!(
                    "permit {at} must request whole-network confirmation without conditions"
                ));
            }
            continue;
        }
        if points.is_empty() && trees.is_empty() {
            gaps.push(format!(
                "permit {at} asks before acting without naming a path or a union of paths as its entire condition"
            ));
            continue;
        }
        for point in points {
            if !granted.iter().filter(|(other, ..)| other == &action).any(
                |(_, granted_points, granted_trees)| {
                    granted_points.contains(&point)
                        || granted_trees
                            .iter()
                            .any(|tree| tree != &point && under(tree, &point))
                },
            ) {
                gaps.push(format!(
                    "permit {at} asks before acting on {point}, which no other permit grants \
                     ({action}), so the confirmation would widen authorization"
                ));
            }
        }
        for tree in trees {
            if !granted
                .iter()
                .filter(|(other, ..)| other == &action)
                .any(|(_, _, granted_trees)| granted_trees.iter().any(|other| under(other, &tree)))
            {
                gaps.push(format!(
                    "permit {at} asks before acting on {tree}, which no other permit grants \
                     ({action}), so the confirmation would widen authorization"
                ));
            }
        }
    }
    // A directory pair that nothing grants fails both checks, but it is one location.
    let mut unique = Vec::new();
    for gap in gaps {
        if !unique.contains(&gap) {
            unique.push(gap);
        }
    }
    Ok(unique)
}

/// One permit that asks a human before it takes effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HumanGate {
    /// Action submitted for approval at launch.
    pub action: String,
    /// Requirement the `@id` names, or the statement number when it has none.
    pub requirement: String,
    /// Explanation the approval prompt shows.
    pub message: String,
    /// Paths the confirmation covers, exactly as the prompt will list them; empty for a
    /// whole-network gate, which has no path.
    pub paths: Vec<String>,
    /// Position of the statement in the revision, counting from one.
    pub position: usize,
}

impl HumanGate {
    /// Names the permit the way the gap messages and the statements do.
    pub(crate) fn label(&self) -> String {
        if self.requirement == NO_REQUIREMENT {
            format!("#{}", self.position)
        } else {
            format!("#{} @id({:?})", self.position, self.requirement)
        }
    }
}

/// Reads the one advice payload a task permit may carry: an escalation with a reason to show.
pub(crate) fn escalation_message(value: &str) -> Result<String, PolicyError> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Payload {
        verdict: String,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        message: Option<String>,
    }
    let payload: Payload = serde_json::from_str(value).map_err(|_| PolicyError::Advice)?;
    // `warn` has no decision surface on this path and `transform` would rewrite the command a
    // task may not choose; only escalation reads a decision back to a human.
    if payload.verdict != "escalate" {
        return Err(PolicyError::Advice);
    }
    if payload
        .reason
        .is_some_and(|reason| reason.trim().is_empty())
    {
        return Err(PolicyError::Advice);
    }
    payload
        .message
        .filter(|message| !message.trim().is_empty())
        .ok_or(PolicyError::Advice)
}

/// Returns the action, point grants and granted subtrees a permit names.
fn statement_paths(est: &Value) -> (String, Vec<String>, Vec<String>) {
    let action = if est["action"]["op"] == "==" {
        est["action"]["entity"]["id"].as_str().unwrap_or_default()
    } else {
        ""
    }
    .to_owned();
    let (mut points, mut trees) = (Vec::new(), Vec::new());
    // A literal inside a negation or an extra condition does not prove permission.
    if let Some([condition]) = est["conditions"].as_array().map(Vec::as_slice)
        && condition["kind"] == "when"
        && path_literals(&condition["body"], &mut points, &mut trees)
    {
        dedupe(&mut points);
        dedupe(&mut trees);
        return (action, points, trees);
    }
    (action, Vec::new(), Vec::new())
}

/// Removes repeated entries, which a `==` half and a `like` half of one directory pair produce.
fn dedupe(paths: &mut Vec<String>) {
    let mut index = 0;
    while index < paths.len() {
        if paths[..index].contains(&paths[index]) {
            paths.remove(index);
        } else {
            index += 1;
        }
    }
}

/// Returns the requirement an `@id` names; sub-rules such as `R2.1` share their parent.
fn requirement_key(policy: &cedar_policy::Policy) -> String {
    match policy.annotation("id") {
        Some(id) => id.split('.').next().unwrap_or(id).to_owned(),
        None => NO_REQUIREMENT.to_owned(),
    }
}

/// Returns whether a granted subtree covers a path.
fn under(tree: &str, path: &str) -> bool {
    tree == "/" || tree == path || path.starts_with(&format!("{tree}/"))
}

fn label(index: usize, policy: &cedar_policy::Policy) -> String {
    match policy.annotation("id") {
        Some(id) => format!("#{} @id({id:?})", index + 1),
        None => format!("#{}", index + 1),
    }
}

/// Collects `resource.path == "..."` literals and the subtrees of `like "<dir>/*"` patterns.
pub(crate) fn path_literals(
    value: &Value,
    points: &mut Vec<String>,
    trees: &mut Vec<String>,
) -> bool {
    let path = json!({".": {"left": {"Var": "resource"}, "attr": "path"}});
    if let Some(or) = value.get("||") {
        return path_literals(&or["left"], points, trees)
            && path_literals(&or["right"], points, trees);
    }
    if let Some(binary) = value.get("==") {
        for (side, other) in [("left", "right"), ("right", "left")] {
            if binary[side] == path
                && let Some(literal) = binary[other]["Value"].as_str()
            {
                points.push(literal.to_owned());
                return true;
            }
        }
    }
    if let Some(like) = value.get("like")
        && like["left"] == path
        && let Ok((tree, true)) = pattern_scope(&like["pattern"])
    {
        trees.push(tree);
        return true;
    }
    false
}

/// Returns whether `value` reads `context`, collecting the `context.input` fields it names.
fn context_fields(value: &Value, fields: &mut Vec<String>) -> bool {
    let input = json!({".": {"left": {"Var": "context"}, "attr": "input"}});
    if value["."]["left"] == input
        && let Some(field) = value["."]["attr"].as_str()
    {
        fields.push(field.to_owned());
        return true;
    }
    if value.get("Var").is_some_and(|var| var == "context") {
        return true;
    }
    let mut found = false;
    match value {
        Value::Object(map) => {
            for child in map.values() {
                found |= context_fields(child, fields);
            }
        }
        Value::Array(values) => {
            for child in values {
                found |= context_fields(child, fields);
            }
        }
        _ => {}
    }
    found
}

fn parse(text: &str) -> Result<PolicySet, PolicyError> {
    if text.len() > MAX_POLICY_BYTES {
        return Err(PolicyError::TooLarge);
    }
    let policies = PolicySet::from_str(text).map_err(|_| PolicyError::Syntax)?;
    if policies.templates().next().is_some() {
        return Err(PolicyError::UnsupportedPolicy);
    }
    if policies.policies().count() > MAX_POLICIES {
        return Err(PolicyError::TooLarge);
    }
    Ok(policies)
}

fn check_entity_references(value: &Value) -> Result<(), PolicyError> {
    match value {
        Value::Object(fields) => {
            if fields
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "SecCore::Subject" | "SecCore::File"))
            {
                return Err(PolicyError::UnsupportedPolicy);
            }
            for child in fields.values() {
                check_entity_references(child)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                check_entity_references(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn path_attribute(value: &Value) -> bool {
    value == &json!({".": {"left": {"Var": "resource"}, "attr": "path"}})
        || ["cwd", "program"].iter().any(|name| {
            value == &json!({".": {"left": {".": {"left": {"Var": "context"}, "attr": "input"}}, "attr": name}})
        })
}

fn check_expression(value: &Value, depth: usize) -> Result<(), PolicyError> {
    if depth > 128 {
        return Err(PolicyError::TooLarge);
    }
    // A path-carrying attribute must be read from its canonical receiver. A record projection
    // such as `{r: resource}.r.path` reaches the same string while hiding it from the checks
    // below, so any other receiver is rejected instead of being compared by shape.
    if let Some(attribute) = value["."]["attr"].as_str() {
        let canonical = match attribute {
            "path" => value["."]["left"] == json!({"Var": "resource"}),
            "cwd" => {
                value["."]["left"] == json!({".": {"left": {"Var": "context"}, "attr": "input"}})
            }
            // `SecCore::Process.program` is a plain entity attribute holding a command name;
            // only the request context's `program` is an executable path.
            "program" => {
                value["."]["left"] == json!({"Var": "resource"})
                    || value["."]["left"]
                        == json!({".": {"left": {"Var": "context"}, "attr": "input"}})
            }
            _ => true,
        };
        if !canonical {
            return Err(PolicyError::InvalidPath);
        }
    }
    for op in ["==", "!="] {
        if let Some(binary) = value.get(op) {
            let (left, right) = (&binary["left"], &binary["right"]);
            let literal = if path_attribute(left) {
                Some(right)
            } else if path_attribute(right) {
                Some(left)
            } else {
                None
            };
            if let Some(literal) = literal {
                // A trailing `/*` belongs to a `like` pattern; as an equality literal it names no
                // concrete path and the projection cannot express it, so the model must retry.
                let Some(value) = literal["Value"].as_str() else {
                    return Err(PolicyError::InvalidPath);
                };
                if value.ends_with("/*") {
                    return Err(PolicyError::WildcardLiteral);
                }
                return if absolute_path(value) {
                    Ok(())
                } else {
                    Err(PolicyError::InvalidPath)
                };
            }
        }
    }
    if let Some(like) = value.get("like")
        && path_attribute(&like["left"])
    {
        return check_pattern(&like["pattern"]);
    }
    // Policies are never rewritten by string replacement; an unchecked path read must fail rather
    // than be guessed.
    if path_attribute(value) {
        return Err(PolicyError::InvalidPath);
    }
    if value.get("Var").is_some_and(|var| var == "principal") {
        return Err(PolicyError::UnsupportedPolicy);
    }
    match value {
        Value::Object(fields) => {
            for child in fields.values() {
                check_expression(child, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                check_expression(child, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_pattern(value: &Value) -> Result<(), PolicyError> {
    pattern_scope(value).map(|_| ())
}

/// Decodes a `like` pattern into its canonical absolute prefix and whether a single trailing
/// wildcard covers the remaining subtree.
pub(crate) fn pattern_scope(value: &Value) -> Result<(String, bool), PolicyError> {
    let elements = value.as_array().ok_or(PolicyError::InvalidPath)?;
    let mut prefix = String::new();
    let mut wildcard = false;
    for (index, element) in elements.iter().enumerate() {
        if element == "Wildcard" {
            if index + 1 != elements.len() || !prefix.ends_with('/') {
                return Err(PolicyError::InvalidPath);
            }
            wildcard = true;
        } else if let Some(literal) = element["Literal"].as_str() {
            prefix.push_str(literal);
        } else {
            return Err(PolicyError::InvalidPath);
        }
    }
    let path = if wildcard && prefix != "/" {
        prefix.strip_suffix('/').ok_or(PolicyError::InvalidPath)?
    } else {
        &prefix
    };
    if absolute_path(path) {
        Ok((path.to_owned(), wildcard))
    } else {
        Err(PolicyError::InvalidPath)
    }
}

/// Accepts canonical absolute paths only; relative and aliased forms must be rewritten by the
/// model or moved into unresolved.
pub(crate) fn absolute_path(path: &str) -> bool {
    path.len() <= 4096
        && (path == "/"
            || path.strip_prefix('/').is_some_and(|p| {
                !p.contains('\\')
                    && !p.chars().any(char::is_control)
                    && p.split('/')
                        .all(|part| !part.is_empty() && part != "." && part != "..")
            }))
}
