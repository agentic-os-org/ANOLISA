//! Remember the service selected by a mutable configuration path, without storing policy.

use super::Result;
use aw_service::{Client, Identity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
        },
    },
    path::{Path, PathBuf},
    time::Instant,
};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    configuration: PathBuf,
    socket: PathBuf,
    identity: Identity,
}

fn private_directory(path: &Path, create: bool) -> Result<bool> {
    if create {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o700
    {
        return Err("unsafe AW connection directory".into());
    }
    Ok(true)
}

fn location(config: &Path, create: bool) -> Result<Option<(PathBuf, PathBuf)>> {
    let configuration = config.canonicalize()?;
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let root = match &runtime {
        Some(directory) => directory.join("aw"),
        None => PathBuf::from(format!("/tmp/aw-{}", unsafe { libc::geteuid() })),
    };
    let directory = root.join("connections");
    let key = format!("{:x}", Sha256::digest(configuration.as_os_str().as_bytes()));
    let path = directory.join(format!("{key}.json"));
    // Legacy explicit service paths do not use XDG_RUNTIME_DIR. Only impose
    // connection-directory requirements when a record is created or present.
    if !create {
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        }
    }
    if let Some(runtime) = &runtime {
        if !runtime.is_absolute() || !private_directory(runtime, false)? {
            return Err("invalid XDG_RUNTIME_DIR for AW connections".into());
        }
    }
    if !private_directory(&root, create)? {
        return Ok(None);
    }
    if !private_directory(&directory, create)? {
        return Ok(None);
    }
    Ok(Some((configuration, path)))
}

fn private_file(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o7777 != 0o600
        || metadata.nlink() != 1
    {
        return Err("unsafe AW connection file".into());
    }
    Ok(())
}

fn read(path: &Path, configuration: &Path) -> Result<Option<Connection>> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("AW connection file exceeds limit".into());
    }
    let connection: Connection = serde_json::from_slice(&bytes)?;
    if connection.configuration != configuration {
        return Err("AW connection configuration path mismatch".into());
    }
    Ok(Some(connection))
}

fn lock(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path.with_extension("lock"))?;
    private_file(&file)?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err("AW connection is being changed; retry explicitly".into());
    }
    Ok(file)
}

pub(super) fn remember(config: &Path, socket: &Path, client: &Client) -> Result<()> {
    let (configuration, path) = location(config, true)?.ok_or("AW connection directory missing")?;
    let _lock = lock(&path)?;
    if let Some(previous) = read(&path, &configuration)? {
        if previous.socket != socket && previous.socket.try_exists()? {
            return Err("configuration path already selects another service; stop it before reconnecting, or use a separate config path".into());
        }
    }
    let connection = Connection {
        configuration,
        socket: socket.into(),
        identity: client.identity().clone(),
    };
    let directory = path.parent().ok_or("connection path has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec(&connection)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(&path)?;
    File::open(directory)?.sync_all()?;
    Ok(())
}

pub(super) fn recorded(config: &Path) -> Result<bool> {
    let Some((configuration, path)) = location(config, false)? else {
        return Ok(false);
    };
    Ok(read(&path, &configuration)?.is_some())
}

pub(super) fn selected(
    config: &Path,
    deadline: Instant,
    reconnect: bool,
) -> Result<Option<(Client, PathBuf)>> {
    let Some((configuration, path)) = location(config, false)? else {
        return Ok(None);
    };
    let Some(connection) = read(&path, &configuration)? else {
        return Ok(None);
    };
    if reconnect && !connection.socket.try_exists()? {
        return Ok(None);
    }
    let client = Client::connect(&connection.socket, deadline)?;
    if !reconnect && client.identity() != &connection.identity {
        return Err("recorded AW service changed; reconnect explicitly before updating".into());
    }
    Ok(Some((client, connection.socket)))
}

pub(super) fn connect(config: &Path, deadline: Instant) -> Result<Option<Client>> {
    Ok(selected(config, deadline, false)?.map(|(client, _)| client))
}

pub(crate) fn forget(config: &Path, identity: &Identity) -> Result<()> {
    let Some((configuration, path)) = location(config, false)? else {
        return Ok(());
    };
    let _lock = lock(&path)?;
    if let Some(connection) = read(&path, &configuration)? {
        if &connection.identity == identity {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}
