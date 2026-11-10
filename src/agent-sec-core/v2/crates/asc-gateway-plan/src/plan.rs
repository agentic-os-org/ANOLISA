use std::collections::BTreeSet;

use asc_credential_store::{CredentialId, ScopeId};
pub use asc_policy_types::authoring::{HttpMethod as Method, HttpScheme as Scheme};
use asc_policy_types::identifiers::{ResourceId, Revision};
use serde::{Deserialize, Serialize};

use crate::PlanError;
pub use asc_policy_cedar::http::CredentialPath;
use asc_policy_cedar::http::{is_exact_host, validate_injection, validate_path};

/// Format identifier carried by every compiled Cedar credential plan.
pub const PLAN_FORMAT: &str = "agent-sec-gateway-plan/v2";

/// The addon loads every plan of every listener on each generation switch; bounds keep that cheap.
const MAX_RULES: usize = 256;
const MAX_CREDENTIALS: usize = 64;
const MAX_BECAUSE_BYTES: usize = 256;

/// Gateway rules for one Scope.
///
/// Requests carrying a managed fake are forwarded only when a rule names that
/// credential. Unmatched managed credentials are blocked; requests without a
/// managed credential are forwarded unchanged after request safety checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Plan {
    /// Always [`PLAN_FORMAT`].
    pub format: PlanFormat,
    /// Scope the plan applies to; listeners map one-to-one to prepared Scopes.
    pub scope_id: ScopeId,
    /// Immutable Policy revisions, sorted by identity, retained for audit.
    pub policies: Vec<PolicySource>,
    /// Credentials whose fakes this domain may present.
    pub credentials: Vec<CredentialId>,
    /// Ordered rules; decisions cite their zero-based position in this array.
    pub rules: Vec<Rule>,
}

/// Versioned gateway plan tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanFormat {
    /// Legacy tag; not valid for the Cedar credential rule shape.
    #[serde(rename = "agent-sec-gateway-plan/v1")]
    V1,
    /// Cedar rules with explicit injection metadata and nullable path constraints.
    #[serde(rename = "agent-sec-gateway-plan/v2")]
    V2,
}

/// Immutable Policy revision contributing gateway rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicySource {
    /// Source Policy identity.
    pub id: ResourceId,
    /// Source Policy revision.
    pub revision: Revision,
}

/// Source location of one compiled rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuleSource {
    /// Source Policy identity.
    pub policy_id: ResourceId,
    /// Source Policy revision.
    pub policy_revision: Revision,
    /// Rule position in canonical Cedar rule-ID order, independent of SDK iteration order.
    pub rule_index: usize,
}

/// One credential-injecting gateway rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rule {
    /// Exact location in the authored Policy snapshot, independent of the Plan array position.
    pub source: RuleSource,
    /// Request facts that must all hold.
    #[serde(rename = "match")]
    pub matcher: RequestMatch,
    /// Authentication header explicitly selected by Cedar annotations.
    pub header: String,
    /// `auto`, `basic`, or a single-`{fake}` template, without an implicit default.
    pub format: String,
    /// Rewrite echoed credentials in response headers, uncompressed bodies and trailers.
    /// False leaves the entire response unchanged; request injection is independent.
    pub response_rewrite: bool,
    /// Operator-facing reason.
    pub because: String,
}

/// Conjunction of request facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RequestMatch {
    /// Only HTTPS may carry a real value.
    pub scheme: Scheme,
    /// Exact lowercase host.
    pub host: String,
    /// `None` serializes as null (unrestricted); `like` retains Cedar string semantics.
    pub path: Option<CredentialPath>,
    /// Sorted, unique uppercase methods; CONNECT, TRACE and OPTIONS are not expressible.
    pub methods: Vec<Method>,
    /// Credential this rule injects.
    pub credential_id: CredentialId,
}

impl Plan {
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
        if self.format != PlanFormat::V2 {
            return invalid(
                "format",
                "Cedar credential rules require agent-sec-gateway-plan/v2",
            );
        }
        let policies = &self.policies;
        let ids: Vec<&ResourceId> = policies.iter().map(|policy| &policy.id).collect();
        if policies.is_empty() || policies.len() > 32 || !is_strictly_sorted(&ids) {
            return invalid(
                "policies",
                "must be sorted, unique and contain 1..32 policies",
            );
        }
        if self.credentials.len() > MAX_CREDENTIALS {
            return invalid("credentials", "too many credentials");
        }
        let credentials: BTreeSet<&CredentialId> = self.credentials.iter().collect();
        if credentials.len() != self.credentials.len() {
            return invalid("credentials", "must be unique");
        }
        if self.rules.len() > MAX_RULES {
            return invalid("rules", "too many rules");
        }
        let mut sources = BTreeSet::new();
        for (position, rule) in self.rules.iter().enumerate() {
            rule.validate(position, &credentials)?;
            let source = &rule.source;
            // Revoked credentials may remove rules without renumbering authored source positions.
            if source.rule_index >= 1024
                || !sources.insert((&source.policy_id, source.rule_index))
                || !policies.iter().any(|policy| {
                    policy.id == source.policy_id && policy.revision == source.policy_revision
                })
            {
                return Err(PlanError::InvalidRule {
                    index: position,
                    field: "source",
                    reason: "must uniquely cite a declared Policy revision and rule index below 1024",
                });
            }
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
        if !is_exact_host(&self.matcher.host) {
            return invalid("match.host", "must be an exact lowercase DNS name");
        }
        if let Some(path) = &self.matcher.path {
            validate_path(path).map_err(|reason| PlanError::InvalidRule {
                index: position,
                field: "match.path",
                reason,
            })?;
        }
        validate_injection(&self.header, &self.format).map_err(|reason| {
            PlanError::InvalidRule {
                index: position,
                field: "header/format",
                reason,
            }
        })?;
        if self.matcher.methods.is_empty() || !is_strictly_sorted(&self.matcher.methods) {
            return invalid("match.methods", "must be non-empty, sorted and unique");
        }
        if !credentials.contains(&self.matcher.credential_id) {
            return invalid("match.credentialId", "names a credential outside the plan");
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
