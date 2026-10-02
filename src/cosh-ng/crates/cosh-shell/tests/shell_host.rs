use std::collections::HashSet;
use std::io::{self, BufRead, Read, Write};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use cosh_shell::adapter::{adapter_for_kind, AdapterKind, AgentAdapter};
use cosh_shell::agent::govern_agent_events;
use cosh_shell::journal::read_shell_events;
use cosh_shell::ledger::build_command_blocks;
use cosh_shell::parser::{agent_request_after_confirmation, findings_from_blocks};
use cosh_shell::raw_input::{RawInputCapture, RawObserverAction, RawRelayAction};
use cosh_shell::shell_host::{
    run_line_interactive_bash as shell_run_line_interactive_bash,
    run_raw_relay_bash as shell_run_raw_relay_bash,
    run_raw_relay_bash_with_actions as shell_run_raw_relay_bash_with_actions,
    run_raw_relay_bash_with_actions_output_control as shell_run_raw_relay_bash_with_actions_output_control,
    run_raw_relay_bash_with_observer as shell_run_raw_relay_bash_with_observer,
    run_raw_relay_zsh_with_actions as shell_run_raw_relay_zsh_with_actions,
    run_raw_relay_zsh_with_output_control as shell_run_raw_relay_zsh_with_output_control,
    run_scripted_bash as shell_run_scripted_bash, run_scripted_zsh as shell_run_scripted_zsh,
    LineInteractiveOutput, ScriptedInput, ShellHostConfig, ShellHostOutput, ShellIntegration,
};
use cosh_shell::types::{
    AgentEvent, GovernanceDecision, ImplicitPagerPolicy, Policy, ShellEvent, ShellEventKind,
    ShellHandoffRequest,
};

#[path = "support/shell_host.rs"]
mod support_shell_host;
use support_shell_host::{
    assert_clean_shell_output_ref, assert_fullscreen_terminal_modes_balanced, assert_no_osc_marker,
    assert_no_synthetic_terminal_restore_after_interrupt, ledger_from_output,
    ledger_output_refs_text, make_executable, render_terminal_screen, shell_arg, stty_flag_probe,
    unique_suffix, DelayedInput,
};

fn bash_supports_command_not_found_handler() -> bool {
    Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "command_not_found_handle(){ return 0; }; __cosh_missing_probe",
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn bash_supports_prompt_command_array() -> bool {
    Command::new("bash")
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "(( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) ))",
        ])
        .status()
        .is_ok_and(|status| status.success())
}

#[path = "shell_host/foreground.rs"]
mod foreground;
#[path = "shell_host/governance.rs"]
mod governance;
#[path = "shell_host/handoff.rs"]
mod handoff;
#[path = "shell_host/heavy.rs"]
mod heavy;
#[path = "shell_host/input_intent.rs"]
mod input_intent;
#[path = "shell_host/marker.rs"]
mod marker;
#[path = "shell_host/native.rs"]
mod native;
#[path = "shell_host/parity.rs"]
mod parity;
#[path = "shell_host/relay.rs"]
mod relay;
#[path = "shell_host/termios.rs"]
mod termios;

static SHELL_HOST_RUN_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn shell_host_run_guard() -> MutexGuard<'static, ()> {
    SHELL_HOST_RUN_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn shell_host_test_config(config: &ShellHostConfig) -> ShellHostConfig {
    let mut config = config.clone();
    if !config.env_overrides.iter().any(|(key, _)| key == "INPUTRC") {
        config = with_raw_byte_readline(config);
    }
    if !config.env_overrides.iter().any(|(key, _)| key == "HOME") {
        config.env_overrides.push((
            "HOME".to_string(),
            config.work_dir.join("home").display().to_string(),
        ));
    }
    for (key, value) in [
        ("HISTIGNORE", ""),
        ("HISTSIZE", "1000"),
        ("HISTFILESIZE", "1000"),
        ("PROMPT_COMMAND", ""),
    ] {
        if !config
            .env_overrides
            .iter()
            .any(|(existing, _)| existing == key)
        {
            config
                .env_overrides
                .push((key.to_string(), value.to_string()));
        }
    }
    config
}

fn with_raw_byte_readline(config: ShellHostConfig) -> ShellHostConfig {
    with_bracketed_paste_readline(config, false)
}

fn with_bracketed_paste_readline(
    config: ShellHostConfig,
    enable_bracketed_paste: bool,
) -> ShellHostConfig {
    std::fs::create_dir_all(&config.work_dir).expect("readline work dir");
    let inputrc = config.work_dir.join("inputrc");
    std::fs::write(
        &inputrc,
        format!(
            "set input-meta on\nset convert-meta off\nset output-meta on\n\
             set enable-bracketed-paste {}\n",
            if enable_bracketed_paste { "on" } else { "off" }
        ),
    )
    .expect("readline inputrc");
    config.with_env("INPUTRC", inputrc.display().to_string())
}

fn without_readline_mode_controls(terminal: &str) -> String {
    terminal
        .replace("\x1b[?2004h", "")
        .replace("\x1b[?2004l", "")
}

fn run_scripted_bash(
    config: &ShellHostConfig,
    inputs: &[ScriptedInput],
) -> io::Result<ShellHostOutput> {
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_scripted_bash(&config, inputs)
}

fn run_scripted_zsh(
    config: &ShellHostConfig,
    inputs: &[ScriptedInput],
) -> io::Result<ShellHostOutput> {
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_scripted_zsh(&config, inputs)
}

fn run_line_interactive_bash<R, W>(
    config: &ShellHostConfig,
    input: R,
    output: W,
) -> io::Result<LineInteractiveOutput>
where
    R: BufRead,
    W: Write,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_line_interactive_bash(&config, input, output)
}

fn run_raw_relay_bash<R, W>(
    config: &ShellHostConfig,
    input: R,
    output: W,
) -> io::Result<ShellHostOutput>
where
    R: Read + Send + 'static,
    W: Write,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_raw_relay_bash(&config, input, output)
}

fn run_raw_relay_bash_with_observer<R, W, F>(
    config: &ShellHostConfig,
    input: R,
    output: W,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    R: Read + Send + 'static,
    W: Write,
    F: FnMut(&[ShellEvent], &mut W) -> io::Result<()>,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_raw_relay_bash_with_observer(&config, input, output, event_observer)
}

fn run_raw_relay_bash_with_actions<W>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
) -> io::Result<ShellHostOutput>
where
    W: Write,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_raw_relay_bash_with_actions(&config, actions, output)
}

fn run_raw_relay_zsh_with_actions<W>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
) -> io::Result<ShellHostOutput>
where
    W: Write,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_raw_relay_zsh_with_actions(&config, actions, output)
}

fn run_raw_relay_bash_with_actions_output_control<W, F>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    W: Write,
    F: FnMut(&[ShellEvent], &mut W) -> io::Result<RawObserverAction>,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_raw_relay_bash_with_actions_output_control(&config, actions, output, event_observer)
}

fn run_raw_relay_zsh_with_output_control<R, W, F>(
    config: &ShellHostConfig,
    input: R,
    output: W,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    R: Read + Send + 'static,
    W: Write,
    F: FnMut(&[ShellEvent], &mut W) -> io::Result<RawObserverAction>,
{
    let _guard = shell_host_run_guard();
    let config = shell_host_test_config(config);
    shell_run_raw_relay_zsh_with_output_control(&config, input, output, event_observer)
}
