//! Typed daemon calls; scanning and authorization remain owned by sec-core.

use std::path::PathBuf;
use std::time::Duration;

pub use asc_daemon_client::ClientError;
pub use asc_daemon_client::code_scan::{
    DaemonResponse as ScanResponse, ErrorResponse, Finding, RequestId, ScanError, ScanRequest,
    ScanResult, SuccessResponse, TraceCarrierV1, Verdict,
};

use crate::{CallContext, Error};

/// Component client retaining UID verification and single-call deadlines.
pub struct Client {
    inner: asc_daemon_client::code_scan::Client,
}

impl Client {
    pub(crate) fn new(socket: PathBuf) -> Result<Self, Error> {
        if !socket.is_absolute() {
            return Err(Error::Configuration("sec-core socket must be absolute"));
        }
        Ok(Self {
            inner: asc_daemon_client::code_scan::Client::new(socket),
        })
    }

    /// Scans once and preserves verdicts separately from daemon rejections.
    ///
    /// Blocks the current thread. The timeout covers transport I/O, not server
    /// cancellation. A returned verdict does not authorize native execution.
    ///
    /// # Errors
    /// Returns security transport, result-shape or contradictory-verdict failures.
    pub fn scan_code(
        &self,
        request: &ScanRequest,
        context: &CallContext,
        timeout: Duration,
    ) -> Result<ScanResponse<ScanResult>, Error> {
        let context = asc_daemon_client::code_scan::Context {
            trace_context: context.trace_context.clone(),
            compatibility: Some(asc_daemon_client::code_scan::CompatibilityV1 {
                version: 1,
                trace_id: context.session_id.clone(),
                invocation_label: context.tool_use_id.clone(),
            }),
        };
        Ok(self.inner.scan_code(request, &context, timeout)?)
    }
}
