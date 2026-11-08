//! Bounded, structured access to the read-only ws-ckpt CLI operations.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_CHARS: usize = 4_000;

/// A ws-ckpt invocation or response-contract failure.
#[derive(Debug, thiserror::Error)]
pub(crate) enum WsCkptError {
    #[error("ws-ckpt process I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("ws-ckpt did not finish within 5 seconds")]
    Timeout,
    #[error("ws-ckpt output exceeds 8 MiB")]
    TooLarge,
    #[error("ws-ckpt failed: {0}")]
    Command(String),
    #[error("ws-ckpt returned an invalid response: {0}")]
    Invalid(String),
}

/// Registered workspace identity returned by `status`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct VerifiedWorkspace {
    pub(crate) ws_id: String,
    pub(crate) path: String,
}

/// Snapshot metadata shared by full and summary list entries.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WorkspaceSnapshot {
    pub(crate) id: String,
    pub(crate) workspace: String,
    pub(crate) meta: SnapshotMeta,
}

/// Metadata needed for temporal grouping and the detail dialog.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct SnapshotMeta {
    pub(crate) created_at: String,
    #[serde(default)]
    pub(crate) message: Option<String>,
    #[serde(default)]
    pub(crate) pinned: bool,
    #[serde(default)]
    pub(crate) missing: bool,
    #[serde(default)]
    pub(crate) parent_id: Option<String>,
}

/// One page from the daemon's real snapshot collection.
#[derive(Debug)]
pub(crate) struct SnapshotPage {
    pub(crate) snapshots: Vec<WorkspaceSnapshot>,
    pub(crate) next_cursor: Option<String>,
}

/// Existing ws-ckpt diff representation, preserved without line parsing.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DiffEntry {
    pub(crate) path: String,
    pub(crate) change_type: ChangeType,
    pub(crate) detail: Option<String>,
}

/// File-level change kinds emitted by ws-ckpt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ChangeType {
    Added,
    Modified,
    Deleted,
    Renamed,
}

#[derive(Deserialize)]
struct StatusReport {
    workspaces: Vec<VerifiedWorkspace>,
}

#[derive(Deserialize)]
struct ListPage {
    snapshots: Vec<WorkspaceSnapshot>,
    next_cursor: Option<String>,
}

/// Direct CLI client; the executable path is injectable for contract tests.
pub(crate) struct WsCkptClient {
    executable: PathBuf,
}

impl Default for WsCkptClient {
    fn default() -> Self {
        Self {
            executable: PathBuf::from("ws-ckpt"),
        }
    }
}

impl WsCkptClient {
    #[cfg(test)]
    pub(crate) fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    /// Verify a path or ID against exactly one registered workspace.
    pub(crate) fn status(&self, reference: &str) -> Result<VerifiedWorkspace, WsCkptError> {
        let output = self.run(&["status", "--workspace", reference, "--format", "json"])?;
        let report: StatusReport = parse_json(&output)?;
        if report.workspaces.len() != 1 {
            return Err(WsCkptError::Invalid(format!(
                "status returned {} workspaces for one reference",
                report.workspaces.len()
            )));
        }
        let workspace = report.workspaces.into_iter().next().ok_or_else(|| {
            WsCkptError::Invalid("status omitted the registered workspace".to_string())
        })?;
        if workspace.ws_id.trim().is_empty() || !Path::new(&workspace.path).is_absolute() {
            return Err(WsCkptError::Invalid(
                "status returned an empty ws_id or a non-absolute path".to_string(),
            ));
        }
        if Path::new(reference).is_absolute() && Path::new(reference) != Path::new(&workspace.path)
        {
            return Err(WsCkptError::Invalid(format!(
                "status registered {} instead of requested {reference}",
                workspace.path
            )));
        }
        Ok(workspace)
    }

    /// Read one page using the verified immutable workspace ID.
    pub(crate) fn list(
        &self,
        workspace: &VerifiedWorkspace,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<SnapshotPage, WsCkptError> {
        let limit_text = limit.to_string();
        let mut args = vec![
            "list",
            "--workspace",
            workspace.ws_id.as_str(),
            "--format",
            "json",
            "--limit",
            &limit_text,
        ];
        if let Some(cursor) = cursor {
            args.extend(["--cursor", cursor]);
        }
        let page: ListPage = parse_json(&self.run(&args)?)?;
        Ok(SnapshotPage {
            snapshots: page.snapshots,
            next_cursor: page.next_cursor,
        })
    }

    /// Compare the target snapshot to the live workspace.
    pub(crate) fn diff_to_current(
        &self,
        workspace: &VerifiedWorkspace,
        snapshot_id: &str,
    ) -> Result<Vec<DiffEntry>, WsCkptError> {
        parse_json(&self.run(&[
            "diff",
            "--workspace",
            &workspace.ws_id,
            "--from",
            snapshot_id,
            "--format",
            "json",
        ])?)
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>, WsCkptError> {
        let mut child = Command::new(&self.executable)
            .args(args)
            .current_dir("/")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| WsCkptError::Invalid("failed to capture ws-ckpt stdout".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| WsCkptError::Invalid("failed to capture ws-ckpt stderr".to_string()))?;
        let stdout_reader = thread::spawn(move || drain(stdout));
        let stderr_reader = thread::spawn(move || drain(stderr));
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= COMMAND_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                join_reader(stdout_reader)?;
                join_reader(stderr_reader)?;
                return Err(WsCkptError::Timeout);
            }
            thread::sleep(Duration::from_millis(10));
        };
        let (stdout, stdout_too_large) = join_reader(stdout_reader)?;
        let (stderr, stderr_too_large) = join_reader(stderr_reader)?;
        if stdout_too_large || stderr_too_large {
            return Err(WsCkptError::TooLarge);
        }
        if !status.success() {
            let message = String::from_utf8_lossy(&stderr);
            return Err(WsCkptError::Command(
                message
                    .trim()
                    .chars()
                    .take(MAX_ERROR_CHARS)
                    .collect::<String>(),
            ));
        }
        Ok(stdout)
    }
}

fn parse_json<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, WsCkptError> {
    serde_json::from_slice(bytes).map_err(|error| WsCkptError::Invalid(error.to_string()))
}

fn drain<R: Read>(mut reader: R) -> std::io::Result<(Vec<u8>, bool)> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut too_large = false;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok((output, too_large));
        }
        if output.len().saturating_add(read) <= MAX_OUTPUT_BYTES {
            output.extend_from_slice(&buffer[..read]);
        } else {
            too_large = true;
        }
    }
}

fn join_reader(
    reader: thread::JoinHandle<std::io::Result<(Vec<u8>, bool)>>,
) -> Result<(Vec<u8>, bool), WsCkptError> {
    let output = reader
        .join()
        .map_err(|_| WsCkptError::Invalid("ws-ckpt output reader panicked".to_string()))??;
    Ok(output)
}
