//! Wait for externally owned startup without retrying a scan or starting a daemon.

use std::{
    io,
    os::unix::fs::FileTypeExt,
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub(super) fn wait(socket: &Path, timeout: Duration) -> Result<(), &'static str> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or("invalid_budget")?;
    loop {
        if Instant::now() >= deadline {
            return Err("deadline_exceeded");
        }
        match std::fs::metadata(socket) {
            Ok(metadata) if metadata.file_type().is_socket() => return Ok(()),
            Ok(_) => return Err("backend_socket_invalid"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err("backend_socket_unavailable"),
        }
        thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(10)),
        );
    }
}
