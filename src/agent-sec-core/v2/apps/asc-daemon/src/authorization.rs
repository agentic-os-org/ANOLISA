//! Checks the PAP generation at each authorization boundary.

use asc_daemon_core::{AuthorizationError, AuthorizationEvaluator};
use asc_daemon_protocol::authorization::{Decision, OsAuthorizationRequest};
use asc_pap::{ConsumerPolicySet, PapRepository, PapService};
use asc_pdp_runtime::{PdpRuntime, selection};
use std::sync::{Arc, Mutex};

/// The launch PDP compiled from the current Sandbox selection.
pub struct PapAuthorizationPolicy<R> {
    pap: PapService<R>,
    active: Mutex<Option<(u64, Arc<PdpRuntime>)>>,
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
        self.current_runtime_with(compile)
    }

    fn current_runtime_with(
        &self,
        load: impl FnOnce(&ConsumerPolicySet) -> Result<PdpRuntime, PublishedPolicyError>,
    ) -> Result<Arc<PdpRuntime>, PublishedPolicyError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| PublishedPolicyError::State)?;
        let result = (|| {
            // Hold the cache lock across the read and load: an older read cannot publish last.
            let policies = self.pap.sandbox_selections(selection::select)?;
            if let Some((_, runtime)) = active
                .as_ref()
                .filter(|(generation, _)| *generation == policies.generation)
            {
                return Ok(Arc::clone(runtime));
            }
            let runtime = Arc::new(load(&policies)?);
            *active = Some((policies.generation, Arc::clone(&runtime)));
            Ok(runtime)
        })();
        if result.is_err() {
            // A failed current-set read or load must never fall back to an earlier grant.
            *active = None;
        }
        result
    }
}

