//! Reads current PAP policies at each authorization boundary.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::{Arc, Mutex};

use asc_daemon_core::{AuthorizationError, AuthorizationEvaluator};
use asc_foundation_types::ResourceId;
use asc_pap::{PapRepository, PapService};
use asc_pdp_runtime::{PdpRuntime, PolicySnapshot};
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::policy::PreparedPolicy;
use asc_policy_types::{AuthorizationRequest, Decision, OsPermissionSet, Validate};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use serde::{Deserialize, Serialize};

const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// Deployment settings select PAP objects without copying policy content.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationConfig {
    /// Mandatory PAP policy defining the authorization ceiling.
    pub baseline_policy: ResourceId,
    /// Optional PAP policy evaluated as an additional restriction.
    #[serde(default)]
    pub task_policy: Option<ResourceId>,
    /// Host-specific upper bound on requested OS permissions.
    pub environment_permissions: OsPermissionSet,
    /// Deadline to accept a decision, excluding subsequent approval waiting.
    pub ttl_seconds: u64,
}

impl AuthorizationConfig {
    fn validate(&self) -> Result<(), PublishedPolicyError> {
        if self.ttl_seconds == 0 || self.ttl_seconds > 3600 {
            return Err(PublishedPolicyError::InvalidConfig(
                "ttl_seconds must be in 1..=3600".into(),
            ));
        }
        if self.task_policy.as_ref() == Some(&self.baseline_policy) {
            return Err(PublishedPolicyError::InvalidConfig(
                "baseline_policy and task_policy must be different".into(),
            ));
        }
        self.environment_permissions.validate()?;
        Ok(())
    }

    fn policy_ids(&self) -> Vec<ResourceId> {
        std::iter::once(&self.baseline_policy)
            .chain(self.task_policy.iter())
            .cloned()
            .collect()
    }
}

/// Compiled cache for the current PAP revisions, including their continuations.
/// The cache never substitutes for a successful current-policy read.
pub struct PapAuthorizationPolicy<R> {
    config: AuthorizationConfig,
    pap: PapService<R>,
    active: Mutex<Option<Arc<PdpRuntime>>>,
}

impl<R: PapRepository> PapAuthorizationPolicy<R> {
    /// Loads root-owned deployment settings; selected policies may be created later through PAP.
    ///
    /// # Errors
    /// Rejects untrusted paths, malformed settings and invalid OS ceilings.
    pub fn load(path: &Path, pap: PapService<R>) -> Result<Self, PublishedPolicyError> {
        let path = fs::canonicalize(path)?;
        validate_parent_directories(&path)?;
        let config: AuthorizationConfig = serde_json::from_slice(&read_config(&path, 0)?)?;
        config.validate()?;
        Ok(Self {
            config,
            pap,
            active: Mutex::new(None),
        })
    }

    fn current_runtime(&self) -> Result<Arc<PdpRuntime>, PublishedPolicyError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| PublishedPolicyError::State)?;
        let result = (|| {
            // The single PAP read pins baseline and task to one consistent database view.
            let policies = self.pap.current_policies(&self.config.policy_ids())?;
            let versions: Vec<_> = policies
                .iter()
                .map(|policy| (&policy.policy_id, policy.revision))
                .collect();
            let bytes = serde_json::to_vec(&(&self.config, versions))?;
            let version = URL_SAFE_NO_PAD.encode(digest(&SHA256, &bytes).as_ref());
            if let Some(runtime) = active
                .as_ref()
                .filter(|cached| cached.policy_version() == version)
            {
                return Ok(Arc::clone(runtime));
            }
            let mut sources = policies.iter().map(cedar_source);
            let baseline = sources
                .next()
                .ok_or(PublishedPolicyError::MissingPolicy)??;
            let task = sources.next().transpose()?;
            let runtime = Arc::new(PdpRuntime::from_snapshot(PolicySnapshot {
                version,
                baseline,
                task,
                environment_permissions: self.config.environment_permissions.clone(),
                ttl_seconds: self.config.ttl_seconds,
            })?);
            *active = Some(Arc::clone(&runtime));
            Ok(runtime)
        })();
        if result.is_err() {
            // A removed or unsupported current policy must never fall back to an earlier grant.
            *active = None;
        }
        result
    }
}

