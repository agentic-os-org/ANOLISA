//! Binds the secret-gateway runtime to the daemon's gateway port.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use asc_daemon_core::{GatewayAdministration, GatewayFailure, GatewayFailureKind, PeerCredentials};
use asc_gateway_runtime::{
    GatewayConfig, GatewayError, GatewayErrorKind, GatewayService, SystemHost,
};
use serde::Serialize;
use serde_json::Value;

/// Disabling Gateway management cannot abandon an existing Gateway responsibility.
#[derive(Debug, thiserror::Error)]
pub enum GatewayOptOutError {
    /// Persistent or runtime artifacts require the managed startup path.
    #[error("cannot start without --enable-gateway while '{}' exists; restart with --enable-gateway", .0.display())]
    ExistingState(PathBuf),
    /// Unreadable evidence must not be treated as an empty installation.
    #[error("cannot inspect Gateway path '{}': {source}", path.display())]
    InspectState {
        /// Path whose metadata could not be read.
        path: PathBuf,
        /// Filesystem failure encountered during inspection.
        source: std::io::Error,
    },
    /// Canonical credential assignments still require Gateway management.
    #[error(
        "cannot start without --enable-gateway while credential Scope '{0}' exists; restart with --enable-gateway"
    )]
    CredentialScope(asc_foundation_types::ResourceId),
    /// Canonical state could not be checked safely.
    #[error("cannot inspect credential Scopes while Gateway management is disabled: {0}")]
    Repository(#[from] asc_pap::PapError),
}

/// Checks local Gateway responsibilities without invoking systemd or nftables.
///
/// The trusted launcher must separately guarantee that no external proxy or routing
/// remains: absent local files cannot prove host data-plane absence.
/// # Errors
/// Rejects existing artifacts, credential Scopes and unavailable state evidence.
pub fn validate_gateway_opt_out(
    config: &GatewayConfig,
    repository: &dyn asc_pap::PapRepository,
) -> Result<(), GatewayOptOutError> {
    for path in [&config.state_dir, &config.store_dir, &config.runtime_dir] {
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(GatewayOptOutError::InspectState {
                    path: path.clone(),
                    source,
                });
            }
            Ok(_) => return Err(GatewayOptOutError::ExistingState(path.clone())),
        }
    }
    let mut after = None;
    loop {
        let scopes = repository.scan_scopes(after.as_ref(), 128)?;
        if scopes.is_empty() {
            return Ok(());
        }
        for scope in &scopes {
            if scope.uses_credentials() {
                return Err(GatewayOptOutError::CredentialScope(scope.scope_id.clone()));
            }
        }
        after = scopes.last().map(|scope| scope.scope_id.clone());
    }
}

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

    fn remove_key(&self, pairid: &str) -> Result<Value, GatewayFailure> {
        project(
            self.0
                .remove_key(pairid)
                .map(|result| serde_json::json!({"pairid": result.credential})),
        )
    }

    fn list_keys(&self) -> Result<Value, GatewayFailure> {
        project(
            self.0
                .list_keys()
                .map(|result| serde_json::json!({"pairids": result.credentials})),
        )
    }

    fn ca(&self, _peer: PeerCredentials) -> Result<Value, GatewayFailure> {
        project(self.0.ca())
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

#[cfg(test)]
mod tests {
    use super::*;
    use asc_pap::PapRepository;
    use asc_policy_repository_sqlite::SqlitePolicyRepository;
    use asc_policy_types::authoring::PolicySource;
    use asc_policy_types::cedar::CedarPolicySource;
    use asc_policy_types::identifiers::{ResourceId, Revision};
    use asc_policy_types::policy::{CredentialBinding, PreparedPolicy};
    use asc_policy_types::scope::{PreparedScope, ProcessMatcher, ScopeSelector, ScopeStatus};

    #[test]
    fn gateway_opt_out_accepts_fresh_state_and_rejects_existing_state() {
        let root = tempfile::tempdir().unwrap();
        let mut config = GatewayConfig::system(root.path());
        config.runtime_dir = root.path().join("runtime");
        let repository = SqlitePolicyRepository::temporary().unwrap();
        validate_gateway_opt_out(&config, &repository).unwrap();
        for path in [&config.state_dir, &config.store_dir, &config.runtime_dir] {
            std::fs::create_dir(path).unwrap();
            assert!(matches!(
                validate_gateway_opt_out(&config, &repository),
                Err(GatewayOptOutError::ExistingState(found)) if found == *path
            ));
            std::fs::remove_dir(path).unwrap();
        }
        let file = root.path().join("not-a-directory");
        std::fs::write(&file, b"").unwrap();
        config.runtime_dir = file.join("runtime");
        assert!(matches!(
            validate_gateway_opt_out(&config, &repository),
            Err(GatewayOptOutError::InspectState { path, source })
                if path == config.runtime_dir && source.kind() == std::io::ErrorKind::NotADirectory
        ));
    }

    #[test]
    fn file_scopes_allow_opt_out_but_credential_scopes_refuse_it() {
        let root = tempfile::tempdir().unwrap();
        let mut config = GatewayConfig::system(root.path());
        config.runtime_dir = root.path().join("runtime");
        for (source, uses_credentials) in [
            (
                "forbid(principal, action == SecCore::Action::\"file.write\", resource);",
                false,
            ),
            (
                include_str!("../../../crates/asc-gateway-plan/policies/github.cedar"),
                true,
            ),
        ] {
            let repository = SqlitePolicyRepository::temporary().unwrap();
            let policy = PreparedPolicy {
                deployment: None,
                policy_id: ResourceId::new("gateway-policy").unwrap(),
                policy_name: "gateway policy".into(),
                revision: Revision::new(1).unwrap(),
                template: PolicySource::Cedar(CedarPolicySource {
                    schema: asc_policy_cedar::schema::embedded_reference(),
                    text: source.into(),
                }),
                credential_bindings: source
                    .lines()
                    .filter_map(|line| line.strip_prefix("@id(\"")?.strip_suffix("\")"))
                    .map(|rule_id| CredentialBinding {
                        rule_id: rule_id.into(),
                        pair_id: "fixture-pair".into(),
                    })
                    .collect(),
            };
            repository.put_policy(&policy).unwrap();
            let scope = repository
                .put_scope(&PreparedScope {
                    scope_id: ResourceId::new("gateway-scope").unwrap(),
                    selector: ScopeSelector::Process {
                        matcher: ProcessMatcher::Name {
                            process_name: "worker".into(),
                        },
                    },
                    policy_snapshots: vec![policy],
                    owner_uid: Some(1000),
                    status: ScopeStatus::Active,
                })
                .unwrap();
            assert_eq!(scope.uses_credentials(), uses_credentials);
            let outcome = validate_gateway_opt_out(&config, &repository);
            if uses_credentials {
                assert!(matches!(
                    outcome,
                    Err(GatewayOptOutError::CredentialScope(id)) if id == scope.scope_id
                ));
            } else {
                outcome.unwrap();
            }
        }
    }
}
