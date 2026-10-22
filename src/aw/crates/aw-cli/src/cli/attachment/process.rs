//! Discover the caller-owned native entrypoint without launching or signalling it.

use super::super::{read_file, Result};
use aw_exec::CommandSpec;
use aw_service::ProcessIdentity;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

pub(super) struct NativeProcess {
    pub identity: ProcessIdentity,
    pub command: CommandSpec,
    pub scope: PathBuf,
    pub pid: u32,
    executable: PathBuf,
    script: Option<PathBuf>,
    adapter: String,
}

fn environment(pid: u32) -> Result<BTreeMap<OsString, OsString>> {
    read_file(format!("/proc/{pid}/environ"), 1024 * 1024)?
        .split(|byte| *byte == 0)
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let index = pair
                .iter()
                .position(|byte| *byte == b'=')
                .ok_or("invalid native environment")?;
            Ok((
                OsString::from_vec(pair[..index].to_vec()),
                OsString::from_vec(pair[index + 1..].to_vec()),
            ))
        })
        .collect()
}

fn arguments(pid: u32) -> Result<Vec<OsString>> {
    Ok(read_file(format!("/proc/{pid}/cmdline"), 1024 * 1024)?
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| OsString::from_vec(arg.to_vec()))
        .collect())
}

fn namespaces(target: &Path, caller: &Path, adapter: &str) -> Result<()> {
    for namespace in ["pid", "mnt", "net"] {
        if namespace == "net" && adapter != "qwenpaw" {
            continue;
        }
        if fs::read_link(target.join(namespace))? != fs::read_link(caller.join(namespace))? {
            return Err(format!("attach requires the same {namespace} namespace").into());
        }
    }
    Ok(())
}

impl NativeProcess {
    pub fn read(pid: u32, adapter: &str, configured_program: &str) -> Result<Self> {
        if !matches!(adapter, "qoder" | "qwenpaw") {
            return Err("external attachment supports Qoder TUI and QwenPaw App only".into());
        }
        let identity = ProcessIdentity::read(pid)?;
        namespaces(
            &PathBuf::from(format!("/proc/{pid}/ns")),
            Path::new("/proc/self/ns"),
            adapter,
        )?;
        let cwd = fs::read_link(format!("/proc/{pid}/cwd"))?.canonicalize()?;
        let environment = environment(pid)?;
        let configured = Path::new(configured_program);
        let program = if configured.is_absolute() {
            configured.canonicalize()?
        } else if configured.components().count() > 1 {
            cwd.join(configured).canonicalize()?
        } else {
            std::env::split_paths(
                environment
                    .get(OsStr::new("PATH"))
                    .ok_or("native PATH missing")?,
            )
            .filter_map(|dir| dir.join(configured).canonicalize().ok())
            .find(|path| path.is_file())
            .ok_or("configured native executable not found")?
        };
        let executable = fs::read_link(format!("/proc/{pid}/exe"))?;
        let argv = arguments(pid)?;
        let script = if executable != program {
            let script = argv
                .get(1)
                .map(PathBuf::from)
                .ok_or("native entrypoint mismatch")?;
            if !script.is_absolute() || script.canonicalize()? != program {
                return Err("native executable does not match configured entrypoint".into());
            }
            Some(program.clone())
        } else {
            None
        };
        let offset = if script.is_some() { 2 } else { 1 };
        let args = argv
            .get(offset..)
            .ok_or("native arguments missing")?
            .to_vec();
        let scope = if adapter == "qoder" {
            PathBuf::from(
                environment
                    .get(OsStr::new("HOME"))
                    .ok_or("native HOME missing")?,
            )
            .join(".qoder")
        } else {
            PathBuf::from(
                environment
                    .get(OsStr::new("QWENPAW_WORKING_DIR"))
                    .ok_or("native QWENPAW_WORKING_DIR missing")?,
            )
        };
        let metadata = fs::symlink_metadata(&scope)?;
        if !metadata.is_dir()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || scope.canonicalize()? != scope
        {
            return Err("attachment requires a canonical private native configuration directory (mode 0700)".into());
        }
        let native = Self {
            identity,
            command: CommandSpec {
                program,
                args,
                cwd,
                environment,
            },
            scope,
            pid,
            executable,
            script,
            adapter: adapter.into(),
        };
        native.exclusive()?;
        native.identity.check()?;
        Ok(native)
    }

