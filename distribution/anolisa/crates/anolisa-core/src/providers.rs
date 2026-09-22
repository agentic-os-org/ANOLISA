//! Delegated-provider bridge between the planner vocabulary and the
//! platform package contracts.
//!
//! The planner speaks in facts and verbs ([`NativeProbe`], [`NativeAction`]);
//! the platform speaks in backend calls ([`PackageQuery`],
//! [`PackageTransaction`]). [`DelegatedProvider`] translates between the two
//! so the executor never touches a backend trait directly: `observe` turns a
//! query result into a probe fact, `transact` turns a native verb into the
//! matching transaction call. No policy lives here — whether a transaction is
//! authorized was already decided by the planner.

use anolisa_platform::pkg_query::{PackageInfo, PackageQuery, PackageQueryError};
use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};
use thiserror::Error;

use crate::domain::Observation;
use crate::planner::{NativeAction, NativeProbe};

/// Errors raised while talking to the native authority.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The read side (rpm/dnf query) failed.
    #[error(transparent)]
    Query(#[from] PackageQueryError),
    /// The write side (dnf transaction) failed.
    #[error(transparent)]
    Transaction(#[from] PackageTransactionError),
}

/// Bridge to one native package manager, read and write side together.
///
/// Borrows trait objects so the CLI can keep constructing
/// `RpmPackageQuery`/`RpmTransaction` (or fakes) wherever it already does.
pub struct DelegatedProvider<'a> {
    query: &'a dyn PackageQuery,
    txn: &'a dyn PackageTransaction,
}

impl<'a> DelegatedProvider<'a> {
    /// Bundle a query and a transaction backend into one provider.
    pub fn new(query: &'a dyn PackageQuery, txn: &'a dyn PackageTransaction) -> Self {
        Self { query, txn }
    }

    /// Probe the native database for `package` and classify the result as a
    /// planner fact.
    ///
    /// The backend's single-instance violation ("N installed versions",
    /// kernel-style multi-install) is mapped to
    /// [`NativeProbe::MultipleVersions`] rather than surfaced as an error:
    /// the planner has a dedicated row for it. The origin lookup is
    /// best-effort — `query_installed` cannot report a repo, and a failed
    /// origin query must not turn a successful probe into a failure.
    ///
    /// `observed_at` is the RFC3339 UTC timestamp stamped into the resulting
    /// [`Observation`]; the caller owns the clock.
    pub fn observe(&self, package: &str, observed_at: &str) -> Result<NativeProbe, ProviderError> {
        match self.query.query_installed(package) {
            Ok(Some(info)) => {
                let origin = self.query.installed_origin(package).ok().flatten();
                Ok(NativeProbe::Present {
                    package: info.name.clone(),
                    observation: observation_from(&info, origin, observed_at),
                })
            }
            Ok(None) => Ok(NativeProbe::Absent),
            Err(err) if is_multiple_versions(&err) => Ok(NativeProbe::MultipleVersions {
                package: package.to_string(),
            }),
            Err(err) => Err(ProviderError::Query(err)),
        }
    }

    /// Configured transaction source; this does not establish an existing package's origin.
    pub fn repository_source(&self) -> Option<&str> {
        self.txn.repository_source()
    }

    /// Check an install without applying the native transaction.
    ///
    /// # Errors
    /// Propagates the native solver or tooling failure.
    pub fn check_install(&self, packages: &[&str]) -> Result<(), ProviderError> {
        self.txn.check_install(packages)?;
        Ok(())
    }

    /// Run one native transaction verb over `packages`.
    ///
    /// The whole slice goes to the backend as a single native transaction:
    /// the package manager's solver resolves the set together and the
    /// transaction commits or fails as a unit. A failure is reported for the
    /// transaction as a whole — attribution to one package is the caller's
    /// job (forward-only: re-observe and read the facts).
    pub fn transact(&self, action: NativeAction, packages: &[String]) -> Result<(), ProviderError> {
        let refs: Vec<&str> = packages.iter().map(String::as_str).collect();
        match action {
            NativeAction::Install => self.txn.install(&refs)?,
            NativeAction::Update => self.txn.update(&refs)?,
            NativeAction::Reinstall => self.txn.reinstall(&refs)?,
            NativeAction::Remove => self.txn.remove(&refs)?,
        }
        Ok(())
    }
}

/// Whether a query error is the backend's "several installed versions"
/// single-instance violation. The backend reports it as `UnexpectedOutput`
/// with detail `"N installed versions"`; `N == 0` is a genuine output
/// anomaly (rpm exited 0 with no rows) and stays an error.
fn is_multiple_versions(err: &PackageQueryError) -> bool {
    match err {
        PackageQueryError::UnexpectedOutput { detail, .. } => detail
            .strip_suffix(" installed versions")
            .and_then(|n| n.parse::<usize>().ok())
            .is_some_and(|n| n >= 2),
        _ => false,
    }
}

