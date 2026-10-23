//! Same-user process identity survives neither PID reuse nor namespace changes.

use crate::{rejected, Result};
use serde_json::{json, Value};
use std::{fs, os::unix::fs::MetadataExt};

pub(super) struct Process {
    pid: u32,
    start: String,
    uid: u32,
    namespace: String,
}

impl Process {
    pub(super) fn read(pid: u32) -> Result<Self> {
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
            pid,
            start,
            uid,
            namespace,
        })
    }

    pub(super) fn check(&self) -> Result<()> {
        let current = Self::read(self.pid).map_err(|_| rejected("stale_process"))?;
        if (current.start, current.uid, current.namespace)
            != (self.start.clone(), self.uid, self.namespace.clone())
        {
            return Err(rejected("stale_process"));
        }
        Ok(())
    }

    pub(super) fn corroborate(&self, mut pid: u32) -> Result<()> {
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

    pub(super) fn metadata(&self) -> Value {
        json!({"pid":self.pid,"start_ticks":self.start,"uid":self.uid,"pid_namespace":self.namespace,"alive":self.check().is_ok()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_and_namespace_changes_invalidate_process_association() {
        let mut process = Process::read(std::process::id()).unwrap();
        process.check().unwrap();
        process.corroborate(std::process::id()).unwrap();
        let start = process.start.clone();
        process.start.push('0');
        assert!(process.check().is_err());
        process.start = start;
        process.namespace.push_str("-changed");
        assert!(process.check().is_err());
        assert!(Process::read(0).is_err());
    }
}
