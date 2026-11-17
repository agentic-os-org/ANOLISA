//! Reads current PAP policies at each authorization boundary.

use asc_daemon_core::{AuthorizationError, AuthorizationEvaluator};
use asc_daemon_protocol::authorization::{Decision, OsAuthorizationRequest};
use asc_pap::{PapRepository, PapService};
use asc_pdp_runtime::PdpRuntime;
use std::sync::{Arc, Mutex};

/// Compiled cache for the complete current PAP authorization set.
/// The cache never substitutes for a successful current-policy read.
pub struct PapAuthorizationPolicy<R> {
    pap: PapService<R>,
    active: Mutex<Option<Arc<PdpRuntime>>>,
}

impl<R: PapRepository> PapAuthorizationPolicy<R> {
    /// Uses PAP immediately; an empty set denies requests until policies are created.
    pub fn new(pap: PapService<R>) -> Self {
        Self {
            pap,
            active: Mutex::new(None),
        }
    }

    fn current_runtime(&self) -> Result<Arc<PdpRuntime>, PublishedPolicyError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| PublishedPolicyError::State)?;
        let result = (|| {
            // Hold the cache lock across the read and load: an older read cannot publish last.
            let policies = self.pap.authorization_policies()?;
            if let Some(runtime) = active
                .as_ref()
                .filter(|cached| cached.policy_version() == policies.version)
            {
                return Ok(Arc::clone(runtime));
            }
            let runtime = Arc::new(PdpRuntime::from_policy_set(policies)?);
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
        request: &OsAuthorizationRequest,
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

/// Failures reading or compiling the current PAP authorization set.
#[derive(Debug, thiserror::Error)]
pub enum PublishedPolicyError {
    /// PAP could not provide the current policies.
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
        let authorization = PapAuthorizationPolicy::new(pap.clone());
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
        let (pap, authorization) = fixture();
        let policy = pap.list_policies(1, 0).unwrap().items.remove(0);
        pap.update_policy(&policy.policy_id, "codex", &source(r#"
            permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource)
            when { principal has agent_type && principal.agent_type == "codex" };
            permit(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource);
        "#)).unwrap();
        let mut input = request();
        input.context.requested_permissions.filesystem.rules =
            serde_json::from_value(serde_json::json!([
                {"effect":"allow","path":"/","rights":["read"]}
            ]))
            .unwrap();
        assert_eq!(
            authorization.evaluate(&input, 100).unwrap().verdict,
            DecisionVerdict::Allow
        );
        for agent_type in [Some("qoder".into()), None] {
            input.context.agent.agent_type = agent_type;
            assert_eq!(
                authorization.evaluate(&input, 100).unwrap().verdict,
                DecisionVerdict::Deny
            );
        }
    }

    fn request() -> OsAuthorizationRequest {
        OsAuthorizationRequest::new("r1".into(), serde_json::from_value(serde_json::json!({
            "execution_id":"e1",
            "agent":{"agent_type":"codex"},
            "launch":{"program":"true","resolved_program":"/usr/bin/true","args":[],"cwd":"/work"},
            "requested_permissions":{"version":1,"filesystem":{"default":"deny","rules":[]},
                "execution":{"deny_paths":[]},"network":{"mode":"off"}}
        }))
        .unwrap())
    }
}
