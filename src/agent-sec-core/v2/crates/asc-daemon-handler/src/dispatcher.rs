use std::io::Write;
use std::sync::Arc;

use asc_daemon_core::{
    ActionService, PeerCredentials, PolicyAdministration, Principal, PrincipalPolicy, PrincipalRole,
};
use asc_daemon_protocol::method::{self, AccessPolicy, MethodId};
use asc_daemon_protocol::{DaemonRequest, DaemonResponse, RequestId, error_code};
use asc_daemon_service::{DispatchError, DispatchRequest, RequestDispatcher, ResponseDisposition};

use crate::action::CodeScanHandler;
use crate::pap::PapHandler;

/// Protocol router composed over daemon application use cases.
pub struct DaemonDispatcher {
    pap: PapHandler,
    code_scan: CodeScanHandler,
    principal_policy: Arc<dyn PrincipalPolicy>,
    observability: Option<asc_daemon_core::ObservabilityService>,
}

impl DaemonDispatcher {
    /// Composes PAP dispatch with trusted server authorization policy.
    ///
    /// The role is process-owned configuration. It is never decoded from the
    /// request or inferred from caller-supplied attribution.
    pub fn new(
        application: impl PolicyAdministration + 'static,
        principal_policy: Arc<dyn PrincipalPolicy>,
        actions: Arc<ActionService>,
    ) -> Self {
        Self {
            pap: PapHandler::new(application),
            code_scan: CodeScanHandler::new(actions),
            principal_policy,
            observability: None,
        }
    }

    /// Installs the explicitly configured observability ingestion application.
    #[must_use]
    pub fn with_observability(mut self, service: asc_daemon_core::ObservabilityService) -> Self {
        self.observability = Some(service);
        self
    }

    /// Handles one decoded request using transport-authenticated peer identity.
    pub fn handle(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        request: DaemonRequest,
    ) -> DaemonResponse {
        self.handle_with_control(
            request_id,
            peer,
            &asc_daemon_service::DispatchControl::new(std::time::Instant::now()),
            request,
        )
    }

    /// Handles one request with its transport-owned control state.
    pub fn handle_with_control(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        control: &asc_daemon_service::DispatchControl,
        request: DaemonRequest,
    ) -> DaemonResponse {
        let parent = request_parent(&request);
        let method = request.method.clone();
        request_scope(parent, &method, || {
            let result = self.handle_inner(request_id, peer, control, request);
            record_result(&result);
            result
        })
    }

    fn handle_inner(
        &self,
        request_id: RequestId,
        peer: PeerCredentials,
        control: &asc_daemon_service::DispatchControl,
        request: DaemonRequest,
    ) -> DaemonResponse {
        let Some(method_id) = method::resolve(&request.method) else {
            return DaemonResponse::error(
                request_id,
                error_code::UNKNOWN_METHOD,
                "daemon method is not implemented",
            );
        };

        let role = self.principal_policy.role_for(peer);
        let principal = Principal::from_authenticated_peer(peer, role);
        if !is_authorized(&principal, method_id.metadata().access) {
            return DaemonResponse::error(
                request_id,
                error_code::PERMISSION_DENIED,
                "principal is not authorized to administer policy",
            );
        }
        match method_id {
            MethodId::ObservabilityRecord => crate::observability::handle(
                request_id,
                control,
                self.observability.as_ref(),
                request.params,
            ),
            MethodId::Pap(method) => {
                self.pap
                    .handle(request_id, &principal, method, request.params)
            }
            MethodId::Action(method) => match method {
                method::ActionMethod::CodeScan => {
                    self.code_scan
                        .handle(request_id, peer, control, request.params)
                }
            },
        }
    }
}

fn is_authorized(principal: &Principal, access: AccessPolicy) -> bool {
    match access {
        AccessPolicy::PolicyAdministrator => principal.role() == PrincipalRole::PolicyAdministrator,
        // Any kernel-authenticated peer is a local user; the transport rejects
        // unauthenticated peers before dispatch, so reaching here already means
        // the peer is authenticated.
        AccessPolicy::LocalUser => true,
    }
}

