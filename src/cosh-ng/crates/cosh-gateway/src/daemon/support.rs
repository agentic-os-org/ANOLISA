fn authorize(task: &TaskAggregate, actor_id: &ActorId) -> Result<(), GatewayDaemonError> {
    if task.owner_actor_id() == actor_id {
        Ok(())
    } else {
        // Hide foreign Task existence from the local actor namespace.
        Err(StoreError::TaskNotFound.into())
    }
}

fn receipt_task_id(outcome: &CommitOutcome) -> &TaskId {
    match outcome {
        CommitOutcome::Applied(receipt) | CommitOutcome::Replayed(receipt) => &receipt.task_id,
    }
}

fn digest_json(value: &impl Serialize) -> Result<Digest, GatewayDaemonError> {
    Ok(sha256_digest(&serde_json::to_vec(value)?))
}

fn sha256_digest(bytes: &[u8]) -> Digest {
    let digest = Sha256::digest(bytes);
    // SHA-256 lower-hex output always satisfies the contract.
    Digest::parse(format!("{digest:x}")).unwrap_or_else(|_| unreachable!())
}

fn now_ms() -> Result<u64, GatewayDaemonError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| GatewayDaemonError::Protocol("system clock precedes Unix epoch".to_owned()))?
        .as_millis()
        .try_into()
        .map_err(|_| GatewayDaemonError::Protocol("system clock is out of range".to_owned()))
}

fn actor_id_for_uid(
    installation_id: &InstallationId,
    uid: u32,
) -> Result<ActorId, GatewayDaemonError> {
    let mut bytes = Sha256::digest(
        [
            b"cosh.gateway.local.actor.v1".as_slice(),
            installation_id.as_str().as_bytes(),
            &uid.to_be_bytes(),
        ]
        .concat(),
    );
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let uuid = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]
    );
    ActorId::parse(format!("act_{uuid}"))
        .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))
}

fn actor_ref_for_uid(
    installation_id: &InstallationId,
    uid: u32,
) -> Result<ActorRef, GatewayDaemonError> {
    Ok(ActorRef {
        actor_id: actor_id_for_uid(installation_id, uid)?,
        actor_kind: ActorKind::Human,
        issuer: BoundedName::new("local-os")
            .map_err(|error| GatewayDaemonError::Protocol(error.to_string()))?,
        assurance: AuthAssurance::LocalOs,
    })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &UnixStream) -> Result<u32, GatewayDaemonError> {
    use nix::sys::socket::sockopt::PeerCredentials;

    Ok(getsockopt(stream, PeerCredentials)
        .map_err(nix_to_io)?
        .uid())
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly"
))]
fn peer_uid(stream: &UnixStream) -> Result<u32, GatewayDaemonError> {
    use nix::sys::socket::sockopt::LocalPeerCred;

    Ok(getsockopt(stream, LocalPeerCred).map_err(nix_to_io)?.uid())
}

fn nix_to_io(error: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(error as i32)
}

fn prepare_socket_path(path: &Path, owner_uid: u32) -> Result<(), GatewayDaemonError> {
    if !path.is_absolute() {
        return Err(unsafe_path(path, "socket path must be absolute"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_path(path, "socket path has no parent"))?;
    match fs::symlink_metadata(parent) {
        Ok(metadata) => {
            validate_socket_ancestor_chain(parent, owner_uid)?;
            validate_private_directory(parent, &metadata, owner_uid)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let grandparent = parent
                .parent()
                .ok_or_else(|| unsafe_path(parent, "socket directory has no parent"))?;
            validate_socket_ancestor_chain(grandparent, owner_uid)?;
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(parent)?;
            let metadata = fs::symlink_metadata(parent)?;
            validate_private_directory(parent, &metadata, owner_uid)?;
        }
        Err(error) => return Err(error.into()),
    }

    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() || metadata.uid() != owner_uid {
                return Err(unsafe_path(
                    path,
                    "existing path is not an owned Unix socket",
                ));
            }
            let stale_identity = (metadata.dev(), metadata.ino());
            if UnixStream::connect(path).is_ok() {
                return Err(GatewayDaemonError::AlreadyRunning(path.to_path_buf()));
            }
            let current = fs::symlink_metadata(path)?;
            if !current.file_type().is_socket()
                || current.uid() != owner_uid
                || (current.dev(), current.ino()) != stale_identity
            {
                return Err(unsafe_path(
                    path,
                    "socket path changed during stale-socket validation",
                ));
            }
            fs::remove_file(path)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn validate_socket_ancestor_chain(
    directory: &Path,
    owner_uid: u32,
) -> Result<(), GatewayDaemonError> {
    for ancestor in directory.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
            return Err(unsafe_path(
                ancestor,
                "socket ancestor is not a real directory",
            ));
        }
        let mode = metadata.permissions().mode();
        let root_sticky = metadata.uid() == 0 && mode & 0o1000 != 0;
        if metadata.uid() != owner_uid && metadata.uid() != 0 {
            return Err(unsafe_path(
                ancestor,
                "socket ancestor has an untrusted owner",
            ));
        }
        if mode & 0o022 != 0 && !root_sticky {
            return Err(unsafe_path(
                ancestor,
                "socket ancestor is writable by another principal",
            ));
        }
    }
    Ok(())
}

fn validate_private_directory(
    path: &Path,
    metadata: &Metadata,
    owner_uid: u32,
) -> Result<(), GatewayDaemonError> {
    let file_type: FileType = metadata.file_type();
    if file_type.is_symlink() || !file_type.is_dir() {
        return Err(unsafe_path(path, "socket parent is not a real directory"));
    }
    if metadata.uid() != owner_uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(unsafe_path(
            path,
            "socket parent must be owned by the effective UID with mode 0700",
        ));
    }
    Ok(())
}

