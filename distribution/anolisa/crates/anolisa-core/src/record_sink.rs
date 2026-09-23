//! [`RecordSink`] implementation over the v5 [`StateStore`].
//!
//! The executors decide *when* a record commit happens; this sink decides
//! *what* an [`crate::planner::RecordWrite`] materialises into — a full
//! [`Installation`] built from the operation's context — and persists the
//! store atomically on every commit, so a crash after the sink returns can
//! never lose an acknowledged write.

use std::path::Path;

use crate::domain::{
    Installation, InstallationScope, LifecycleStatus, ManagementRelation, NativePm, Observation,
    OwnedArtifact, PackageIdentity, ProviderBinding,
};
use crate::executor::{RecordSink, RecordSinkError};
use crate::planner::RecordWrite;
use crate::state::ObjectKind;
use crate::state_store::StateStore;

/// Native identity a delegated record write binds to.
#[derive(Debug, Clone)]
pub struct DelegatedIdentity {
    /// Which native manager owns the package.
    pub pm: NativePm,
    /// Resolved native package name.
    pub package: String,
}

/// Everything a record commit needs to know about the running operation.
/// Assembled by the command layer next to the plan; the sink never guesses.
#[derive(Debug, Clone)]
pub struct RecordContext {
    /// Object vocabulary (component, adapter, osbase).
    pub kind: ObjectKind,
    /// Object name.
    pub name: String,
    /// Scope the operation runs in.
    pub scope: InstallationScope,
    /// RFC3339 UTC timestamp used for `installed_at` / relation `since`.
    pub now: String,
    /// Operation id stamped into the record.
    pub operation_id: Option<String>,
    /// Native identity, required for the delegated write kinds.
    pub delegated: Option<DelegatedIdentity>,
    /// Owned artifact payload, required for [`RecordWrite::Owned`].
    pub owned_artifact: Option<OwnedArtifact>,
}

/// [`RecordSink`] that materialises record writes into a [`StateStore`] and
/// saves it to `state_path` on every commit.
pub struct StoreRecordSink<'a> {
    store: &'a mut StateStore,
    state_path: &'a Path,
    context: RecordContext,
}

impl<'a> StoreRecordSink<'a> {
    /// Bind a sink to one operation's context.
    pub fn new(store: &'a mut StateStore, state_path: &'a Path, context: RecordContext) -> Self {
        Self {
            store,
            state_path,
            context,
        }
    }

    /// The store this sink writes through (for post-execution reads).
    pub fn store(&self) -> &StateStore {
        self.store
    }

    fn persist_candidate(&mut self, candidate: StateStore) -> Result<(), RecordSinkError> {
        candidate
            .save(self.state_path)
            .map_err(|err| RecordSinkError(err.to_string()))?;
        *self.store = candidate;
        Ok(())
    }

    fn delegated_identity(&self) -> Result<&DelegatedIdentity, RecordSinkError> {
        self.context.delegated.as_ref().ok_or_else(|| {
            RecordSinkError(format!(
                "delegated record write for '{}' has no native package identity",
                self.context.name
            ))
        })
    }

    /// Fresh installation skeleton for this operation's object.
    fn new_installation(&self, binding: ProviderBinding) -> Installation {
        Installation {
            kind: self.context.kind,
            name: self.context.name.clone(),
            scope: self.context.scope,
            binding,
            status: LifecycleStatus::Installed,
            installed_at: self.context.now.clone(),
            last_operation_id: self.context.operation_id.clone(),
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        }
    }

    /// Write or upgrade a delegated record with `relation`. An existing
    /// delegated record of the same identity is upgraded in place — its
    /// observation cache, features, and history survive; only the relation
    /// (and refreshed observation) change. This is A6's "upgrade the
    /// management consent in place".
    fn write_delegated(
        &mut self,
        relation: ManagementRelation,
        observation: Option<&Observation>,
    ) -> Result<(), RecordSinkError> {
        let identity = self.delegated_identity()?.clone();
        let operation_id = self.context.operation_id.clone();
        let mut candidate = self.store.clone();

        if let Some(existing) = candidate.find_mut(self.context.kind, &self.context.name)
            && let ProviderBinding::Delegated {
                pm,
                package,
                relation: existing_relation,
                last_observed,
            } = &mut existing.binding
        {
            *pm = identity.pm;
            *package = PackageIdentity::Resolved {
                name: identity.package,
            };
            *existing_relation = relation;
            if let Some(observation) = observation {
                let mut refreshed = observation.clone();
                if refreshed.source_repo.is_none() {
                    refreshed.source_repo = last_observed
                        .as_ref()
                        .filter(|prior| {
                            prior.evr.is_some()
                                && prior.arch.is_some()
                                && prior.evr == refreshed.evr
                                && prior.arch == refreshed.arch
                        })
                        .and_then(|prior| prior.source_repo.clone());
                }
                *last_observed = Some(refreshed);
            }
            existing.status = LifecycleStatus::Installed;
            existing.last_operation_id = operation_id;
            return self.persist_candidate(candidate);
        }

        let installation = self.new_installation(ProviderBinding::Delegated {
            pm: identity.pm,
            package: PackageIdentity::Resolved {
                name: identity.package,
            },
            relation,
            last_observed: observation.cloned(),
        });
        candidate.upsert(installation);
        self.persist_candidate(candidate)
    }
}

