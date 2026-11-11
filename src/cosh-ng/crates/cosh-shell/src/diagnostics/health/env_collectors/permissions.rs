use std::path::{Path, PathBuf};

use super::env_finding;
use crate::diagnostics::health::builder::HealthReportBuilder;
use crate::diagnostics::health::model::{
    HealthCollector, HealthFactCategory, HealthFactSource, HealthFactValue, HealthMessageId,
    HealthSeverity, HealthUnavailableReason,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionsReadiness {
    Ok,
    Unwritable,
}

/// Pure classification for config-directory writability. No I/O.
pub(crate) fn classify_permissions(writable: bool) -> PermissionsReadiness {
    if writable {
        PermissionsReadiness::Ok
    } else {
        PermissionsReadiness::Unwritable
    }
}

fn config_state_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".copilot-shell"))
}

fn dir_writable(path: &Path) -> Result<bool, HealthUnavailableReason> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err(HealthUnavailableReason::Unsupported)
    }
    #[cfg(target_os = "linux")]
    {
        let probe_dir = if path.is_dir() {
            path
        } else if let Some(parent) = path.parent() {
            if parent.is_dir() {
                parent
            } else {
                return Ok(false);
            }
        } else {
            return Ok(false);
        };
        match open_anonymous_probe(probe_dir) {
            Ok(_) => Ok(true),
            Err(error) => match error.raw_os_error() {
                Some(
                    nix::libc::EACCES
                    | nix::libc::EPERM
                    | nix::libc::EROFS
                    | nix::libc::ENOENT
                    | nix::libc::ENOTDIR,
                ) => Ok(false),
                Some(
                    nix::libc::EOPNOTSUPP
                    | nix::libc::EINVAL
                    | nix::libc::ENOSYS
                    | nix::libc::EISDIR,
                ) => Err(HealthUnavailableReason::Unsupported),
                _ => Err(HealthUnavailableReason::ParseError),
            },
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn open_anonymous_probe(directory: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_TMPFILE | nix::libc::O_CLOEXEC)
        .mode(0o600)
        .open(directory)
}

pub(super) fn collect_permissions(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let dir = config_state_dir();
    let (path_str, writable) = match &dir {
        Some(path) => (path.display().to_string(), dir_writable(path)),
        None => (String::new(), Ok(false)),
    };
    let writable = match writable {
        Ok(writable) => writable,
        Err(reason) => {
            builder.add_unavailable(
                HealthCollector::Permissions,
                reason,
                HealthSeverity::Unavailable,
                elapsed_ms,
            );
            return;
        }
    };
    builder
        .add_fact(
            HealthFactCategory::Permissions,
            "permissions.state_dir",
            HealthFactValue::String(path_str.clone()),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_fact(
            HealthFactCategory::Permissions,
            "permissions.state_dir_writable",
            HealthFactValue::Bool(writable),
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        )
        .add_check_done("permissions");

    if classify_permissions(writable) == PermissionsReadiness::Unwritable {
        let mut args = BTreeMap::new();
        args.insert("path".to_string(), path_str);
        builder.add_finding(env_finding(
            "env-permissions",
            HealthMessageId::HealthFindingPermissionsUnwritable,
            HealthMessageId::HealthRemediationPermissions,
            args,
            vec!["permissions.state_dir_writable".to_string()],
        ));
    }
}
