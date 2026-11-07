//! Standalone public audit reads with bounded request files and JSON responses.

use crate::cli::{Exit, Result};
use std::{
    fs::OpenOptions,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) fn run(state: String, request: String, timeout: u64) -> Result<Exit> {
    crate::install_stop_signals()?;
    let deadline = Instant::now() + Duration::from_millis(timeout);
    if !Path::new(&state).is_absolute() || !Path::new(&request).is_absolute() {
        return Err("audit-export requires absolute --state-dir and --request paths".into());
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(request)?;
    if !file.metadata()?.is_file() {
        return Err("audit-export request must be a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(aw_service::audit_export::MAX_REQUEST_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let response = aw_service::audit_export::query(
        &Path::new(&state).join("journal"),
        &bytes,
        deadline,
        &crate::STOP,
    );
    let failed = !response["error"].is_null();
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response)?;
    writeln!(stdout)?;
    Ok(Exit::Code(i32::from(failed)))
}
