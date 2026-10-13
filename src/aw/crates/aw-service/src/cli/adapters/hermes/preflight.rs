//! Check the native installation and Git environment before importing the Hermes CLI.

mod git;

use super::{adapter, Result, REVISION};
use std::{
    ffi::OsString,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::PermissionsExt,
    },
    path::PathBuf,
    time::Duration,
};

/// Validate the pinned installation before and after native dotenv loading.
pub(super) fn checked_profile_command(
    command: &mut aw_exec::CommandSpec,
    profile: &std::path::Path,
) -> Result<(OsString, PathBuf, tempfile::TempDir)> {
    checked_loader_environment(command)?;
    let python = python_entrypoint(command)?;
    let venv = python
        .parent()
        .and_then(std::path::Path::parent)
        .ok_or("Hermes interpreter has no venv directory")?;
    trusted_native_path(&venv.join("pyvenv.cfg"))?;
    command
        .environment
        .insert("HERMES_HOME".into(), profile.as_os_str().into());
    // Hermes follows active_profile even with a root HERMES_HOME. An explicit
    // native selector pins the same directory that AW inspected above.
    let selector = if profile
        .parent()
        .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "profiles"))
    {
        profile
            .file_name()
            .ok_or("Hermes profile has no name")?
            .to_os_string()
    } else {
        OsString::from("default")
    };
    // Read installation metadata with site initialization disabled: discovery must
    // not import the framework, editable finders, .pth files or project modules.
    let cache = tempfile::Builder::new()
        .prefix("aw-hermes-python-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    trusted_native_path(cache.path())?;
    let mut discovery = command.clone();
    discovery.program = python.clone();
    let script = r#"import json, sys, sysconfig, site as site_module
from importlib.metadata import Distribution
from pathlib import Path
from urllib.parse import unquote, urlsplit
prefix = str(Path(sys.executable).parent.parent)
config = dict(line.split('=', 1) for line in (Path(prefix) / 'pyvenv.cfg').read_text().splitlines() if '=' in line)
if any(key.strip().lower() == 'include-system-site-packages' and value.strip().lower() != 'false' for key, value in config.items()):
    raise RuntimeError('Hermes requires a venv without system site packages')
site = sysconfig.get_path('purelib', vars={'base': prefix, 'platbase': prefix})
matches = list(Distribution.discover(name='hermes-agent', path=[site]))
if len(matches) != 1:
    raise RuntimeError('Hermes requires one installed editable distribution in its venv')
origin = json.loads(matches[0].read_text('direct_url.json'))
url = urlsplit(origin['url'])
if origin.get('dir_info', {}).get('editable') is not True or url.scheme != 'file' or url.netloc not in ('', 'localhost'):
    raise RuntimeError('Hermes requires a local editable checkout')
sites = {site, *site_module.getsitepackages([prefix]),
         *(sysconfig.get_path(name, scheme='venv', vars={'base': prefix, 'platbase': prefix}) for name in ('purelib', 'platlib'))}
print(json.dumps([unquote(url.path, errors='strict'), [path for path in sorted(sites) if Path(path).exists()]]))
"#;
    discovery.args = python_args(
        cache.path(),
        &["-I".into(), "-S".into(), "-c".into(), script.into()],
    );
    let output = aw_exec::run(
        &discovery,
        &[],
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 4 * 1024 * 1024,
            stderr_bytes: 4096,
        },
        std::time::Instant::now() + Duration::from_secs(5),
        &super::super::super::run::CANCELLED,
    )
    .map_err(|error| {
        format!(
            "Hermes source discovery requires a local editable installation with UTF-8 paths in its venv: {error}"
        )
    })?;
    if !output.status.success() {
        return Err(
            "Hermes source discovery requires a local editable installation with UTF-8 paths in its venv".into(),
        );
    }
    let (source, sites): (PathBuf, Vec<PathBuf>) = serde_json::from_slice(&output.stdout)?;
    let source = trusted_native_path(&source)?;
    checked_checkout(command, &source)?;
    checked_site_paths(sites)?;
    super::install::dotenv(&source.join(".env"))?;
    // Use the pinned loader's dotenv precedence/interpolation, before native CLI
    // imports. This subprocess cannot alter the eventual chat/provider environment.
    let script = r#"import json, os, sys
