//! Standalone CLI for running optimization analysis on an ATIF trajectory file.
//!
//! Usage:
//!   analyze <trajectory.json> --dim <perf|cost|accuracy> [--stats-only]
//!
//! Environment variables (required for LLM-backed dimensions):
//!   OPENAI_API_KEY   — LLM API key
//!   OPENAI_BASE_URL  — LLM base URL (default: https://api.openai.com/v1)
//!   OPENAI_MODEL     — model name (default: gpt-4o)

use std::process;

use anyhow::{bail, Context, Result};

use agentsight_opt::{AnalyzePipeline, AtifTrajectory, LlmClient};

fn usage() {
    eprintln!(
        "Usage: analyze <trajectory.json> --dim <perf|cost|accuracy> [--stats-only]\n\
         \n\
         Dimensions:\n\
         \x20 perf      Performance stats + LLM strategy selection\n\
         \x20 cost      Cost stats + LLM waste identification\n\
         \x20 accuracy  LLM accuracy attribution\n\
         \n\
         Options:\n\
         \x20 --stats-only  Local perf/cost statistics; no LLM requests or API key\n\
         \n\
         Environment variables:\n\
         \x20 OPENAI_API_KEY   LLM API key (required)\n\
         \x20 OPENAI_BASE_URL  LLM base URL (default: https://api.openai.com/v1)\n\
         \x20 OPENAI_MODEL     Model name (default: gpt-4o)"
    );
}

fn parse_args() -> Result<(String, String, bool)> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.is_empty() || args.iter().any(|a| a == "--help" || a == "-h") {
        usage();
        process::exit(if args.is_empty() { 1 } else { 0 });
    }

    let mut file_path = None;
    let mut dim = None;
    let mut stats_only = false;
    let mut i = 0;

    while i < args.len() {
        match args[i].as_str() {
            "--stats-only" => stats_only = true,
            "--dim" => {
                i += 1;
                if i >= args.len() {
                    bail!("--dim requires a value (perf|cost|accuracy)");
                }
                dim = Some(args[i].clone());
            }
            arg if arg.starts_with("--dim=") => {
                dim = Some(arg.trim_start_matches("--dim=").to_string());
            }
            other => {
                if file_path.is_some() {
                    bail!("unexpected argument: {other}");
                }
                file_path = Some(other.to_string());
            }
        }
        i += 1;
    }

    let file_path = file_path.context("missing <trajectory.json> path")?;
    let dim = dim.context("missing --dim <perf|cost|accuracy>")?;

    match dim.as_str() {
        "perf" | "cost" | "accuracy" => {}
        other => bail!("unknown dimension '{other}', expected: perf, cost, accuracy"),
    }

    if stats_only && dim == "accuracy" {
        bail!("--stats-only supports perf and cost; accuracy requires LLM analysis");
    }

    Ok((file_path, dim, stats_only))
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e:#}");
        process::exit(1);
    }
}

async fn run() -> Result<()> {
    let (file_path, dim, stats_only) = parse_args()?;

    // Load trajectory
    let content =
        std::fs::read_to_string(&file_path).with_context(|| format!("cannot read {file_path}"))?;
    let trajectory = AtifTrajectory::from_json(&content)
        .with_context(|| format!("failed to parse ATIF trajectory from {file_path}"))?;

    // Keep the credential and network boundary below the entire local path.
    if stats_only {
        let output = match dim.as_str() {
            "perf" => serde_json::json!({"perf": AnalyzePipeline::run_perf(&trajectory)?}),
            "cost" => serde_json::json!({"cost": AnalyzePipeline::run_cost(&trajectory)?}),
            _ => unreachable!(), // Validated by parse_args before reading the input.
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }

    // Build LLM client + pipeline
    let client = LlmClient::from_env().context(
        "LLM not configured — set OPENAI_API_KEY (and optionally OPENAI_BASE_URL, OPENAI_MODEL)",
    )?;
    let pipeline = AnalyzePipeline::new(&client);

    // Run selected dimension
    let output: serde_json::Value = match dim.as_str() {
        "perf" => {
            let stats = AnalyzePipeline::run_perf(&trajectory)?;
            let issues = pipeline.run_perf_issues(&trajectory).await?;
            serde_json::json!({
                "perf": stats,
                "perf_issues": issues,
            })
        }
        "cost" => {
            let stats = AnalyzePipeline::run_cost(&trajectory)?;
            let waste = pipeline.run_cost_waste(&trajectory).await?;
            serde_json::json!({
                "cost": stats,
                "cost_waste": waste,
            })
        }
        "accuracy" => {
            let result = pipeline.run_accuracy(&trajectory, None).await?;
            serde_json::to_value(result)?
        }
        _ => unreachable!(),
    };

    let pretty = serde_json::to_string_pretty(&output)?;
    println!("{pretty}");
    Ok(())
}
