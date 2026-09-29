//! Read and bound PII input locally, then send text through the daemon protocol.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;

use asc_daemon_protocol::{DaemonRequest, PiiScanParams, method};
use clap::{ArgGroup, Args, ValueEnum};

use crate::InputError;

/// Client presentation for a PII scan result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PiiOutputFormat {
    /// V1-compatible structured result without the RPC envelope.
    Json,
    /// Human-readable findings with redacted evidence.
    Text,
}

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("input_source").required(true).args(["text", "stdin", "input"])))]
#[allow(clippy::struct_excessive_bools)] // Independent existing CLI switches.
pub(crate) struct ScanPiiCommand {
    /// Text to scan, including an empty string.
    #[arg(long, allow_hyphen_values = true)]
    text: Option<String>,
    /// Read UTF-8 input from stdin.
    #[arg(long, alias = "text-stdin")]
    stdin: bool,
    /// Read a UTF-8 input file locally; its path is never sent to the daemon.
    #[arg(long)]
    input: Option<PathBuf>,
    /// Output presentation.
    #[arg(long, value_enum, default_value = "json")]
    pub(crate) format: PiiOutputFormat,
    /// Caller-declared input origin; does not grant authorization.
    #[arg(long, default_value = "unknown", value_parser = ["user_input", "tool_input", "tool_output", "model_output", "observability", "manual", "unknown"])]
    source: String,
    /// Optional positive byte prefix; omitted means no implicit truncation.
    #[arg(long, value_parser = positive_limit)]
    max_bytes: Option<usize>,
    /// Retain low-confidence findings.
    #[arg(long)]
    include_low_confidence: bool,
    /// Include raw evidence in JSON output only, never in audit records.
    #[arg(long)]
    raw_evidence: bool,
    /// Also return the redacted text without rewriting the original input.
    #[arg(long)]
    redact_output: bool,
}

impl ScanPiiCommand {
    pub(crate) fn request(&self) -> Result<DaemonRequest, InputError> {
        let (text, truncated, count) = if let Some(text) = &self.text {
            read_input(text.as_bytes(), self.max_bytes)?
        } else if let Some(path) = &self.input {
            read_input(
                File::open(path).map_err(InputError::PiiRead)?,
                self.max_bytes,
            )?
        } else {
            read_input(io::stdin().lock(), self.max_bytes)?
        };
        Ok(DaemonRequest {
            trace_context: None,
            compatibility: None,
            method: method::ACTION_PII_SCAN.to_owned(),
            params: serde_json::to_value(PiiScanParams {
                text,
                source: self.source.clone(),
                include_low_confidence: self.include_low_confidence,
                raw_evidence: self.raw_evidence,
                redact_output: self.redact_output,
                max_bytes: self.max_bytes,
                input_truncated: truncated,
                input_bytes_scanned: truncated.then_some(count),
            })?,
        })
    }
}

fn positive_limit(value: &str) -> Result<usize, String> {
    value
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(|| "--max-bytes must be a positive integer".to_owned())
}

fn read_input(
    reader: impl Read,
    maximum: Option<usize>,
) -> Result<(String, bool, usize), InputError> {
    let transport_limit = asc_daemon_client::MAX_FRAME_BYTES;
    let limit = maximum.unwrap_or(transport_limit).min(transport_limit);
    let mut bytes = Vec::new();
    reader
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(InputError::PiiRead)?;
    if bytes.len() > transport_limit && maximum.is_none_or(|n| n > transport_limit) {
        return Err(InputError::PiiTooLarge);
    }
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    let count = bytes.len();
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) if truncated && error.utf8_error().error_len().is_none() => {
            let valid = error.utf8_error().valid_up_to();
            String::from_utf8(error.into_bytes()[..valid].to_vec())
                .map_err(|_| InputError::PiiUtf8)?
        }
        Err(_) => return Err(InputError::PiiUtf8),
    };
    Ok((text, truncated, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_truncation_does_not_hide_malformed_input_or_add_a_default_limit() {
        assert_eq!(
            read_input("备注🙂tail".as_bytes(), Some(7)).unwrap(),
            ("备注".to_owned(), true, 7)
        );
        assert!(matches!(
            read_input(&[0xff][..], None),
            Err(InputError::PiiUtf8)
        ));
        assert!(matches!(
            read_input(&[0xe4][..], Some(1)),
            Err(InputError::PiiUtf8)
        ));
        assert!(matches!(
            read_input(&[0xff, b'a'][..], Some(1)),
            Err(InputError::PiiUtf8)
        ));
        let large = vec![b'a'; 1_048_577];
        assert_eq!(
            read_input(large.as_slice(), None).unwrap().0.len(),
            large.len()
        );
        let oversized = vec![b'a'; asc_daemon_client::MAX_FRAME_BYTES + 1];
        assert!(matches!(
            read_input(oversized.as_slice(), None),
            Err(InputError::PiiTooLarge)
        ));
        assert_eq!(
            read_input(&b""[..], None).unwrap(),
            (String::new(), false, 0)
        );
    }
}
