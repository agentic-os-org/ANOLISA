use std::io::Write;
use std::sync::Arc;

use asc_action_runtime::{Finalizer, SecurityEventSink};
use asc_daemon_core::{
    PeerCredentials, PolicyAdministration, Principal, PrincipalPolicy, PrincipalRole,
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
    skill_guard: Option<crate::skill_guard::SkillGuardHandler>,
    principal_policy: Arc<dyn PrincipalPolicy>,
}

impl DaemonDispatcher {
    /// Composes PAP dispatch with trusted server authorization policy.
    ///
    /// The role is process-owned configuration. It is never decoded from the
    /// request or inferred from caller-supplied attribution.
    pub fn new(
        application: impl PolicyAdministration + 'static,
        principal_policy: Arc<dyn PrincipalPolicy>,
    ) -> Self {
        Self::new_with_finalizer(application, principal_policy, default_finalizer())
    }

    /// Composes dispatch with an explicitly configured action finalizer.
    pub fn new_with_finalizer(
        application: impl PolicyAdministration + 'static,
        principal_policy: Arc<dyn PrincipalPolicy>,
        finalizer: Finalizer,
    ) -> Self {
        Self {
            pap: PapHandler::new(application),
            code_scan: CodeScanHandler::new(finalizer),
            skill_guard: None,
            principal_policy,
        }
    }

    /// Adds `SkillGuard` using the same service and public finalizer as other entrypoints.
    #[must_use]
    pub fn with_skill_guard(
        mut self,
        service: Arc<asc_capability_skill_guard::SkillGuardService>,
        finalizer: Finalizer,
    ) -> Self {
        self.skill_guard = Some(crate::skill_guard::SkillGuardHandler::new(
            service, finalizer,
        ));
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
            MethodId::Pap(method) => {
                self.pap
                    .handle(request_id, &principal, method, request.params)
            }
            MethodId::Action(method) => match method {
                method::ActionMethod::SkillGuard => match &self.skill_guard {
                    Some(handler) => handler.handle(request_id, peer, control, request.params),
                    None => DaemonResponse::error(
                        request_id,
                        error_code::INTERNAL,
                        "SkillGuard is not configured",
                    ),
                },
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
    fn dispatch_timeout(&self, payload: &[u8]) -> Option<std::time::Duration> {
        let request: DaemonRequest = serde_json::from_slice(payload).ok()?;
        if request.method != method::ACTION_SKILL_GUARD || self.skill_guard.is_none() {
            return None;
        }
        let millis = request
            .params
            .get("timeoutMs")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(60_000)
            .clamp(1, 120_000);
        Some(std::time::Duration::from_millis(millis))
    }

    fn dispatch(
        &self,
        request: DispatchRequest,
        response: &mut dyn Write,
    ) -> Result<ResponseDisposition, DispatchError> {
        let request_id = new_request_id();
        if request.control.is_cancelled() {
            return write_response(
                response,
                &DaemonResponse::<serde_json::Value>::error(
                    request_id,
                    error_code::DEADLINE_EXCEEDED,
                    "request dispatch deadline expired",
                ),
            );
        }

        let peer = PeerCredentials::new(request.peer.uid(), request.peer.gid(), request.peer.pid());
        let Ok(decoded) = serde_json::from_slice::<DaemonRequest>(&request.payload) else {
            return write_response(
                response,
                &DaemonResponse::<serde_json::Value>::error(
                    request_id,
                    error_code::INVALID_REQUEST,
                    "request envelope is invalid",
                ),
            );
        };
        write_response(
            response,
            &self.handle_with_control(request_id, peer, &request.control, decoded),
        )
    }
}

struct NoopEventSink;

impl SecurityEventSink for NoopEventSink {
    fn write(&self, _: &asc_security_events::SecurityEvent) {}
}

fn default_finalizer() -> Finalizer {
    Finalizer::new(Arc::new(NoopEventSink))
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
