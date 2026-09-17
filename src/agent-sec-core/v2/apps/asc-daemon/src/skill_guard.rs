//! Process-owned `SkillGuard` configuration and startup recovery, independent of user HOME.

#[cfg(test)]
mod tests;

use asc_action_runtime::{ActionRuntime, ExecutionControl, Finalizer};
use asc_action_types::{ActionAttribution, ActionId, CallerIdentity, Correlation};
use asc_capability_skill_guard::command::GuardCommand;
use asc_capability_skill_guard::executor::{
    SkillGuardAuditProjector, SkillGuardExecutor, SkillGuardRequest,
};
use asc_capability_skill_guard::scanner::{ScannerConfig, ScannerRegistry};
use asc_capability_skill_guard::{
    GuardConfig, GuardError, SkillGuardService, SkillIdentity, SkillRoot,
};
use asc_daemon_handler::skillfs::{SkillFsBridge, SkillFsConfig};
use rustix::fs::{Mode, OFlags, mkdirat, open, openat};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read as _};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Settings {
    #[serde(default = "default_state")]
    state_dir: PathBuf,
    #[serde(default)]
    managed_skill_dirs: Vec<SkillIdentity>,
    #[serde(default)]
    scanners: Vec<ScannerConfig>,
    #[serde(default)]
    parsers: BTreeMap<String, String>,
    #[serde(default)]
    skillfs: Option<SkillFsConfig>,
}

fn default_state() -> PathBuf {
    PathBuf::from("/var/lib/agent-sec/skillguard")
}

pub(super) fn start(
    config: Option<&Path>,
) -> Result<(Arc<SkillGuardService>, Option<SkillFsConfig>), StartupError> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(StartupError::RootRequired);
    }
    let settings = if let Some(path) = config {
        read_settings(path)?
    } else {
        let path = Path::new("/etc/agent-sec/skillguard.json");
        match read_settings(path) {
            Ok(settings) => settings,
            Err(StartupError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Settings {
                state_dir: default_state(),
                managed_skill_dirs: Vec::new(),
                scanners: Vec::new(),
                parsers: BTreeMap::new(),
                skillfs: None,
            },
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
    private_state(&settings.state_dir)?;
    let service = Arc::new(SkillGuardService::new(
        GuardConfig {
            state_dir: settings.state_dir,
            managed_skill_dirs: settings.managed_skill_dirs,
        },
        ScannerRegistry::new(settings.scanners, settings.parsers)?,
    )?);
    Ok((service, settings.skillfs))
}

fn read_settings(path: &Path) -> Result<Settings, StartupError> {
    SkillIdentity::new(path)?;
    let parent = trusted_directory(path.parent().ok_or(StartupError::UnsafePath)?, false)?;
    let name = path.file_name().ok_or(StartupError::UnsafePath)?;
    let file = File::from(
        openat(
            &parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.nlink() != 1
        || metadata.len() > 1024 * 1024
    {
        return Err(StartupError::UnsafePath);
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(StartupError::UnsafePath);
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn private_state(path: &Path) -> Result<(), StartupError> {
    let directory = trusted_directory(path, true)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != 0 || metadata.mode() & 0o077 != 0 {
        return Err(StartupError::UnsafePath);
    }
    Ok(())
}

fn trusted_directory(path: &Path, create: bool) -> Result<File, StartupError> {
    SkillIdentity::new(path)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(open("/", flags, Mode::empty()).map_err(io::Error::from)?);
    for component in path.components() {
        if let Component::Normal(name) = component {
            if create {
                match mkdirat(&directory, name, Mode::from_raw_mode(0o700)) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(io::Error::from(error).into()),
                }
            }
            directory = File::from(
                openat(&directory, name, flags, Mode::empty()).map_err(io::Error::from)?,
            );
            let metadata = directory.metadata()?;
            if metadata.uid() != 0
                || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
            {
                return Err(StartupError::UnsafePath);
            }
        }
    }
    Ok(directory)
}

#[derive(Debug, thiserror::Error)]
pub(super) enum StartupError {
    #[error("SkillGuard system daemon must run as root")]
    RootRequired,
    #[error("SkillGuard startup recovery failed: {0}")]
    Recovery(String),
    #[error("unsafe SkillGuard configuration or private state path")]
    UnsafePath,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid SkillGuard system configuration: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Guard(#[from] GuardError),
}

/// Run startup recovery through the same public Action Runtime before socket admission.
pub(super) fn recover(
    service: &Arc<SkillGuardService>,
    finalizer: Finalizer,
    bridge: Option<&SkillFsBridge>,
) -> Result<(), StartupError> {
    let deadline = Instant::now() + Duration::from_secs(120);
    let runtime = ActionRuntime::new(
        ActionId::SkillGuard,
        SkillGuardExecutor::new(service.clone()),
        SkillGuardAuditProjector,
        finalizer,
    );
    let attribution = ActionAttribution {
        caller: CallerIdentity {
            uid: 0,
            gid: rustix::process::getegid().as_raw(),
            pid: u32::try_from(rustix::process::getpid().as_raw_pid()).map_err(io::Error::other)?,
        },
        correlation: Correlation::default(),
    };
    let run = |command, roots| {
        runtime.invoke(
            &ExecutionControl {
                deadline,
                cancelled: false,
            },
            &attribution,
            &SkillGuardRequest {
                command,
                roots,
                caller_uid: 0,
                preparation_error: None,
            },
        )
    };
    if service.key_status(deadline)?["rotationPending"] == true {
        let roots = service
            .rotation_skills(deadline)?
            .into_iter()
            .map(|identity| {
                bridge
                    .map_or_else(
                        || SkillRoot::direct(identity.path()),
                        |bridge| bridge.resolve(&identity, deadline),
                    )
                    .unwrap_or_else(|error| SkillRoot::unavailable(identity, error.to_string()))
            })
            .collect();
        let outcome = run(GuardCommand::RotateKeys {}, roots);
        if !outcome.success {
            return Err(StartupError::Recovery(outcome.error_type));
        }
    }
    if service.key_status(deadline)?["initialized"] == true {
        for identity in service.managed_skills()? {
            let root = match bridge.map_or_else(
                || SkillRoot::direct(identity.path()),
                |bridge| bridge.resolve(&identity, deadline),
            ) {
                Ok(root) => root,
                Err(error) => {
                    eprintln!(
                        "agent-sec-daemon: SkillGuard startup mapping unavailable for {}: {error}",
                        identity.path().display()
                    );
                    continue;
                }
            };
            let outcome = run(
                GuardCommand::Reconcile {
                    skill_dir: identity.clone(),
                },
                vec![root],
            );
            if !outcome.success {
                eprintln!(
                    "agent-sec-daemon: SkillGuard reconcile failed for {}: {}",
                    identity.path().display(),
                    outcome.error_type
                );
            }
        }
    }
    if let Some(bridge) = bridge {
        bridge
            .schedule_reconcile(service.managed_skills()?)
            .map_err(|e| StartupError::Recovery(e.to_string()))?;
    }
    Ok(())
}
