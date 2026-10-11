//! Kernel permission checks run only in a disposable, single-threaded CLI child.

use super::Error;
use nix::{
    errno::Errno,
    unistd::{
        access, getegid, geteuid, getgid, getuid, initgroups, setgid, setuid, AccessFlags, Gid, Uid,
    },
};
use std::{ffi::CString, fs, path::Path};

pub(super) fn probe(user: &str, uid: u32, gid: u32, shell: &Path) -> Result<bool, Error> {
    if geteuid().is_root() {
        let user =
            CString::new(user).map_err(|_| Error::Invalid("account name contains NUL".into()))?;
        // Drop supplementary groups before dropping the privilege needed to set
        // them. The kernel then evaluates directory search, ACLs and mount flags.
        initgroups(&user, Gid::from_raw(gid))
            .and_then(|()| setgid(Gid::from_raw(gid)))
            .and_then(|()| setuid(Uid::from_raw(uid)))
            .map_err(|error| Error::Backend(format!("prepare account access probe: {error}")))?;
    } else if getuid().as_raw() != uid
        || geteuid().as_raw() != uid
        || getgid().as_raw() != gid
        || getegid().as_raw() != gid
    {
        return Err(Error::Permission);
    }
    match access(shell, AccessFlags::X_OK) {
        Ok(()) => Ok(fs::metadata(shell)?.is_file()),
        Err(Errno::EACCES | Errno::ENOENT | Errno::ENOTDIR) => Ok(false),
        Err(error) => Err(std::io::Error::from(error).into()),
    }
}
