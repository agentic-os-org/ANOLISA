//! SecCore-internal Lua deployment artifacts and their canonical source encoding.
//!
//! Only Lua source reaches the kernel's register node; unregister receives the
//! module name. Binding metadata and routes remain internal to `SecCore`.

#![forbid(unsafe_code)]

mod artifact;

pub use artifact::{
    PathView, is_supported_path, is_supported_pattern, module_name, render_module,
    render_module_with_views,
};

use asc_file_policy::{FilePathPattern, FileProtectionPolicy};
use asc_policy_types::{
    identifiers::{ResourceId, Revision},
    process_discovery::ProcessIdentity,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// SecCore-internal envelope; only its Lua source is written to register.
pub const LUA_LSM_PLAN_FORMAT: &str = "lua-lsm.file-protection.v1";
/// Stable route retained in deployment cleanup records.
pub const LUA_LSM_ROUTE: &str = "lua-lsm";
/// Private bootstrap commands and durable deployment confirmations.
pub const DEFAULT_STATE_DIRECTORY: &str = "/var/lib/agent-sec/lua-lsm";

/// Complete immutable input for local validation and deployment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LuaLsmPlan {
    /// `SecCore` Binding identity; the kernel treats the resulting name as opaque.
    pub binding_id: ResourceId,
    /// Immutable deployed snapshot revision.
    pub binding_revision: Revision,
    /// PID-reuse-resistant discovery identity, checked before local attachment.
    pub process: ProcessIdentity,
    /// Shared file intent used for filesystem validation and initialization.
    pub policy: FileProtectionPolicy,
    /// Additional predicates for the zero-based targets that use runtime path matching.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub patterns: BTreeMap<usize, Vec<FilePathPattern>>,
    /// Canonical policy source, validated before adding the local mount view.
    pub source: String,
}

impl LuaLsmPlan {
    /// Ordinary-write rules check new directory entries before the filesystem creates them.
    pub fn protects_creation(&self) -> bool {
        self.policy
            .targets
            .iter()
            .any(|target| target.protections.ordinary_write)
    }
}
