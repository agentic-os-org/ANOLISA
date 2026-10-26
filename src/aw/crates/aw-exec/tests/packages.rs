//! Runtime leases are independent of package-record contents and never create files.

#![cfg(target_os = "linux")]

use aw_exec::{CommandSpec, LeaseError, PackageLeases};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    os::unix::fs::{symlink, DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        // Runner checkouts may have writable ancestors, unlike installed prefixes.
        let id = fs::read_to_string("/proc/sys/kernel/random/uuid").unwrap();
        let path = Path::new("/tmp").join(format!("aw-package-leases-{}", id.trim()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }

    fn prefix(&self, name: &str) -> PathBuf {
        let prefix = self.0.join(name);
        fs::create_dir_all(prefix.join(".aw-packages")).unwrap();
        fs::create_dir(prefix.join("bin")).unwrap();
        for directory in [&prefix, &prefix.join(".aw-packages"), &prefix.join("bin")] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::write(prefix.join(".aw-install.lock"), []).unwrap();
        fs::set_permissions(
            prefix.join(".aw-install.lock"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();
        fs::write(prefix.join("bin/runtime"), b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            prefix.join("bin/runtime"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        prefix
    }

    fn lock(prefix: &Path) -> File {
        File::open(prefix.join(".aw-install.lock")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn active_shared_lease_blocks_mutation_until_the_last_owner_releases_it() {
    let fixture = Fixture::new();
    let prefix = fixture.prefix("installation");
    let first = PackageLeases::for_paths([prefix.join("bin/runtime")]).unwrap();
    let second = PackageLeases::for_paths([prefix.join("bin/runtime")]).unwrap();
    let writer = Fixture::lock(&prefix);
    assert!(writer.try_lock().is_err());
    drop(first);
    assert!(writer.try_lock().is_err());
    drop(second);
    writer.try_lock().unwrap();
}

#[test]
fn exclusive_mutation_rejects_new_runtime_and_drops_partial_shared_leases() {
    let fixture = Fixture::new();
    let first = fixture.prefix("a");
    let second = fixture.prefix("z");
    let writer = Fixture::lock(&second);
    writer.try_lock().unwrap();
    assert!(matches!(
        PackageLeases::for_paths([first.join("bin/runtime"), second.join("bin/runtime")]),
        Err(LeaseError::Busy(path)) if path == second
    ));
    Fixture::lock(&first).try_lock().unwrap();
    drop(writer);
    PackageLeases::for_paths([second.join("bin/runtime")]).unwrap();
}

#[test]
fn path_resolution_and_symbolic_aliases_hold_the_actual_managed_executable() {
    let fixture = Fixture::new();
    let prefix = fixture.prefix("installation");
    let alias = fixture.0.join("alias");
    symlink(prefix.join("bin"), &alias).unwrap();
    let mut command = CommandSpec {
        program: "runtime".into(),
        args: Vec::new(),
        cwd: fixture.0.clone(),
        environment: BTreeMap::from([("PATH".into(), alias.clone().into_os_string())]),
    };
    let lease = PackageLeases::acquire([&mut command]).unwrap();
    assert_eq!(command.program, alias.join("runtime"));
    assert!(Fixture::lock(&prefix).try_lock().is_err());
    drop(lease);
    Fixture::lock(&prefix).try_lock().unwrap();
}

#[test]
fn absolute_argument_paths_protect_backends_even_when_the_executable_is_unmanaged() {
    let fixture = Fixture::new();
    let first = fixture.prefix("backend");
    let second = fixture.prefix("rtk");
    let mut command = CommandSpec {
        program: "/bin/sh".into(),
        args: vec![
            first.join("bin/runtime").into_os_string(),
            format!("--backend={}", second.join("bin/runtime").display()).into(),
        ],
        cwd: fixture.0.clone(),
        environment: BTreeMap::new(),
    };
    let lease = PackageLeases::acquire([&mut command]).unwrap();
    for prefix in [&first, &second] {
        assert!(Fixture::lock(prefix).try_lock().is_err());
    }
    drop(lease);
    for prefix in [&first, &second] {
        Fixture::lock(prefix).try_lock().unwrap();
    }
}

#[test]
fn missing_symbolic_hardlinked_and_writable_locks_are_never_treated_as_unmanaged() {
    let fixture = Fixture::new();
    let prefix = fixture.prefix("installation");
    let lock = prefix.join(".aw-install.lock");
    fs::remove_file(&lock).unwrap();
    assert!(PackageLeases::for_paths([prefix.join("bin/runtime")]).is_err());
    let outside = fixture.0.join("outside");
    fs::write(&outside, []).unwrap();
    symlink(&outside, &lock).unwrap();
    assert!(PackageLeases::for_paths([prefix.join("bin/runtime")]).is_err());
    fs::remove_file(&lock).unwrap();
    fs::hard_link(&outside, &lock).unwrap();
    assert!(matches!(
        PackageLeases::for_paths([prefix.join("bin/runtime")]),
        Err(LeaseError::Unsafe(_))
    ));
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, []).unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(matches!(
        PackageLeases::for_paths([prefix.join("bin/runtime")]),
        Err(LeaseError::Unsafe(_))
    ));
}

#[test]
fn shared_lease_lookup_does_not_create_records_or_locks() {
    let fixture = Fixture::new();
    let uninstalled = fixture.0.join("new/output");
    let leases = PackageLeases::for_paths([uninstalled]).unwrap();
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    drop(leases);
    let mut command = CommandSpec {
        program: "runtime".into(),
        args: Vec::new(),
        cwd: fixture.0.clone(),
        environment: BTreeMap::new(),
    };
    assert!(matches!(
        PackageLeases::acquire([&mut command]),
        Err(LeaseError::Invalid(_))
    ));
    assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
}

#[test]
fn absent_arguments_retain_managed_symbolic_ancestor_protection() {
    let fixture = Fixture::new();
    let prefix = fixture.prefix("installation");
    let alias = fixture.0.join("alias");
    symlink(prefix.join("bin"), &alias).unwrap();
    let lease = PackageLeases::for_paths([alias.join("missing/child")]).unwrap();
    assert!(Fixture::lock(&prefix).try_lock().is_err());
    drop(lease);
    Fixture::lock(&prefix).try_lock().unwrap();
    fs::remove_file(&alias).unwrap();
    symlink(prefix.join("missing"), &alias).unwrap();
    assert!(PackageLeases::for_paths([alias.join("child")]).is_err());
}

#[test]
fn symbolic_parent_components_follow_filesystem_resolution() {
    let fixture = Fixture::new();
    let prefix = fixture.prefix("installation");
    let alias = fixture.0.join("alias");
    symlink(prefix.join("bin"), &alias).unwrap();
    let lease = PackageLeases::for_paths([alias.join("../bin/runtime")]).unwrap();
    assert!(Fixture::lock(&prefix).try_lock().is_err());
    drop(lease);
    fs::remove_file(&alias).unwrap();
    symlink(&prefix, &alias).unwrap();
    let lease = PackageLeases::for_paths([alias.join("bin/runtime")]).unwrap();
    assert!(Fixture::lock(&prefix).try_lock().is_err());
    drop(lease);
    Fixture::lock(&prefix).try_lock().unwrap();
}
