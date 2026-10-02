//! Unix-domain socket service for the enforcement protocol.

use std::fs;
use std::io::BufReader;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use agentsight_enforcement_protocol::{
    Command, PROTOCOL_VERSION, ProtocolError, RemoteError, ReplaceOutcome, ReplacePolicy, Request,
    Response, ResponseBody, read_frame, write_frame,
};
use thiserror::Error;
use uuid::Uuid;

use crate::{BackendError, EnforcementBackend, SubscriberClass};

const REQUIRED_SUBSCRIPTION_UNAVAILABLE: &str = "required_subscription_unavailable";

/// Failures while binding or serving the local protocol socket.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// Socket or filesystem setup failed.
    #[error("enforcer service I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Backend cleanup failed during controlled shutdown.
    #[error("enforcer backend shutdown failed: {0}")]
    Backend(#[from] BackendError),
}

/// Local UDS server around one enforcement backend.
pub struct EnforcerService<B: EnforcementBackend> {
    listener: UnixListener,
    backend: Arc<B>,
    socket_path: PathBuf,
    allowed_gid: Option<u32>,
}

#[derive(Default)]
struct RequiredSubscriptions {
    current: Mutex<Option<RequiredSubscription>>,
}

struct RequiredSubscription {
    id: Uuid,
    stream: UnixStream,
}