/// Build a fresh [`Observation`] from a query row plus a best-effort origin.
fn observation_from(info: &PackageInfo, origin: Option<String>, observed_at: &str) -> Observation {
    Observation {
        version: info.version.version.clone(),
        evr: Some(info.version.to_string()),
        arch: Some(info.arch.clone()),
        source_repo: origin.or_else(|| info.origin.clone()),
        observed_at: observed_at.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod test_fakes {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use anolisa_platform::pkg_query::{
        PackageInfo, PackageQuery, PackageQueryError, PackageVersion,
    };
    use anolisa_platform::pkg_transaction::{PackageTransaction, PackageTransactionError};

    /// Canned per-package outcome for [`FakeQuery::query_installed`].
    pub enum InstalledOutcome {
        Present(PackageInfo),
        Absent,
        MultipleVersions(usize),
        Fail,
    }

    #[derive(Default)]
    pub struct FakeQuery {
        pub installed: HashMap<String, InstalledOutcome>,
        pub origins: HashMap<String, String>,
        pub origin_fails: bool,
    }

    impl PackageQuery for FakeQuery {
        fn query_installed(&self, package: &str) -> Result<Option<PackageInfo>, PackageQueryError> {
            match self.installed.get(package) {
                Some(InstalledOutcome::Present(info)) => Ok(Some(info.clone())),
                Some(InstalledOutcome::Absent) | None => Ok(None),
                Some(InstalledOutcome::MultipleVersions(n)) => {
                    Err(PackageQueryError::UnexpectedOutput {
                        command: "rpm".to_string(),
                        detail: format!("{n} installed versions"),
                    })
                }
                Some(InstalledOutcome::Fail) => Err(PackageQueryError::QueryFailed {
                    command: "rpm".to_string(),
                    code: Some(1),
                    stderr: "rpmdb open failed".to_string(),
                }),
            }
        }

        fn query_available(&self, _package: &str) -> Result<Vec<PackageInfo>, PackageQueryError> {
            Ok(Vec::new())
        }

        fn installed_origin(&self, package: &str) -> Result<Option<String>, PackageQueryError> {
            if self.origin_fails {
                return Err(PackageQueryError::CommandMissing {
                    command: "dnf".to_string(),
                });
            }
            Ok(self.origins.get(package).cloned())
        }
    }

    /// Transaction fake recording one `(verb, packages.join(","))` entry per
    /// backend call — a merged multi-package call is one entry, so tests can
    /// pin that a batch really shared one native transaction. Verbs listed in
    /// `fail` return a canned `TransactionFailed`.
    #[derive(Default)]
    pub struct FakeTxn {
        pub calls: RefCell<Vec<(String, String)>>,
        pub fail: Vec<&'static str>,
        pub repository: Option<&'static str>,
    }

    impl FakeTxn {
        fn run(&self, verb: &str, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.calls
                .borrow_mut()
                .push((verb.to_string(), packages.join(",")));
            if self.fail.contains(&verb) {
                return Err(PackageTransactionError::TransactionFailed {
                    command: "dnf".to_string(),
                    operation: verb.to_string(),
                    code: Some(1),
                    stderr: format!("fake dnf {verb} failure"),
                });
            }
            Ok(())
        }
    }

    impl PackageTransaction for FakeTxn {
        fn repository_source(&self) -> Option<&str> {
            self.repository
        }
        fn check_install(&self, _packages: &[&str]) -> Result<(), PackageTransactionError> {
            panic!("this path must not preflight an install")
        }

        fn install(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.run("install", packages)
        }
        fn update(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.run("update", packages)
        }
        fn reinstall(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.run("reinstall", packages)
        }
        fn remove(&self, packages: &[&str]) -> Result<(), PackageTransactionError> {
            self.run("remove", packages)
        }
    }

    pub fn pkg_info(name: &str, version: &str, release: Option<&str>, arch: &str) -> PackageInfo {
        PackageInfo {
            name: name.to_string(),
            version: PackageVersion {
                epoch: None,
                version: version.to_string(),
                release: release.map(str::to_string),
            },
            arch: arch.to_string(),
            origin: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_fakes::{FakeQuery, FakeTxn, InstalledOutcome, pkg_info};
    use super::*;

    const NOW: &str = "2026-07-16T00:00:00Z";

    fn query_with(package: &str, outcome: InstalledOutcome) -> FakeQuery {
        let mut q = FakeQuery::default();
        q.installed.insert(package.to_string(), outcome);
        q
    }

    #[test]
    fn observe_present_builds_full_observation() {
        let mut query = query_with(
            "cosh",
            InstalledOutcome::Present(pkg_info("cosh", "2.7.0", Some("1.al4"), "x86_64")),
        );
        query
            .origins
            .insert("cosh".to_string(), "anolisa-release".to_string());
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        let probe = provider.observe("cosh", NOW).expect("observe ok");
        match probe {
            NativeProbe::Present {
                package,
                observation,
            } => {
                assert_eq!(package, "cosh");
                assert_eq!(observation.version, "2.7.0");
                assert_eq!(observation.evr.as_deref(), Some("2.7.0-1.al4"));
                assert_eq!(observation.arch.as_deref(), Some("x86_64"));
                assert_eq!(observation.source_repo.as_deref(), Some("anolisa-release"));
                assert_eq!(observation.observed_at, NOW);
            }
            other => panic!("expected Present, got {other:?}"),
        }
    }

    #[test]
    fn observe_absent_maps_to_absent_probe() {
        let query = query_with("cosh", InstalledOutcome::Absent);
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        assert_eq!(
            provider.observe("cosh", NOW).expect("observe ok"),
            NativeProbe::Absent
        );
    }

    #[test]
    fn observe_multi_install_maps_to_multiple_versions_fact() {
        // Kernel-style multi-install is a planner fact (dedicated table row),
        // not a provider failure.
        let query = query_with("kernel", InstalledOutcome::MultipleVersions(3));
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        assert_eq!(
            provider.observe("kernel", NOW).expect("observe ok"),
            NativeProbe::MultipleVersions {
                package: "kernel".to_string()
            }
        );
    }

    #[test]
    fn observe_other_query_errors_pass_through() {
        let query = query_with("cosh", InstalledOutcome::Fail);
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        let err = provider.observe("cosh", NOW).unwrap_err();
        assert!(matches!(
            err,
            ProviderError::Query(PackageQueryError::QueryFailed { .. })
        ));
    }

    #[test]
    fn observe_zero_rows_anomaly_stays_an_error() {
        // "0 installed versions" is rpm exiting 0 with no rows — a genuine
        // output anomaly, not the multi-install fact.
        let query = query_with("cosh", InstalledOutcome::MultipleVersions(0));
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        let err = provider.observe("cosh", NOW).unwrap_err();
        assert!(matches!(
            err,
            ProviderError::Query(PackageQueryError::UnexpectedOutput { .. })
        ));
    }

    #[test]
    fn observe_survives_origin_lookup_failure() {
        // Origin is display metadata; its failure must not kill the probe.
        let mut query = query_with(
            "cosh",
            InstalledOutcome::Present(pkg_info("cosh", "2.7.0", None, "x86_64")),
        );
        query.origin_fails = true;
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        match provider.observe("cosh", NOW).expect("observe ok") {
            NativeProbe::Present { observation, .. } => {
                assert_eq!(observation.source_repo, None);
            }
            other => panic!("expected Present, got {other:?}"),
        }
    }

    #[test]
    fn transact_routes_each_verb_to_its_backend_call() {
        let query = FakeQuery::default();
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        for (action, verb) in [
            (NativeAction::Install, "install"),
            (NativeAction::Update, "update"),
            (NativeAction::Reinstall, "reinstall"),
            (NativeAction::Remove, "remove"),
        ] {
            provider
                .transact(action, &["cosh".to_string()])
                .expect("transact ok");
            let last = txn.calls.borrow().last().cloned().expect("recorded call");
            assert_eq!(last, (verb.to_string(), "cosh".to_string()));
        }
    }

    #[test]
    fn transact_shares_one_backend_call_across_packages() {
        // The multi-package promise: the whole slice reaches the backend as a
        // single native transaction, not per-package calls.
        let query = FakeQuery::default();
        let txn = FakeTxn::default();
        let provider = DelegatedProvider::new(&query, &txn);

        provider
            .transact(NativeAction::Install, &["a".to_string(), "b".to_string()])
            .expect("transact ok");
        assert_eq!(
            txn.calls.borrow().as_slice(),
            &[("install".to_string(), "a,b".to_string())]
        );
    }

    #[test]
    fn transact_failure_is_reported_for_the_whole_set() {
        let query = FakeQuery::default();
        let txn = FakeTxn {
            fail: vec!["install"],
            ..FakeTxn::default()
        };
        let provider = DelegatedProvider::new(&query, &txn);

        let err = provider
            .transact(NativeAction::Install, &["a".to_string(), "b".to_string()])
            .unwrap_err();
        assert!(matches!(err, ProviderError::Transaction(_)));
        // One backend call covered both packages; the failure names neither.
        assert_eq!(txn.calls.borrow().len(), 1);
    }
}
