#![forbid(unsafe_code)]
#![allow(dead_code)]

mod audit;
mod auth;
mod brokered_profile;
mod cli;
mod compaction;
mod compression;
mod config;
mod context;
mod core;
mod extension;
mod headless;
mod hook;
mod interactive;
mod logging;
mod loop_detect;
mod metrics;
mod migrate;
mod paths;
mod process;
mod protocol;
mod redaction;
mod registry;
mod session;
mod session_control;
mod skill;
mod sls;
mod state;
mod tool;
mod truncator;

use clap::Parser;
use cosh_core::provider;
#[cfg(unix)]
use std::time::Duration;

use config::CoreConfig;
use provider::openai_compat::OpenAICompatProvider;
use provider::profile;

fn create_provider(config: &CoreConfig) -> Box<dyn provider::ContentGenerator> {
    let resolved = config.resolve_provider();
    if resolved.provider_type == "mock" {
        if resolved.model == "mock-partial-error" {
            return Box::new(provider::mock::MockProvider::partial_error());
        }
        if resolved.model == "mock-compact-summary" {
            // Deterministic bounded output for compaction lifecycle tests.
            return Box::new(provider::mock::MockProvider::repeat_text(
                "## Objective and constraints\n- deterministic mock summary",
            ));
        }
        return Box::new(provider::mock::MockProvider::history_echo());
    }
    // Aliyun provider uses AK/SK, not API key
    if resolved.provider_type == "aliyun" {
        if resolved.auth_source.as_deref() == Some("ecs_ram_role") {
            return Box::new(provider::sysom::SysomProvider::from_ecs_ram_role(
                &resolved.sysom_endpoint,
            ));
        }
        if resolved.access_key_id.is_empty() || resolved.access_key_secret.is_empty() {
            tracing::warn!("no AK/SK configured for aliyun, using mock provider");
            return Box::new(provider::mock::MockProvider::text_only(
                "No AK/SK configured. Please set ALIBABA_CLOUD_ACCESS_KEY_ID/SECRET or use /auth.",
            ));
        }
        return Box::new(provider::sysom::SysomProvider::new(
            &resolved.access_key_id,
            &resolved.access_key_secret,
            resolved.security_token.as_deref(),
            &resolved.sysom_endpoint,
        ));
    }
    if resolved.api_key.is_empty() {
        tracing::warn!("no API key configured, using mock provider");
        return Box::new(provider::mock::MockProvider::text_only(
            "No API key configured. Please set DASHSCOPE_API_KEY or configure [ai.providers] in config.toml.",
        ));
    }
    create_provider_from_resolved(&resolved)
}

fn create_provider_from_resolved(
    resolved: &config::ResolvedProvider,
) -> Box<dyn provider::ContentGenerator> {
    let provider_profile = profile::profile_from_name(&resolved.provider_type);
    Box::new(OpenAICompatProvider::new(
        &resolved.base_url,
        &resolved.api_key,
        provider_profile,
        resolved.explicit_cache,
    ))
}

/// Check if auth is needed (no API key or AK/SK configured).
fn needs_auth(config: &CoreConfig) -> bool {
    config.resolve_provider().auth_required()
}

#[cfg(unix)]
fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|error| {
            eprintln!("failed to start async runtime: {error}");
            std::process::exit(1);
        });

    runtime.block_on(run_until_sigint());
    // Tokio reads stdin on a blocking thread, which cannot be cancelled while a pipe stays open.
    runtime.shutdown_timeout(Duration::from_millis(100));
}

#[cfg(not(unix))]
#[tokio::main]
async fn main() {
    run().await;
}

#[cfg(unix)]
async fn run_until_sigint() {
    tokio::select! {
        signal = wait_for_sigint() => {
            match signal {
                Ok(()) => tracing::info!("received SIGINT, shutting down cosh-core"),
                Err(error) => {
                    eprintln!("failed to install SIGINT handler: {error}");
                    std::process::exit(1);
                }
            }
        }
        _ = run() => {}
    }
}