impl<B: EnforcementBackend> EnforcerService<B> {
    /// Binds a mode-0660 socket at `socket_path`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the socket cannot be bound, permissioned, or
    /// switched to non-blocking accept mode.
    pub fn bind(
        socket_path: impl AsRef<Path>,
        backend: Arc<B>,
        allowed_gid: Option<u32>,
    ) -> Result<Self, ServiceError> {
        let socket_path = socket_path.as_ref().to_path_buf();
        prepare_socket_path(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)?;
        fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o660))?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            backend,
            socket_path,
            allowed_gid,
        })
    }

    /// Accepts connections until `stop` is set.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when accepting a connection fails for a reason
    /// other than the non-blocking listener having no pending client.
    pub fn serve_until(self, stop: &AtomicBool) -> Result<(), ServiceError> {
        let required_subscriptions = Arc::new(RequiredSubscriptions::default());
        while !stop.load(Ordering::Acquire) {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let backend = Arc::clone(&self.backend);
                    let required_subscriptions = Arc::clone(&required_subscriptions);
                    let allowed_gid = self.allowed_gid;
                    thread::spawn(move || {
                        if let Err(error) =
                            handle_connection(stream, backend, required_subscriptions, allowed_gid)
                        {
                            log::error!("agentsight-enforcer connection failed: {error}");
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => return Err(ServiceError::Io(error)),
            }
        }
        self.backend.shutdown()?;
        Ok(())
    }
}

fn prepare_socket_path(socket_path: &Path) -> Result<(), std::io::Error> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("refusing to replace non-socket path {socket_path:?}"),
        ));
    }
    match UnixStream::connect(socket_path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("enforcer is already listening at {socket_path:?}"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            fs::remove_file(socket_path)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

impl RequiredSubscriptions {
    fn install(&self, id: Uuid, stream: &UnixStream) -> Result<Option<Uuid>, std::io::Error> {
        let replacement = RequiredSubscription {
            id,
            stream: stream.try_clone()?,
        };
        Ok(self
            .current()
            .replace(replacement)
            .map(|previous| previous.id))
    }

    fn remove_if_current(&self, id: Uuid) {
        let mut current = self.current();
        if current.as_ref().is_some_and(|entry| entry.id == id) {
            *current = None;
        }
    }

    fn is_current(&self, id: Uuid) -> bool {
        self.current().as_ref().is_some_and(|entry| entry.id == id)
    }

    fn with_current<T>(
        &self,
        id: Uuid,
        operation: impl FnOnce() -> Result<T, BackendError>,
        rollback: impl FnOnce(&T) -> Result<(), BackendError>,
    ) -> Result<T, RemoteError> {
        let mut current = self.current();
        let Some(entry) = current.as_ref() else {
            return Err(required_subscription_error(
                "required violation subscription is not connected",
            ));
        };
        if entry.id != id {
            return Err(required_subscription_error(
                "required violation subscription lease is stale",
            ));
        }
        match peer_is_connected(&entry.stream) {
            Ok(true) => {}
            Ok(false) => {
                *current = None;
                return Err(required_subscription_error(
                    "required violation subscription disconnected",
                ));
            }
            Err(error) => {
                *current = None;
                return Err(required_subscription_error(&format!(
                    "check required violation subscription: {error}"
                )));
            }
        }
        let result = operation().map_err(remote_backend_error);
        if result.is_ok() {
            let lease_failure = match peer_is_connected(&entry.stream) {
                Ok(true) => None,
                Ok(false) => Some(
                    "required violation subscription disconnected during policy apply".to_string(),
                ),
                Err(error) => Some(format!(
                    "recheck required violation subscription after policy apply: {error}"
                )),
            };
            if let Some(mut message) = lease_failure {
                if let Some(value) = result.as_ref().ok()
                    && let Err(error) = rollback(value)
                {
                    message.push_str(&format!("; policy rollback failed: {error}"));
                }
                *current = None;
                return Err(required_subscription_error(&message));
            }
        }
        drop(current);
        result
    }

    fn serialize_backend_operation<T>(
        &self,
        operation: impl FnOnce() -> Result<T, BackendError>,
    ) -> Result<T, BackendError> {
        let current = self.current();
        let result = operation();
        drop(current);
        result
    }

    fn combine_health(
        &self,
        mut health: agentsight_enforcement_protocol::HealthStatus,
    ) -> agentsight_enforcement_protocol::HealthStatus {
        let message = {
            let mut current = self.current();
            match current.as_ref() {
                None => Some("required violation subscription is not connected".to_string()),
                Some(entry) => match peer_is_connected(&entry.stream) {
                    Ok(true) => None,
                    Ok(false) => {
                        *current = None;
                        Some("required violation subscription disconnected".to_string())
                    }
                    Err(error) => {
                        *current = None;
                        Some(format!("check required violation subscription: {error}"))
                    }
                },
            }
        };
        let Some(message) = message else {
            return health;
        };
        health.ready = false;
        health.message = Some(match health.message.take() {
            Some(backend) if !backend.is_empty() && backend != message => {
                format!("{backend}; {message}")
            }
            Some(backend) if !backend.is_empty() => backend,
            _ => message,
        });
        health
    }

    fn current(&self) -> MutexGuard<'_, Option<RequiredSubscription>> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn peer_is_connected(stream: &UnixStream) -> Result<bool, std::io::Error> {
    loop {
        let mut pollfd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `pollfd` points to one initialized descriptor for this call.
        let result = unsafe { libc::poll(&mut pollfd, 1, 0) };
        if result == 0 {
            return Ok(true);
        }
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if pollfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            return Ok(false);
        }
        if pollfd.revents & libc::POLLIN == 0 {
            return Ok(true);
        }
        let mut byte = [0_u8; 1];
        // SAFETY: the socket FD is valid and the one-byte buffer is writable
        // for the supplied length; MSG_PEEK leaves stream contents untouched.
        let result = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                byte.as_mut_ptr().cast(),
                byte.len(),
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if result == 0 {
            return Ok(false);
        }
        if result > 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        return if error.kind() == std::io::ErrorKind::WouldBlock {
            Ok(true)
        } else {
            Err(error)
        };
    }
}

impl<B: EnforcementBackend> Drop for EnforcerService<B> {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.socket_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("agentsight-enforcer could not remove socket: {error}");
        }
    }
}

