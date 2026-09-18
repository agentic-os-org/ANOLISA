use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nix::libc;

use crate::input::{AssistanceControl, InputClassifier};
use crate::raw_input::{
    spawn_raw_action_relay_with_wake, spawn_raw_input_relay_with_wake, MainPromptGate,
    RawInputEvent, RawInputMode, RawInputShellRoute, RawObserverAction, RawRelayAction,
    UserPtyInputGeneration,
};
use crate::types::ShellEvent;

use super::bootstrap::{assistance_state_file, start_bash_session, start_zsh_session, PtySession};
use super::io_loop::{read_until_streaming_with_presentation, wait_child_preserving_signal};
use super::lifecycle::{build_shell_host_output, push_shell_exited_event};
use super::model::{ShellEventView, ShellHostConfig, ShellHostOutput};
use super::prompt_presentation::PromptPresentation;
use super::raw_relay::{read_raw_until_exit, DriverCompletion, RawActionWatchdog};

mod interactive;
pub(super) mod raw_mode_guard;
mod wake;

pub use interactive::{
    run_raw_interactive_bash, run_raw_interactive_bash_with_observer,
    run_raw_interactive_bash_with_output_control, run_raw_interactive_zsh_with_output_control,
};
pub(crate) use interactive::{
    run_raw_interactive_bash_with_event_view, run_raw_interactive_zsh_with_event_view,
};
#[cfg(test)]
use raw_mode_guard::RawModeGuard;
use wake::{notify_relay, RelayWake};

pub fn run_raw_relay_bash<R, W>(
    config: &ShellHostConfig,
    input: R,
    mut output: W,
) -> io::Result<ShellHostOutput>
where
    R: Read + Send + 'static,
    W: Write,
{
    run_raw_relay_bash_with_observer(config, input, &mut output, |_, _| Ok(()))
}

pub fn run_raw_relay_bash_with_observer<R, W, F>(
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
    let mut event_observer = event_observer;
    run_raw_relay_bash_with_output_control(config, input, output, move |events, output| {
        event_observer(events, output)?;
        Ok(RawObserverAction::Continue)
    })
}

pub fn run_raw_relay_bash_with_output_control<R, W, F>(
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
    let mut event_observer = event_observer;
    run_raw_relay_bash_with_output_control_and_input_fd(
        config,
        input,
        output,
        move |view, output| event_observer(view.events(), output),
        None,
    )
}

fn run_raw_relay_bash_with_output_control_and_input_fd<R, W, F>(
    config: &ShellHostConfig,
    input: R,
    output: W,
    event_observer: F,
    input_fd: Option<RawFd>,
) -> io::Result<ShellHostOutput>
where
    R: Read + Send + 'static,
    W: Write,
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
{
    run_raw_relay_with_driver(
        config,
        start_bash_session,
        output,
        event_observer,
        config.input_classifier.clone(),
        None,
        true,
        |master,
         _,
         input_events,
         input_classifier,
         input_mode,
         input_generation,
         shell_route,
         wake| {
            spawn_raw_input_relay_with_wake(
                input,
                master,
                input_events,
                input_classifier,
                input_mode,
                input_generation,
                shell_route,
                input_fd,
                Some(wake),
            )
        },
    )
}

pub fn run_raw_relay_zsh_with_output_control<R, W, F>(
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
    let mut event_observer = event_observer;
    run_raw_relay_zsh_with_output_control_and_input_fd(
        config,
        input,
        output,
        move |view, output| event_observer(view.events(), output),
        None,
    )
}

fn run_raw_relay_zsh_with_output_control_and_input_fd<R, W, F>(
    config: &ShellHostConfig,
    input: R,
    output: W,
    event_observer: F,
    input_fd: Option<RawFd>,
) -> io::Result<ShellHostOutput>
where
    R: Read + Send + 'static,
    W: Write,
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
{
    run_raw_relay_with_driver(
        config,
        start_zsh_session,
        output,
        event_observer,
        config.input_classifier.clone(),
        None,
        false,
        |master,
         _,
         input_events,
         input_classifier,
         input_mode,
         input_generation,
         shell_route,
         wake| {
            spawn_raw_input_relay_with_wake(
                input,
                master,
                input_events,
                input_classifier,
                input_mode,
                input_generation,
                shell_route,
                input_fd,
                Some(wake),
            )
        },
    )
}

