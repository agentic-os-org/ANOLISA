//! Explicit profile installation never contacts Hermes gateways or rewrites credentials.

use super::super::super::{read_file, Result};
use serde_yaml_ng::Value;
use std::{
    collections::BTreeSet,
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) const PLUGIN: &str = "aw-native-hooks";
const MANIFEST: &[u8] = include_bytes!("../../../native/hermes/plugin.yaml");
const CODE: &[u8] = include_bytes!("../../../native/hermes/__init__.py");
const MARKER: &[u8] = b"aw-hermes-plugin/v1alpha1\n";

pub(super) fn profile(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("Hermes --native-profile must be an absolute existing directory".into());
    }
    directory(path)?;
    let path = path.canonicalize()?;
    path.to_str()
        .ok_or("Hermes canonical profile path must be UTF-8")?;
    directory(&path)?;
    // Trusted owners and sticky parents prevent replacement of checked paths.
    // SAFETY: geteuid only reads the current process identity.
    let uid = unsafe { libc::geteuid() };
    for ancestor in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir()
            || (metadata.uid() != uid && metadata.uid() != 0)
            || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0)
        {
            return Err(format!(
                "Hermes profile ancestor permits replacement by others: {}",
                ancestor.display()
            )
            .into());
        }
    }
    if fs::symlink_metadata(path.join(".container-mode")).is_ok() {
        return Err(
            "Hermes managed-container profiles are not supported by the local adapter".into(),
        );
    }
    dotenv(&path.join(".env")).map_err(|error| format!("Hermes profile .env: {error}"))?;
    Ok(path)
}

/// Validate each dotenv input before the native loader can read it.
pub(super) fn dotenv(path: &Path) -> Result<()> {
    let parent = path.parent().ok_or("Hermes dotenv has no parent")?;
    // Sticky permits creating an absent .env, unlike replacing a checked inode.
    if fs::metadata(parent)?.mode() & 0o022 != 0 {
        return Err(format!(
            "Hermes dotenv parent permits modification by others: {}",
            path.display()
        )
        .into());
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {
            read_owned(path)
                .map_err(|error| format!("Hermes dotenv {}: {error}", path.display()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    // SAFETY: geteuid only reads the current process identity.
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "Hermes profile directory must be owned and not writable by others: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn read_owned(path: &Path) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid only reads the current process identity.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(
            "Hermes installation file must be regular, owned and not writable by others".into(),
        );
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Hermes installation file exceeds 4 MiB".into());
    }
    Ok(bytes)
}

pub(super) fn config(profile: &Path) -> Result<Value> {
    parse_config(&read_owned(&profile.join("config.yaml"))?)
}

fn parse_config(bytes: &[u8]) -> Result<Value> {
    fn resolve(value: &mut Value) -> Result<()> {
        // Resolve merge sources first: apply_merge itself visits parents first.
        match value {
            Value::Mapping(mapping) => {
                for child in mapping.values_mut() {
                    resolve(child)?;
                }
            }
            Value::Sequence(sequence) => {
                for child in sequence {
                    resolve(child)?;
                }
            }
            Value::Tagged(tagged) => resolve(&mut tagged.value)?,
            _ => {}
        }
        Ok(value.apply_merge()?)
    }
    let mut value = serde_yaml_ng::from_slice(bytes)?;
    resolve(&mut value)?;
    Ok(value)
}

fn check_plugin(plugin: &Path) -> Result<()> {
    directory(plugin)?;
    let mismatch = |reason: String| {
        format!(
            "refusing to use or overwrite an unusable Hermes aw-native-hooks plugin at {}: {reason}; stop Hermes sessions, move this directory to a backup outside the profile's plugins directory, then rerun aw install",
            plugin.display()
        )
    };
    for (name, expected) in files() {
        let actual = read_owned(&plugin.join(name))
            .map_err(|error| mismatch(format!("cannot read {name}: {error}")))?;
        if actual != expected {
            return Err(mismatch(format!("{name} differs from the bundled version")).into());
        }
    }
    Ok(())
}

pub(super) fn installed(profile: &Path) -> Result<Value> {
    directory(&profile.join("plugins"))?;
    check_plugin(&profile.join("plugins").join(PLUGIN))?;
    let config = config(profile)?;
    let plugins = config
        .get("plugins")
        .and_then(Value::as_mapping)
        .ok_or("Hermes AW plugin is not enabled")?;
    let enabled = names(plugins.get("enabled"))?;
    let disabled = names(plugins.get("disabled"))?;
    if !enabled.iter().any(|name| name == PLUGIN) || disabled.iter().any(|name| name == PLUGIN) {
        return Err("Hermes AW plugin is not enabled".into());
    }
    Ok(config)
}

fn files() -> [(&'static str, &'static [u8]); 3] {
    [
        ("plugin.yaml", MANIFEST),
        ("__init__.py", CODE),
        (".aw-owned", MARKER),
    ]
}

fn names(value: Option<&Value>) -> Result<Vec<String>> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::Sequence(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| "Hermes plugin selections must contain strings".into())
            })
            .collect(),
        _ => Err("Hermes plugin selections must be lists".into()),
    }
}

