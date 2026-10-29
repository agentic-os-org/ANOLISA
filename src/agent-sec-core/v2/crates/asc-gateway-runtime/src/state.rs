//! Durable desired state: which Agent domains exist, who runs them, and their presets.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::RangeInclusive;
use std::path::Path;
use std::str::FromStr as _;

use asc_credential_store::AgentName;
use asc_gateway_plan::Preset;
use serde::{Deserialize, Serialize};

use crate::GatewayError;
use crate::files::{self, ensure_directory, read_limited};

pub(crate) const STATE_FILE_NAME: &str = "state.json";
const STATE_VERSION: u32 = 1;
const STATE_FILE_MODE: u32 = 0o600;
const MAX_STATE_BYTES: u64 = 1024 * 1024;

/// A built-in preset as persisted by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct StoredPreset(pub(crate) Preset);

impl TryFrom<String> for StoredPreset {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Preset::from_str(&value)
            .map(Self)
            .map_err(|_| format!("unknown preset {value:?}"))
    }
}

impl From<StoredPreset> for String {
    fn from(value: StoredPreset) -> Self {
        value.0.name().to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct AgentEntry {
    pub(crate) uid: u32,
    pub(crate) port: u16,
    pub(crate) presets: BTreeSet<StoredPreset>,
}

impl AgentEntry {
    pub(crate) fn presets(&self) -> Vec<Preset> {
        self.presets.iter().map(|preset| preset.0).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct State {
    pub(crate) version: u32,
    pub(crate) enabled: bool,
    /// Last generation handed to the data plane, successful or not.
    ///
    /// Never reused: the gateway reports failures per generation, so a reused
    /// number could match a stale failure report.
    pub(crate) generation: u64,
    pub(crate) agents: BTreeMap<AgentName, AgentEntry>,
}

impl State {
    pub(crate) fn load(
        directory: &Path,
        ports: &RangeInclusive<u16>,
    ) -> Result<Self, GatewayError> {
        let path = directory.join(STATE_FILE_NAME);
        let Some(bytes) = read_limited(&path, MAX_STATE_BYTES)? else {
            return Ok(Self {
                version: STATE_VERSION,
                ..Self::default()
            });
        };
        let state: Self = serde_json::from_slice(&bytes).map_err(|error| {
            GatewayError::internal(format!(
                "corrupt gateway state {} at line {} column {}",
                path.display(),
                error.line(),
                error.column()
            ))
        })?;
        state.validate(ports)?;
        Ok(state)
    }

    pub(crate) fn save(&self, directory: &Path) -> Result<(), GatewayError> {
        ensure_directory(directory, 0o700, None)?;
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|_| GatewayError::internal("cannot encode gateway state"))?;
        files::write_atomic(directory, STATE_FILE_NAME, &bytes, STATE_FILE_MODE, None)
    }

    fn validate(&self, ports: &RangeInclusive<u16>) -> Result<(), GatewayError> {
        let corrupt = |reason: &str| {
            Err(GatewayError::internal(format!(
                "corrupt gateway state: {reason}"
            )))
        };
        if self.version != STATE_VERSION {
            return corrupt("unsupported version");
        }
        let mut used = BTreeSet::new();
        for entry in self.agents.values() {
            if entry.uid == 0 {
                return corrupt("an Agent domain runs as root");
            }
            if !ports.contains(&entry.port) || !used.insert(entry.port) {
                return corrupt("listener ports are invalid or shared");
            }
        }
        Ok(())
    }

    pub(crate) fn free_port(&self, ports: &RangeInclusive<u16>) -> Option<u16> {
        let used: BTreeSet<u16> = self.agents.values().map(|entry| entry.port).collect();
        ports.clone().find(|port| !used.contains(port))
    }

    /// Whether any Agent domain other than `except` still uses `preset`.
    pub(crate) fn preset_in_use(&self, preset: Preset, except: &AgentName) -> bool {
        self.agents
            .iter()
            .any(|(name, entry)| name != except && entry.presets.contains(&StoredPreset(preset)))
    }
}
