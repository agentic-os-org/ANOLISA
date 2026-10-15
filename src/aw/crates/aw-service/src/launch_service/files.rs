//! Private startup files are never repaired by chmod or followed through symlinks.

use super::{process, ServicePaths};
use crate::{rejected, Result};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub(super) fn uid() -> u32 {
    // geteuid has no pointer arguments or preconditions.
    unsafe { libc::geteuid() }
}

pub(super) fn check_private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o7777 != 0o700 {
        return Err(rejected("unsafe_service_directory"));
    }
    Ok(())
}

pub(super) fn check_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| rejected("service_path_has_no_parent"))?;
    let metadata = fs::symlink_metadata(parent)?;
    if !metadata.is_dir() || metadata.uid() != uid() || metadata.mode() & 0o022 != 0 {
        return Err(rejected("unsafe_service_parent"));
    }
    Ok(())
}

pub(super) fn private_directory(path: &Path) -> Result<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    check_private_directory(path)
}

fn check_file(file: &File, mode: u32) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid()
        || metadata.mode() & 0o7777 != mode
        || metadata.nlink() != 1
    {
        return Err(rejected("unsafe_service_startup_file"));
    }
    Ok(())
}

fn open_private(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    check_file(&file, 0o600)?;
    Ok(file)
}

pub(super) fn lock(state: &Path, deadline: Instant) -> Result<File> {
    let file = open_private(&state.join("launch.lock"))?;
    loop {
        let remaining = process::remaining(deadline)?;
        // The lock is held until discovery or owned startup has completed.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error.into());
        }
        thread::sleep(Duration::from_millis(5).min(remaining));
    }
}

pub(super) fn snapshot(paths: &ServicePaths, bytes: &[u8]) -> Result<PathBuf> {
    let path = paths
        .state_dir
        .join(format!("config-{}.yaml", paths.config_revision));
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o400)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(bytes)?;
            file.sync_all()?;
            File::open(&paths.state_dir)?.sync_all()?;
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)?;
    check_file(&file, 0o400)?;
    let mut retained = Vec::new();
    file.take(aw_config::MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut retained)?;
    if retained != bytes {
        return Err(rejected("service_snapshot_mismatch"));
    }
    Ok(path)
}

pub(super) fn log(state: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(state.join("service.log"))?;
    check_file(&file, 0o600)?;
    Ok(file)
}
