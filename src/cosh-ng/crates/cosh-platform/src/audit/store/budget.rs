use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path};

use nix::dir::Dir;
use nix::fcntl::{openat, AtFlags, Flock, FlockArg, OFlag};
use nix::sys::stat::{fstat, fstatat, FileStat, Mode, SFlag};

fn directory_flags() -> OFlag {
    OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC
}

fn open_root(root: &Path) -> io::Result<Dir> {
    let mut directory = Dir::open("/", directory_flags(), Mode::empty())?;
    for component in root.components() {
        match component {
            Component::RootDir => (),
            Component::Normal(name) => {
                directory = Dir::openat(
                    Some(directory.as_raw_fd()),
                    name,
                    directory_flags(),
                    Mode::empty(),
                )?;
            }
            _ => return Err(unsafe_entry()),
        }
    }
    validate(&fstat(directory.as_raw_fd())?, SFlag::S_IFDIR)?;
    Ok(directory)
}

fn child_directory(parent: &Dir, name: &std::ffi::CStr) -> io::Result<Dir> {
    let directory = Dir::openat(
        Some(parent.as_raw_fd()),
        name,
        directory_flags(),
        Mode::empty(),
    )?;
    validate(&fstat(directory.as_raw_fd())?, SFlag::S_IFDIR)?;
    Ok(directory)
}

pub(super) fn lock(root_path: &Path) -> io::Result<Flock<File>> {
    let root = open_root(root_path)?;
    let v1 = child_directory(&root, c"v1")?;
    let fd = openat(
        Some(v1.as_raw_fd()),
        "write-budget.lock",
        OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?;
    let opened = fstat(fd);
    let closed = nix::unistd::close(fd);
    validate(&opened?, SFlag::S_IFREG)?;
    closed?;
    // Standard File owns the reopened fd; verify it against the anchored parent before locking.
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
        .open(root_path.join("v1/write-budget.lock"))?;
    let actual = fstat(file.as_raw_fd())?;
    let expected = fstatat(
        Some(v1.as_raw_fd()),
        c"write-budget.lock",
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    validate(&actual, SFlag::S_IFREG)?;
    if !same_file(&actual, &expected) {
        return Err(unsafe_entry());
    }
    Flock::lock(file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, error)| io::Error::from_raw_os_error(error as i32))
}

pub(super) fn admit(
    root: &Path,
    guard: &File,
    active: &File,
    cap: u64,
    buffered: u64,
    incoming: u64,
) -> io::Result<()> {
    let mut total = buffered.checked_add(incoming).ok_or_else(full)?;
    if total > cap {
        return Err(full());
    }
    let root = open_root(root)?;
    let v1 = child_directory(&root, c"v1")?;
    let lock_stat = fstatat(
        Some(v1.as_raw_fd()),
        c"write-budget.lock",
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    let held_stat = fstat(guard.as_raw_fd())?;
    validate(&lock_stat, SFlag::S_IFREG)?;
    if !same_file(&lock_stat, &held_stat) {
        return Err(unsafe_entry());
    }
    let active_stat = fstat(active.as_raw_fd())?;
    validate(&active_stat, SFlag::S_IFREG)?;
    let mut segments = child_directory(&v1, c"segments")?;
    let mut inspected = 0;
    let mut counted_active = false;
    let segments_fd = segments.as_raw_fd();
    for date in segments.iter() {
        let date = date?;
        let name = date.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        inspect(&mut inspected)?;
        let mut directory = Dir::openat(Some(segments_fd), name, directory_flags(), Mode::empty())?;
        validate(&fstat(directory.as_raw_fd())?, SFlag::S_IFDIR)?;
        let directory_fd = directory.as_raw_fd();
        for entry in directory.iter() {
            let entry = entry?;
            let name = entry.file_name();
            if name == c"." || name == c".." {
                continue;
            }
            inspect(&mut inspected)?;
            let bytes = name.to_bytes();
            if !bytes.ends_with(b".jsonl") && !bytes.ends_with(b".jsonl.active") {
                continue;
            }
            let stat = fstatat(Some(directory_fd), name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
            validate(&stat, SFlag::S_IFREG)?;
            counted_active |= same_file(&stat, &active_stat);
            total = total.checked_add(stat.st_size as u64).ok_or_else(full)?;
            if total > cap {
                return Err(full());
            }
        }
    }
    if !counted_active {
        return Err(unsafe_entry());
    }
    Ok(())
}

fn inspect(count: &mut usize) -> io::Result<()> {
    *count += 1;
    if *count > 4096 {
        return Err(io::Error::other("audit budget scan limit reached"));
    }
    Ok(())
}

fn same_file(left: &FileStat, right: &FileStat) -> bool {
    left.st_dev == right.st_dev && left.st_ino == right.st_ino
}

fn validate(stat: &FileStat, kind: SFlag) -> io::Result<()> {
    if SFlag::from_bits_truncate(stat.st_mode) != kind
        || stat.st_uid != nix::unistd::Uid::effective().as_raw()
        || stat.st_mode & 0o077 != 0
        || (kind == SFlag::S_IFREG && stat.st_nlink != 1)
        || stat.st_size < 0
    {
        return Err(unsafe_entry());
    }
    Ok(())
}

fn full() -> io::Error {
    io::Error::other("per-user audit disk budget exhausted")
}

fn unsafe_entry() -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, "unsafe audit budget entry")
}
