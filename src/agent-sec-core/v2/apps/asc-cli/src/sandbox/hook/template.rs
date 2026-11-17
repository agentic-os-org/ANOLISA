//! Expand the installed Cosh template before the existing permission provider binds paths.

use std::fs::{self, File, OpenOptions};
use std::io::Read as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use asc_os_permission_types::{
    FileRight, FilesystemRule, OsPermissionSet, RuleEffect, Validate as _,
};
use asc_pep_runtime::PermissionInput;
use serde::Deserialize;

use super::super::{MAX_PERMISSION_BYTES, SandboxError};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Template {
    format_version: String,
    permissions: OsPermissionSet,
    #[serde(default)]
    include_tmpdir: bool,
    #[serde(default)]
    read_only_subpaths: Vec<String>,
}

/// Loads one trusted template without publishing or caching its resolved permissions.
pub(super) fn read(
    path: &Path,
    cwd: &Path,
    tmpdir: Option<&str>,
) -> Result<PermissionInput, SandboxError> {
    let result = (|| {
        check_location(path)?;
        let file = OpenOptions::new()
            .read(true)
            .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || !trusted(metadata.uid(), metadata.mode()) {
            return Err(invalid(
                "expected a root-owned regular file without group/other writes",
            ));
        }
        let text = read_bounded(file)?;
        let permissions = expand(&text, cwd, tmpdir)?;
        Ok(PermissionInput::parse(&serde_json::to_string(
            &permissions,
        )?)?)
    })();
    result.map_err(|error: SandboxError| invalid(format!("{}: {error}", path.display())))
}

fn invalid(message: impl Into<String>) -> SandboxError {
    SandboxError::Execution(format!(
        "invalid sandbox permission template: {}",
        message.into()
    ))
}

fn trusted(uid: u32, mode: u32) -> bool {
    uid == 0 && mode & 0o022 == 0
}

fn check_location(path: &Path) -> Result<(), SandboxError> {
    asc_os_permission_types::validate_os_path(path.to_str().ok_or(SandboxError::Environment)?)
        .map_err(|error| invalid(error.to_string()))?;
    for parent in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || !trusted(metadata.uid(), metadata.mode()) {
            return Err(invalid(format!(
                "template directory must be root-owned and not writable by other users: {}",
                parent.display()
            )));
        }
    }
    Ok(())
}

fn read_bounded(file: File) -> Result<String, SandboxError> {
    let mut bytes = Vec::new();
    file.take(MAX_PERMISSION_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PERMISSION_BYTES {
        return Err(SandboxError::TooLarge);
    }
    Ok(std::str::from_utf8(&bytes)?.to_owned())
}

fn expand(text: &str, cwd: &Path, tmpdir: Option<&str>) -> Result<OsPermissionSet, SandboxError> {
    let mut template: Template = serde_json::from_str(text)?;
    if template.format_version != "asc.sandbox.permissions-template.v1" {
        return Err(invalid("unsupported format_version"));
    }
    let cwd = fs::canonicalize(cwd)?;
    if !cwd.is_dir() {
        return Err(invalid("cwd must be a directory"));
    }
    let cwd = cwd.to_str().ok_or(SandboxError::Environment)?;
    for rule in &mut template.permissions.filesystem.rules {
        rule.path = expand_path(&rule.path, cwd)?;
    }
    for path in &mut template.permissions.execution.deny_paths {
        *path = expand_path(path, cwd)?;
    }
    if template.include_tmpdir
        && let Some(path) = tmpdir.filter(|value| Path::new(value).is_absolute())
    {
        let path =
            fs::canonicalize(path).map_err(|error| invalid(format!("TMPDIR {path}: {error}")))?;
        if !path.is_dir() {
            return Err(invalid("TMPDIR must be a directory"));
        }
        add_rule(&mut template.permissions, &path, RuleEffect::Allow)?;
    }
    for name in &template.read_only_subpaths {
        if name.is_empty() || name.contains('/') || matches!(name.as_str(), "." | "..") {
            return Err(invalid(
                "read_only_subpaths requires single path components",
            ));
        }
    }
    let roots: Vec<_> = template
        .permissions
        .filesystem
        .rules
        .iter()
        .filter(|rule| rule.effect == RuleEffect::Allow && rule.rights.contains(&FileRight::Write))
        .map(|rule| rule.path.clone())
        .collect();
    for root in roots {
        let root = fs::canonicalize(root)?;
        if root.is_dir() {
            for name in &template.read_only_subpaths {
                protect_subpath(&mut template.permissions, &root.join(name), name == ".git")?;
            }
        }
    }
    template
        .permissions
        .validate()
        .map_err(|error| invalid(error.to_string()))?;
    Ok(template.permissions)
}

fn add_rule(
    permissions: &mut OsPermissionSet,
    path: &Path,
    effect: RuleEffect,
) -> Result<(), SandboxError> {
    let rule = FilesystemRule {
        effect,
        path: path.to_str().ok_or(SandboxError::Environment)?.into(),
        rights: vec![FileRight::Write],
    };
    if !permissions.filesystem.rules.contains(&rule) {
        permissions.filesystem.rules.push(rule);
    }
    Ok(())
}

fn protect_subpath(
    permissions: &mut OsPermissionSet,
    path: &Path,
    git: bool,
) -> Result<(), SandboxError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "protected path must not contain symlinks: {}",
            path.display()
        )));
    }
    if !metadata.is_dir() && !git {
        return Ok(());
    }
    protect(permissions, path)?;
    if git && !metadata.is_dir() {
        // Nonblocking + fstat prevents a replaced Git pointer from hanging prepare on a FIFO.
        let file = OpenOptions::new()
            .read(true)
            .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("Git pointer must be a regular file"));
        }
        let text = read_bounded(file)?;
        // Git removes line endings, not whitespace belonging to the directory name.
        let target = text
            .trim_end_matches(['\r', '\n'])
            .strip_prefix("gitdir: ")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| invalid("invalid Git pointer: expected gitdir: <path>"))?;
        let target = path.parent().ok_or(SandboxError::Environment)?.join(target);
        protect(permissions, &target)?;
        if !target.is_dir() {
            return Err(invalid("Git pointer target must be a directory"));
        }
    }
    Ok(())
}

fn protect(permissions: &mut OsPermissionSet, path: &Path) -> Result<(), SandboxError> {
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            return Err(invalid(format!(
                "protected path must not contain symlinks: {}",
                ancestor.display()
            )));
        }
    }
    add_rule(permissions, &fs::canonicalize(path)?, RuleEffect::Deny)
}

fn expand_path(path: &str, cwd: &str) -> Result<String, SandboxError> {
    let expanded = match path.strip_prefix("${cwd}") {
        Some("") => cwd.to_owned(),
        Some(suffix) if suffix.starts_with('/') => format!("{}{suffix}", cwd.trim_end_matches('/')),
        _ => path.to_owned(),
    };
    if expanded.contains("${") {
        return Err(invalid("only a leading ${cwd} path component is supported"));
    }
    asc_os_permission_types::validate_os_path(&expanded)
        .map_err(|error| invalid(error.to_string()))?;
    Ok(expanded)
}

#[cfg(test)]
mod tests;
