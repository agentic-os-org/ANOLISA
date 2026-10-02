//! Home-directory resolution rooted in the passwd database.
//!
//! Every tokenless crate that writes state under the user's home (config,
//! stats DB, log files) must agree on what "home" means. Reading `$HOME`
//! directly is unsafe — a parent process can set it to anything before
//! invoking the binary, redirecting state files into attacker-writable
//! paths. This module derives the home directory from `getpwuid_r(getuid())`
//! and refuses to fall back to anything that reads `$HOME` (dirs::home_dir,
//! std::env, etc.); a missing passwd entry must surface as an empty string
//! so callers can refuse `$HOME`-relative writes rather than silently
//! redirecting them into an attacker-controlled location.

/// Resolve the current user's home directory.
///
/// Returns an empty string when no trusted home anchor is available; callers
/// must treat that as "no $HOME-relative writes" rather than using `.` or
/// $HOME as a fallback (either would silently place state wherever the
/// binary was invoked from or wherever a parent process pointed $HOME).
#[cfg(unix)]
pub fn get_home_dir() -> String {
    home_dir_from_passwd().unwrap_or_default()
}

/// Non-unix targets aren't a supported tokenless deployment, but keep a
/// compilation-only stub so the rest of the crate still builds.
#[cfg(not(unix))]
pub fn get_home_dir() -> String {
    String::new()
}

#[cfg(unix)]
fn home_dir_from_passwd() -> Option<String> {
    use std::ffi::CStr;
    // SAFETY: getuid is infallible and always safe. getpwuid_r is the
    // thread-safe variant: we hand it a stack-allocated passwd struct and
    // a 4 KiB heap buffer, and it never writes past the buffer length we
    // pass. result is left null when no entry is found, which we detect.
    let uid = unsafe { libc::getuid() };
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0u8; 4096];
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into our buf and is NUL-terminated by the libc
    // contract. The CStr borrow is short-lived; we copy the bytes out before
    // pwd/buf are dropped.
    let home = unsafe { CStr::from_ptr(pwd.pw_dir) }.to_str().ok()?;
    // Reject empty, non-absolute, and root ("/") entries:
    //   - empty / non-absolute: any later starts_with check would silently
    //     match the wrong scope (an empty prefix matches every path; a
    //     relative prefix can't be safely composed with canonicalized paths)
    //   - "/" as home: Path::starts_with("/") matches every absolute path,
    //     so validation routines built on it would let arbitrary system
    //     destinations through (e.g. /etc/evil.db).
    if home.is_empty() || home == "/" || !home.starts_with('/') {
        return None;
    }
    Some(home.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_home_dir_returns_passwd_entry_or_empty() {
        // The result depends on /etc/passwd in the test environment, but it
        // must never be "/" and must never silently fall back to $HOME.
        //
        // NOTE: This test mutates $HOME via std::env::set_var, which is
        // not thread-safe. cargo test runs tests in parallel by default,
        // so a concurrent test that reads $HOME could observe the injected
        // value. Risk is low — get_home_dir() reads from passwd, not $HOME,
        // and no other tokenless test depends on $HOME. Run with
        // `--test-threads=1` if flakiness is observed.
        let prior = std::env::var_os("HOME");
        // Point HOME at a path the passwd lookup cannot return so we'd
        // catch a regression that re-introduces the dirs::home_dir fallback.
        unsafe {
            std::env::set_var("HOME", "/should/not/be/used/as/home");
        }
        let h = get_home_dir();
        assert_ne!(h, "/", "pw_dir == '/' must be rejected");
        assert_ne!(
            h, "/should/not/be/used/as/home",
            "must not read $HOME as a fallback",
        );
        if !h.is_empty() {
            assert!(h.starts_with('/'), "passwd home must be absolute: {h}");
        }
        // Restore previous HOME (or unset it) so we don't pollute sibling tests.
        unsafe {
            match prior {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
