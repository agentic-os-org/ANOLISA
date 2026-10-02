//! Test-only handshake for marker-gated raw CLI input.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::raw_input::RawInputMode;

const REQUEST_PATH_ENV: &str = "COSH_RAW_CLI_TEST_INPUT_READY_REQUEST";
const TOKEN_ENV: &str = "COSH_RAW_CLI_TEST_INPUT_READY_TOKEN";

pub(super) struct RawInputReadinessProbe {
    request_path: Option<PathBuf>,
    token: String,
    acknowledged: u64,
}

impl RawInputReadinessProbe {
    pub(super) fn from_env() -> Self {
        let request_path = std::env::var_os(REQUEST_PATH_ENV).map(PathBuf::from);
        let token = std::env::var(TOKEN_ENV)
            .ok()
            .filter(|token| valid_token(token))
            .unwrap_or_default();
        Self {
            request_path: request_path.filter(|_| !token.is_empty()),
            token,
            acknowledged: 0,
        }
    }

    pub(super) fn acknowledge_if_ready<W: Write>(
        &mut self,
        output: &mut W,
        input_mode: &Arc<Mutex<RawInputMode>>,
    ) -> io::Result<()> {
        let Some(request_path) = self.request_path.as_ref() else {
            return Ok(());
        };
        let request = match fs::read_to_string(request_path) {
            Ok(request) => request,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let Ok(request) = request.trim().parse::<u64>() else {
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
        write!(output, "\x1e{}:{request}:{readiness}\x1f", self.token)?;
        output.flush()?;
        self.acknowledged = request;
        Ok(())
    }
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::raw_input::RawInputCapture;

    use super::*;

    #[test]
    fn acknowledges_only_after_the_next_capture_is_installed() {
        let request_path = std::env::temp_dir().join(format!(
            "cosh-input-ready-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::write(&request_path, "1").expect("readiness request");
        let capture = RawInputCapture::Consultation {
            id: "consult-1".to_string(),
        };
        let input_mode = Arc::new(Mutex::new(RawInputMode::Submitted {
            capture: capture.clone(),
            generation: 7,
        }));
        let mut probe = RawInputReadinessProbe {
            request_path: Some(request_path.clone()),
            token: "test-token".to_string(),
            acknowledged: 0,
        };
        let mut output = Vec::new();

        probe
            .acknowledge_if_ready(&mut output, &input_mode)
            .expect("transitional mode");
        assert!(output.is_empty());

        *input_mode.lock().expect("input mode") = RawInputMode::Capture {
            capture,
            generation: 8,
            installed_at: std::time::Instant::now(),
        };
        probe
            .acknowledge_if_ready(&mut output, &input_mode)
            .expect("ready capture");

        assert_eq!(output, b"\x1etest-token:1:capture=8\x1f");
        let _ = fs::remove_file(request_path);
    }
}
