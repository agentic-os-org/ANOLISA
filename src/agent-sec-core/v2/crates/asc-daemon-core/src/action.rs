//! Authenticated scan application operations, composed over the shared lifecycle.
use crate::PeerCredentials;
use asc_action_runtime::{ExecutionControl, Invocation, InvokeError};
use asc_action_types::{ActionOutcome, CallerIdentity, CodeScanRequest};

/// Holds capability registrations assembled by the process composition root.
pub struct ActionService {
    code_scan: Box<dyn Invocation<CodeScanRequest>>,
    skill_sec: Option<Box<dyn Invocation<asc_action_types::SkillSecRequest>>>,
}

impl ActionService {
    /// Requires an explicitly configured code-scan invocation runtime.
    #[must_use]
    pub fn new(code_scan: impl Invocation<CodeScanRequest> + 'static) -> Self {
        Self {
            code_scan: Box::new(code_scan),
            skill_sec: None,
        }
    }

    /// Adds the process-owned `SkillSec` invocation without a capability dependency.
    #[must_use]
    pub fn with_skill_sec(
        mut self,
        invocation: impl Invocation<asc_action_types::SkillSecRequest> + 'static,
    ) -> Self {
        self.skill_sec = Some(Box::new(invocation));
        self
    }

    /// Executes a Skill operation using the authenticated peer for authorization and audit.
    ///
    /// # Errors
    /// Returns a controlled error when unconfigured or after finalizing an execution panic.
    pub fn skill_sec(
        &self,
        peer: PeerCredentials,
        control: &ExecutionControl,
        command: asc_action_types::SkillSecCommand,
    ) -> Result<ActionOutcome, InvokeError> {
        self.skill_sec.as_ref().ok_or(InvokeError)?.invoke(
            control,
            &CallerIdentity {
                uid: peer.uid(),
                gid: peer.gid(),
                pid: peer.pid(),
            },
            &asc_action_types::SkillSecRequest {
                command,
                caller_uid: peer.uid(),
            },
        )
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
