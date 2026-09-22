//! Spawn helper for executors that run ANOLISA-owned executables
//! (hook scripts, installed binaries).

use std::io;
use std::process::{Child, Command};
use std::thread;
use std::time::Duration;

/// `ETXTBSY` ("text file busy") on Linux and macOS.
const ETXTBSY: i32 = 26;

/// Spawn with a bounded retry on `ETXTBSY`.
///
/// ANOLISA writes executables and then spawns them. On Unix, `spawn` is
/// fork+exec: a fork in another thread briefly inherits every open
/// descriptor (CLOEXEC closes them only at *its* exec), so exec'ing a
/// just-written file can race a concurrent spawn's write-descriptor
/// snapshot and fail with `ETXTBSY` even though the writer already
/// closed the file. The window is microseconds; a few short retries
/// make the spawn deterministic without masking real errors — a file
/// genuinely held open for writing keeps failing and the last error
/// is returned.
///
/// # Errors
///
/// Propagates the final [`Command::spawn`] error once retries are
/// exhausted, and any non-`ETXTBSY` error immediately.
pub fn spawn_retry_etxtbsy(cmd: &mut Command) -> io::Result<Child> {
    const ATTEMPTS: u32 = 5;
    let mut delay = Duration::from_millis(5);
    for _ in 1..ATTEMPTS {
        match cmd.spawn() {
            Err(err) if err.raw_os_error() == Some(ETXTBSY) => {
                thread::sleep(delay);
                delay *= 2;
            }
            other => return other,
        }
    }
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_succeeds_for_plain_binary() {
        let child = spawn_retry_etxtbsy(
            Command::new("/bin/sh")
                .arg("-c")
                .arg("exit 0")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        );
        let status = child.expect("spawn").wait().expect("wait");
        assert!(status.success());
    }

    #[test]
    fn non_etxtbsy_error_is_not_retried() {
        let err = spawn_retry_etxtbsy(&mut Command::new("/no/such/binary"))
            .expect_err("must fail to spawn");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
