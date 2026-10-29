use std::collections::BTreeSet;

use asc_credential_store::{AgentName, CredentialId};
use serde::{Deserialize, Serialize};

use crate::PlanError;

/// Format identifier carried by every v1 plan.
pub const PLAN_FORMAT: &str = "agent-sec-gateway-plan/v1";

/// The addon loads every plan of every listener on each generation switch; bounds keep that cheap.
const MAX_RULES: usize = 256;
const MAX_CREDENTIALS: usize = 64;
const MAX_BECAUSE_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 512;
const MAX_HOST_BYTES: usize = 253;

/// Gateway rules for one Agent domain.
///
/// Requests on the domain's listener are evaluated against `rules`. A request
/// that carries one of the domain's fakes is forwarded only when an `allow`
/// rule names that credential (`credentialRequestDefault` is always `block`);
/// requests without a managed credential follow `requestDefault`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    /// Always [`PLAN_FORMAT`].
    pub format: PlanFormat,
    /// Agent domain the plan applies to.
    pub subject: Subject,
    /// What produced the plan.
    pub source: PlanSource,
    /// Credentials whose fakes this domain may present.
    pub credentials: Vec<CredentialRef>,
    /// Outcome for a managed credential that no rule allows.
    pub credential_request_default: CredentialDefault,
    /// Outcome for requests without a managed credential.
    pub request_default: DefaultEffect,
    /// Ordered rules; `index` equals the position.
    pub rules: Vec<Rule>,
}

/// Format tag; only v1 exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanFormat {
    /// `agent-sec-gateway-plan/v1`.
    #[serde(rename = "agent-sec-gateway-plan/v1")]
    V1,
}

/// Policy subject; listeners map one-to-one to Agent domains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Subject {
    /// An Agent domain.
    Agent {
        /// Domain name.
        name: AgentName,
    },
}

/// Origin of a plan, kept for status reporting only; the gateway does not branch on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PlanSource {
    /// Expanded from built-in presets, as `name@version`.
    Presets {
        /// Sorted preset identifiers.
        presets: Vec<String>,
    },
}

/// A credential the subject may present.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialRef {
    /// Credential Store identifier.
    pub id: CredentialId,
}

/// Outcome for an unmatched managed credential. Only `block` is expressible:
/// letting a fake through unmatched would make an authorization gap silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CredentialDefault {
    /// Reject the request.
    Block,
}

/// Outcome for requests that no rule decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DefaultEffect {
    /// Forward unchanged.
    Allow,
    /// Reject.
    Block,
}

/// One gateway rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rule {
    /// Position in `rules`, repeated so decisions can cite it.
    pub index: usize,
    /// Evaluation phase.
    pub phase: Phase,
    /// Effect on a match.
    pub effect: Effect,
    /// Request facts that must all hold.
    #[serde(rename = "match")]
    pub matcher: RequestMatch,
    /// How the response to an injected request is inspected.
    pub response_rewrite: ResponseRewrite,
    /// Operator-facing reason.
    pub because: String,
}

/// Evaluation phase; v1 has request rules only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Before the request is forwarded.
    Request,
}

/// Rule effect; v1 has credential-injecting `allow` rules only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Effect {
    /// Forward, replacing the fake with the real value.
    Allow,
}

/// Conjunction of request facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestMatch {
    /// Only HTTPS may carry a real value.
    pub scheme: Scheme,
    /// Exact lowercase host.
    pub host: String,
    /// Path pattern: `*` matches within one segment, `**` across segments.
    pub path: String,
    /// Allowed methods.
    pub method: MethodMatch,
    /// Credentials this rule injects.
    pub credential: CredentialMatch,
}

/// URL scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Scheme {
    /// TLS between client and gateway, with verified upstream TLS.
    Https,
}

/// Method set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MethodMatch {
    /// Uppercase methods; CONNECT, TRACE and OPTIONS are not expressible.
    #[serde(rename = "in")]
    pub any_of: Vec<Method>,
}

/// Credential set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialMatch {
    /// Credentials named by the rule.
    #[serde(rename = "in")]
    pub any_of: Vec<CredentialId>,
}

