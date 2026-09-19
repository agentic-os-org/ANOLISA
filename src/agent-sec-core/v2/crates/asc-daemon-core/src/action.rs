//! Authenticated scan application operations, composed over the shared lifecycle.
use crate::PeerCredentials;
use asc_action_runtime::{ExecutionControl, Invocation, InvokeError};
use asc_action_types::{ActionOutcome, CallerIdentity, CodeScanRequest};

/// Holds capability registrations assembled by the process composition root.
pub struct ActionService {
    code_scan: Box<dyn Invocation<CodeScanRequest>>,
}

impl ActionService {
    /// Requires an explicitly configured code-scan invocation runtime.
    #[must_use]
    pub fn new(code_scan: impl Invocation<CodeScanRequest> + 'static) -> Self {
        Self {
            code_scan: Box::new(code_scan),
        }
    }

    /// Scans code for any authenticated local peer, without a role requirement.
    ///
    /// # Errors
    /// Returns a controlled internal failure after runtime finalization.
    pub fn code_scan(
        &self,
        peer: PeerCredentials,
        control: &ExecutionControl,
        request: &CodeScanRequest,
    ) -> Result<ActionOutcome, InvokeError> {
        self.code_scan.invoke(
            control,
            &CallerIdentity {
                uid: peer.uid(),
                gid: peer.gid(),
                pid: peer.pid(),
            },
            request,
        )
    }
}
