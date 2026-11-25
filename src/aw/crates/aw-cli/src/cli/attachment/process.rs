//! Discover the caller-owned native entrypoint without launching or signalling it.

mod profile;
mod python;
#[cfg(test)]
mod tests;

use super::super::{read_file, Result};
use aw_exec::CommandSpec;
use aw_service::ProcessIdentity;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStringExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

pub(in crate::cli) struct NativeProcess {
    pub identity: ProcessIdentity,
    pub command: CommandSpec,
    pub scope: PathBuf,
    pub pid: u32,
    executable: PathBuf,
    script: Option<PathBuf>,
    adapter: String,
}

fn environment(task: &Path) -> Result<BTreeMap<OsString, OsString>> {
    read_file(task.join("environ"), 1024 * 1024)?
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

fn peer_exited(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // A pidfd opened without PIDFD_THREAD reports exit of the entire group.
    // SAFETY: pidfd_open takes only a numeric PID and flags.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd < 0 {
        return std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
    }
    // SAFETY: pidfd_open returned a new owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let mut exit = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: the descriptor and pollfd remain live through the nonblocking call.
    let ready = unsafe { libc::poll(&mut exit, 1, 0) };
    ready == 1 && exit.revents & libc::POLLIN != 0
}

fn peer_executable(entry: &Path, pid: u32) -> Option<(PathBuf, PathBuf)> {
    match fs::read_link(entry.join("exe")) {
        Ok(exe) => Some((entry.to_owned(), exe)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let leader_exited = fs::read(entry.join("stat")).is_ok_and(|stat| {
                let Some(comm_end) = stat.iter().rposition(|byte| *byte == b')') else {
                    return false;
                };
                stat[comm_end + 1..]
                    .iter()
                    .find(|byte| !byte.is_ascii_whitespace())
                    .is_some_and(|state| matches!(state, b'Z' | b'X'))
            });
            if !leader_exited || peer_exited(pid) {
                return None;
            }
            // procfs hides the leader's executable after its exit. Identify
            // the peer via a surviving thread before inspecting its scope.
            // If no thread still exposes an executable, the process has torn
            // down its address space (e.g. mid-exit_group) and is not a live peer.
            if let Ok(threads) = fs::read_dir(entry.join("task")) {
                for thread in threads.flatten() {
                    let path = thread.path();
                    if let Ok(exe) = fs::read_link(path.join("exe")) {
                        return Some((path, exe));
                    }
                }
            }
            None
        }
        Err(_) => None,
    }
}

