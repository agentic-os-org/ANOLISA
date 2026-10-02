mod adapter;
mod auth;
mod bootstrap;
mod input_intent;
mod io_loop;
mod lifecycle;
mod line_interactive;
mod marker;
mod model;
#[cfg(test)]
mod model_tests;
mod osc;
mod osc_output;
#[cfg(test)]
mod osc_tests;
mod prompt_presentation;
mod prompt_replay;
mod raw_relay;
mod raw_runner;
mod routing;
mod scripted;
pub(crate) mod sigpipe;
mod transcript;

pub(crate) use bootstrap::{assistance_state_file, spawn_profile_probe_on_pty};
pub use line_interactive::{run_line_interactive_bash, LineInteractiveOutput};
pub(crate) use model::{HintCardRenderer, ShellEventView};
pub use model::{ScriptedInput, ShellHostConfig, ShellHostOutput, ShellIntegration};
pub(crate) use raw_relay::interactive_sentinel::InputWaitStatus;
pub(crate) use raw_runner::raw_mode_guard::restore_raw_mode_signal_state;
pub use raw_runner::{
    run_raw_interactive_bash, run_raw_interactive_bash_with_observer,
    run_raw_interactive_bash_with_output_control, run_raw_interactive_zsh_with_output_control,
    run_raw_relay_bash, run_raw_relay_bash_with_actions, run_raw_relay_bash_with_actions_observer,
    run_raw_relay_bash_with_actions_output_control, run_raw_relay_bash_with_observer,
    run_raw_relay_bash_with_output_control, run_raw_relay_zsh_with_actions,
    run_raw_relay_zsh_with_output_control,
};
pub(crate) use raw_runner::{
    run_raw_interactive_bash_with_event_view, run_raw_interactive_zsh_with_event_view,
};
pub use scripted::{run_scripted_bash, run_scripted_zsh, run_streaming_line_bash};
