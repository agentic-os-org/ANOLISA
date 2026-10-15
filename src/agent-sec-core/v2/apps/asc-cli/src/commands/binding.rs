//! Read-only queries for system-owned Bindings.
use super::common::{Page, encode, resource_id};
use crate::InputError;
use asc_daemon_protocol::method::{POLICY_BINDINGS_GET, POLICY_BINDINGS_LIST};
use asc_daemon_protocol::{DaemonRequest, ResourceParams};
use asc_foundation_types::ResourceId;
use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub(crate) enum BindingCommand {
    /// Read a Binding and its deployment status.
    Get(BindingId),
    /// List system-created Bindings.
    List(Page),
}
#[derive(Debug, Args)]
pub(crate) struct BindingId {
    #[arg(long, value_parser = resource_id)]
    binding_id: ResourceId,
}
impl BindingCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            Self::Get(input) => encode(
                POLICY_BINDINGS_GET,
                &ResourceParams {
                    id: input.binding_id.clone(),
                },
            ),
            Self::List(page) => encode(POLICY_BINDINGS_LIST, &page.params()),
        }
    }
}
