use std::io::{self, Read};
use std::os::fd::RawFd;
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use super::{current_raw_input_mode, InputRead, RawInputMode};

pub(super) fn read_input_chunks<R>(
    mut input: R,
    sender: SyncSender<InputRead>,
    input_mode: Arc<Mutex<RawInputMode>>,
    input_fd: Option<RawFd>,
) where
    R: Read,
{
    let mut buffer = [0_u8; 8192];
    let mut idle_backoff = Duration::from_millis(2);
    loop {
        let observed_mode = current_raw_input_mode(&input_mode);
        let read_result = input.read(&mut buffer);
        // Compare ownership boundaries instead of full modes: display-only
        // updates inside the same owner (e.g. prompt ghost candidate cycling)
        // must not mark bytes as crossing an ownership cutover.
        let ownership_changed_during_read = current_raw_input_mode(&input_mode).input_ownership()
            != observed_mode.input_ownership();
        let input = match read_result {
            Ok(0) => InputRead::Eof,
            Ok(count) => {
                idle_backoff = Duration::from_millis(2);
                InputRead::Bytes {
                    bytes: buffer[..count].to_vec(),
                    received_at: Instant::now(),
                    observed_mode,
                    ownership_changed_during_read,
                    pending_shell_submits: 0,
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if let Some(fd) = input_fd {
                    if let Err(error) = wait_until_readable(fd) {
                        let _ = sender.send(InputRead::Error(error));
                        return;
                    }
                } else {
                    // Generic test and embedding readers may not expose an
                    // fd. Interactive stdin always takes the poll path.
                    thread::sleep(idle_backoff);
                    idle_backoff = (idle_backoff * 2).min(Duration::from_millis(32));
                }
                continue;
            }
            Err(error) => InputRead::Error(error),
        };
        let done = !matches!(input, InputRead::Bytes { .. });
        if sender.send(input).is_err() || done {
            return;
        }
    }
}

fn wait_until_readable(fd: RawFd) -> io::Result<()> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, -1) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            continue;
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
