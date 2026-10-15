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
) -> Result<Vec<LoadedExternalHookConfig>, ()> {
    #[cfg(unix)]
    {
        load_external_hook_configs_unix(dir, source, project_root, trusted, max_hooks)
    }

    #[cfg(not(unix))]
    {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(Vec::new()),
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
        Ok(configs)
    }
}

pub(super) fn load_project_external_hook_configs(
    project_root: &Path,
    trusted: bool,
    max_hooks: usize,
) -> Result<Vec<LoadedExternalHookConfig>, ()> {
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
) -> Result<Vec<LoadedExternalHookConfig>, ()> {
    let Ok(directory) = Dir::open(
        dir,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) else {
        return Ok(Vec::new());
    };

    load_external_hook_configs_from_dir(directory, dir, source, project_root, trusted, max_hooks)
}

#[cfg(unix)]
fn load_project_external_hook_configs_unix(
    project_root: &Path,
    trusted: bool,
    max_hooks: usize,
) -> Result<Vec<LoadedExternalHookConfig>, ()> {
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let Ok(root) = Dir::open(project_root, flags, Mode::empty()) else {
        return Ok(Vec::new());
    };
    let Ok(cosh_dir) = Dir::openat(
        Some(root.as_raw_fd()),
        OsStr::new(".cosh"),
        flags,
        Mode::empty(),
    ) else {
        return Ok(Vec::new());
    };
    let Ok(hooks_dir) = Dir::openat(
        Some(cosh_dir.as_raw_fd()),
        OsStr::new("hooks"),
        flags,
        Mode::empty(),
    ) else {
        return Ok(Vec::new());
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
) -> Result<Vec<LoadedExternalHookConfig>, ()> {
    let directory_fd = directory.as_raw_fd();
    let mut configs = Vec::new();
    let mut candidates = 0;
    let mut remaining = body_byte_limit(&source);
    for entry in directory.iter().flatten() {
        let name = entry.file_name();
        let name_bytes = name.to_bytes();
        if name_bytes == b"." || name_bytes == b".." {
            continue;
        }
        if source == ExternalHookSource::Project {
            candidates += 1;
            if candidates > super::MAX_EXTERNAL_HOOKS || configs.len() >= max_hooks {
                return Err(());
            }
        } else if configs.len() >= max_hooks {
            break;
        }
        // Reject the cheap directory-entry type signal before opening: only a
        // regular file can be a hook, and opening anything else first is what let
        // a FIFO in a project's `.cosh/hooks` block the open until a writer
        // appeared. `d_type` is not guaranteed, so `O_NONBLOCK` plus the `fstat`
        // below remain authoritative for filesystems that omit it or race a
        // replacement after readdir; the same is true of `O_NOFOLLOW` for
        // symlinks.
        if matches!(entry.file_type(), Some(kind) if kind != Type::File) {
            continue;
        }

        let Ok(fd) = openat(
            Some(directory_fd),
            name,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK,
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

        if remaining.is_some_and(|bytes| metadata.len() > bytes) {
            return Err(());
        }
        let mut content = String::new();
        if read_hook_body(&mut file, &mut content, &mut remaining).is_err() {
            if source == ExternalHookSource::Project {
                return Err(());
            }
            continue;
        }
        let path = display_dir.join(OsStr::from_bytes(name_bytes));
        if let Some(mut config) = parse_hook_header_content(&content, &path) {
            config.source = source.clone();
            config.project_root = project_root.clone();
            config.trusted = trusted;
            let Some(executable) = frozen_executable(&source, content.as_bytes(), file) else {
                continue;
            };
            configs.push(LoadedExternalHookConfig { config, executable });
        }
    }
    Ok(configs)
}

fn read_hook_body(
    file: &mut fs::File,
    content: &mut String,
    remaining: &mut Option<u64>,
) -> std::io::Result<()> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8 * 1024];
    loop {
        let capacity = remaining.map_or(chunk.len(), |limit| {
            limit.saturating_add(1).min(chunk.len() as u64) as usize
        });
        let read = file.read(&mut chunk[..capacity])?;
        if read == 0 {
            break;
        }
        if let Some(limit) = remaining {
            *limit = limit.checked_sub(read as u64).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "hook tree exceeds size limit",
                )
            })?;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    *content = String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "hook is not valid UTF-8")
    })?;
    Ok(())
}

// User hooks keep their existing behavior; all inspected project bodies,
// including invalid headers, share one budget before parsing or sealing.
fn body_byte_limit(source: &ExternalHookSource) -> Option<u64> {
    match source {
        ExternalHookSource::Project => Some(super::MAX_PROJECT_HOOK_FINGERPRINT_BYTES),
        ExternalHookSource::User => None,
    }
}

/// Freezes the verified bytes of a project hook so the fingerprint and the
/// execution refer to one immutable object.
///
/// A pinned descriptor fixes the inode, not the content: the project can truncate
/// and rewrite the same file after its fingerprint matched, and the executor reads
/// the new bytes through that descriptor with no re-check. Only project hooks are
/// frozen — they are the trust boundary — and a hook that cannot be frozen is not
/// registered at all, because running it would execute bytes nobody verified.
#[cfg(target_os = "linux")]
fn frozen_executable(
    source: &ExternalHookSource,
    content: &[u8],
    original: fs::File,
) -> Option<Arc<fs::File>> {
    if *source == ExternalHookSource::Project {
        return Some(Arc::new(sealed_snapshot(content)?));
    }
    Some(Arc::new(original))
}

/// Non-Linux platforms have no memfd, so the bytes cannot be frozen. Hook
/// execution there already depends on `/proc/self/fd`, which they do not provide
/// either, so this keeps the previous behaviour rather than claiming a second
/// guarantee that cannot be tested where it would run.
#[cfg(not(target_os = "linux"))]
fn frozen_executable(
    _source: &ExternalHookSource,
    _content: &[u8],
    original: fs::File,
) -> Option<Arc<fs::File>> {
    Some(Arc::new(original))
}

/// Copies hook bytes into a sealed anonymous file and returns it read-execute only.
///
/// `F_SEAL_WRITE` plus `F_SEAL_SHRINK`/`F_SEAL_GROW` make the snapshot immutable at
/// the kernel level, so not even its owner can change what was approved, and being
/// anonymous it has no path a project could reach. Sealing happens after the write
/// and after the mode change, since `F_SEAL_WRITE` would reject neither but a later
/// write would be refused.
#[cfg(target_os = "linux")]
fn sealed_snapshot(content: &[u8]) -> Option<fs::File> {
    use nix::fcntl::{fcntl, FcntlArg, SealFlag};
    use nix::sys::memfd::{memfd_create, MemFdCreateFlag};
    use std::io::Write;
    use std::os::fd::AsRawFd;

    let owned = memfd_create(
        c"cosh-project-hook",
        MemFdCreateFlag::MFD_CLOEXEC | MemFdCreateFlag::MFD_ALLOW_SEALING,
    )
    .ok()?;
    // Wrapping the owned descriptor makes the File close it on drop, which is the
    // only lifetime an anonymous snapshot needs.
    let mut file = fs::File::from(owned);
    file.write_all(content).ok()?;
    file.set_permissions(fs::Permissions::from_mode(0o500))
        .ok()?;
    fcntl(
        file.as_raw_fd(),
        FcntlArg::F_ADD_SEALS(
            SealFlag::F_SEAL_WRITE | SealFlag::F_SEAL_SHRINK | SealFlag::F_SEAL_GROW,
        ),
    )
    .ok()?;
    Some(file)
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
