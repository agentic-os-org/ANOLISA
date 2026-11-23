use super::*;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[test]
fn qualified_rpm_build_is_explicit_for_the_supported_architecture() {
    validate_build(
        b"4.18.2-6.alnx4.aarch64\n4.18.2-6.alnx4.aarch64\n",
        "aarch64",
    )
    .unwrap();
    assert!(validate_build(
        b"4.18.2-7.alnx4.aarch64\n4.18.2-7.alnx4.aarch64\n",
        "aarch64"
    )
    .is_err());
    assert!(validate_build(b"4.18.2-6.alnx4.aarch64\n", "aarch64").is_err());
    assert!(validate_build(
        b"4.18.2-6.alnx4.aarch64\n4.18.2-6.alnx4.aarch64\n",
        "x86_64"
    )
    .is_err());
    assert!(validate_build(b"", "riscv64").is_err());
}

#[test]
fn rejects_effective_paths_outside_the_qualified_rpm_database() {
    validate_effective_path(format!("{LOCK_PATH}\n").as_bytes()).unwrap();
    for invalid in [
        b"/var/lib/rpm/.rpm.lock\n".as_slice(),
        b"/tmp/.rpm.lock\n",
        b"%{_rpmlock_path}\n",
        b"/usr/lib/sysimage/rpm/.rpm.lock\nextra\n",
    ] {
        assert!(validate_effective_path(invalid).is_err());
    }
}

#[test]
fn validates_the_open_lock_inode_instead_of_only_the_path() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rpm.lock");
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o644)
        .open(&path)
        .unwrap();
    let owner = unsafe { nix::libc::geteuid() };
    validate_metadata(&file, owner).unwrap();
    assert!(validate_metadata(&file, owner.wrapping_add(1)).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert!(validate_metadata(&file, owner).is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::hard_link(&path, directory.path().join("alias")).unwrap();
    assert!(validate_metadata(&file, owner).is_err());
}

#[cfg(not(target_os = "linux"))]
#[test]
fn unsupported_platform_does_not_construct_a_native_rpm_lease() {
    let temporary = tempfile::NamedTempFile::new().unwrap();
    assert!(NativeLease::lock_file(temporary.reopen().unwrap()).is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn ofd_reader_survives_descriptor_copy_and_blocks_the_native_posix_writer() {
    use std::os::fd::AsRawFd;
    let temporary = tempfile::NamedTempFile::new().unwrap();
    let lease = NativeLease::lock_file(temporary.reopen().unwrap()).unwrap();
    assert!(lease.file.as_raw_fd() >= 4);
    assert_ne!(
        unsafe { nix::libc::fcntl(lease.file.as_raw_fd(), nix::libc::F_GETFD) }
            & nix::libc::FD_CLOEXEC,
        0
    );
    let inherited = lease.file.try_clone().unwrap();
    let writer = temporary.reopen().unwrap();
    let mut write_lock: nix::libc::flock = unsafe { std::mem::zeroed() };
    write_lock.l_type = nix::libc::F_WRLCK as _;
    write_lock.l_whence = nix::libc::SEEK_SET as _;
    let acquire_writer = || unsafe {
        nix::libc::fcntl(
            writer.as_raw_fd(),
            nix::libc::F_SETLK,
            &write_lock as *const nix::libc::flock,
        )
    };
    assert_eq!(acquire_writer(), -1);
    drop(lease);
    assert_eq!(acquire_writer(), -1);
    drop(inherited);
    assert_eq!(acquire_writer(), 0);
}
