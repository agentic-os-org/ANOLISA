//! Pure, bounded lowering of credential-annotated Cedar into HTTP request rules.

use std::collections::BTreeSet;

use asc_policy_types::authoring::HttpMethod;
use cedar_policy::PolicySet;
use cedar_policy::pst::{
    ActionConstraint, BinaryOp, Clause, Effect, Expr, Literal, PatternElem, PrincipalConstraint,
    ResourceConstraint, Var,
};
use serde::{Deserialize, Serialize};

use crate::CedarError;

const ANNOTATIONS: [&str; 6] = [
    "modify",
    "real_token_file",
    "fake_token_file",
    "inject_header",
    "inject_format",
    "response_rewrite",
];
const MAX_RULES: usize = 256;
const MAX_NODES: usize = 4096;
const MAX_DEPTH: usize = 16;

/// Credential use authorized by one static Cedar statement; contains no secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialRule {
    /// Stable annotation identity, or the SDK parser's policy identity when omitted.
    pub rule_id: String,
    /// Canonical absolute file path; the frontend never reads it.
    pub real_token_file: String,
    /// Canonical absolute fake-token file path; the frontend never reads it.
    pub fake_token_file: String,
    /// Authentication header to inject.
    pub header: String,
    /// `auto`, `basic`, or a template containing exactly one `{fake}`.
    pub format: String,
    /// Rewrite echoed credentials in response headers, uncompressed bodies and trailers.
    /// False leaves the entire response unchanged; request injection is independent.
    pub response_rewrite: bool,
    /// Exact lowercase DNS host.
    pub host: String,
    /// Canonically sorted supported methods; omission expands to all six.
    pub methods: Vec<HttpMethod>,
    /// Omission leaves the request path unrestricted.
    pub path: Option<CredentialPath>,
}

/// Exact path or Cedar string pattern; `*` can cross path separators.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialPath {
    /// `eq` or `like`.
    pub kind: String,
    /// Literal path or Cedar pattern, without conversion to a filesystem glob.
    pub value: String,
}

pub(crate) fn is_annotation(name: &str) -> bool {
    ANNOTATIONS.contains(&name)
}

pub(crate) fn lower(
    policies: &PolicySet,
    revision: u32,
) -> Result<Vec<CredentialRule>, CedarError> {
    if !policies
        .policies()
        .any(|policy| policy.annotations().any(|(name, _)| is_annotation(name)))
    {
        return Ok(Vec::new());
    }
    let reject = |id: &str, reason| CedarError::Unsupported {
        policy_id: id.into(),
        reason,
    };
    if revision != 2 {
        return Err(reject(
            "policy-set",
            "credential HTTP policies require schema revision 2",
        ));
    }
    if policies.policies().count() > MAX_RULES {
        return Err(reject(
            "policy-set",
            "credential policy set exceeds 256 rules",
        ));
    }
    let syntax = policies.to_pst().map_err(Box::new)?;
    let mut nodes = 0;
    let mut ids = BTreeSet::new();
    let mut rules = Vec::new();
    // PST and SDK iterators have different orders. Resolve annotations by identity,
    // then sort effective rule IDs instead of pairing unrelated iterator positions.
    for policy in policies.policies() {
        rules.push(lower_rule(policy, &syntax, &mut ids, &mut nodes)?);
    }
    rules.sort_by(|a, b| a.rule_id.cmp(&b.rule_id));
    Ok(rules)
}

