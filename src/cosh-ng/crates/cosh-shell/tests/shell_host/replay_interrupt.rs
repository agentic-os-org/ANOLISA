use super::*;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

struct ReplayInput {
    chunks: Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
    read: Arc<AtomicUsize>,
}

impl Read for ReplayInput {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.pending.is_empty() {
            match self.chunks.try_recv() {
                Ok(chunk) => self.pending.extend(chunk),
                Err(mpsc::TryRecvError::Empty) => return Err(io::ErrorKind::WouldBlock.into()),
                Err(mpsc::TryRecvError::Disconnected) => return Ok(0),
            }
        }
        let count = bytes.len().min(self.pending.len());
        for byte in &mut bytes[..count] {
            *byte = self.pending.pop_front().unwrap();
        }
        self.read.fetch_add(count, Ordering::Release);
        Ok(count)
    }
}

fn replay_interrupt_session(
    noflsh: bool,
    vintr: u8,
    isig: bool,
    canonical: bool,
    bash: bool,
    exclusive: bool,
    ctrl_c: bool,
) {
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("reader");
    let release = root.path().join("release");
    let stale = root.path().join("stale-executed");
    let received_file = root.path().join("received-input");
    let script = if bash {
        String::from(
            r#"#!/bin/bash
stty echo isig -noflsh icanon
trap ':' INT
printf 'REPLAY_READER_READY\n'
sleep 30
printf 'INTERRUPTED\n'
IFS= read -r line
eval "$line"
"#,
        )
    } else {
        format!(
            r#"#!/usr/bin/python3
import fcntl, os, signal, termios, time
attributes = termios.tcgetattr(0)
attributes[3] |= termios.ECHO
attributes[3] = (attributes[3] | termios.ICANON) if {canonical} else (attributes[3] & ~termios.ICANON)
attributes[3] = (attributes[3] | termios.ISIG) if {isig} else (attributes[3] & ~termios.ISIG)
attributes[3] = (attributes[3] | termios.NOFLSH) if {noflsh} else (attributes[3] & ~termios.NOFLSH)
attributes[6][termios.VINTR] = bytes([{vintr}])
termios.tcsetattr(0, termios.TCSANOW, attributes)
if {exclusive}: fcntl.ioctl(0, termios.TIOCEXCL)
interrupted = False
def interrupt(*_):
    global interrupted
    interrupted = True
    os.write(1, b'INTERRUPTED\n')
signal.signal(signal.SIGINT, interrupt)
os.write(1, b'REPLAY_READER_READY\n')
limit = time.monotonic() + 12
while not interrupted and not os.path.exists({release:?}):
    if time.monotonic() >= limit: raise RuntimeError('missing replay interrupt')
    time.sleep(.002)
attributes[3] &= ~(termios.ICANON | termios.ECHO)
termios.tcsetattr(0, termios.TCSANOW, attributes)
received = b''
while not received.endswith(b'FRESH\n'):
    received += os.read(0, 4096)
with open({received_file:?}, 'wb') as saved: saved.write(received)
print('RESULT:' + ('fresh' if received == b'FRESH\n' else 'stale') + ':' + str(b'\3' in received), flush=True)
"#,
            canonical = if canonical { "True" } else { "False" },
            isig = if isig { "True" } else { "False" },
            noflsh = if noflsh { "True" } else { "False" },
            release = release.to_str().unwrap(),
            exclusive = if exclusive { "True" } else { "False" },
            received_file = received_file.to_str().unwrap()
        )
    };
    std::fs::write(&fixture, script).unwrap();
    make_executable(&fixture);
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let mut config = ShellHostConfig::new("replay-interrupt", root.path().join("session"))
        .with_env("HOME", home.display().to_string());
    config.native_mode = false;
    let capture = RawInputCapture::Question {
        id: "replay-question".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let (sender, chunks) = mpsc::channel();
    let read = Arc::new(AtomicUsize::new(0));
    let input = ReplayInput {
        chunks,
        pending: VecDeque::new(),
        read: read.clone(),
    };
    let mut payload = b"yes\n".to_vec();
    let stale_command = format!("touch '{}'; printf 'STALE_EXECUTED\\n'\n", stale.display());
    let line = if bash {
        stale_command.as_bytes()
    } else {
        b"STALE\n"
    };
    while payload.len() + line.len() < 60 * 1024 {
        payload.extend_from_slice(line);
    }
    let initial_input = format!("command {}\n", shell_arg(&fixture)).into_bytes();
    let initial_input_len = initial_input.len();
    let payload_len = payload.len();
    let interrupted = ctrl_c && isig && vintr == 3;
    let control_chunks = if interrupted {
        vec![b"\x03".to_vec()]
    } else if ctrl_c {
        vec![
            b"before-ctrl-c\x03after-ctrl-c".to_vec(),
            b"before-second\x03after-second".to_vec(),
        ]
    } else {
        vec![
            b"before-keyzafter-key".to_vec(),
            b"before-secondzafter-second".to_vec(),
        ]
    };
    let control_bytes: Vec<u8> = control_chunks.iter().flatten().copied().collect();
    let fresh_bytes = b"FRESH\n";
    let expected_literal = [
        payload[4..].to_vec(),
        control_bytes.clone(),
        fresh_bytes.to_vec(),
    ]
    .concat();
    let mut phase = 0;
    let mut fixture_started = false;
    let mut finished = false;
    let mut supplied = false;
    let mut control = false;
    let mut fresh = false;
    let started = Instant::now();
    let output = cosh_shell::shell_host::run_raw_relay_zsh_with_output_control(
        &config, input, Vec::new(), |events, output| {
            if stale.exists() {
                return Err(io::Error::other("cancelled replay executed a queued shell command"));
            }
            if started.elapsed() > Duration::from_secs(15) {
                return Err(io::Error::other(format!("replay test timed out at phase {phase}, read={}, payload={payload_len}, events={events:?}: {}", read.load(Ordering::Acquire), String::from_utf8_lossy(&output[output.len().saturating_sub(4096)..]))));
            }
            if !fixture_started && output.windows(config.prompt.len()).any(|w| w == config.prompt.as_bytes()) {
                sender.send(initial_input.clone()).unwrap();
                fixture_started = true;
            }
            if !finished && (output.windows(b"RESULT:".len()).any(|w| w == b"RESULT:") || (bash && String::from_utf8_lossy(output).lines().any(|line| line == "FRESH_EXECUTED"))) {
                sender.send(b"exit\n".to_vec()).unwrap();
                finished = true;
            }
            if phase == 0 && output.windows(b"REPLAY_READER_READY".len()).any(|w| w == b"REPLAY_READER_READY") {
                phase = 1;
                return Ok(RawObserverAction::CaptureInput(capture.clone()));
            }
            if phase == 1 {
                if !supplied {
                    sender.send(payload.clone()).unwrap();
                    supplied = true;
                }
                if read.load(Ordering::Acquire) >= payload_len + initial_input_len && events.iter().any(|event| event.message.as_deref() == Some("capture_submitted")) {
                    phase = 2;
                    return Ok(RawObserverAction::RawPassthrough);
                }
                return Ok(RawObserverAction::CaptureInput(capture.clone()));
            }
            if phase == 2 && !control && output.windows(b"STALE".len()).any(|w| w == b"STALE") {
                for chunk in &control_chunks { sender.send(chunk.clone()).unwrap(); }
                control = true;
            }
            if control && !interrupted && !release.exists() && read.load(Ordering::Acquire) >= payload_len + initial_input_len + control_bytes.len() {
                std::fs::write(&release, b"release").unwrap();
            }
            if control && !fresh && ((!interrupted && release.exists()) || output.windows(b"INTERRUPTED".len()).any(|w| w == b"INTERRUPTED")) {
                let bytes = if bash { b"printf 'FRESH_EXECUTED\\n'\n".as_slice() } else { b"FRESH\n".as_slice() };
                sender.send(bytes.to_vec()).unwrap();
                fresh = true;
            }
            Ok(if phase >= 2 { RawObserverAction::RawPassthrough } else { RawObserverAction::Continue })
        }
    ).expect("public capture replay PTY session");
    let text = String::from_utf8_lossy(&output.terminal_output);
    assert!(control && fresh, "{text}");
    assert_eq!(output.exit_status, Some(0), "{text}");
    let generation = output
        .events
        .iter()
        .find(|event| event.message.as_deref() == Some("capture_submitted"))
        .and_then(|event| event.capture.as_ref())
        .map(|capture| capture.generation)
        .expect("submitted capture generation");
    let rejected = output.events.iter().any(|event| {
        event.message.as_deref() == Some("capture_input_rejected")
            && event
                .capture
                .as_ref()
                .is_some_and(|capture| capture.generation == generation)
    });
    if !interrupted {
        let actual = std::fs::read(&received_file).expect("literal reader receipt");
        assert_eq!(
            actual.len(),
            expected_literal.len(),
            "literal FIFO length: {text}"
        );
        assert_eq!(actual, expected_literal, "literal FIFO contents");
        assert!(
            !rejected,
            "literal input must never reject the blocked suffix: {text}"
        );
    } else {
        assert!(
            rejected,
            "native cancellation must reject the blocked suffix: {text}"
        );
    }
    assert!(
        !output.events.iter().any(|event| {
            matches!(
                event.message.as_deref(),
                Some("capture_expired" | "capture_overflow")
            )
        }),
        "replay must reach the native owner without expiry or overflow: {text}"
    );
    if bash {
        assert!(
            !stale.exists(),
            "cancelled replay executed a queued shell command: {text}"
        );
        assert!(text.lines().any(|line| line == "FRESH_EXECUTED"), "{text}");
    } else {
        let actual = std::fs::read(&received_file).expect("native reader receipt");
        if interrupted && !noflsh {
            assert_eq!(
                actual, fresh_bytes,
                "cancelled terminal input must not reach the fresh reader"
            );
        } else if interrupted {
            let prefix = actual
                .strip_suffix(fresh_bytes)
                .expect("fresh suffix after retained bytes");
            assert!(
                !prefix.is_empty(),
                "NOFLSH must retain the committed prefix"
            );
            assert!(
                payload[4..].starts_with(prefix),
                "NOFLSH committed prefix must remain ordered"
            );
        }
        let expected = match (interrupted, noflsh) {
            (true, false) => "RESULT:fresh:False",
            (true, true) => "RESULT:stale:False",
            (false, _) if ctrl_c => "RESULT:stale:True",
            (false, _) => "RESULT:stale:False",
        };
        assert!(text.contains(expected), "missing {expected}: {text}");
        assert_eq!(text.contains("INTERRUPTED"), interrupted, "{text}");
    }
}

#[test]
fn canonical_replay_interrupt_discards_committed_prefix() {
    replay_interrupt_session(false, 3, true, true, false, false, true);
}

#[test]
fn noncanonical_replay_interrupt_discards_saturated_queue() {
    replay_interrupt_session(false, 3, true, false, false, false, true);
}

#[test]
fn canonical_replay_interrupt_preserves_noflsh_prefix() {
    replay_interrupt_session(true, 3, true, true, false, false, true);
}

#[test]
fn replay_ctrl_c_preserves_custom_vintr_literal() {
    replay_interrupt_session(false, 28, true, false, false, false, true);
}

#[test]
fn replay_ctrl_c_preserves_disabled_vintr_literal() {
    replay_interrupt_session(
        false,
        nix::libc::_POSIX_VDISABLE,
        true,
        false,
        false,
        false,
        true,
    );
}

#[test]
fn replay_ctrl_c_preserves_isig_disabled_literal() {
    replay_interrupt_session(false, 3, false, false, false, false, true);
}

#[test]
fn interrupted_replay_never_executes_queued_shell_command() {
    replay_interrupt_session(false, 3, true, true, true, false, true);
}

#[test]
fn canonical_literal_custom_vintr_preserves_complete_fifo() {
    replay_interrupt_session(false, 28, true, true, false, false, true);
}

#[test]
fn canonical_literal_disabled_vintr_preserves_complete_fifo() {
    replay_interrupt_session(
        false,
        nix::libc::_POSIX_VDISABLE,
        true,
        true,
        false,
        false,
        true,
    );
}

#[test]
fn canonical_literal_isig_disabled_preserves_complete_fifo() {
    replay_interrupt_session(false, 3, false, true, false, false, true);
}

#[test]
fn ordinary_native_key_preserves_complete_fifo() {
    replay_interrupt_session(false, 3, true, false, false, false, false);
}

#[test]
fn exclusive_terminal_interrupt_flushes_saturated_queue() {
    replay_interrupt_session(false, 3, true, false, false, true, true);
}

#[test]
fn exclusive_terminal_noflsh_preserves_committed_prefix() {
    replay_interrupt_session(true, 3, true, false, false, true, true);
}
