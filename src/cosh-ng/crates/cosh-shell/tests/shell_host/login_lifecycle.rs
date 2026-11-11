use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn run_missing_backend_preinstall(directory: &Path) -> std::process::Output {
    use std::os::unix::fs::PermissionsExt;
    let state = directory.join("login");
    let stat = directory.join("stat-fixture");
    std::fs::write(&stat, "#!/usr/bin/env python3\nimport os, stat, sys\ns = os.lstat(sys.argv[-1])\nprint(s.st_uid, format(stat.S_IMODE(s.st_mode), 'o'), s.st_nlink, s.st_size)\n").unwrap();
    std::fs::set_permissions(&stat, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packaging/rpm/cosh-ng.spec.in"
    ));
    let script = spec
        .split_once("%pre\n")
        .unwrap()
        .1
        .split_once("\n%preun\n")
        .unwrap()
        .0;
    let mut script = script
        .replace(
            "for cosh_parent in / /var /var/lib /var/lib/cosh /var/lib/cosh/login; do",
            &format!(
                "for cosh_parent in '{}' '{}'; do",
                directory.display(),
                state.display()
            ),
        )
        .replace(
            "/usr/libexec/cosh/login-preflight",
            &directory.join("missing-backend").display().to_string(),
        )
        .replace("/var/lib/cosh/login", &state.display().to_string())
        .replace(
            "[ \"$1\" -eq 0 ]",
            &format!("[ \"$1\" -eq {} ]", unsafe { nix::libc::geteuid() }),
        );
    if !cfg!(target_os = "linux") {
        script = script.replace("/usr/bin/stat", &stat.display().to_string());
    }
    std::process::Command::new("/bin/sh")
        .args(["-c", &script])
        .output()
        .unwrap()
}

#[test]
fn missing_old_backend_allows_only_absent_empty_or_safe_lock_state() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(run_missing_backend_preinstall(&root).status.success());
    let state = root.join("login");
    std::fs::create_dir(&state).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        run_missing_backend_preinstall(&root).status.success(),
        "empty state must not prevent reinstall"
    );
    std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(state.join(".lock"))
        .unwrap();
    assert!(run_missing_backend_preinstall(&root).status.success());
    let restored = serde_json::json!({"schema":1,"username":"alice","uid":1001,
        "original_shell":"/bin/bash","cosh_shell":"/usr/bin/cosh",
        "original_sha256":"a".repeat(64),"phase":"restored"});
    std::fs::write(
        state.join("1001.json"),
        serde_json::to_vec(&restored).unwrap(),
    )
    .unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::remove_file(state.join("1001.json")).unwrap();
    std::fs::write(state.join("unknown"), b"unknown").unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::remove_file(state.join("unknown")).unwrap();
    std::fs::set_permissions(state.join(".lock"), std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::set_permissions(state.join(".lock"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(state.join(".lock"), root.join("lock-alias")).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::remove_file(root.join("lock-alias")).unwrap();
    std::fs::write(state.join(".lock"), vec![b'x'; 8193]).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::write(state.join(".lock"), b"").unwrap();
    assert!(run_missing_backend_preinstall(&root).status.success());
    std::fs::remove_file(state.join(".lock")).unwrap();
    symlink(root.join("missing"), state.join(".lock")).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::remove_file(state.join(".lock")).unwrap();
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&state, root.join("real-state")).unwrap();
    symlink(root.join("real-state"), &state).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::remove_file(&state).unwrap();
    std::fs::rename(root.join("real-state"), &state).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(!run_missing_backend_preinstall(&root).status.success());
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn packaged_shell_assets_parse_and_noninteractive_loader_is_silent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/login");
    for name in ["login-preflight", "cosh-switch"] {
        assert!(std::process::Command::new("/bin/sh")
            .arg("-n")
            .arg(root.join(name))
            .status()
            .unwrap()
            .success());
    }
    let loader = root.join("../system-integration/bashrc-source.bash");
    assert!(std::process::Command::new("/bin/bash")
        .arg("-n")
        .arg(&loader)
        .status()
        .unwrap()
        .success());
    let output = std::process::Command::new("/bin/bash")
        .args(["--noprofile", "--norc", "-c", "source \"$1\"", "bash"])
        .arg(loader)
        .env_clear()
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
