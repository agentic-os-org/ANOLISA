//! Bounded transport for the public AW audit-export CLI.

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const VERSION: &str = "audit-export.aw/v1alpha1";
const MAX_OUTPUT: u64 = 1024 * 1024 + 1;
const MAX_WAIT: Duration = Duration::from_secs(2);

/// Explicit same-user AW installation and retained state directory.
pub struct Export {
    binary: PathBuf,
    state: PathBuf,
    scratch: tempfile::TempDir,
}

#[derive(Deserialize)]
pub(super) struct Envelope {
    api_version: String,
    pub(super) source_id: Option<String>,
    pub(super) result: Option<Reply>,
    pub(super) error: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Reply {
    Enumerate {
        keys: Vec<String>,
        next_cursor: Option<Value>,
    },
    Read {
        event: Value,
        records: Vec<Value>,
        chain_id: String,
        cursor: Value,
        tip: Value,
        caught_up: bool,
        terminal: bool,
    },
}

impl Export {
    /// Select two absolute paths and create a private directory for request files.
    pub fn new(binary: PathBuf, state: PathBuf) -> Result<Self> {
        ensure!(
            binary.is_absolute() && state.is_absolute(),
            "AW audit paths must be absolute"
        );
        Ok(Self {
            binary,
            state,
            scratch: tempfile::tempdir()?,
        })
    }

    pub(super) fn selection(&self) -> (&Path, &Path) {
        (&self.binary, &self.state)
    }

    pub(super) fn call(&self, operation: Value) -> Result<Envelope> {
        let request = self.scratch.path().join("request.json");
        std::fs::write(
            &request,
            serde_json::to_vec(&json!({
                "api_version": VERSION, "operation": operation,
            }))?,
        )?;
        let output = tempfile::tempfile_in(self.scratch.path())?;
        let mut child = Command::new(&self.binary)
            .arg("audit-export")
            .arg("--state-dir")
            .arg(&self.state)
            .arg("--request")
            .arg(&request)
            .args(["--timeout-ms", "1000"])
            .stdin(Stdio::null())
            .stdout(output.try_clone()?)
            .stderr(Stdio::null())
            .spawn()
            .context("start AW audit export")?;
        let deadline = Instant::now() + MAX_WAIT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
                outcome => {
                    let _ = child.kill();
                    let _ = child.wait();
                    outcome.context("wait for AW audit export")?;
                    anyhow::bail!("AW audit export timed out");
                }
            }
        };
        ensure!(
            matches!(status.code(), Some(0 | 1)),
            "AW audit export exited unexpectedly"
        );
        let mut bytes = Vec::new();
        use std::io::{Seek, SeekFrom};
        let mut output = output;
        output.seek(SeekFrom::Start(0))?;
        output.take(MAX_OUTPUT + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_OUTPUT,
            "AW audit response exceeds byte limit"
        );
        let reply: Envelope = serde_json::from_slice(&bytes).context("decode AW audit response")?;
        ensure!(
            reply.api_version == VERSION,
            "unsupported AW audit response version"
        );
        ensure!(
            reply.result.is_some() == status.success() && reply.error.is_some() != status.success(),
            "AW audit response contradicts its exit status"
        );
        ensure!(
            reply.source_id.as_deref().is_none_or(is_identity),
            "invalid AW audit source"
        );
        Ok(reply)
    }
}

pub(super) fn is_identity(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(super) fn check_anchor(anchor: &Value) -> Result<u64> {
    ensure!(
        anchor["digest"].as_str().is_some_and(is_identity),
        "invalid AW chain digest"
    );
    anchor["sequence"]
        .as_u64()
        .filter(|sequence| *sequence < 4096)
        .context("invalid AW chain sequence")
}
