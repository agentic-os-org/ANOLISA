use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::super::generation::LineSubmitCounter;
use super::super::mode::current_raw_input_mode;
use super::super::spawn::{
    finish_input_relay, input_relay_context, relay_input_bytes, relay_late_capture_input,
    RawInputRelayState, RawInputShellRoute,
};
use super::super::{
    update_input_mode, PromptGhostCandidate, RawInputCapture, RawObserverAction, RawRelayAction,
    UserPtyInputGeneration,
};
use super::*;
use crate::input::AssistanceControl;

fn output_file(label: &str) -> (std::path::PathBuf, File) {
    let path = std::env::temp_dir().join(format!(
        "cosh-shell-prompt-ghost-{label}-{}",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&path)
        .expect("test output file");
    (path, file)
}

struct PausingEventSink {
    sender: mpsc::Sender<RawInputEvent>,
    published: mpsc::SyncSender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}

impl RawInputEventSink for PausingEventSink {
    fn send(&self, event: RawInputEvent) -> Result<(), mpsc::SendError<RawInputEvent>> {
        self.sender.send(event)?;
        self.published.send(()).expect("publish barrier");
        self.resume
            .lock()
            .expect("resume lock")
            .recv()
            .expect("resume event send");
        Ok(())
    }
}

#[test]
fn pty_user_write_waits_until_its_event_is_visible() {
    let (path, mut master) = output_file("event-before-pty-write");
    let (event_tx, event_rx) = mpsc::channel();
    let (published_tx, published_rx) = mpsc::sync_channel(0);
    let (resume_tx, resume_rx) = mpsc::channel();
    let sink = PausingEventSink {
        sender: event_tx,
        published: published_tx,
        resume: Mutex::new(resume_rx),
    };
    let writer = thread::spawn(move || {
        write_user_bytes_to_pty(
            &mut master,
            &UserPtyInputGeneration::default(),
            &mut LineSubmitCounter::default(),
            &sink,
            &super::super::MainPromptGate::default(),
            b"echo ordered\n",
        )
    });

    published_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("event publication");
    assert!(matches!(
        event_rx.try_recv().expect("visible PTY write event"),
        RawInputEvent::PtyUserWrite {
            line_submits: 1,
            ..
        }
    ));
    assert!(fs::read(&path).expect("read pre-write output").is_empty());

    resume_tx.send(()).expect("allow PTY write");
    writer.join().expect("writer join").expect("PTY write");
    assert_eq!(fs::read(&path).expect("read PTY output"), b"echo ordered\n");
    fs::remove_file(path).ok();
}

fn selection_input_mode() -> Arc<Mutex<RawInputMode>> {
    let candidates = vec![
        PromptGhostCandidate {
            text: "inspect memory".to_string(),
            suggestion_id: "health-1".to_string(),
        },
        PromptGhostCandidate {
            text: "continue deployment".to_string(),
            suggestion_id: "personal-1".to_string(),
        },
    ];
    Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: candidates[0].text.clone(),
        route: PromptGhostRoute::AgentSelection {
            candidates,
            active: 0,
        },
    }))
}

fn expect_prompt_ghost_dismissal(receiver: &mpsc::Receiver<RawInputEvent>) {
    for _ in 0..2 {
        if receiver
            .recv_timeout(Duration::from_millis(250))
            .expect("prompt ghost dismissal event")
            == RawInputEvent::PromptGhostDismissed
        {
            return;
        }
    }
    panic!("missing prompt ghost dismissal event");
}

#[test]
fn shift_tab_toggles_enhanced_routing_only_at_an_empty_prompt() {
    let (path, mut master) = output_file("assistance-toggle");
    let state_file = path.with_extension("enabled");
    fs::write(&state_file, b"enabled\n").expect("initial state file");
    let control = AssistanceControl::enabled(state_file.clone());
    let classifier = InputClassifier::default().with_assistance_control(control.clone());
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let (tx, rx) = mpsc::channel();
    let gate = MainPromptGate::default();
    gate.set_at_prompt(true);
    control.set_at_prompt(true);
    let mut state = RawInputRelayState::with_generation_and_gate(
        UserPtyInputGeneration::default(),
        gate.clone(),
        false,
    );

    for bytes in [b"\x1b".as_slice(), b"[".as_slice(), b"Z".as_slice()] {
        relay_input_bytes(
            bytes,
            Instant::now(),
            &mut master,
            &tx,
            &classifier,
            &input_mode,
            &mut state,
        )
        .expect("split Shift+Tab");
    }

    assert!(!control.is_enabled());
    assert!(!state_file.exists());
    assert!(rx
        .try_iter()
        .any(|event| event == RawInputEvent::AssistanceToggled));
    assert!(fs::read(&path).expect("toggle output").is_empty());

    relay_input_bytes(
        b"/help\n",
        Instant::now(),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("shell-only input");
    assert_eq!(fs::read(&path).expect("shell-only output"), b"/help\n");

    gate.set_at_prompt(true);
    control.set_at_prompt(true);
    relay_input_bytes(
        b"\x1b[Z",
        Instant::now(),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("reenable assistance");
    assert!(control.is_enabled());
    assert!(state_file.is_file());

    relay_input_bytes(
        b"draft",
        Instant::now(),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("draft input");
    relay_input_bytes(
        b"\x1b[Z",
        Instant::now(),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("draft Shift+Tab");
    assert!(control.is_enabled());
    assert!(fs::read(&path)
        .expect("draft output")
        .ends_with(b"draft\x1b[Z"));

    fs::remove_file(path).ok();
    fs::remove_file(state_file).ok();
}

#[test]
fn visible_shell_only_insight_owns_shift_tab() {
    let (path, mut master) = output_file("assistance-toggle-shell-only-insight");
    let state_file = path.with_extension("enabled");
    fs::write(&state_file, b"enabled\n").expect("initial state file");
    let control = AssistanceControl::enabled(state_file.clone());
    control.toggle().expect("disable assistance");
    control.set_at_prompt(true);
    let classifier = InputClassifier::default().with_assistance_control(control.clone());
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "analyze failed input".to_string(),
        route: PromptGhostRoute::AgentIntercept {
            suggestion_id: Some("insight-1".to_string()),
        },
    }));
    let (tx, rx) = mpsc::channel();
    let gate = MainPromptGate::default();
    gate.set_at_prompt(true);
    let mut state = RawInputRelayState::with_generation_and_gate(
        UserPtyInputGeneration::default(),
        gate,
        false,
    );

    relay_input_bytes(
        b"\x1b[Z",
        Instant::now(),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("Shift+Tab over shell-only insight");

    assert!(!control.is_enabled());
    assert!(!state_file.exists());
    assert_eq!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::PromptGhost {
            text: "analyze failed input".to_string(),
            route: PromptGhostRoute::AgentIntercept {
                suggestion_id: Some("insight-1".to_string()),
            },
        }
    );
    assert!(rx.try_iter().next().is_none());
    assert!(fs::read(&path).expect("toggle output").is_empty());
}

#[test]
fn visible_failure_insight_owns_split_shift_tab() {
    let (path, mut master) = output_file("assistance-disable-over-insight");
    let state_file = path.with_extension("enabled");
    fs::write(&state_file, b"enabled\n").expect("initial state file");
    let control = AssistanceControl::enabled(state_file.clone());
    control.set_at_prompt(true);
    let classifier = InputClassifier::default().with_assistance_control(control.clone());
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "analyze failed input".to_string(),
        route: PromptGhostRoute::AgentIntercept {
            suggestion_id: Some("insight-1".to_string()),
        },
    }));
    let (tx, rx) = mpsc::channel();
    let gate = MainPromptGate::default();
    gate.set_at_prompt(true);
    let mut state = RawInputRelayState::with_generation_and_gate(
        UserPtyInputGeneration::default(),
        gate,
        false,
    );

    for bytes in [b"\x1b".as_slice(), b"[".as_slice(), b"Z".as_slice()] {
        relay_input_bytes(
            bytes,
            Instant::now(),
            &mut master,
            &tx,
            &classifier,
            &input_mode,
            &mut state,
        )
        .expect("split Shift+Tab over failure insight");
    }

    assert!(control.is_enabled());
    assert!(state_file.is_file());
    assert_eq!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::PromptGhost {
            text: "analyze failed input".to_string(),
            route: PromptGhostRoute::AgentIntercept {
                suggestion_id: Some("insight-1".to_string()),
            },
        }
    );
    assert!(rx.try_iter().next().is_none());
    assert!(fs::read(&path).expect("toggle output").is_empty());
}

struct ChannelReader {
    receiver: mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
}

impl Read for ChannelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() {
            match self.receiver.try_recv() {
                Ok(bytes) => self.pending = bytes,
                Err(mpsc::TryRecvError::Empty) => {
                    return Err(io::ErrorKind::WouldBlock.into());
                }
                Err(mpsc::TryRecvError::Disconnected) => return Ok(0),
            }
        }
        let count = buffer.len().min(self.pending.len());
        buffer[..count].copy_from_slice(&self.pending[..count]);
        self.pending.drain(..count);
        Ok(count)
    }
}

struct ReadStartChannelReader {
    receiver: mpsc::Receiver<Vec<u8>>,
    read_started_tx: mpsc::Sender<()>,
}

impl Read for ReadStartChannelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.read_started_tx.send(()).expect("observe read start");
        let bytes = match self.receiver.try_recv() {
            Ok(bytes) => bytes,
            Err(mpsc::TryRecvError::Empty) => {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            Err(mpsc::TryRecvError::Disconnected) => return Ok(0),
        };
        assert!(bytes.len() <= buffer.len());
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

struct PausingChannelReader {
    receiver: mpsc::Receiver<Vec<u8>>,
    pause_on_read: usize,
    read_count: usize,
    bytes_ready_tx: mpsc::Sender<()>,
    resume_rx: mpsc::Receiver<()>,
}

impl Read for PausingChannelReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let bytes = match self.receiver.recv() {
            Ok(bytes) => bytes,
            Err(_) => return Ok(0),
        };
        assert!(bytes.len() <= buffer.len());
        buffer[..bytes.len()].copy_from_slice(&bytes);
        self.read_count += 1;
        if self.read_count == self.pause_on_read {
            self.bytes_ready_tx.send(()).expect("observe read bytes");
            self.resume_rx.recv().expect("resume read");
        }
        Ok(bytes.len())
    }
}

struct SelectionRelay {
    path: std::path::PathBuf,
    master: File,
    input_tx: Option<mpsc::Sender<Vec<u8>>>,
    event_rx: mpsc::Receiver<RawInputEvent>,
    input_mode: Arc<Mutex<RawInputMode>>,
    relay: thread::JoinHandle<io::Result<()>>,
}

