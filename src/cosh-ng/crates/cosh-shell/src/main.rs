// Shared renderer tests run once in the library; private runtime helpers stay in both targets.
#[cfg(test)]
macro_rules! library_tests {
    ($($item:item)*) => {};
}

mod activity;
#[allow(dead_code, unused_imports)]
mod adapter;
mod agent;
mod approval;
mod auth;
#[allow(dead_code, unused_imports)]
mod command;
#[allow(dead_code, unused_imports)]
mod config;
#[allow(dead_code, unused_imports)]
mod diagnostics;
#[allow(dead_code, unused_imports)]
mod evidence;
#[allow(dead_code, unused_imports)]
mod hooks;
#[allow(dead_code, unused_imports)]
mod i18n;
#[allow(dead_code, unused_imports)]
mod input;
#[allow(dead_code)]
mod insight;
#[allow(dead_code, unused_imports)]
mod journal;
#[allow(dead_code, unused_imports)]
mod ledger;
#[allow(dead_code, unused_imports)]
mod parser;
mod process;
mod question;
#[allow(dead_code, unused_imports)]
mod raw_input;
mod recommendation;
mod runtime;
#[allow(dead_code, unused_imports)]
mod shell_host;
mod slash;
#[allow(dead_code, unused_imports)]
mod tools;
#[cfg(test)]
#[path = "ui/trial_layout_tests.rs"]
mod trial_layout_tests;
#[allow(dead_code, unused_imports)]
mod types;
#[allow(dead_code, unused_imports)]
#[path = "ui/public.rs"]
mod ui;
#[allow(dead_code, unused_imports)]
mod upgrade;

use runtime::cli_args::configured_raw_invocation;
use runtime::invocation::{
    classify_invocation, exec_gateway, exec_shell, gateway_plan, is_cosh_entry, Invocation,
    TuiEntry,
};
use runtime::startup::{exec_direct_command, print_usage_help};

#[allow(unused_imports)]
mod binary_compat {
    pub(crate) use super::adapter::{adapter_for_kind, AdapterInstance, AdapterKind, AgentAdapter};
    pub(crate) use super::agent::governance::{
        govern_agent_events, govern_agent_events_with_language, GovernanceOutput,
    };
    pub(crate) use super::command::{
        classify_executed_command_outcome, classify_exit, classify_shell_handoff_command_outcome,
        first_program_token, CommandOutcome, ExitCodeCategory,
    };
    pub(crate) use super::config::{
        language_config_status, load_config, parse_language_setting, resolve_language_setting,
        write_user_language_config, CoshConfig, Language, LanguageConfigStatus,
    };
    pub(crate) use super::hooks::builtin::default_builtin_hooks;
    pub(crate) use super::hooks::model::{HookInput, HookMatcher, HookTrigger};
    pub(crate) use super::i18n::{I18n, MessageId};
    pub(crate) use super::slash::registry::{
        active_slash_commands, active_slash_hint_commands, exact_slash_control_commands,
        slash_command_registry, visible_slash_commands, SlashCommandSpec, SlashCommandState,
    };
    pub(crate) use super::types::{FindingSeverity, HookFinding};
    pub(crate) use super::ui::render_transcript;
}
#[allow(unused_imports)]
pub(crate) use binary_compat::*;

fn run_tui(mut entry: TuiEntry) -> i32 {
    match runtime::login::login_shell_plan(entry.login_argv0) {
        Ok(Some(plan)) => {
            if let Some(warning) = runtime::login::apply_login_shell_plan(&mut entry, plan) {
                eprintln!("cosh: {warning}");
            }
        }
        Ok(None) => {}
        Err(error) => {
            eprintln!("cosh: {error}");
            return 1;
        }
    }
    runtime::terminal::install_terminal_recovery();
    let cosh_config = config::load_config();
    runtime::logging::init_logging(&cosh_config.log_level);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "cosh-shell starting");
    let (adapter_name, shell_kind, launch_options) = configured_raw_invocation(entry, &cosh_config);
    runtime::controller::run_raw(&adapter_name, shell_kind, launch_options)
}

