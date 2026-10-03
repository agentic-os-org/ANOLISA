use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::types::COMMAND_OUTPUT_REF_MAX_BYTES;

use super::transcript::Transcript;

const REDACTION_LINE_MAX_BYTES: usize = 64 * 1024;

#[cfg(test)]
pub(super) fn write_output_ref(dir: &Path, command_id: &str, output: &[u8]) -> io::Result<PathBuf> {
    Ok(
        write_output_ref_with_session_cap(dir, command_id, output, 0, usize::MAX)?
            .path
            .expect("unbounded session cap should capture output ref"),
    )
}

#[derive(Debug)]
pub(super) struct OutputRefCapture {
    pub(super) path: Option<PathBuf>,
    pub(super) captured_bytes: usize,
    pub(super) status: OutputRefCaptureStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OutputRefCaptureStatus {
    Captured,
    SessionCapReached,
}

pub(super) fn write_output_ref_with_session_cap(
    dir: &Path,
    command_id: &str,
    output: &[u8],
    session_captured_bytes: usize,
    session_cap_bytes: usize,
) -> io::Result<OutputRefCapture> {
    let output = String::from_utf8_lossy(output);
    let (output, _) = crate::evidence::redact_sensitive_text(&output);
    let captured = capped_output_ref_bytes(output.as_bytes(), COMMAND_OUTPUT_REF_MAX_BYTES);
    write_prepared_output_ref_with_session_cap(
        dir,
        command_id,
        &captured,
        session_captured_bytes,
        session_cap_bytes,
    )
}

pub(super) fn write_output_ref_range_with_session_cap(
    dir: &Path,
    command_id: &str,
    transcript: &Transcript,
    start: usize,
    end: usize,
    session_captured_bytes: usize,
    session_cap_bytes: usize,
) -> io::Result<OutputRefCapture> {
    let mut capture = RedactedRangeCapture::default();
    transcript.visit_range_chunks(start, end, |chunk| {
        capture.feed(chunk);
        Ok(())
    })?;
    let captured = capture.finish();
    write_prepared_output_ref_with_session_cap(
        dir,
        command_id,
        &captured,
        session_captured_bytes,
        session_cap_bytes,
    )
}

fn write_prepared_output_ref_with_session_cap(
    dir: &Path,
    command_id: &str,
    captured: &[u8],
    session_captured_bytes: usize,
    session_cap_bytes: usize,
) -> io::Result<OutputRefCapture> {
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    if session_captured_bytes.saturating_add(captured.len()) > session_cap_bytes {
        return Ok(OutputRefCapture {
            path: None,
            captured_bytes: 0,
            status: OutputRefCaptureStatus::SessionCapReached,
        });
    }

    let path = dir.join(format!("{command_id}.txt"));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    options.mode(0o600);
    let mut file = options.open(&path)?;
    file.write_all(captured)?;
    file.sync_all()?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    Ok(OutputRefCapture {
        path: Some(path),
        captured_bytes: captured.len(),
        status: OutputRefCaptureStatus::Captured,
    })
}

#[derive(Debug, Default)]
struct RedactedRangeCapture {
    line: Vec<u8>,
    batch: Vec<u8>,
    line_overflow: bool,
    in_private_key: bool,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    redacted_len: usize,
}

impl RedactedRangeCapture {
    fn feed(&mut self, bytes: &[u8]) {
        for byte in bytes.iter().copied() {
            if !self.line_overflow && self.line.len() < REDACTION_LINE_MAX_BYTES {
                self.line.push(byte);
            } else {
                self.line_overflow = true;
            }
            if byte == b'\n' {
                self.finish_line();
            }
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if !self.line.is_empty() || self.line_overflow {
            self.finish_line();
        }
        self.flush_batch();
        if self.redacted_len <= COMMAND_OUTPUT_REF_MAX_BYTES {
            self.head.truncate(self.redacted_len);
            return self.head;
        }

        let marker = format!(
            "\n[captured output truncated: original_bytes={}, max_capture_bytes={}]\n",
            self.redacted_len, COMMAND_OUTPUT_REF_MAX_BYTES
        )
        .into_bytes();
        if COMMAND_OUTPUT_REF_MAX_BYTES <= marker.len() {
            return marker[..COMMAND_OUTPUT_REF_MAX_BYTES].to_vec();
        }
        let available = COMMAND_OUTPUT_REF_MAX_BYTES - marker.len();
        let head_len = utf8_floor_boundary(&self.head, available / 2);
        let tail = self.tail.into_iter().collect::<Vec<_>>();
        let wanted_tail = available.saturating_sub(head_len);
        let tail_start = utf8_ceil_boundary(&tail, tail.len().saturating_sub(wanted_tail));
        let mut captured = Vec::with_capacity(COMMAND_OUTPUT_REF_MAX_BYTES);
        captured.extend_from_slice(&self.head[..head_len]);
        captured.extend_from_slice(&marker);
        captured.extend_from_slice(&tail[tail_start..]);
        captured
    }

    fn finish_line(&mut self) {
        if self.line_overflow {
            self.flush_batch();
            self.push_redacted(b"<redacted overlong terminal line>\n");
            self.line.clear();
            self.line_overflow = false;
            return;
        }

        let private_key_candidate = self.line.contains(&b'-');
        let upper =
            private_key_candidate.then(|| String::from_utf8_lossy(&self.line).to_ascii_uppercase());
        let begins_private_key = upper
            .as_deref()
            .is_some_and(|line| line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----"));
        let ends_private_key = upper
            .as_deref()
            .is_some_and(|line| line.contains("-----END ") && line.contains("PRIVATE KEY-----"));
        if self.in_private_key {
            if ends_private_key {
                self.in_private_key = false;
            }
        } else if begins_private_key {
            self.flush_batch();
            let line = String::from_utf8_lossy(&self.line);
            let (redacted, _) = crate::evidence::redact_sensitive_text(&line);
            self.push_redacted(redacted.as_bytes());
            if !ends_private_key {
                self.in_private_key = true;
            }
        } else {
            if self.batch.len().saturating_add(self.line.len()) > REDACTION_LINE_MAX_BYTES {
                self.flush_batch();
            }
            self.batch.extend_from_slice(&self.line);
        }
        self.line.clear();
    }

    fn flush_batch(&mut self) {
        if self.batch.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(&self.batch);
        let (redacted, _) = crate::evidence::redact_sensitive_text(&text);
        self.push_redacted(redacted.as_bytes());
        self.batch.clear();
    }

    fn push_redacted(&mut self, bytes: &[u8]) {
        self.redacted_len = self.redacted_len.saturating_add(bytes.len());
        if self.head.len() < COMMAND_OUTPUT_REF_MAX_BYTES {
            let take = (COMMAND_OUTPUT_REF_MAX_BYTES - self.head.len()).min(bytes.len());
            self.head.extend_from_slice(&bytes[..take]);
        }
        self.tail.extend(bytes.iter().copied());
        let excess = self.tail.len().saturating_sub(COMMAND_OUTPUT_REF_MAX_BYTES);
        self.tail.drain(..excess);
    }
}

pub(super) fn capped_output_ref_bytes(output: &[u8], max_bytes: usize) -> Vec<u8> {
    if output.len() <= max_bytes {
        return output.to_vec();
    }

    let marker = format!(
        "\n[captured output truncated: original_bytes={}, max_capture_bytes={}]\n",
        output.len(),
        max_bytes
    )
    .into_bytes();
    if max_bytes <= marker.len() {
        return marker[..max_bytes].to_vec();
    }

    let available = max_bytes - marker.len();
    let head_len = utf8_floor_boundary(output, available / 2);
    let tail_len = available.saturating_sub(head_len);
    let tail_start = utf8_ceil_boundary(output, output.len().saturating_sub(tail_len));

    let mut captured = Vec::with_capacity(max_bytes);
    captured.extend_from_slice(&output[..head_len]);
    captured.extend_from_slice(&marker);
    captured.extend_from_slice(&output[tail_start..]);
    captured
}

fn utf8_floor_boundary(bytes: &[u8], mut index: usize) -> usize {
    index = index.min(bytes.len());
    while index > 0 && index < bytes.len() && is_utf8_continuation(bytes[index]) {
        index -= 1;
    }
    index
}

fn utf8_ceil_boundary(bytes: &[u8], mut index: usize) -> usize {
    index = index.min(bytes.len());
    while index < bytes.len() && is_utf8_continuation(bytes[index]) {
        index += 1;
    }
    index
}

fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

#[cfg(test)]
mod tests {
    use super::super::transcript::TranscriptRetention;
    use super::*;

    #[test]
    fn bounded_range_redacts_secret_whose_prefix_is_before_tail_cut() {
        let work_dir = std::env::temp_dir().join(format!(
            "cosh-output-ref-range-secret-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&work_dir);
        std::fs::create_dir_all(&work_dir).expect("work dir");
        let mut transcript = Transcript::new(
            TranscriptRetention::Bounded { window_bytes: 128 },
            &work_dir.join("terminal-output.transcript"),
        )
        .expect("transcript");
        let mut output = Vec::new();
        append_bounded_lines(&mut output, 1_500_000);
        let secret = "tail-secret-value";
        output.extend_from_slice(b"password=");
        output.resize(output.len() + 32 * 1024, b'a');
        output.extend_from_slice(secret.as_bytes());
        output.push(b'\n');
        append_bounded_lines(&mut output, 500_000);
        transcript.append(&output).expect("append secret output");

        let capture = write_output_ref_range_with_session_cap(
            &work_dir.join("output-refs"),
            "cmd-1",
            &transcript,
            0,
            transcript.position(),
            0,
            usize::MAX,
        )
        .expect("capture");
        let captured =
            std::fs::read_to_string(capture.path.expect("output ref")).expect("captured output");

        assert!(!captured.contains(secret), "{captured}");
        assert!(captured.contains("[captured output truncated:"));
        assert!(!captured.contains("<redacted overlong terminal line>"));
        let _ = std::fs::remove_dir_all(work_dir);
    }

    fn append_bounded_lines(output: &mut Vec<u8>, bytes: usize) {
        let full_lines = bytes / 1024;
        for _ in 0..full_lines {
            output.resize(output.len() + 1023, b'x');
            output.push(b'\n');
        }
        let remainder = bytes % 1024;
        if remainder > 0 {
            output.resize(output.len() + remainder.saturating_sub(1), b'x');
            output.push(b'\n');
        }
    }
}
