//! Extended-attribute (Package T3) helpers.
//!
//! Includes the `user.`-namespace classifier, the `PathType`-level
//! gate used by the `*xattr` FUSE callbacks, and thin `l{get,list,set,
//! remove}xattr` libc wrappers that surface errno as a plain `i32` for
//! the FUSE reply path.

use std::path::Path;

use crate::path::{PathType, is_skill_discover_path};
use crate::sys::errno;

/// Classification of an xattr name's namespace prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum XattrNamespace {
    /// Belongs to the `user.` namespace.
    User,
    /// Disallowed: `security.`, `trusted.`, `system.`, missing namespace
    /// prefix, or any other namespace SkillFS does not pass through in T3.
    Disallowed,
}

pub(crate) fn xattr_namespace(name: &std::ffi::OsStr) -> XattrNamespace {
    use std::os::unix::ffi::OsStrExt;
    let bytes = name.as_bytes();
    if bytes.starts_with(b"user.") && bytes.len() > b"user.".len() {
        XattrNamespace::User
    } else {
        XattrNamespace::Disallowed
    }
}

/// Returns `true` for path types whose physical leaf can host an xattr in
/// T3 — only ordinary passthrough leaves under a non-`skill-discover` skill
/// qualify. Other path types are rejected before any libc work.
pub(crate) fn path_type_supports_xattr_passthrough(path_type: &PathType) -> bool {
    match path_type {
        PathType::Passthrough { skill_name, .. } => !is_skill_discover_path(skill_name),
        _ => false,
    }
}

/// Filter a null-separated list of xattr names (as produced by `llistxattr`)
/// to entries starting with `user.`. Returns a fresh null-separated buffer
/// suitable for the FUSE `listxattr` reply.
pub(crate) fn filter_user_xattr_list(raw: &[u8]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    for entry in raw.split(|b| *b == 0u8) {
        if entry.is_empty() {
            continue;
        }
        if entry.starts_with(b"user.") {
            out.extend_from_slice(entry);
            out.push(0u8);
        }
    }
    out
}

fn cstring_from_path(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

fn cstring_from_xattr_name(name: &std::ffi::OsStr) -> std::io::Result<std::ffi::CString> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    CString::new(name.as_bytes()).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
}

/// `lgetxattr` wrapper. Returns the xattr value on success; on error
/// returns the underlying errno (or `EIO` as a fallback). When `size` is
/// `0` the function still allocates a one-byte probe so it can return the
/// real value length via a follow-up `lgetxattr(NULL, 0)` size query.
pub(crate) fn xattr_lget(path: &Path, name: &std::ffi::OsStr, size: usize) -> Result<Vec<u8>, i32> {
    let c_path = cstring_from_path(path).map_err(|e| errno(&e))?;
    let c_name = cstring_from_xattr_name(name).map_err(|e| errno(&e))?;
    let needed =
        unsafe { libc::lgetxattr(c_path.as_ptr(), c_name.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    let needed = needed as usize;
    if size == 0 {
        return Ok(vec![0u8; needed]); // length is what the caller wants
    }
    if needed > size {
        return Err(libc::ERANGE);
    }
    let mut buf = vec![0u8; needed];
    let got = unsafe {
        libc::lgetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// `llistxattr` wrapper that returns the full physical null-separated name
/// list, sized via a probing call so the caller does not have to guess.
pub(crate) fn xattr_llist(path: &Path) -> Result<Vec<u8>, i32> {
    let c_path = cstring_from_path(path).map_err(|e| errno(&e))?;
    let needed = unsafe { libc::llistxattr(c_path.as_ptr(), std::ptr::null_mut(), 0) };
    if needed < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    let needed = needed as usize;
    if needed == 0 {
        return Ok(Vec::new());
    }
    let mut buf = vec![0u8; needed];
    let got = unsafe {
        libc::llistxattr(
            c_path.as_ptr(),
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
        )
    };
    if got < 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    buf.truncate(got as usize);
    Ok(buf)
}

/// `lsetxattr` wrapper. Preserves the kernel's `XATTR_CREATE` /
/// `XATTR_REPLACE` flag semantics.
pub(crate) fn xattr_lset(
    path: &Path,
    name: &std::ffi::OsStr,
    value: &[u8],
    flags: i32,
) -> Result<(), i32> {
    let c_path = cstring_from_path(path).map_err(|e| errno(&e))?;
    let c_name = cstring_from_xattr_name(name).map_err(|e| errno(&e))?;
    let rc = unsafe {
        libc::lsetxattr(
            c_path.as_ptr(),
            c_name.as_ptr(),
            value.as_ptr() as *const libc::c_void,
            value.len(),
            flags as libc::c_int,
        )
    };
    if rc != 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    Ok(())
}

/// `lremovexattr` wrapper.
pub(crate) fn xattr_lremove(path: &Path, name: &std::ffi::OsStr) -> Result<(), i32> {
    let c_path = cstring_from_path(path).map_err(|e| errno(&e))?;
    let c_name = cstring_from_xattr_name(name).map_err(|e| errno(&e))?;
    let rc = unsafe { libc::lremovexattr(c_path.as_ptr(), c_name.as_ptr()) };
    if rc != 0 {
        return Err(errno(&std::io::Error::last_os_error()));
    }
    Ok(())
}
