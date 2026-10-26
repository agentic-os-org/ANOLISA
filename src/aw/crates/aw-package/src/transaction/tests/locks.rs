//! Pin the fork-before-exec window that can outlive a parent's lock guard.

use super::*;
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    time::Duration,
};

struct Release(UnixStream);

impl Drop for Release {
    fn drop(&mut self) {
        let _ = self.0.write_all(b"x");
    }
}

fn during_fork(child_action: impl Fn() + Send + Sync + 'static, body: impl FnOnce()) {
    let (mut parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    parent
        .set_write_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    std::thread::scope(|scope| {
        let spawned = scope.spawn(move || {
            let mut command = Command::new("/bin/true");
            // SAFETY: after fork the closure uses only async-signal-safe syscalls,
            // preallocated stack data and a borrowed descriptor; no Rust allocation.
            unsafe {
                command.pre_exec(move || {
                    child_action();
                    let fd = child.as_raw_fd();
                    let byte = b"x";
                    if libc::write(fd, byte.as_ptr().cast(), 1) != 1 {
                        libc::_exit(120);
                    }
                    let mut ready = libc::pollfd {
                        fd,
                        events: libc::POLLIN,
                        revents: 0,
                    };
                    let mut reply = 0u8;
                    if libc::poll(&mut ready, 1, 10000) != 1
                        || libc::read(fd, (&mut reply as *mut u8).cast(), 1) != 1
                    {
                        libc::_exit(121);
                    }
                    Ok(())
                });
            }
            command.status().unwrap()
        });
        // Release before scope joins, including an assertion unwind in body.
        let mut release = Release(parent.try_clone().unwrap());
        let mut ready = [0];
        parent.read_exact(&mut ready).unwrap();
        body();
        release.0.write_all(b"x").unwrap();
        assert!(spawned.join().unwrap().success());
    });
}

fn released_before_child_exec<T>(path: &Path, acquire: impl FnOnce() -> T) {
    let held = acquire();
    let contender = disk::File::open(path).unwrap();
    assert!(
        contender.try_lock().is_err(),
        "live owner must exclude writers"
    );
    during_fork(
        || {},
        || {
            drop(held);
            contender
                .try_lock()
                .expect("an unrelated child must not extend the lock lifetime");
        },
    );
    // Other concurrent cases may have forked while this test held the contender.
    contender.unlock().unwrap();
}

#[test]
fn transaction_locks_release_while_an_unrelated_child_is_before_exec() {
    let fixture = Fixture::new();
    let path = fixture.root.join("lock");
    fixture.record(&path, b"", 0o644);
    released_before_child_exec(&path, || fs::lock(&path, false).unwrap());
    disk::remove_file(&path).unwrap();
    released_before_child_exec(&path, || fs::new_lock(&path).unwrap());
}

#[test]
fn inventory_locks_release_while_an_unrelated_child_is_before_exec() {
    let fixture = Fixture::new();
    let path = fixture.root.join("lock");
    fixture.record(&path, b"", 0o644);
    released_before_child_exec(&path, || crate::database::shared_file_lock(&path).unwrap());
}

#[test]
fn installation_locks_release_while_an_unrelated_child_is_before_exec() {
    let fixture = Fixture::new();
    fixture.directory(&fixture.prefix);
    let path = fixture.prefix.join(".aw-install.lock");
    fixture.record(&path, b"", 0o644);
    released_before_child_exec(&path, || crate::filesystem::lock(&fixture.prefix).unwrap());
    released_before_child_exec(&path, || {
        crate::filesystem::shared_lock(&fixture.prefix).unwrap()
    });
}

#[test]
fn runtime_leases_release_while_an_unrelated_child_is_before_exec() {
    // TMPDIR may have writable ancestors that runtime admission rejects.
    let root = tempfile::tempdir_in("/tmp").unwrap();
    disk::set_permissions(root.path(), disk::Permissions::from_mode(0o700)).unwrap();
    let fixture = Fixture::at(root.path());
    fixture.directory(&fixture.prefix.join(".aw-packages"));
    let path = fixture.prefix.join(".aw-install.lock");
    fixture.record(&path, b"", 0o644);
    released_before_child_exec(&path, || {
        aw_exec::PackageLeases::for_paths([fixture.prefix.clone()]).unwrap()
    });
}

#[test]
fn dropping_a_fork_copy_does_not_unlock_the_parent_owner() {
    let fixture = Fixture::new();
    let path = fixture.root.join("lock");
    fixture.record(&path, b"", 0o644);
    let mut held = Box::new(fs::lock(&path, false).unwrap());
    let address = (&mut *held as *mut aw_exec::PackageLock) as usize;
    let contender = disk::File::open(&path).unwrap();
    during_fork(
        move || {
            // SAFETY: only the fork child drops its private copy at this stable
            // address. PackageLock/File drop use only getpid/flock/close; the Box
            // allocation is left intact until exec. The parent retains its guard.
            unsafe { std::ptr::drop_in_place(address as *mut aw_exec::PackageLock) };
        },
        || assert!(contender.try_lock().is_err(), "parent still owns the lock"),
    );
    drop(held);
    contender.try_lock().unwrap();
}
