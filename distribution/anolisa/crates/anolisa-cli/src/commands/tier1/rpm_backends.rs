//! Construct local observations and native transactions with the same repository source.

use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_repo::RpmRepoSource;
use anolisa_platform::rpm_transaction::RpmTransaction;

/// Neither construction nor local observation fetches metadata or probes yum/dnf.
pub(crate) fn system(repo: Option<RpmRepoSource>) -> (RpmPackageQuery, RpmTransaction) {
    match repo {
        Some(repo) => (
            RpmPackageQuery::system_with_repo(repo.clone()),
            RpmTransaction::system_with_repo(repo),
        ),
        None => (RpmPackageQuery::system(), RpmTransaction::system()),
    }
}
