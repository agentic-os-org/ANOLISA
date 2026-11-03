//! Process-owned `SkillSec` configuration, independent of user HOME.

mod defaults;

use asc_capability_skill_sec::scanner::{ScannerConfig, ScannerRegistry};
use asc_capability_skill_sec::{ManagedSkillDir, SkillSecConfig, SkillSecError, SkillSecService};
use asc_daemon::skillfs::SkillFsConfig;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    #[serde(default = "default_state")]
    state_dir: PathBuf,
    #[serde(default)]
    managed_skill_dirs: Vec<ManagedSkillDir>,
    #[serde(default = "defaults_enabled")]
    enable_default_skill_dirs: bool,
    #[serde(default)]
    scanners: Vec<ScannerConfig>,
    #[serde(default)]
    parsers: BTreeMap<String, String>,
    #[serde(default)]
    skillfs: Option<SkillFsConfig>,
}

fn default_state() -> PathBuf {
    PathBuf::from("/var/lib/agent-sec/skillsec")
}

fn defaults_enabled() -> bool {
    true
}

pub(super) fn start(
    config: Option<&Path>,
) -> Result<(Arc<SkillSecService>, Option<SkillFsConfig>), StartupError> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(StartupError::RootRequired);
    }
    let settings = if let Some(path) = config {
        read_settings(path)?
    } else {
        let path = Path::new("/etc/agent-sec/skillsec.json");
        match read_settings(path) {
            Ok(settings) => settings,
            Err(StartupError::Guard(SkillSecError::Io { source: error, .. }))
                if error.kind() == io::ErrorKind::NotFound =>
            {
                Settings {
                    state_dir: default_state(),
                    managed_skill_dirs: Vec::new(),
                    enable_default_skill_dirs: true,
                    scanners: Vec::new(),
                    parsers: BTreeMap::new(),
                    skillfs: None,
                }
            }
            Err(error) => return Err(error),
        }
    };
    if settings
        .skillfs
        .as_ref()
        .is_some_and(|s| s.auth_key_file == settings.state_dir.join("signing-key.pk8"))
    {
        return Err(StartupError::UnsafePath);
    }
    asc_capability_skill_sec::config::prepare_private_state(&settings.state_dir)?;
    let default_skill_dirs = if settings.enable_default_skill_dirs {
        defaults::directories(Path::new("/etc/passwd"))?
    } else {
        Vec::new()
    };
    let service = Arc::new(
        SkillSecService::new(
            SkillSecConfig {
                state_dir: settings.state_dir,
                managed_skill_dirs: settings.managed_skill_dirs,
                default_skill_dirs,
            },
            ScannerRegistry::new(settings.scanners, settings.parsers)?,
        )?
        .with_config_path(
            config
                .unwrap_or_else(|| Path::new("/etc/agent-sec/skillsec.json"))
                .to_path_buf(),
        )?,
    );
    Ok((service, settings.skillfs))
}

fn read_settings(path: &Path) -> Result<Settings, StartupError> {
    Ok(serde_json::from_value(
        asc_capability_skill_sec::config::read_system_config(path)?,
    )?)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum StartupError {
    #[error("SkillSec system daemon must run as root")]
    RootRequired,
    #[error("unsafe SkillSec configuration or private state path")]
    UnsafePath,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid SkillSec system configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Guard(#[from] SkillSecError),
}

impl From<asc_action_types::SkillSecInputError> for StartupError {
    fn from(error: asc_action_types::SkillSecInputError) -> Self {
        Self::Guard(error.into())
    }
}