// After leader exit, Qoder threads can retain distinct project directories.
fn any_peer_cwd(
    process: &Path,
    task: &Path,
    inspect_siblings: bool,
    matches: impl Fn(&Path) -> Result<bool>,
) -> Result<bool> {
    if matches(&fs::read_link(task.join("cwd"))?)? {
        return Ok(true);
    }
    if task == process || !inspect_siblings {
        return Ok(false);
    }
    for sibling in fs::read_dir(process.join("task"))? {
        let sibling = sibling?.path();
        if sibling == task || sibling.file_name() == process.file_name() {
            continue;
        }
        let cwd = match fs::read_link(sibling.join("cwd")) {
            Ok(cwd) => cwd,
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound && !sibling.try_exists()? =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        };
        if matches(&cwd)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn peer_environment(pid: u32, task: &Path) -> Result<Option<BTreeMap<OsString, OsString>>> {
    match environment(task) {
        Ok(env) => Ok(Some(env)),
        Err(_) if peer_exited(pid) => Ok(None),
        Err(error) => Err(format!("read native peer {pid} environment: {error}").into()),
    }
}

fn arguments(path: &Path) -> Result<Vec<OsString>> {
    Ok(read_file(path.join("cmdline"), 1024 * 1024)?
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

fn same_entrypoint(left: &Path, right: &Path) -> Result<bool> {
    if left.file_name() == right.file_name() {
        return Ok(true);
    }
    let left_metadata = fs::metadata(left)?;
    let right_metadata = fs::metadata(right)?;
    if !left_metadata.is_file()
        || !right_metadata.is_file()
        || left_metadata.len() != right_metadata.len()
    {
        return Ok(false);
    }
    let mut left = fs::File::open(left)?;
    let mut right = fs::File::open(right)?;
    // Renaming a copied native binary does not isolate the configuration it reads.
    let mut remaining = left_metadata.len();
    let mut a = [0; 65536];
    let mut b = [0; 65536];
    while remaining > 0 {
        let count = remaining.min(a.len() as u64) as usize;
        left.read_exact(&mut a[..count])?;
        right.read_exact(&mut b[..count])?;
        if a[..count] != b[..count] {
            return Ok(false);
        }
        remaining -= count as u64;
    }
    Ok(true)
}

fn executable(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path_bytes) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // NativeProcess requires the caller and native process to share a user.
    path.is_file()
        && unsafe {
            libc::faccessat(
                libc::AT_FDCWD,
                path_bytes.as_ptr(),
                libc::X_OK,
                libc::AT_EACCESS,
            )
        } == 0
}

impl NativeProcess {
    pub fn read(pid: u32, adapter: &str, configured_program: &str) -> Result<Self> {
        let native = Self::read_identity(pid, adapter, configured_program)?;
        native.exclusive()?;
        Ok(native)
    }

    pub fn read_identity(pid: u32, adapter: &str, configured_program: &str) -> Result<Self> {
        if !matches!(adapter, "qoder" | "qwenpaw") {
            return Err("external attachment supports Qoder TUI and QwenPaw App only".into());
        }
        let identity = ProcessIdentity::read(pid)?;
        let task = PathBuf::from(format!("/proc/{pid}"));
        namespaces(&task.join("ns"), Path::new("/proc/self/ns"), adapter)?;
        let cwd = fs::read_link(task.join("cwd"))?.canonicalize()?;
        let environment = environment(&task)?;
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
            .filter_map(|dir| cwd.join(dir).join(configured).canonicalize().ok())
            .find(|path| executable(path))
            .ok_or("configured native executable not found")?
        };
        let executable = fs::read_link(task.join("exe"))?;
        let argv = arguments(&task)?;
        let script = if executable != program {
            let script = argv
                .get(1)
                .map(PathBuf::from)
                .ok_or("native entrypoint mismatch")?;
            if cwd.join(script).canonicalize()? != program {
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
        native.identity.check()?;
        Ok(native)
    }

    pub fn exclusive(&self) -> Result<()> {
        self.identity.check()?;
        let qoder = self.adapter == "qoder";
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
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.uid() != unsafe { libc::geteuid() } {
                continue;
            }
            let path = entry.path();
            let Some((task, exe)) = peer_executable(&path, pid) else {
                continue;
            };
            // Inspect only native peers. Unrelated processes may expose an exe
            // link while restricting access to their environment (for example CI).
            let candidate = (|| -> Result<bool> {
                Ok(if let Some(script) = &self.script {
                    let argv = arguments(&task)?;
                    let arg = if self.adapter == "qwenpaw" {
                        match python::entrypoint(&argv) {
                            Some(python::Entrypoint::Module(module)) => {
                                return Ok(module == "qwenpaw")
                            }
                            Some(python::Entrypoint::Script(script)) => script,
                            None => return Ok(false),
                        }
                    } else if let Some(arg) = argv.get(1) {
                        arg
                    } else {
                        return Ok(false);
                    };
                    if arg.to_string_lossy().starts_with('-') {
                        return Ok(false);
                    }
                    let peer_script = PathBuf::from(arg);
                    let matches = |path: &Path| -> Result<bool> {
                        Ok(path.is_file() && same_entrypoint(script, path)?)
                    };
                    if peer_script.is_absolute() {
                        matches(&peer_script)?
                    } else {
                        any_peer_cwd(&path, &task, qoder, |cwd| matches(&cwd.join(&peer_script)))?
                    }
                } else {
                    self.executable.file_name() == exe.file_name()
                        || same_entrypoint(&self.executable, &task.join("exe"))?
                })
            })();
            let candidate = match candidate {
                Ok(candidate) => candidate,
                // An entry that disappears during identification may be
                // unrelated. Recheck surviving threads before discarding it.
                Err(_) if peer_executable(&path, pid).is_none() => continue,
                Err(error) => return Err(format!("inspect native peer {pid}: {error}").into()),
            };
            if !candidate {
                continue;
            }
            let Some(env) = peer_environment(pid, &task)? else {
                continue;
            };
            let shared = any_peer_cwd(&path, &task, qoder, |cwd| {
                if qoder && cwd == self.command.cwd {
                    return Ok(true);
                }
                let scope = if qoder {
                    env.get(OsStr::new("HOME"))
                        .map(|home| cwd.join(home).join(".qoder"))
                } else {
                    Some(profile::qwenpaw(&env, cwd)?)
                };
                Ok(scope
                    .and_then(|scope| scope.canonicalize().ok())
                    .is_some_and(|scope| scope == self.scope))
            });
            let shared = match shared {
                Ok(shared) => shared,
                Err(_) if peer_exited(pid) => continue,
                Err(error) => return Err(format!("inspect native peer {pid}: {error}").into()),
            };
            if !shared {
                continue;
            }
            if self.identity.corroborate(pid).is_ok() {
                continue;
            }
            return Err("native configuration scope is shared by another process".into());
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
