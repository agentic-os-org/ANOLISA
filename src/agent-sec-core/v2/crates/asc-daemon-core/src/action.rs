//! Authenticated scan application operations, composed over the shared lifecycle.
use crate::PeerCredentials;
use asc_action_runtime::{ExecutionControl, Invocation, InvokeError};
use asc_action_types::{
    ActionOutcome, AuditProjection, CallerIdentity, CodeScanRequest, Failure, PiiScanRequest,
};

/// Holds capability registrations assembled by the process composition root.
pub struct ActionService {
    code_scan: Box<dyn Invocation<CodeScanRequest>>,
    pii_scan: Box<dyn Invocation<PiiScanRequest>>,
}

impl ActionService {
    /// Requires explicitly configured scan invocation runtimes.
    #[must_use]
    pub fn new(
        code_scan: impl Invocation<CodeScanRequest> + 'static,
        pii_scan: impl Invocation<PiiScanRequest> + 'static,
    ) -> Self {
        Self {
            code_scan: Box::new(code_scan),
            pii_scan: Box::new(pii_scan),
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
        self.code_scan.invoke(control, &caller(peer), request)
    }

    /// Scans caller-supplied text with kernel identity and normalized business metadata.
    ///
    /// # Errors
    /// Returns a controlled internal failure after runtime finalization.
    pub fn pii_scan(
        &self,
        peer: PeerCredentials,
        control: &ExecutionControl,
        request: &PiiScanRequest,
    ) -> Result<ActionOutcome, InvokeError> {
        self.pii_scan.invoke(control, &caller(peer), request)
    }

    /// Finalizes an authorized PII parameter rejection without retaining invalid input.
    ///
    /// Ingress failures before method authorization do not enter this lifecycle.
    pub fn reject_pii_scan(&self, peer: PeerCredentials) -> ActionOutcome {
        const MESSAGE: &str = "PII scan parameters are invalid";
        self.pii_scan.reject(
            &caller(peer),
            Failure {
                error: Some(MESSAGE.to_owned()),
                error_type: "invalid_parameters".to_owned(),
                exit_code: 1,
            },
            AuditProjection::Failed {
                request: serde_json::Map::new(),
                error: MESSAGE.to_owned(),
                error_type: "invalid_parameters".to_owned(),
            },
        )
    }
}

fn caller(peer: PeerCredentials) -> CallerIdentity {
    CallerIdentity {
        uid: peer.uid(),
        gid: peer.gid(),
        pid: peer.pid(),
    }
}