from pathlib import Path
from hermes_cli.env_loader import load_hermes_dotenv
load_hermes_dotenv(project_env=Path(sys.argv[1]) / '.env', load_external_secrets=False)
print(json.dumps([[list(os.fsencode(key)), list(os.fsencode(value))]
    for key, value in os.environ.items()
    if key in ('PATH', 'HOME', 'XDG_CONFIG_HOME') or key.startswith(('GIT_', 'LD_'))]))
"#;
    discovery.args = python_args(
        cache.path(),
        &[
            "-P".into(),
            "-c".into(),
            script.into(),
            source.as_os_str().into(),
        ],
    );
    let output = aw_exec::run(
        &discovery,
        &[],
        aw_exec::Limits {
            input_bytes: 0,
            stdout_bytes: 4 * 1024 * 1024,
            stderr_bytes: 4096,
        },
        std::time::Instant::now() + Duration::from_secs(5),
        &super::super::super::run::CANCELLED,
    )?;
    if !output.status.success() {
        return Err("Hermes native dotenv probe failed".into());
    }
    let effective: Vec<(Vec<u8>, Vec<u8>)> = serde_json::from_slice(&output.stdout)?;
    let mut after_dotenv = command.clone();
    after_dotenv.environment.retain(|key, _| {
        !key.to_str().is_some_and(|key| {
            matches!(key, "PATH" | "HOME" | "XDG_CONFIG_HOME")
                || key.starts_with("GIT_")
                || key.starts_with("LD_")
        })
    });
    after_dotenv.environment.extend(
        effective
            .into_iter()
            .map(|(key, value)| (OsString::from_vec(key), OsString::from_vec(value))),
    );
    checked_loader_environment(&after_dotenv)?;
    checked_checkout(&after_dotenv, &source)?;
    // Sticky directories protect the script inode, not sibling import names.
    let version = adapter::version_output_with_timeout(
        &discovery,
        &python_args(
            cache.path(),
            &[
                "-P".into(),
                command.program.as_os_str().into(),
                "--profile".into(),
                selector.clone(),
                "--version".into(),
            ],
        ),
        Duration::from_secs(60),
    )?;
    let reported = version
        .lines()
        .find_map(|line| line.strip_prefix("Install directory: "))
        .ok_or("Hermes version output has no installation directory")?;
    if PathBuf::from(reported).canonicalize()? != source {
        return Err("Hermes version source differs from its installed metadata".into());
    }
    Ok((selector, python, cache))
}