fn lower_rule(
    policy: &cedar_policy::Policy,
    syntax: &cedar_policy::pst::PolicySet,
    ids: &mut BTreeSet<String>,
    nodes: &mut usize,
) -> Result<CredentialRule, CedarError> {
    let reject = |id: &str, reason| CedarError::Unsupported {
        policy_id: id.into(),
        reason,
    };
    let sdk_id = policy.id().to_string();
    let body = syntax
        .policies
        .iter()
        .find(|(id, _)| id.to_string() == sdk_id)
        .map(|(_, policy)| policy.body())
        .ok_or_else(|| reject(&sdk_id, "missing SDK policy identity in PST"))?;
    let annotation = |name: &str| {
        policy
            .annotations()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value)
    };
    let required = |name: &str| {
        annotation(name)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                reject(
                    &sdk_id,
                    "credential annotations must all be present and non-empty",
                )
            })
    };
    let rule_id = annotation("id").unwrap_or(&sdk_id).to_owned();
    if asc_policy_types::identifiers::ResourceId::new(&rule_id).is_err()
        || !ids.insert(rule_id.clone())
    {
        return Err(reject(
            &sdk_id,
            "credential rule identities must be bounded and unique",
        ));
    }
    if body.effect != Effect::Permit
        || body.principal != PrincipalConstraint::Any
        || annotation("advice").is_some()
    {
        return Err(reject(
            &sdk_id,
            "credential rules require permit without principal constraints or advice",
        ));
    }
    if !matches!(&body.action, ActionConstraint::Eq(id) if id.ty.to_string() == "SecCore::Action" && id.eid == "network.send")
        || !matches!(&body.resource, ResourceConstraint::Is(ty) if ty.to_string() == "SecCore::HttpEndpoint")
    {
        return Err(reject(
            &sdk_id,
            "credential rules require network.send and resource is SecCore::HttpEndpoint",
        ));
    }
    if required("modify")? != "inject_credential" {
        return Err(reject(&sdk_id, "modify must be inject_credential"));
    }
    let real_token_file = required("real_token_file")?;
    let fake_token_file = required("fake_token_file")?;
    if !is_token_file(real_token_file)
        || !is_token_file(fake_token_file)
        || real_token_file == fake_token_file
    {
        return Err(reject(
            &sdk_id,
            "token files must be distinct canonical absolute file paths without expansion",
        ));
    }
    let header = required("inject_header")?;
    let format = required("inject_format")?;
    validate_injection(header, format).map_err(|reason| reject(&sdk_id, reason))?;
    let response_rewrite = match required("response_rewrite")? {
        "true" => true,
        "false" => false,
        _ => return Err(reject(&sdk_id, "response_rewrite must be true or false")),
    };
    let (host, methods, path) =
        lower_predicates(body.clauses(), nodes).map_err(|reason| reject(&sdk_id, reason))?;
    Ok(CredentialRule {
        rule_id,
        real_token_file: real_token_file.into(),
        fake_token_file: fake_token_file.into(),
        header: header.into(),
        format: format.into(),
        response_rewrite,
        host,
        methods,
        path,
    })
}

fn lower_predicates<'a>(
    clauses: impl IntoIterator<Item = &'a Clause>,
    nodes: &mut usize,
) -> Result<(String, Vec<HttpMethod>, Option<CredentialPath>), &'static str> {
    let mut predicates = Predicates::default();
    for clause in clauses {
        let Clause::When(expr) = clause else {
            return Err("credential rules do not support unless");
        };
        check_budget(expr, 1, nodes)?;
        predicates.consume(expr)?;
    }
    let host = predicates
        .host
        .ok_or("credential rules require exact host equality")?;
    if !is_exact_host(&host) {
        return Err("host must be an exact lowercase DNS name");
    }
    let mut methods = predicates.methods.unwrap_or_else(|| {
        vec![
            HttpMethod::Get,
            HttpMethod::Head,
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Delete,
        ]
    });
    methods.sort_unstable();
    Ok((host, methods, predicates.path))
}

fn is_token_file(value: &str) -> bool {
    value.len() <= 4096
        && value != "/"
        && value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains(['~', '$', '\\', '*', '?'])
        && !value.chars().any(char::is_control)
        && !value.contains("//")
        && !value.split('/').any(|part| matches!(part, "." | ".."))
}