impl<R: PapRepository> AuthorizationEvaluator for PapAuthorizationPolicy<R> {
    fn evaluate(
        &self,
        request: &AuthorizationRequest,
        now: u64,
    ) -> Result<Decision, AuthorizationError> {
        let runtime = self.current_runtime().map_err(|error| {
            tracing::warn!(target: "asc_process_diagnostic", reason = %error,
                "Current PAP authorization policy is unavailable");
            AuthorizationError::Unavailable
        })?;
        runtime.evaluate(request, now).map_err(|error| {
            tracing::warn!(target: "asc_process_diagnostic", reason = %error, "Authorization evaluation failed");
            match error {
                asc_pdp_runtime::PdpError::Validation(_) => AuthorizationError::InvalidArgument,
                _ => AuthorizationError::Internal,
            }
        })
    }
}

fn cedar_source(
    policy: &PreparedPolicy,
) -> Result<asc_policy_types::cedar::CedarPolicySource, PublishedPolicyError> {
    match &policy.template {
        PolicySource::Cedar(source) => Ok(source.clone()),
        PolicySource::SecCore(_) => Err(PublishedPolicyError::UnsupportedSource),
    }
}

fn validate_parent_directories(path: &Path) -> Result<(), PublishedPolicyError> {
    for parent in path.ancestors().skip(1) {
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(PublishedPolicyError::UntrustedPath);
        }
    }
    Ok(())
}

