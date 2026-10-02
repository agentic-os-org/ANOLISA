//! Filesystem and peer-path trust boundaries for the checkpoint adapter.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cosh_gateway::capability::{SecurityAuditError, SecurityAuditGate};
use cosh_gateway::storage::{ExecutionRecord, SecurityAuditProof};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{digest_parts, CheckpointOperation};

const AUDIT_DOMAIN: &[u8] = b"cosh.gateway.security-audit.v1\0";
const AUDIT_HEADER: &[u8] = b"{\"schema\":\"cosh.gateway.security-audit-log.v1\"}\n";

pub(super) type AuditFile = nix::fcntl::Flock<File>;

#[derive(Debug, Error)]
pub(crate) enum CheckpointAdmissionError {
    #[error("checkpoint profile/provider admission failed")]
    Profile,
    #[error("checkpoint socket path must be an absolute Unix socket")]
    Socket,
    #[error("checkpoint socket path has an untrusted owner or writable ancestor")]
    SocketTrust,
    #[error("checkpoint workspace registration path must be absolute UTF-8")]
    Workspace,
    #[error("checkpoint daemon did not prove the configured workspace identity")]
    Identity,
    #[error("checkpoint audit file could not be opened safely")]
    Audit,
}

impl CheckpointAdmissionError {
    pub(crate) const fn is_unavailable(&self) -> bool {
        matches!(self, Self::Socket | Self::Identity)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CheckpointBinding {
    pub(super) version: u16,
    pub(super) ws_id: String,
    pub(super) registered_path: String,
    pub(super) generation: [u8; 32],
    pub(super) owner_uid: u32,
}

pub(super) struct FileAuditGate<'a> {
    file: &'a mut AuditFile,
}

impl<'a> FileAuditGate<'a> {
    pub(super) fn new(file: &'a mut AuditFile) -> Self {
        Self { file }
    }
}

impl SecurityAuditGate<CheckpointOperation> for FileAuditGate<'_> {
    fn persist_start(
        &mut self,
        execution: &ExecutionRecord,
        operation: &CheckpointOperation,
    ) -> Result<SecurityAuditProof, SecurityAuditError> {
        let mut line = serde_json::to_vec(&serde_json::json!({
            "schema": "cosh.gateway.security-audit.v1",
            "execution_id": execution.execution_id,
            "task_id": execution.task_id,
            "run_id": execution.run_id,
            "target_identity_digest": operation.target_identity_digest,
            "operation_digest": operation.operation_digest,
        }))
        .map_err(|_| SecurityAuditError)?;
        line.push(b'\n');
        self.file.write_all(&line).map_err(|_| SecurityAuditError)?;
        self.file.sync_data().map_err(|_| SecurityAuditError)?;
        let proof_digest = digest_parts(&[AUDIT_DOMAIN, &line]).map_err(|_| SecurityAuditError)?;
        Ok(SecurityAuditProof {
            proof_digest,
            persisted_at_ms: current_time_ms()?,
        })
    }
}

pub(super) fn verify_socket_trust(
    path: &Path,
    owner_uid: u32,
) -> Result<(u64, u64, u32), CheckpointAdmissionError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| CheckpointAdmissionError::Socket)?;
    if !metadata.file_type().is_socket() || !trusted_owner(metadata.uid(), owner_uid) {
        return Err(CheckpointAdmissionError::SocketTrust);
    }
    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        let metadata =
            fs::symlink_metadata(directory).map_err(|_| CheckpointAdmissionError::SocketTrust)?;
        let mode = metadata.permissions().mode();
        if !metadata.is_dir()
            || !trusted_owner(metadata.uid(), owner_uid)
            || (mode & 0o022 != 0 && mode & 0o1000 == 0)
        {
            return Err(CheckpointAdmissionError::SocketTrust);
        }
        ancestor = directory.parent();
    }
    Ok((metadata.dev(), metadata.ino(), metadata.uid()))
}

fn trusted_owner(actual: u32, owner: u32) -> bool {
    actual == 0 || actual == owner
}

