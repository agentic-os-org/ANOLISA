//! Authenticated shared-path resolution; configured mounts never fall back to FUSE I/O.

use super::{
    Mount, SkillFsError,
    auth::{self, Frame, Secret},
};
use asc_capability_skill_guard::{SkillIdentity, SkillRoot};
use rustix::fs::{Mode, OFlags, open, openat};
use serde_json::{Value, json};
use socket2::{Domain, SockAddr, Socket, Type};
use std::fs::File;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::{
    fd::OwnedFd,
    unix::{
        fs::{FileTypeExt as _, MetadataExt as _},
        net::UnixStream,
    },
};
use std::path::{Component, Path};
use std::time::{Duration, Instant};

pub(super) struct Resolver {
    pub mounts: Vec<Mount>,
    pub secret: Secret,
}

impl Resolver {
    pub fn resolve(
        &self,
        identity: &SkillIdentity,
        deadline: Instant,
    ) -> Result<SkillRoot, SkillFsError> {
        for mount in &self.mounts {
            let relative = identity
                .path()
                .strip_prefix(&mount.canonical_root)
                .or_else(|_| identity.path().strip_prefix(&mount.live_root));
            let Ok(relative) = relative else {
                continue;
            };
            validate_relative(relative)?;
            let canonical = SkillIdentity::new(mount.canonical_root.join(relative))?;
            let physical = mount.live_root.join(relative);
            let deadline = deadline.min(Instant::now() + Duration::from_secs(5));
            let result = resolve_remote(mount, &self.secret, &canonical, deadline)?;
            if result["managed"] != true
                || result["transport"] != "shared_path"
                || result["canonicalSkillDir"] != json!(canonical.path())
                || result["liveSkillDir"] != json!(physical)
                || result["skillId"] != json!(relative)
                || result["relativeSkillDir"] != json!(relative)
            {
                return Err(SkillFsError::Invalid(
                    "resolver mapping does not match configured mount",
                ));
            }
            let device = result["identity"]["device"]
                .as_u64()
                .ok_or(SkillFsError::Invalid("missing resolver device"))?;
            let inode = result["identity"]["inode"]
                .as_u64()
                .ok_or(SkillFsError::Invalid("missing resolver inode"))?;
            return Ok(SkillRoot::resolved(canonical, physical)?.with_file_identity(device, inode)?);
        }
        Ok(SkillRoot::direct(identity.path())?)
    }

    pub fn notify_identity(
        &self,
        identity: &SkillIdentity,
        uid: u32,
        skill_id: &str,
    ) -> Result<(), SkillFsError> {
        for mount in &self.mounts {
            if let Ok(relative) = identity.path().strip_prefix(&mount.canonical_root) {
                validate_relative(relative)?;
                if mount.peer_uid == uid && relative.to_str() == Some(skill_id) {
                    return Ok(());
                }
                return Err(SkillFsError::Invalid(
                    "notify identity does not match authenticated mount",
                ));
            }
        }
        Err(SkillFsError::Invalid(
            "notify path is outside configured mounts",
        ))
    }
}

fn validate_relative(path: &Path) -> Result<(), SkillFsError> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| match component {
            Component::Normal(name) => name
                .to_str()
                .is_none_or(|s| s.starts_with('.') || s == "skill-discover"),
            _ => true,
        })
    {
        return Err(SkillFsError::Invalid("invalid relative Skill identity"));
    }
    Ok(())
}

pub(super) fn trusted_directory(path: &Path, uid: u32) -> Result<File, SkillFsError> {
    SkillIdentity::new(path)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(open("/", flags, Mode::empty())?);
    for part in path.components() {
        if let Component::Normal(name) = part {
            directory = File::from(openat(&directory, name, flags, Mode::empty())?);
            let meta = directory.metadata()?;
            if ![0, uid].contains(&meta.uid())
                || (meta.mode() & 0o022 != 0 && !(meta.uid() == 0 && meta.mode() & 0o1000 != 0))
            {
                return Err(SkillFsError::Invalid("untrusted endpoint parent"));
            }
        }
    }
    Ok(directory)
}

