#[allow(dead_code, unused_imports)]
#[path = "mod.rs"]
mod implementation;

pub use implementation::{
    run_line_interactive_bash, run_raw_relay_bash, run_raw_relay_bash_with_actions,
    run_raw_relay_bash_with_actions_output_control, run_raw_relay_bash_with_observer,
    run_raw_relay_zsh_with_actions, run_raw_relay_zsh_with_output_control, run_scripted_bash,
    run_scripted_zsh, LineInteractiveOutput, ScriptedInput, ShellHostConfig, ShellHostOutput,
    ShellIntegration,
};

pub(crate) use implementation::run_streaming_line_bash;
pub(crate) use implementation::HintCardRenderer;