pub(super) fn open_audit_file(
    path: &Path,
    owner_uid: u32,
) -> Result<AuditFile, CheckpointAdmissionError> {
    if !path.is_absolute() {
        return Err(CheckpointAdmissionError::Audit);
    }
    let parent = path.parent().ok_or(CheckpointAdmissionError::Audit)?;
    let directory = open_trusted_audit_parent(parent, owner_uid)?;
    let name = path.file_name().ok_or(CheckpointAdmissionError::Audit)?;
    let descriptor_path =
        PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
    let (file, created) = open_audit_leaf(&descriptor_path)?;
    let mut file = AuditFile::lock(file, nix::fcntl::FlockArg::LockExclusiveNonblock)
        .map_err(|_| CheckpointAdmissionError::Audit)?;
    validate_audit_file(&file, owner_uid)?;
    if created {
        file.write_all(AUDIT_HEADER)
            .map_err(|_| CheckpointAdmissionError::Audit)?;
        file.sync_all()
            .map_err(|_| CheckpointAdmissionError::Audit)?;
        nix::unistd::fsync(directory.as_raw_fd()).map_err(|_| CheckpointAdmissionError::Audit)?;
    } else {
        validate_audit_header(&mut file)?;
    }
    Ok(file)
}

fn open_audit_leaf(path: &Path) -> Result<(File, bool), CheckpointAdmissionError> {
    match OpenOptions::new()
        .create_new(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
        .open(path)
    {
        Ok(file) => Ok((file, true)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => OpenOptions::new()
            .read(true)
            .append(true)
            .custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW)
            .open(path)
            .map(|file| (file, false))
            .map_err(|_| CheckpointAdmissionError::Audit),
        Err(_) => Err(CheckpointAdmissionError::Audit),
    }
}

fn validate_audit_file(file: &AuditFile, owner_uid: u32) -> Result<(), CheckpointAdmissionError> {
    let metadata = file
        .metadata()
        .map_err(|_| CheckpointAdmissionError::Audit)?;
    if !metadata.is_file()
        || metadata.uid() != owner_uid
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(CheckpointAdmissionError::Audit);
    }
    Ok(())
}

fn validate_audit_header(file: &mut AuditFile) -> Result<(), CheckpointAdmissionError> {
    let mut header = vec![0_u8; AUDIT_HEADER.len()];
    file.rewind().map_err(|_| CheckpointAdmissionError::Audit)?;
    file.read_exact(&mut header)
        .map_err(|_| CheckpointAdmissionError::Audit)?;
    if header != AUDIT_HEADER {
        return Err(CheckpointAdmissionError::Audit);
    }
    Ok(())
}

fn open_trusted_audit_parent(
    path: &Path,
    owner_uid: u32,
) -> Result<nix::dir::Dir, CheckpointAdmissionError> {
    use nix::dir::Dir;
    use nix::fcntl::OFlag;
    use nix::sys::stat::{fstat, Mode, SFlag};

    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mut directory =
        Dir::open("/", flags, Mode::empty()).map_err(|_| CheckpointAdmissionError::Audit)?;
    let components = path
        .components()
        .filter_map(|component| match component {
            Component::RootDir => None,
            Component::Normal(name) => Some(Ok(name)),
            _ => Some(Err(CheckpointAdmissionError::Audit)),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if components.is_empty() {
        return Err(CheckpointAdmissionError::Audit);
    }
    for (index, name) in components.iter().enumerate() {
        directory = Dir::openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty())
            .map_err(|_| CheckpointAdmissionError::Audit)?;
        let metadata = fstat(directory.as_raw_fd()).map_err(|_| CheckpointAdmissionError::Audit)?;
        let mode = metadata.st_mode as u32;
        let is_final = index + 1 == components.len();
        if SFlag::from_bits_truncate(metadata.st_mode) != SFlag::S_IFDIR
            || !trusted_owner(metadata.st_uid, owner_uid)
            || (mode & 0o022 != 0 && mode & 0o1000 == 0)
            || (is_final && (metadata.st_uid != owner_uid || mode & 0o077 != 0))
        {
            return Err(CheckpointAdmissionError::Audit);
        }
    }
    Ok(directory)
}

pub(super) fn current_time_ms() -> Result<u64, SecurityAuditError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SecurityAuditError)?
        .as_millis();
    u64::try_from(millis).map_err(|_| SecurityAuditError)
}

#[cfg(test)]
#[path = "trust/tests.rs"]
mod tests;
