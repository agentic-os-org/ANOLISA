//! Explicit same-user authorization pinned to a live executable and boot identity.

use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, io::Read, os::unix::fs::MetadataExt, path::Path};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProcessRef {
    pub pid: i32,
    pub start_ticks: String,
    pub uid: u32,
    pub pid_namespace: String,
    pub boot_id: String,
    pub exe: String,
    pub exe_device: u64,
    pub exe_inode: u64,
}

impl ProcessRef {
    pub fn read(pid: i32) -> Result<Self> {
        ensure!(pid > 0, "invalid_pid");
        let base = format!("/proc/{pid}");
        let stat = fs::read_to_string(format!("{base}/stat"))?;
        let (start_ticks, state) = parse_stat(&stat)?;
        ensure!(state != "Z" && state != "X", "process_exited");
        let uid = fs::metadata(&base)?.uid();
        // SAFETY: geteuid has no arguments or side effects.
        ensure!(uid == unsafe { libc::geteuid() }, "process_owner_mismatch");
        let exe_meta = fs::metadata(format!("{base}/exe"))?;
        Ok(Self {
            pid,
            start_ticks,
            uid,
            pid_namespace: fs::read_link(format!("{base}/ns/pid"))?
                .to_string_lossy()
                .into(),
            boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .into(),
            exe: fs::read_link(format!("{base}/exe"))?
                .to_string_lossy()
                .into(),
            exe_device: exe_meta.dev(),
            exe_inode: exe_meta.ino(),
        })
    }

    pub fn check(&self) -> Result<()> {
        ensure!(Self::read(self.pid)? == *self, "stale_process_identity");
        Ok(())
    }
}

fn parse_stat(stat: &str) -> Result<(String, String)> {
    let fields: Vec<_> = stat
        .rsplit_once(") ")
        .context("invalid_proc_stat")?
        .1
        .split_whitespace()
        .collect();
    ensure!(fields.len() > 19, "invalid_proc_stat");
    ensure!(fields[19].parse::<u64>().is_ok(), "invalid_start_ticks");
    Ok((fields[19].into(), fields[0].into()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Permit {
    pub schema_version: String,
    pub authorized_experiment: bool,
    pub process: ProcessRef,
    pub exe_sha256: String,
}

pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

impl Permit {
    pub fn load(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path)?;
        // A permit is an operator assertion, not protection against a malicious same-UID peer.
        ensure!(
            meta.is_file() && meta.mode() & 0o077 == 0,
            "permit_must_be_private_regular_file"
        );
        // SAFETY: geteuid only reads the caller's identity.
        ensure!(
            meta.uid() == unsafe { libc::geteuid() } && meta.len() <= 8192,
            "invalid_permit_owner_or_size"
        );
        let permit: Self = serde_json::from_slice(&fs::read(path)?)?;
        ensure!(
            permit.schema_version == "aw.ptrace-permit-poc/v0" && permit.authorized_experiment,
            "experiment_not_authorized"
        );
        permit.process.check()?;
        let actual = hash_file(Path::new(&format!("/proc/{}/exe", permit.process.pid)))?;
        if actual != permit.exe_sha256 {
            bail!("executable_fingerprint_mismatch");
        }
        permit.process.check()?;
        Ok(permit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stat_name_can_contain_spaces_and_parentheses() {
        let stat = format!("12 (a ) tricky) S {} 456 0", vec!["0"; 18].join(" "));
        assert_eq!(parse_stat(&stat).unwrap(), ("456".into(), "S".into()));
        assert!(parse_stat("12 broken").is_err());
    }
    #[test]
    fn identity_rejects_start_namespace_and_executable_changes() {
        let current = ProcessRef::read(std::process::id() as i32).unwrap();
        current.check().unwrap();
        for field in ["start", "namespace", "exe", "boot"] {
            let mut stale = current.clone();
            match field {
                "start" => stale.start_ticks.push('0'),
                "namespace" => stale.pid_namespace.push('x'),
                "boot" => stale.boot_id.push('x'),
                _ => stale.exe_inode += 1,
            }
            assert!(stale.check().is_err());
        }
    }
}