pub fn run_raw_relay_bash_with_actions<W>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
) -> io::Result<ShellHostOutput>
where
    W: Write,
{
    run_raw_relay_bash_with_actions_observer(config, actions, output, |_, _| Ok(()))
}

pub fn run_raw_relay_zsh_with_actions<W>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
) -> io::Result<ShellHostOutput>
where
    W: Write,
{
    run_raw_relay_with_driver(
        config,
        start_zsh_session,
        output,
        |_, _| Ok(RawObserverAction::Continue),
        config.input_classifier.clone(),
        Some(config.raw_action_watchdog),
        false,
        |master,
         child_pid,
         input_events,
         input_classifier,
         input_mode,
         input_generation,
         shell_route,
         wake| {
            spawn_raw_action_relay_with_wake(
                actions,
                master,
                child_pid,
                input_events,
                input_classifier,
                input_mode,
                input_generation,
                shell_route,
                Some(wake),
            )
        },
    )
}

pub fn run_raw_relay_bash_with_actions_observer<W, F>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    W: Write,
    F: FnMut(&[ShellEvent], &mut W) -> io::Result<()>,
{
    let mut event_observer = event_observer;
    run_raw_relay_with_driver(
        config,
        start_bash_session,
        output,
        move |view, output| {
            event_observer(view.events(), output)?;
            Ok(RawObserverAction::Continue)
        },
        config.input_classifier.clone(),
        Some(config.raw_action_watchdog),
        true,
        |master,
         child_pid,
         input_events,
         input_classifier,
         input_mode,
         input_generation,
         shell_route,
         wake| {
            spawn_raw_action_relay_with_wake(
                actions,
                master,
                child_pid,
                input_events,
                input_classifier,
                input_mode,
                input_generation,
                shell_route,
                Some(wake),
            )
        },
    )
}

pub fn run_raw_relay_bash_with_actions_output_control<W, F>(
    config: &ShellHostConfig,
    actions: Vec<RawRelayAction>,
    output: W,
    event_observer: F,
) -> io::Result<ShellHostOutput>
where
    W: Write,
    F: FnMut(&[ShellEvent], &mut W) -> io::Result<RawObserverAction>,
{
    let mut event_observer = event_observer;
    run_raw_relay_with_driver(
        config,
        start_bash_session,
        output,
        move |view, output| event_observer(view.events(), output),
        config.input_classifier.clone(),
        Some(config.raw_action_watchdog),
        true,
        |master,
         child_pid,
         input_events,
         input_classifier,
         input_mode,
         input_generation,
         shell_route,
         wake| {
            spawn_raw_action_relay_with_wake(
                actions,
                master,
                child_pid,
                input_events,
                input_classifier,
                input_mode,
                input_generation,
                shell_route,
                Some(wake),
            )
        },
    )
}

