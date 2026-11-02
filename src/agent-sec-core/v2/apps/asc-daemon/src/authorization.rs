//! Reads current PAP policies at each authorization boundary.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};

use asc_daemon_core::{AuthorizationError, AuthorizationEvaluator};
use asc_daemon_protocol::authorization::{
    AuthorizationRequest, Decision, OsPermissionSet, RuleEffect, Validate,
};
use asc_foundation_types::ResourceId;
use asc_pap::{PapRepository, PapService};
use asc_pdp_runtime::{PdpRuntime, PolicySnapshot};
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::policy::PreparedPolicy;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use rustix::fs::{Mode, OFlags, open, openat};
use serde::{Deserialize, Serialize};

const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// Policy selection and permission limits for OS authorization requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationConfig {
    /// Mandatory PAP policy defining the authorization ceiling.
    pub baseline_policy: ResourceId,
    /// Optional PAP policy evaluated as an additional restriction.
    #[serde(default)]
    pub task_policy: Option<ResourceId>,
    /// Upper bound on OS permissions granted to each authorized process tree.
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
        let permissions = &self.environment_permissions;
        permissions.validate()?;
        for path in permissions
            .filesystem
            .rules
            .iter()
            .filter(|r| r.effect == RuleEffect::Deny)
            .map(|r| &r.path)
            .chain(&permissions.execution.deny_paths)
        {
            // Requests use physical paths. An alias here could silently miss its target.
            if !fs::canonicalize(path).is_ok_and(|physical| physical == Path::new(path)) {
                return Err(PublishedPolicyError::InvalidConfig(format!(
                    "deny/noexec path must exist and be canonical: {path}"
                )));
            }
        }
        Ok(())
    }

    fn policy_ids(&self) -> Vec<ResourceId> {
        std::iter::once(&self.baseline_policy)
            .chain(self.task_policy.iter())
            .cloned()
            .collect()
    }
}

/// Compiled cache for the current PAP revisions, reused only while both policy revisions match.
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
        let config: AuthorizationConfig = serde_json::from_slice(&read_config(path)?)?;
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

fn read_config(path: &Path) -> Result<Vec<u8>, PublishedPolicyError> {
    let mut parts = path.components();
    if parts.next() != Some(Component::RootDir) {
        return Err(PublishedPolicyError::UntrustedPath);
    }
    let Some(Component::Normal(name)) = parts.next_back() else {
        return Err(PublishedPolicyError::UntrustedPath);
    };
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut directory = File::from(open("/", flags, Mode::empty()).map_err(std::io::Error::from)?);
    loop {
        let metadata = directory.metadata()?;
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
            return Err(PublishedPolicyError::UntrustedPath);
        }
        match parts.next() {
            Some(Component::Normal(part)) => {
                directory = File::from(
                    openat(&directory, part, flags, Mode::empty()).map_err(std::io::Error::from)?,
                );
            }
            None => return read_config_at(&directory, name, 0),
            _ => return Err(PublishedPolicyError::UntrustedPath),
        }
    }
}

fn read_config_at(
    directory: &File,
    name: &OsStr,
    owner: u32,
) -> Result<Vec<u8>, PublishedPolicyError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file =
        File::from(openat(directory, name, flags, Mode::empty()).map_err(std::io::Error::from)?);
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
    /// A failed cache lock cannot safely reuse compiled policies.
    #[error("authorization policy cache unavailable")]
    State,
}

#[cfg(test)]
mod tests {
    use super::*;
    use asc_policy_repository_sqlite::SqlitePolicyRepository;
    use std::os::unix::fs::{PermissionsExt, symlink};

    fn source(text: &str) -> PolicySource {
        PolicySource::Cedar(
            serde_json::from_value(serde_json::json!({
                "schema":{"schemaId":"seccore.system","revision":1},"text":text
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
        // PAP accepts this glob, but OS rules require exact paths or directory prefixes.
        let incompatible = source(
            r#"forbid(principal, action == SecCore::Action::"file.read", resource)
               when { resource.path like "/work/sec*" };"#,
        );
        pap.update_policy(
            &authorization.config.baseline_policy,
            "baseline",
            &incompatible,
        )
        .unwrap();
        assert!(matches!(
            authorization.current_runtime(),
            Err(PublishedPolicyError::Compile(_))
        ));
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
        let parent = File::open(directory.path()).unwrap();
        let name = OsStr::new("config.json");
        let path = directory.path().join(name);
        fs::write(&path, b"{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let owner = fs::metadata(&path).unwrap().uid();
        assert_eq!(read_config_at(&parent, name, owner).unwrap(), b"{}");
        assert!(read_config_at(&parent, name, owner.wrapping_add(1)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(matches!(
            read_config_at(&parent, name, owner),
            Err(PublishedPolicyError::UntrustedPath)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let link = directory.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_config_at(&parent, OsStr::new("link"), owner).is_err());
        File::create(&path)
            .unwrap()
            .set_len(MAX_CONFIG_BYTES + 1)
            .unwrap();
        assert!(matches!(
            read_config_at(&parent, name, owner),
            Err(PublishedPolicyError::TooLarge)
        ));
    }

    #[test]
    fn load_rejects_a_mutable_path_to_a_root_owned_file() {
        let (pap, _) = fixture();
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let link = directory.path().join("config.json");
        symlink("/etc/passwd", &link).unwrap();
        assert!(matches!(
            PapAuthorizationPolicy::load(&link, pap),
            Err(PublishedPolicyError::UntrustedPath | PublishedPolicyError::Io(_))
        ));
    }

    #[test]
    fn deployment_denials_require_physical_paths() {
        let (_, authorization) = fixture();
        let mut config = authorization.config;
        let tmp = tempfile::tempdir().unwrap();
        let real = fs::canonicalize(tmp.path()).unwrap();
        let program = real.join("program");
        File::create(&program).unwrap();
        let alias = real.join("bin");
        symlink(&real, &alias).unwrap();
        let through_alias = alias.join("program").to_str().unwrap().to_owned();
        let physical = program.to_str().unwrap().to_owned();

        config.environment_permissions.execution.deny_paths = vec![through_alias.clone()];
        assert!(config.validate().is_err());
        config.environment_permissions.execution.deny_paths = vec![physical.clone()];
        assert!(config.validate().is_ok());
        config.environment_permissions.filesystem.rules.push(
            asc_daemon_protocol::authorization::FilesystemRule {
                path: through_alias,
                effect: RuleEffect::Deny,
                rights: vec![asc_daemon_protocol::authorization::FileRight::Read],
            },
        );
        assert!(config.validate().is_err());
        config.environment_permissions.filesystem.rules[0].path = physical;
        assert!(config.validate().is_ok());
    }
}