pub(super) fn load_secret(path: &Path) -> Result<Secret, SkillFsError> {
    let uid = rustix::process::geteuid().as_raw();
    let parent = trusted_directory(
        path.parent()
            .ok_or(SkillFsError::Invalid("key has no parent"))?,
        uid,
    )?;
    let name = path
        .file_name()
        .ok_or(SkillFsError::Invalid("invalid key path"))?;
    let file = File::from(openat(
        &parent,
        name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != uid
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(SkillFsError::Invalid(
            "authentication key must be daemon-owned regular 0600 file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    if !(32..=4096).contains(&bytes.len()) {
        return Err(SkillFsError::Invalid(
            "authentication key must contain 32..4096 bytes",
        ));
    }
    Ok(Secret(bytes.into()))
}

fn resolve_remote(
    mount: &Mount,
    secret: &Secret,
    canonical: &SkillIdentity,
    deadline: Instant,
) -> Result<Value, SkillFsError> {
    let parent = trusted_directory(
        mount
            .control_socket
            .parent()
            .ok_or(SkillFsError::Invalid("control socket has no parent"))?,
        mount.peer_uid,
    )?;
    let meta = parent.metadata()?;
    if meta.uid() != mount.peer_uid || meta.mode() & 0o777 != 0o700 {
        return Err(SkillFsError::Invalid(
            "control socket requires private peer-owned parent",
        ));
    }
    let meta = std::fs::symlink_metadata(&mount.control_socket)?;
    if !meta.file_type().is_socket() || meta.uid() != mount.peer_uid || meta.mode() & 0o777 != 0o600
    {
        return Err(SkillFsError::Invalid(
            "control socket must be peer-owned mode 0600",
        ));
    }
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
    socket.connect_timeout(
        &SockAddr::unix(&mount.control_socket)?,
        remaining(deadline)?,
    )?;
    socket.peer_addr()?;
    let stream = UnixStream::from(OwnedFd::from(socket));
    let peer = rustix::net::sockopt::socket_peercred(&stream)?;
    if peer.uid.as_raw() != mount.peer_uid {
        return Err(SkillFsError::Authentication);
    }
    let mut reader = BufReader::new(stream);
    write(
        &mut reader,
        &Frame::encode("auth.init", None, None),
        deadline,
    )?;
    let challenge = Frame::parse(&read(&mut reader, 4096, deadline)?, "auth.challenge")?.nonce()?;
    let proof = auth::sign(secret, auth::CONTROL_CLIENT, &challenge, None);
    write(
        &mut reader,
        &Frame::encode("auth.proof", None, Some(proof.as_ref())),
        deadline,
    )?;
    let ok = Frame::parse(&read(&mut reader, 4096, deadline)?, "auth.ok")?.proof()?;
    auth::verify(secret, auth::CONTROL_SERVER, &challenge, None, &ok)?;
    let request = serde_json::to_vec(
        &json!({"schemaVersion":"1","method":"skill.resolveLiveSource","canonicalSkillDir":canonical.path()}),
    )?;
    let tag = auth::sign(secret, auth::CONTROL_CLIENT, &challenge, Some(&request));
    write(&mut reader, &request, deadline)?;
    write(
        &mut reader,
        &Frame::encode("auth.frame", None, Some(tag.as_ref())),
        deadline,
    )?;
    let response = read(&mut reader, 64 * 1024, deadline)?;
    let tag = Frame::parse(&read(&mut reader, 4096, deadline)?, "auth.frame")?.proof()?;
    auth::verify(
        secret,
        auth::CONTROL_SERVER,
        &challenge,
        Some(&response),
        &tag,
    )?;
    let response: Value = serde_json::from_slice(&response)?;
    if response["schemaVersion"] != "1" || response["ok"] != true {
        return Err(SkillFsError::Invalid(
            "SkillFS resolver rejected the canonical Skill",
        ));
    }
    Ok(response["result"].clone())
}

fn remaining(deadline: Instant) -> Result<Duration, SkillFsError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
        .ok_or(SkillFsError::Timeout)
}

fn write(
    reader: &mut BufReader<UnixStream>,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), SkillFsError> {
    for chunk in [bytes, b"\n"] {
        let mut pending = chunk;
        while !pending.is_empty() {
            reader
                .get_ref()
                .set_write_timeout(Some(remaining(deadline)?))?;
            let count = reader.get_mut().write(pending)?;
            if count == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            pending = &pending[count..];
        }
    }
    Ok(())
}

fn read(
    reader: &mut BufReader<UnixStream>,
    limit: usize,
    deadline: Instant,
) -> Result<Vec<u8>, SkillFsError> {
    let mut bytes = Vec::new();
    loop {
        reader
            .get_ref()
            .set_read_timeout(Some(remaining(deadline)?))?;
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(SkillFsError::Authentication);
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.unwrap_or(available.len());
        if bytes.len().saturating_add(count) > limit {
            return Err(SkillFsError::Invalid("control frame exceeds limit"));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count + usize::from(newline.is_some()));
        if newline.is_some() {
            return Ok(bytes);
        }
    }
}