impl SelectionRelay {
    fn start(label: &str) -> Self {
        let (path, master) = output_file(label);
        let (input_tx, input_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let input_mode = selection_input_mode();
        let relay = super::super::spawn_raw_input_relay(
            ChannelReader {
                receiver: input_rx,
                pending: Vec::new(),
            },
            master.try_clone().expect("clone output file"),
            event_tx,
            InputClassifier::default(),
            input_mode.clone(),
            UserPtyInputGeneration::default(),
            super::super::MainPromptGate::default(),
            false,
        );
        Self {
            path,
            master,
            input_tx: Some(input_tx),
            event_rx,
            input_mode,
            relay,
        }
    }

    fn send(&self, bytes: &[u8]) {
        self.input_tx
            .as_ref()
            .expect("input sender")
            .send(bytes.to_vec())
            .expect("send input");
    }

    fn finish(mut self) -> (Vec<RawInputEvent>, Vec<u8>, RawInputMode) {
        self.input_tx.take();
        self.relay
            .join()
            .expect("relay thread")
            .expect("relay result");
        self.master.sync_all().expect("sync test output");
        let output = fs::read(&self.path).expect("read test output");
        fs::remove_file(&self.path).ok();
        let mode = self.input_mode.lock().expect("input mode").clone();
        (self.event_rx.try_iter().collect(), output, mode)
    }
}

struct DelayRelay {
    path: std::path::PathBuf,
    master: File,
    input_tx: Option<mpsc::Sender<Vec<u8>>>,
    event_rx: mpsc::Receiver<RawInputEvent>,
    input_mode: Arc<Mutex<RawInputMode>>,
    relay: thread::JoinHandle<io::Result<()>>,
}

impl DelayRelay {
    fn start(label: &str) -> Self {
        let (path, master) = output_file(label);
        let (input_tx, input_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let input_mode = Arc::new(Mutex::new(super::super::mode::new_delay_input_mode()));
        let relay = super::super::spawn_raw_input_relay(
            ChannelReader {
                receiver: input_rx,
                pending: Vec::new(),
            },
            master.try_clone().expect("clone output file"),
            event_tx,
            InputClassifier::default(),
            input_mode.clone(),
            UserPtyInputGeneration::default(),
            super::super::MainPromptGate::default(),
            false,
        );
        Self {
            path,
            master,
            input_tx: Some(input_tx),
            event_rx,
            input_mode,
            relay,
        }
    }

    fn send(&self, bytes: &[u8]) {
        self.input_tx
            .as_ref()
            .expect("input sender")
            .send(bytes.to_vec())
            .expect("send input");
    }

    fn finish(mut self) -> (Vec<RawInputEvent>, Vec<u8>, RawInputMode) {
        self.input_tx.take();
        self.relay
            .join()
            .expect("relay thread")
            .expect("relay result");
        self.master.sync_all().expect("sync test output");
        let output = fs::read(&self.path).expect("read test output");
        fs::remove_file(&self.path).ok();
        let mode = self.input_mode.lock().expect("input mode").clone();
        (self.event_rx.try_iter().collect(), output, mode)
    }
}

fn expect_esc_event(receiver: &mpsc::Receiver<RawInputEvent>) {
    assert_eq!(
        receiver.recv_timeout(Duration::from_millis(250)),
        Ok(RawInputEvent::Esc),
        "expected Esc cancel event"
    );
}

#[test]
fn delay_bare_escape_requests_cancel_and_does_not_forward() {
    let relay = DelayRelay::start("delay-bare-escape");
    relay.send(b"\x1b");
    expect_esc_event(&relay.event_rx);
    let (events, output, mode) = relay.finish();
    assert!(
        !events.contains(&RawInputEvent::CtrlC),
        "unexpected CtrlC event"
    );
    assert_eq!(output, b"exit\n");
    assert!(matches!(mode, RawInputMode::Delay { .. }));
}

#[test]
fn delay_escape_sequence_is_forwarded_without_cancel() {
    let relay = DelayRelay::start("delay-escape-sequence");
    relay.send(b"\x1b[A");
    thread::sleep(Duration::from_millis(100));
    let (events, output, mode) = relay.finish();
    assert!(
        !events.contains(&RawInputEvent::Esc),
        "unexpected Esc event for arrow sequence"
    );
    assert_eq!(output, b"\x1b[A");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(matches!(mode, RawInputMode::Delay { .. }));
}

#[test]
fn delay_split_escape_sequence_is_forwarded_without_cancel() {
    let relay = DelayRelay::start("delay-split-escape-sequence");
    relay.send(b"\x1b");
    thread::sleep(Duration::from_millis(10));
    relay.send(b"[A");
    thread::sleep(Duration::from_millis(100));
    let (events, output, mode) = relay.finish();
    assert!(
        !events.contains(&RawInputEvent::Esc),
        "unexpected Esc event for split arrow sequence"
    );
    assert_eq!(output, b"\x1b[A");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(matches!(mode, RawInputMode::Delay { .. }));
}

#[test]
fn delay_escape_is_forwarded_when_run_finishes_before_deadline() {
    let relay = DelayRelay::start("delay-escape-run-finishes");
    relay.send(b"\x1b");
    thread::sleep(Duration::from_millis(10));
    *relay.input_mode.lock().expect("input mode") = RawInputMode::Passthrough;
    relay.send(b"x");
    let (events, output, mode) = relay.finish();
    assert!(
        !events.contains(&RawInputEvent::Esc),
        "unexpected Esc event after run finished"
    );
    assert_eq!(output, b"\x1bx");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(matches!(mode, RawInputMode::Passthrough));
}

#[test]
fn submitted_capture_discards_a_later_owned_read_when_the_chain_ends() {
    let (path, master) = output_file("capture-read-ahead");
    let (input_tx, input_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let first = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let second = RawInputCapture::Question {
        id: "q-2".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: first,
        generation: 1,
        installed_at: Instant::now(),
    }));
    let relay = super::super::spawn_raw_input_relay(
        ChannelReader {
            receiver: input_rx,
            pending: Vec::new(),
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"first\n".to_vec()).expect("first answer");
    let mut events = Vec::new();
    let first_generation = loop {
        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first capture submission");
        let generation = match &event {
            RawInputEvent::CaptureSubmitted { generation, .. } => Some(*generation),
            _ => None,
        };
        events.push(event);
        if let Some(generation) = generation {
            break generation;
        }
    };

    input_tx
        .send(b"typed-ahead\n".to_vec())
        .expect("read during ack");
    thread::sleep(Duration::from_millis(50));
    update_input_mode(
        &input_mode,
        &RawObserverAction::CaptureInput(second),
        Some(first_generation),
    );
    let deadline = Instant::now() + Duration::from_millis(250);
    while let Ok(event) = event_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
    {
        let second_generation = match &event {
            RawInputEvent::CaptureSubmitted {
                target_id,
                generation,
                ..
            } if target_id == "q-2" => Some(*generation),
            _ => None,
        };
        events.push(event);
        if let Some(generation) = second_generation {
            update_input_mode(&input_mode, &RawObserverAction::Continue, Some(generation));
            break;
        }
    }
    drop(input_tx);
    relay.join().expect("relay thread").expect("relay result");
    events.extend(event_rx.try_iter());

    assert!(
        !events
            .iter()
            .any(|event| event == &RawInputEvent::CardAnswer("typed-ahead".to_string())),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn input_obtained_before_capture_replacement_does_not_enter_the_new_capture() {
    let (path, master) = output_file("capture-read-return-cutoff");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let first = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let second = RawInputCapture::Question {
        id: "q-2".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: first,
        generation: 1,
        installed_at: Instant::now(),
    }));
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 2,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"first\n".to_vec()).expect("first answer");
    let first_generation = loop {
        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("first capture submission");
        if let RawInputEvent::CaptureSubmitted { generation, .. } = event {
            break generation;
        }
    };

    input_tx.send(b"stale".to_vec()).expect("stale input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("stale bytes obtained before replacement");
    let (updated_tx, updated_rx) = mpsc::channel();
    let update_mode = input_mode.clone();
    let updater = thread::spawn(move || {
        update_input_mode(
            &update_mode,
            &RawObserverAction::CaptureInput(second),
            Some(first_generation),
        );
        updated_tx.send(()).expect("replacement update");
    });
    updated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("replacement installs while read is pending");
    resume_tx.send(()).expect("release stale read");
    updater.join().expect("replacement updater");
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if matches!(
            current_raw_input_mode(&input_mode),
            RawInputMode::Capture {
                capture: RawInputCapture::Question { ref id, .. },
                ..
            } if id == "q-2"
        ) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "replacement capture not installed"
        );
        thread::yield_now();
    }
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::CardInput(target_id, _) if target_id == "q-2"
        )),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn input_obtained_after_capture_install_enters_the_new_capture() {
    let (path, master) = output_file("capture-read-after-install");
    let (input_tx, input_rx) = mpsc::channel();
    let (read_started_tx, read_started_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let capture = RawInputCapture::Question {
        id: "q-1".to_string(),
        option_count: 0,
        selected: 0,
        allow_free_text: true,
        multiple: false,
        secret: false,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let relay = super::super::spawn_raw_input_relay(
        ReadStartChannelReader {
            receiver: input_rx,
            read_started_tx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    read_started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("reader blocked before capture install");
    update_input_mode(&input_mode, &RawObserverAction::CaptureInput(capture), None);
    input_tx.send(b"answer\n".to_vec()).expect("capture answer");

    let mut events = Vec::new();
    let generation = loop {
        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("capture submission");
        let generation = match &event {
            RawInputEvent::CaptureSubmitted { generation, .. } => Some(*generation),
            _ => None,
        };
        events.push(event);
        if let Some(generation) = generation {
            break generation;
        }
    };
    update_input_mode(&input_mode, &RawObserverAction::Continue, Some(generation));
    drop(input_tx);
    relay.join().expect("relay thread").expect("relay result");
    events.extend(event_rx.try_iter());

    assert!(
        events
            .iter()
            .any(|event| event == &RawInputEvent::CardAnswer("answer".to_string())),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn passthrough_owned_input_does_not_enter_a_later_capture() {
    let (path, master) = output_file("passthrough-read-capture-cutover");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"stale".to_vec()).expect("passthrough input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("passthrough bytes obtained");
    let (updated_tx, updated_rx) = mpsc::channel();
    let update_mode = input_mode.clone();
    let updater = thread::spawn(move || {
        update_input_mode(
            &update_mode,
            &RawObserverAction::CaptureInput(RawInputCapture::Question {
                id: "q-1".to_string(),
                option_count: 0,
                selected: 0,
                allow_free_text: true,
                multiple: false,
                secret: false,
            }),
            None,
        );
        updated_tx.send(()).expect("capture update");
    });
    updated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("capture installs while read is pending");
    resume_tx.send(()).expect("release passthrough read");
    updater.join().expect("capture updater");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CardInput(target, _) if target == "q-1")),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn delay_owned_escape_does_not_reach_a_later_capture_or_shell() {
    let (path, master) = output_file("delay-read-capture-cutover");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Delay { generation: 1 }));
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(vec![0x1b]).expect("delay escape");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("delay escape obtained");
    update_input_mode(
        &input_mode,
        &RawObserverAction::CaptureInput(RawInputCapture::Question {
            id: "q-1".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        }),
        None,
    );
    resume_tx.send(()).expect("release delay read");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::QuestionCancel(target) if target == "q-1"
        )),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn capture_owned_input_does_not_enter_later_passthrough() {
    let (path, master) = output_file("capture-read-passthrough-cutover");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, _event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "q-1".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 1,
        installed_at: Instant::now(),
    }));
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx
        .send(b"stale-capture-input\n".to_vec())
        .expect("capture input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("capture bytes obtained");
    let (updated_tx, updated_rx) = mpsc::channel();
    let update_mode = input_mode.clone();
    let updater = thread::spawn(move || {
        *update_mode.lock().expect("input mode") = RawInputMode::Passthrough;
        updated_tx.send(()).expect("passthrough update");
    });
    updated_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("passthrough installs while read is pending");
    resume_tx.send(()).expect("release capture read");
    updater.join().expect("passthrough updater");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn prompt_ghost_candidate_cycle_during_read_does_not_drop_input() {
    let (path, master) = output_file("ghost-cycle-read-ownership");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"x".to_vec()).expect("typed input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("input bytes obtained");
    // Simulate Shift+Tab candidate cycling landing between the read obtaining
    // bytes and the reader publishing them: same prompt ghost owner, only the
    // active candidate changes.
    {
        let mut mode = input_mode.lock().expect("input mode");
        let RawInputMode::PromptGhost {
            route: PromptGhostRoute::AgentSelection { candidates, .. },
            ..
        } = mode.clone()
        else {
            panic!("prompt ghost selection mode");
        };
        *mode = RawInputMode::PromptGhost {
            text: candidates[1].text.clone(),
            route: PromptGhostRoute::AgentSelection {
                candidates,
                active: 1,
            },
        };
    }
    resume_tx.send(()).expect("release read");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.contains(&RawInputEvent::PromptGhostDismissed),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"x");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    fs::remove_file(path).ok();
}

