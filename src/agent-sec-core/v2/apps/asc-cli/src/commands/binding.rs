//! Binding commands mapping desired-state references to PAP requests.

use asc_daemon_protocol::method::{
    POLICY_BINDINGS_CREATE, POLICY_BINDINGS_DELETE, POLICY_BINDINGS_GET, POLICY_BINDINGS_LIST,
    POLICY_BINDINGS_UPDATE,
};
use asc_daemon_protocol::{
    CreateBindingParams, DaemonRequest, ResourceParams, UpdateBindingParams,
};
use asc_foundation_types::{ResourceId, Revision};
use clap::{Args, Subcommand};

use super::common::{Page, encode, resource_id, revision};
use crate::InputError;

#[derive(Debug, Subcommand)]
pub(crate) enum BindingCommand {
    /// Create an Apply intent with a server-generated ID.
    Create(BindingInput),
    /// Read the current Binding and lifecycle status.
    Get(BindingId),
    /// List one page of current Bindings.
    List(Page),
    /// Replace an existing Binding's references and request Apply.
    Update {
        #[arg(long, value_parser = resource_id)]
        binding_id: ResourceId,
        #[command(flatten)]
        input: BindingInput,
    },
    /// Request deletion; completion is asynchronous.
    Delete(BindingId),
}

#[derive(Debug, Args)]
pub(crate) struct BindingId {
    #[arg(long, value_parser = resource_id)]
    binding_id: ResourceId,
}

#[derive(Debug, Args)]
pub(crate) struct BindingInput {
    #[arg(long, value_parser = resource_id)]
    policy_id: ResourceId,
    #[arg(long, value_parser = revision)]
    policy_revision: Revision,
    #[arg(long, value_parser = resource_id)]
    scope_id: ResourceId,
    #[arg(long, value_parser = revision)]
    scope_revision: Revision,
}

impl BindingCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            Self::Create(input) => encode(
                POLICY_BINDINGS_CREATE,
                &CreateBindingParams {
                    policy_id: input.policy_id.clone(),
                    policy_revision: input.policy_revision,
                    scope_id: input.scope_id.clone(),
                    scope_revision: input.scope_revision,
                },
            ),
            Self::Update { binding_id, input } => encode(
                POLICY_BINDINGS_UPDATE,
                &UpdateBindingParams {
                    binding_id: binding_id.clone(),
                    policy_id: input.policy_id.clone(),
                    policy_revision: input.policy_revision,
                    scope_id: input.scope_id.clone(),
                    scope_revision: input.scope_revision,
                },
            ),
            Self::Get(input) | Self::Delete(input) => encode(
                if matches!(self, Self::Get(_)) {
                    POLICY_BINDINGS_GET
                } else {
                    POLICY_BINDINGS_DELETE
                },
                &ResourceParams {
                    id: input.binding_id.clone(),
                },
            ),
            Self::List(page) => encode(POLICY_BINDINGS_LIST, &page.params()),
        }
    }
}