fn read_config(path: &Path, owner: u32) -> Result<Vec<u8>, PublishedPolicyError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != owner || metadata.mode() & 0o022 != 0 {
        return Err(PublishedPolicyError::UntrustedPath);
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(PublishedPolicyError::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() > usize::try_from(MAX_CONFIG_BYTES).unwrap_or(usize::MAX) {
        return Err(PublishedPolicyError::TooLarge);
    }
    Ok(bytes)
}

/// Admission failures for authorization settings and current PAP sources.
#[derive(Debug, thiserror::Error)]
pub enum PublishedPolicyError {
    /// The selected file or one of its parent directories is unsafe.
    #[error(
        "authorization config must be a root-owned regular file in root-owned directories without group or other write access"
    )]
    UntrustedPath,
    /// Settings exceed the bounded configuration size.
    #[error("authorization config exceeds 4 MiB")]
    TooLarge,
    /// The configuration could not be read.
    #[error("cannot read authorization config: {0}")]
    Io(#[from] std::io::Error),
    /// Settings do not match the deployment format.
    #[error("invalid authorization config JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A deployment setting is outside its supported range.
    #[error("invalid authorization config: {0}")]
    InvalidConfig(String),
    /// Requested OS ceilings are malformed.
    #[error("invalid authorization permissions: {0}")]
    Validation(#[from] asc_policy_types::ValidationError),
    /// The current PAP selection could not be loaded.
    #[error("cannot read current PAP policies: {0}")]
    Pap(#[from] asc_pap::PapError),
    /// An incomplete repository result cannot authorize an operation.
    #[error("PAP did not return the selected baseline policy")]
    MissingPolicy,
    /// This executor requires Cedar sources rather than legacy backend rules.
    #[error("sandbox authorization requires Cedar policy sources")]
    UnsupportedSource,
    /// Cedar or the OS policy compiler rejected the current policies.
    #[error("invalid authorization policy: {0}")]
    Compile(#[from] asc_pdp_runtime::PdpError),
    /// A failed cache lock cannot safely retain continuation state.
    #[error("authorization policy cache unavailable")]
    State,
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_policy_repository_sqlite::SqlitePolicyRepository;
    use std::fs::File;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn source(text: &str) -> PolicySource {
        PolicySource::Cedar(
            serde_json::from_value(serde_json::json!({
                "schema":{"schemaId":"seccore.system","revision":2},"text":text
            }))
            .unwrap(),
        )
    }

    fn fixture() -> (
        PapService<SqlitePolicyRepository>,
        PapAuthorizationPolicy<SqlitePolicyRepository>,
    ) {
        let pap = PapService::new(Arc::new(SqlitePolicyRepository::temporary().unwrap()));
        let baseline = pap
            .create_policy(
                "baseline",
                &source(
                    "permit(principal, action == SecCore::Action::\"process.spawn\", resource);",
                ),
            )
            .unwrap();
        let config: AuthorizationConfig = serde_json::from_value(serde_json::json!({
            "baseline_policy":baseline.policy_id,
            "environment_permissions":{"version":1,"filesystem":{"default":"deny","rules":[]},
                "execution":{"deny_paths":[]},"network":{"mode":"off"}},
            "ttl_seconds":60
        }))
        .unwrap();
        let authorization = PapAuthorizationPolicy {
            config,
            pap: pap.clone(),
            active: Mutex::new(None),
        };
        (pap, authorization)
    }

    #[test]
    fn pap_updates_replace_cache_and_deletion_never_falls_back() {
        let (pap, authorization) = fixture();
        let first = authorization.current_runtime().unwrap();
        assert!(Arc::ptr_eq(
            &first,
            &authorization.current_runtime().unwrap()
        ));
        let updated = pap
            .update_policy(
                &authorization.config.baseline_policy,
                "baseline",
                &source(
                    "forbid(principal, action == SecCore::Action::\"process.spawn\", resource);",
                ),
            )
            .unwrap();
        let second = authorization.current_runtime().unwrap();
        assert_ne!(first.policy_version(), second.policy_version());
        pap.delete_policy_revision(&updated.policy_id, updated.revision)
            .unwrap();
        assert!(authorization.current_runtime().is_err());
        assert!(authorization.active.lock().unwrap().is_none());
    }

    #[test]
    fn unusable_new_revision_never_keeps_an_old_runtime() {
        let (pap, authorization) = fixture();
        authorization.current_runtime().unwrap();
        // Valid Cedar revision 1 is admitted by PAP but unsupported by the runtime PDP.
        let incompatible = PolicySource::Cedar(
            serde_json::from_value(serde_json::json!({
                "schema":{"schemaId":"seccore.system","revision":1},
                "text":"permit(principal, action == SecCore::Action::\"file.read\", resource);"
            }))
            .unwrap(),
        );
        pap.update_policy(
            &authorization.config.baseline_policy,
            "baseline",
            &incompatible,
        )
        .unwrap();
        assert!(authorization.current_runtime().is_err());
        assert!(authorization.active.lock().unwrap().is_none());
    }

    #[test]
    fn missing_task_does_not_drop_the_restriction() {
        let (_, mut authorization) = fixture();
        authorization.config.task_policy = Some(ResourceId::new("missing-task").unwrap());
        assert!(authorization.current_runtime().is_err());
        assert!(authorization.active.lock().unwrap().is_none());
    }

    #[test]
    fn config_rejects_policy_content_invalid_ttl_and_duplicate_selection() {
        let (_, authorization) = fixture();
        let mut value = serde_json::to_value(&authorization.config).unwrap();
        value["baseline"] = serde_json::json!({"text":"permit(principal, action, resource);"});
        assert!(serde_json::from_value::<AuthorizationConfig>(value).is_err());
        let mut config = authorization.config;
        config.ttl_seconds = 0;
        assert!(config.validate().is_err());
        config.ttl_seconds = 60;
        config.task_policy = Some(config.baseline_policy.clone());
        assert!(config.validate().is_err());
    }

    #[test]
    fn config_file_rejects_writable_symlink_and_oversized_sources() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let owner = fs::metadata(&path).unwrap().uid();
        assert_eq!(read_config(&path, owner).unwrap(), b"{}");
        assert!(read_config(&path, owner.wrapping_add(1)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            read_config(&path, owner),
            Err(PublishedPolicyError::UntrustedPath)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_config(&link, owner).is_err());
        File::create(&path)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES + 1)
            .unwrap();
        assert!(matches!(
            read_config(&path, owner),
            Err(PublishedPolicyError::TooLarge)
        ));
    }

    #[test]
    fn mutable_parent_cannot_publish_trusted_settings() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            validate_parent_directories(&directory.path().join("config.json")),
            Err(PublishedPolicyError::UntrustedPath)
        ));
    }
}