fn handle_connection<B: EnforcementBackend>(
    mut stream: UnixStream,
    backend: Arc<B>,
    required_subscriptions: Arc<RequiredSubscriptions>,
    allowed_gid: Option<u32>,
) -> Result<(), ProtocolError> {
    if !peer_is_authorized(&stream, allowed_gid)? {
        return write_frame(
            &mut stream,
            &error_response(uuid::Uuid::nil(), "unauthorized", "peer is not authorized"),
        );
    }

    let request = match read_frame::<_, Request>(&mut BufReader::new(&stream)) {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        Err(error) => {
            write_frame(
                &mut stream,
                &error_response(uuid::Uuid::nil(), "invalid_frame", &error.to_string()),
            )?;
            return Ok(());
        }
    };

    let command = match request.command {
        Command::SubscribeViolations { subscription_id } => {
            return serve_subscription(
                stream,
                backend,
                required_subscriptions,
                request.request_id,
                subscription_id,
                SubscriberClass::BestEffort,
            );
        }
        Command::SubscribeRequiredViolations { subscription_id } => {
            return serve_subscription(
                stream,
                backend,
                required_subscriptions,
                request.request_id,
                subscription_id,
                SubscriberClass::Required,
            );
        }
        Command::SubscribeSecurityEvents => {
            let receiver = backend.subscribe_security_events();
            write_frame(
                &mut stream,
                &success_response(request.request_id, ResponseBody::Subscribed),
            )?;
            while let Ok(event) = receiver.recv() {
                if write_frame(
                    &mut stream,
                    &success_response(request.request_id, ResponseBody::SecurityEvent(event)),
                )
                .is_err()
                {
                    let queued_events = receiver
                        .try_iter()
                        .fold(0_u64, |count, _| count.saturating_add(1));
                    backend.record_security_delivery_loss(queued_events.saturating_add(1));
                    return Ok(());
                }
            }
            return Ok(());
        }
        command => command,
    };

    let result = dispatch(&*backend, &required_subscriptions, command);
    write_frame(
        &mut stream,
        &Response {
            protocol_version: PROTOCOL_VERSION,
            request_id: request.request_id,
            result,
        },
    )
}

fn serve_subscription<B: EnforcementBackend>(
    mut stream: UnixStream,
    backend: Arc<B>,
    required_subscriptions: Arc<RequiredSubscriptions>,
    request_id: Uuid,
    subscription_id: Uuid,
    class: SubscriberClass,
) -> Result<(), ProtocolError> {
    let receiver = backend.subscribe(subscription_id, class);
    if class == SubscriberClass::Required {
        let previous = match required_subscriptions.install(subscription_id, &stream) {
            Ok(previous) => previous,
            Err(error) => {
                backend.unsubscribe(subscription_id);
                return Err(error.into());
            }
        };
        if let Some(previous) = previous
            && previous != subscription_id
        {
            backend.unsubscribe(previous);
        }
    }

    let result = stream_subscription(
        &mut stream,
        &receiver,
        &required_subscriptions,
        request_id,
        subscription_id,
        class,
    );
    backend.unsubscribe(subscription_id);
    if class == SubscriberClass::Required {
        let queued_events = receiver
            .try_iter()
            .fold(0_u64, |count, _| count.saturating_add(1));
        let failed_write = result.as_ref().copied().unwrap_or(0);
        backend.record_required_delivery_loss(queued_events.saturating_add(failed_write));
        required_subscriptions.remove_if_current(subscription_id);
    }
    result.map(|_| ())
}