fn compile(policies: &ConsumerPolicySet) -> Result<PdpRuntime, PublishedPolicyError> {
    let version = policies.authorization_version()?;
    Ok(PdpRuntime::from_selected(version, policies.iter())?)
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
        schema_source(1, text)
    }

    fn schema_source(revision: u32, text: &str) -> PolicySource {
        PolicySource::Cedar(
            serde_json::from_value(serde_json::json!({
                "schema":{"schemaId":"seccore.system","revision":revision},"text":text
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
    fn requests_started_after_revocation_never_use_the_pending_old_load() {
        use asc_daemon_protocol::authorization::DecisionVerdict;
        use std::sync::{Barrier, mpsc};

        let (pap, authorization) = fixture();
        let policy = pap.list_policies(1, 0).unwrap().items.remove(0);
        let launch = r#"permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource);"#;
        let read = r#"permit(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource);"#;
        pap.update_policy(
            &policy.policy_id,
            "read allowed",
            &source(&format!("{launch} {read}")),
        )
        .unwrap();
        let input = readable_request();
        std::thread::scope(|scope| {
            let (loaded, loaded_rx) = mpsc::channel();
            let (release, release_rx) = mpsc::channel();
            let authorization = &authorization;
            let old = scope.spawn(move || {
                authorization
                    .current_runtime_with(|snapshot| {
                        let runtime = compile(snapshot)?;
                        loaded.send(()).unwrap();
                        release_rx.recv().unwrap();
                        Ok(runtime)
                    })
                    .unwrap()
            });
            loaded_rx.recv().unwrap();
            pap.update_policy(&policy.policy_id, "read revoked", &source(launch))
                .unwrap();
            let waiting = Arc::new(Barrier::new(5));
            let input = &input;
            let requests: Vec<_> = (0..4)
                .map(|_| {
                    let waiting = Arc::clone(&waiting);
                    scope.spawn(move || {
                        waiting.wait();
                        authorization.evaluate(input, 101).unwrap()
                    })
                })
                .collect();
            waiting.wait();
            release.send(()).unwrap();

            let old = old.join().unwrap();
            assert_eq!(
                old.evaluate(input, 100).unwrap().verdict,
                DecisionVerdict::Allow
            );
            let current = authorization.current_runtime().unwrap();
            assert_ne!(current.policy_version(), old.policy_version());
            for request in requests {
                let decision = request.join().unwrap();
                assert_eq!(decision.verdict, DecisionVerdict::Deny);
                assert_eq!(decision.policy_version, current.policy_version());
            }
        });
    }

    #[test]
    fn manual_v2_scope_update_preserves_sandbox_decision_and_receipt() {
        use asc_daemon_protocol::authorization::DecisionVerdict;
        use asc_pap::{PapError, ScopeDiscovery, ScopeDiscoverySeed};
        use asc_policy_types::identifiers::ResourceId;
        use asc_policy_types::scope::{PolicyReference, ScopeSelector};

        struct Discovery;
        impl ScopeDiscovery for Discovery {
            fn start(&self, _: &ScopeDiscoverySeed) -> Result<(), PapError> {
                Ok(())
            }
            fn stop(&self, _: &ResourceId) -> Result<(), PapError> {
                Ok(())
            }
        }

        let (pap, authorization) = fixture();
        let pap = pap.with_scope_discovery(Arc::new(Discovery));
        let permit = r#"permit(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource);"#;
        pap.create_policy("read", &source(permit)).unwrap();
        let before = authorization.evaluate(&readable_request(), 100).unwrap();
        assert_eq!(before.verdict, DecisionVerdict::Allow);
        let manual = |path| {
            schema_source(
                2,
                &format!(
                    r#"forbid(principal, action == SecCore::Action::"file.read", resource) when {{resource.path == "{path}"}};"#
                ),
            )
        };
        let first = pap.create_policy("manual", &manual("/private")).unwrap();
        let scope = pap
            .create_scope_assignment(
                &ScopeSelector::Pid { pid: 4242 },
                &[PolicyReference {
                    policy_id: first.policy_id.clone(),
                    policy_revision: first.revision,
                }],
                None,
            )
            .unwrap();
        pap.update_policy(&first.policy_id, "manual", &manual("/usr/bin/true"))
            .unwrap();
        assert_eq!(
            pap.get_scope(&scope.scope_id).unwrap().policy_snapshots,
            [first]
        );
        let current = authorization.evaluate(&readable_request(), 101).unwrap();
        assert_eq!(current.verdict, before.verdict);
        assert_eq!(current.policy_version, before.policy_version);
    }

    #[test]
    fn one_compiled_sandbox_evaluates_1000_requests_with_their_own_agent_types() {
        use asc_daemon_protocol::authorization::DecisionVerdict;
        let (pap, authorization) = fixture();
        let policy = pap.list_policies(1, 0).unwrap().items.remove(0);
        pap.update_policy(&policy.policy_id, "codex", &source(r#"
            permit(principal is SecCore::Subject, action == SecCore::Action::"process.spawn", resource)
            when { principal has agentType && principal.agentType == "codex" };
            permit(principal is SecCore::Subject, action == SecCore::Action::"file.read", resource);
        "#)).unwrap();
        let runtime = authorization.current_runtime().unwrap();
        let mut input = readable_request();
        let cases = [
            (Some("codex"), DecisionVerdict::Allow),
            (Some("qoder"), DecisionVerdict::Deny),
            (None, DecisionVerdict::Deny),
        ];
        for (agent_type, expected) in cases.iter().cycle().take(1_000) {
            input.context.agent.agent_type = agent_type.map(str::to_owned);
            let decision = authorization.evaluate(&input, 100).unwrap();
            assert_eq!(decision.verdict, *expected);
            assert_eq!(decision.policy_version, runtime.policy_version());
        }
        assert!(Arc::ptr_eq(
            &runtime,
            &authorization.current_runtime().unwrap()
        ));
    }

    fn readable_request() -> OsAuthorizationRequest {
        let mut input = request();
        input.context.requested_permissions.filesystem.rules = serde_json::from_value(
            serde_json::json!([{"effect":"allow","path":"/","rights":["read"]}]),
        )
        .unwrap();
        input
    }

    fn request() -> OsAuthorizationRequest {
        OsAuthorizationRequest::new("r1".into(), serde_json::from_value(serde_json::json!({
            "execution_id":"e1",
            "agent":{"agentType":"codex"},
            "launch":{"program":"true","resolved_program":"/usr/bin/true","args":[],"cwd":"/work"},
            "requested_permissions":{"version":1,"filesystem":{"default":"deny","rules":[]},
                "execution":{"deny_paths":[]},"network":{"mode":"off"}}
        }))
        .unwrap())
    }
}
