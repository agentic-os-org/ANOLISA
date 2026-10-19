//! Inspect effective Git configuration without running status or external helpers.

use super::{checked_native_name, trusted_native_path, Result};
use std::{ffi::OsString, os::unix::ffi::OsStrExt, time::Duration};

/// Run a bounded Git probe in its native directory.
pub(super) fn inspect(
    command: &aw_exec::CommandSpec,
    args: &[&str],
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Vec<u8>> {
    let mut probe = command.clone();
    probe.args = ["--no-replace-objects", "--no-optional-locks"]
        .into_iter()
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let output = aw_exec::run(
        &probe,
        &[],
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 4 * 1024 * 1024,
            stderr_bytes: 4096,
        },
        std::time::Instant::now() + Duration::from_secs(5),
        cancelled,
    )?;
    if !output.status.success() {
        return Err(format!(
            "Hermes Git inspection failed in {}: {}",
            command.cwd.display(),
            args.join(" ")
        )
        .into());
    }
    Ok(output.stdout)
}

/// Protect repository locators, configuration and the automatic index Hook.
fn checked_repository<'a>(
    identity: &'a aw_exec::CommandSpec,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<Option<&'a std::path::Path>> {
    let source = &identity.cwd;
    checked_native_name(&source.join(".git"))?;
    // Match Git's ceiling normalization, including its empty-entry shortcut.
    let mut resolve = true;
    let ceilings: Vec<_> = identity
        .environment
        .get(&OsString::from("GIT_CEILING_DIRECTORIES"))
        .into_iter()
        .flat_map(std::env::split_paths)
        .filter_map(|path| {
            if path.as_os_str().is_empty() {
                resolve = false;
                None
            } else if !path.is_absolute() {
                None
            } else if resolve {
                path.canonicalize().ok()
            } else {
                Some(path)
            }
        })
        .collect();
    let Some(root) = source
        .ancestors()
        .take_while(|directory| *directory == source || !ceilings.iter().any(|p| p == directory))
        .find(|directory| directory.join(".git").symlink_metadata().is_ok())
    else {
        return Ok(None);
    };
    // Subdirectory launches must not acquire a nearer repository after inspection.
    for directory in source.ancestors() {
        checked_native_name(&directory.join(".git"))?;
        if directory == root {
            break;
        }
    }
    let inspect = |args: &[&str]| inspect(identity, args, cancelled);
    // Linked worktrees locate their common metadata through this optional file.
    let git_dir = inspect(&["rev-parse", "--absolute-git-dir"])?;
    let git_dir = git_dir
        .strip_suffix(b"\n")
        .ok_or("invalid Hermes Git directory path")?;
    use std::os::unix::ffi::OsStrExt;
    checked_native_name(
        &std::path::Path::new(std::ffi::OsStr::from_bytes(git_dir)).join("commondir"),
    )?;
    let worktree = inspect(&[
        "config",
        "--type=bool",
        "--default=false",
        "--get",
        "extensions.worktreeConfig",
    ])?;
    // Native status may refresh the real index before AW callbacks are ready.
    for name in std::iter::once("config")
        .chain((worktree == b"true\n").then_some("config.worktree"))
        .chain(std::iter::once("hooks"))
    {
        let path = inspect(&["rev-parse", "--path-format=absolute", "--git-path", name])?;
        let path = path
            .strip_suffix(b"\n")
            .ok_or("invalid Hermes Git metadata path")?;
        let path = source.join(std::ffi::OsStr::from_bytes(path));
        // Git supports core.hooksPath=/dev/null to disable Hooks entirely.
        let path = if name == "hooks" && path != std::path::Path::new("/dev/null") {
            path.join("post-index-change")
        } else {
            path
        };
        checked_native_name(&path)?;
    }
    Ok(Some(root))
}

