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
#[allow(dead_code, unused_imports)]
mod types;
#[allow(dead_code, unused_imports)]
#[path = "ui/public.rs"]
mod ui;

use runtime::cli_args::configured_raw_invocation;
use runtime::invocation::{
    classify_invocation, exec_gateway, exec_shell, gateway_plan, is_cosh_entry, Invocation,
};
use runtime::startup::{
    passthrough_non_interactive, passthrough_raw_non_interactive, print_usage_help,
};

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

/// Full argv (argv[0] included) when this process was invoked through the
/// `/usr/bin/cosh` entry; `None` for the `cosh-shell` CLI surface.
fn cosh_entry_args() -> Option<Vec<std::ffi::OsString>> {
    let args = std::env::args_os().collect::<Vec<_>>();
    is_cosh_entry(args.first()?).then_some(args)
}

fn main() {
    if let Some(status) = runtime::startup::run_profile_probe_helper_if_requested() {
        std::process::exit(status);
    }

    // `/usr/bin/cosh` entry (argv[0] basename `cosh`, login `-cosh` included):
    // the invocation-transparency contract dispatches before any runtime
    // initialization so passthrough leaves no logs or terminal side effects.
    if let Some(args_os) = cosh_entry_args() {
        use std::io::IsTerminal;

        if let Some(plan) = gateway_plan(&args_os[1..]) {
            std::process::exit(exec_gateway(plan));
        }
        let stdin_tty = std::io::stdin().is_terminal();
        let stdout_tty = std::io::stdout().is_terminal();
        let stderr_tty = std::io::stderr().is_terminal();
        match classify_invocation(
            &args_os[0],
            &args_os[1..],
            stdin_tty,
            stdout_tty,
            stderr_tty,
        ) {
            Invocation::ExecShell(plan) => std::process::exit(exec_shell(plan)),
            Invocation::Tui(entry) => {
                runtime::terminal::install_terminal_recovery();
                let cosh_config = config::load_config();
                runtime::logging::init_logging(&cosh_config.log_level);
                tracing::info!(version = env!("CARGO_PKG_VERSION"), "cosh-shell starting");
                // The classifier only admits valid UTF-8 launch arguments;
                // argv[0] may be arbitrary bytes, so it never reaches the
                // TUI parser.
                let launch_args = entry
                    .launch_args
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                let (adapter_name, shell_kind, launch_options) =
                    configured_raw_invocation(&launch_args);
                let status =
                    runtime::controller::run_raw(&adapter_name, shell_kind, launch_options);
                std::process::exit(status);
            }
        }
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

    let has_subcommand = matches!(
        args.get(1).map(String::as_str),
        Some(
            "demo"
                | "doctor"
                | "host-demo"
                | "raw"
                | "interactive"
                | "interactive-demo"
                | "adapter-demo"
        )
    );
    if let Some(status) = passthrough_raw_non_interactive(&args) {
        std::process::exit(status);
    }
    if !has_subcommand {
        if let Some(status) = passthrough_non_interactive(&args) {
            std::process::exit(status);
        }
    }

    runtime::terminal::install_terminal_recovery();

    // Initialize structured logging (file output to ~/.copilot-shell/logs/)
    let cosh_config = config::load_config();
    runtime::logging::init_logging(&cosh_config.log_level);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "cosh-shell starting");

    if !has_subcommand {
        // The classifier admitted the TUI: only cosh-owned/login tokens on a
        // terminal reach this point.
        let (adapter_name, shell_kind, launch_options) = configured_raw_invocation(&args[1..]);
        let status = runtime::controller::run_raw(&adapter_name, shell_kind, launch_options);
        std::process::exit(status);
    }

    let status = match args.get(1).map(String::as_str) {
        Some("demo") => runtime::controller::run_demo(),
        Some("doctor") => runtime::doctor::run_doctor(),
        Some("host-demo") => runtime::controller::run_host_demo(),
        Some("raw") => {
            let (adapter_name, shell_kind, launch_options) = configured_raw_invocation(&args[2..]);
            runtime::controller::run_raw(&adapter_name, shell_kind, launch_options)
        }
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