fn checked_checkout(command: &aw_exec::CommandSpec, source: &std::path::Path) -> Result<()> {
    // Native Git runs from both cwd and source; relative scopes select different files.
    for key in [
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_GLOBAL",
        "HOME",
        "XDG_CONFIG_HOME",
    ] {
        if let Some(path) = command.environment.get(&OsString::from(key)) {
            if (key == "HOME" || !path.is_empty()) && !std::path::Path::new(path).is_absolute() {
                return Err(format!("Hermes requires absolute {key}; use an absolute path or unset it before retrying").into());
            }
        }
    }
    // Native imports retain these selectors. Reject unsupported repositories
    // instead of inspecting one checkout while native Git uses another.
    for key in ["GIT_DIR", "GIT_COMMON_DIR"] {
        if command.environment.contains_key(&OsString::from(key)) {
            return Err(format!("Hermes does not support {key}; unset it before retrying").into());
        }
    }
    let mut identity = command.clone();
    // Native Git runs from both the launch directory and its installation checkout.
    let launch_git = native_executable(command, std::path::Path::new("git"))?;
    identity.cwd = source.to_path_buf();
    identity.program = native_executable(&identity, std::path::Path::new("git"))?;
    if identity.program != launch_git {
        return Err("Hermes requires the same Git executable in cwd and checkout; use an absolute PATH entry".into());
    }
    trusted_native_path(&source.join(".git"))?;
    // GIT_CONFIG selects a file only for `git config`, not status/rev-parse.
    // Configuration inspection must describe the commands it protects.
    identity.environment.remove(&OsString::from("GIT_CONFIG"));
    identity
        .environment
        .insert("GIT_WORK_TREE".into(), source.into());
    let revision = adapter::version_output(
        &identity,
        &[
            "--no-replace-objects".into(),
            "--no-optional-locks".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-C".into(),
            source.as_os_str().into(),
            "rev-parse".into(),
            "HEAD".into(),
        ],
    )?;
    if revision != REVISION {
        return Err(
            format!("Hermes official checkout {REVISION} is required by this adapter").into(),
        );
    }
    let inspect = |args: &[&str]| git::inspect(&identity, args);
    git::checked_configuration(&identity)?;
    // Native imports also invoke Git from the caller's directory. Keep its
    // repository/includeIf context instead of reusing the installation context.
    if command.cwd != identity.cwd {
        let mut launch_git = command.clone();
        launch_git.program = identity.program.clone();
        launch_git.environment.remove(&OsString::from("GIT_CONFIG"));
        git::checked_configuration(&launch_git)?;
    }
    // Porcelain intentionally trusts these index flags, even when bytes differ.
    // Reject them without refreshing or rewriting the caller's index.
    let entries = inspect(&["-c", "core.fsmonitor=false", "ls-files", "-v", "-z"])?;
    if entries.split(|byte| *byte == 0).any(|entry| {
        entry
            .first()
            .is_some_and(|tag| tag.is_ascii_lowercase() || *tag == b'S')
    }) {
        return Err("Hermes checkout must not use assume-unchanged or skip-worktree; clear these flags before retrying".into());
    }
    for entry in entries
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
    {
        let path = entry.get(2..).ok_or("invalid Hermes Git index entry")?;
        trusted_native_path(&source.join(std::ffi::OsStr::from_bytes(path)))?;
    }
    let status = |command: &aw_exec::CommandSpec| {
        adapter::version_output(
            command,
            &[
                "--no-replace-objects".into(),
                "--no-optional-locks".into(),
                "-c".into(),
                "core.fsmonitor=false".into(),
                "-c".into(),
                "core.trustctime=true".into(),
                "-c".into(),
                "core.checkStat=default".into(),
                "status".into(),
                "--porcelain".into(),
                "--untracked-files=no".into(),
            ],
        )
    };
    let dirty = "Hermes official checkout must have no staged or unstaged tracked changes";
    if !status(&identity)?.is_empty() {
        return Err(dirty.into());
    }
    // Even full stat checks can miss same-second, same-size edits on some Git
    // builds. A fresh index has no cached stat data, so Git must read the files.
    let scratch = tempfile::Builder::new()
        .prefix("aw-hermes-checkout-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let mut fresh = identity.clone();
    fresh
        .environment
        .insert("GIT_INDEX_FILE".into(), scratch.path().join("index").into());
    adapter::version_output(
        &fresh,
        &[
            "--no-replace-objects".into(),
            "--no-optional-locks".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "core.splitIndex=false".into(),
            "read-tree".into(),
            "HEAD".into(),
        ],
    )?;
    if !status(&fresh)?.is_empty() {
        return Err(dirty.into());
    }
    scratch.close()?;
    Ok(())
}

/// Isolate cache reads before Python initializes site or imports any native module.
pub(super) fn python_args(cache: &std::path::Path, args: &[OsString]) -> Vec<OsString> {
    let mut prefix = OsString::from("pycache_prefix=");
    prefix.push(cache);
    [OsString::from("-X"), prefix]
        .into_iter()
        .chain(args.iter().cloned())
        .collect()
}

fn checked_loader_environment(command: &aw_exec::CommandSpec) -> Result<()> {
    for (key, value) in &command.environment {
        if key.to_str().is_some_and(|key| key.starts_with("LD_")) && !value.is_empty() {
            return Err(format!(
                "Hermes does not support loader environment variable {}; unset it before retrying",
                key.to_string_lossy()
            )
            .into());
        }
    }
    Ok(())
}

fn checked_site_paths(mut pending: Vec<PathBuf>) -> Result<()> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut visited = std::collections::BTreeSet::new();
    while let Some(path) = pending.pop() {
        if super::super::super::run::CANCELLED.load(std::sync::atomic::Ordering::Acquire) {
            return Err(aw_exec::Error::Cancelled.into());
        }
        if std::time::Instant::now() >= deadline {
            return Err("Hermes venv site permission inspection exceeded 30 seconds".into());
        }
        let path = trusted_native_path(&path)?;
        let metadata = path.metadata()?;
        // Sticky directories protect existing inodes, but permit new import names.
        if metadata.is_dir() && metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "Hermes venv site directory permits modification by others: {}",
                path.display()
            )
            .into());
        }
        if metadata.is_dir() && visited.insert(path.clone()) {
            for entry in std::fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        }
    }
    Ok(())
}

