//! Check the native installation and Git environment before importing the Hermes CLI.

use super::{adapter, Result, REVISION};
use std::{
    collections::BTreeMap, ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf,
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
    let (source, sites): (PathBuf, Vec<PathBuf>) = serde_json::from_str(&adapter::version_output(
        &discovery,
        &python_args(cache.path(), &["-I".into(), "-S".into(), "-c".into(), script.into()]),
    ).map_err(|error| format!("Hermes source discovery requires a local editable installation in its venv: {error}"))?)?;
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
print(json.dumps({key: value for key, value in os.environ.items()
    if key in ('PATH', 'HOME', 'XDG_CONFIG_HOME') or key.startswith(('GIT_', 'LD_'))}))
"#;
    let effective: BTreeMap<String, String> = serde_json::from_str(&adapter::version_output(
        &discovery,
        &python_args(
            cache.path(),
            &[
                "-P".into(),
                "-c".into(),
                script.into(),
                source.as_os_str().into(),
            ],
        ),
    )?)?;
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
            .map(|(key, value)| (key.into(), value.into())),
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
    // Native imports retain these selectors. Reject unsupported repositories
    // instead of inspecting one checkout while native Git uses another.
    for key in ["GIT_DIR", "GIT_COMMON_DIR"] {
        if command.environment.contains_key(&OsString::from(key)) {
            return Err(format!("Hermes does not support {key}; unset it before retrying").into());
        }
    }
    let mut identity = command.clone();
    // Native Git runs from both the launch directory and its installation checkout.
    native_executable(command, std::path::Path::new("git"))?;
    identity.cwd = source.to_path_buf();
    identity.program = native_executable(&identity, std::path::Path::new("git"))?;
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
    let inspect = |args: &[&str]| -> Result<Vec<u8>> {
        let mut probe = identity.clone();
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
            &super::super::super::run::CANCELLED,
        )?;
        if !output.status.success() {
            return Err(format!("Hermes checkout inspection failed: {}", args.join(" ")).into());
        }
        Ok(output.stdout)
    };
    // Linked worktrees locate their common metadata through this optional file.
    let git_dir = inspect(&["rev-parse", "--absolute-git-dir"])?;
    let git_dir = git_dir
        .strip_suffix(b"\n")
        .ok_or("invalid Hermes Git directory path")?;
    use std::os::unix::ffi::OsStrExt;
    checked_native_name(
        &std::path::Path::new(std::ffi::OsStr::from_bytes(git_dir)).join("commondir"),
    )?;
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
    let worktree = inspect(&[
        "config",
        "--type=bool",
        "--default=false",
        "--get",
        "extensions.worktreeConfig",
    ])?;
    for name in
        std::iter::once("config").chain((worktree == b"true\n").then_some("config.worktree"))
    {
        let path = inspect(&["rev-parse", "--path-format=absolute", "--git-path", name])?;
        let path = path
            .strip_suffix(b"\n")
            .ok_or("invalid Hermes Git configuration path")?;
        checked_native_name(&source.join(std::ffi::OsStr::from_bytes(path)))?;
    }
    let configuration = inspect(&["config", "--null", "--show-origin", "--list"])?;
    for origin in configuration.split(|byte| *byte == 0).step_by(2) {
        if let Some(path) = origin.strip_prefix(b"file:") {
            trusted_native_path(&source.join(std::ffi::OsStr::from_bytes(path)))?;
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
                source
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
            return Err("Hermes checkout does not support external Git filters; remove the filter commands before retrying".into());
        }
    }
    // A protected config can still reference an unprotected shell command. Let
    // Git parse booleans; supporting arbitrary helper command lines needs a
    // separate trust contract, not a partial shell/path parser.
    inspect(&["config", "--type=bool", "--default=false", "--get", "core.fsmonitor"])
        .map_err(|error| format!("Hermes checkout requires boolean core.fsmonitor; external command hooks are unsupported, disable the hook before retrying: {error}"))?;
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