async fn run() {
    let args = cli::CliArgs::parse();
    let agent_headless = is_agent_headless_mode(&args);
    if args.execution_profile.is_brokered()
        && (!agent_headless
            || args.is_session_control()
            || args.prompt.is_some()
            || args.cosh_shell_transport
            || args.enable_shell_evidence_tool
            || args.approval_mode.is_some()
            || args.allowed_tools.is_some()
            || args.tools.is_some())
    {
        match args.execution_profile {
            cli::ExecutionProfile::GatewayBrokeredV1 => eprintln!(
                "[cosh-core] gateway-brokered-v1 requires persistent headless mode and rejects legacy tool or approval overrides"
            ),
            cli::ExecutionProfile::GatewayBrokeredCheckpointV1 => eprintln!(
                "[cosh-core] gateway-brokered-checkpoint-v1 requires persistent headless mode and rejects legacy tool or approval overrides"
            ),
            cli::ExecutionProfile::GatewayBrokeredWorkspaceWriteV1 => eprintln!(
                "[cosh-core] gateway-brokered-workspace-write-v1 requires persistent headless mode and rejects legacy tool or approval overrides"
            ),
            cli::ExecutionProfile::Legacy => unreachable!(
                "legacy execution cannot enter the brokered launch validation branch"
            ),
        }
        std::process::exit(2);
    }
    if args.is_session_control() {
        std::process::exit(session_control::run());
    }
    let (project_root, session_workspace) = if agent_headless {
        let requested_root = args.workspace_path();
        match tool::SessionWorkspace::try_new(&requested_root) {
            Ok(workspace) => {
                let project_root = workspace.root().to_path_buf();
                (project_root, Some(workspace))
            }
            Err(error) => {
                eprintln!("[cosh-core] {error}");
                std::process::exit(2);
            }
        }
    } else {
        (args.workspace_root(), None)
    };
    // The execution boundary is a launch property. Resolve it before reading
    // any workspace project config or constructing hooks/extensions/MCP.
    let config = if args.execution_profile.is_brokered() {
        CoreConfig::load_gateway_brokered()
    } else if args.bare {
        CoreConfig::load_bare()
    } else {
        CoreConfig::load_for_workspace(&project_root)
    };

    let log_level = config.logging.effective_level(args.verbose);
    logging::init_logging(&log_level);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "cosh-core starting");

    if let Some(cli::Command::Mcp(mcp)) = args.command {
        if let Err(error) = tool::mcp::run_command(mcp, &config, &project_root).await {
            eprintln!("MCP command failed: {error}");
            std::process::exit(1);
        }
    } else if args.is_registry() {
        registry::run(&args, config).await;
    } else if args.is_compact() {
        std::process::exit(compaction::run_compact_cli(&args, config).await);
    } else if agent_headless {
        let Some(session_workspace) = session_workspace else {
            eprintln!("[cosh-core] headless workspace was not initialized");
            std::process::exit(2);
        };
        match headless::run(&args, config, project_root, session_workspace).await {
            Ok(0) => {}
            Ok(exit_code) => std::process::exit(exit_code),
            Err(error) => {
                eprintln!("[cosh-core] {error}");
                std::process::exit(2);
            }
        }
    } else {
        interactive::run(&args, config).await;
    }
}

fn is_agent_headless_mode(args: &cli::CliArgs) -> bool {
    args.command.is_none() && !args.is_registry() && !args.is_compact() && args.is_headless()
}

#[cfg(unix)]
async fn wait_for_sigint() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AiConfig, CoreConfig, ProviderConfig};
    use std::collections::HashMap;

    fn parse_args(args: &[&str]) -> cli::CliArgs {
        let mut full = vec!["cosh-core"];
        full.extend_from_slice(args);
        cli::CliArgs::try_parse_from(full).unwrap()
    }

    #[test]
    fn only_agent_mode_initializes_the_headless_workspace() {
        assert!(is_agent_headless_mode(&parse_args(&["--headless"])));
        assert!(!is_agent_headless_mode(&parse_args(&[
            "--headless",
            "mcp",
            "list"
        ])));
        assert!(!is_agent_headless_mode(&parse_args(&[
            "--headless",
            "--registry"
        ])));
        assert!(!is_agent_headless_mode(&parse_args(&[
            "--headless",
            "--compact"
        ])));
    }

    #[test]
    fn ecs_ram_role_aliyun_provider_does_not_need_static_auth() {
        let old_ak = std::env::var("ALIBABA_CLOUD_ACCESS_KEY_ID").ok();
        let old_sk = std::env::var("ALIBABA_CLOUD_ACCESS_KEY_SECRET").ok();
        let old_token = std::env::var("ALIBABA_CLOUD_SECURITY_TOKEN").ok();
        std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_ID");
        std::env::remove_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET");
        std::env::remove_var("ALIBABA_CLOUD_SECURITY_TOKEN");

        let mut providers = HashMap::new();
        providers.insert(
            "aliyun-ecs".to_string(),
            ProviderConfig {
                provider_type: Some("aliyun".to_string()),
                auth_source: Some("ecs_ram_role".to_string()),
                model: Some("qwen3.7-plus".to_string()),
                ..Default::default()
            },
        );
        let config = CoreConfig {
            ai: AiConfig {
                active_provider: Some("aliyun-ecs".to_string()),
                providers,
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(!needs_auth(&config));

        if let Some(value) = old_ak {
            std::env::set_var("ALIBABA_CLOUD_ACCESS_KEY_ID", value);
        }
        if let Some(value) = old_sk {
            std::env::set_var("ALIBABA_CLOUD_ACCESS_KEY_SECRET", value);
        }
        if let Some(value) = old_token {
            std::env::set_var("ALIBABA_CLOUD_SECURITY_TOKEN", value);
        }
    }
}