    pub fn exclusive(&self) -> Result<()> {
        self.identity.check()?;
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
            else {
                continue;
            };
            if pid == self.pid || pid == std::process::id() {
                continue;
            }
            let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
                continue;
            };
            if exe != self.executable {
                continue;
            }
            if let Some(script) = &self.script {
                let argv = arguments(pid)?;
                if argv.get(1).map(PathBuf::from).as_ref() != Some(script) {
                    continue;
                }
            }
            if self.identity.corroborate(pid).is_ok() {
                continue;
            }
            let env = environment(pid)?;
            let shared = if self.adapter == "qoder" {
                env.get(OsStr::new("HOME")) == self.command.environment.get(OsStr::new("HOME"))
                    || fs::read_link(entry.path().join("cwd")).ok().as_ref()
                        == Some(&self.command.cwd)
            } else {
                env.get(OsStr::new("QWENPAW_WORKING_DIR"))
                    == self
                        .command
                        .environment
                        .get(OsStr::new("QWENPAW_WORKING_DIR"))
            };
            if shared {
                return Err("native configuration scope is shared by another process".into());
            }
        }
        Ok(())
    }

    pub fn endpoint(&self, registration: &aw_adapter::attachment::Registration) -> Result<()> {
        let aw_adapter::attachment::Registration::Qwenpaw { port, .. } = registration else {
            return Ok(());
        };
        // Loopback requests must address the same network that owns the listener.
        namespaces(
            &PathBuf::from(format!("/proc/{}/ns", self.pid)),
            Path::new("/proc/self/ns"),
            "qwenpaw",
        )?;
        let listeners = fs::read_to_string(format!("/proc/{}/net/tcp", self.pid))?;
        let inode = listeners
            .lines()
            .skip(1)
            .find_map(|line| {
                let columns: Vec<_> = line.split_whitespace().collect();
                (columns.get(1) == Some(&format!("0100007F:{port:04X}").as_str())
                    && columns.get(3) == Some(&"0A"))
                .then(|| columns.get(9).copied())
                .flatten()
            })
            .ok_or("QwenPaw must listen on the selected loopback IPv4 port")?;
        let socket = format!("socket:[{inode}]");
        if !fs::read_dir(format!("/proc/{}/fd", self.pid))?
            .filter_map(|entry| entry.ok())
            .any(|entry| {
                fs::read_link(entry.path())
                    .is_ok_and(|path| path.as_os_str() == OsStr::new(&socket))
            })
        {
            return Err("QwenPaw endpoint is not owned by the selected process".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn qwenpaw_requires_the_callers_network_namespace() {
        let root = tempfile::tempdir().unwrap();
        let caller = root.path().join("caller");
        let target = root.path().join("target");
        fs::create_dir(&caller).unwrap();
        fs::create_dir(&target).unwrap();
        for namespace in ["pid", "mnt", "net"] {
            let identity = format!("{namespace}:[1]");
            symlink(&identity, caller.join(namespace)).unwrap();
            symlink(&identity, target.join(namespace)).unwrap();
        }
        namespaces(&target, &caller, "qwenpaw").unwrap();
        fs::remove_file(target.join("net")).unwrap();
        symlink("net:[2]", target.join("net")).unwrap();
        assert!(namespaces(&target, &caller, "qwenpaw")
            .unwrap_err()
            .to_string()
            .contains("same net namespace"));
        namespaces(&target, &caller, "qoder").unwrap();
        fs::remove_file(target.join("net")).unwrap();
        assert!(namespaces(&target, &caller, "qwenpaw").is_err());
    }
}