fn stream_subscription(
    stream: &mut UnixStream,
    receiver: &Receiver<agentsight_enforcement_protocol::ViolationEvent>,
    required_subscriptions: &RequiredSubscriptions,
    request_id: Uuid,
    subscription_id: Uuid,
    class: SubscriberClass,
) -> Result<u64, ProtocolError> {
    write_frame(
        stream,
        &success_response(request_id, ResponseBody::Subscribed),
    )?;
    loop {
        if class == SubscriberClass::Required && !required_subscriptions.is_current(subscription_id)
        {
            return Ok(0);
        }
        match receiver.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => {
                if !peer_is_connected(stream)?
                    || (class == SubscriberClass::Required
                        && !required_subscriptions.is_current(subscription_id))
                {
                    return Ok(u64::from(class == SubscriberClass::Required));
                }
                if write_frame(
                    stream,
                    &success_response(request_id, ResponseBody::Violation(event)),
                )
                .is_err()
                {
                    return Ok(u64::from(class == SubscriberClass::Required));
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if !peer_is_connected(stream)? {
                    return Ok(0);
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(0),
        }
    }
}

fn dispatch<B: EnforcementBackend>(
    backend: &B,
    required_subscriptions: &RequiredSubscriptions,
    command: Command,
) -> Result<ResponseBody, RemoteError> {
    match command {
        Command::Health => backend
            .health()
            .map(|health| ResponseBody::Health(required_subscriptions.combine_health(health)))
            .map_err(remote_backend_error),
        Command::ApplyPolicy(_) => Err(required_subscription_error(
            "policy apply requires a live required subscription lease",
        )),
        Command::ApplyPolicyLeased {
            request,
            required_subscription_id,
        } => {
            let binding_id = request.binding_id;
            required_subscriptions
                .with_current(
                    required_subscription_id,
                    || {
                        let already_present = backend
                            .bindings()?
                            .iter()
                            .any(|binding| binding.request.binding_id == binding_id);
                        backend
                            .apply(request)
                            .map(|binding| (binding, !already_present))
                    },
                    |(binding, created)| {
                        if *created {
                            backend.detach(binding.request.binding_id)
                        } else {
                            Ok(())
                        }
                    },
                )
                .map(|(binding, _)| ResponseBody::Applied(binding))
        }
        Command::ApplyCredentialPolicy(_) => Err(required_subscription_error(
            "credential policy apply requires a live required subscription lease",
        )),
        Command::ApplyCredentialPolicyLeased {
            request,
            required_subscription_id,
        } => {
            let binding_id = request.binding_id;
            required_subscriptions
                .with_current(
                    required_subscription_id,
                    || {
                        let already_present = backend
                            .bindings()?
                            .iter()
                            .any(|binding| binding.request.binding_id == binding_id);
                        backend
                            .apply_credential_policy(request)
                            .map(|binding| (binding, !already_present))
                    },
                    |(binding, created)| {
                        if *created {
                            backend.detach(binding.request.binding_id)
                        } else {
                            Ok(())
                        }
                    },
                )
                .map(|(binding, _)| ResponseBody::Applied(binding))
        }
        Command::ReplacePolicy(_) => Err(required_subscription_error(
            "policy replacement requires a live required subscription lease",
        )),
        Command::ReplacePolicyLeased {
            request,
            required_subscription_id,
        } => {
            let rollback_request = request.clone();
            required_subscriptions
                .with_current(
                    required_subscription_id,
                    || backend.replace(request),
                    |outcome| rollback_replacement(backend, &rollback_request, outcome),
                )
                .map(ResponseBody::Replaced)
        }
        Command::DetachAgent { binding_id } => {
            required_subscriptions
                .serialize_backend_operation(|| backend.detach(binding_id))
                .map_err(remote_backend_error)?;
            Ok(ResponseBody::Detached)
        }
        Command::ListBindings => backend
            .bindings()
            .map(ResponseBody::Bindings)
            .map_err(remote_backend_error),
        Command::SubscribeViolations { .. } | Command::SubscribeRequiredViolations { .. } => {
            Ok(ResponseBody::Subscribed)
        }
        Command::SubscribeSecurityEvents => Ok(ResponseBody::Subscribed),
    }
}

fn rollback_replacement<B: EnforcementBackend>(
    backend: &B,
    request: &ReplacePolicy,
    outcome: &ReplaceOutcome,
) -> Result<(), BackendError> {
    let ReplaceOutcome::Applied(target) = outcome else {
        return if matches!(outcome, ReplaceOutcome::Indeterminate { .. }) {
            Err(BackendError::KernelFailure(
                "replacement ownership is indeterminate".into(),
            ))
        } else {
            Ok(())
        };
    };
    let reverse = request.reverse(target.clone());
    let expected = reverse.clone();
    match backend.replace(reverse)? {
        ReplaceOutcome::Applied(restored)
            if expected.validate_acknowledgement(&restored).is_ok() =>
        {
            Ok(())
        }
        _ => Err(BackendError::KernelFailure(
            "replacement rollback could not restore the source".into(),
        )),
    }
}

fn success_response(request_id: uuid::Uuid, body: ResponseBody) -> Response {
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result: Ok(body),
    }
}

fn error_response(request_id: uuid::Uuid, code: &str, message: &str) -> Response {
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        result: Err(RemoteError {
            code: code.into(),
            message: message.into(),
        }),
    }
}

