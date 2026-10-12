use asc_foundation_types::ResourceId;
use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicyTemplate;
use asc_policy_types::scope::{PolicyReference, ScopeSelector};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Create one Policy with a server-generated identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreatePolicyParams {
    /// Human-readable Policy name.
    pub policy_name: String,
    /// Complete authored Policy intent.
    pub template: PolicyTemplate,
}

/// Update one existing Policy identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdatePolicyParams {
    /// Existing Policy identity.
    pub policy_id: ResourceId,
    /// Human-readable Policy name.
    pub policy_name: String,
    /// Complete authored Policy intent.
    pub template: PolicyTemplate,
}

/// Create one Scope with a server-generated identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateScopeParams {
    /// Authored process selection; unsupported selectors are rejected at admission.
    #[serde(
        deserialize_with = "deserialize_authored_selector",
        serialize_with = "serialize_authored_selector"
    )]
    pub selector: ScopeSelector,
    /// Exact policies assigned to every Scope; admission requires 1–32 distinct IDs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_templates: Vec<PolicyReference>,
}

fn deserialize_authored_selector<'de, D>(deserializer: D) -> Result<ScopeSelector, D::Error>
where
    D: Deserializer<'de>,
{
    let selector = ScopeSelector::deserialize(deserializer)?;
    validate_authored_selector(&selector).map_err(serde::de::Error::custom)?;
    Ok(selector)
}

fn serialize_authored_selector<S>(
    selector: &ScopeSelector,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    validate_authored_selector(selector).map_err(serde::ser::Error::custom)?;
    selector.serialize(serializer)
}

fn validate_authored_selector(selector: &ScopeSelector) -> Result<(), String> {
    selector
        .validate()
        .map_err(|error| format!("invalid selector at {}: {}", error.path, error.message))
}