impl RequestDispatcher for DaemonDispatcher {
    fn dispatch(
        &self,
        request: DispatchRequest,
        response: &mut dyn Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        let request_id = new_request_id();
        if request.control.is_cancelled() {
            return asc_observability::rejection_scope("deadline_exceeded", || {
                write_response(
                    response,
                    &DaemonResponse::<serde_json::Value>::error(
                        request_id,
                        error_code::DEADLINE_EXCEEDED,
                        "request dispatch deadline expired",
                    ),
                )
            });
        }

        let peer = PeerCredentials::new(request.peer.uid(), request.peer.gid(), request.peer.pid());
        let decoded = serde_json::from_slice::<DaemonRequest>(&request.payload);
        let Some(decoded) = decoded.ok().filter(|_| {
            // After strict decoding, a small frame fits both independent budgets.
            // Transport strips LF, so reserve one byte even on this fast path.
            request.payload.len() < asc_daemon_protocol::CONTEXT_FRAME_BYTES
                || asc_daemon_protocol::request_fits_budget(&request.payload).unwrap_or(false)
        }) else {
            return asc_observability::rejection_scope("invalid_request", || {
                write_response(
                    response,
                    &DaemonResponse::<serde_json::Value>::error(
                        request_id,
                        error_code::INVALID_REQUEST,
                        "request envelope is invalid",
                    ),
                )
            });
        };
        let parent = request_parent(&decoded);
        let method = decoded.method.clone();
        request_scope(parent, &method, || {
            let result = self.handle_inner(request_id, peer, &request.control, decoded);
            record_result(&result);
            if request.control.is_cancelled() {
                tracing::Span::current().record("cancel_requested", true);
                asc_observability::diagnostic("request_completed_after_cancellation");
            }
            let success = matches!(&result, DaemonResponse::Success(_));
            let encoded = write_response(response, &result);
            if encoded.is_err() {
                asc_observability::mark_error("response_encode_failed");
            }
            asc_observability::diagnostic(if encoded.is_err() {
                "response_encode_failed"
            } else if success {
                "request_completed"
            } else {
                "request_rejected"
            });
            encoded
        })
    }
}

pub(crate) fn new_request_id() -> RequestId {
    RequestId::new(uuid::Uuid::new_v4().to_string())
        .expect("UUID request identities are always non-empty")
}

pub(crate) fn write_response<T: serde::Serialize>(
    response: &mut dyn Write,
    value: &DaemonResponse<T>,
) -> Result<ResponseDisposition, DispatchError> {
    serde_json::to_writer(response, value).map_err(|_| DispatchError)?;
    Ok(ResponseDisposition::Send)
}

fn request_parent(request: &DaemonRequest) -> asc_observability::Context {
    let parent = request
        .trace_context
        .as_ref()
        .map_or_else(asc_observability::Context::new, |carrier| {
            asc_observability::extract_parent(&carrier.headers())
        });
    if let Some(labels) = &request.compatibility {
        parent.with_value(asc_observability::CompatibilityCorrelation {
            trace_id: labels
                .trace_id
                .as_deref()
                .and_then(asc_observability::normalize),
            invocation_label: labels
                .invocation_label
                .as_deref()
                .and_then(asc_observability::normalize),
        })
    } else {
        parent
    }
}

fn request_scope<T>(
    parent: asc_observability::Context,
    method_name: &str,
    work: impl FnOnce() -> T,
) -> T {
    let safe_method = if method::resolve(method_name).is_some() {
        method_name
    } else {
        "unknown"
    };
    let _parent = parent.clone().attach();
    let span = asc_observability::parent_span(
        tracing::info_span!(parent: None, "daemon.request", otel.kind = "server", rpc.method = safe_method,
            rpc.request_id = tracing::field::Empty, cancel_requested = false),
        parent,
    );
    span.in_scope(|| {
        let _context = asc_observability::request_context().attach();
        asc_observability::report_propagation_issues();
        let _completion = RequestCompletion;
        asc_observability::diagnostic("request_started");
        work()
    })
}
struct RequestCompletion;
impl Drop for RequestCompletion {
    fn drop(&mut self) {
        if std::thread::panicking() {
            asc_observability::mark_error("request_panicked");
            asc_observability::diagnostic("request_panicked");
        }
    }
}

fn record_result(result: &DaemonResponse) {
    let response_id = result.request_id().as_str();
    if response_id.len() <= 256 {
        tracing::Span::current().record("rpc.request_id", response_id);
    }
    match result {
        DaemonResponse::Success(_) => asc_observability::mark_success(),
        DaemonResponse::Error(response) => {
            let category = match response.error.code.as_str() {
                error_code::INVALID_REQUEST => error_code::INVALID_REQUEST,
                error_code::INVALID_ARGUMENT => error_code::INVALID_ARGUMENT,
                error_code::UNKNOWN_METHOD => error_code::UNKNOWN_METHOD,
                error_code::PERMISSION_DENIED => error_code::PERMISSION_DENIED,
                error_code::NOT_FOUND => error_code::NOT_FOUND,
                error_code::CONFLICT => error_code::CONFLICT,
                error_code::RESOURCE_EXHAUSTED => error_code::RESOURCE_EXHAUSTED,
                error_code::DEADLINE_EXCEEDED => error_code::DEADLINE_EXCEEDED,
                error_code::UNAVAILABLE => error_code::UNAVAILABLE,
                _ => error_code::INTERNAL,
            };
            asc_observability::mark_error(category);
        }
    }
}
