//! Bounded metadata output; transport entry observations never assert native Hook coverage.

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
use crate::identity::ProcessRef;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use anyhow::{ensure, Result};
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
use serde_json::{json, Value};
#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
use sha2::{Digest, Sha256};
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::Instant,
};

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) const SCHEMA: &str = "aw.transport-observation-poc/v0";

#[cfg(any(test, all(target_os = "linux", target_arch = "x86_64")))]
pub(crate) fn observation(
    process: &ProcessRef,
    tid: i32,
    function: &str,
    length: u64,
    bytes: Option<&[u8]>,
) -> Value {
    let digest = bytes.map(|b| format!("{:x}", Sha256::digest(b)));
    let mut clock = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: CLOCK_MONOTONIC writes a timespec into valid storage.
    let valid_clock = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut clock) } == 0;
    let time = valid_clock
        .then(|| (clock.tv_sec as u128 * 1_000_000_000 + clock.tv_nsec as u128).to_string());
    json!({"schema_version":SCHEMA,"kind":"transport.before_send","source":"ptrace.ssl_write",
        "process":process,"thread_id":tid,"aw_instance_id":null,"aw_scope_id":null,
        "association":{"status":"unassociated","process_state":"observed"},
        "observed_at_ns":time,"clock_domain":"linux.CLOCK_MONOTONIC",
        "transport":{"function":function,"length_bytes":length,"payload_sha256":digest,
        "hashed_bytes":bytes.map_or(0, |b| b.len()),"adoption":"unknown"},
        "coverage":{"status":"unknown","reason":if length > 4096 {"payload_over_limit"} else if bytes.is_none() {"payload_read_failed"} else {"transport_write_only"}}})
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub(crate) struct Sink {
    file: File,
    pub emitted: u64,
    pub dropped: u64,
    bytes: usize,
    max_events: u64,
    window: Instant,
    in_window: u64,
    rate: u64,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
impl Sink {
    pub fn new(path: &Path, max_events: u64, rate: u64) -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        Ok(Self {
            file,
            emitted: 0,
            dropped: 0,
            bytes: 0,
            max_events,
            window: Instant::now(),
            in_window: 0,
            rate,
        })
    }

    pub fn control(&mut self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)?;
        ensure!(bytes.len() <= 8192, "event_size_limit");
        bytes.push(b'\n');
        self.file.write_all(&bytes)?;
        Ok(())
    }

    pub fn emit(&mut self, value: Value) -> Result<()> {
        if self.window.elapsed().as_secs() >= 1 {
            self.window = Instant::now();
            self.in_window = 0;
        }
        if self.emitted >= self.max_events
            || self.in_window >= self.rate
            || self.bytes >= 64 * 1024 * 1024
        {
            self.dropped += 1;
            return Ok(());
        }
        self.bytes += serde_json::to_vec(&value)?.len() + 1;
        self.control(value)?;
        self.emitted += 1;
        self.in_window += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn oversize_never_claims_a_full_digest_or_semantic_hook() {
        let p = ProcessRef::read(std::process::id() as i32).unwrap();
        let event = observation(&p, p.pid, "SSL_write", 4097, None);
        assert!(event["transport"]["payload_sha256"].is_null());
        assert_eq!(event["coverage"]["reason"], "payload_over_limit");
        let small = observation(&p, p.pid, "SSL_write", 6, Some(b"secret"));
        let text = small.to_string();
        assert!(!text.contains("secret"));
        assert!(!text.contains("tool.before"));
        assert_eq!(small["association"]["process_state"], "observed");
    }
}
