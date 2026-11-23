use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use super::super::event_sender::WakingRawInputEventSender;
use super::super::RawInputEvent;
use super::capture::{replay_mode_accepts_interrupt, REPLAY_CTRL_C, REPLAY_EOF, REPLAY_IDLE};
use super::{
    current_raw_input_mode, InputRead, RawInputMode, RawInputRelayState, INPUT_READ_AHEAD_CAPACITY,
};
use std::sync::atomic::{AtomicU8, Ordering};

pub(super) fn spawn_input_reader<R>(
    input: R,
    input_events: Sender<RawInputEvent>,
    input_mode: &Arc<Mutex<RawInputMode>>,
    input_fd: Option<RawFd>,
    wake: Option<UnixStream>,
    state: &RawInputRelayState,
) -> io::Result<(
    Receiver<InputRead>,
    WakingRawInputEventSender,
    Option<UnixStream>,
)>
where
    R: Read + Send + 'static,
{
    let reader_wake = wake.as_ref().map(UnixStream::try_clone).transpose()?;
    let replay_wake = wake.as_ref().map(UnixStream::try_clone).transpose()?;
    let input_events = WakingRawInputEventSender::new(input_events, wake);
    let (read_tx, read_rx) = mpsc::sync_channel(INPUT_READ_AHEAD_CAPACITY);
    // The relay must wake without a later keystroke to resolve a bare ESC.
    let reader_input_mode = input_mode.clone();
    let replay_control = state.capture_owned_input.replay_control.clone();
    thread::spawn(move || {
        read_input_chunks(
            input,
            read_tx,
            reader_input_mode,
            input_fd,
            reader_wake,
            Some(replay_control),
        )
    });
    Ok((read_rx, input_events, replay_wake))
}

