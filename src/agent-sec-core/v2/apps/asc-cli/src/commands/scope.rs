//! Scope commands and mutually exclusive process/cgroup selectors.

use asc_daemon_protocol::method::{
    POLICY_SCOPES_CREATE, POLICY_SCOPES_DELETE, POLICY_SCOPES_GET, POLICY_SCOPES_LIST,
    POLICY_SCOPES_UPDATE,
};
use asc_daemon_protocol::{CreateScopeParams, DaemonRequest, RevisionParams, UpdateScopeParams};
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::scope::ScopeSelector;
use clap::{Args, Subcommand};

use super::common::{Page, encode, resource_id, revision};
use crate::InputError;

#[derive(Debug, Subcommand)]
pub(crate) enum ScopeCommand {
    /// Create a Scope with a server-generated ID.
    Create(Selector),
    /// Read the exact current revision.
    Get(ScopeRevision),
    /// List one page of current Scopes.
    List(Page),
    /// Replace an existing Scope's complete selector (not upsert).
    Update {
        #[arg(long, value_parser = resource_id)]
        scope_id: ResourceId,
        #[command(flatten)]
        selector: Selector,
    },
    /// Delete the exact current revision.
    Delete(ScopeRevision),
}

#[derive(Debug, Args)]
pub(crate) struct ScopeRevision {
    #[arg(long, value_parser = resource_id)]
    scope_id: ResourceId,
    #[arg(long, value_parser = revision)]
    revision: Revision,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(crate) struct Selector {
    /// Positive root process ID.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pid: Option<u32>,
    /// Positive cgroup ID; mutually exclusive with --pid.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    cgroup_id: Option<u64>,
}

impl ScopeCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            Self::Create(selector) => encode(
                POLICY_SCOPES_CREATE,
                &CreateScopeParams {
                    selector: selector.value(),
                },
            ),
            Self::Update { scope_id, selector } => encode(
                POLICY_SCOPES_UPDATE,
                &UpdateScopeParams {
                    scope_id: scope_id.clone(),
                    selector: selector.value(),
                },
            ),
            Self::Get(input) | Self::Delete(input) => encode(
                if matches!(self, Self::Get(_)) {
                    POLICY_SCOPES_GET
                } else {
                    POLICY_SCOPES_DELETE
                },
                &RevisionParams {
                    id: input.scope_id.clone(),
                    revision: input.revision,
                },
            ),
            Self::List(page) => encode(POLICY_SCOPES_LIST, &page.params()),
        }
    }
}

impl Selector {
    fn value(&self) -> ScopeSelector {
        match (self.pid, self.cgroup_id) {
            (Some(pid), None) => ScopeSelector::Pid { pid },
            (None, Some(cgroup_id)) => ScopeSelector::CgroupId { cgroup_id },
            _ => unreachable!("clap requires exactly one selector; fields are private"),
        }
    }
}