/// HTTP methods a credential rule may allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
#[allow(missing_docs)]
pub enum Method {
    Get,
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

/// Response handling for injected requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseRewrite {
    /// Buffer a bounded identity/gzip body and replace echoed real values with the fake.
    #[serde(rename = "buffered")]
    Buffered,
    /// Stream unchanged; reserved for bodies that cannot echo the credential, such as packfiles.
    #[serde(rename = "none")]
    Passthrough,
}

impl Plan {
    /// Decodes and validates a plan document.
    ///
    /// # Errors
    /// Returns [`PlanError::Decode`] for malformed JSON or unknown fields and
    /// the validation errors of [`Plan::validate`].
    pub fn from_json(bytes: &[u8]) -> Result<Self, PlanError> {
        let plan: Self =
            serde_json::from_slice(bytes).map_err(|error| PlanError::Decode(error.to_string()))?;
        plan.validate()?;
        Ok(plan)
    }

    /// Encodes the plan as the JSON document the gateway loads.
    #[must_use]
    pub fn to_json(&self) -> Vec<u8> {
        // Every field is a plain string, number, enum or vector; encoding cannot fail.
        serde_json::to_vec_pretty(self).unwrap_or_else(|_| unreachable!("plan encoding is total"))
    }

    /// Checks the constraints the type system does not express.
    ///
    /// # Errors
    /// Returns [`PlanError::Invalid`] or [`PlanError::InvalidRule`] naming the violated field.
    pub fn validate(&self) -> Result<(), PlanError> {
        let invalid = |field, reason| Err(PlanError::Invalid { field, reason });
        let PlanSource::Presets { presets } = &self.source;
        if presets.is_empty() || !is_strictly_sorted(presets) {
            return invalid("source.presets", "must be non-empty, sorted and unique");
        }
        if self.credentials.len() > MAX_CREDENTIALS {
            return invalid("credentials", "too many credentials");
        }
        let credentials: BTreeSet<&CredentialId> =
            self.credentials.iter().map(|entry| &entry.id).collect();
        if credentials.len() != self.credentials.len() {
            return invalid("credentials", "must be unique");
        }
        if self.rules.len() > MAX_RULES {
            return invalid("rules", "too many rules");
        }
        for (position, rule) in self.rules.iter().enumerate() {
            rule.validate(position, &credentials)?;
        }
        Ok(())
    }
}

impl Rule {
    fn validate(
        &self,
        position: usize,
        credentials: &BTreeSet<&CredentialId>,
    ) -> Result<(), PlanError> {
        let invalid = |field, reason| {
            Err(PlanError::InvalidRule {
                index: position,
                field,
                reason,
            })
        };
        if self.index != position {
            return invalid("index", "must equal the rule position");
        }
        if !is_exact_host(&self.matcher.host) {
            return invalid("match.host", "must be an exact lowercase DNS name");
        }
        if !is_path_pattern(&self.matcher.path) {
            return invalid("match.path", "must be an absolute path pattern");
        }
        if self.matcher.method.any_of.is_empty() || !is_strictly_sorted(&self.matcher.method.any_of)
        {
            return invalid("match.method", "must be non-empty, sorted and unique");
        }
        let named = &self.matcher.credential.any_of;
        if named.is_empty() || !is_strictly_sorted(named) {
            return invalid("match.credential", "must be non-empty, sorted and unique");
        }
        if !named.iter().all(|id| credentials.contains(id)) {
            return invalid("match.credential", "names a credential outside the plan");
        }
        if self.because.is_empty()
            || self.because.len() > MAX_BECAUSE_BYTES
            || self.because.chars().any(char::is_control)
        {
            return invalid(
                "because",
                "must be 1..=256 bytes without control characters",
            );
        }
        Ok(())
    }
}

/// Sorted uniqueness keeps encodings canonical, so equal plans compare byte-equal.
fn is_strictly_sorted<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

fn is_exact_host(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    host.len() <= MAX_HOST_BYTES
        && labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

/// Literal segments use RFC 3986 unreserved characters only, so a pattern
/// never depends on percent-decoding or dot-segment removal.
fn is_path_pattern(path: &str) -> bool {
    let Some(rest) = path.strip_prefix('/') else {
        return false;
    };
    path.len() <= MAX_PATH_BYTES
        && rest.split('/').all(|segment| {
            matches!(segment, "*" | "**")
                || (!segment.is_empty()
                    && segment != "."
                    && segment != ".."
                    && segment.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
                    }))
        })
}
