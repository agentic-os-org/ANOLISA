use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::sync::Arc;

#[cfg(unix)]
use nix::dir::{Dir, Type};
#[cfg(unix)]
use nix::fcntl::{open, openat, OFlag};
#[cfg(unix)]
use nix::sys::stat::Mode;

use crate::hooks::model::{HookMatcher, HookTrigger};

use super::{ExternalHookConfig, ExternalHookSource};

pub(super) struct LoadedExternalHookConfig {
    pub config: ExternalHookConfig,
    #[cfg(unix)]
    pub executable: Arc<fs::File>,
}

#[cfg(unix)]
pub(super) fn reserve_hook_descriptor_headroom(count: usize) -> Option<Vec<fs::File>> {
    (0..count)
        .map(|_| fs::File::open("/dev/null"))
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

pub(super) fn load_external_hook_configs(
    dir: &Path,
    source: ExternalHookSource,
    project_root: Option<PathBuf>,
    trusted: bool,
    max_hooks: usize,
) -> Vec<LoadedExternalHookConfig> {
    #[cfg(unix)]
    {
        load_external_hook_configs_unix(dir, source, project_root, trusted, max_hooks)
    }

    #[cfg(not(unix))]
    {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return Vec::new(),
        };

        let mut configs = Vec::new();
        for entry in entries.flatten() {
            if configs.len() >= max_hooks {
                break;
            }
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if let Some(mut config) = parse_hook_header(&path) {
                config.source = source.clone();
                config.project_root = project_root.clone();
                config.trusted = trusted;
                configs.push(LoadedExternalHookConfig { config });
            }
        }
        configs
    }
}

pub(super) fn load_project_external_hook_configs(
    project_root: &Path,
    trusted: bool,
    max_hooks: usize,
) -> Vec<LoadedExternalHookConfig> {
    #[cfg(unix)]
    {
        load_project_external_hook_configs_unix(project_root, trusted, max_hooks)
    }

    #[cfg(not(unix))]
    {
        load_external_hook_configs(
            &project_root.join(".cosh/hooks"),
            ExternalHookSource::Project,
            Some(project_root.to_path_buf()),
            trusted,
            max_hooks,
        )
    }
}

pub(super) fn parse_hook_header(path: &Path) -> Option<ExternalHookConfig> {
    #[cfg(unix)]
    let mut file = open_hook_file(path)?;
    #[cfg(not(unix))]
    let mut file = {
        let metadata = fs::symlink_metadata(path).ok()?;
        if !metadata.file_type().is_file() {
            return None;
        }
        fs::File::open(path).ok()?
    };

    let mut content = String::new();
    file.read_to_string(&mut content).ok()?;
    parse_hook_header_content(&content, path)
}

fn parse_hook_header_content(content: &str, path: &Path) -> Option<ExternalHookConfig> {
    let mut hook_id: Option<String> = None;
    let mut match_commands: Vec<String> = Vec::new();
    let mut trigger = HookTrigger::OnComplete;
    let mut timeout_ms: u64 = 5000;

    for line in content.lines().take(10) {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("# cosh-hook:") {
            hook_id = Some(val.trim().to_string());
        } else if let Some(val) = line.strip_prefix("# match-commands:") {
            match_commands = val.split(',').map(|s| s.trim().to_string()).collect();
        } else if let Some(val) = line.strip_prefix("# trigger:") {
            trigger = match val.trim() {
                "on_fail" => HookTrigger::OnFail,
                "on_success" => HookTrigger::OnSuccess,
                _ => HookTrigger::OnComplete,
            };
        } else if let Some(val) = line.strip_prefix("# timeout:") {
            timeout_ms = parse_timeout(val.trim());
        }
    }

    let id = hook_id?;
    Some(ExternalHookConfig {
        path: path.to_path_buf(),
        matcher: HookMatcher {
            id,
            commands: match_commands,
            command_patterns: Vec::new(),
            command_regex: None,
            min_output_bytes: None,
            exit_codes: None,
            trigger,
        },
        timeout_ms,
        source: ExternalHookSource::User,
        project_root: None,
        trusted: true,
    })
}

