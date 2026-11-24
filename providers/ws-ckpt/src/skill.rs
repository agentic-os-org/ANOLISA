//! Installs the existing ws-ckpt Skill on input submission.

use std::{
    collections::BTreeMap,
    ffi::CString,
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Component, Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;

use super::Result;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Installation {
    source: PathBuf,
    destination: PathBuf,
}

pub(super) fn configured(config: &Value) -> Result<BTreeMap<String, Installation>> {
    let Some(skills) = config.get("skills") else {
        return Ok(BTreeMap::new());
    };
    let installs: BTreeMap<String, Installation> = serde_json::from_value(skills.clone())?;
    for (target, install) in &installs {
        if target.is_empty()
            || !install.source.is_absolute()
            || !valid_destination(&install.destination)
        {
            return Err(format!("invalid Skill paths for AW target {target:?}").into());
        }
    }
    Ok(installs)
}

impl Installation {
    pub(super) fn apply(&self, cwd: &Path) -> Result<bool> {
        install(&self.source, &project_destination(cwd, &self.destination)?)
    }
}

fn valid_destination(relative: &Path) -> bool {
    relative.components().count() >= 2
        && relative
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}

fn project_destination(project: &Path, relative: &Path) -> Result<PathBuf> {
    if !valid_destination(relative) {
        return Err(
            "Skill destination must be a relative project path without parent components".into(),
        );
    }
    let parts: Vec<_> = relative.components().collect();
    let mut directory = project.to_owned();
    for part in &parts[..parts.len() - 1] {
        directory.push(part.as_os_str());
        match fs::create_dir(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        if !fs::symlink_metadata(&directory)?.is_dir() {
            return Err("Skill destination contains a non-directory or symbolic link".into());
        }
    }
    Ok(directory.join(parts[parts.len() - 1].as_os_str()))
}

fn install(source: &Path, destination: &Path) -> Result<bool> {
    match fs::symlink_metadata(destination) {
        Ok(_) => {
            check_existing(source, destination)?;
            return Ok(false);
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    if !fs::symlink_metadata(source)?.is_dir()
        || !fs::symlink_metadata(source.join("SKILL.md"))?.is_file()
    {
        return Err("Skill source must contain a regular SKILL.md".into());
    }
    let parent = destination
        .parent()
        .ok_or("Skill destination has no parent")?;
    let staging = tempfile::Builder::new()
        .prefix(".aw-skill-")
        .tempdir_in(parent)?;
    copy_tree(source, staging.path())?;
    let from = CString::new(staging.path().as_os_str().as_bytes())?;
    let to = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: both paths are live C strings; RENAME_NOREPLACE preserves concurrent installs.
    if unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    } != 0
    {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::AlreadyExists {
            return Err(error.into());
        }
        check_existing(source, destination)?;
        return Ok(false);
    }
    Ok(true)
}

fn require_existing(path: &Path, directory: bool) -> Result<()> {
    let expected = if directory {
        "directory"
    } else {
        "regular file"
    };
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "existing Skill requires {expected} {}: {error}",
            path.display()
        )
    })?;
    if if directory {
        !metadata.is_dir()
    } else {
        !metadata.is_file()
    } {
        return Err(format!("existing Skill requires {expected} {}", path.display()).into());
    }
    Ok(())
}

fn check_existing(source: &Path, destination: &Path) -> Result<()> {
    require_existing(destination, true)?;
    let manifest = destination.join("SKILL.md");
    require_existing(&manifest, false)?;
    if fs::read(&manifest)? != fs::read(source.join("SKILL.md"))? {
        return Ok(());
    }
    let scripts = source.join("scripts");
    if scripts.try_exists()? {
        check_scripts(&scripts, &destination.join("scripts"))?;
    }
    Ok(())
}

fn check_scripts(source: &Path, destination: &Path) -> Result<()> {
    require_existing(destination, true)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            check_scripts(&entry.path(), &target)?;
        } else {
            require_existing(&target, false)?;
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            fs::create_dir(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), target)?;
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Skill source contains a symbolic link or special file",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_reused_skill_without_replacing_user_files() -> Result<()> {
        let root = tempfile::tempdir()?;
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        fs::create_dir_all(source.join("scripts"))?;
        fs::write(source.join("SKILL.md"), "default")?;
        fs::write(source.join("scripts/control.py"), "source control script")?;
        fs::create_dir(&destination)?;

        let error = install(&source, &destination).unwrap_err().to_string();
        assert!(error.contains(destination.join("SKILL.md").to_str().unwrap()));

        fs::write(destination.join("SKILL.md"), "default")?;
        fs::create_dir(destination.join("scripts"))?;
        let script = destination.join("scripts/control.py");
        let error = install(&source, &destination).unwrap_err().to_string();
        assert!(error.contains(script.to_str().unwrap()));
        assert!(!script.exists());

        fs::write(&script, "user control script")?;
        install(&source, &destination)?;
        assert_eq!(fs::read_to_string(&script)?, "user control script");

        fs::remove_file(script)?;
        fs::write(destination.join("SKILL.md"), "custom Skill")?;
        install(&source, &destination)?;
        assert_eq!(
            fs::read_to_string(destination.join("SKILL.md"))?,
            "custom Skill"
        );
        Ok(())
    }
}
