//! Install stable, reviewable hook definitions without granting native trust.

use super::{hook_command, native_event};
use crate::Result;
use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(super) fn read_owned(path: &Path) -> Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let stat = file.metadata()?;
    // SAFETY: geteuid only reads the current process identity.
    if !stat.is_file() || stat.uid() != unsafe { libc::geteuid() } || stat.mode() & 0o022 != 0 {
        return Err(
            "Codex integration files must be owned regular files, not writable by others".into(),
        );
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err("Codex integration file exceeds one MiB".into());
    }
    Ok(bytes)
}

pub(super) fn install() -> Result<()> {
    let directory = match std::env::var_os("CODEX_HOME") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?).join(".codex"),
    };
    if !directory.is_absolute() {
        return Err("Codex home must be absolute".into());
    }
    match fs::symlink_metadata(&directory) {
        Ok(stat) => {
            // SAFETY: geteuid only reads the current process identity.
            if !stat.is_dir()
                || stat.uid() != unsafe { libc::geteuid() }
                || stat.mode() & 0o022 != 0
            {
                return Err("Codex home must be an owned directory, not writable by others".into());
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(&directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    let path = directory.join("hooks.json");
    let original = match fs::symlink_metadata(&path) {
        Ok(_) => Some(read_owned(&path)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let mut value = match &original {
        Some(bytes) => serde_json::from_slice::<Value>(bytes)?,
        None => json!({"hooks":{}}),
    };
    let executable = std::env::current_exe()?;
    let mut changed = false;
    let hooks = value
        .as_object_mut()
        .ok_or("Codex hooks file must be an object")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid Codex hooks")?;
    for event in ["tool.before", "tool.after"] {
        let command = hook_command(&executable, event)?;
        let expected =
            json!({"matcher":".*","hooks":[{"type":"command","command":command,"timeout":60}]});
        let groups = hooks
            .entry(native_event(event)?)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("Codex hook groups must be arrays")?;
        let matching: Vec<_> = groups
            .iter()
            .filter(|group| {
                group["hooks"]
                    .as_array()
                    .is_some_and(|handlers| handlers.iter().any(|h| h["command"] == command))
            })
            .collect();
        if matching.is_empty() {
            groups.push(expected);
            changed = true;
        } else if matching.len() != 1 || matching[0] != &expected {
            return Err("existing AW Codex hook differs; review it before reinstalling".into());
        }
    }
    if changed {
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        serde_json::to_writer_pretty(&mut file, &value)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        if let Some(original) = original {
            if read_owned(&path)? != original {
                return Err("Codex hooks changed during installation".into());
            }
            file.persist(&path).map_err(|error| error.error)?;
        } else {
            file.persist_noclobber(&path).map_err(|error| error.error)?;
        }
        fs::File::open(&directory)?.sync_all()?;
    }
    println!("AW hooks installed at {}. Open Codex /hooks and review both AW command hooks. Installation does not grant trust.", path.display());
    Ok(())
}
