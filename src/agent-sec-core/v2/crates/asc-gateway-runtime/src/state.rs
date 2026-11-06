//! Durable execution projection; canonical authorization lives only in PAP.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use std::path::Path;

use asc_credential_store::ScopeId;
use asc_policy_types::identifiers::ResourceId;
use asc_policy_types::process_discovery::ProcessIdentity;
use serde::{Deserialize, Serialize};

use crate::GatewayError;
use crate::files::{self, ensure_directory, read_limited};

pub(crate) const STATE_FILE_NAME: &str = "state.json";
const STATE_VERSION: u32 = 2;
const MAX_STATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Attachment {
    pub(crate) process: ProcessIdentity,
    pub(crate) cgroup: String,
    /// Cleanup references preserve intersecting groups without moving their owned members.
    #[serde(default)]
    pub(crate) shared_cgroups: BTreeSet<String>,
}

impl Attachment {
    pub(crate) fn cgroups(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.cgroup.as_str()).chain(self.shared_cgroups.iter().map(String::as_str))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Domain {
    pub(crate) uid: u32,
    pub(crate) port: u16,
    /// Irreversible local fence, retained until PAP confirms Binding cleanup.
    pub(crate) revoked: bool,
    pub(crate) bindings: BTreeMap<ResourceId, Attachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct State {
    version: u32,
    pub(crate) enabled: bool,
    /// Reserved before publication, including failed attempts; never reused.
    pub(crate) generation: u64,
    pub(crate) scopes: BTreeMap<ScopeId, Domain>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            enabled: false,
            generation: 0,
            scopes: BTreeMap::new(),
        }
    }
}

impl State {
    pub(crate) fn load(
        directory: &Path,
        ports: &RangeInclusive<u16>,
    ) -> Result<Self, GatewayError> {
        let Some(bytes) = read_limited(&directory.join(STATE_FILE_NAME), MAX_STATE_BYTES)? else {
            return Ok(Self::default());
        };
        let state: Self = serde_json::from_slice(&bytes)
            .map_err(|_| GatewayError::internal("corrupt gateway execution projection"))?;
        if state.version != STATE_VERSION {
            return Err(GatewayError::internal(
                "unsupported gateway projection version",
            ));
        }
        let mut used = BTreeSet::new();
        for domain in state.scopes.values() {
            if domain.uid == 0 || !ports.contains(&domain.port) || !used.insert(domain.port) {
                return Err(GatewayError::internal(
                    "invalid gateway owner or listener projection",
                ));
            }
            for attachment in domain.bindings.values() {
                if attachment.shared_cgroups.contains(&attachment.cgroup)
                    || attachment
                        .cgroups()
                        .any(|path| !crate::nft::is_safe_cgroup_path(path))
                {
                    return Err(GatewayError::internal(
                        "unsafe gateway attachment projection",
                    ));
                }
            }
        }
        Ok(state)
    }

    pub(crate) fn save(&self, directory: &Path) -> Result<(), GatewayError> {
        ensure_directory(directory, 0o700, None)?;
        let bytes = serde_json::to_vec(self)
            .map_err(|_| GatewayError::internal("cannot encode gateway execution projection"))?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(GatewayError::new(
                crate::GatewayErrorKind::ResourceExhausted,
                "gateway projection capacity exceeded",
            ));
        }
        files::write_atomic(directory, STATE_FILE_NAME, &bytes, 0o600, None)
    }

    pub(crate) fn free_port(&self, ports: &RangeInclusive<u16>) -> Option<u16> {
        let used: BTreeSet<_> = self.scopes.values().map(|domain| domain.port).collect();
        ports.clone().find(|port| !used.contains(port))
    }
}