/// Checks header syntax, protocol-control exclusions and the explicit injection template.
///
/// # Errors
/// Rejects malformed headers, control fields, controls and missing/repeated placeholders.
pub fn validate_injection(header: &str, format: &str) -> Result<(), &'static str> {
    let control = matches!(
        header.to_ascii_lowercase().as_str(),
        "host"
            | "content-length"
            | "transfer-encoding"
            | "connection"
            | "te"
            | "trailer"
            | "upgrade"
            | "keep-alive"
            | "proxy-connection"
            | "expect"
            | "content-type"
            | "content-encoding"
            | "accept-encoding"
            | "proxy-authorization"
            | "proxy-authenticate"
    );
    if header.is_empty()
        || header.len() > 128
        || control
        || !header
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
    {
        return Err(
            "inject_header must be an HTTP authentication header, not a protocol-control field",
        );
    }
    if format.is_empty() || format.len() > 512 || format.chars().any(char::is_control) {
        return Err("inject_format must be bounded and contain no controls");
    }
    if !matches!(format, "auto" | "basic") {
        let Some((before, after)) = format.split_once("{fake}") else {
            return Err("inject_format requires auto, basic, or exactly one {fake}");
        };
        if before.contains(['{', '}']) || after.contains(['{', '}']) {
            return Err("inject_format requires exactly one {fake} and no other placeholders");
        }
    }
    Ok(())
}

/// Accepts only exact, bounded lowercase DNS names, not IP literals or wildcards.
#[must_use]
pub fn is_exact_host(host: &str) -> bool {
    host.len() <= 253
        && host.split('.').count() >= 2
        && host.parse::<std::net::IpAddr>().is_err()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

/// Checks the path representation without imposing filesystem-glob semantics.
///
/// # Errors
/// Rejects unknown kinds, relative paths, overlong strings, controls and invalid pattern escapes.
pub fn validate_path(path: &CredentialPath) -> Result<(), &'static str> {
    if !matches!(path.kind.as_str(), "eq" | "like")
        || !path.value.starts_with('/')
        || path.value.len() > 512
        || path.value.chars().any(char::is_control)
    {
        return Err(
            "credential path requires eq/like and a bounded absolute string without controls",
        );
    }
    if path.kind == "eq" {
        let bytes = path.value.as_bytes();
        if path.value.contains(['\\', '?', '#'])
            || path.value.contains("//")
            || path.value.chars().any(char::is_whitespace)
            || path.value.split('/').any(|part| matches!(part, "." | ".."))
            || bytes.iter().enumerate().any(|(index, byte)| {
                if *byte != b'%' {
                    return false;
                }
                let Some(encoded) = bytes.get(index + 1..index + 3) else {
                    return true;
                };
                !encoded.iter().all(u8::is_ascii_hexdigit)
                    || encoded.eq_ignore_ascii_case(b"2f")
                    || encoded.eq_ignore_ascii_case(b"5c")
                    || encoded.eq_ignore_ascii_case(b"2e")
            })
        {
            return Err("credential equality path must be canonical");
        }
    }
    if path.kind == "like" {
        let mut chars = path.value.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' && !matches!(chars.next(), Some('*' | '\\')) {
                return Err("Cedar path pattern supports only escaped star or backslash");
            }
        }
    }
    Ok(())
}

fn check_budget(expr: &Expr, depth: usize, nodes: &mut usize) -> Result<(), &'static str> {
    *nodes += 1;
    if depth > MAX_DEPTH || *nodes > MAX_NODES {
        return Err("credential AST exceeds 16 levels or 4096 nodes");
    }
    match expr {
        Expr::BinaryOp { left, right, .. } => {
            check_budget(left, depth + 1, nodes)?;
            check_budget(right, depth + 1, nodes)
        }
        Expr::GetAttr { expr, .. } | Expr::Like { expr, .. } => {
            check_budget(expr, depth + 1, nodes)
        }
        Expr::Set(values) => {
            for value in values {
                check_budget(value, depth + 1, nodes)?;
            }
            Ok(())
        }
        Expr::Literal(_) | Expr::Var(_) => Ok(()),
        _ => Err("unsupported credential condition"),
    }
}

