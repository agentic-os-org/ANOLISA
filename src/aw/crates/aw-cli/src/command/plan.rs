//! Show static Source assignments without probing or activating any process.

use aw_contracts::source::SourceCapability;
use aw_core::resolver::{resolve_configuration, MAX_CANDIDATE_SOURCES};
use clap::Args;
use serde_json::json;
use std::{io::Write, path::PathBuf};

#[derive(Args, Debug, PartialEq)]
pub(super) struct Options {
    /// Configuration document containing the target and policy
    #[arg(long, value_name = "FILE")]
    config: PathBuf,
    /// Target name declared in the configuration
    #[arg(long, value_name = "TARGET")]
    agent: String,
    /// SourceCapability JSON document (repeat for each candidate)
    #[arg(long, value_name = "FILE")]
    source: Vec<PathBuf>,
}

pub(super) fn run(options: Options) -> crate::cli::Result<crate::cli::Exit> {
    if options.source.len() > MAX_CANDIDATE_SOURCES {
        return Err("candidate source set exceeds maximum capacity".into());
    }
    let bytes = crate::cli::read_file(&options.config, 4 * 1024 * 1024)?;
    let configuration = aw_config::Validator::new()?.parse(&bytes)?;
    let sources = options
        .source
        .iter()
        .map(|path| {
            SourceCapability::parse(&crate::cli::read_file(path, 64 * 1024)?).map_err(Into::into)
        })
        .collect::<crate::cli::Result<Vec<_>>>()?;
    let plan = resolve_configuration(&configuration, &options.agent, &sources)?;
    let evidence = &configuration.as_value()["spec"]["agents"][&options.agent]["evidence"];
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(
        &mut stdout,
        &json!({
            "hook_plan": plan,
            "source_readiness": "unknown",
            "configured_observation": if evidence.is_null() { serde_json::Value::Null } else { json!({
                "id": evidence["source_id"],
                "mode": evidence["mode"].as_str().unwrap_or("auto"),
                "collector_coverage": "unknown",
                "ebpf_coverage": "unknown",
            }) }
        }),
    )?;
    writeln!(stdout)?;
    Ok(crate::cli::Exit::Code(0))
}
