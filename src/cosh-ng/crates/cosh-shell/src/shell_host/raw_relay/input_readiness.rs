//! Test-only handshake for marker-gated raw CLI input.
//!
//! Two channels share one token and the frame layout
//! `\x1e{token}:{channel}:{request}:{readiness}\x1f`:
//!
//! - `input` acknowledges the raw input mode alone. It keeps working while a
//!   foreground program owns the terminal, so marker-gated fixtures can type
//!   into a pager, ssh or repl.
//! - `routing` additionally requires the path-prompt routing gate and a prompt
//!   newer than the previous routing acknowledgement. Fixtures use it to type
//!   path fragments only once a submit was consumed and the next prompt is up.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::raw_input::{MainPromptGate, RawInputMode};

const REQUEST_PATH_ENV: &str = "COSH_RAW_CLI_TEST_INPUT_READY_REQUEST";
const ROUTING_REQUEST_PATH_ENV: &str = "COSH_RAW_CLI_TEST_ROUTING_READY_REQUEST";
const TOKEN_ENV: &str = "COSH_RAW_CLI_TEST_INPUT_READY_TOKEN";

pub(super) struct RawInputReadinessProbe {
    request_path: Option<PathBuf>,
    routing_request_path: Option<PathBuf>,
    token: String,
    acknowledged: u64,
    routing_acknowledged: u64,
    routing_epoch: Option<u64>,
}

impl RawInputReadinessProbe {
    pub(super) fn from_env() -> Self {
        let token = std::env::var(TOKEN_ENV)
            .ok()
            .filter(|token| valid_token(token))
            .unwrap_or_default();
        let configured = !token.is_empty();
        Self {
            request_path: std::env::var_os(REQUEST_PATH_ENV)
                .map(PathBuf::from)
                .filter(|_| configured),
            routing_request_path: std::env::var_os(ROUTING_REQUEST_PATH_ENV)
                .map(PathBuf::from)
                .filter(|_| configured),
            token,
            acknowledged: 0,
            routing_acknowledged: 0,
            routing_epoch: None,
        }
    }

    pub(super) fn acknowledge_if_ready<W: Write>(
        &mut self,
        output: &mut W,
        input_mode: &Arc<Mutex<RawInputMode>>,
        main_prompt_gate: &MainPromptGate,
    ) -> io::Result<()> {
        self.acknowledge_input_ready(output, input_mode)?;
        self.acknowledge_routing_ready(output, input_mode, main_prompt_gate)
    }

    fn acknowledge_input_ready<W: Write>(
        &mut self,
        output: &mut W,
        input_mode: &Arc<Mutex<RawInputMode>>,
    ) -> io::Result<()> {
        let Some(request_path) = self.request_path.as_ref() else {
            return Ok(());
        };
        let Some(request) = read_request(request_path)? else {
            return Ok(());
        };
        if request <= self.acknowledged {
            return Ok(());
        }
        let mode = input_mode
            .lock()
            .map_err(|_| io::Error::other("raw input mode lock poisoned"))?;
        let Some(readiness) = ready_mode(&mode) else {
            return Ok(());
        };
        write!(output, "\x1e{}:input:{request}:{readiness}\x1f", self.token)?;
        output.flush()?;
        self.acknowledged = request;
        Ok(())
    }

    fn acknowledge_routing_ready<W: Write>(
        &mut self,
        output: &mut W,
        input_mode: &Arc<Mutex<RawInputMode>>,
        main_prompt_gate: &MainPromptGate,
    ) -> io::Result<()> {
        let Some(request_path) = self.routing_request_path.as_ref() else {
            return Ok(());
        };
        let Some(request) = read_request(request_path)? else {
            return Ok(());
        };
        if request <= self.routing_acknowledged {
            return Ok(());
        }
        // prompt_ready raises the gate and a pending submit lowers it again, so
        // the open gate alone may still belong to the prompt before the submit.
        if !main_prompt_gate.is_path_prompt_ready() {
            return Ok(());
        }
        let epoch = main_prompt_gate.prompt_epoch();
        if self
            .routing_epoch
            .is_some_and(|acknowledged| epoch <= acknowledged)
        {
            return Ok(());
        }
        let mode = input_mode
            .lock()
            .map_err(|_| io::Error::other("raw input mode lock poisoned"))?;
        let Some(readiness) = ready_mode(&mode) else {
            return Ok(());
        };
        write!(
            output,
            "\x1e{}:routing:{request}:{readiness}\x1f",
            self.token
        )?;
        output.flush()?;
        self.routing_acknowledged = request;
        self.routing_epoch = Some(epoch);
        Ok(())
    }
}