pub(super) fn read_input_chunks<R>(
    mut input: R,
    sender: SyncSender<InputRead>,
    input_mode: Arc<Mutex<RawInputMode>>,
    input_fd: Option<RawFd>,
    mut wake: Option<UnixStream>,
    replay_control: Option<Arc<AtomicU8>>,
) where
    R: Read,
{
    let mut buffer = [0_u8; 8192];
    let mut idle_backoff = Duration::from_millis(2);
    loop {
        let nonblocking = input_fd.is_some_and(|fd| {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            flags >= 0 && flags & libc::O_NONBLOCK != 0
        });
        let (observed_mode, read_result, ownership_changed_during_read) = if nonblocking {
            let Ok(mode) = input_mode.lock() else {
                let _ = sender.send(InputRead::Error(io::Error::other(
                    "raw input mode lock poisoned",
                )));
                return;
            };
            let observed_mode = mode.clone();
            let read_result = input.read(&mut buffer);
            (observed_mode, read_result, false)
        } else {
            let observed_mode = current_raw_input_mode(&input_mode);
            let read_result = input.read(&mut buffer);
            let changed = current_raw_input_mode(&input_mode).input_ownership()
                != observed_mode.input_ownership();
            (observed_mode, read_result, changed)
        };
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
                    match wait_until_readable(fd, wake.as_mut()) {
                        Ok(true) => {
                            if sender.send(InputRead::CaptureInvalidated).is_err() {
                                return;
                            }
                        }
                        Ok(false) => {}
                        Err(error) => {
                            let _ = sender.send(InputRead::Error(error));
                            return;
                        }
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
        if let Some(control) = replay_control.as_ref() {
            if control.load(Ordering::Acquire) != REPLAY_IDLE {
                match &input {
                    InputRead::Eof => control.store(REPLAY_EOF, Ordering::Release),
                    InputRead::Bytes {
                        bytes,
                        observed_mode,
                        ownership_changed_during_read: false,
                        ..
                    } if replay_mode_accepts_interrupt(observed_mode)
                        && bytes.contains(&super::super::CTRL_C) =>
                    {
                        let mut current = control.load(Ordering::Acquire);
                        while current != REPLAY_EOF && current != REPLAY_IDLE {
                            match control.compare_exchange_weak(
                                current,
                                REPLAY_CTRL_C,
                                Ordering::AcqRel,
                                Ordering::Acquire,
                            ) {
                                Ok(_) => break,
                                Err(observed) => current = observed,
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let done = !matches!(input, InputRead::Bytes { .. });
        if sender.send(input).is_err() || done {
            return;
        }
    }
}

#[cfg(test)]
type WakeRead = Box<dyn FnOnce(&mut UnixStream, &mut [u8]) -> io::Result<usize>>;

#[cfg(test)]
thread_local! {
    static WAKE_READ: std::cell::RefCell<Option<WakeRead>> = const { std::cell::RefCell::new(None) };
}

fn wait_until_readable(fd: RawFd, mut wake: Option<&mut UnixStream>) -> io::Result<bool> {
    let mut poll_fds = [
        libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_ref().map_or(-1, |stream| stream.as_raw_fd()),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    loop {
        let result = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, -1) };
        if result > 0 {
            if poll_fds[1].revents & libc::POLLIN != 0 {
                let mut bytes = [0; 64];
                if let Some(stream) = wake.as_mut() {
                    #[cfg(test)]
                    let read_result = match WAKE_READ.with(|slot| slot.borrow_mut().take()) {
                        Some(read) => read(stream, &mut bytes),
                        None => stream.read(&mut bytes),
                    };
                    #[cfg(not(test))]
                    let read_result = stream.read(&mut bytes);
                    match read_result {
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(error) => return Err(error),
                    }
                }
                return Ok(true);
            }
            return Ok(false);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;

    #[test]
    fn competing_replay_waiter_can_consume_the_readers_wakeup() {
        let (input, _input_writer) = UnixStream::pair().unwrap();
        let (mut wake, mut wake_writer) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        let mut competing_waiter = wake.try_clone().unwrap();
        wake_writer.write_all(&[1]).unwrap();
        WAKE_READ.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move |stream, bytes| {
                let mut consumed = [0; 1];
                competing_waiter.read_exact(&mut consumed).unwrap();
                assert_eq!(consumed, [1]);
                stream.read(bytes)
            }));
        });

        assert!(wait_until_readable(input.as_raw_fd(), Some(&mut wake))
            .expect("a competing waiter must not terminate the input reader"));
        wake_writer.write_all(&[2]).unwrap();
        assert!(wait_until_readable(input.as_raw_fd(), Some(&mut wake)).unwrap());
        assert_eq!(
            wake.read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn interrupted_wake_read_keeps_the_input_reader_available() {
        let (input, _input_writer) = UnixStream::pair().unwrap();
        let (mut wake, mut wake_writer) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        wake_writer.write_all(&[1]).unwrap();
        WAKE_READ.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|_, _| Err(io::ErrorKind::Interrupted.into())));
        });

        assert!(wait_until_readable(input.as_raw_fd(), Some(&mut wake))
            .expect("an interrupted wake read must not terminate the input reader"));
        assert!(wait_until_readable(input.as_raw_fd(), Some(&mut wake)).unwrap());
        assert_eq!(
            wake.read(&mut [0; 1]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn permanent_wake_read_errors_are_reported() {
        let (input, _input_writer) = UnixStream::pair().unwrap();
        let (mut wake, mut wake_writer) = UnixStream::pair().unwrap();
        wake.set_nonblocking(true).unwrap();
        wake_writer.write_all(&[1]).unwrap();
        WAKE_READ.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(|_, _| Err(io::ErrorKind::PermissionDenied.into())));
        });

        assert_eq!(
            wait_until_readable(input.as_raw_fd(), Some(&mut wake))
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn capture_invalidation_wakes_idle_reader_without_user_bytes() {
        let (input, mut writer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        let input_fd = input.as_raw_fd();
        let (wake_reader, mut wake_writer) = UnixStream::pair().unwrap();
        wake_reader.set_nonblocking(true).unwrap();
        let (sender, receiver) = mpsc::sync_channel(3);
        let mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
        let reader = std::thread::spawn(move || {
            read_input_chunks(input, sender, mode, Some(input_fd), Some(wake_reader), None);
        });
        wake_writer.write_all(&[1]).unwrap();
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            InputRead::CaptureInvalidated
        ));
        writer.write_all(b"fresh\n").unwrap();
        drop(writer);
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            InputRead::Bytes { bytes, .. } if bytes == b"fresh\n"
        ));
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap(),
            InputRead::Eof
        ));
        reader.join().unwrap();
    }

    #[test]
    fn nonblocking_reader_samples_owner_and_bytes_under_one_lock() {
        struct CheckedReader {
            stream: UnixStream,
            mode: Arc<Mutex<RawInputMode>>,
        }
        impl Read for CheckedReader {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                assert!(
                    self.mode.try_lock().is_err(),
                    "owner cutover must not cross read"
                );
                self.stream.read(bytes)
            }
        }
        let (stream, mut writer) = UnixStream::pair().unwrap();
        stream.set_nonblocking(true).unwrap();
        writer.write_all(b"fresh\n").unwrap();
        drop(writer);
        let fd = stream.as_raw_fd();
        let mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
        let reader = CheckedReader {
            stream,
            mode: mode.clone(),
        };
        let (tx, rx) = mpsc::sync_channel(3);
        read_input_chunks(reader, tx, mode, Some(fd), None, None);
        match rx.recv().unwrap() {
            InputRead::Bytes {
                bytes,
                observed_mode,
                ownership_changed_during_read,
                ..
            } => {
                assert_eq!(bytes, b"fresh\n");
                assert_eq!(observed_mode, RawInputMode::Passthrough);
                assert!(!ownership_changed_during_read);
            }
            _ => panic!("expected first fresh input"),
        }
        assert!(matches!(rx.recv().unwrap(), InputRead::Eof));
    }
}
