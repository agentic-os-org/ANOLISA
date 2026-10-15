//! A private, locked service endpoint removes only the socket inode it created.

use std::{
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
};

pub(crate) struct Endpoint {
    listener: UnixListener,
    socket: PathBuf,
    identity: (u64, u64),
    // Retained until socket cleanup finishes, preventing a second service owner.
    _lock: File,
}

impl Endpoint {
    pub(crate) fn bind(state_dir: &Path) -> io::Result<Self> {
        private_directory(state_dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(state_dir.join("service.lock"))?;
        let metadata = lock.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != current_uid()
            || metadata.mode() & 0o7777 != 0o600
            || metadata.nlink() != 1
        {
            return Err(permission(
                "service lock must be a private, singly linked regular file",
            ));
        }
        // flock operates on the live descriptor retained by Endpoint.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = state_dir.join("aw.sock");
        // Existing sockets, including stale ones, require explicit operator cleanup.
        let listener = UnixListener::bind(&socket)?;
        let metadata = fs::symlink_metadata(&socket)?;
        let endpoint = Self {
            listener,
            socket,
            identity: (metadata.dev(), metadata.ino()),
            _lock: lock,
        };
        fs::set_permissions(&endpoint.socket, fs::Permissions::from_mode(0o600))?;
        endpoint.listener.set_nonblocking(true)?;
        Ok(endpoint)
    }

    pub(crate) fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.socket) {
            if metadata.file_type().is_socket() && (metadata.dev(), metadata.ino()) == self.identity
            {
                let _ = fs::remove_file(&self.socket);
            }
        }
    }
}

fn current_uid() -> u32 {
    // geteuid has no preconditions and identifies the service's filesystem owner.
    unsafe { libc::geteuid() }
}

fn permission(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn owned_directory(metadata: &Metadata) -> bool {
    metadata.is_dir() && metadata.uid() == current_uid()
}

fn private_directory(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "service state directory must be absolute",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "state directory needs a parent",
        )
    })?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !owned_directory(&parent_metadata) || parent_metadata.mode() & 0o022 != 0 {
        return Err(permission(
            "state directory parent must be owned by the current user and not writable by others",
        ));
    }
    match fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !owned_directory(&metadata) || metadata.mode() & 0o7777 != 0o700 {
        return Err(permission(
            "state directory must be owned by the current user with mode 0700",
        ));
    }
    Ok(())
}

pub(crate) fn same_user(stream: &UnixStream) -> io::Result<()> {
    // ucred is a plain C structure and getsockopt initializes it on success.
    let mut credentials = unsafe { std::mem::zeroed::<libc::ucred>() };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if length as usize != std::mem::size_of::<libc::ucred>() || credentials.uid != current_uid() {
        return Err(permission("service peer must belong to the current user"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc;
    use std::{
        os::unix::fs::symlink,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    static NEXT: AtomicU64 = AtomicU64::new(1);

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/endpoint-tests");
            fs::create_dir_all(&root).unwrap();
            let root = fs::canonicalize(root).unwrap();
            let path = root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            DirBuilder::new().mode(0o700).create(&path).unwrap();
            Self(path)
        }

        fn state(&self) -> PathBuf {
            self.0.join("state")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn creates_private_endpoint_and_removes_owned_socket() {
        let fixture = Fixture::new();
        let state = fixture.state();
        let socket = state.join("aw.sock");
        let endpoint = Endpoint::bind(&state).unwrap();
        assert_eq!(endpoint.socket_path(), socket);
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        let client = ipc::connect(&socket, Instant::now() + Duration::from_secs(1)).unwrap();
        let (server, _) = endpoint.listener().accept().unwrap();
        same_user(&server).unwrap();
        same_user(&client).unwrap();
        drop(endpoint);
        assert!(!socket.exists());
        assert!(state.join("service.lock").exists());
        assert!(Endpoint::bind(&state).is_ok());
    }

    #[test]
    fn rejects_second_owner_and_existing_socket() {
        let fixture = Fixture::new();
        let state = fixture.state();
        let endpoint = Endpoint::bind(&state).unwrap();
        assert!(Endpoint::bind(&state).is_err());
        drop(endpoint);
        let stale = UnixListener::bind(state.join("aw.sock")).unwrap();
        assert!(Endpoint::bind(&state).is_err());
        assert!(state.join("aw.sock").exists());
        drop(stale);
    }

    #[test]
    fn bounds_connection_when_listener_backlog_is_full() {
        let fixture = Fixture::new();
        let endpoint = Endpoint::bind(&fixture.state()).unwrap();
        // Reduce this owned listener's queue so saturation requires few sockets.
        assert_eq!(
            unsafe { libc::listen(endpoint.listener().as_raw_fd(), 1) },
            0
        );
        let mut queued = Vec::new();
        let mut saturated = false;
        for _ in 0..8 {
            let start = Instant::now();
            match ipc::connect(endpoint.socket_path(), start + Duration::from_millis(25)) {
                Ok(stream) => queued.push(stream),
                Err(error) => {
                    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
                    assert!(start.elapsed() < Duration::from_secs(1));
                    saturated = true;
                    break;
                }
            }
        }
        assert!(saturated);
    }

    #[test]
    fn does_not_remove_a_replacement_socket() {
        let fixture = Fixture::new();
        let state = fixture.state();
        let endpoint = Endpoint::bind(&state).unwrap();
        let socket = state.join("aw.sock");
        fs::rename(&socket, state.join("moved.sock")).unwrap();
        let replacement = UnixListener::bind(&socket).unwrap();
        drop(endpoint);
        assert!(socket.exists());
        assert!(ipc::connect(&socket, Instant::now() + Duration::from_secs(1)).is_ok());
        drop(replacement);
    }

    #[test]
    fn rejects_relative_symlink_and_permissive_state_without_chmod() {
        let fixture = Fixture::new();
        assert!(Endpoint::bind(Path::new("relative-state")).is_err());
        let state = fixture.state();
        DirBuilder::new().mode(0o755).create(&state).unwrap();
        assert!(Endpoint::bind(&state).is_err());
        assert_eq!(fs::metadata(&state).unwrap().mode() & 0o777, 0o755);
        let alias = fixture.0.join("alias");
        symlink(&state, &alias).unwrap();
        assert!(Endpoint::bind(&alias).is_err());
        assert!(Endpoint::bind(&fixture.0.join("missing-parent/state")).is_err());
    }

    #[test]
    fn rejects_symlink_hardlink_and_permissive_lock() {
        let fixture = Fixture::new();
        let state = fixture.state();
        DirBuilder::new().mode(0o700).create(&state).unwrap();
        let lock = state.join("service.lock");
        let source = fixture.0.join("source");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&source)
            .unwrap();
        symlink(&source, &lock).unwrap();
        assert!(Endpoint::bind(&state).is_err());
        fs::remove_file(&lock).unwrap();
        fs::hard_link(&source, &lock).unwrap();
        assert!(Endpoint::bind(&state).is_err());
        fs::remove_file(&lock).unwrap();
        fs::rename(&source, &lock).unwrap();
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Endpoint::bind(&state).is_err());
        assert_eq!(fs::metadata(&lock).unwrap().mode() & 0o777, 0o644);
    }
}
