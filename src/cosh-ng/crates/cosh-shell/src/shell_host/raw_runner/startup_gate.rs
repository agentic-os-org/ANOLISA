//! Startup readiness for releasing the input relay (#7768).

use super::super::model::ShellHostConfig;
use super::super::osc::OscParser;
use crate::raw_input::MainPromptGate;

/// The relay may start only once the shell sits at a prompt AND the
/// path-routing gate is up. The shell writes the `precmd` marker (or paints
/// the prompt) before the separate `prompt_ready` marker that raises
/// `main_prompt_gate`; releasing in between let startup type-ahead be
/// processed while `is_path_prompt_ready()` was still false, so path-bearing
/// prompts were refused routing and executed by the shell.
pub(super) fn relay_release_ready(
    config: &ShellHostConfig,
    parser: &OscParser,
    main_prompt_gate: &MainPromptGate,
) -> bool {
    let shell_ready = if config.native_mode {
        parser.precmd_count() >= 1
    } else {
        parser.prompt_count(config.prompt.as_bytes()) >= 1
    };
    shell_ready && main_prompt_gate.is_path_prompt_ready()
}
