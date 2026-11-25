//! Native peer discovery and profile isolation tests.

use super::*;
use std::{
    os::unix::fs::symlink,
    time::{Duration, Instant},
};

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
            if environment(&PathBuf::from(format!("/proc/{}", child.0.id()))).is_ok_and(
                |environment| {
                    environment.get(OsStr::new("AW_DISCOVERY_FIXTURE"))
                        == Some(&OsString::from(&token))
                },
            ) {
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

fn wait_for_zombie_leader(entry: &Path) {
    let stat = entry.join("stat");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let text = fs::read_to_string(&stat).unwrap();
        let (_, fields) = text.rsplit_once(") ").unwrap();
        if fields.starts_with("Z ") {
            break;
        }
        assert!(Instant::now() < deadline, "child did not become a zombie");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn zombie_peer_with_unreadable_environment_is_skipped() {
    let _processes = PROCESSES.lock().unwrap();
    let mut child = Child(std::process::Command::new("/bin/true").spawn().unwrap());
    let pid = child.0.id();
    let entry = PathBuf::from(format!("/proc/{pid}"));
    wait_for_zombie_leader(&entry);
    assert!(entry.exists());
    assert!(peer_exited(pid));
    assert!(peer_executable(&entry, pid).is_none());
    if let Err(error) = environment(&entry) {
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(peer_environment(pid, &entry).unwrap().is_none());
    }
    assert!(child.0.wait().unwrap().success());
}

#[test]
fn non_dumpable_peer_preserves_environment_permission_error() {
    let _processes = PROCESSES.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("ready");
    let mut child = Child(
        std::process::Command::new("python3")
            .args([
                "-c",
                "import ctypes, pathlib, sys, time\n\
                 assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0\n\
                 pathlib.Path(sys.argv[1]).touch()\n\
                 time.sleep(20)",
            ])
            .arg(&marker)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(child.0.try_wait().unwrap().is_none(), "fixture exited");
        assert!(
            Instant::now() < deadline,
            "fixture never became non-dumpable"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let pid = child.0.id();
    let entry = PathBuf::from(format!("/proc/{pid}"));
    let Err(error) = fs::read_link(entry.join("exe")) else {
        eprintln!("skipping access-denial check: observer can inspect non-dumpable peer");
        return;
    };
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let error = environment(&entry).unwrap_err();
    assert_eq!(
        error.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert!(!peer_exited(pid));
    let error = peer_environment(pid, &entry).unwrap_err();
    assert!(error.to_string().contains("Permission denied"), "{error}");
}

#[test]
fn exited_leader_with_live_sibling_uses_peer_profile_and_cwd() {
    use std::os::unix::fs::DirBuilderExt;
    let _guard = PROCESSES.lock().unwrap();
    let root = tempfile::tempdir().unwrap();
    let target_home = root.path().join("target");
    let other_home = root.path().join("other");
    for home in [&target_home, &other_home] {
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(home.join(".qoder"))
            .unwrap();
    }
    let program = root.path().join("threaded-fixture");
    // Give the fixture unique executable contents for copied-binary matching.
    assert!(std::process::Command::new("/bin/sh")
        .args([
            "-c",
            r#"cp /usr/bin/python3 "$1" && printf '%s' "$1" >> "$1""#,
            "fixture"
        ])
        .arg(&program)
        .status()
        .unwrap()
        .success());
    let native_child = Child::spawn(
        std::process::Command::new(&program)
            .args(["-c", "import time; time.sleep(20)"])
            .env("HOME", &target_home)
            .current_dir(&target_home),
    );
    let native =
        NativeProcess::read_identity(native_child.0.id(), "qoder", program.to_str().unwrap())
            .unwrap();
    for (home, cwd, shared) in [
        (&other_home, &other_home, false),
        (&target_home, &other_home, true),
        (&other_home, &target_home, true),
    ] {
        let peer = Child(
            std::process::Command::new(&program)
                .args([
                    "-c",
                    "import ctypes, threading, time; \
                     threading.Thread(target=time.sleep, args=(20,)).start(); \
                     ctypes.CDLL(None).pthread_exit(None)",
                ])
                .env("HOME", home)
                .current_dir(cwd)
                .spawn()
                .unwrap(),
        );
        let pid = peer.0.id();
        wait_for_zombie_leader(&PathBuf::from(format!("/proc/{pid}")));
        assert!(!peer_exited(pid));
        let result = native.exclusive();
        if shared {
            assert!(result.unwrap_err().to_string().contains("scope is shared"));
        } else {
            result.unwrap();
        }
    }
}

#[test]
fn qoder_project_check_inspects_each_surviving_cwd() {
    let root = tempfile::tempdir().unwrap();
    let process = root.path().join("42");
    let first = process.join("task/43");
    let other = process.join("task/44");
    for task in [process.join("task/42"), first.clone(), other.clone()] {
        fs::create_dir_all(task).unwrap();
    }
    let project = root.path().join("project");
    symlink(root.path().join("independent"), first.join("cwd")).unwrap();
    symlink(&project, other.join("cwd")).unwrap();
    let shares = |target: &Path| any_peer_cwd(&process, &first, true, |cwd| Ok(cwd == target));
    assert!(shares(&project).unwrap());
    assert!(!shares(&root.path().join("elsewhere")).unwrap());
    fs::remove_file(other.join("cwd")).unwrap();
    assert!(shares(&project).is_err());
}

#[test]
fn peer_environment_preserves_error_when_exit_cannot_be_established() {
    assert!(!peer_exited(u32::MAX));
    let error = peer_environment(u32::MAX, Path::new("/proc/4294967295")).unwrap_err();
    assert!(error.to_string().contains("read native peer"), "{error}");
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
        // Distinct bytes exclude unrelated sleep jobs from copied-binary matching.
        // Write in a child so concurrent forks cannot inherit an executable writer.
        assert!(std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "cp /bin/sleep \"$1\" && printf '%s' \"$1\" >> \"$1\"",
                "fixture"
            ])
            .arg(&program)
            .status()
            .unwrap()
            .success());
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
