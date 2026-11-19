//! Provider-owned workspace bindings and private snapshot switches.

use std::{
    collections::BTreeMap,
    ffi::{CStr, OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::json;

const LIMIT: usize = 64 * 1024;
pub(super) const TARGET_ENV: &str = "WS_CKPT_AW_TARGET";

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    #[serde(default)]
    bindings: BTreeMap<String, String>,
    workspaces: BTreeMap<String, BTreeMap<String, Switch>>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Switch {
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_result: Option<String>,
}

impl Policy {
    pub(super) fn workspace(&self, target: &str) -> Option<&str> {
        self.bindings
            .get(target)
            .map(String::as_str)
            .filter(|path| valid_workspace(path))
    }

    fn entry(&self, workspace: &str, target: &str) -> Option<&Switch> {
        self.workspaces.get(workspace)?.get(target)
    }

    pub(super) fn enabled(&self, workspace: &str, target: &str) -> bool {
        self.entry(workspace, target)
            .is_some_and(|entry| entry.enabled)
    }
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn valid_workspace(path: &str) -> bool {
    Path::new(path).is_absolute() && !path.contains('\0')
}

/// Resolve the same policy path for Hooks and Skill commands even if their environments differ.
pub(super) fn account_policy_path() -> io::Result<PathBuf> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut found = std::ptr::null_mut();
    // SAFETY: getpwuid_r writes to the supplied record, buffer and result pointer.
    let status = unsafe {
        libc::getpwuid_r(
            libc::geteuid(),
            record.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut found,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status));
    }
    if found.is_null() {
        return Err(invalid("cannot resolve the Provider account"));
    }
    // SAFETY: the successful lookup points into the live record and lookup buffer.
    let home = unsafe {
        let directory = (*found).pw_dir;
        if directory.is_null() {
            return Err(invalid("Provider account has no home directory"));
        }
        CStr::from_ptr(directory)
    };
    let home = Path::new(OsStr::from_bytes(home.to_bytes()));
    if !home.is_absolute() {
        return Err(invalid(
            "expected an absolute Provider account home directory",
        ));
    }
    Ok(home.join(".local/state/ws-ckpt/aw-policy.json"))
}

fn private_file(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    // SAFETY: geteuid reads process identity without dereferencing pointers.
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(invalid("expected an account-owned private file"));
    }
    Ok(())
}

