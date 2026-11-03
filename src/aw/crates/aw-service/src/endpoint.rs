//! A private, locked service endpoint removes only the socket inode it created.

use std::{
    ffi::CString,
    fs::{self, DirBuilder, File, Metadata, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
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
        // The socket must be private from the instant it is observable at its
        // final path: launch_service rejects broader modes without retrying,
        // so a chmod after binding aw.sock races every readiness poll. Bind a
        // staging name inside this locked state directory, apply the permanent
        // mode, then rename: the final path appears atomically at 0600.
        // Existing sockets, including stale ones, require explicit operator
        // cleanup, and the no-replace rename keeps an unexpected aw.sock
        // intact instead of silently replacing it. The reserved staging
        // basename is exactly as long as aw.sock, so the public socket-path
        // limit resolve enforces on the final path (one byte below
        // sockaddr_un's sun_path capacity) also covers the staged bind: every
        // accepted state directory can hold both names at once.
        let staged = state_dir.join(".aw.tmp");
        // The service lock serializes binders of this state directory; leftover
        // staging files are debris from a crashed binder.
        let _ = fs::remove_file(&staged);
        let listener = match bind_staged(&staged, &socket) {
            Ok(listener) => listener,
            Err(error) => {
                let _ = fs::remove_file(&staged);
                return Err(error);
            }
        };
        let metadata = fs::symlink_metadata(&socket)?;
        let endpoint = Self {
            listener,
            socket,
            identity: (metadata.dev(), metadata.ino()),
            _lock: lock,
        };
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

// Deterministic test synchronization for the publication sequence: bind_staged
// notifies this thread's installed checker at every phase boundary, so tests
// assert the publication invariants exactly where they hold instead of polling
// the filesystem from a racing watcher thread.
#[cfg(test)]
mod publication {
    use std::{cell::RefCell, path::Path};

    /// Publication phases of the endpoint socket, notified in order.
    pub(super) enum Phase<'a> {
        /// The staging name is bound; the final path must not exist yet.
        Staged(&'a Path),
        /// The permanent 0600 mode is installed on the staging name; the final
        /// path must not exist yet.
        Sealed(&'a Path),
        /// The final path is published; it must already hold the permanent
        /// mode and accept connections.
        Published(&'a Path),
    }

    /// Name of a phase, for asserting that the full ordered sequence ran.
    fn name(phase: &Phase<'_>) -> &'static str {
        match phase {
            Phase::Staged(_) => "staged",
            Phase::Sealed(_) => "sealed",
            Phase::Published(_) => "published",
        }
    }

    thread_local! {
        static CHECK: RefCell<Option<fn(&Phase<'_>)>> = RefCell::new(None);
        static RECORDED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    /// Run this thread's installed checker, if any, at one phase boundary and
    /// record the phase for [`during`].
    pub(super) fn notify(phase: &Phase<'_>) {
        CHECK.with(|cell| {
            if let Some(check) = *cell.borrow() {
                check(phase);
                RECORDED.with(|recorded| recorded.borrow_mut().push(name(phase)));
            }
        });
    }

    /// Run `body` with `check` called synchronously at every publication phase
    /// of endpoint binds on this thread, returning the body's value and the
    /// ordered names of the phases that ran.
    pub(super) fn during<T>(
        check: fn(&Phase<'_>),
        body: impl FnOnce() -> T,
    ) -> (T, Vec<&'static str>) {
        CHECK.with(|cell| {
            assert!(
                cell.borrow_mut().replace(check).is_none(),
                "nested publication checks are unsupported"
            );
        });
        RECORDED.with(|cell| cell.borrow_mut().clear());
        let value = body();
        CHECK.with(|cell| *cell.borrow_mut() = None);
        let recorded = RECORDED.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
        (value, recorded)
    }
}

fn bind_staged(staged: &Path, socket: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(staged)?;
    #[cfg(test)]
    publication::notify(&publication::Phase::Staged(socket));
    fs::set_permissions(staged, fs::Permissions::from_mode(0o600))?;
    #[cfg(test)]
    publication::notify(&publication::Phase::Sealed(socket));
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket paths must not contain a NUL byte",
        )
    };
    let from = CString::new(staged.as_os_str().as_bytes()).map_err(|_| invalid())?;
    let to = CString::new(socket.as_os_str().as_bytes()).map_err(|_| invalid())?;
    // SAFETY: both names are owned, NUL-terminated path strings that remain
    // valid for the duration of this synchronous system call.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    #[cfg(test)]
    publication::notify(&publication::Phase::Published(socket));
    Ok(listener)
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
    peer_credentials(stream).map(|_| ())
}

pub(crate) fn peer_credentials(stream: &UnixStream) -> io::Result<libc::ucred> {
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
    Ok(credentials)
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

    fn assert_private_publication(phase: &publication::Phase<'_>) {
        match phase {
            publication::Phase::Staged(final_path) | publication::Phase::Sealed(final_path) => {
                // Before publication the final path must not exist at all: a
                // bind directly at the final path would already be observable
                // here with its umask-derived mode.
                match fs::symlink_metadata(final_path) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => panic!("{error}"),
                    Ok(_) => panic!("final path observable before publication"),
                }
            }
            publication::Phase::Published(final_path) => {
                let mode = fs::symlink_metadata(final_path)
                    .expect("published socket")
                    .mode()
                    & 0o777;
                assert_eq!(mode, 0o600, "published socket is not private");
                ipc::connect(final_path, Instant::now() + Duration::from_secs(1))
                    .expect("published socket accepts connections");
            }
        }
    }

    #[test]
    fn socket_permissions_are_private_from_creation() {
        // The launcher's readiness probe rejects a socket whose mode is broader
        // than 0600 without retrying, so the final path must never be observable
        // before its permanent mode is installed. The publication path calls
        // this thread's checker at each phase boundary, which makes the checks
        // exact regardless of scheduling: a watcher thread polling the
        // filesystem can only hope to be running between the vulnerable bind
        // and its chmod.
        let fixture = Fixture::new();
        let state = fixture.state();
        let socket = state.join("aw.sock");
        let (endpoint, phases) = publication::during(assert_private_publication, || {
            Endpoint::bind(&state).unwrap()
        });
        // A revert that binds aw.sock directly cannot produce this sequence:
        // either a phase observes the final path before publication, or the
        // publication phases never run at all.
        assert_eq!(phases, ["staged", "sealed", "published"]);
        assert_eq!(fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        drop(endpoint);
        assert!(!socket.exists());
    }

    struct TempRoot(PathBuf);

    impl TempRoot {
        fn new() -> Self {
            let mut template = b"/tmp/aw-boundary-XXXXXX\0".to_vec();
            // SAFETY: mkdtemp receives a writable, terminated template and
            // creates an exclusively owned 0700 directory; Drop removes it.
            let pointer = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
            assert!(!pointer.is_null(), "{}", io::Error::last_os_error());
            let name = std::ffi::CStr::from_bytes_with_nul(&template).unwrap();
            Self(PathBuf::from(name.to_str().unwrap()))
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn accepted_maximum_socket_path_binds_publishes_and_connects() {
        // resolve accepts socket paths of at most 107 bytes (sockaddr_un
        // reserves its final byte for the terminator), so the staging name has
        // to fit the same budget: staging under a longer basename would push
        // accepted paths past the bind limit. Drive the accepted maximum end
        // to end through resolve and Server::bind, and keep the one-byte-longer
        // path rejected.
        let root = TempRoot::new();
        // A state directory of exactly 99 bytes makes state/aw.sock exactly
        // 107 bytes, the accepted maximum.
        let state = root
            .0
            .join("x".repeat(99 - root.0.as_os_str().as_bytes().len() - 1));
        let mut document = serde_json::json!({
            "apiVersion":"aw/v1alpha1","kind":"AWConfiguration","metadata":{"name":"launch"},"spec":{
                "daemon":{"startup":"on_demand","endpoint":"auto","state_dir":"auto"},
                "execution":{"guarantee":"native_hook","default_event_budget_ms":1000},
                "audit":{"enabled":true,"payload":"metadata_only"},
                "agents":{"qoder":{"adapter":"qoder","argv":["qodercli"]}},"providers":{},"events":{}}
        });
        document["spec"]["daemon"]["state_dir"] = serde_json::json!(state.to_str().unwrap());
        let bytes = serde_json::to_vec(&document).unwrap();
        let paths = crate::launch_service::resolve(&bytes, None).unwrap();
        assert_eq!(paths.socket.as_os_str().as_bytes().len(), 107);
        assert_eq!(paths.socket, state.join("aw.sock"));
        let server = crate::Server::bind(bytes, &state).unwrap();
        let socket = server.socket_path();
        assert_eq!(fs::metadata(&socket).unwrap().mode() & 0o777, 0o600);
        let client = ipc::connect(&socket, Instant::now() + Duration::from_secs(1)).unwrap();
        drop(client);
        drop(server);
        assert!(!socket.exists());
        // One byte more stays rejected by resolve before any bind is attempted.
        let too_long = root
            .0
            .join("x".repeat(100 - root.0.as_os_str().as_bytes().len() - 1));
        document["spec"]["daemon"]["state_dir"] = serde_json::json!(too_long.to_str().unwrap());
        let bytes = serde_json::to_vec(&document).unwrap();
        assert!(crate::launch_service::resolve(&bytes, None).is_err());
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
