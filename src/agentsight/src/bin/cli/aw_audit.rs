//! Same-user queries for the AW execution projection.

use std::path::PathBuf;
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct AwAuditCommand {
    /// AgentSight storage base used by serve
    #[structopt(long, parse(from_os_str))]
    storage_base: PathBuf,
    /// Source identity from AW audit export
    #[structopt(long)]
    source_id: String,
    /// Inclusive start time in RFC 3339 format
    #[structopt(long)]
    from: String,
    /// Exclusive end time in RFC 3339 format
    #[structopt(long)]
    to: String,
    /// Only include objects for this AW instance
    #[structopt(long)]
    instance_id: Option<String>,
}

impl AwAuditCommand {
    pub fn execute(&self) -> anyhow::Result<()> {
        let result = agentsight::aw_audit::query(
            &self.storage_base,
            &self.source_id,
            self.instance_id.as_deref(),
            chrono::DateTime::parse_from_rfc3339(&self.from)?.timestamp_millis(),
            chrono::DateTime::parse_from_rfc3339(&self.to)?.timestamp_millis(),
        )?;
        super::print_json(&result);
        Ok(())
    }
}