fn run_raw_relay_with_driver<W, F, D>(
    config: &ShellHostConfig,
    start_session: fn(&ShellHostConfig) -> io::Result<PtySession>,
    mut output: W,
    mut event_observer: F,
    input_classifier: InputClassifier,
    action_watchdog: Option<Duration>,
    bounded_bash_handoff: bool,
    spawn_driver: D,
) -> io::Result<ShellHostOutput>
where
    W: Write,
    F: FnMut(ShellEventView<'_>, &mut W) -> io::Result<RawObserverAction>,
    D: FnOnce(
        File,
        u32,
        Sender<RawInputEvent>,
        InputClassifier,
        Arc<Mutex<RawInputMode>>,
        UserPtyInputGeneration,
        RawInputShellRoute,
        UnixStream,
    ) -> JoinHandle<io::Result<()>>,
{
    let assistance_control = config.integration.uses_markers().then(|| {
        config
            .assistance_control
            .clone()
            .unwrap_or_else(|| AssistanceControl::enabled(assistance_state_file(config)))
    });
    let input_classifier = match assistance_control.as_ref() {
        Some(control) => input_classifier.with_assistance_control(control.clone()),
        None => input_classifier,
    };
    let mut session = start_session(config)?;
    session.parser.set_prompt_cwd(input_classifier.prompt_cwd());
    session
        .parser
        .set_shell_path_command_names(input_classifier.shell_path_command_names());
    let mut prompt_presentation = PromptPresentation::new();
    // Attach the gate before startup output consumes the first prompt_ready marker.
    let main_prompt_gate = MainPromptGate::default();
    session
        .parser
        .set_main_prompt_gate(main_prompt_gate.clone());
    let input_generation = UserPtyInputGeneration::default();
    session
        .parser
        .set_prompt_epoch_exchange(input_generation.prompt_epoch_exchange());
    if let Some(control) = assistance_control.as_ref() {
        session.parser.set_assistance_control(control.clone());
    }
    if config.integration.uses_markers() {
        read_until_streaming_with_presentation(
            &mut session.master,
            &mut session.child,
            &mut session.parser,
            &mut output,
            &mut prompt_presentation,
            Duration::from_secs(5),
            |parser| {
                if config.native_mode {
                    parser.precmd_count() >= 1
                } else {
                    parser.prompt_count(config.prompt.as_bytes()) >= 1
                }
            },
        )?;
    }
    let input_master = session.master.try_clone()?;
    let (input_event_sender, input_event_receiver) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    // #1721/#1718: prompt_ready raises the gated route; submits lower it.
    let slash_route_enabled = bounded_bash_handoff
        && config.integration.uses_markers()
        && config.slash_via_shell
        && config.native_mode;
    session.parser.publish_quiescent_prompt_snapshot();
    let (mut wake_reader, wake_writer, mut resize_reader, _resize_wake) =
        RelayWake::new()?.into_parts();
    // Keep the channel open after the driver and completion notifier exit;
    // otherwise POLLHUP would keep the relay readable until the child exits.
    let _wake_keepalive = wake_writer.try_clone()?;
    let mut completion_wake = wake_writer.try_clone()?;
    let driver_thread = spawn_driver(
        input_master,
        session.child.id(),
        input_event_sender,
        input_classifier
            .with_shell_passthrough(!config.integration.uses_markers())
            .with_bash_readline_history_privacy(
                bounded_bash_handoff && config.integration.uses_markers(),
            )
            .with_bash_slash_submission_guard(slash_route_enabled),
        Arc::clone(&input_mode),
        input_generation.clone(),
        RawInputShellRoute::new(
            main_prompt_gate,
            slash_route_enabled,
            session.zsh_path_prompt_buffering.take(),
        ),
        wake_writer,
    );
    let (driver_completion_sender, driver_completion_receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = driver_thread
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("raw input driver panicked")));
        let _ = driver_completion_sender.send(DriverCompletion {
            result,
            completed_at: Instant::now(),
        });
        notify_relay(&mut completion_wake);
    });
    let watchdog = action_watchdog.map(RawActionWatchdog::new);
    let mut last_winsize = config.winsize;
    let relay_prompt = if config.native_mode || !config.integration.uses_markers() {
        ""
    } else {
        &config.prompt
    };
    let eof_shutdown = read_raw_until_exit(
        &mut session.master,
        &session.terminal,
        &mut session.child,
        &mut session.parser,
        &mut prompt_presentation,
        &mut output,
        &mut event_observer,
        &input_event_receiver,
        &driver_completion_receiver,
        &mut wake_reader,
        &mut resize_reader,
        &input_mode,
        &input_generation,
        &mut last_winsize,
        relay_prompt,
        &session.recovery_request_file,
        &session.handoff_request_file,
        bounded_bash_handoff,
        watchdog.as_ref(),
        &config.input_wait_status,
        &crate::i18n::I18n::new(config.hint_language),
        config.input_wait_timeout_secs,
        config.hint_card_renderer.as_ref(),
    )?;
    let display_start = session.parser.display_position();
    session.parser.flush_pending()?;
    prompt_presentation.observe(&mut session.parser);
    prompt_presentation.write_range(
        &session.parser,
        display_start,
        session.parser.display_position(),
        &mut output,
    )?;
    output.flush()?;

    let exit_status = wait_child_preserving_signal(&mut session.child, eof_shutdown)?;
    push_shell_exited_event(&mut session.parser, config, exit_status)?;
    session
        .parser
        .observe_events(&mut output, &mut event_observer)?;
    output.flush()?;
    build_shell_host_output(config, session.parser, exit_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn raw_mode_guard_restores_echo_and_canonical_mode() {
        let pty = nix::pty::openpty(None, None).expect("open pty");
        let fd = pty.slave.as_raw_fd();
        let original = termios_for_fd(fd);

        {
            let _guard = RawModeGuard::activate_fd(fd)
                .expect("activate raw mode")
                .expect("pty is tty");
            let raw = termios_for_fd(fd);
            assert_eq!(raw.c_lflag & libc::ECHO, 0);
            assert_eq!(raw.c_lflag & libc::ICANON, 0);
        }

        let restored = termios_for_fd(fd);
        assert_eq!(restored.c_lflag & libc::ECHO, original.c_lflag & libc::ECHO);
        assert_eq!(
            restored.c_lflag & libc::ICANON,
            original.c_lflag & libc::ICANON
        );
    }

    #[test]
    fn raw_mode_guard_restores_nonblocking_flag_for_pipe_input() {
        let pipe = nix::unistd::pipe().expect("open pipe");
        let fd = pipe.0.as_raw_fd();
        let original = unsafe { libc::fcntl(fd, libc::F_GETFL) };

        {
            let _guard = RawModeGuard::activate_fd(fd)
                .expect("activate input mode")
                .expect("pipe guard");
            let active = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            assert_ne!(active & libc::O_NONBLOCK, 0);
        }

        let restored = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert_eq!(restored & libc::O_NONBLOCK, original & libc::O_NONBLOCK);
    }

    #[test]
    fn raw_mode_guard_disable_write_survives_inherited_nonblocking_full_buffer() {
        // PoC: inherited O_NONBLOCK + full buffer would lose the disable
        // sequence to EAGAIN. A pipe gives deterministic "buffer full"; the
        // guard is built with original_flags containing O_NONBLOCK and a fake
        // termios so the cleanup write path runs. Without the fix the write
        // returns EAGAIN; with the fix O_NONBLOCK is cleared, the write blocks
        // until the drain thread frees space, and the disable sequence is
        // delivered.
        use std::thread;
        use std::time::Duration;

        let (read_fd_owned, write_fd_owned) = nix::unistd::pipe().expect("open pipe");
        let read_fd = read_fd_owned.as_raw_fd();
        let write_fd = write_fd_owned.as_raw_fd();

        let original = unsafe { libc::fcntl(write_fd, libc::F_GETFL) };
        unsafe { libc::fcntl(write_fd, libc::F_SETFL, original | libc::O_NONBLOCK) };
        let chunk = [0_u8; 8192];
        while unsafe { libc::write(write_fd, chunk.as_ptr().cast(), chunk.len()) } >= 0 {}
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN),
            "expected EAGAIN when the pipe buffer is full"
        );

        let fake_termios = unsafe { std::mem::zeroed::<libc::termios>() };
        let guard = RawModeGuard::for_test(
            write_fd,
            write_fd,
            Some(fake_termios),
            original | libc::O_NONBLOCK,
        );

        let drain_handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            let mut all = Vec::new();
            let mut buf = [0_u8; 4096];
            loop {
                let n = unsafe { libc::read(read_fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                all.extend_from_slice(&buf[..n as usize]);
            }
            all
        });

        drop(guard);
        drop(write_fd_owned);
        let drained = drain_handle.join().expect("drain thread");
        let output = String::from_utf8_lossy(&drained);
        assert!(
            output.contains("\x1b[>4;0m"),
            "disable sequence not found in pipe output: {output:?}"
        );
    }

    #[test]
    fn raw_mode_guard_disable_write_targets_output_fd() {
        // The withdrawal must travel on the output path that carried the
        // enable, not on the input fd: a read-only or separately opened
        // stdin never delivers bytes to the terminal. Input and output are
        // separate pipes standing in for stdin and stdout; the guard is
        // built with a fake termios so the cleanup write path runs. The
        // disable sequence must land on the output pipe and nothing may be
        // written to the input pipe.
        let (input_read_owned, input_write_owned) = nix::unistd::pipe().expect("open input pipe");
        let (output_read_owned, output_write_owned) =
            nix::unistd::pipe().expect("open output pipe");
        let input_read = input_read_owned.as_raw_fd();
        let input_write = input_write_owned.as_raw_fd();
        let output_read = output_read_owned.as_raw_fd();
        let output_write = output_write_owned.as_raw_fd();

        let original = unsafe { libc::fcntl(input_write, libc::F_GETFL) };
        let fake_termios = unsafe { std::mem::zeroed::<libc::termios>() };
        let guard = RawModeGuard::for_test(input_write, output_write, Some(fake_termios), original);
        drop(guard);

        // The drop above already wrote synchronously, so a non-blocking read
        // either sees the sequence or fails immediately instead of hanging
        // when the write went to the wrong fd.
        let output_flags = unsafe { libc::fcntl(output_read, libc::F_GETFL) };
        unsafe { libc::fcntl(output_read, libc::F_SETFL, output_flags | libc::O_NONBLOCK) };
        let mut buf = [0_u8; 64];
        let n = unsafe { libc::read(output_read, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n > 0, "expected disable sequence on the output fd");
        assert_eq!(&buf[..n as usize], b"\x1b[>4;0m");

        let input_flags = unsafe { libc::fcntl(input_read, libc::F_GETFL) };
        unsafe { libc::fcntl(input_read, libc::F_SETFL, input_flags | libc::O_NONBLOCK) };
        let n = unsafe { libc::read(input_read, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(
            n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN),
            "expected no bytes written to the input fd, got {:?}",
            &buf[..n.max(0) as usize]
        );
    }

    #[test]
    fn raw_mode_guard_full_non_tty_output_does_not_block_exit() {
        // With stdout on a pipe the enable never reached a terminal, so the
        // withdrawal must not block shell exit when that pipe is full and
        // its reader never drains: the write is best-effort non-blocking on
        // non-tty outputs. The guard drops on a worker thread so a
        // regression to a blocking write surfaces as a fast timeout here
        // instead of a hang.
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let (_input_read_owned, input_write_owned) = nix::unistd::pipe().expect("open input pipe");
        let (output_read_owned, output_write_owned) =
            nix::unistd::pipe().expect("open output pipe");
        let input_write = input_write_owned.as_raw_fd();
        let output_write = output_write_owned.as_raw_fd();

        let output_flags = unsafe { libc::fcntl(output_write, libc::F_GETFL) };
        unsafe { libc::fcntl(output_write, libc::F_SETFL, output_flags | libc::O_NONBLOCK) };
        let chunk = [0_u8; 8192];
        while unsafe { libc::write(output_write, chunk.as_ptr().cast(), chunk.len()) } >= 0 {}
        unsafe { libc::fcntl(output_write, libc::F_SETFL, output_flags) };

        let original = unsafe { libc::fcntl(input_write, libc::F_GETFL) };
        let fake_termios = unsafe { std::mem::zeroed::<libc::termios>() };
        let guard = RawModeGuard::for_test(input_write, output_write, Some(fake_termios), original);

        let (done_tx, done_rx) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(guard);
            done_tx.send(()).expect("send drop completion");
        });
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("guard drop must not block on a full non-tty output");
        dropper.join().expect("join dropper thread");
        drop(output_read_owned);
    }

    fn termios_for_fd(fd: i32) -> libc::termios {
        let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut termios) }, 0);
        termios
    }
}