#[test]
fn tab_read_under_selection_is_not_reinterpreted_by_native_route() {
    let (path, master) = output_file("ghost-route-cutover-selection-native");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"\t".to_vec()).expect("tab input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("tab bytes obtained");
    // The prompt ghost is replaced by a native-shell route between the read
    // obtaining the Tab and the reader publishing it: the route kinds differ,
    // so the Tab must be discarded instead of writing the new native ghost
    // text to the PTY.
    {
        let mut mode = input_mode.lock().expect("input mode");
        *mode = RawInputMode::PromptGhost {
            text: "echo native".to_string(),
            route: PromptGhostRoute::NativeShell,
        };
    }
    resume_tx.send(()).expect("release read");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptGhostAccepted { .. })),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn tab_read_under_native_route_is_not_reinterpreted_by_agent_intercept() {
    let (path, master) = output_file("ghost-route-cutover-native-intercept");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "echo native".to_string(),
        route: PromptGhostRoute::NativeShell,
    }));
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"\t".to_vec()).expect("tab input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("tab bytes obtained");
    // The native ghost is replaced by an agent-intercept route while the Tab
    // read is in flight: the Tab must neither accept the new suggestion nor
    // write the old native ghost text to the PTY.
    {
        let mut mode = input_mode.lock().expect("input mode");
        *mode = RawInputMode::PromptGhost {
            text: "inspect memory".to_string(),
            route: PromptGhostRoute::AgentIntercept {
                suggestion_id: Some("health-1".to_string()),
            },
        };
    }
    resume_tx.send(()).expect("release read");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptGhostAccepted { .. })),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn enter_read_under_intercept_is_not_reinterpreted_by_selection() {
    let (path, master) = output_file("ghost-route-cutover-intercept-selection");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "inspect memory".to_string(),
        route: PromptGhostRoute::AgentIntercept {
            suggestion_id: Some("health-1".to_string()),
        },
    }));
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"\r".to_vec()).expect("enter input");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("enter bytes obtained");
    // The intercept ghost is replaced by a selection route while the Enter
    // read is in flight: the Enter must not commit the newly installed
    // candidate to the agent.
    {
        let mut mode = input_mode.lock().expect("input mode");
        *mode = current_raw_input_mode(&selection_input_mode());
    }
    resume_tx.send(()).expect("release read");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::CandidateCommit(_) | RawInputEvent::PromptGhostIntercept { .. }
        )),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn tab_after_route_cutover_is_relayed_under_the_new_route() {
    let (path, master) = output_file("ghost-route-cutover-window-bound");
    let (input_tx, input_rx) = mpsc::channel();
    let (bytes_ready_tx, bytes_ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let relay = super::super::spawn_raw_input_relay(
        PausingChannelReader {
            receiver: input_rx,
            pause_on_read: 1,
            read_count: 0,
            bytes_ready_tx,
            resume_rx,
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"\t".to_vec()).expect("in-flight tab");
    bytes_ready_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("tab bytes obtained");
    {
        let mut mode = input_mode.lock().expect("input mode");
        *mode = RawInputMode::PromptGhost {
            text: "echo native".to_string(),
            route: PromptGhostRoute::NativeShell,
        };
    }
    resume_tx.send(()).expect("release read");
    // The discard window is bounded to the single in-flight read: a second
    // Tab pressed after the native route is installed must be relayed under
    // that route (accepting the native ghost exactly once), not dropped.
    input_tx.send(b"\t".to_vec()).expect("post-cutover tab");
    drop(input_tx);

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptGhostAccepted { .. })),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"echo native");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    fs::remove_file(path).ok();
}

#[test]
fn stale_generation_reads_are_discarded_without_affecting_the_next_capture() {
    let (path, mut master) = output_file("capture-overflow-tagged-reads");
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "q-2".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 2,
        installed_at: Instant::now(),
    }));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let chunks = (0..12)
        .map(|index| vec![b'a' + index; 8192])
        .collect::<Vec<_>>();
    for chunk in &chunks {
        relay_late_capture_input(
            chunk,
            1,
            &mut master,
            &event_tx,
            &classifier,
            &input_mode,
            &mut state,
        )
        .expect("relay tagged capture input");
    }
    assert!(matches!(
        current_raw_input_mode(&input_mode),
        RawInputMode::Capture {
            capture: RawInputCapture::Question { id, .. },
            generation: 2,
            ..
        } if id == "q-2"
    ));
    finish_input_relay(&mut master, &event_tx, &classifier, &input_mode, &mut state)
        .expect("finish relay");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::CardInput(target_id, _)
                | RawInputEvent::CaptureSubmitted { target_id, .. }
                if target_id == "q-2"
        )),
        "{events:?}"
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::CaptureOverflow { .. })));
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn active_capture_eof_drains_the_generation_without_input() {
    let (path, master) = output_file("capture-empty-eof");
    let (input_tx, input_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "q-1".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 9,
        installed_at: Instant::now(),
    }));
    let relay = super::super::spawn_raw_input_relay(
        ChannelReader {
            receiver: input_rx,
            pending: Vec::new(),
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    drop(input_tx);
    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();

    assert_eq!(
        events
            .iter()
            .filter(|event| { matches!(event, RawInputEvent::CaptureDrained { generation: 9 }) })
            .count(),
        1,
        "{events:?}"
    );
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Terminal { generation: 9, .. }
    ));
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    fs::remove_file(path).ok();
}

#[test]
fn selection_bare_escape_times_out_without_waiting_for_another_key() {
    let (path, master) = output_file("selection-bare-escape");
    let (input_tx, input_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let candidates = vec![PromptGhostCandidate {
        text: "inspect memory".to_string(),
        suggestion_id: "health-1".to_string(),
    }];
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: candidates[0].text.clone(),
        route: PromptGhostRoute::AgentSelection {
            candidates,
            active: 0,
        },
    }));
    let relay = super::super::spawn_raw_input_relay(
        ChannelReader {
            receiver: input_rx,
            pending: Vec::new(),
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx.send(b"\x1b".to_vec()).expect("send escape");
    expect_prompt_ghost_dismissal(&event_rx);
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));

    drop(input_tx);
    relay.join().expect("relay thread").expect("relay result");
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"\x1b");
    fs::remove_file(path).ok();
}

#[test]
fn selection_action_wait_flushes_escape_at_the_deadline() {
    let (path, master) = output_file("selection-action-wait");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "inspect memory".to_string(),
        route: PromptGhostRoute::AgentSelection {
            candidates: vec![PromptGhostCandidate {
                text: "inspect memory".to_string(),
                suggestion_id: "health-1".to_string(),
            }],
            active: 0,
        },
    }));
    let relay = super::super::spawn_raw_action_relay(
        vec![
            RawRelayAction::write(b"\x1b"),
            RawRelayAction::wait(Duration::from_millis(500)),
        ],
        master.try_clone().expect("clone output file"),
        0,
        tx,
        InputClassifier::default(),
        input_mode.clone(),
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    expect_prompt_ghost_dismissal(&rx);
    assert!(!relay.is_finished());
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));

    relay.join().expect("relay thread").expect("relay result");
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read test output"), b"\x1b");
    fs::remove_file(path).ok();
}

#[test]
fn selection_shift_tab_cycles_when_arriving_in_one_chunk() {
    let relay = SelectionRelay::start("selection-shift-tab");
    relay.send(b"\x1b[Z");

    let (events, output, mode) = relay.finish();

    assert_eq!(output, b"exit\n");
    assert!(events.contains(&RawInputEvent::PromptGhostCycle {
        text: "continue deployment".to_string(),
    }));
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(matches!(mode, RawInputMode::Passthrough));
}

#[test]
fn selection_shift_tab_cycles_when_arriving_in_three_chunks_within_window() {
    let (path, mut master) = output_file("selection-split-shift-tab");
    let (tx, rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now();

    for (bytes, offset) in [
        (b"\x1b".as_slice(), 0),
        (b"[".as_slice(), 1),
        (b"Z".as_slice(), 2),
    ] {
        relay_input_bytes(
            bytes,
            received_at + Duration::from_millis(offset),
            &mut master,
            &tx,
            &classifier,
            &input_mode,
            &mut state,
        )
        .expect("relay split shift-tab");
    }

    let events = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, RawInputEvent::PromptGhostCycle { .. }))
            .count(),
        1
    );
    fs::remove_file(path).ok();
}