fn selections(
    bytes: &[u8],
    expand: &mut impl FnMut(&[Vec<String>; 2]) -> Result<[BTreeSet<usize>; 2]>,
) -> Result<Vec<(&'static str, Vec<String>)>> {
    let value = parse_config(bytes)?;
    let object = value
        .as_mapping()
        .ok_or("Hermes configuration must be a mapping")?;
    let empty = serde_yaml_ng::Mapping::new();
    let plugins = match object.get("plugins") {
        None => &empty,
        Some(value) => value
            .as_mapping()
            .ok_or("Hermes plugins must be a mapping")?,
    };
    let raw = [
        names(plugins.get("enabled"))?,
        names(plugins.get("disabled"))?,
    ];
    let effective = if raw.iter().flatten().any(|name| name.contains("${")) {
        expand(&raw)?
    } else {
        std::array::from_fn(|i| {
            raw[i]
                .iter()
                .enumerate()
                .filter_map(|(index, name)| (name == PLUGIN).then_some(index))
                .collect()
        })
    };
    if raw
        .iter()
        .zip(&effective)
        .any(|(raw, matches)| matches.iter().any(|index| *index >= raw.len()))
    {
        return Err("Hermes native expansion returned an invalid plugin selection index".into());
    }
    // Canonicalize AW only; other templates and secret references stay intact.
    let mut enabled: Vec<_> = raw[0]
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            if effective[0].contains(&index) {
                PLUGIN.to_owned()
            } else {
                raw.clone()
            }
        })
        .collect();
    let disabled: Vec<_> = raw[1]
        .iter()
        .enumerate()
        .filter(|(index, _)| !effective[1].contains(index))
        .map(|(_, raw)| raw.clone())
        .collect();
    if !enabled.iter().any(|name| name == PLUGIN) {
        enabled.push(PLUGIN.into());
    }
    let mut updates = Vec::new();
    if disabled != raw[1] {
        updates.push(("plugins.disabled", disabled));
    }
    if enabled != raw[0] {
        updates.push(("plugins.enabled", enabled));
    }
    Ok(updates)
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

struct Staging(PathBuf);

