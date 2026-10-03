use crate::runtime::mode::render_mode_command;
use crate::runtime::prelude::*;
use crate::slash::audit::render_audit_command;
use crate::slash::config::render_config_command;
use crate::slash::debug::render_debug_command;
use crate::slash::extensions::render_extensions_command;
use crate::slash::health::render_health_command;
use crate::slash::hooks::render_hooks_command;
use crate::slash::mcp::render_mcp_command;
use crate::slash::notices::{
    render_help, render_hint, render_info, render_removed_command, render_unknown,
};
use crate::slash::panel::render_notice_panel;
use crate::slash::parser::SlashCommand;
use crate::slash::recommendations::render_recommendations_command;
use crate::slash::session::render_session_command;
use crate::slash::skills::{completion_skill_names, render_skills_command};
use crate::slash::status::{render_stats_command, render_status_command};
use crate::slash::task::render_task_command;

pub(super) fn render_slash_command<W: Write>(
    command: SlashCommand<'_>,
    event: &ShellEvent,
    blocks: &[CommandBlock],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    shell_cwd: Option<&str>,
    output: &mut W,
) -> std::io::Result<bool> {
    // Propagate the shell cwd to the adapter so registry queries
    // (skills/hooks/extensions) spawn cosh-core with the correct
    // --workspace and discover project-level skills and hooks.
    // The Rust intercept path (zsh, or bash with
    // COSH_SLASH_VIA_SHELL=0) creates events with cwd=None because
    // the input never reaches the shell; fall back to the last
    // ShellReady cwd tracked by the dispatcher so registry queries
    // and local health checks resolve the correct project root. If both
    // sources are unavailable the cached cwd may be stale (e.g. a `cd` whose OSC
    // 1337 markers were lost), so clear it rather than forwarding the
    // old project root.
    let shell_cwd = shell_cwd
        .map(str::to_string)
        .or_else(|| state.shell_prompt_cwd.clone());
    let shell_cwd = shell_cwd.as_deref();
    if let AdapterInstance::CoshCore(cosh_core) = adapter {
        match shell_cwd {
            Some(cwd) => cosh_core.set_shell_cwd(Some(cwd)),
            None => cosh_core.clear_shell_cwd(),
        }
    }
    match command {
        SlashCommand::Noop => Ok(true),
        SlashCommand::Auth => {
            crate::auth::runtime::trigger_auth_from_slash(adapter, state, output)?;
            Ok(false)
        }
        SlashCommand::Audit(arguments) => {
            render_audit_command(arguments, state, output)?;
            Ok(true)
        }
        SlashCommand::Help => {
            render_help(state, output)?;
            Ok(true)
        }
        SlashCommand::Agent => {
            let Some((workspace_cwd, skill_names)) =
                agent_composer_context(adapter, state, shell_cwd)
            else {
                render_notice_panel(
                    output,
                    state.i18n().t(MessageId::AgentComposerTitle),
                    vec![state
                        .i18n()
                        .t(MessageId::SlashRegistryUnavailable)
                        .to_string()],
                    None,
                )?;
                return Ok(true);
            };
            crate::runtime::prompt_draft::open_agent_composer(
                state,
                output,
                adapter.name(),
                workspace_cwd.as_deref(),
                skill_names,
            )?;
            // The draft capture owns the terminal until submit/cancel. A
            // restored PTY prompt would sit below the card and corrupt the
            // cursor origin used for every in-place redraw.
            Ok(false)
        }
        SlashCommand::Hooks(sub, arg, extra) => {
            render_hooks_command(sub, arg, extra, blocks, adapter, state, output)?;
            // When a hook id collides between shell and agent layers, the
            // disambiguation panel is now active; withhold the PTY prompt
            // until the user picks a layer.
            if crate::slash::hooks::has_pending_hook_action(state) {
                Ok(false)
            } else {
                Ok(true)
            }
        }
        SlashCommand::Mode(arg, sub, confirm) => {
            render_mode_command(arg, sub, confirm, state, output)
        }
        SlashCommand::Config(sub, value) => render_config_command(sub, value, state, output),
        SlashCommand::Debug(sub) => {
            render_debug_command(sub, adapter, state, output)?;
            Ok(true)
        }
        SlashCommand::Info(command) => {
            render_info(command, state, output)?;
            Ok(true)
        }
        SlashCommand::Removed(command) => {
            render_removed_command(command, state, output)?;
            Ok(true)
        }
        SlashCommand::Hint(prefix) => {
            render_hint(prefix, state, output)?;
            Ok(true)
        }
        SlashCommand::Unknown(command) => {
            render_unknown(command, state, output)?;
            Ok(true)
        }
        SlashCommand::Extensions(args) => {
            render_extensions_command(args, adapter, state, output)?;
            Ok(true)
        }
        SlashCommand::Skills(sub, arg) => {
            render_skills_command(sub, arg, adapter, state, output)?;
            Ok(true)
        }
        SlashCommand::Mcp(sub, arg, extra) => {
            render_mcp_command(sub, arg, extra, adapter, state, output)?;
            Ok(true)
        }
        SlashCommand::Session(arguments) => {
            render_session_command(arguments, blocks, adapter, state, output)
        }
        SlashCommand::Task(arguments) => render_task_command(arguments, state, output),
        SlashCommand::Recommendations(sub, arg, extra) => {
            render_recommendations_command(sub, arg, extra, event, adapter, state, output)?;
            Ok(true)
        }
        SlashCommand::Health => {
            render_health_command(state, shell_cwd, output)?;
            Ok(true)
        }
        SlashCommand::Status => {
            render_status_command(adapter, state, output)?;
            Ok(true)
        }
        SlashCommand::Stats(arguments) => {
            render_stats_command(arguments, adapter, state, output)?;
            Ok(true)
        }
    }
}

fn agent_composer_context(
    adapter: &AdapterInstance,
    state: &InlineState,
    shell_cwd: Option<&str>,
) -> Option<(Option<String>, Vec<String>)> {
    let skill_names = match adapter {
        AdapterInstance::CoshCore(cosh_core) => cosh_core
            .registry_query("skills", "list", serde_json::Value::Null)
            .map(|data| completion_skill_names(&data))
            .unwrap_or_default(),
        AdapterInstance::Fake(_) => Vec::new(),
        _ => return None,
    };
    let workspace_cwd = shell_cwd
        .map(str::to_string)
        .or_else(|| state.shell_prompt_cwd.clone());
    Some((workspace_cwd, skill_names))
}
