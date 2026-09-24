//! Authenticated scan application operations, composed over the shared lifecycle.
use crate::PeerCredentials;
use asc_action_runtime::{
    CapabilityWarmup, ExecutionControl, Invocation, InvokeError, WarmupStatus,
};
use asc_action_types::{
    ActionOutcome, CallerIdentity, CodeScanRequest, PromptScanRequest, PromptScanWarmupRequest,
};

/// Holds capability registrations assembled by the process composition root.
pub struct ActionService {
    code_scan: Box<dyn Invocation<CodeScanRequest>>,
    prompt_scan: Box<dyn Invocation<PromptScanRequest>>,
    prompt_scan_warmup: Box<dyn CapabilityWarmup<Request = PromptScanWarmupRequest>>,
}

impl ActionService {
    /// Requires explicitly configured code- and prompt-scan invocation
    /// runtimes plus the prompt-scan readiness probe.
    #[must_use]
    pub fn new(
        code_scan: impl Invocation<CodeScanRequest> + 'static,
        prompt_scan: impl Invocation<PromptScanRequest> + 'static,
        prompt_scan_warmup: impl CapabilityWarmup<Request = PromptScanWarmupRequest> + 'static,
    ) -> Self {
        Self {
            code_scan: Box::new(code_scan),
            prompt_scan: Box::new(prompt_scan),
            prompt_scan_warmup: Box::new(prompt_scan_warmup),
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

    /// Scans a prompt for any authenticated local peer, without a role requirement.
    ///
    /// # Errors
    /// Returns a controlled internal failure after runtime finalization.
    pub fn prompt_scan(
        &self,
        peer: PeerCredentials,
        control: &ExecutionControl,
        request: &PromptScanRequest,
    ) -> Result<ActionOutcome, InvokeError> {
        self.prompt_scan.invoke(
            control,
            &CallerIdentity {
                uid: peer.uid(),
                gid: peer.gid(),
                pid: peer.pid(),
            },
            request,
        )
    }

    /// Probes the prompt scanner's backing services; a readiness check that
    /// emits no security event, so it needs no peer attribution.
    #[must_use]
    pub fn prompt_scan_warmup(&self, request: &PromptScanWarmupRequest) -> WarmupStatus {
        self.prompt_scan_warmup.warmup(request)
    }
}