impl RecordSink for StoreRecordSink<'_> {
    fn write_record(
        &mut self,
        write: RecordWrite,
        observation: Option<&Observation>,
    ) -> Result<(), RecordSinkError> {
        match write {
            RecordWrite::Owned => {
                let artifact = self.context.owned_artifact.clone().ok_or_else(|| {
                    RecordSinkError(format!(
                        "owned record write for '{}' has no artifact payload",
                        self.context.name
                    ))
                })?;
                let installation = self.new_installation(ProviderBinding::Owned { artifact });
                let mut candidate = self.store.clone();
                candidate.upsert(installation);
                self.persist_candidate(candidate)
            }
            RecordWrite::DelegatedManaged => self.write_delegated(
                ManagementRelation::Managed {
                    since: self.context.now.clone(),
                },
                observation,
            ),
            RecordWrite::DelegatedAdopted => self.write_delegated(
                ManagementRelation::Adopted {
                    since: self.context.now.clone(),
                },
                observation,
            ),
            RecordWrite::DelegatedObserved => {
                self.write_delegated(ManagementRelation::Observed, observation)
            }
            RecordWrite::RefreshObservation => {
                let name = self.context.name.clone();
                let operation_id = self.context.operation_id.clone();
                let identity = self.context.delegated.clone();
                let mut candidate = self.store.clone();
                let Some(existing) = candidate.find_mut(self.context.kind, &name) else {
                    return Err(RecordSinkError(format!(
                        "cannot refresh observation: no record for '{name}'"
                    )));
                };
                let ProviderBinding::Delegated {
                    package,
                    last_observed,
                    ..
                } = &mut existing.binding
                else {
                    return Err(RecordSinkError(format!(
                        "cannot refresh observation: record for '{name}' is not delegated"
                    )));
                };
                // A legacy record can carry an unresolved package hint; the
                // operation just probed a concrete package, so pin it. A
                // record that is already resolved keeps its name — refresh
                // must never rebind an identity.
                if let (PackageIdentity::Unresolved { .. }, Some(identity)) = (&*package, identity)
                {
                    *package = PackageIdentity::Resolved {
                        name: identity.package,
                    };
                }
                if let Some(observation) = observation {
                    let mut refreshed = observation.clone();
                    if refreshed.source_repo.is_none() {
                        refreshed.source_repo = last_observed
                            .as_ref()
                            .filter(|prior| {
                                prior.evr.is_some()
                                    && prior.arch.is_some()
                                    && prior.evr == refreshed.evr
                                    && prior.arch == refreshed.arch
                            })
                            .and_then(|prior| prior.source_repo.clone());
                    }
                    *last_observed = Some(refreshed);
                }
                existing.status = LifecycleStatus::Installed;
                existing.last_operation_id = operation_id;
                self.persist_candidate(candidate)
            }
        }
    }

    fn drop_record(&mut self) -> Result<(), RecordSinkError> {
        let mut candidate = self.store.clone();
        candidate.remove(self.context.kind, &self.context.name);
        self.persist_candidate(candidate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::RecordFacts;

    const NOW: &str = "2026-07-16T00:00:00Z";

    fn context(name: &str) -> RecordContext {
        RecordContext {
            kind: ObjectKind::Component,
            name: name.to_string(),
            scope: InstallationScope::System,
            now: NOW.to_string(),
            operation_id: Some("op-test-1".to_string()),
            delegated: Some(DelegatedIdentity {
                pm: NativePm::Rpm,
                package: name.to_string(),
            }),
            owned_artifact: None,
        }
    }

    fn observation(version: &str) -> Observation {
        Observation {
            version: version.to_string(),
            evr: Some(format!("{version}-1.al4")),
            arch: Some("x86_64".to_string()),
            source_repo: Some("anolisa-release".to_string()),
            observed_at: NOW.to_string(),
        }
    }

    fn artifact(version: &str) -> OwnedArtifact {
        OwnedArtifact {
            version: version.to_string(),
            distribution_source: Some("https://repo.example/cosh.tar.gz".to_string()),
            raw_package: None,
            manifest_digest: None,
            files: Vec::new(),
            services: Vec::new(),
            external_modified_files: Vec::new(),
            provisioned_packages: Vec::new(),
        }
    }

    #[test]
    fn source_follows_artifact_identity_on_refresh_and_management_change() {
        for write in [
            RecordWrite::RefreshObservation,
            RecordWrite::DelegatedManaged,
        ] {
            for (version, arch, expected) in [
                ("1.0", Some("x86_64"), Some("anolisa-release")),
                ("2.0", Some("x86_64"), None),
                ("1.0", Some("aarch64"), None),
                ("1.0", None, None),
            ] {
                let tmp = tempfile::tempdir().unwrap();
                let path = tmp.path().join("installed.toml");
                let mut store = StateStore::empty();
                let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
                sink.write_record(RecordWrite::DelegatedManaged, Some(&observation("1.0")))
                    .unwrap();
                let mut fresh = observation(version);
                fresh.arch = arch.map(String::from);
                fresh.source_repo = None;
                sink.write_record(write, Some(&fresh)).unwrap();
                let record = sink.store().find(ObjectKind::Component, "cosh").unwrap();
                let ProviderBinding::Delegated {
                    last_observed: Some(observed),
                    ..
                } = &record.binding
                else {
                    panic!("delegated observation")
                };
                assert_eq!(observed.version, version);
                assert_eq!(observed.arch.as_deref(), arch);
                assert_eq!(observed.source_repo.as_deref(), expected);
            }
        }
    }

    #[test]
    fn delegated_managed_write_creates_and_persists_the_record() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));

        let obs = observation("2.7.0");
        sink.write_record(RecordWrite::DelegatedManaged, Some(&obs))
            .expect("write");

        let reloaded = StateStore::load(&path, 0).expect("reload");
        let record = reloaded
            .find(ObjectKind::Component, "cosh")
            .expect("record");
        match &record.binding {
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { since },
                last_observed,
                package,
                ..
            } => {
                assert_eq!(since, NOW);
                assert_eq!(last_observed.as_ref(), Some(&obs));
                assert_eq!(package.resolved_name(), Some("cosh"));
            }
            other => panic!("expected managed delegated binding, got {other:?}"),
        }
        assert_eq!(record.last_operation_id.as_deref(), Some("op-test-1"));
    }

    #[test]
    fn adopt_upgrades_an_observed_record_in_place() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();

        // Seed an Observed record with history worth preserving.
        {
            let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
            sink.write_record(RecordWrite::DelegatedObserved, Some(&observation("2.6.0")))
                .expect("seed");
        }
        store
            .find_mut(ObjectKind::Component, "cosh")
            .expect("seeded")
            .enabled_features
            .push("telemetry".to_string());

        // A6: adopt upgrades the consent, keeps everything else.
        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
        sink.write_record(RecordWrite::DelegatedAdopted, None)
            .expect("adopt");

        let record = store.find(ObjectKind::Component, "cosh").expect("record");
        match &record.binding {
            ProviderBinding::Delegated {
                relation: ManagementRelation::Adopted { .. },
                last_observed,
                ..
            } => {
                // No fresh observation was passed: the cache survives.
                assert_eq!(
                    last_observed.as_ref().map(|o| o.version.as_str()),
                    Some("2.6.0")
                );
            }
            other => panic!("expected adopted binding, got {other:?}"),
        }
        assert_eq!(record.enabled_features, vec!["telemetry".to_string()]);
    }

    #[test]
    fn refresh_observation_updates_only_the_cache() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        {
            let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
            sink.write_record(RecordWrite::DelegatedManaged, Some(&observation("2.6.0")))
                .expect("seed");
        }

        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
        sink.write_record(RecordWrite::RefreshObservation, Some(&observation("2.7.0")))
            .expect("refresh");

        let record = store.find(ObjectKind::Component, "cosh").expect("record");
        match &record.binding {
            ProviderBinding::Delegated {
                relation: ManagementRelation::Managed { .. },
                last_observed,
                ..
            } => {
                assert_eq!(
                    last_observed.as_ref().map(|o| o.version.as_str()),
                    Some("2.7.0")
                );
            }
            other => panic!("expected managed binding to survive, got {other:?}"),
        }
    }

    #[test]
    fn refresh_backfills_an_unresolved_package_name() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();

        // A migrated legacy record: delegated, but the package name was
        // never captured.
        store.upsert(Installation {
            kind: ObjectKind::Component,
            name: "cosh".to_string(),
            scope: InstallationScope::System,
            binding: ProviderBinding::Delegated {
                pm: NativePm::Rpm,
                package: PackageIdentity::Unresolved {
                    component_hint: "cosh".to_string(),
                },
                relation: ManagementRelation::Managed {
                    since: NOW.to_string(),
                },
                last_observed: None,
            },
            status: LifecycleStatus::Installed,
            installed_at: NOW.to_string(),
            last_operation_id: None,
            subscription_scope: Default::default(),
            enabled_features: Vec::new(),
            health: Vec::new(),
        });

        let mut ctx = context("cosh");
        ctx.delegated = Some(DelegatedIdentity {
            pm: NativePm::Rpm,
            package: "cosh-cli".to_string(),
        });
        let mut sink = StoreRecordSink::new(&mut store, &path, ctx);
        sink.write_record(RecordWrite::RefreshObservation, Some(&observation("2.7.0")))
            .expect("refresh");

        let record = store.find(ObjectKind::Component, "cosh").expect("record");
        match &record.binding {
            ProviderBinding::Delegated { package, .. } => {
                assert_eq!(package.resolved_name(), Some("cosh-cli"));
            }
            other => panic!("expected delegated binding, got {other:?}"),
        }
    }

    #[test]
    fn refresh_never_rebinds_a_resolved_package_name() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        {
            let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
            sink.write_record(RecordWrite::DelegatedManaged, Some(&observation("2.6.0")))
                .expect("seed");
        }

        let mut ctx = context("cosh");
        ctx.delegated = Some(DelegatedIdentity {
            pm: NativePm::Rpm,
            package: "other-package".to_string(),
        });
        let mut sink = StoreRecordSink::new(&mut store, &path, ctx);
        sink.write_record(RecordWrite::RefreshObservation, Some(&observation("2.7.0")))
            .expect("refresh");

        let record = store.find(ObjectKind::Component, "cosh").expect("record");
        match &record.binding {
            ProviderBinding::Delegated { package, .. } => {
                assert_eq!(package.resolved_name(), Some("cosh"));
            }
            other => panic!("expected delegated binding, got {other:?}"),
        }
    }

    #[test]
    fn refresh_without_a_record_is_an_error() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));

        let err = sink
            .write_record(RecordWrite::RefreshObservation, Some(&observation("2.7.0")))
            .unwrap_err();
        assert!(err.to_string().contains("no record"));
    }

    #[test]
    fn owned_write_requires_and_uses_the_artifact_payload() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();

        // Missing payload is a hard error, not a guessed-empty artifact.
        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
        assert!(sink.write_record(RecordWrite::Owned, None).is_err());

        let mut ctx = context("cosh");
        ctx.owned_artifact = Some(artifact("2.7.0"));
        let mut sink = StoreRecordSink::new(&mut store, &path, ctx);
        sink.write_record(RecordWrite::Owned, None).expect("write");

        let record = store.find(ObjectKind::Component, "cosh").expect("record");
        assert!(matches!(
            &record.binding,
            ProviderBinding::Owned { artifact } if artifact.version == "2.7.0"
        ));
    }

    #[test]
    fn drop_record_removes_and_persists() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        {
            let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
            sink.write_record(RecordWrite::DelegatedManaged, Some(&observation("2.7.0")))
                .expect("seed");
        }

        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));
        sink.drop_record().expect("drop");

        let reloaded = StateStore::load(&path, 0).expect("reload");
        assert!(matches!(
            reloaded.record_facts(ObjectKind::Component, "cosh"),
            RecordFacts::Absent
        ));
    }

    #[test]
    fn delegated_write_without_identity_is_an_error() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        let mut ctx = context("cosh");
        ctx.delegated = None;
        let mut sink = StoreRecordSink::new(&mut store, &path, ctx);

        let err = sink
            .write_record(RecordWrite::DelegatedManaged, None)
            .unwrap_err();
        assert!(err.to_string().contains("no native package identity"));
    }

    #[test]
    fn failed_write_does_not_mutate_the_shared_store() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let blocked_parent = tmp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"block").expect("write blocker");
        let path = blocked_parent.join("installed.toml");
        let mut store = StateStore::empty();
        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));

        sink.write_record(RecordWrite::DelegatedManaged, Some(&observation("2.7.0")))
            .expect_err("save must fail");

        assert!(sink.store().find(ObjectKind::Component, "cosh").is_none());
    }

    #[test]
    fn failed_drop_does_not_mutate_the_shared_store() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let good_path = tmp.path().join("installed.toml");
        let mut store = StateStore::empty();
        {
            let mut sink = StoreRecordSink::new(&mut store, &good_path, context("cosh"));
            sink.write_record(RecordWrite::DelegatedManaged, Some(&observation("2.7.0")))
                .expect("seed");
        }
        let blocked_parent = tmp.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"block").expect("write blocker");
        let path = blocked_parent.join("installed.toml");
        let mut sink = StoreRecordSink::new(&mut store, &path, context("cosh"));

        sink.drop_record().expect_err("save must fail");

        assert!(sink.store().find(ObjectKind::Component, "cosh").is_some());
    }
}