#[cfg(unix)]
fn open_hook_file(path: &Path) -> Option<fs::File> {
    let fd = open(
        path,
        OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
        Mode::empty(),
    )
    .ok()?;
    // `open` transfers ownership of the descriptor to this File. Keeping the
    // descriptor open through parsing prevents a path replacement from
    // changing the bytes that are inspected.
    let file = unsafe { fs::File::from_raw_fd(fd) };
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some(file)
}

#[cfg(unix)]
pub(super) fn open_hook_executable(path: &Path) -> Option<fs::File> {
    let file = open_hook_file(path)?;
    let metadata = file.metadata().ok()?;
    (metadata.permissions().mode() & 0o111 != 0).then_some(file)
}

#[cfg(unix)]
fn load_external_hook_configs_unix(
    dir: &Path,
    source: ExternalHookSource,
    project_root: Option<PathBuf>,
    trusted: bool,
    max_hooks: usize,
) -> Vec<LoadedExternalHookConfig> {
    let Ok(directory) = Dir::open(
        dir,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) else {
        return Vec::new();
    };

    load_external_hook_configs_from_dir(directory, dir, source, project_root, trusted, max_hooks)
}

#[cfg(unix)]
fn load_project_external_hook_configs_unix(
    project_root: &Path,
    trusted: bool,
    max_hooks: usize,
) -> Vec<LoadedExternalHookConfig> {
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let Ok(root) = Dir::open(project_root, flags, Mode::empty()) else {
        return Vec::new();
    };
    let Ok(cosh_dir) = Dir::openat(
        Some(root.as_raw_fd()),
        OsStr::new(".cosh"),
        flags,
        Mode::empty(),
    ) else {
        return Vec::new();
    };
    let Ok(hooks_dir) = Dir::openat(
        Some(cosh_dir.as_raw_fd()),
        OsStr::new("hooks"),
        flags,
        Mode::empty(),
    ) else {
        return Vec::new();
    };
    let display_dir = project_root.join(".cosh/hooks");
    load_external_hook_configs_from_dir(
        hooks_dir,
        &display_dir,
        ExternalHookSource::Project,
        Some(project_root.to_path_buf()),
        trusted,
        max_hooks,
    )
}

#[cfg(unix)]
fn load_external_hook_configs_from_dir(
    mut directory: Dir,
    display_dir: &Path,
    source: ExternalHookSource,
    project_root: Option<PathBuf>,
    trusted: bool,
    max_hooks: usize,
) -> Vec<LoadedExternalHookConfig> {
    let directory_fd = directory.as_raw_fd();
    let mut configs = Vec::new();
    for entry in directory.iter().flatten() {
        if configs.len() >= max_hooks {
            break;
        }
        let name = entry.file_name();
        let name_bytes = name.to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }
        // Reject the cheap directory-entry symlink signal before opening. The
        // O_NOFOLLOW flag below remains authoritative for filesystems that do
        // not expose d_type or race a replacement after readdir.
        if entry.file_type() == Some(Type::Symlink) {
            continue;
        }

        let Ok(fd) = openat(
            Some(directory_fd),
            name,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        ) else {
            continue;
        };
        // `openat` transfers ownership of the descriptor to this File. All
        // metadata checks and reads therefore refer to one stable object.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        let Ok(metadata) = file.metadata() else {
            continue;
        };
        if !metadata.is_file()
            || (entry.ino() != 0 && metadata.ino() != entry.ino())
            || metadata.permissions().mode() & 0o111 == 0
        {
            continue;
        }

        let mut content = String::new();
        if file.read_to_string(&mut content).is_err() {
            continue;
        }
        let path = display_dir.join(OsStr::from_bytes(name_bytes));
        if let Some(mut config) = parse_hook_header_content(&content, &path) {
            config.source = source.clone();
            config.project_root = project_root.clone();
            config.trusted = trusted;
            configs.push(LoadedExternalHookConfig {
                config,
                executable: Arc::new(file),
            });
        }
    }
    configs
}

pub(super) fn parse_timeout(s: &str) -> u64 {
    if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse::<u64>().unwrap_or(5000)
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.trim().parse::<u64>().unwrap_or(5) * 1000
    } else {
        s.parse::<u64>().unwrap_or(5000)
    }
}