/// Validate effective configuration origins, includes and helper settings.
pub(super) fn checked_configuration(
    identity: &aw_exec::CommandSpec,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<()> {
    let repository = checked_repository(identity, cancelled)?;
    let source = &identity.cwd;
    // Git reports relative origins after entering the discovered worktree.
    let origin_directory = repository.unwrap_or(source);
    let inspect = |args: &[&str]| inspect(identity, args, cancelled);
    // Native imports also run Git. Check every loaded configuration file,
    // including includes, before any status check or framework import can run helpers.
    // Origins omit absent scope files. Explicit scope paths work with older Git;
    // only an unspecified system path needs Git's compiled-location query.
    let scope_path = |key: &str| identity.environment.get(&OsString::from(key));
    let no_system = if scope_path("GIT_CONFIG_NOSYSTEM").is_some() {
        inspect(&[
            "--config-env=aw.preflight.nosystem=GIT_CONFIG_NOSYSTEM",
            "config",
            "--type=bool",
            "--get",
            "aw.preflight.nosystem",
        ])? == b"true\n"
    } else {
        false
    };
    if !no_system {
        if let Some(path) = scope_path("GIT_CONFIG_SYSTEM") {
            if !path.is_empty() {
                checked_native_name(&source.join(path))?;
            }
        } else {
            let paths = inspect(&["var", "GIT_CONFIG_SYSTEM"])
                .map_err(|error| format!("Hermes cannot locate Git's system configuration; set GIT_CONFIG_SYSTEM to its actual path (or /dev/null to disable it): {error}"))?;
            for path in paths
                .split(|byte| *byte == b'\n')
                .filter(|path| !path.is_empty())
            {
                checked_native_name(&source.join(std::ffi::OsStr::from_bytes(path)))?;
            }
        }
    }
    if let Some(path) = scope_path("GIT_CONFIG_GLOBAL") {
        if !path.is_empty() {
            checked_native_name(&source.join(path))?;
        }
    } else {
        // Let Git expand HOME, including relative/empty spellings, without changing it.
        for (key, default) in [
            ("HOME", "~/.gitconfig"),
            ("XDG_CONFIG_HOME", "~/.config/git/config"),
        ] {
            if key == "XDG_CONFIG_HOME" {
                if let Some(path) = scope_path(key).filter(|path| !path.is_empty()) {
                    checked_native_name(&source.join(path).join("git/config"))?;
                    continue;
                }
            }
            if scope_path("HOME").is_some() {
                let setting = format!("aw.preflight.path={default}");
                let path = inspect(&[
                    "-c",
                    &setting,
                    "config",
                    "--type=path",
                    "--get",
                    "aw.preflight.path",
                ])?;
                let path = path
                    .strip_suffix(b"\n")
                    .ok_or("invalid Hermes global Git path")?;
                checked_native_name(&source.join(std::ffi::OsStr::from_bytes(path)))?;
            }
        }
    }
    let configuration = inspect(&["config", "--null", "--show-origin", "--list"])?;
    for origin in configuration.split(|byte| *byte == 0).step_by(2) {
        if let Some(path) = origin.strip_prefix(b"file:") {
            trusted_native_path(&origin_directory.join(std::ffi::OsStr::from_bytes(path)))?;
        }
    }
    let is_include = |key: &[u8]| {
        key == b"include.path" || (key.starts_with(b"includeif.") && key.ends_with(b".path"))
    };
    if configuration
        .split(|byte| *byte == 0)
        .skip(1)
        .step_by(2)
        .any(|setting| {
            is_include(
                setting
                    .split(|byte| *byte == b'\n')
                    .next()
                    .unwrap_or_default(),
            )
        })
    {
        // Git expands ~ and its installation prefix; relative includes use their origin.
        let includes = inspect(&[
            "config",
            "--null",
            "--show-origin",
            "--type=path",
            "--get-regexp",
            r"^(include|includeif\..*)\.path$",
        ])?;
        for entry in includes
            .split(|byte| *byte == 0)
            .collect::<Vec<_>>()
            .chunks_exact(2)
        {
            let value = entry[1]
                .splitn(2, |byte| *byte == b'\n')
                .nth(1)
                .ok_or("invalid Hermes Git include entry")?;
            let path = std::path::Path::new(std::ffi::OsStr::from_bytes(value));
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                let origin = entry[0]
                    .strip_prefix(b"file:")
                    .ok_or("relative Hermes Git include requires a file origin")?;
                origin_directory
                    .join(std::ffi::OsStr::from_bytes(origin))
                    .parent()
                    .ok_or("Hermes Git include origin has no parent")?
                    .join(path)
            };
            checked_native_name(&path)?;
        }
    }
    for setting in configuration.split(|byte| *byte == 0).skip(1).step_by(2) {
        let key = setting
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        if key.starts_with(b"filter.")
            && [b".clean".as_slice(), b".process", b".smudge"]
                .iter()
                .any(|suffix| key.ends_with(suffix))
        {
            return Err("Hermes does not support external Git filters; remove the filter commands before retrying".into());
        }
    }
    // A protected config can still reference an unprotected shell command. Let
    // Git parse booleans; supporting arbitrary helper command lines needs a
    // separate trust contract, not a partial shell/path parser.
    inspect(&["config", "--type=bool", "--default=false", "--get", "core.fsmonitor"])
        .map_err(|error| format!("Hermes Git requires boolean core.fsmonitor; external command hooks are unsupported, disable the hook before retrying: {error}"))?;
    Ok(())
}
