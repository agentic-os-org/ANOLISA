//! Binds the secret-gateway runtime to the daemon's gateway port.

use std::path::Path;
use std::sync::Arc;

use asc_daemon_core::{
    GatewayAdministration, GatewayFailure, GatewayFailureKind, GatewayKeyRequest, PeerCredentials,
};
use asc_gateway_runtime::{
    GatewayConfig, GatewayError, GatewayErrorKind, GatewayService, KeyRequest, SystemHost,
};
use serde::Serialize;
use serde_json::Value;

/// Gateway port backed by [`GatewayService`].
pub struct GatewayAdapter(Arc<GatewayService>);

impl GatewayAdapter {
    /// Loads production state without host mutation on success.
    ///
    /// # Errors
    /// Rejects invalid state after attempting to stop and withdraw the old data plane.
    pub fn system(
        data_dir: &Path,
        repository: Arc<dyn asc_pap::PapRepository>,
    ) -> Result<Self, GatewayError> {
        GatewayService::new(
            GatewayConfig::system(data_dir),
            Box::new(SystemHost),
            repository,
        )
        .map(|service| Self(Arc::new(service)))
    }

    /// Shares the Scope execution backend with PAP and Binding reconciliation.
    pub fn service(&self) -> Arc<GatewayService> {
        Arc::clone(&self.0)
    }
}

impl GatewayAdministration for GatewayAdapter {
    fn enable(&self) -> Result<Value, GatewayFailure> {
        project(self.0.enable().map(|()| serde_json::json!({})))
    }

    fn disable(&self) -> Result<Value, GatewayFailure> {
        project(self.0.disable().map(|()| serde_json::json!({})))
    }

    fn status(&self, peer: PeerCredentials) -> Result<Value, GatewayFailure> {
        project(Ok(self.0.status(peer.uid())))
    }

    fn add_key(
        &self,
        peer: PeerCredentials,
        request: GatewayKeyRequest,
    ) -> Result<Value, GatewayFailure> {
        project(self.0.add_key(&KeyRequest {
            credential: request.credential,
            secret_file: request.secret_file,
            caller_uid: peer.uid(),
        }))
    }

    fn remove_key(&self, credential: &str) -> Result<Value, GatewayFailure> {
        project(self.0.remove_key(credential))
    }

    fn list_keys(&self) -> Result<Value, GatewayFailure> {
        project(self.0.list_keys())
    }

    fn credentials(&self, peer: PeerCredentials, scope_id: &str) -> Result<Value, GatewayFailure> {
        project(self.0.credentials(peer.uid(), scope_id))
    }
}

fn project<T: Serialize>(result: Result<T, GatewayError>) -> Result<Value, GatewayFailure> {
    let value = result.map_err(|error| GatewayFailure {
        kind: match error.kind() {
            GatewayErrorKind::InvalidArgument => GatewayFailureKind::InvalidArgument,
            GatewayErrorKind::NotFound => GatewayFailureKind::NotFound,
            GatewayErrorKind::Conflict => GatewayFailureKind::Conflict,
            GatewayErrorKind::PermissionDenied => GatewayFailureKind::PermissionDenied,
            GatewayErrorKind::ResourceExhausted => GatewayFailureKind::ResourceExhausted,
            GatewayErrorKind::Unavailable => GatewayFailureKind::Unavailable,
            GatewayErrorKind::Internal => GatewayFailureKind::Internal,
        },
        message: error.to_string(),
    })?;
    serde_json::to_value(value).map_err(|_| GatewayFailure {
        kind: GatewayFailureKind::Internal,
        message: "cannot encode the gateway result".into(),
    })
}
