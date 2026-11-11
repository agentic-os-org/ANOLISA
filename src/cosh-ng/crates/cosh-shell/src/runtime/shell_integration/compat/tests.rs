use super::*;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    #[cfg(target_os = "linux")]
    let directory = tempfile::tempdir_in("/dev/shm").unwrap();
    #[cfg(not(target_os = "linux"))]
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("home");
    let module = root.join(MODULE.trim_start_matches('/'));
    std::fs::create_dir_all(module.parent().unwrap()).unwrap();
    std::fs::create_dir(&home).unwrap();
    for ancestor in module.parent().unwrap().ancestors() {
        if !ancestor.starts_with(&root) {
            break;
        }
        std::fs::set_permissions(ancestor, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    for directory in [&root, &home] {
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    std::fs::write(&module, MODULE_BYTES).unwrap();
    std::fs::set_permissions(&module, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(snapshot(&root, &home).is_ok());
    (directory, home, module)
}

fn snapshot(root: &Path, home: &Path) -> Result<String, ()> {
    let uid = unsafe { nix::libc::geteuid() };
    verify_at(root, home, uid, uid)
}

#[test]
fn unrelated_user_configuration_edits_do_not_disable_runtime_compatibility() {
    let (directory, home, _) = fixture();
    let root = directory.path().canonicalize().unwrap();
    let before = snapshot(&root, &home).unwrap();
    for (path, content) in [
        (".bashrc", "alias ll='ls -l'\n"),
        (".bash_profile", "export EDITOR=vi\n"),
        (".profile", "# custom preferences\n"),
        (".bash_history", "true\n"),
    ] {
        std::fs::write(home.join(path), content).unwrap();
        assert_eq!(snapshot(&root, &home).unwrap(), before);
    }
    std::fs::create_dir_all(root.join("etc/profile.d")).unwrap();
    std::fs::write(
        root.join("etc/profile.d/customer.sh"),
        b"# customer setup\n",
    )
    .unwrap();
    assert_eq!(snapshot(&root, &home).unwrap(), before);
}

#[test]
fn module_replacement_invalidates_the_session_without_binding_a_release_version() {
    let (directory, home, module) = fixture();
    let root = directory.path().canonicalize().unwrap();
    let before = snapshot(&root, &home).unwrap();
    let replacement = module.with_extension("new");
    std::fs::write(&replacement, MODULE_BYTES).unwrap();
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::rename(&replacement, &module).unwrap();
    assert_ne!(snapshot(&root, &home).unwrap(), before);
    std::fs::write(&module, b"# different protocol module\n").unwrap();
    assert!(snapshot(&root, &home).is_err());
}

#[test]
fn missing_or_symlinked_or_hardlinked_module_is_rejected() {
    let (directory, home, module) = fixture();
    let root = directory.path().canonicalize().unwrap();
    let saved = module.with_extension("saved");
    std::fs::rename(&module, &saved).unwrap();
    assert!(snapshot(&root, &home).is_err());
    std::os::unix::fs::symlink(&saved, &module).unwrap();
    assert!(snapshot(&root, &home).is_err());
    std::fs::remove_file(&module).unwrap();
    std::fs::hard_link(&saved, &module).unwrap();
    assert!(snapshot(&root, &home).is_err());
}

#[test]
fn writable_module_or_parent_or_home_is_rejected() {
    let (directory, home, module) = fixture();
    let root = directory.path().canonicalize().unwrap();
    for path in [&module, module.parent().unwrap(), &home] {
        let old = std::fs::metadata(path).unwrap().permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(old.mode() | 0o020))
            .unwrap();
        assert!(snapshot(&root, &home).is_err(), "{}", path.display());
        std::fs::set_permissions(path, old).unwrap();
        assert!(snapshot(&root, &home).is_ok());
    }
}

#[test]
fn symlinked_module_ancestor_or_home_is_rejected() {
    let (directory, home, module) = fixture();
    let root = directory.path().canonicalize().unwrap();
    for path in [module.parent().unwrap(), home.as_path()] {
        let saved = path.with_extension("saved");
        std::fs::rename(path, &saved).unwrap();
        std::os::unix::fs::symlink(&saved, path).unwrap();
        assert!(snapshot(&root, &home).is_err());
        std::fs::remove_file(path).unwrap();
        std::fs::rename(&saved, path).unwrap();
    }
}

#[test]
fn unowned_module_and_home_are_rejected() {
    let (directory, home, _) = fixture();
    let root = directory.path().canonicalize().unwrap();
    let uid = unsafe { nix::libc::geteuid() };
    assert!(verify_at(&root, &home, uid.wrapping_add(1), uid).is_err());
    assert!(verify_at(&root, &home, uid, uid.wrapping_add(1)).is_err());
}
