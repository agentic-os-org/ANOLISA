use asc_foundation_types::{ResourceId, Revision};
use serde::{Deserialize, Deserializer, Serialize};

/// One stable resource identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceParams {
    /// Stable resource identity.
    pub id: ResourceId,
}

/// One stable resource identity plus an exact current revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevisionParams {
    /// Stable resource identity.
    pub id: ResourceId,
    /// Exact positive revision.
    pub revision: Revision,
}

/// Bounded client-maintained pagination state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListParams {
    /// Page size in the inclusive range 1..=1000.
    pub limit: u32,
    /// Zero-based client-maintained offset.
    pub offset: u32,
}

impl Default for ListParams {
    fn default() -> Self {
        Self {
            limit: 100,
            offset: 0,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ListParamsWire {
    #[serde(default = "default_limit")]
    limit: u32,
    #[serde(default)]
    offset: u32,
}

impl<'de> Deserialize<'de> for ListParams {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ListParamsWire::deserialize(deserializer)?;
        if !(1..=1_000).contains(&wire.limit) {
            return Err(serde::de::Error::custom("limit must be between 1 and 1000"));
        }
        Ok(Self {
            limit: wire.limit,
            offset: wire.offset,
        })
    }
}

const fn default_limit() -> u32 {
    100
}

/// One bounded page and its total before pagination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListResult<T> {
    /// Selected current records.
    pub items: Vec<T>,
    /// Total matching records before pagination.
    pub total: u64,
}
