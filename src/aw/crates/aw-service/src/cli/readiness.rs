//! A native startup receipt is separate from daemon and foreground-process readiness.

use super::{adapter::Readiness, read_file, Result};
use aw_exec::CommandSpec;
use serde_json::Value;
use std::{
    io,
    os::unix::fs::MetadataExt,
    process::ExitStatus,
    sync::{
        atomic::{AtomicI32, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub(super) fn run(
    command: &CommandSpec,
    signal: &AtomicI32,
    ready: Option<Readiness>,
) -> Result<ExitStatus> {
    let Some(ready) = ready else {
        return aw_exec::run_foreground(command, signal).map_err(Into::into);
    };
    if ready.timeout.is_zero() || ready.timeout > Duration::from_secs(120) {
        return Err("native startup deadline must be within 1..120000 ms".into());
    }
    let (stop, receiver) = mpsc::sync_channel(1);
    thread::scope(|scope| {
        let mut watcher = None;
        let execution = aw_exec::run_foreground_observed(command, signal, |pid| {
            let ready = &ready;
            watcher = Some(scope.spawn(move || {
                let result = wait(ready, receiver, pid);
                if result.is_err() {
                    // aw-exec consumes this signal and cleans up only its owned group.
                    let _ = signal.compare_exchange(
                        0,
                        libc::SIGTERM,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    );
                }
                result
            }));
        });
        let _ = stop.send(());
        let registered = match watcher {
            Some(watcher) => watcher
                .join()
                .map_err(|_| "native startup watcher panicked")?
                .map_err(|message| -> Box<dyn std::error::Error> { message.into() })?,
            None => false,
        };
        let status = execution?;
        if !registered && status.success() {
            return Err("Agent exited before native hooks became ready".into());
        }
        Ok(status)
    })
}

fn wait(
    ready: &Readiness,
    stop: mpsc::Receiver<()>,
    pid: u32,
) -> std::result::Result<bool, String> {
    let deadline = Instant::now() + ready.timeout;
    loop {
        if receipt(ready, pid).map_err(|error| error.to_string())? {
            eprintln!("AW {} native hooks ready", ready.adapter);
            return Ok(true);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "{} native hook startup deadline exceeded",
                ready.adapter
            ));
        }
        match stop.recv_timeout(remaining.min(Duration::from_millis(25))) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return receipt(ready, pid).map_err(|error| error.to_string());
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn receipt(ready: &Readiness, pid: u32) -> Result<bool> {
    let metadata = match std::fs::symlink_metadata(&ready.path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    // A receipt is same-user installation evidence, not a privileged attestation.
    if !metadata.is_file()
        || metadata.mode() & 0o7777 != 0o600
        || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("unsafe native startup receipt".into());
    }
    let value: Value = serde_json::from_slice(&read_file(&ready.path, 4096)?)?;
    if value["version"] != 1
        || value["adapter"] != ready.adapter
        || value["token"] != ready.token
        || value["hooks"].as_u64() != Some(ready.hooks as u64)
        || value["pid"].as_u64() != Some(u64::from(pid))
    {
        return Err("native startup receipt does not match this launch".into());
    }
    Ok(true)
}

#[cfg(test)]
#[path = "readiness/tests.rs"]
mod tests;