fn unsafe_path(path: &Path, message: &str) -> GatewayDaemonError {
    GatewayDaemonError::UnsafePath {
        path: path.to_path_buf(),
        message: message.to_owned(),
    }
}

fn read_frame<T: for<'de> Deserialize<'de>>(
    reader: &mut impl Read,
) -> Result<T, GatewayDaemonError> {
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header)?;
    let length = usize::try_from(u32::from_be_bytes(header))
        .map_err(|_| GatewayDaemonError::Protocol("frame length is out of range".to_owned()))?;
    if length == 0 || length > MAX_GATEWAY_FRAME_BYTES {
        return Err(GatewayDaemonError::Protocol(format!(
            "frame length must be between 1 and {MAX_GATEWAY_FRAME_BYTES} bytes"
        )));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    serde_json::from_slice(&payload).map_err(GatewayDaemonError::from)
}

fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), GatewayDaemonError> {
    let payload = serde_json::to_vec(value)?;
    let length = u32::try_from(payload.len()).map_err(|_| {
        GatewayDaemonError::Protocol("serialized frame length is out of range".to_owned())
    })?;
    if payload.is_empty() || payload.len() > MAX_GATEWAY_FRAME_BYTES {
        return Err(GatewayDaemonError::Protocol(format!(
            "serialized frame exceeds {MAX_GATEWAY_FRAME_BYTES} bytes"
        )));
    }
    writer.write_all(&length.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()?;
    Ok(())
}

fn error_response(request_id: Option<RequestId>, error: &GatewayDaemonError) -> GatewayResponse {
    let (code, message, recoverable) = match error {
        GatewayDaemonError::Unauthorized => {
            ("unauthenticated", "local peer authentication failed", false)
        }
        GatewayDaemonError::Protocol(_) | GatewayDaemonError::Serialization(_) => (
            "invalid_request",
            "request violates the Gateway contract",
            false,
        ),
        GatewayDaemonError::Store(StoreError::TaskNotFound) => {
            ("not_found", "Task was not found", false)
        }
        GatewayDaemonError::Store(StoreError::IdempotencyConflict) => (
            "idempotency_conflict",
            "idempotency key was used for another command",
            false,
        ),
        GatewayDaemonError::Store(StoreError::RevisionConflict { .. }) => (
            "task_version_conflict",
            "Task changed before the command committed",
            true,
        ),
        GatewayDaemonError::Store(error) => (
            "store_unavailable",
            "durable Task storage is unavailable",
            error.recoverable(),
        ),
        GatewayDaemonError::Io(_) => ("internal", "local transport failed", true),
        GatewayDaemonError::UnsafePath { .. }
        | GatewayDaemonError::AlreadyRunning(_)
        | GatewayDaemonError::Remote { .. } => {
            ("internal", "Gateway cannot complete the request", false)
        }
    };
    GatewayResponse {
        api_version: GATEWAY_API_VERSION.to_owned(),
        request_id,
        outcome: GatewayResponseOutcome::Error {
            error: GatewayErrorBody {
                code: code.to_owned(),
                message: message.to_owned(),
                recoverable,
            },
        },
    }
}