fn main() {
    if let Some(status) = runtime::startup::run_profile_probe_helper_if_requested() {
        std::process::exit(status);
    }

    let args_os = std::env::args_os().collect::<Vec<_>>();
    let cosh_entry = is_cosh_entry(&args_os[0]);
    if cosh_entry {
        if let Some(plan) = gateway_plan(&args_os[1..]) {
            std::process::exit(exec_gateway(plan));
        }
    }
    let subcommand = args_os.get(1).and_then(|arg| arg.to_str());
    let legacy_command = matches!(
        subcommand,
        Some(
            "--version"
                | "--help"
                | "diagnostics"
                | "demo"
                | "doctor"
                | "host-demo"
                | "interactive"
                | "interactive-demo"
                | "adapter-demo"
        )
    );
    if cosh_entry || !legacy_command {
        use std::io::IsTerminal;
        let status = match classify_invocation(
            &args_os[0],
            &args_os[1..],
            std::io::stdin().is_terminal(),
            std::io::stdout().is_terminal(),
            std::io::stderr().is_terminal(),
        ) {
            Invocation::ExecShell(plan) => {
                if !cosh_entry
                    && matches!(subcommand, Some("--" | "raw"))
                    && plan.args.first().and_then(|arg| arg.to_str()) == Some("--")
                {
                    exec_direct_command(&plan.args[1..])
                } else {
                    exec_shell(plan)
                }
            }
            Invocation::Tui(entry) => run_tui(entry),
            Invocation::Guide(args) => config::guide::run_cli(&args),
            Invocation::Login(args) => runtime::login::run_cli(&args),
            Invocation::ShellIntegration(operation) => {
                runtime::shell_integration::run_cli(operation)
            }
            Invocation::HealthWorker => diagnostics::health::runtime::run_private_health_worker(),
            Invocation::LoginShellPlanWorker => runtime::login::probe::run_worker(),
            Invocation::LoginInvitationWorker => runtime::login::invitation::run_worker(),
            Invocation::LoginEditor => runtime::login::editor::run_editor(),
            Invocation::LoginLifecycle(args) => runtime::login::lifecycle::run_cli(&args),
            Invocation::Health(args) => runtime::health_banner::run_cli(&args),
            Invocation::UsageError(error) => {
                eprintln!("cosh: {error}");
                2
            }
        };
        std::process::exit(status);
    }

    let args = std::env::args().collect::<Vec<_>>();

    if args.get(1).map(String::as_str) == Some("--version") {
        println!("cosh-shell {}", env!("CARGO_PKG_VERSION"));
        std::process::exit(0);
    }
    if args.get(1).map(String::as_str) == Some("--help") {
        print_usage_help();
        std::process::exit(0);
    }
    if args.get(1).map(String::as_str) == Some("diagnostics") {
        std::process::exit(diagnostics::bundle::run_cli(&args[2..]));
    }

    runtime::terminal::install_terminal_recovery();

    // Initialize structured logging (file output to ~/.copilot-shell/logs/)
    let cosh_config = config::load_config();
    runtime::logging::init_logging(&cosh_config.log_level);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "cosh-shell starting");

    let status = match args.get(1).map(String::as_str) {
        Some("demo") => runtime::controller::run_demo(),
        Some("doctor") => runtime::doctor::run_doctor(),
        Some("host-demo") => runtime::controller::run_host_demo(),
        Some("interactive") => {
            runtime::controller::run_interactive(args.get(2).map(String::as_str).unwrap_or("fake"))
        }
        Some("interactive-demo") => runtime::controller::run_interactive_demo(
            args.get(2).map(String::as_str).unwrap_or("fake"),
        ),
        Some("adapter-demo") => {
            runtime::controller::run_adapter_demo(args.get(2).map(String::as_str).unwrap_or("fake"))
        }
        _ => {
            eprintln!(
                "usage: cosh-shell <demo|host-demo|raw|interactive|interactive-demo|adapter-demo [fake|claude|co|qwen|cosh-core] [--shell bash|zsh] [--resume [session-id]]>"
            );
            2
        }
    };
    std::process::exit(status);
}
