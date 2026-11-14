//! Same-user process identity survives neither PID reuse nor namespace changes.

use crate::{rejected, Result};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::MetadataExt, sync::Arc};

mod files;

/// Same-user process identity pinned to PID, start time and PID namespace.
#[derive(Clone)]
pub struct Process {
    pid: u32,
    start: String,
    uid: u32,
    namespace: String,
    files: Option<Arc<files::Files>>,
}

impl Process {
    /// Read a live same-user process identity.
    pub fn read(pid: u32) -> Result<Self> {
        if pid == 0 {
            return Err(rejected("process_identity"));
        }
        let directory = format!("/proc/{pid}");
        let stat = fs::read_to_string(format!("{directory}/stat"))?;
        let fields = stat
            .rsplit_once(") ")
            .ok_or_else(|| rejected("process_identity"))?
            .1
            .split_whitespace()
            .collect::<Vec<_>>();
        if matches!(fields.first(), Some(&"Z" | &"X")) {
            return Err(rejected("process_exited"));
        }
        let start = fields
            .get(19)
            .ok_or_else(|| rejected("process_identity"))?
            .to_string();
        let uid = fs::metadata(&directory)?.uid();
        // SAFETY: geteuid only reads this process's effective user identity.
        if uid != unsafe { libc::geteuid() } {
            return Err(rejected("process_owner"));
        }
        let namespace = fs::read_link(format!("{directory}/ns/pid"))?
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            files: None,
            pid,
            start,
            uid,
            namespace,
        })
    }

    /// Retain live procfs files for repeated checks, falling back when unavailable.
    pub(super) fn watch(mut self) -> Result<Self> {
        if let Ok(files) = files::Files::open(&self) {
            // Recheck after opening so an exit/reuse race cannot bind another process.
            self.check()?;
            files.check(&self).map_err(|_| rejected("stale_process"))?;
            self.files = Some(Arc::new(files));
        } else {
            self.check()?;
        }
        Ok(self)
    }

    /// Hold the exit notification open while the service polls it.
    pub(crate) fn exit_notification(&self) -> Option<Arc<fs::File>> {
        self.files.as_ref().map(|files| Arc::clone(&files.exit))
    }

    /// Require the exact start identity supplied by an independent client.
    pub(super) fn registration_owner(&self, owner: &crate::RegistrationOwner) -> Result<()> {
        self.check()?;
        if self.pid != owner.pid || self.start != owner.start_ticks {
            return Err(rejected("registration_owner_mismatch"));
        }
        Ok(())
    }

    /// Authenticate a direct caller or its immediately spawned CLI transport.
    pub(super) fn caller(&self, peer: &Self) -> Result<()> {
        self.check()?;
        peer.check()?;
        if peer.namespace != self.namespace
            || fs::read_link("/proc/self/ns/pid")?.to_string_lossy() != self.namespace
        {
            return Err(rejected("registration_peer_mismatch"));
        }
        if peer.pid != self.pid {
            let stat = fs::read_to_string(format!("/proc/{}/stat", peer.pid))?;
            let parent = stat
                .rsplit_once(") ")
                .and_then(|(_, fields)| fields.split_whitespace().nth(1))
                .and_then(|value| value.parse::<u32>().ok());
            if parent != Some(self.pid) {
                return Err(rejected("registration_peer_mismatch"));
            }
        }
        peer.check()?;
        self.check()
    }

    /// Reject exit, PID reuse or namespace changes.
    pub fn check(&self) -> Result<()> {
        if let Some(files) = &self.files {
            return files.check(self).map_err(|_| rejected("stale_process"));
        }
        let current = Self::read(self.pid).map_err(|_| rejected("stale_process"))?;
        if (current.start, current.uid, current.namespace)
            != (self.start.clone(), self.uid, self.namespace.clone())
        {
            return Err(rejected("stale_process"));
        }
        Ok(())
    }

    /// Require the exact worker identity recorded at task admission.
    pub(super) fn matches(&self, pid: u32) -> Result<()> {
        self.check()?;
        if self.pid != pid {
            return Err(rejected("scope_worker_mismatch"));
        }
        Ok(())
    }

    /// Require the callback process to descend from the selected native process.
    pub fn corroborate(&self, mut pid: u32) -> Result<()> {
        self.check()?;
        for _ in 0..64 {
            let process = Self::read(pid).map_err(|_| rejected("bridge_process_mismatch"))?;
            if process.namespace != self.namespace {
                break;
            }
            if pid == self.pid {
                return Ok(());
            }
            let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
            pid = stat
                .rsplit_once(") ")
                .and_then(|(_, fields)| fields.split_whitespace().nth(1))
                .and_then(|parent| parent.parse().ok())
                .filter(|parent| *parent > 0 && *parent != pid)
                .ok_or_else(|| rejected("bridge_process_mismatch"))?;
        }
        Err(rejected("bridge_process_mismatch"))
    }

    /// Metadata suitable for audit; contains no process environment or arguments.
    pub fn metadata(&self) -> Value {
        json!({"pid":self.pid,"start_ticks":self.start,"uid":self.uid,"pid_namespace":self.namespace,"alive":self.check().is_ok()})
    }
}

#[cfg(test)]
mod tests;