fn required_subscription_error(message: &str) -> RemoteError {
    RemoteError {
        code: REQUIRED_SUBSCRIPTION_UNAVAILABLE.into(),
        message: message.into(),
    }
}

fn remote_backend_error(error: BackendError) -> RemoteError {
    let code = match &error {
        BackendError::BindingConflict(_) => "binding_conflict",
        BackendError::MissingBinding(_) => "missing_binding",
        BackendError::StaleProcess { .. } => "stale_process",
        BackendError::CompileFailure(_) => "compile_failure",
        BackendError::KernelFailure(_) => "kernel_failure",
    };
    RemoteError {
        code: code.into(),
        message: error.to_string(),
    }
}

#[cfg(target_os = "linux")]
fn peer_is_authorized(
    stream: &UnixStream,
    allowed_gid: Option<u32>,
) -> Result<bool, ProtocolError> {
    use std::mem::{size_of, zeroed};
    use std::os::fd::AsRawFd;

    // SAFETY: `ucred` is a plain C struct initialized before the kernel writes
    // exactly `size_of::<ucred>()` bytes through getsockopt.
    let mut credentials: libc::ucred = unsafe { zeroed() };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the socket FD is valid for this call and both pointers reference
    // writable storage whose size is provided in `length`.
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
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: `geteuid` has no preconditions and does not dereference pointers.
    let service_uid = unsafe { libc::geteuid() };
    Ok(credentials.uid == 0
        || credentials.uid == service_uid
        || allowed_gid.is_some_and(|gid| credentials.gid == gid))
}

