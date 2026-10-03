//! Real-terminal entry points for the raw bash and zsh relays.

use std::io;

use nix::libc;

use crate::raw_input::RawObserverAction;
use crate::types::ShellEvent;

use super::raw_mode_guard::{reopen_stdout_blocking, RawModeGuard};
use super::{
    run_raw_relay_bash_with_output_control_and_input_fd,
    run_raw_relay_zsh_with_output_control_and_input_fd, ShellEventView, ShellHostConfig,
    ShellHostOutput,
};

pub fn run_raw_interactive_bash(config: &ShellHostConfig) -> io::Result<ShellHostOutput> {
    let _raw_mode = RawModeGuard::activate_stdin()?;
    reopen_stdout_blocking()?;
    run_raw_relay_bash_with_output_control_and_input_fd(
        config,
        io::stdin(),
        io::stdout(),
        |_, _| Ok(RawObserverAction::Continue),
        Some(libc::STDIN_FILENO),
    )
}

pub fn run_raw_interactive_bash_with_observer<F>(
    config: &ShellHostConfig,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    F: FnMut(&[ShellEvent], &mut io::Stdout) -> io::Result<()>,
{
    let _raw_mode = RawModeGuard::activate_stdin()?;
    reopen_stdout_blocking()?;
    let mut event_observer = event_observer;
    run_raw_relay_bash_with_output_control_and_input_fd(
        config,
        io::stdin(),
        io::stdout(),
        move |view, output| {
            event_observer(view.events(), output)?;
            Ok(RawObserverAction::Continue)
        },
        Some(libc::STDIN_FILENO),
    )
}

pub fn run_raw_interactive_bash_with_output_control<F>(
    config: &ShellHostConfig,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    F: FnMut(&[ShellEvent], &mut io::Stdout) -> io::Result<RawObserverAction>,
{
    let mut event_observer = event_observer;
    let _raw_mode = RawModeGuard::activate_stdin()?;
    reopen_stdout_blocking()?;
    run_raw_relay_bash_with_output_control_and_input_fd(
        config,
        io::stdin(),
        io::stdout(),
        move |view, output| event_observer(view.events(), output),
        Some(libc::STDIN_FILENO),
    )
}

pub fn run_raw_interactive_zsh_with_output_control<F>(
    config: &ShellHostConfig,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    F: FnMut(&[ShellEvent], &mut io::Stdout) -> io::Result<RawObserverAction>,
{
    let mut event_observer = event_observer;
    let _raw_mode = RawModeGuard::activate_stdin()?;
    reopen_stdout_blocking()?;
    run_raw_relay_zsh_with_output_control_and_input_fd(
        config,
        io::stdin(),
        io::stdout(),
        move |view, output| event_observer(view.events(), output),
        Some(libc::STDIN_FILENO),
    )
}

pub(crate) fn run_raw_interactive_bash_with_event_view<F>(
    config: &ShellHostConfig,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    F: FnMut(ShellEventView<'_>, &mut io::Stdout) -> io::Result<RawObserverAction>,
{
    let _raw_mode = RawModeGuard::activate_stdin()?;
    reopen_stdout_blocking()?;
    run_raw_relay_bash_with_output_control_and_input_fd(
        config,
        io::stdin(),
        io::stdout(),
        event_observer,
        Some(libc::STDIN_FILENO),
    )
}

pub(crate) fn run_raw_interactive_zsh_with_event_view<F>(
    config: &ShellHostConfig,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    F: FnMut(ShellEventView<'_>, &mut io::Stdout) -> io::Result<RawObserverAction>,
{
    let _raw_mode = RawModeGuard::activate_stdin()?;
    reopen_stdout_blocking()?;
    run_raw_relay_zsh_with_output_control_and_input_fd(
        config,
        io::stdin(),
        io::stdout(),
        event_observer,
        Some(libc::STDIN_FILENO),
    )
}