#[test]
fn selection_shift_tab_received_before_deadline_survives_a_delayed_relay() {
    let (path, mut master) = output_file("selection-delayed-shift-tab");
    let (tx, rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();

    relay_input_bytes(
        b"\x1b",
        Instant::now()
            .checked_sub(Duration::from_millis(100))
            .expect("recent timestamp"),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    let received_at = Instant::now()
        .checked_sub(Duration::from_millis(90))
        .expect("recent timestamp");
    relay_input_bytes(
        b"[Z",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("cycle delayed shift-tab");

    assert!(rx.try_iter().any(|event| matches!(
        event,
        RawInputEvent::PromptGhostCycle { text } if text == "continue deployment"
    )));
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

#[test]
fn selection_escape_with_nonmatching_follow_up_dismisses_and_forwards_all_bytes() {
    let relay = SelectionRelay::start("selection-escape-nonmatching");
    relay.send(b"\x1b");
    relay.send(b"x");

    let (events, output, mode) = relay.finish();

    assert_eq!(output, b"\x1bx");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::PromptGhostCycle { .. })));
    assert!(matches!(mode, RawInputMode::Passthrough));
}

#[test]
fn selection_partial_csi_times_out_and_forwards_all_bytes() {
    let relay = SelectionRelay::start("selection-partial-csi");
    relay.send(b"\x1b[");
    expect_prompt_ghost_dismissal(&relay.event_rx);

    let (events, output, mode) = relay.finish();
    assert_eq!(output, b"\x1b[");
    assert!(events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(matches!(mode, RawInputMode::Passthrough));
}

#[test]
fn selection_pending_escape_at_eof_is_cancelled_before_exit() {
    let relay = SelectionRelay::start("selection-escape-eof");
    relay.send(b"\x1b");

    let (events, output, mode) = relay.finish();

    assert_eq!(output, b"exit\n");
    assert!(!events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(matches!(mode, RawInputMode::Passthrough));
}

#[test]
fn selection_pending_escape_at_eof_after_route_change_is_cancelled() {
    let (path, mut master) = output_file("selection-escape-route-eof");
    let (tx, rx) = mpsc::channel();
    let old_route = PromptGhostRoute::AgentSelection {
        candidates: vec![PromptGhostCandidate {
            text: "old selection".to_string(),
            suggestion_id: "old-1".to_string(),
        }],
        active: 0,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "old selection".to_string(),
        route: old_route,
    }));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    relay_input_bytes(
        b"\x1b",
        Instant::now(),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    *input_mode.lock().expect("input mode") = RawInputMode::PromptGhost {
        text: "new selection".to_string(),
        route: PromptGhostRoute::AgentSelection {
            candidates: vec![PromptGhostCandidate {
                text: "new selection".to_string(),
                suggestion_id: "new-1".to_string(),
            }],
            active: 0,
        },
    };
    finish_input_relay(&mut master, &tx, &classifier, &input_mode, &mut state)
        .expect("finish relay");

    assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(!events.contains(&RawInputEvent::EofShutdownRequested));
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));
    fs::remove_file(path).ok();
}

#[test]
fn selection_route_change_before_deadline_dismisses_then_forwards_shift_tab() {
    let (path, mut master) = output_file("selection-route-change-shift-tab");
    let (tx, rx) = mpsc::channel();
    let old_route = PromptGhostRoute::AgentSelection {
        candidates: vec![PromptGhostCandidate {
            text: "old selection".to_string(),
            suggestion_id: "old-1".to_string(),
        }],
        active: 0,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "old selection".to_string(),
        route: old_route,
    }));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now();

    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    *input_mode.lock().expect("input mode") = RawInputMode::PromptGhost {
        text: "new selection".to_string(),
        route: PromptGhostRoute::AgentSelection {
            candidates: vec![
                PromptGhostCandidate {
                    text: "new selection".to_string(),
                    suggestion_id: "new-1".to_string(),
                },
                PromptGhostCandidate {
                    text: "another selection".to_string(),
                    suggestion_id: "new-2".to_string(),
                },
            ],
            active: 0,
        },
    };

    relay_input_bytes(
        b"[Z",
        received_at + Duration::from_millis(1),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("handle shift-tab after route change");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(fs::read(&path).expect("read test output"), b"\x1b[Z");
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::PromptGhostCycle { .. })));
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));
    fs::remove_file(path).ok();
}

#[test]
fn selection_expired_escape_dismisses_instead_of_rebuffering_for_a_new_route() {
    let (path, mut master) = output_file("selection-expired-route-change");
    let (tx, rx) = mpsc::channel();
    let old_route = PromptGhostRoute::AgentSelection {
        candidates: vec![PromptGhostCandidate {
            text: "old selection".to_string(),
            suggestion_id: "old-1".to_string(),
        }],
        active: 0,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "old selection".to_string(),
        route: old_route,
    }));
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now()
        .checked_sub(Duration::from_millis(100))
        .expect("recent timestamp");
    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    *input_mode.lock().expect("input mode") = RawInputMode::PromptGhost {
        text: "new selection".to_string(),
        route: PromptGhostRoute::AgentSelection {
            candidates: vec![PromptGhostCandidate {
                text: "new selection".to_string(),
                suggestion_id: "new-1".to_string(),
            }],
            active: 0,
        },
    };
    relay_input_bytes(
        b"",
        received_at + Duration::from_millis(51),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("flush expired escape");

    assert_eq!(fs::read(&path).expect("read test output"), b"\x1b");
    assert!(rx
        .try_iter()
        .any(|event| event == RawInputEvent::PromptGhostDismissed));
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));
    fs::remove_file(path).ok();
}

#[test]
fn selection_timeout_and_follow_up_byte_do_not_duplicate_or_reorder_input() {
    let (path, mut master) = output_file("selection-timeout-follow-up");
    let (tx, rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now()
        .checked_sub(Duration::from_millis(100))
        .expect("recent timestamp");

    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    relay_input_bytes(
        b"x",
        received_at + Duration::from_millis(51),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("flush escape and relay follow-up");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(fs::read(&path).expect("read test output"), b"\x1bx");
    assert_eq!(
        events
            .iter()
            .filter(|event| **event == RawInputEvent::PromptGhostDismissed)
            .count(),
        1
    );
    fs::remove_file(path).ok();
}

#[test]
fn selection_pending_escape_is_forwarded_when_the_input_mode_changes() {
    let (path, mut master) = output_file("selection-mode-change");
    let (tx, rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now();

    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    *input_mode.lock().expect("input mode") = RawInputMode::RawPassthrough;
    relay_input_bytes(
        b"x",
        received_at + Duration::from_millis(1),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("relay pending escape after mode change");

    let events = rx.try_iter().collect::<Vec<_>>();
    let mode = input_mode.lock().expect("input mode").clone();
    assert_eq!(fs::read(&path).expect("read test output"), b"\x1bx");
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::PromptGhostCycle { .. })));
    assert!(matches!(mode, RawInputMode::RawPassthrough));
    fs::remove_file(path).ok();
}

#[test]
fn selection_pending_escape_does_not_cancel_a_new_capture() {
    let (path, mut master) = output_file("selection-to-capture");
    let (tx, rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now();

    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    *input_mode.lock().expect("input mode") = RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "q-1".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 7,
        installed_at: Instant::now(),
    };

    let mode_for_ack = input_mode.clone();
    let ack = thread::spawn(move || {
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_timeout(Duration::from_millis(250)) {
            let generation = match &event {
                RawInputEvent::CaptureSubmitted { generation, .. } => Some(*generation),
                _ => None,
            };
            events.push(event);
            if let Some(generation) = generation {
                update_input_mode(
                    &mode_for_ack,
                    &RawObserverAction::Continue,
                    Some(generation),
                );
                break;
            }
        }
        events
    });

    relay_input_bytes(
        b"",
        received_at + Duration::from_millis(1),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("flush pending escape after capture install");
    let events = ack.join().expect("ack thread");

    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::QuestionCancel(_) | RawInputEvent::CaptureSubmitted { .. }
        )),
        "{events:?}"
    );
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Capture { generation: 7, .. }
    ));
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

#[test]
fn selection_split_shift_tab_suffix_does_not_enter_a_new_capture() {
    let (path, mut master) = output_file("selection-split-shift-tab-to-capture");
    let (tx, rx) = mpsc::channel();
    let input_mode = selection_input_mode();
    let classifier = InputClassifier::default();
    let mut state = RawInputRelayState::default();
    let received_at = Instant::now();

    relay_input_bytes(
        b"\x1b",
        received_at,
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("buffer escape");
    *input_mode.lock().expect("input mode") = RawInputMode::Capture {
        capture: RawInputCapture::Question {
            id: "q-1".to_string(),
            option_count: 0,
            selected: 0,
            allow_free_text: true,
            multiple: false,
            secret: false,
        },
        generation: 7,
        installed_at: Instant::now(),
    };

    relay_input_bytes(
        b"[Z",
        received_at + Duration::from_millis(1),
        &mut master,
        &tx,
        &classifier,
        &input_mode,
        &mut state,
    )
    .expect("discard replaced ghost suffix");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::CardInput(_, _)
                | RawInputEvent::QuestionCancel(_)
                | RawInputEvent::CaptureSubmitted { .. }
        )),
        "{events:?}"
    );
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Capture { generation: 7, .. }
    ));
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

#[test]
fn shell_rewrite_tab_writes_to_native_line_editor_without_agent_intercept() {
    let (path, mut master) = output_file("native");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "grep file".to_string(),
        route: PromptGhostRoute::NativeShell,
    }));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    assert!(relay_prompt_ghost_input(
        b"\t",
        "grep file",
        &PromptGhostRoute::NativeShell,
        &mut relay,
    )
    .expect("accept native ghost"));
    relay_passthrough_input(b"\t\x15", &mut relay)
        .expect("native completion and line clearing remain available");
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"grep file\t\x15"
    );
    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        vec![
            RawInputEvent::PromptGhostClear,
            RawInputEvent::PtyUserWrite {
                generation: 1,
                line_submits: 0,
            },
            RawInputEvent::ShellInputActivity { empty: true },
            RawInputEvent::PtyUserWrite {
                generation: 2,
                line_submits: 0,
            },
        ]
    );
    assert!(!line_buffer.force_agent_intercept);
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::RawPassthrough
    ));
    fs::remove_file(path).ok();
}

#[test]
fn zsh_native_slash_tab_is_handed_to_shell_immediately() {
    let (path, mut master) = output_file("native-slash-tab");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut zsh_path_prompt_buffering = ZshPathPromptBuffering::new();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: Some(&mut zsh_path_prompt_buffering),
    };

    relay_passthrough_input(b"/ho", &mut relay).expect("buffer slash prefix");
    relay_passthrough_input(b"\t\r", &mut relay)
        .expect("split completion from coalesced submission");
    relay.master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        RawInputEvent::CandidateRedraw { input, .. } if input == b"/ho"
    )));
    assert!(events.iter().all(|event| !matches!(
        event,
        RawInputEvent::CandidateRedraw { input, .. } if input.contains(&b'\t')
    )));
    assert!(events.contains(&RawInputEvent::CandidateClearLine));
    let writes = events
        .iter()
        .filter_map(|event| match event {
            RawInputEvent::PtyUserWrite {
                generation,
                line_submits,
            } => Some((*generation, *line_submits)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(writes, vec![(1, 0)]);
    assert_eq!(fs::read(&path).expect("read test output"), b"/ho\t");
    assert!(!relay.line_buffer.is_active());

    relay_passthrough_input(b"\r", &mut relay).expect("submit after visible completion");
    relay.master.sync_all().expect("sync confirmed submission");
    assert_eq!(fs::read(&path).expect("read test output"), b"/ho\t\r");
    fs::remove_file(path).ok();
}

#[test]
fn zsh_candidate_submit_precedes_later_tab_in_same_read() {
    let cwd = tempfile::tempdir().expect("temp cwd");
    let (path, mut master) = output_file("candidate-submit-before-tab");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    classifier
        .shell_path_command_names()
        .set(Some(Vec::new()), Some(Vec::new()));
    classifier.prompt_cwd().set(Some(
        cwd.path()
            .canonicalize()
            .expect("canonical cwd")
            .to_string_lossy()
            .into_owned(),
    ));
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut zsh_path_prompt_buffering = ZshPathPromptBuffering::new();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: Some(&mut zsh_path_prompt_buffering),
    };

    relay_passthrough_input("打开./definitely-not-command".as_bytes(), &mut relay)
        .expect("buffer missing path candidate");
    relay_passthrough_input(b"\r\t", &mut relay).expect("route submit before later tab");
    relay.master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::NativePathPromptIntercept { input, .. }
                if input == "打开./definitely-not-command"
        )),
        "{events:?}"
    );
    assert!(!fs::read(&path)
        .expect("read test output")
        .starts_with("打开./definitely-not-command".as_bytes()));
    fs::remove_file(path).ok();
}