impl Drop for Staging {
    fn drop(&mut self) {
        if self.0.is_dir() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

struct InstallationLock(File);

impl Drop for InstallationLock {
    fn drop(&mut self) {
        // Close alone leaves flock held by an unrelated concurrent fork until exec.
        // SAFETY: the owned file keeps this locked descriptor valid through Drop.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

pub(super) fn install(
    profile: &Path,
    cancelled: &AtomicBool,
    mut expand: impl FnMut(&[Vec<String>; 2]) -> Result<[BTreeSet<usize>; 2]>,
    mut write_native: impl FnMut(&Path, &str, &[String]) -> Result<()>,
) -> Result<Option<PathBuf>> {
    check_cancelled(cancelled)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(profile.join(".aw-install.lock"))?;
    let metadata = lock.metadata()?;
    // SAFETY: geteuid reads identity; flock only applies to the owned descriptor.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o600
        || metadata.nlink() != 1
        || unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0
    {
        return Err("Hermes AW installation lock is unsafe or already held".into());
    }
    let _lock = InstallationLock(lock);
    let config_path = profile.join("config.yaml");
    let original = read_owned(&config_path)?;
    let updates = selections(&original, &mut expand)?;
    let plugins = profile.join("plugins");
    let plugins_exist = match fs::symlink_metadata(&plugins) {
        Ok(_) => {
            directory(&plugins)?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    let destination = plugins.join(PLUGIN);
    let plugin_exists = match fs::symlink_metadata(&destination) {
        Ok(_) => {
            check_plugin(&destination)?;
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.into()),
    };
    // Only Hermes' YAML 1.1 writer can preserve native scalar meanings. Run it
    // against a private candidate; failures never modify the real config.
    let transaction = if updates.is_empty() {
        None
    } else {
        let id = String::from_utf8(read_file("/proc/sys/kernel/random/uuid", 128)?)?;
        let backup = profile.join(format!("config.yaml.aw-backup-{}", id.trim()));
        let staging = Staging(profile.join(format!(".aw-config-{}", id.trim())));
        DirBuilder::new().mode(0o700).create(&staging.0)?;
        write_new(&staging.0.join("config.yaml"), &original)?;
        for (field, names) in updates {
            write_native(&staging.0, field, &names)?;
        }
        let candidate = config(&staging.0)?;
        if !selections(&read_owned(&staging.0.join("config.yaml"))?, &mut expand)?.is_empty()
            || !candidate.is_mapping()
        {
            return Err("Hermes native writer did not enable the AW plugin".into());
        }
        Some((staging, backup))
    };
    if read_owned(&config_path)? != original {
        return Err(
            "Hermes configuration changed during installation; original config was not overwritten"
                .into(),
        );
    }
    check_cancelled(cancelled)?;
    if !plugins_exist {
        DirBuilder::new().mode(0o700).create(&plugins)?;
        File::open(profile)?.sync_all()?;
    }
    if !plugin_exists {
        let id = String::from_utf8(read_file("/proc/sys/kernel/random/uuid", 128)?)?;
        let staged = Staging(plugins.join(format!(".aw-install-{}", id.trim())));
        DirBuilder::new().mode(0o700).create(&staged.0)?;
        for (name, bytes) in files() {
            write_new(&staged.0.join(name), bytes)?;
        }
        File::open(&staged.0)?.sync_all()?;
        let source = std::ffi::CString::new(staged.0.as_os_str().as_bytes())?;
        let destination = std::ffi::CString::new(destination.as_os_str().as_bytes())?;
        check_cancelled(cancelled)?;
        // Native installers do not honor our lock; never replace their directory.
        // SAFETY: both terminated paths are live for the duration of renameat2.
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        File::open(&plugins)?.sync_all()?;
    }
    let Some((staging, backup)) = transaction else {
        return Ok(None);
    };
    let candidate = staging.0.join("config.yaml");
    if read_owned(&config_path)? != original {
        return Err(
            "Hermes configuration changed during installation; original config was not overwritten"
                .into(),
        );
    }
    check_cancelled(cancelled)?;
    write_new(&backup, &original)
        .and_then(|()| publish(&candidate, &config_path, &original, &backup, cancelled))
        .map_err(|error| format!("{error}; review initial backup path {}", backup.display()))?;
    // Success includes durable removal; Drop remains best-effort on earlier errors.
    fs::remove_dir_all(&staging.0)
        .and_then(|()| File::open(profile)?.sync_all())
        .map_err(|error| {
            format!(
                "Hermes config candidate was published, but staging cleanup at {} failed: {error}; review initial backup {} and displaced config {}; no automatic rollback was attempted",
                staging.0.display(), backup.display(), displaced(&backup).display()
            )
        })?;
    Ok(Some(backup))
}

pub(super) fn displaced(backup: &Path) -> PathBuf {
    let mut path = backup.as_os_str().to_os_string();
    path.push(".displaced");
    path.into()
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(aw_exec::Error::Cancelled.into());
    }
    Ok(())
}

pub(super) fn publish(
    candidate: &Path,
    live: &Path,
    original: &[u8],
    backup: &Path,
    cancelled: &AtomicBool,
) -> Result<()> {
    check_cancelled(cancelled)?;
    let displaced = displaced(backup);
    let source = std::ffi::CString::new(displaced.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(live.as_os_str().as_bytes())?;
    fs::hard_link(candidate, &displaced)?;
    if let Err(error) = check_cancelled(cancelled) {
        fs::remove_file(&displaced)?;
        return Err(error);
    }
    // Native writers do not honor our lock. Exchange retains the actual old
    // inode, including writes through already-open descriptors, for recovery.
    // SAFETY: both terminated paths are live for the duration of renameat2.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        fs::remove_file(&displaced)?;
        return Err(error.into());
    }
    let retained = || -> Result<()> {
        File::open(live.parent().ok_or("Hermes config has no parent")?)?.sync_all()?;
        if read_owned(&displaced)? != original {
            return Err("configuration changed at publication".into());
        }
        Ok(())
    };
    retained().map_err(|error| {
        format!(
            "Hermes config candidate was published, but {error}; displaced config retained at {}; review it before retrying or restoring; no automatic rollback was attempted",
            displaced.display()
        )
        .into()
    })
}
