//! Scope commands and mutually exclusive process/cgroup selectors.

use asc_daemon_protocol::method::{
    POLICY_SCOPES_CREATE, POLICY_SCOPES_DELETE, POLICY_SCOPES_GET, POLICY_SCOPES_LIST,
    POLICY_SCOPES_RETRY,
};
use asc_daemon_protocol::{CreateScopeParams, DaemonRequest, ResourceParams};
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::scope::{PolicyReference, ProcessMatcher, ScopeSelector};
use clap::{Args, Subcommand};

use super::common::{Page, encode, resource_id, revision};
use crate::InputError;

#[derive(Debug, Subcommand)]
pub(crate) enum ScopeCommand {
    /// Create a Scope with a server-generated ID.
    Create(CreateScope),
    /// Read an immutable assignment and its lifecycle status.
    Get(ScopeId),
    /// List one page of current Scopes.
    List(Page),
    /// Retry failed Bindings owned by this Scope.
    Retry(ScopeId),
    /// Stop discovery and request cleanup of owned Bindings.
    Delete(ScopeId),
}

#[derive(Debug, Args)]
pub(crate) struct CreateScope {
    #[command(flatten)]
    selector: Selector,
    /// Exact Policy ID to snapshot into the assignment.
    #[arg(long, value_parser = resource_id, requires = "policy_revision")]
    policy_id: ResourceId,
    #[arg(long, value_parser = revision, requires = "policy_id")]
    policy_revision: Revision,
}

#[derive(Debug, Args)]
pub(crate) struct ScopeId {
    #[arg(long, value_parser = resource_id)]
    scope_id: ResourceId,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub(crate) struct Selector {
    /// Positive root process ID.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pid: Option<u32>,
    /// Exact kernel process name (comm), selecting existing and future processes.
    #[arg(long)]
    process_name: Option<String>,
    /// Exact normalized absolute executable path.
    #[arg(long)]
    executable: Option<String>,
    /// Positive cgroup ID; mutually exclusive with --pid.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    cgroup_id: Option<u64>,
}

impl ScopeCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            Self::Create(input) => encode(
                POLICY_SCOPES_CREATE,
                &CreateScopeParams {
                    selector: input.selector.value(),
                    policy_templates: vec![PolicyReference {
                        policy_id: input.policy_id.clone(),
                        policy_revision: input.policy_revision,
                    }],
                },
            ),
            Self::Get(input) => encode(
                POLICY_SCOPES_GET,
                &ResourceParams {
                    id: input.scope_id.clone(),
                },
            ),
            Self::Retry(input) => encode(
                POLICY_SCOPES_RETRY,
                &ResourceParams {
                    id: input.scope_id.clone(),
                },
            ),
            Self::Delete(input) => encode(
                POLICY_SCOPES_DELETE,
                &ResourceParams {
                    id: input.scope_id.clone(),
                },
            ),
            Self::List(page) => encode(POLICY_SCOPES_LIST, &page.params()),
        }
    }
}

impl Selector {
    fn value(&self) -> ScopeSelector {
        match (
            self.pid,
            self.cgroup_id,
            &self.process_name,
            &self.executable,
        ) {
            (Some(pid), None, None, None) => ScopeSelector::Pid { pid },
            (None, Some(cgroup_id), None, None) => ScopeSelector::CgroupId { cgroup_id },
            (None, None, Some(name), None) => ScopeSelector::Process {
                matcher: ProcessMatcher::Name {
                    process_name: name.clone(),
                },
            },
            (None, None, None, Some(path)) => ScopeSelector::Process {
                matcher: ProcessMatcher::Executable {
                    executable: path.clone(),
                },
            },
            _ => unreachable!("clap requires exactly one selector; fields are private"),
        }
    }
}
