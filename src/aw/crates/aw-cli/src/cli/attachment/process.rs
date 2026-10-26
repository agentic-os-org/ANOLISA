//! Discover the caller-owned native entrypoint without launching or signalling it.

mod profile;
mod python;

use super::super::{read_file, Result};
use aw_exec::CommandSpec;
use aw_service::ProcessIdentity;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    io::Read,
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
            .filter_map(|dir| cwd.join(dir).join(configured).canonicalize().ok())
            .find(|path| executable(path))
            .ok_or("configured native executable not found")?
        };
        let executable = fs::read_link(format!("/proc/{pid}/exe"))?;
        let argv = arguments(pid)?;
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
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.uid() != unsafe { libc::geteuid() } {
                continue;
            }
            let Ok(exe) = fs::read_link(entry.path().join("exe")) else {
                continue;
            };
            // Inspect only native peers. Unrelated processes may expose an exe
            // link while restricting access to their environment (for example CI).
            let candidate = (|| -> Result<bool> {
                Ok(if let Some(script) = &self.script {
                    let argv = arguments(pid)?;
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
                    let peer_script = if peer_script.is_absolute() {
                        peer_script
                    } else {
                        fs::read_link(entry.path().join("cwd"))?.join(peer_script)
                    };
                    peer_script.is_file() && same_entrypoint(script, &peer_script)?
                } else {
                    self.executable.file_name() == exe.file_name()
                        || same_entrypoint(&self.executable, &entry.path().join("exe"))?
                })
            })();
            let candidate = match candidate {
                Ok(candidate) => candidate,
                Err(_) if fs::read_link(entry.path().join("exe")).is_err() => continue,
                Err(error) => return Err(format!("inspect native peer {pid}: {error}").into()),
            };
            if !candidate {
                continue;
            }
            let peer_cwd = match fs::read_link(entry.path().join("cwd")) {
                Ok(cwd) => cwd,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let env = match environment(pid) {
                Ok(env) => env,
                Err(_) if !entry.path().exists() => continue,
                Err(error) => {
                    return Err(format!("read native peer {pid} environment: {error}").into())
                }
            };
            let peer_scope = if self.adapter == "qoder" {
                env.get(OsStr::new("HOME"))
                    .map(|home| peer_cwd.join(home).join(".qoder"))
            } else {
                Some(
                    profile::qwenpaw(&env, &peer_cwd)
                        .map_err(|error| format!("inspect native peer {pid}: {error}"))?,
                )
            };
            let shared = peer_scope
                .and_then(|scope| scope.canonicalize().ok())
                .is_some_and(|scope| scope == self.scope)
                || (self.adapter == "qoder" && peer_cwd == self.command.cwd);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    static PROCESSES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct Child(std::process::Child);
    impl Child {
        fn spawn(command: &mut std::process::Command) -> Self {
            let token = fs::read_to_string("/proc/sys/kernel/random/uuid").unwrap();
            command.env("AW_DISCOVERY_FIXTURE", &token);
            let mut child = Self(command.spawn().unwrap());
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            // Command::spawn alone does not confirm that procfs exposes the
            // executed process's environment. Attach requires a prewarmed host.
            loop {
                if environment(child.0.id()).is_ok_and(|environment| {
                    environment.get(OsStr::new("AW_DISCOVERY_FIXTURE"))
                        == Some(&OsString::from(&token))
                }) {
                    return child;
                }
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "native fixture exited during startup"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "native fixture environment did not become ready"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
    }
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            self.0.wait().unwrap();
        }
    }

    #[test]
    fn copied_native_binary_with_a_different_name_cannot_share_profile() {
        let _processes = PROCESSES.lock().unwrap();
        let root = tempfile::tempdir().unwrap();
        let scope = root.path().join(".qoder");
        fs::create_dir(&scope).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&scope, fs::Permissions::from_mode(0o700)).unwrap();
        let original = root.path().join("qodercli");
        let copied = root.path().join("renamed-copy");
        // Write copies in a separate process so parallel test forks cannot
        // inherit a writer and transiently prevent exec with ETXTBSY.
        for path in [&original, &copied] {
            assert!(std::process::Command::new("/bin/cp")
                .arg("/bin/sleep")
                .arg(path)
                .status()
                .unwrap()
                .success());
        }
        let spawn = |path: &Path| {
            Child::spawn(
                std::process::Command::new(path)
                    .arg("20")
                    .env("HOME", root.path())
                    .current_dir(root.path()),
            )
        };
        let target = spawn(&original);
        let _other = spawn(&copied);
        let result = NativeProcess::read(target.0.id(), "qoder", original.to_str().unwrap());
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("scope is shared"));
        assert!(!scope.join(".aw-attachment.json").exists());
    }

    #[test]
    fn peer_profile_aliases_are_shared_for_both_adapters() {
        let _processes = PROCESSES.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        for adapter in ["qoder", "qwenpaw"] {
            let root = tempfile::tempdir().unwrap();
            let home = root.path().join("home");
            let other = root.path().join("other");
            fs::create_dir(&home).unwrap();
            fs::create_dir(&other).unwrap();
            let profile = home.join(".qoder");
            fs::create_dir(&profile).unwrap();
            fs::set_permissions(&profile, fs::Permissions::from_mode(0o700)).unwrap();
            let alias = other.join("alias");
            symlink(&home, &alias).unwrap();
            let program = root.path().join(adapter);
            symlink("/bin/sleep", &program).unwrap();
            let spawn = |home: &Path, cwd: &Path| {
                Child::spawn(
                    std::process::Command::new(&program)
                        .arg("20")
                        .env("HOME", home)
                        .env("QWENPAW_WORKING_DIR", home.join(".qoder"))
                        .current_dir(cwd),
                )
            };
            let target = spawn(&home, root.path());
            for peer_home in [&alias, &home.join("../home")] {
                let peer = spawn(peer_home, &other);
                let result = NativeProcess::read(target.0.id(), adapter, program.to_str().unwrap());
                let error = result.err().unwrap();
                assert!(
                    error.to_string().contains("scope is shared"),
                    "{adapter}: {error}"
                );
                drop(peer);
            }
        }
    }

    #[test]
    fn relative_and_empty_path_entries_use_the_native_working_directory() {
        let _processes = PROCESSES.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        for entry in ["bin", ""] {
            let root = tempfile::tempdir().unwrap();
            fs::create_dir(root.path().join("bin")).unwrap();
            let scope = root.path().join(".qoder");
            fs::create_dir(&scope).unwrap();
            fs::set_permissions(&scope, fs::Permissions::from_mode(0o700)).unwrap();
            let program = root.path().join(entry).join("qodercli");
            symlink("/bin/sleep", &program).unwrap();
            let target = Child::spawn(
                std::process::Command::new(&program)
                    .arg("20")
                    .env("HOME", root.path())
                    .env("PATH", format!("{entry}:/usr/bin"))
                    .current_dir(root.path()),
            );
            let native = NativeProcess::read(target.0.id(), "qoder", "qodercli").unwrap();
            assert_eq!(native.command.program, program.canonicalize().unwrap());
            native.identity.check().unwrap();
        }
    }

    #[test]
    fn path_lookup_skips_regular_files_without_execute_permission() {
        let _processes = PROCESSES.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        for directory in ["bad", "good", ".qoder"] {
            fs::create_dir(root.path().join(directory)).unwrap();
            fs::set_permissions(
                root.path().join(directory),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        fs::write(root.path().join("bad/qodercli"), "not executable").unwrap();
        fs::set_permissions(
            root.path().join("bad/qodercli"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let program = root.path().join("good/qodercli");
        symlink("/bin/sleep", &program).unwrap();
        let child = Child::spawn(
            std::process::Command::new(&program)
                .arg("20")
                .env("HOME", root.path())
                .env("PATH", "bad:good:/usr/bin")
                .current_dir(root.path()),
        );
        let native = NativeProcess::read(child.0.id(), "qoder", "qodercli").unwrap();
        assert_eq!(native.command.program, program.canonicalize().unwrap());
    }

    #[test]
    fn relative_interpreted_entrypoint_uses_the_selected_process_working_directory() {
        let _processes = PROCESSES.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        for directory in ["bin", ".qoder"] {
            fs::create_dir(root.path().join(directory)).unwrap();
            fs::set_permissions(
                root.path().join(directory),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
        }
        let script = root.path().join("bin/qodercli");
        fs::write(&script, "import time; time.sleep(20)\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let child = Child::spawn(
            std::process::Command::new("/usr/bin/python3")
                .arg("bin/qodercli")
                .env("HOME", root.path())
                .env("PATH", "bin:/usr/bin")
                .current_dir(root.path()),
        );
        let native = NativeProcess::read(child.0.id(), "qoder", "qodercli").unwrap();
        assert_eq!(native.command.program, script.canonicalize().unwrap());
        native.identity.check().unwrap();
    }

    #[test]
    fn renamed_interpreted_peer_cannot_share_native_configuration() {
        let _processes = PROCESSES.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".qoder")).unwrap();
        fs::set_permissions(
            root.path().join(".qoder"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let first = root.path().join("qodercli.py");
        let second = root.path().join("copied.py");
        fs::write(&first, "import time; time.sleep(20)\n").unwrap();
        fs::copy(&first, &second).unwrap();
        let spawn = |script: &Path| {
            Child::spawn(
                std::process::Command::new("python3")
                    .arg(script)
                    .env("HOME", root.path())
                    .current_dir(root.path()),
            )
        };
        let target = spawn(&first);
        let _peer = spawn(&second);
        let error = NativeProcess::read(target.0.id(), "qoder", first.to_str().unwrap())
            .err()
            .unwrap();
        assert!(error.to_string().contains("scope is shared"), "{error}");
    }

    #[test]
    fn qwenpaw_python_options_preserve_profile_isolation_and_command_boundaries() {
        let _processes = PROCESSES.lock().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let other = root.path().join("other");
        for path in [&profile, &other] {
            fs::create_dir(path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let script = root.path().join("qwenpaw.py");
        fs::write(&script, "import time; time.sleep(20)\n").unwrap();
        let spawn = |args: &[&str], profile: &Path| {
            Child::spawn(
                std::process::Command::new("python3")
                    .args(args)
                    .env("QWENPAW_WORKING_DIR", profile)
                    .current_dir(root.path()),
            )
        };
        let target = spawn(&["qwenpaw.py"], &profile);
        let read = || NativeProcess::read(target.0.id(), "qwenpaw", script.to_str().unwrap());
        let peer = spawn(&["-u", "qwenpaw.py"], &other);
        read().unwrap();
        drop(peer);
        let peer = spawn(
            &["-c", "import time; time.sleep(20)", "-m", "qwenpaw"],
            &profile,
        );
        read().unwrap();
        drop(peer);
        let _peer = spawn(&["-W", "ignore", "-u", "qwenpaw.py"], &profile);
        assert!(read()
            .err()
            .unwrap()
            .to_string()
            .contains("scope is shared"));
    }

    #[test]
    fn unrelated_execute_only_file_does_not_require_read_permission() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let unrelated = root.path().join("unrelated");
        fs::write(&unrelated, "different size").unwrap();
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o100)).unwrap();
        assert!(!same_entrypoint(Path::new("/bin/sleep"), &unrelated).unwrap());
    }

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