#[derive(Default)]
struct Predicates {
    host: Option<String>,
    https: bool,
    methods: Option<Vec<HttpMethod>>,
    path: Option<CredentialPath>,
}

impl Predicates {
    fn consume(&mut self, expr: &Expr) -> Result<(), &'static str> {
        match expr {
            Expr::BinaryOp {
                op: BinaryOp::And,
                left,
                right,
            } => {
                self.consume(left)?;
                self.consume(right)
            }
            Expr::BinaryOp {
                op: BinaryOp::Eq,
                left,
                right,
            } => {
                let (field, literal) = if matches!(left.as_ref(), Expr::Literal(_)) {
                    (right, left)
                } else {
                    (left, right)
                };
                let Expr::Literal(Literal::String(value)) = literal.as_ref() else {
                    return Err("credential equality requires a string literal");
                };
                if resource_host(field) {
                    if self.host.replace(value.to_string()).is_some() {
                        return Err("duplicate host constraint");
                    }
                } else if input_field(field, "scheme") && value == "https" {
                    if self.https {
                        return Err("duplicate scheme constraint");
                    }
                    self.https = true;
                } else if input_field(field, "method") {
                    self.set_methods(vec![method(value)?])?;
                } else if input_field(field, "path") {
                    self.set_path(CredentialPath {
                        kind: "eq".into(),
                        value: value.to_string(),
                    })?;
                } else {
                    return Err("unsupported credential equality");
                }
                Ok(())
            }
            Expr::BinaryOp {
                op: BinaryOp::Contains,
                left,
                right,
            } if input_field(right, "method") => {
                let Expr::Set(values) = left.as_ref() else {
                    return Err("method contains requires a fixed literal set");
                };
                let methods = values
                    .iter()
                    .map(|value| match value.as_ref() {
                        Expr::Literal(Literal::String(value)) => method(value),
                        _ => Err("method set must contain only fixed strings"),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.set_methods(methods)
            }
            Expr::Like { expr, pattern } if input_field(expr, "path") => {
                let mut value = String::new();
                for part in pattern {
                    match part {
                        PatternElem::Wildcard => value.push('*'),
                        // Preserve literal '*' and '\\' separately from wildcard tokens.
                        PatternElem::Char(ch) => {
                            if matches!(ch, '*' | '\\') {
                                value.push('\\');
                            }
                            value.push(*ch);
                        }
                    }
                }
                self.set_path(CredentialPath {
                    kind: "like".into(),
                    value,
                })
            }
            _ => Err(
                "credential rules consume only &&, host/scheme/method/path equality, fixed method contains, and path like",
            ),
        }
    }

    fn set_methods(&mut self, mut methods: Vec<HttpMethod>) -> Result<(), &'static str> {
        let count = methods.len();
        methods.sort_unstable();
        methods.dedup();
        if count == 0
            || count > 6
            || count != methods.len()
            || self.methods.replace(methods).is_some()
        {
            return Err("methods require one constraint containing 1..6 unique supported values");
        }
        Ok(())
    }

    fn set_path(&mut self, path: CredentialPath) -> Result<(), &'static str> {
        validate_path(&path)?;
        if self.path.replace(path).is_some() {
            return Err("duplicate path constraint");
        }
        Ok(())
    }
}

fn resource_host(expr: &Expr) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == "host" && matches!(expr.as_ref(), Expr::Var(Var::Resource)))
}

fn input_field(expr: &Expr, name: &str) -> bool {
    matches!(expr, Expr::GetAttr { expr, attr } if attr == name &&
        matches!(expr.as_ref(), Expr::GetAttr { expr, attr } if attr == "input" && matches!(expr.as_ref(), Expr::Var(Var::Context))))
}

fn method(value: &str) -> Result<HttpMethod, &'static str> {
    match value {
        "GET" => Ok(HttpMethod::Get),
        "HEAD" => Ok(HttpMethod::Head),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "PATCH" => Ok(HttpMethod::Patch),
        "DELETE" => Ok(HttpMethod::Delete),
        _ => Err("unsupported credential HTTP method"),
    }
}