#[test]
fn zsh_tab_handoff_drains_preserved_input_at_eof() {
    let (path, mut master) = output_file("tab-handoff-typeahead");
    let (tx, _rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let classifier = InputClassifier::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut state = RawInputRelayState::with_shell_route(
        UserPtyInputGeneration::default(),
        RawInputShellRoute::new(main_prompt_gate, false, Some(ZshPathPromptBuffering::new())),
    );
    {
        let mut relay = input_relay_context(&mut master, &classifier, &tx, &input_mode, &mut state);
        relay_passthrough_input(b"/ho", &mut relay).expect("buffer slash prefix");
        relay_passthrough_input(b"\t\r echo preserved\r", &mut relay)
            .expect("preserve typeahead after protected submit");
    }
    master.sync_all().expect("sync test output");
    assert_eq!(fs::read(&path).expect("read held output"), b"/ho\t");

    {
        let mut relay = input_relay_context(&mut master, &classifier, &tx, &input_mode, &mut state);
        relay_passthrough_input(b"echo next\r", &mut relay)
            .expect("retain input arriving before the handoff deadline");
    }
    master.sync_all().expect("sync retained output");
    assert_eq!(fs::read(&path).expect("read retained output"), b"/ho\t");

    finish_input_relay(&mut master, &tx, &classifier, &input_mode, &mut state)
        .expect("EOF drains preserved typeahead");
    master.sync_all().expect("sync replayed output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"/ho\t\x03 echo preserved\recho next\rexit\n"
    );
    fs::remove_file(path).ok();
}

#[test]
fn zsh_space_prefix_returns_to_shell_when_ascii_command_resolves() {
    let (path, mut master) = output_file("native-space-prefix");
    let (tx, _rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut zsh_path_prompt_buffering = ZshPathPromptBuffering::new();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: Some(&mut zsh_path_prompt_buffering),
    };

    relay_passthrough_input(b" ", &mut relay).expect("hold unresolved space prefix");
    assert!(relay.line_buffer.is_active());
    assert_eq!(fs::read(&path).expect("read held output"), b"");

    relay_passthrough_input(b"echo", &mut relay).expect("return ASCII command to ZLE");
    relay.master.sync_all().expect("sync returned command");
    assert!(!relay.line_buffer.is_active());
    assert_eq!(fs::read(&path).expect("read returned output"), b" echo");
    fs::remove_file(path).ok();
}

#[test]
fn native_exact_slash_tab_submit_keeps_readline_guard() {
    let (path, mut master) = output_file("native-slash-tab-submit");
    let (tx, _rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default().with_bash_slash_submission_guard(true);
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: true,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(b"/help\t\n", &mut relay).expect("submit completed slash");
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"/help\t\x1b[99~\n"
    );
    fs::remove_file(path).ok();
}

#[test]
fn native_shell_input_reports_editing_then_empty_without_content() {
    let (path, mut master) = output_file("input-state");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::RawPassthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(b"partial", &mut relay).expect("type partial line");
    relay_passthrough_input(&[super::super::CTRL_U], &mut relay).expect("clear line");

    assert_eq!(
        rx.try_iter().collect::<Vec<_>>(),
        vec![
            RawInputEvent::ShellInputActivity { empty: false },
            RawInputEvent::PtyUserWrite {
                generation: 1,
                line_submits: 0,
            },
            RawInputEvent::ShellInputActivity { empty: true },
            RawInputEvent::PtyUserWrite {
                generation: 2,
                line_submits: 0,
            },
        ]
    );
    fs::remove_file(path).ok();
}

#[test]
fn agent_prompt_tab_stays_local_until_enter_and_keeps_suggestion_id() {
    let (path, mut master) = output_file("agent");
    let (tx, rx) = mpsc::channel();
    let route = PromptGhostRoute::AgentIntercept {
        suggestion_id: Some("suggestion-1".to_string()),
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "analyze failure".to_string(),
        route: route.clone(),
    }));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_prompt_ghost_input(b"\t", "analyze failure", &route, &mut relay)
        .expect("accept agent ghost");
    let accepted = rx.try_iter().collect::<Vec<_>>();
    assert!(accepted.contains(&RawInputEvent::PromptGhostAccepted {
        suggestion_id: Some("suggestion-1".to_string()),
    }));
    assert!(accepted
        .iter()
        .all(|event| !matches!(event, RawInputEvent::PromptGhostIntercept { .. })));

    relay_passthrough_input(b" safely\n", &mut relay).expect("submit edited agent prompt");
    assert!(rx.try_iter().any(|event| matches!(
        event,
        RawInputEvent::PromptGhostIntercept { input, suggestion_id }
            if input == "analyze failure safely"
                && suggestion_id.as_deref() == Some("suggestion-1")
    )));
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

#[test]
fn selection_shift_tab_cycles_and_tab_inserts_the_active_prompt() {
    let relay = SelectionRelay::start("selection-cycle-tab");
    relay.send(b"\x1b[Z");
    relay.send(b"\t");

    let (events, output, mode) = relay.finish();
    assert!(events.contains(&RawInputEvent::PromptGhostCycle {
        text: "continue deployment".to_string(),
    }));
    assert!(events.contains(&RawInputEvent::PromptGhostAccepted {
        suggestion_id: Some("personal-1".to_string()),
    }));
    assert_eq!(output, b"exit\n");
    assert!(matches!(mode, RawInputMode::Passthrough));
}

#[test]
fn selection_enter_submits_the_active_prompt_without_shell_execution() {
    let (path, mut master) = output_file("selection-enter");
    let (tx, rx) = mpsc::channel();
    let route = PromptGhostRoute::AgentSelection {
        candidates: vec![PromptGhostCandidate {
            text: "inspect disk pressure".to_string(),
            suggestion_id: "health-disk".to_string(),
        }],
        active: 0,
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "inspect disk pressure".to_string(),
        route: route.clone(),
    }));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_prompt_ghost_input(b"\r", "inspect disk pressure", &route, &mut relay)
        .expect("submit active selection");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        RawInputEvent::PromptGhostIntercept { input, suggestion_id }
            if input == "inspect disk pressure"
                && suggestion_id.as_deref() == Some("health-disk")
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::PromptGhostAccepted { .. })));
    assert_eq!(fs::read(&path).unwrap(), b"");
    assert!(matches!(
        *input_mode.lock().unwrap(),
        RawInputMode::Delay { .. }
    ));
    fs::remove_file(path).ok();
}

#[test]
fn clearing_accepted_agent_prompt_emits_binding_dismissal() {
    let (path, mut master) = output_file("clear-agent");
    let (tx, rx) = mpsc::channel();
    let route = PromptGhostRoute::AgentIntercept {
        suggestion_id: Some("suggestion-1".to_string()),
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "analyze failure".to_string(),
        route: route.clone(),
    }));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_prompt_ghost_input(b"\t", "analyze failure", &route, &mut relay)
        .expect("accept agent ghost");
    relay_passthrough_input(&[0x15], &mut relay).expect("clear accepted prompt");

    assert!(rx
        .try_iter()
        .any(|event| event == RawInputEvent::PromptGhostDismissed));
    assert!(!line_buffer.is_active());
    fs::remove_file(path).ok();
}

#[test]
fn unsupported_arrow_after_agent_prompt_tab_cancels_without_writing_to_shell() {
    let (path, mut master) = output_file("agent-arrow-cancel");
    let (tx, rx) = mpsc::channel();
    let route = PromptGhostRoute::AgentIntercept {
        suggestion_id: Some("suggestion-1".to_string()),
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "analyze failure".to_string(),
        route: route.clone(),
    }));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_prompt_ghost_input(b"\t", "analyze failure", &route, &mut relay)
        .expect("accept agent ghost");
    relay_passthrough_input(b"\x1b[D", &mut relay).expect("cancel unsupported edit");
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::PromptGhostIntercept { .. })));
    assert!(!line_buffer.is_active());
    assert!(line_buffer.forced_agent_suggestion_id.is_none());
    fs::remove_file(path).ok();
}

#[test]
fn split_cursor_sequences_after_agent_prompt_tab_never_reach_shell() {
    for (name, sequence) in [
        ("left", b"\x1b[D".as_slice()),
        ("right", b"\x1b[C".as_slice()),
        ("home", b"\x1b[H".as_slice()),
        ("end", b"\x1b[F".as_slice()),
    ] {
        let (path, mut master) = output_file(&format!("agent-split-{name}"));
        let (tx, rx) = mpsc::channel();
        let route = PromptGhostRoute::AgentIntercept {
            suggestion_id: Some("suggestion-1".to_string()),
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
            text: "analyze failure".to_string(),
            route: route.clone(),
        }));
        let mut line_buffer = CandidateLineBuffer::default();
        let mut native_line_state = NativeLineState::default();
        let mut exit_tracker = ExplicitExitTracker::default();
        let classifier = InputClassifier::default();
        let input_generation = UserPtyInputGeneration::default();
        let mut line_submits = LineSubmitCounter::default();
        let main_prompt_gate = super::super::MainPromptGate::default();
        let mut relay = InputRelayContext {
            master: &mut master,
            input_classifier: &classifier,
            input_events: &tx,
            input_mode: &input_mode,
            input_generation: &input_generation,
            line_submits: &mut line_submits,
            line_buffer: &mut line_buffer,
            native_line_state: &mut native_line_state,
            exit_tracker: &mut exit_tracker,
            main_prompt_gate: &main_prompt_gate,
            slash_route_enabled: false,
            zsh_path_prompt_buffering: None,
        };

        relay_prompt_ghost_input(b"\t", "analyze failure", &route, &mut relay)
            .expect("accept agent ghost");
        for byte in sequence {
            relay_passthrough_input(&[*byte], &mut relay).expect("relay split sequence");
        }
        master.sync_all().expect("sync test output");

        let events = rx.try_iter().collect::<Vec<_>>();
        assert_eq!(fs::read(&path).expect("read test output"), b"");
        assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
        assert!(!events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptGhostIntercept { .. })));
        assert!(!line_buffer.is_active());
        fs::remove_file(path).ok();
    }
}

