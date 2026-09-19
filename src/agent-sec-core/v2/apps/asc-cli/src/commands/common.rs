//! Shared pagination, identifier parsing and daemon request encoding.

use asc_daemon_protocol::{DaemonRequest, ListParams};
use asc_foundation_types::{ResourceId, Revision};
use clap::Args;
use serde::Serialize;

use crate::InputError;

#[derive(Debug, Args)]
pub(crate) struct Page {
    /// Number of records in this page (1..=1000).
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=1000))]
    limit: u32,
    /// Zero-based offset; the CLI does not fetch additional pages automatically.
    #[arg(long, default_value_t = 0)]
    offset: u32,
}

impl Page {
    pub(super) fn params(&self) -> ListParams {
        ListParams {
            limit: self.limit,
            offset: self.offset,
        }
    }
}

pub(super) fn encode(method: &str, params: &impl Serialize) -> Result<DaemonRequest, InputError> {
    Ok(DaemonRequest {
        trace_context: None,
        compatibility: None,
        method: method.to_owned(),
        params: serde_json::to_value(params)?,
    })
}

pub(super) fn resource_id(value: &str) -> Result<ResourceId, String> {
    ResourceId::new(value).map_err(|error| error.to_string())
}

pub(super) fn revision(value: &str) -> Result<Revision, String> {
    let number = value.parse::<u32>().map_err(|error| error.to_string())?;
    Revision::new(number).map_err(|error| error.to_string())
}