fn read_request(path: &Path) -> io::Result<Option<u64>> {
    let request = match fs::read_to_string(path) {
        Ok(request) => request,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(request.trim().parse::<u64>().ok())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= 96
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn ready_mode(mode: &RawInputMode) -> Option<String> {
    match mode {
        RawInputMode::Passthrough => Some("passthrough".to_string()),
        RawInputMode::RawPassthrough => Some("raw-passthrough".to_string()),
        RawInputMode::Hold => Some("hold".to_string()),
        RawInputMode::Delay { generation } => Some(format!("delay={generation}")),
        RawInputMode::PromptGhost { .. } => Some("prompt-ghost".to_string()),
        RawInputMode::Capture { generation, .. } => Some(format!("capture={generation}")),
        RawInputMode::Submitted { .. }
        | RawInputMode::Draining { .. }
        | RawInputMode::Terminal { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::raw_input::RawInputCapture;

    use super::*;

    /// Parallel unit tests can sample the same clock value; without a counter
    /// they would share one fixture directory and overwrite each other's
    /// request files.
    static NEXT_FIXTURE_DIR: AtomicU64 = AtomicU64::new(0);

    struct ProbeFixture {
        directory: PathBuf,
        probe: RawInputReadinessProbe,
        input_mode: Arc<Mutex<RawInputMode>>,
    }

    impl ProbeFixture {
        fn new() -> Self {
            let directory = std::env::temp_dir().join(format!(
                "cosh-input-ready-{}-{}-{}",
                std::process::id(),
                NEXT_FIXTURE_DIR.fetch_add(1, Ordering::Relaxed),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            fs::create_dir_all(&directory).expect("readiness directory");
            let probe = RawInputReadinessProbe {
                request_path: Some(directory.join("request")),
                routing_request_path: Some(directory.join("routing-request")),
                token: "test-token".to_string(),
                acknowledged: 0,
                routing_acknowledged: 0,
                routing_epoch: None,
            };
            let capture = RawInputCapture::Consultation {
                id: "consult-1".to_string(),
            };
            let input_mode = Arc::new(Mutex::new(RawInputMode::Submitted {
                capture,
                generation: 7,
            }));
            Self {
                directory,
                probe,
                input_mode,
            }
        }

        fn request(&self, number: u64) {
            fs::write(
                self.probe.request_path.as_ref().expect("request path"),
                number.to_string(),
            )
            .expect("input readiness request");
        }

        fn routing_request(&self, number: u64) {
            fs::write(
                self.probe
                    .routing_request_path
                    .as_ref()
                    .expect("routing request path"),
                number.to_string(),
            )
            .expect("routing readiness request");
        }

        fn install_ready_capture(&self) {
            let capture = {
                let mode = self.input_mode.lock().expect("input mode");
                let RawInputMode::Submitted { capture, .. } = &*mode else {
                    panic!("fixture starts in a submitted mode");
                };
                capture.clone()
            };
            *self.input_mode.lock().expect("input mode") = RawInputMode::Capture {
                capture,
                generation: 8,
                installed_at: std::time::Instant::now(),
            };
        }

        fn acknowledge(&mut self, gate: &MainPromptGate) -> Vec<u8> {
            let mut output = Vec::new();
            self.probe
                .acknowledge_if_ready(&mut output, &self.input_mode, gate)
                .expect("readiness probe");
            output
        }
    }

    impl Drop for ProbeFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn input_ready_ignores_the_routing_gate() {
        let mut fixture = ProbeFixture::new();
        fixture.install_ready_capture();
        fixture.request(1);
        // A foreground program holds the routing gate down for its whole
        // lifetime; typing into it must still be acknowledged.
        let gate = MainPromptGate::default();

        assert_eq!(
            fixture.acknowledge(&gate),
            b"\x1etest-token:input:1:capture=8\x1f"
        );
    }

    #[test]
    fn input_ready_waits_for_a_transitional_mode() {
        let mut fixture = ProbeFixture::new();
        fixture.request(1);
        let gate = MainPromptGate::default();
        gate.set_at_prompt(true);

        assert!(fixture.acknowledge(&gate).is_empty());

        fixture.install_ready_capture();
        assert_eq!(
            fixture.acknowledge(&gate),
            b"\x1etest-token:input:1:capture=8\x1f"
        );
    }

    #[test]
    fn routing_ready_needs_an_open_gate() {
        let mut fixture = ProbeFixture::new();
        fixture.install_ready_capture();
        fixture.routing_request(1);
        let gate = MainPromptGate::default();

        assert!(fixture.acknowledge(&gate).is_empty());

        gate.set_at_prompt(true);
        assert_eq!(
            fixture.acknowledge(&gate),
            b"\x1etest-token:routing:1:capture=8\x1f"
        );
    }

    #[test]
    fn routing_ready_ignores_a_prompt_acknowledged_before_the_request() {
        let mut fixture = ProbeFixture::new();
        fixture.install_ready_capture();
        let gate = MainPromptGate::default();
        gate.set_at_prompt(true);

        fixture.routing_request(1);
        assert_eq!(
            fixture.acknowledge(&gate),
            b"\x1etest-token:routing:1:capture=8\x1f"
        );

        // The submit is still pending in the input relay, so the gate keeps the
        // previous prompt's state. A request written after it must not ack.
        fixture.routing_request(2);
        assert!(fixture.acknowledge(&gate).is_empty());

        gate.set_at_prompt(false);
        gate.set_at_prompt(true);
        assert_eq!(
            fixture.acknowledge(&gate),
            b"\x1etest-token:routing:2:capture=8\x1f"
        );
    }
}
