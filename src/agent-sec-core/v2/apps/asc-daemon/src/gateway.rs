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
    /// Loads the production gateway state below `data_dir`; the host is not touched.
    ///
    /// # Errors
    /// Returns a corrupt or unreadable gateway state.
    pub fn system(data_dir: &Path) -> Result<Self, GatewayError> {
        GatewayService::new(GatewayConfig::system(data_dir), Box::new(SystemHost))
            .map(|service| Self(Arc::new(service)))
    }

    /// Re-applies an enabled gateway in the background so startup is not delayed.
    pub fn recover_in_background(&self, report: impl Fn(&str) + Send + 'static) {
        let service = Arc::clone(&self.0);
        // Failing to spawn leaves the saved configuration for the next enable.
        let _ = std::thread::Builder::new()
            .name("gateway-recovery".into())
            .spawn(move || {
                if let Err(error) = service.recover() {
                    report(&format!(
                        "asc-daemon: secret gateway recovery failed: {error}"
                    ));
                }
            });
    }
}

impl GatewayAdministration for GatewayAdapter {
    fn enable(&self) -> Result<Value, GatewayFailure> {
        project(self.0.enable())
    }

    fn disable(&self) -> Result<Value, GatewayFailure> {
        project(self.0.disable())
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
            agent: request.agent,
            preset: request.preset,
            secret_file: request.secret_file,
            caller_uid: peer.uid(),
        }))
    }

    fn remove_key(&self, agent: &str, preset: &str) -> Result<Value, GatewayFailure> {
        project(self.0.remove_key(agent, preset))
    }

    fn list_keys(&self) -> Result<Value, GatewayFailure> {
        project(self.0.list_keys())
    }

    fn register(&self, uid: u32, pid: u32, agent: &str) -> Result<Value, GatewayFailure> {
        project(self.0.register(uid, pid, agent))
    }

    fn unregister(&self, agent: &str) -> Result<Value, GatewayFailure> {
        project(self.0.unregister(agent))
    }

    fn credentials(&self, peer: PeerCredentials, agent: &str) -> Result<Value, GatewayFailure> {
        project(self.0.credentials(peer.uid(), agent))
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
