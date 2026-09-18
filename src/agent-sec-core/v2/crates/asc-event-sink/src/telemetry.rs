//! Append-only telemetry writer. File management belongs to the telemetry infrastructure.
use asc_telemetry::{TelemetryRecord, TelemetryStatus, config::TelemetryConfig};
use rustix::fs::{FlockOperation, Mode, OFlags, flock, open};
use rustix::io::Errno;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;

/// Shared writer with V1's nonblocking thread and process lock behavior.
pub struct TelemetryWriter {
    config: TelemetryConfig,
    lock: Mutex<()>,
}

impl TelemetryWriter {
    /// Accepts explicit deployment paths without creating files or directories.
    #[must_use]
    pub const fn new(config: TelemetryConfig) -> Self {
        Self {
            config,
            lock: Mutex::new(()),
        }
    }

    /// Gates projection on policy and an existing regular file, rejecting target symlinks.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.config.enabled()
            && self
                .config
                .path
                .symlink_metadata()
                .is_ok_and(|m| m.is_file())
    }

    /// Rechecks deployment policy and attempts one complete UTF-8 JSONL append.
    /// Return does not promise fsync or uploader delivery.
    pub fn write(&self, record: &TelemetryRecord) -> TelemetryStatus {
        if !self.config.enabled() {
            return TelemetryStatus::Skipped;
        }
        let Ok(_guard) = self.lock.try_lock() else {
            return TelemetryStatus::Skipped;
        };
        let Ok(mut bytes) = serde_json::to_vec(record) else {
            return TelemetryStatus::Failed;
        };
        bytes.push(b'\n');
        let fd = match open(
            &self.config.path,
            // Enforce the final-component check again if the path changed after enabled().
            OFlags::WRONLY | OFlags::APPEND | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOFOLLOW,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return TelemetryStatus::Skipped,
            Err(_) => return TelemetryStatus::Failed,
        };
        let mut file = File::from(fd);
        // A configured FIFO/device must not block or receive telemetry.
        if !file.metadata().is_ok_and(|m| m.is_file()) {
            return TelemetryStatus::Failed;
        }
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => (),
            Err(Errno::AGAIN | Errno::ACCESS) => return TelemetryStatus::Skipped,
            Err(_) => return TelemetryStatus::Failed,
        }
        // write_all handles short/interrupted writes; closing releases the flock on every path.
        if file.write_all(&bytes).is_ok() {
            TelemetryStatus::Written
        } else {
            TelemetryStatus::Failed
        }
    }
}
