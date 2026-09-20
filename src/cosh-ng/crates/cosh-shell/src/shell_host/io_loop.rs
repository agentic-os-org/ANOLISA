use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};
use wait_timeout::ChildExt;

use crate::raw_input::{foreground_process_group_for_fds, signal_process_group_id};

use super::osc::OscParser;
use super::prompt_presentation::PromptPresentation;

pub(super) fn read_until(
    master: &mut File,
    child: &mut Child,
    parser: &mut OscParser,
    timeout: Duration,
    condition: impl Fn(&OscParser) -> bool,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut buffer = [0_u8; 8192];

    while Instant::now() < deadline {
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    parser.feed(&buffer[..n])?;
                    if condition(parser) {
                        return Ok(true);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) if child.try_wait()?.is_some() => return Ok(condition(parser)),
                Err(err) => return Err(err),
            }
        }

        if child.try_wait()?.is_some() {
            return Ok(condition(parser));
        }
        thread::sleep(Duration::from_millis(10));
    }

    Ok(condition(parser))
}

pub(super) fn read_until_streaming<W: Write>(
    master: &mut File,
    child: &mut Child,
    parser: &mut OscParser,
    output: &mut W,
    timeout: Duration,
    condition: impl Fn(&OscParser) -> bool,
) -> io::Result<bool> {
    read_until_streaming_with_presentation(
        master,
        child,
        parser,
        output,
        &mut PromptPresentation::new(false),
        timeout,
        condition,
    )
}

pub(super) fn read_until_streaming_with_presentation<W: Write>(
    master: &mut File,
    child: &mut Child,
    parser: &mut OscParser,
    output: &mut W,
    prompt_presentation: &mut PromptPresentation,
    timeout: Duration,
    condition: impl Fn(&OscParser) -> bool,
) -> io::Result<bool> {
    let deadline = Instant::now() + timeout;
    let mut buffer = [0_u8; 8192];
    let mut display_start = parser.display_position();

    while Instant::now() < deadline {
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    parser.feed(&buffer[..n])?;
                    prompt_presentation.observe(parser);
                    if parser.display_position() > display_start {
                        let display_end = parser.display_position();
                        prompt_presentation.write_range(
                            parser,
                            display_start,
                            display_end,
                            output,
                        )?;
                        output.flush()?;
                        display_start = display_end;
                    }
                    if condition(parser) {
                        return Ok(true);
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) if child.try_wait()?.is_some() => return Ok(condition(parser)),
                Err(err) => return Err(err),
            }
        }

        if child.try_wait()?.is_some() {
            return Ok(condition(parser));
        }
        thread::sleep(Duration::from_millis(10));
    }

    Ok(condition(parser))
}

pub(super) fn wait_pty_foreground_bounded(
    master: &File,
    terminal: &File,
    child: &mut Child,
    timeout: Duration,
) -> io::Result<Option<i32>> {
    if let Some(status) = child.wait_timeout(timeout)? {
        return Ok(status.code());
    }

    let shell_group = child.id() as i32;
    let mut process_groups =
        foreground_process_group_for_fds(master.as_raw_fd(), terminal.as_raw_fd())
            .into_iter()
            .collect::<Vec<_>>();
    if !process_groups.contains(&shell_group) {
        process_groups.push(shell_group);
    }
    let mut signal_error = None;
    for process_group in process_groups {
        if let Err(error) = signal_process_group_id(process_group, nix::libc::SIGKILL) {
            signal_error.get_or_insert(error);
        }
    }
    let _ = child.kill();
    child.wait()?;
    if let Some(error) = signal_error {
        return Err(error);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("shell did not exit within {timeout:?}"),
    ))
}

#[cfg(unix)]
pub(super) fn wait_child_preserving_signal(
    child: &mut Child,
    normalize_signal: bool,
) -> io::Result<Option<i32>> {
    use std::os::unix::process::ExitStatusExt;

    let status = match child.try_wait()? {
        Some(status) => status,
        None => child.wait()?,
    };
    Ok(status
        .code()
        .or_else(|| normalize_signal.then(|| status.signal().map(|signal| 128 + signal))?))
}