#[test]
fn clearing_and_submitting_in_one_buffer_dismisses_binding() {
    let (path, mut master) = output_file("clear-submit-agent");
    let (tx, rx) = mpsc::channel();
    let route = PromptGhostRoute::AgentIntercept {
        suggestion_id: Some("suggestion-1".to_string()),
    };
    let input_mode = Arc::new(Mutex::new(RawInputMode::PromptGhost {
        text: "analyze failure".to_string(),
        route: route.clone(),
    }));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    };

    relay_prompt_ghost_input(b"\t", "analyze failure", &route, &mut relay)
        .expect("accept agent ghost");
    relay_passthrough_input(b"\x15\n", &mut relay).expect("clear and submit");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.contains(&RawInputEvent::PromptGhostDismissed));
    assert!(!events
        .iter()
        .any(|event| matches!(event, RawInputEvent::PromptGhostIntercept { .. })));
    assert!(line_buffer.forced_agent_suggestion_id.is_none());
    assert_eq!(fs::read(&path).expect("read test output"), b"\n");
    fs::remove_file(path).ok();
}

#[allow(clippy::too_many_arguments)]
fn passthrough_relay_fixture<'a>(
    master: &'a mut File,
    tx: &'a mpsc::Sender<RawInputEvent>,
    input_mode: &'a Arc<Mutex<RawInputMode>>,
    line_buffer: &'a mut CandidateLineBuffer,
    native_line_state: &'a mut NativeLineState,
    exit_tracker: &'a mut ExplicitExitTracker,
    classifier: &'a InputClassifier,
    input_generation: &'a UserPtyInputGeneration,
    line_submits: &'a mut LineSubmitCounter,
    main_prompt_gate: &'a super::super::MainPromptGate,
) -> InputRelayContext<'a> {
    InputRelayContext {
        master,
        input_classifier: classifier,
        input_events: tx,
        input_mode,
        input_generation,
        line_submits,
        line_buffer,
        native_line_state,
        exit_tracker,
        main_prompt_gate,
        slash_route_enabled: false,
        zsh_path_prompt_buffering: None,
    }
}

#[test]
fn routing_c3_explicit_draft_upgrade_clears_the_shell_copy() {
    let (path, mut master) = output_file("soft-newline-agent");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input("请帮我分析系统负载".as_bytes(), &mut relay)
        .expect("relay Shell-owned Han input");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("soft newline");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| {
            matches!(
                event,
                RawInputEvent::PromptDraftOpen { text } if text == "请帮我分析系统负载\n"
            )
        }),
        "soft newline must upgrade the shell line: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::UserIntercept(..))),
        "upgrade must not submit early: {events:?}"
    );
    let mut expected = "请帮我分析系统负载".as_bytes().to_vec();
    expected.extend_from_slice(&[super::super::CTRL_U, b'\r']);
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    fs::remove_file(path).ok();
}

// Matrix #19 (#1721): a whitespace-only multi-line draft is consumed
// without submitting an empty prompt.
#[test]
fn routing_c3_explicit_whitespace_upgrade_uses_the_shell_mirror() {
    let (path, mut master) = output_file("soft-newline-empty");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    // Full-width space starts the intercept candidate (>= 0x80); the soft
    // newline upgrades even a whitespace-only draft into the card, where
    // Enter is inert on blank drafts (matrix #19 under D13).
    relay_passthrough_input("\u{3000}".as_bytes(), &mut relay).expect("start draft");
    relay_passthrough_input(b"\x1b\r", &mut relay).expect("soft newline");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::UserIntercept(..))),
        "whitespace draft must not submit: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text == "\u{3000}\n"
        )),
        "whitespace draft still opens the card: {events:?}"
    );
    let mut expected = "\u{3000}".as_bytes().to_vec();
    expected.extend_from_slice(&[super::super::CTRL_U, b'\r']);
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    assert!(!line_buffer.is_active());
    fs::remove_file(path).ok();
}

// Matrix #17 (#1721 T-b): composition-state hint appears once the draft
// contains a soft newline, and the redraw shows the marker instead of raw
// sequence bytes.
#[test]
fn routing_c3_explicit_upgrade_does_not_require_candidate_redraw() {
    let (path, mut master) = output_file("soft-newline-hint");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input("请帮我分析".as_bytes(), &mut relay).expect("draft");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("soft newline");

    // D13: no inline redraw survives the upgrade; the raw sequence must not
    // leak into any event payload either.
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text == "请帮我分析\n"
        )),
        "soft newline opens the card: {events:?}"
    );
    for event in &events {
        if let RawInputEvent::CandidateRedraw { input, .. } = event {
            let display = String::from_utf8_lossy(input).into_owned();
            assert!(!display.contains(";2u"), "no literal leak: {display}");
        }
    }
    let mut expected = "请帮我分析".as_bytes().to_vec();
    expected.extend_from_slice(&[super::super::CTRL_U, b'\r']);
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    fs::remove_file(path).ok();
}

// Matrix #18: a shortcut on the passthrough path is observed for the
// discoverability tip and stripped from the relayed bytes so bash never
// echoes the negotiated CSI tail as literal garbage (#1932).
#[test]
fn passthrough_shortcut_is_observed_and_stripped() {
    let (path, mut master) = output_file("soft-newline-observe");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"analyze load \x1b[13;2u tail", &mut relay)
        .expect("english input stays bash-owned");
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.contains(&RawInputEvent::SoftNewlineShortcutObserved));
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"analyze load  tail",
        "negotiated soft-newline sequences are stripped from bash-owned lines"
    );
    fs::remove_file(path).ok();
}

// Matrix #21 (#1721 G9): a native CJK draft without soft newlines flushes
// back to bash byte-for-byte (history/handoff unchanged).
#[test]
fn native_cjk_single_line_flushes_bytes_unchanged() {
    let (path, mut master) = output_file("native-cjk-single");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input("请帮我分析".as_bytes(), &mut relay).expect("cjk draft");
    relay_passthrough_input(b"\r", &mut relay).expect("submit single line");
    master.sync_all().expect("sync test output");

    let mut expected = "请帮我分析".as_bytes().to_vec();
    expected.push(b'\r');
    assert_eq!(
        fs::read(&path).expect("read test output"),
        expected,
        "single-line CJK must flush to bash byte-for-byte (G9 invariant)"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::UserIntercept(..))),
        "single line must not intercept: {events:?}"
    );
    fs::remove_file(path).ok();
}

// Matrix #22 (#1721 G9): a native CJK draft with a soft newline submits one
// multi-line NL prompt and never reaches bash.
#[test]
fn routing_c3_typed_cjk_then_explicit_upgrade_uses_shell_mirror() {
    let (path, mut master) = output_file("native-cjk-multi");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input("请帮我分析".as_bytes(), &mut relay).expect("cjk draft");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("soft newline");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text == "请帮我分析\n"
        )),
        "native CJK soft newline must open the card: {events:?}"
    );
    let mut expected = "请帮我分析".as_bytes().to_vec();
    expected.extend_from_slice(&[super::super::CTRL_U, b'\r']);
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    fs::remove_file(path).ok();
}

// Matrix #20 (#1721 G8): a native `??` draft keeps AgentMarker semantics
// when submitted as multi-line.
#[test]
fn native_agent_marker_multiline_keeps_reason() {
    let (path, mut master) = output_file("native-marker-multi");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"?? deploy plan", &mut relay).expect("marker draft");
    relay_passthrough_input(b"\x1b\r", &mut relay).expect("soft newline");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text == "deploy plan\n"
        )),
        "?? soft newline must open the card with the marker intact: {events:?}"
    );
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

// Matrix #24 (#1721): mid-line CJK (not at line start) stays bash-owned.
#[test]
fn native_midline_cjk_stays_passthrough() {
    let (path, mut master) = output_file("native-cjk-midline");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"echo ", &mut relay).expect("ascii prefix");
    relay_passthrough_input("中文".as_bytes(), &mut relay).expect("midline cjk");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let mut expected = b"echo ".to_vec();
    expected.extend_from_slice("中文".as_bytes());
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    assert!(!line_buffer.is_active());
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::CandidateRedraw { .. })),
        "mid-line CJK must not start a candidate: {events:?}"
    );
    fs::remove_file(path).ok();
}

// Matrix #23 (#1721): Tab inside a native CJK draft returns the draft to
// bash so native completion still works.
#[test]
fn native_cjk_tab_returns_draft_to_shell() {
    let (path, mut master) = output_file("native-cjk-tab");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input("分析 /tm".as_bytes(), &mut relay).expect("cjk draft");
    relay_passthrough_input(b"\t", &mut relay).expect("tab returns to shell");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let mut expected = "分析 /tm".as_bytes().to_vec();
    expected.push(b'\t');
    assert_eq!(
        fs::read(&path).expect("read test output"),
        expected,
        "tab must flush the draft so bash completion works"
    );
    assert!(!line_buffer.is_active());
    drop(rx);
    fs::remove_file(path).ok();
}

// Matrix #25/#26 (#1721 D16/I7): while bash owns a PS2 continuation or a
// heredoc body the main-prompt gate is down, so CJK line starts stay
// byte-for-byte passthrough exactly like before the fix.
#[test]
fn cjk_line_start_stays_passthrough_when_gate_is_down() {
    let (path, mut master) = output_file("cjk-gate-down");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    // Gate stays down: bash is inside a heredoc / PS2 continuation.
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input("中文配置内容".as_bytes(), &mut relay).expect("heredoc body");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("shortcut observed only");
    relay_passthrough_input(b"\r", &mut relay).expect("newline passthrough");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let mut expected = "中文配置内容".as_bytes().to_vec();
    expected.extend_from_slice(b"\x1b[13;2u\r");
    assert_eq!(
        fs::read(&path).expect("read test output"),
        expected,
        "gate-down CJK bytes must pass through unchanged (I7)"
    );
    assert!(!line_buffer.is_active());
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::CandidateRedraw { .. } | RawInputEvent::UserIntercept(..)
        )),
        "gate-down CJK must not open a candidate: {events:?}"
    );
    fs::remove_file(path).ok();
}

// #1721 D16: submitting a line lowers the gate until the next prompt_ready,
// so follow-up CJK bytes (e.g. heredoc body after `cat <<EOF`) pass through.
#[test]
fn submit_lowers_gate_until_next_prompt_ready() {
    let (path, mut master) = output_file("gate-lowered-by-submit");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"cat <<EOF\n", &mut relay).expect("start heredoc");
    assert!(
        !main_prompt_gate.is_at_prompt(),
        "a submitted line must lower the gate"
    );
    relay_passthrough_input("中文正文".as_bytes(), &mut relay).expect("heredoc body");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let mut expected = b"cat <<EOF\n".to_vec();
    expected.extend_from_slice("中文正文".as_bytes());
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    assert!(!line_buffer.is_active());
    drop(rx);
    fs::remove_file(path).ok();
}
#[test]
fn routing_c3_wrapped_multiline_paste_stays_shell_owned_across_chunks() {
    let (path, mut master) = output_file("native-paste-split");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    relay_passthrough_input("分析负载".as_bytes(), &mut relay).expect("payload head");
    relay_passthrough_input(b"\r\n", &mut relay).expect("pasted newline");
    relay_passthrough_input("给出建议".as_bytes(), &mut relay).expect("payload tail");
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.contains(&RawInputEvent::MultilinePasteObserved));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::UserIntercept(..))),
        "ordinary paste must not become Agent-owned: {events:?}"
    );
    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice("分析负载".as_bytes());
    expected.extend_from_slice(b"\r\n");
    expected.extend_from_slice("给出建议".as_bytes());
    expected.extend_from_slice(b"\x1b[201~");
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    fs::remove_file(path).ok();
}

