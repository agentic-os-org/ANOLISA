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
use asc_pap::{PapRepository, PapService};
use asc_pdp_runtime::PdpRuntime;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest::{SHA256, digest};
use rustix::fs::{Mode, OFlags, open, openat};
use serde::{Deserialize, Serialize};

const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

/// Permission limits for OS authorization requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorizationConfig {
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
}

/// Compiled cache for the complete current PAP authorization set.
/// The cache never substitutes for a successful current-policy read.
pub struct PapAuthorizationPolicy<R> {
    config: AuthorizationConfig,
    pap: PapService<R>,
    active: Mutex<Option<Arc<PdpRuntime>>>,
}

impl<R: PapRepository> PapAuthorizationPolicy<R> {
    /// Loads root-owned OS settings; policies may be created later through PAP.
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
            // Hold the cache lock across the read and load: an older read cannot publish last.
            let mut policies = self.pap.authorization_policies()?;
            let bytes = serde_json::to_vec(&(&self.config, &policies.version))?;
            let version = URL_SAFE_NO_PAD.encode(digest(&SHA256, &bytes).as_ref());
            if let Some(runtime) = active
                .as_ref()
                .filter(|cached| cached.policy_version() == version)
            {
                return Ok(Arc::clone(runtime));
            }
            policies.version = version;
            let runtime = Arc::new(PdpRuntime::from_policy_set(
                policies,
                self.config.environment_permissions.clone(),
                self.config.ttl_seconds,
            )?);
            *active = Some(Arc::clone(&runtime));
            Ok(runtime)
        })();
        if result.is_err() {
            // A failed current-set read or load must never fall back to an earlier grant.
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
    use asc_policy_types::authoring::PolicySource;
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
        pap
            .create_policy(
                "baseline",
                &source(
                    "permit(principal is SecCore::Subject, action == SecCore::Action::\"process.spawn\", resource);",
                ),
            )
            .unwrap();
        let config: AuthorizationConfig = serde_json::from_value(serde_json::json!({
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
                &pap.list_policies(1, 0).unwrap().items[0].policy_id,
                "baseline",
                &source(
                    "forbid(principal is SecCore::Subject, action == SecCore::Action::\"process.spawn\", resource);",
                ),
            )
            .unwrap();
        let second = authorization.current_runtime().unwrap();
        assert_ne!(first.policy_version(), second.policy_version());
        pap.delete_policy_revision(&updated.policy_id, updated.revision)
            .unwrap();
        let empty = authorization.current_runtime().unwrap();
        assert_ne!(second.policy_version(), empty.policy_version());
        assert_eq!(
            empty.evaluate(&request(), 100).unwrap().verdict,
            asc_daemon_protocol::authorization::DecisionVerdict::Deny
        );
    }

    #[test]
    fn unusable_new_revision_never_keeps_an_old_runtime() {
        let (pap, authorization) = fixture();
        authorization.current_runtime().unwrap();
        // PAP accepts this glob, but OS rules require exact paths or directory prefixes.
        let incompatible = source(
            r#"forbid(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource)
               when { resource.path like "/work/sec*" };"#,
        );
        pap.update_policy(
            &pap.list_policies(1, 0).unwrap().items[0].policy_id,
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
    fn read_failure_clears_cache_and_new_members_replace_it() {
        let (pap, authorization) = fixture();
        let first = authorization.current_runtime().unwrap();
        let added = pap.create_policy("new restriction", &source(
            "forbid(principal is SecCore::Subject, action == SecCore::Action::\"process.spawn\", resource);"
        )).unwrap();
        let second = authorization.current_runtime().unwrap();
        assert_ne!(first.policy_version(), second.policy_version());
        pap.update_policy(
            &added.policy_id,
            "deployment only",
            &source("forbid(principal, action == SecCore::Action::\"process.spawn\", resource);"),
        )
        .unwrap();
        assert_eq!(
            first.policy_version(),
            authorization.current_runtime().unwrap().policy_version()
        );

        SqlitePolicyRepository::before_next_commit_for_test(|transaction| {
            transaction.execute("DROP TABLE policies", []).unwrap();
        });
        pap.update_policy(
            &added.policy_id,
            "corrupted storage",
            &source("forbid(principal, action == SecCore::Action::\"network.use\", resource);"),
        )
        .unwrap();
        assert!(matches!(
            authorization.current_runtime(),
            Err(PublishedPolicyError::Pap(_))
        ));
        assert!(authorization.active.lock().unwrap().is_none());
    }

    #[test]
    fn concurrent_refreshes_cannot_publish_an_old_snapshot_after_the_new_one() {
        let (pap, authorization) = fixture();
        let policy = pap.list_policies(1, 0).unwrap().items.remove(0);
        let old = authorization.current_runtime().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let authorization = &authorization;
                scope.spawn(move || {
                    for _ in 0..4 {
                        authorization.current_runtime().unwrap();
                    }
                });
            }
            pap.update_policy(&policy.policy_id, "deny", &source(
                "forbid(principal is SecCore::Subject, action == SecCore::Action::\"process.spawn\", resource);"
            )).unwrap();
            let latest = authorization.current_runtime().unwrap();
            assert_ne!(latest.policy_version(), old.policy_version());
        });
        let latest = authorization.current_runtime().unwrap();
        assert_ne!(latest.policy_version(), old.policy_version());
        assert!(Arc::ptr_eq(
            &latest,
            authorization.active.lock().unwrap().as_ref().unwrap()
        ));
    }

    #[test]
    fn agent_type_matches_policy_without_identity_registration() {
        use asc_daemon_protocol::authorization::DecisionVerdict;
        let (pap, mut authorization) = fixture();
        let policy = pap.list_policies(1, 0).unwrap().items.remove(0);
        pap.update_policy(&policy.policy_id, "codex", &source(r#"
            permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource)
            when { principal has agent_type && principal.agent_type == "codex" };
            permit(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource);
        "#)).unwrap();
        let mut input = request();
        input.requested_permissions.filesystem.rules = serde_json::from_value(serde_json::json!([
            {"effect":"allow","path":"/","rights":["read"]}
        ]))
        .unwrap();
        authorization.config.environment_permissions = input.requested_permissions.clone();
        assert_eq!(
            authorization.evaluate(&input, 100).unwrap().verdict,
            DecisionVerdict::Allow
        );
        for agent_type in [Some("qoder".into()), None] {
            input.principal.agent_type = agent_type;
            assert_eq!(
                authorization.evaluate(&input, 100).unwrap().verdict,
                DecisionVerdict::Deny
            );
        }
    }

    #[test]
    fn config_rejects_legacy_selection_and_invalid_ttl() {
        let (_, authorization) = fixture();
        for key in ["baseline", "baseline_policy", "task_policy"] {
            let mut value = serde_json::to_value(&authorization.config).unwrap();
            value[key] = serde_json::json!("old-policy");
            assert!(serde_json::from_value::<AuthorizationConfig>(value).is_err());
        }
        let mut config = authorization.config;
        config.ttl_seconds = 0;
        assert!(config.validate().is_err());
    }

    fn request() -> AuthorizationRequest {
        serde_json::from_value(serde_json::json!({
            "type":"os", "request_id":"r1", "execution_id":"e1",
            "principal":{"agent_type":"codex"},
            "launch":{"program":"true","resolved_program":"/usr/bin/true","args":[],"cwd":"/work"},
            "requested_permissions":{"version":1,"filesystem":{"default":"deny","rules":[]},
                "execution":{"deny_paths":[]},"network":{"mode":"off"}}
        }))
        .unwrap()
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