/// Resolve the installed console script's protected venv interpreter.
pub(super) fn python_entrypoint(command: &mut aw_exec::CommandSpec) -> Result<PathBuf> {
    use std::{io::Read, os::unix::fs::OpenOptionsExt};
    let executable = native_executable(command, &command.program)?;
    command.program = executable.clone();
    // A path can be replaced after lookup; never block opening a substituted FIFO.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&executable)?;
    if !file.metadata()?.is_file() {
        return Err("Hermes entrypoint must be a regular file".into());
    }
    let mut script = Vec::new();
    file.take(65537).read_to_end(&mut script)?;
    if script.len() > 65536 {
        return Err("Hermes entrypoint exceeds 65536 bytes".into());
    }
    let script = std::str::from_utf8(&script)?;
    let python = script
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
        .map(std::path::Path::new)
        .filter(|path| {
            path.is_absolute()
                && path.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("python"))
        })
        .ok_or("Hermes requires its installed Python console-script entrypoint")?;
    if !script.contains("from hermes_cli.main import main") {
        return Err("Hermes requires its installed Python console-script entrypoint".into());
    }
    trusted_native_path(python)?;
    // Keep the venv path: resolving its Python symlink changes prefix discovery.
    Ok(python.to_path_buf())
}

fn native_executable(command: &aw_exec::CommandSpec, program: &std::path::Path) -> Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let can_execute = |path: &std::path::Path| {
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: name is live; check execute access as the native child identity.
        path.is_file()
            && unsafe {
                libc::faccessat(libc::AT_FDCWD, name.as_ptr(), libc::X_OK, libc::AT_EACCESS) == 0
            }
    };
    let executable = if program.components().count() > 1 {
        command.cwd.join(program)
    } else {
        let mut executable = None;
        for directory in std::env::split_paths(
            command
                .environment
                .get(&OsString::from("PATH"))
                .ok_or("Hermes executable requires PATH")?,
        ) {
            let candidate = command.cwd.join(directory).join(program);
            // A skipped name may become executable before native PATH lookup.
            checked_native_name(&candidate)?;
            if can_execute(&candidate) {
                executable = Some(candidate);
                break;
            }
        }
        executable.ok_or("Hermes executable was not found in PATH")?
    };
    if !can_execute(&executable) {
        return Err(format!(
            "Hermes entrypoint must be an executable regular file: {}",
            executable.display()
        )
        .into());
    }
    trusted_native_path(&executable)
}

fn checked_native_name(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    // Git documents /dev/null as the explicit way to disable file scopes.
    if path == std::path::Path::new("/dev/null") && path.metadata()?.file_type().is_char_device() {
        return Ok(());
    }
    for ancestor in path.ancestors() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(_) => {
                let resolved = trusted_native_path(ancestor)?;
                // A missing name has no inode; protect who can create it.
                if ancestor != path && resolved.metadata()?.permissions().mode() & 0o022 != 0 {
                    return Err(format!(
                        "Hermes native path parent permits modification by others: {}",
                        ancestor.display()
                    )
                    .into());
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Err("Hermes native path has no existing ancestor".into())
}

fn trusted_native_path(path: &std::path::Path) -> Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let resolved = path.canonicalize()?;
    // Check the selected spelling as well as its target: a protected target
    // does not make a replaceable venv directory or symlink trustworthy.
    // SAFETY: geteuid only reads the current process identity.
    let uid = unsafe { libc::geteuid() };
    for spelling in [path, resolved.as_path()] {
        for component in spelling.ancestors() {
            let metadata = std::fs::symlink_metadata(component)?;
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(component)?;
                trusted_native_path(
                    &component
                        .parent()
                        .ok_or("Hermes symlink has no parent")?
                        .join(target),
                )?;
            }
            let protected = metadata.file_type().is_symlink()
                || metadata.mode() & 0o022 == 0
                || (metadata.is_dir() && metadata.mode() & 0o1000 != 0);
            if (metadata.uid() != uid && metadata.uid() != 0) || !protected {
                return Err(format!(
                    "Hermes native executable path permits modification by others: {}",
                    component.display()
                )
                .into());
            }
        }
    }
    Ok(resolved)
}