// A pasted slash command remains pending until the explicit Enter after the
// closer, then the Rust relay owns it without relying on a shell DEBUG trap.
#[test]
fn routing_c3_wrapped_slash_paste_intercepts_on_explicit_enter() {
    let (path, mut master) = output_file("escape-slash-paste");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"\x1b[200~/mode approval", &mut relay)
        .expect("paste opener and partial payload");
    relay_passthrough_input(b" trust confirm\n\x1b[201~", &mut relay)
        .expect("paste payload and closer");
    assert!(rx
        .try_iter()
        .all(|event| !matches!(event, RawInputEvent::UserIntercept(..))));
    relay_passthrough_input(b"\n", &mut relay).expect("submit pasted slash");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                RawInputEvent::UserIntercept(input, InterceptReason::Slash)
                    if input == "/mode approval trust confirm"
            ))
            .count(),
        1,
        "submitted slash paste must stay Rust-owned: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftOpen { .. })),
        "slash paste must never open the draft card: {events:?}"
    );
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

// ASCII paste routing (#1721): a split opener remains Shell-owned and the
// wrapper reaches the PTY byte-for-byte, so bash buffers embedded newlines.
#[test]
fn native_split_ascii_paste_passes_wrapper_to_shell() {
    let (path, mut master) = output_file("native-paste-ascii-split");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener");
    relay_passthrough_input(b"touch probe\r\n", &mut relay).expect("payload");
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let written = fs::read(&path).expect("read test output");
    assert_eq!(written, b"\x1b[200~touch probe\r\n\x1b[201~");
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftOpen { .. })),
        "ASCII paste must not open the draft card: {events:?}"
    );
    fs::remove_file(path).ok();
}

#[test]
fn routing_c3_split_paste_delimiter_is_byte_identical_and_shell_owned() {
    let (path, mut master) = output_file("native-split-opener");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"\x1b[2", &mut relay).expect("split opener head");
    let mut tail = b"00~".to_vec();
    tail.extend_from_slice("分析负载".as_bytes());
    tail.extend_from_slice(b"\r\n");
    tail.extend_from_slice("给出建议".as_bytes());
    tail.extend_from_slice(b"\x1b[201~");
    relay_passthrough_input(&tail, &mut relay).expect("opener tail + payload");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let mut expected = b"\x1b[200~".to_vec();
    expected.extend_from_slice("分析负载".as_bytes());
    expected.extend_from_slice(b"\r\n");
    expected.extend_from_slice("给出建议".as_bytes());
    expected.extend_from_slice(b"\x1b[201~");
    assert_eq!(fs::read(&path).expect("read test output"), expected);
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftOpen { .. })),
        "ordinary split paste must stay Shell-owned: {events:?}"
    );
    assert!(events.contains(&RawInputEvent::MultilinePasteObserved));
    fs::remove_file(path).ok();
}

// Typed `??` ownership (#1932): key-by-key `?` chunks must own the line so
// a following multi-line paste composes in the draft card instead of
// leaking to bash line by line.
#[test]
fn native_typed_qq_then_paste_composes_in_card() {
    let (path, mut master) = output_file("native-typed-qq-paste");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"?", &mut relay).expect("first ? chunk");
    relay_passthrough_input(b"?", &mut relay).expect("second ? chunk");
    let mut paste = b"\x1b[200~Hello what can you do?\r".to_vec();
    paste.extend_from_slice(b"What's your name?\r\x1b[201~");
    relay_passthrough_input(&paste, &mut relay).expect("multi-line paste");
    let _ = relay;
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"",
        "typed ?? + paste must never leak to the shell"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text }
                if text == "Hello what can you do?\nWhat's your name?\n"
        )),
        "typed ?? paste must open the card with both lines: {events:?}"
    );
    fs::remove_file(path).ok();
}

// Lone `?` fail-closed (#1932): a `?` followed by shell input flushes back
// to bash byte-identically (glob usage stays native).
#[test]
fn native_lone_question_mark_flushes_back_to_shell() {
    let (path, mut master) = output_file("native-lone-question");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"?", &mut relay).expect("lone ? chunk");
    relay_passthrough_input(b"conf*\r", &mut relay).expect("glob tail");
    let _ = relay;
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"?conf*\r",
        "glob line must reach the shell byte-identically"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftOpen { .. })),
        "glob usage must not open the draft card: {events:?}"
    );
    fs::remove_file(path).ok();
}

#[test]
fn routing_c3_eof_candidate_prefix_is_cancelled_before_clean_exit() {
    for (label, bytes) in [
        ("question", b"?".as_slice()),
        ("slash", b"/mode".as_slice()),
    ] {
        let (path, mut master) = output_file(label);
        let (tx, rx) = mpsc::channel();
        let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
        let classifier = InputClassifier::default();
        let mut state = RawInputRelayState::default();

        relay_input_bytes(
            bytes,
            Instant::now(),
            &mut master,
            &tx,
            &classifier,
            &input_mode,
            &mut state,
        )
        .expect("buffer explicit prefix");
        finish_input_relay(&mut master, &tx, &classifier, &input_mode, &mut state)
            .expect("cancel prefix at EOF");

        assert_eq!(fs::read(&path).expect("read test output"), b"exit\n");
        let events = rx.try_iter().collect::<Vec<_>>();
        assert!(events.contains(&RawInputEvent::CandidateClearLine));
        assert!(!events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserIntercept(..) | RawInputEvent::EofShutdownRequested
        )));
        fs::remove_file(path).ok();
    }
}

// Lone `??` + Enter (#1932): the terminal-agnostic entry opens an empty
// draft card instead of submitting an empty agent prompt.
#[test]
fn native_lone_qq_enter_opens_empty_draft() {
    let (path, mut master) = output_file("native-lone-qq-enter");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"?", &mut relay).expect("first ? chunk");
    relay_passthrough_input(b"?", &mut relay).expect("second ? chunk");
    relay_passthrough_input(b"\r", &mut relay).expect("enter");
    let _ = relay;
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"",
        "?? + Enter must not reach the shell"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text.is_empty()
        )),
        "?? + Enter must open an empty draft card: {events:?}"
    );
    fs::remove_file(path).ok();
}

// Shift+Enter on a bash-owned english line (#1932 F6): the keypress is an
// explicit multi-line intent. With a clean observed mirror the line
// upgrades into the draft card and readline's copy is cleared with Ctrl-U.
#[test]
fn native_prompt_line_shortcut_upgrades_the_line() {
    let (path, mut master) = output_file("native-shortcut-upgrade");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"Hello", &mut relay).expect("english prefix");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("shift+enter upgrade");
    let _ = relay;
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"Hello\x15\r",
        "the upgrade clears readline's line and accepts it so bash repaints PS1"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text == "Hello\n"
        )),
        "the observed line must open the card with the cursor on line two: {events:?}"
    );
    fs::remove_file(path).ok();
}

// Dirty mirror fail-closed (#1932 F6): after Tab the observed line no
// longer matches readline, so the shortcut is stripped instead of
// upgrading and the discoverability tip still fires.
#[test]
fn native_dirty_line_shortcut_is_stripped() {
    let (path, mut master) = output_file("native-shortcut-strip");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"Hel\tlo\x1b[13;2u", &mut relay).expect("shortcut on edited line");
    let _ = relay;
    master.sync_all().expect("sync test output");

    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"Hel\tlo",
        "the negotiated sequence must not reach bash on the prompt line"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.contains(&RawInputEvent::SoftNewlineShortcutObserved));
    fs::remove_file(path).ok();
}

// Review regression (#1932): a multi-line bracketed paste keeps composing
// inside readline's buffer, which the single-line mirror cannot express.
// Shift+Enter must fail closed (strip, no upgrade) or the Ctrl-U would
// wipe the user's pasted lines.
#[test]
fn native_multiline_paste_marks_mirror_dirty() {
    let (path, mut master) = output_file("native-paste-dirty");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    // The paste wrapper and payload arrive in separate chunks, mirroring
    // real PTY fragmentation; the shell-command shape keeps the multi-line
    // paste on the native bash route (D13).
    relay_passthrough_input(b"\x1b[200~", &mut relay).expect("paste opener chunk");
    relay_passthrough_input(b"ls -l\rpwd", &mut relay).expect("paste payload chunk");
    relay_passthrough_input(b"\x1b[201~", &mut relay).expect("paste closer chunk");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("shortcut after paste");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let written = fs::read(&path).expect("read test output");
    assert!(
        !written.windows(4).any(|window| window == b"13;2"),
        "the sequence must be stripped, not leaked to bash: {written:?}"
    );
    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, RawInputEvent::PromptDraftOpen { .. })),
        "a poisoned mirror must never upgrade: {events:?}"
    );
    fs::remove_file(path).ok();
}

// Review regression (#1932): Delete at the end of the line is a readline
// no-op (the clean-mirror invariant keeps the cursor at EOL), so the
// mirror must not shrink and the upgrade still carries the full line.
#[test]
fn native_delete_at_line_end_keeps_the_mirror() {
    let (path, mut master) = output_file("native-delete-eol");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut relay = passthrough_relay_fixture(
        &mut master,
        &tx,
        &input_mode,
        &mut line_buffer,
        &mut native_line_state,
        &mut exit_tracker,
        &classifier,
        &input_generation,
        &mut line_submits,
        &main_prompt_gate,
    );

    relay_passthrough_input(b"Hello", &mut relay).expect("english prefix");
    relay_passthrough_input(b"\x1b[3~", &mut relay).expect("delete at eol");
    relay_passthrough_input(b"\x1b[13;2u", &mut relay).expect("shift+enter upgrade");
    let _ = relay;
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::PromptDraftOpen { text } if text == "Hello\n"
        )),
        "EOL Delete must not eat a mirrored char: {events:?}"
    );
    fs::remove_file(path).ok();
}

#[test]
fn native_exact_slash_routes_to_shell_when_at_prompt() {
    let label = "slash-route-exact";
    let (path, mut master) = output_file(label);
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default().with_bash_slash_submission_guard(true);
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: true,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(b"/mode approval\n", &mut relay).expect("route exact slash");
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    // Routed submissions leave interception to the shell marker: no
    // Rust-side intercept or commit events, bytes written to the PTY.
    assert!(events
        .iter()
        .all(|event| !matches!(event, RawInputEvent::UserIntercept(..))));
    assert!(events
        .iter()
        .all(|event| !matches!(event, RawInputEvent::CandidateCommit(..))));
    assert!(events.contains(&RawInputEvent::CandidateClearLine));
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"/mode approval\x1b[99~\n"
    );
    // The private no-op submission lowers the prompt gate until its bounded
    // prompt hook commits slash history and emits prompt_ready.
    assert!(!main_prompt_gate.is_at_prompt());
    assert!(matches!(
        *input_mode.lock().expect("input mode"),
        RawInputMode::Passthrough
    ));
    fs::remove_file(path).ok();
}

