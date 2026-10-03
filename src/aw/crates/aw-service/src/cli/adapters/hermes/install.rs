//! Explicit profile installation never contacts Hermes gateways or rewrites credentials.

use super::super::super::{read_file, Result};
use serde_yaml_ng::Value;
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::Write,
    os::unix::{
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};

pub(super) const PLUGIN: &str = "aw-native-hooks";
const MANIFEST: &[u8] = include_bytes!("../../../../../../adapters/hermes/plugin.yaml");
const CODE: &[u8] = include_bytes!("../../../../../../adapters/hermes/__init__.py");
const MARKER: &[u8] = b"aw-hermes-plugin/v1alpha1\n";

pub(super) fn profile(path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("Hermes --native-profile must be an absolute existing directory".into());
    }
    directory(path)?;
    Ok(path.canonicalize()?)
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
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err("Hermes installation file must be a regular owned file".into());
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
    Ok(serde_yaml_ng::from_slice(&read_owned(
        &profile.join("config.yaml"),
    )?)?)
}

pub(super) fn installed(profile: &Path) -> Result<Value> {
    let plugin = profile.join("plugins").join(PLUGIN);
    directory(&profile.join("plugins"))?;
    directory(&plugin)?;
    for (name, expected) in files() {
        if read_owned(&plugin.join(name))? != expected {
            return Err(
                "Hermes AW plugin differs from this binary; reinstall matching files explicitly"
                    .into(),
            );
        }
    }
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

fn selections(bytes: &[u8]) -> Result<Vec<(&'static str, Vec<String>)>> {
    let value: Value = serde_yaml_ng::from_slice(bytes)?;
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
    let mut enabled = names(plugins.get("enabled"))?;
    let mut disabled = names(plugins.get("disabled"))?;
    let mut updates = Vec::new();
    if disabled.iter().any(|name| name == PLUGIN) {
        disabled.retain(|name| name != PLUGIN);
        updates.push(("plugins.disabled", disabled));
    }
    if !enabled.iter().any(|name| name == PLUGIN) {
        enabled.push(PLUGIN.into());
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

pub(super) fn install(
    profile: &Path,
    mut write_native: impl FnMut(&Path, &str, &[String]) -> Result<()>,
) -> Result<Option<PathBuf>> {
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
    let config_path = profile.join("config.yaml");
    let original = read_owned(&config_path)?;
    let updates = selections(&original)?;
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
            directory(&destination)?;
            for (name, expected) in files() {
                if read_owned(&destination.join(name))? != expected {
                    return Err(
                        "refusing to overwrite a different Hermes aw-native-hooks plugin".into(),
                    );
                }
            }
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
        write_new(&backup, &original)?;
        let staging = Staging(profile.join(format!(".aw-config-{}", id.trim())));
        DirBuilder::new().mode(0o700).create(&staging.0)?;
        write_new(&staging.0.join("config.yaml"), &original)?;
        for (field, names) in updates {
            write_native(&staging.0, field, &names)?;
        }
        let candidate = config(&staging.0)?;
        if !selections(&read_owned(&staging.0.join("config.yaml"))?)?.is_empty()
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
    if !plugins_exist {
        DirBuilder::new().mode(0o700).create(&plugins)?;
    }
    if !plugin_exists {
        let id = String::from_utf8(read_file("/proc/sys/kernel/random/uuid", 128)?)?;
        let staged = Staging(plugins.join(format!(".aw-install-{}", id.trim())));
        DirBuilder::new().mode(0o700).create(&staged.0)?;
        for (name, bytes) in files() {
            write_new(&staged.0.join(name), bytes)?;
        }
        File::open(&staged.0)?.sync_all()?;
        fs::rename(&staged.0, &destination)?;
        File::open(&plugins)?.sync_all()?;
    }
    let Some((staging, backup)) = transaction else {
        return Ok(None);
    };
    let candidate = staging.0.join("config.yaml");
    if read_owned(&config_path)? != original {
        return Err(format!(
            "Hermes configuration changed during installation; retry after reviewing backup {}",
            backup.display()
        )
        .into());
    }
    fs::rename(&candidate, &config_path)?;
    File::open(profile)?.sync_all()?;
    Ok(Some(backup))
}