#[cfg(not(target_os = "linux"))]
fn peer_is_authorized(
    _stream: &UnixStream,
    _allowed_gid: Option<u32>,
) -> Result<bool, ProtocolError> {
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    use agentsight_enforcement_protocol::{
        ApplyCredentialPolicy, ApplyPolicy, Binding, CredentialExfiltrationPolicy,
        DestinationScope, HealthStatus, PolicyMode, ReplacePolicy, ReplacementPolicy,
        ReplacementSource, SecurityEvent, ViolationEvent,
    };

    use super::*;
    use crate::MockBackend;

    struct DetachRaceBackend {
        active: Arc<AtomicBool>,
    }

    fn credential_policy() -> ApplyCredentialPolicy {
        ApplyCredentialPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "fixture-agent".into(),
            session_id: None,
            root_pid: 42,
            process_start_time: 99,
            policy: CredentialExfiltrationPolicy {
                policy_id: "credential-exfiltration".into(),
                revision: 1,
                source_patterns: vec!["/tmp/fixture-credential".into()],
                trusted_endpoints: Vec::new(),
                taint_label: "CREDENTIAL".into(),
                taint_ttl_secs: 900,
                destination_scope: DestinationScope::PublicIpv4,
                mode: PolicyMode::Audit,
            },
        }
    }

    fn replacement_policy() -> ReplacePolicy {
        let source = ApplyPolicy {
            binding_id: Uuid::new_v4(),
            agent_id: "fixture-agent".into(),
            session_id: None,
            root_pid: 42,
            process_start_time: 99,
            policy_id: "fixture-audit".into(),
            policy_revision: "1".into(),
            policy_dsl: "label AGENT".into(),
            policy_mode: Some(PolicyMode::Audit),
        };
        let mut target = source.clone();
        target.binding_id = Uuid::new_v4();
        target.policy_id = "fixture-enforce".into();
        ReplacePolicy {
            expected: Binding {
                request: source,
                state: agentsight_enforcement_protocol::BindingState::Enforced,
                message: None,
                domain_id: Some(1),
            },
            source: ReplacementSource::Generic,
            replacement: ReplacementPolicy::Generic(target),
        }
    }

    #[test]
    fn controlled_service_shutdown_detaches_active_bindings() {
        let backend = Arc::new(MockBackend::new());
        let request = replacement_policy().expected.request;
        backend
            .apply(request)
            .expect("fixture binding should become active");
        let socket_path = std::env::temp_dir().join(format!(
            "as-shutdown-{}.sock",
            &Uuid::new_v4().to_string()[..8]
        ));
        let service = EnforcerService::bind(&socket_path, Arc::clone(&backend), None)
            .expect("fixture service should bind");
        let stop = AtomicBool::new(true);

        service
            .serve_until(&stop)
            .expect("controlled shutdown should clean the backend");

        assert!(
            EnforcementBackend::bindings(backend.as_ref())
                .expect("bindings should remain queryable")
                .is_empty()
        );
        let _ = std::fs::remove_file(socket_path);
    }

    impl EnforcementBackend for DetachRaceBackend {
        fn health(&self) -> Result<HealthStatus, BackendError> {
            Ok(HealthStatus {
                ready: true,
                backend: "detach-race".into(),
                capabilities:
                    agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                message: None,
            })
        }

        fn apply(&self, _request: ApplyPolicy) -> Result<Binding, BackendError> {
            Err(BackendError::KernelFailure(
                "fixture apply is driven by the test interleaving".into(),
            ))
        }

        fn apply_credential_policy(
            &self,
            _request: ApplyCredentialPolicy,
        ) -> Result<Binding, BackendError> {
            Err(BackendError::KernelFailure(
                "fixture does not apply credential policies".into(),
            ))
        }

        fn replace(&self, _request: ReplacePolicy) -> Result<ReplaceOutcome, BackendError> {
            Err(BackendError::KernelFailure(
                "fixture does not replace policies".into(),
            ))
        }

        fn detach(&self, binding_id: Uuid) -> Result<(), BackendError> {
            if self.active.swap(false, Ordering::AcqRel) {
                Ok(())
            } else {
                Err(BackendError::MissingBinding(binding_id))
            }
        }

        fn bindings(&self) -> Result<Vec<Binding>, BackendError> {
            Ok(Vec::new())
        }

        fn subscribe(&self, _id: Uuid, _class: SubscriberClass) -> Receiver<ViolationEvent> {
            mpsc::sync_channel(1).1
        }

        fn unsubscribe(&self, _id: Uuid) {}

        fn record_required_delivery_loss(&self, _count: u64) {}

        fn subscribe_security_events(&self) -> Receiver<SecurityEvent> {
            mpsc::sync_channel(1).1
        }

        fn record_security_delivery_loss(&self, _count: u64) {}
    }

    #[test]
    fn disconnect_during_leased_operation_rolls_back_the_side_effect() {
        let (service_stream, peer_stream) =
            UnixStream::pair().expect("fixture socket pair should open");
        let subscription_id = Uuid::new_v4();
        let subscriptions = Arc::new(RequiredSubscriptions::default());
        subscriptions
            .install(subscription_id, &service_stream)
            .expect("fixture subscription should install");
        let (operation_entered, entered) = mpsc::channel();
        let (release_operation, released) = mpsc::channel();
        let rolled_back = Arc::new(AtomicBool::new(false));
        let thread_subscriptions = Arc::clone(&subscriptions);
        let thread_rolled_back = Arc::clone(&rolled_back);

        let operation = thread::spawn(move || {
            thread_subscriptions.with_current(
                subscription_id,
                || {
                    operation_entered
                        .send(())
                        .expect("fixture receiver should remain");
                    released.recv().expect("operation should be released");
                    Ok(())
                },
                |_| {
                    thread_rolled_back.store(true, Ordering::Release);
                    Ok(())
                },
            )
        });
        entered.recv().expect("operation should enter");
        drop(peer_stream);
        release_operation
            .send(())
            .expect("operation should still be running");

        let result = operation.join().expect("operation should stop");
        assert!(matches!(
            result,
            Err(error) if error.code == REQUIRED_SUBSCRIPTION_UNAVAILABLE
        ));
        assert!(rolled_back.load(Ordering::Acquire));
    }

    #[test]
    fn concurrent_detach_is_serialized_after_a_rejected_leased_apply() {
        let (service_stream, peer_stream) =
            UnixStream::pair().expect("fixture socket pair should open");
        let subscription_id = Uuid::new_v4();
        let binding_id = Uuid::new_v4();
        let subscriptions = Arc::new(RequiredSubscriptions::default());
        subscriptions
            .install(subscription_id, &service_stream)
            .expect("fixture subscription should install");
        let active = Arc::new(AtomicBool::new(true));
        let backend = Arc::new(DetachRaceBackend {
            active: Arc::clone(&active),
        });
        let (apply_entered, entered) = mpsc::channel();
        let (release_apply, released) = mpsc::channel();
        let apply_subscriptions = Arc::clone(&subscriptions);
        let apply_active = Arc::clone(&active);
        let apply = thread::spawn(move || {
            apply_subscriptions.with_current(
                subscription_id,
                || {
                    apply_entered
                        .send(())
                        .expect("fixture receiver should remain");
                    released.recv().expect("apply should be released");
                    apply_active.store(true, Ordering::Release);
                    Ok(((), false))
                },
                |(_, created)| {
                    if *created {
                        apply_active.store(false, Ordering::Release);
                    }
                    Ok(())
                },
            )
        });
        entered.recv().expect("apply should enter");

        let detach_backend = Arc::clone(&backend);
        let detach_subscriptions = Arc::clone(&subscriptions);
        let (detach_started, started) = mpsc::channel();
        let (detach_completed, detached) = mpsc::channel();
        let detach = thread::spawn(move || {
            detach_started
                .send(())
                .expect("fixture receiver should remain");
            let result = dispatch(
                &*detach_backend,
                &detach_subscriptions,
                Command::DetachAgent { binding_id },
            );
            detach_completed
                .send(())
                .expect("fixture receiver should remain");
            result
        });
        started.recv().expect("detach should start");
        let detached_before_apply_finished =
            detached.recv_timeout(Duration::from_millis(100)).is_ok();
        drop(peer_stream);
        release_apply
            .send(())
            .expect("apply should still be running");

        let apply_result = apply.join().expect("apply should stop");
        let detach_result = detach.join().expect("detach should stop");
        assert!(!detached_before_apply_finished);
        assert!(matches!(
            apply_result,
            Err(error) if error.code == REQUIRED_SUBSCRIPTION_UNAVAILABLE
        ));
        assert_eq!(detach_result, Ok(ResponseBody::Detached));
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn unleased_credential_apply_is_rejected() {
        let backend = crate::MockBackend::new();
        let subscriptions = RequiredSubscriptions::default();

        let result = dispatch(
            &backend,
            &subscriptions,
            Command::ApplyCredentialPolicy(credential_policy()),
        );

        assert!(matches!(
            result,
            Err(error) if error.code == "required_subscription_unavailable"
        ));
    }

    #[test]
    fn unleased_replacement_is_rejected() {
        let backend = crate::MockBackend::new();
        let subscriptions = RequiredSubscriptions::default();

        let result = dispatch(
            &backend,
            &subscriptions,
            Command::ReplacePolicy(replacement_policy()),
        );

        assert!(matches!(
            result,
            Err(error) if error.code == REQUIRED_SUBSCRIPTION_UNAVAILABLE
        ));
    }

    #[test]
    fn replacement_rollback_accepts_a_restored_live_process_identity() {
        let backend = crate::MockBackend::new();
        let source_request = replacement_policy().expected.request;
        let source = backend
            .apply(source_request.clone())
            .expect("source policy should apply");
        let mut target_request = source_request;
        target_request.binding_id = Uuid::new_v4();
        target_request.policy_id = "fixture-enforce".into();
        target_request.root_pid = 77;
        target_request.process_start_time = 123;
        let replacement = ReplacePolicy {
            expected: source,
            source: ReplacementSource::Generic,
            replacement: ReplacementPolicy::Generic(target_request),
        };
        let outcome = backend
            .replace(replacement.clone())
            .expect("replacement should apply");

        rollback_replacement(&backend, &replacement, &outcome)
            .expect("rollback should accept the restored live process identity");

        let restored = backend
            .bindings()
            .expect("bindings should load")
            .into_iter()
            .find(|binding| binding.request.binding_id == replacement.expected.request.binding_id)
            .expect("source policy should be restored");
        assert_eq!(restored.request.root_pid, 77);
        assert_eq!(restored.request.process_start_time, 123);
    }
}
