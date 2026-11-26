use super::sealed_snapshot;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;

#[test]
fn sealed_hook_snapshot_preserves_bytes_and_blocks_mutation() {
    let content = b"#!/bin/sh\nprintf 'approved-hook'\n";
    let mut snapshot = sealed_snapshot(content).expect("sealed snapshot");
    snapshot.seek(SeekFrom::Start(0)).unwrap();
    let mut actual = Vec::new();
    snapshot.read_to_end(&mut actual).unwrap();
    assert_eq!(actual, content);
    assert_eq!(
        snapshot.metadata().unwrap().permissions().mode() & 0o777,
        0o500
    );
    assert_eq!(
        snapshot.write_all(b"changed").unwrap_err().raw_os_error(),
        Some(1)
    );
    assert_eq!(snapshot.set_len(0).unwrap_err().raw_os_error(), Some(1));
    assert_eq!(snapshot.set_len(4096).unwrap_err().raw_os_error(), Some(1));
    let flags = nix::fcntl::fcntl(snapshot.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFD).unwrap();
    assert_ne!(flags & nix::libc::FD_CLOEXEC, 0);
    let descriptor = snapshot.as_raw_fd();
    drop(snapshot);
    assert!(nix::fcntl::fcntl(descriptor, nix::fcntl::FcntlArg::F_GETFD).is_err());
}
