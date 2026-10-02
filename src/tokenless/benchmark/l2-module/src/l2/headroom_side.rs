// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Headroom side: a resident Python worker process speaking line-delimited
//! JSON over stdin/stdout.
//!
//! Headroom is a Python+PyO3 library and cannot be linked into this harness,
//! so a long-lived subprocess (`$HEADROOM_PYTHON assets/worker/headroom_worker.py`)
//! amortises interpreter and import cost across all requests; per-request
//! latency then reflects compression, not process startup.
//!
//! Latency basis: **worker-internal** (`perf_counter` inside the worker
//! around `router.compress` only — pipe and JSON framing excluded).

use crate::l2::L2Error;
use serde::Deserialize;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

/// Latency-basis label stamped on every headroom-side result row.
pub const LATENCY_BASIS: &str = "worker-internal";

#[derive(Deserialize)]
struct Handshake {
    ready: bool,
    #[serde(default)]
    error: Option<String>,
    /// Commit of the headroom checkout the worker imported, when it could be
    /// determined (absent for wheel installs or when git is unavailable).
    #[serde(default)]
    revision: Option<String>,
    /// Whether that checkout had uncommitted changes.
    #[serde(default)]
    dirty: Option<bool>,
    /// Count of untracked files in that checkout (an editable install imports
    /// whatever sits in its source dir, so untracked modules affect the run).
    #[serde(default)]
    untracked: Option<usize>,
}

/// Provenance of the comparator the worker actually loaded.
///
/// Recorded in the report header so two runs that differ only in the headroom
/// build cannot produce identical-looking provenance.
#[derive(Debug, Clone, Default)]
pub struct HeadroomProvenance {
    pub revision: Option<String>,
    pub dirty: Option<bool>,
    pub untracked: Option<usize>,
}

/// One worker response line. `error` is set instead of `compressed` when the
/// worker caught an exception for that request; the worker stays alive.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkerResponse {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub compressed: Option<String>,
    #[serde(default)]
    pub strategy_used: Option<String>,
    #[serde(default)]
    pub wall_time_s: Option<f64>,
    /// Headroom's own before/after token counts — cross-check evidence only;
    /// authoritative counts come from tiktoken-rs on the Rust side.
    #[serde(default)]
    pub hr_tokens_before: Option<u64>,
    #[serde(default)]
    pub hr_tokens_after: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// A running headroom worker. Dropping it closes stdin (the protocol's
/// shutdown signal) and reaps the child.
pub struct HeadroomWorker {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_seq: u64,
    provenance: HeadroomProvenance,
}

// Manual impl: `ChildStdin`/`BufReader` carry no useful state to print, and
// tests need `Result<HeadroomWorker, _>::expect_err`, which requires `Debug`.
impl std::fmt::Debug for HeadroomWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeadroomWorker")
            .field("pid", &self.child.id())
            .field("next_seq", &self.next_seq)
            .finish_non_exhaustive()
    }
}

impl HeadroomWorker {
    /// Spawns the worker under `python_bin` and waits for the handshake.
    ///
    /// # Errors
    ///
    /// Returns [`L2Error::HeadroomUnavailable`] on spawn failure, a
    /// `{"ready": false}` handshake, or EOF before any handshake — every
    /// case where the caller should degrade to a one-sided run rather than
    /// abort.
    pub fn spawn(python_bin: &str, worker_script: &Path) -> Result<Self, L2Error> {
        let mut child = Command::new(python_bin)
            .arg(worker_script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Let worker stderr flow through to the harness stderr so import
            // tracebacks are visible when diagnosing an unavailable side.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                L2Error::HeadroomUnavailable(format!("spawn {python_bin:?} failed: {e}"))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| L2Error::HeadroomUnavailable("worker stdin pipe missing".to_string()))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            L2Error::HeadroomUnavailable("worker stdout pipe missing".to_string())
        })?;
        let mut reader = BufReader::new(stdout);

        let mut line = String::new();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(L2Error::HeadroomUnavailable(
                "worker exited before handshake".to_string(),
            ));
        }
        let hs: Handshake = serde_json::from_str(line.trim()).map_err(|e| {
            L2Error::HeadroomUnavailable(format!("bad handshake line {line:?}: {e}"))
        })?;
        if !hs.ready {
            let _ = child.wait();
            return Err(L2Error::HeadroomUnavailable(
                hs.error
                    .unwrap_or_else(|| "worker reported ready=false".to_string()),
            ));
        }

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout: reader,
            next_seq: 0,
            provenance: HeadroomProvenance {
                revision: hs.revision,
                dirty: hs.dirty,
                untracked: hs.untracked,
            },
        })
    }

    /// Provenance of the headroom build this worker imported.
    pub fn provenance(&self) -> &HeadroomProvenance {
        &self.provenance
    }

    /// Sends one compression request and blocks for the matching response.
    ///
    /// Ids are generated internally and verified on the response, so a
    /// desynchronised worker is detected instead of silently mispairing
    /// measurements.
    ///
    /// # Errors
    ///
    /// [`L2Error::Protocol`] on id mismatch, per-request worker errors, or a
    /// closed pipe; [`L2Error::Io`]/[`L2Error::Json`] on transport issues.
    pub fn compress(&mut self, content: &str, context: &str) -> Result<WorkerResponse, L2Error> {
        self.next_seq += 1;
        let id = format!("s{}", self.next_seq);
        let request = serde_json::json!({ "id": id, "content": content, "context": context });
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| L2Error::Protocol("worker stdin already closed".to_string()))?;
        stdin.write_all(serde_json::to_string(&request)?.as_bytes())?;
        stdin.write_all(b"\n")?;
        stdin.flush()?;

        let mut line = String::new();
        let n = self.stdout.read_line(&mut line)?;
        if n == 0 {
            return Err(L2Error::Protocol(
                "worker closed stdout mid-session".to_string(),
            ));
        }
        let resp: WorkerResponse = serde_json::from_str(line.trim())?;
        if let Some(err) = &resp.error {
            return Err(L2Error::Protocol(format!("worker error for {id}: {err}")));
        }
        if resp.id.as_deref() != Some(id.as_str()) {
            return Err(L2Error::Protocol(format!(
                "response id {:?} does not match request id {id:?}",
                resp.id
            )));
        }
        if resp.compressed.is_none() {
            return Err(L2Error::Protocol(format!(
                "response for {id} lacks `compressed`"
            )));
        }
        Ok(resp)
    }
}

impl Drop for HeadroomWorker {
    fn drop(&mut self) {
        // Dropping stdin sends EOF — the protocol's shutdown signal — then
        // reap so no zombie outlives the harness.
        self.stdin.take();
        let _ = self.child.wait();
    }
}