fn load<T: for<'de> Deserialize<'de>>(file: File) -> io::Result<T> {
    private_file(&file)?;
    let mut bytes = Vec::new();
    file.take((LIMIT + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > LIMIT {
        return Err(invalid("configuration file exceeds size limit"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

fn open(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
}

pub(super) fn read(path: &Path) -> io::Result<Policy> {
    match open(path) {
        Ok(file) => {
            require_private_directory(policy_directory(path)?)?;
            load(file)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Policy::default()),
        Err(err) => Err(err),
    }
}

pub(super) fn bind(path: &Path, target: &str, workspace: &str) -> io::Result<()> {
    if target.is_empty() || !valid_workspace(workspace) {
        return Err(invalid("expected a target and an absolute workspace path"));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(policy_directory(path)?)?;
    update(path, false, |policy| {
        if policy.workspace(target) == Some(workspace) {
            return Ok(false);
        }
        policy
            .bindings
            .insert(target.to_owned(), workspace.to_owned());
        Ok(true)
    })
}

pub(super) fn set_enabled(
    path: &Path,
    workspace: &str,
    target: &str,
    enabled: bool,
) -> io::Result<()> {
    update(path, false, |policy| {
        if policy.workspace(target) != Some(workspace) {
            return Err(invalid("workspace binding changed; check status and retry"));
        }
        policy
            .workspaces
            .entry(workspace.to_owned())
            .or_default()
            .entry(target.to_owned())
            .or_default()
            .enabled = enabled;
        Ok(true)
    })
}

pub(super) fn record(path: &Path, workspace: &str, target: &str, result: &str) -> io::Result<()> {
    update(path, true, |policy| {
        let Some(entry) = policy
            .workspaces
            .get_mut(workspace)
            .and_then(|targets| targets.get_mut(target))
        else {
            return Ok(false);
        };
        entry.last_result = Some(result.to_owned());
        Ok(true)
    })
}

fn policy_directory(path: &Path) -> io::Result<&Path> {
    path.parent()
        .ok_or_else(|| invalid("expected policy directory"))
}

fn require_private_directory(path: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    // SAFETY: geteuid reads process identity without dereferencing pointers.
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
        return Err(invalid(
            "expected an account-owned private policy directory",
        ));
    }
    Ok(())
}

fn update(
    path: &Path,
    try_only: bool,
    change: impl FnOnce(&mut Policy) -> io::Result<bool>,
) -> io::Result<()> {
    let parent = policy_directory(path)?;
    require_private_directory(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("expected policy filename"))?;
    let sibling = |suffix: &str| {
        let mut value = name.to_os_string();
        value.push(suffix);
        parent.join(value)
    };
    let lock = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(sibling(".lock"))?;
    private_file(&lock)?;
    if try_only {
        lock.try_lock().map_err(io::Error::from)?;
    } else {
        lock.lock()?;
    }
    let mut policy = read(path)?;
    if !change(&mut policy)? {
        return Ok(());
    }
    let bytes = serde_json::to_vec_pretty(&policy).map_err(io::Error::other)?;
    if bytes.len() > LIMIT {
        return Err(invalid("policy file exceeds size limit"));
    }
    let nonce = fs::read_to_string("/proc/sys/kernel/random/uuid")?;
    let temporary = sibling(&format!(".{}.tmp", nonce.trim()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub(super) fn control(args: &[OsString]) -> io::Result<()> {
    let target = std::env::var(TARGET_ENV)
        .ok()
        .filter(|target| !target.is_empty())
        .ok_or_else(|| invalid("missing AW target; configure WS_CKPT_AW_TARGET"))?;
    let path = account_policy_path()?;
    match args {
        [action] if action == "status" => {}
        [action, flag, workspace]
            if matches!(action.to_str(), Some("enable" | "disable")) && flag == "--workspace" =>
        {
            let workspace = workspace
                .to_str()
                .filter(|path| valid_workspace(path))
                .ok_or_else(|| invalid("expected an absolute workspace path"))?;
            set_enabled(&path, workspace, &target, action == "enable")?;
        }
        _ => {
            return Err(invalid(
                "expected policy status or policy enable|disable --workspace PATH",
            ))
        }
    }
    let policy = read(&path)?;
    let workspace = policy.workspace(&target).ok_or_else(|| {
        invalid("no workspace for this target; explicitly bind a workspace first")
    })?;
    let entry = policy.entry(workspace, &target);
    let enabled = entry.is_some_and(|entry| entry.enabled);
    let last_result = entry.and_then(|entry| entry.last_result.as_deref());
    println!(
        "{}",
        json!({"workspace":workspace,"target":target,"enabled":enabled,"last_result":last_result})
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebinding_preserves_saved_switches_and_rejects_a_stale_write() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("state");
        fs::DirBuilder::new().mode(0o700).create(&private).unwrap();
        let file = private.join("policy.json");
        let mut legacy = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file)
            .unwrap();
        legacy
            .write_all(br#"{"workspaces":{"/first":{"qoder":{"enabled":true}}}}"#)
            .unwrap();

        assert!(read(&file).unwrap().workspace("qoder").is_none());
        bind(&file, "qoder", "/first").unwrap();
        bind(&file, "qoder", "/second").unwrap();
        assert!(set_enabled(&file, "/first", "qoder", false).is_err());
        let policy = read(&file).unwrap();
        assert_eq!(policy.workspace("qoder"), Some("/second"));
        assert!(policy.enabled("/first", "qoder"));
        assert!(!policy.enabled("/second", "qoder"));
    }
}