#[test]
fn private_history_submission_uses_private_slash_guard() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();
    assert_eq!(
        guarded_bash_submission(
            true,
            true,
            false,
            &classifier,
            &line_submits,
            b"\x01 \x05\n",
        ),
        Some(b"\x01 \x05\x1b[100~\n".to_vec())
    );
}

#[test]
fn recoverable_history_submission_uses_distinct_guard() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();
    assert_eq!(
        guarded_bash_submission(true, true, true, &classifier, &line_submits, b"\x01 \x05\n",),
        Some(b"\x01 \x05\x1b[101~\n".to_vec())
    );
}

#[test]
fn slash_guard_precedes_ctrl_o_submission() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();

    assert_eq!(
        guarded_bash_submission(
            true,
            false,
            false,
            &classifier,
            &line_submits,
            b"/skills detail xlsx\x0f",
        ),
        Some(b"/skills detail xlsx\x1b[99~\x0f".to_vec())
    );
}

#[test]
fn slash_guard_ignores_pasted_newlines() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();
    let paste = b"\x1b[200~/skills detail\nnext line\x1b[201~";

    assert_eq!(
        guarded_bash_submission(true, false, false, &classifier, &line_submits, paste),
        None
    );

    let mut submitted = paste.to_vec();
    submitted.push(b'\r');
    let mut expected = paste.to_vec();
    expected.extend_from_slice(b"\x1b[99~\r");
    assert_eq!(
        guarded_bash_submission(true, false, false, &classifier, &line_submits, &submitted,),
        Some(expected)
    );
}

#[test]
fn slash_guard_does_not_infer_follow_up_readline_ownership() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();

    assert_eq!(
        guarded_bash_submission(
            true,
            false,
            false,
            &classifier,
            &line_submits,
            b"/mode approval\r\x1b[D/skills detail $(touch should-not-run)\r",
        ),
        Some(b"/mode approval\x1b[99~\r\x1b[D/skills detail $(touch should-not-run)\r".to_vec())
    );
}

#[test]
fn slash_guard_stops_after_ordinary_shell_submission() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();

    assert_eq!(
        guarded_bash_submission(
            true,
            false,
            false,
            &classifier,
            &line_submits,
            b"/mode approval\necho ordinary\n/skills detail $(touch should-not-run)\n",
        ),
        Some(
            b"/mode approval\x1b[99~\necho ordinary\n/skills detail $(touch should-not-run)\n"
                .to_vec()
        )
    );
}

#[test]
fn slash_guard_does_not_trust_blank_bytes_with_dirty_mirror() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();

    // `required` may represent an ordinary command recalled outside the
    // mirror. The raw blank only submits that unknown line; it does not prove
    // that a later slash is still owned by Readline.
    assert_eq!(
        guarded_bash_submission(
            true,
            false,
            false,
            &classifier,
            &line_submits,
            b"\n/skills detail $(touch should-not-run)\n",
        ),
        Some(b"\x1b[99~\n/skills detail $(touch should-not-run)\n".to_vec())
    );
}

#[test]
fn bash_slash_guard_skips_clean_ordinary_and_empty_submissions() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();
    let mut state = NativeLineState::default();

    state.observe_shell_bytes(b"echo ordinary");
    assert!(!bash_submission_needs_guard(
        true,
        true,
        &classifier,
        &state,
        &line_submits,
        b"\r",
    ));
    state.clear();
    assert!(!bash_submission_needs_guard(
        true,
        true,
        &classifier,
        &state,
        &line_submits,
        b"\r",
    ));
}

#[test]
fn bash_slash_guard_keeps_exact_and_readline_owned_submissions() {
    let classifier = InputClassifier::default();
    let line_submits = LineSubmitCounter::default();
    let mut state = NativeLineState::default();

    assert!(bash_submission_needs_guard(
        true,
        true,
        &classifier,
        &state,
        &line_submits,
        b"/help\r",
    ));
    state.observe_shell_bytes(b"\x1b[A");
    assert!(bash_submission_needs_guard(
        true,
        true,
        &classifier,
        &state,
        &line_submits,
        b"\r",
    ));
}

#[test]
fn native_exact_slash_falls_back_to_rust_intercept_when_not_at_prompt() {
    let (path, mut master) = output_file("slash-route-not-at-prompt");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let main_prompt_gate = super::super::MainPromptGate::default();
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: true,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(b"/mode approval\n", &mut relay).expect("fall back to intercept");
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    // Without a proven prompt the routing must not leak bytes into the PTY
    // (a foreground REPL could own it): the Rust intercept path stays.
    assert!(events.iter().any(|event| matches!(
        event,
        RawInputEvent::UserIntercept(input, InterceptReason::Slash) if input == "/mode approval"
    )));
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    fs::remove_file(path).ok();
}

#[test]
fn native_shell_submission_lowers_gate_before_follow_up_slash() {
    // Regression for the review P1 (#1922): a plain shell submission (e.g.
    // one that starts a REPL) must lower the prompt gate synchronously, so a
    // slash typed before the parser observes preexec still takes the Rust
    // intercept path instead of leaking into the foreground process.
    let (path, mut master) = output_file("slash-route-after-shell-submit");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: true,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(b"python\n/mode approval\n", &mut relay)
        .expect("submit shell command and queued slash");
    assert!(
        !main_prompt_gate.is_at_prompt(),
        "plain submission must lower the gate synchronously"
    );
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(
        event,
        RawInputEvent::UserInterceptAtPrompt {
            input,
            reason: InterceptReason::Slash,
            pending_submits: 1,
        } if input == "/mode approval"
    )));
    // Only the shell command reaches the PTY; the slash stays Rust-side.
    assert_eq!(fs::read(&path).expect("read test output"), b"python\n");
    fs::remove_file(path).ok();
}

#[test]
fn same_batch_follow_up_slash_stays_batch_owned_and_guarded() {
    let (path, mut master) = output_file("same-batch-follow-up-slash");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default().with_bash_slash_submission_guard(true);
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: true,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(
        b"/mode approval\n/skills detail $(touch should-not-run)\n",
        &mut relay,
    )
    .expect("submit guarded slash and queued follow-up slash");
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    assert!(!events.iter().any(|event| matches!(
        event,
        RawInputEvent::UserIntercept(input, InterceptReason::Slash)
            | RawInputEvent::UserInterceptAtPrompt {
                input,
                reason: InterceptReason::Slash,
                ..
            } if input == "/skills detail $(touch should-not-run)"
    )));
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"/mode approval\x1b[99~\n/skills detail $(touch should-not-run)\x1b[99~\n",
        "the whole shell-owned batch must keep every exact slash guarded"
    );
    fs::remove_file(path).ok();
}

#[test]
fn chunked_native_submission_defers_follow_up_control() {
    let (path, master) = output_file("chunked-native-submit-before-control");
    let (input_tx, input_rx) = mpsc::channel();
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let relay = super::super::spawn_raw_input_relay(
        ChannelReader {
            receiver: input_rx,
            pending: Vec::new(),
        },
        master.try_clone().expect("clone output file"),
        event_tx,
        InputClassifier::default(),
        input_mode,
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    input_tx
        .send(b"echo ok".to_vec())
        .expect("send partial line");
    let mut events = Vec::new();
    loop {
        let event = event_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("partial line relay event");
        let relayed = matches!(
            event,
            RawInputEvent::PtyUserWrite {
                line_submits: 0,
                ..
            }
        );
        events.push(event);
        if relayed {
            break;
        }
    }
    input_tx
        .send(b"\n/mode routing shell-only\n".to_vec())
        .expect("send submit and control");
    drop(input_tx);
    relay.join().expect("relay thread").expect("relay result");
    events.extend(event_rx.try_iter());

    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserInterceptAtPrompt {
                input,
                reason: InterceptReason::Slash,
                pending_submits: 1,
            } if input == "/mode routing shell-only"
        )),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserIntercept(input, InterceptReason::Slash)
                if input == "/mode routing shell-only"
        )),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"echo ok\nexit\n"
    );
    fs::remove_file(path).ok();
}

#[test]
fn chunked_action_submission_defers_follow_up_control() {
    let (path, master) = output_file("chunked-action-submit-before-control");
    let (event_tx, event_rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let relay = super::super::spawn_raw_action_relay(
        vec![
            RawRelayAction::write(b"echo ok"),
            RawRelayAction::write(b"\n/mode routing shell-only\n"),
        ],
        master.try_clone().expect("clone output file"),
        0,
        event_tx,
        InputClassifier::default(),
        input_mode,
        UserPtyInputGeneration::default(),
        super::super::MainPromptGate::default(),
        false,
    );

    relay.join().expect("relay thread").expect("relay result");
    let events = event_rx.try_iter().collect::<Vec<_>>();
    assert!(
        events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserInterceptAtPrompt {
                input,
                reason: InterceptReason::Slash,
                pending_submits: 1,
            } if input == "/mode routing shell-only"
        )),
        "{events:?}"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            RawInputEvent::UserIntercept(input, InterceptReason::Slash)
                if input == "/mode routing shell-only"
        )),
        "{events:?}"
    );
    master.sync_all().expect("sync test output");
    assert_eq!(
        fs::read(&path).expect("read test output"),
        b"echo ok\nexit\n"
    );
    fs::remove_file(path).ok();
}

#[test]
fn native_hint_prefix_slash_keeps_rust_intercept_when_routed() {
    let (path, mut master) = output_file("slash-route-hint-prefix");
    let (tx, rx) = mpsc::channel();
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    let main_prompt_gate = super::super::MainPromptGate::default();
    main_prompt_gate.set_at_prompt(true);
    let mut line_buffer = CandidateLineBuffer::default();
    let mut native_line_state = NativeLineState::default();
    let mut exit_tracker = ExplicitExitTracker::default();
    let classifier = InputClassifier::default();
    let input_generation = UserPtyInputGeneration::default();
    let mut line_submits = LineSubmitCounter::default();
    let mut relay = InputRelayContext {
        master: &mut master,
        input_classifier: &classifier,
        input_events: &tx,
        input_mode: &input_mode,
        input_generation: &input_generation,
        line_submits: &mut line_submits,
        line_buffer: &mut line_buffer,
        native_line_state: &mut native_line_state,
        exit_tracker: &mut exit_tracker,
        main_prompt_gate: &main_prompt_gate,
        slash_route_enabled: true,
        zsh_path_prompt_buffering: None,
    };

    relay_passthrough_input(b"/sk\n", &mut relay).expect("hint prefix intercept");
    master.sync_all().expect("sync test output");

    let events = rx.try_iter().collect::<Vec<_>>();
    // Hint prefixes are not in the shell marker's exact case lists, so
    // routing them would make bash execute the line; they keep the Rust
    // intercept path and never enter history.
    assert!(events.iter().any(|event| matches!(
        event,
        RawInputEvent::UserIntercept(input, InterceptReason::Slash) if input == "/sk"
    )));
    assert_eq!(fs::read(&path).expect("read test output"), b"");
    // A non-routed submission does not touch the prompt gate.
    assert!(main_prompt_gate.is_at_prompt());
    fs::remove_file(path).ok();
}
