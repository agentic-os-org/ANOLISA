//! Compatibility command for sending code scans to the daemon.

use asc_daemon_protocol::{CodeScanParams, DaemonRequest, method};
use clap::Args;

use crate::InputError;

/// Scans one Bash or Python snippet through `asc-daemon`.
#[derive(Debug, Args)]
pub(crate) struct ScanCodeCommand {
    /// Source code to scan.
    #[arg(long, default_value = "", allow_hyphen_values = true)]
    code: String,
    /// Language: bash or python.
    #[arg(long, default_value = "bash")]
    language: String,
    /// Engine mode: regex or llm.
    #[arg(long, default_value = "regex")]
    mode: String,
}

impl ScanCodeCommand {
    /// Builds the request sent to the code-scan Action handler.
    pub(crate) fn request(&self) -> Result<DaemonRequest, InputError> {
        if self.code.trim().is_empty() {
            return Err(InputError::EmptyCode);
        }
        Ok(DaemonRequest {
            method: method::ACTION_CODE_SCAN.to_owned(),
            params: serde_json::to_value(CodeScanParams {
                code: self.code.clone(),
                language: self.language.clone(),
                rules: None,
                mode: Some(self.mode.clone()),
            })?,
        })
    }
}
