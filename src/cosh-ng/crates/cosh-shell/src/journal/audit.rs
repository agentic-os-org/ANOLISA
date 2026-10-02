//! Shell-owned audit segment writer and command-event projection.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

use crate::config::audit::{load_audit_settings, resolve_audit_root, AuditSettings};
use crate::types::audit::{
    AuditApprovalData, AuditEventOutcome, AuditEventV1, AuditEvidenceData, AuditIdentity,
    AuditMode, AuditOutcomeStatus, AuditRedaction, AuditShellCommandData, AuditSubject,
};
use crate::types::{ShellEvent, ShellEventKind, COMMAND_OUTPUT_REF_MAX_BYTES};

mod writer;

use writer::AuditSegmentWriter;

/// Session recorder that projects only Shell-owned facts.
pub(crate) struct ShellAuditRecorder {
    writer: Option<AuditSegmentWriter>,
    writer_root: Option<PathBuf>,
    mode: AuditMode,
    shell_session_id: String,
    seen_events: usize,
    hash_salt: String,
    degraded: bool,
    warning_emitted: bool,
    // Approvals for which Shell emitted `approval.requested`.
    owned_approvals: std::collections::HashSet<String>,
    // Approvals for which Shell emitted `approval.resolved`, tracked so an
    // external owner's `approval.requested` can still receive exactly one
    // Shell-owned terminal event.
    resolved_approvals: std::collections::HashSet<String>,
    command_refs: std::collections::HashMap<String, String>,
}

/// Approval projection kept independent from binary-only runtime types.
pub(crate) struct ShellApprovalAuditInput<'a> {
    pub(crate) id: &'a str,
    pub(crate) audit_ref: Option<&'a str>,
    pub(crate) session_id: &'a str,
    pub(crate) run_id: &'a str,
    pub(crate) request_id: Option<&'a str>,
    pub(crate) tool_use_id: Option<&'a str>,
    pub(crate) subject: &'a str,
    pub(crate) risk: &'a str,
    pub(crate) assessment: Option<&'a str>,
    pub(crate) preview: &'a str,
    pub(crate) status: &'a str,
}

impl ShellAuditRecorder {
    /// Initializes production audit recording for one Shell session.
    pub(crate) fn initialize(shell_session_id: impl Into<String>) -> Self {
        let shell_session_id = shell_session_id.into();
        let settings = load_audit_settings().unwrap_or_else(|error| {
            tracing::warn!(target: "cosh_audit", "invalid audit configuration: {error}");
            AuditSettings {
                mode: AuditMode::Required,
                ..AuditSettings::default()
            }
        });
        let writer_root = resolve_audit_root().map_err(|error| {
            tracing::warn!(target: "cosh_audit", "audit root unavailable: {error}");
            error
        });
        let writer = writer_root.as_ref().ok().and_then(|root| {
            AuditSegmentWriter::create(root)
                .map_err(|error| {
                    tracing::warn!(target: "cosh_audit", "audit writer unavailable: {error}");
                    error
                })
                .ok()
        });
        let mut recorder = Self {
            degraded: writer.is_none(),
            writer,
            writer_root: writer_root.ok(),
            mode: settings.mode,
            shell_session_id,
            seen_events: 0,
            hash_salt: uuid::Uuid::new_v4().to_string(),
            warning_emitted: false,
            owned_approvals: std::collections::HashSet::new(),
            resolved_approvals: std::collections::HashSet::new(),
            command_refs: std::collections::HashMap::new(),
        };
        recorder.record_session("session.started", AuditOutcomeStatus::Started);
        recorder
    }

    /// Test-only recorder in `Required` mode with no usable writer: every
    /// governed boundary fails closed, letting production-chain tests
    /// assert that blocked approvals never reach an execution path.
    #[cfg(test)]
    pub(crate) fn test_required_unavailable(shell_session_id: impl Into<String>) -> Self {
        Self {
            writer: None,
            writer_root: None,
            mode: AuditMode::Required,
            shell_session_id: shell_session_id.into(),
            seen_events: 0,
            hash_salt: "test-salt".to_string(),
            degraded: true,
            warning_emitted: false,
            owned_approvals: std::collections::HashSet::new(),
            resolved_approvals: std::collections::HashSet::new(),
            command_refs: std::collections::HashMap::new(),
        }
    }

    /// Builds a recorder rooted at a test-private directory.
    #[cfg(test)]
    pub(crate) fn test_with_root(root: &std::path::Path) -> Self {
        Self {
            writer: AuditSegmentWriter::create(root).ok(),
            writer_root: Some(root.to_path_buf()),
            mode: AuditMode::BestEffort,
            shell_session_id: "audit-test-session".to_string(),
            seen_events: 0,
            hash_salt: "salt".to_string(),
            degraded: false,
            warning_emitted: false,
            owned_approvals: std::collections::HashSet::new(),
            resolved_approvals: std::collections::HashSet::new(),
            command_refs: std::collections::HashMap::new(),
        }
    }

    /// Projects newly observed native command events exactly once.
    pub(crate) fn observe_shell_events(&mut self, events: &[ShellEvent]) {
        if self.seen_events > events.len() {
            self.seen_events = 0;
        }
        for event in &events[self.seen_events..] {
            if matches!(
                event.kind,
                ShellEventKind::CommandStarted
                    | ShellEventKind::CommandCompleted
                    | ShellEventKind::CommandFailed
            ) {
                self.record_command(event);
            }
        }
        self.seen_events = events.len();
    }

    /// Projects an already de-duplicated absolute-cursor event batch.
    pub(crate) fn observe_shell_event_batch(&mut self, events: &[ShellEvent]) {
        for event in events {
            if matches!(
                event.kind,
                ShellEventKind::CommandStarted
                    | ShellEventKind::CommandCompleted
                    | ShellEventKind::CommandFailed
            ) {
                self.record_command(event);
            }
        }
        self.seen_events = self.seen_events.saturating_add(events.len());
    }

    /// Returns whether the current session has an unclosed audit gap.
    pub(crate) fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Returns the effective governed-boundary mode.
    pub(crate) fn mode(&self) -> AuditMode {
        self.mode
    }

    /// Returns the last successfully persisted event for a command.
    pub(crate) fn command_audit_ref(&self, command_id: &str) -> Option<&str> {
        self.command_refs.get(command_id).map(String::as_str)
    }

    /// Records metadata-only access to Shell evidence.
    pub(crate) fn record_evidence_accessed(
        &mut self,
        evidence_type: &str,
        size_category: Option<&str>,
        range_category: Option<&str>,
        succeeded: bool,
    ) -> Option<String> {
        let event = AuditEventV1::shell(
            "evidence.accessed",
            AuditIdentity {
                shell_session_id: Some(self.shell_session_id.clone()),
                ..AuditIdentity::default()
            },
            AuditEventOutcome {
                status: if succeeded {
                    AuditOutcomeStatus::Success
                } else {
                    AuditOutcomeStatus::Failed
                },
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "evidence".to_string(),
                name: None,
            },
            &AuditEvidenceData {
                scheme: "terminal-output".to_string(),
                evidence_type: evidence_type.to_string(),
                size_category: size_category.map(str::to_string),
                range_category: range_category.map(str::to_string),
            },
            // Categories are derived, never carrying a raw path or byte range.
            AuditRedaction::clean(),
        );
        match event {
            Ok(event) => {
                let event_id = event.event_id.clone();
                self.append(event, false).then_some(event_id)
            }
            Err(error) => {
                self.mark_degraded(&error);
                None
            }
        }
    }

    /// Records a Shell-owned approval request and returns its real event ID.
    pub(crate) fn record_approval_requested(
        &mut self,
        request: ShellApprovalAuditInput<'_>,
    ) -> Option<String> {
        if request.audit_ref.is_some() {
            // An external owner already wrote `approval.requested`; preserve
            // its audit_ref without claiming ownership.
            return request.audit_ref.map(str::to_string);
        }
        let event = AuditEventV1::shell(
            "approval.requested",
            approval_identity(&request),
            AuditEventOutcome {
                status: AuditOutcomeStatus::Started,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "approval".to_string(),
                name: Some(request.subject.to_string()),
            },
            &AuditApprovalData {
                risk: Some(request.risk.to_string()),
                assessment: request.assessment.map(str::to_string),
                preview_hash: Some(self.hash(request.preview)),
                ..AuditApprovalData::default()
            },
            // The raw preview is replaced by a salted hash.
            AuditRedaction::dropped(&["preview"]),
        );
        match event {
            Ok(event) => {
                let event_id = event.event_id.clone();
                self.owned_approvals.insert(request.id.to_string());
                if self.append(event, false) {
                    Some(event_id)
                } else {
                    None
                }
            }
            Err(error) => {
                self.mark_degraded(&error);
                None
            }
        }
    }

    /// Durably records a Shell-owned approval resolution before execution.
    pub(crate) fn record_approval_resolved(
        &mut self,
        request: ShellApprovalAuditInput<'_>,
    ) -> Result<Option<String>, String> {
        let shell_owned = self.owned_approvals.contains(request.id);
        let externally_recorded = request.audit_ref.is_some();
        if !shell_owned && !externally_recorded {
            return Ok(request.audit_ref.map(str::to_string));
        }
        if self.resolved_approvals.contains(request.id) {
            return Ok(request.audit_ref.map(str::to_string));
        }
        let status = match request.status {
            "approved" => AuditOutcomeStatus::Allowed,
            "pending" => AuditOutcomeStatus::Started,
            "cancelled" => AuditOutcomeStatus::Cancelled,
            _ => AuditOutcomeStatus::Denied,
        };
        let mut event = AuditEventV1::shell(
            "approval.resolved",
            approval_identity(&request),
            AuditEventOutcome {
                status,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "approval".to_string(),
                name: Some(request.subject.to_string()),
            },
            &AuditApprovalData {
                decision: Some(request.status.to_string()),
                ..AuditApprovalData::default()
            },
            AuditRedaction::clean(),
        )?;
        let event_id = event.event_id.clone();
        self.ensure_writer();
        let result = self
            .writer
            .as_mut()
            .ok_or_else(|| "audit writer is unavailable".to_string())
            .and_then(|writer| writer.append(&mut event, true));
        match result {
            Ok(()) => {
                self.resolved_approvals.insert(request.id.to_string());
                self.degraded = false;
                Ok(Some(event_id))
            }
            Err(error) => {
                self.mark_degraded(&error);
                if self.mode == AuditMode::Required {
                    Err(
                        "AUDIT_REQUIRED_UNAVAILABLE: approval was blocked before execution"
                            .to_string(),
                    )
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// #1940: durably records that a registered control approval was
    /// dropped before any user decision, so production audits can
    /// distinguish a user denial from a drain and can locate the exact
    /// drop site. The drained request was never surfaced, so there is no
    /// subject or preview to hash — only the ids and the drop site.
    pub(crate) fn record_approval_dropped(
        &mut self,
        run_id: &str,
        request_id: &str,
        drop_site: &str,
    ) -> Option<String> {
        let event = AuditEventV1::shell(
            "approval.dropped",
            AuditIdentity {
                shell_session_id: Some(self.shell_session_id.clone()),
                run_id: Some(run_id.to_string()),
                request_id: Some(request_id.to_string()),
                ..AuditIdentity::default()
            },
            AuditEventOutcome {
                status: AuditOutcomeStatus::Cancelled,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "approval".to_string(),
                name: None,
            },
            &AuditApprovalData {
                decision: Some("dropped".to_string()),
                reason_code: Some(drop_site.to_string()),
                ..AuditApprovalData::default()
            },
            AuditRedaction::clean(),
        );
        match event {
            Ok(event) => {
                let event_id = event.event_id.clone();
                self.append(event, false).then_some(event_id)
            }
            Err(error) => {
                self.mark_degraded(&error);
                None
            }
        }
    }

    fn record_command(&mut self, event: &ShellEvent) {
        let Some(command_id) = event.command_id.clone() else {
            return;
        };
        let command = event.command.as_deref().unwrap_or_default();
        let first_token = command.split_whitespace().next().unwrap_or("unknown");
        let basename = Path::new(first_token)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown");
        let (program, program_redacted) = crate::evidence::redact_sensitive_text(basename);
        let identity = AuditIdentity {
            shell_session_id: Some(self.shell_session_id.clone()),
            run_id: event
                .audit_identity
                .as_ref()
                .map(|identity| identity.run_id.clone()),
            request_id: event
                .audit_identity
                .as_ref()
                .and_then(|identity| identity.request_id.clone()),
            tool_use_id: event
                .audit_identity
                .as_ref()
                .and_then(|identity| identity.tool_use_id.clone()),
            command_id: Some(command_id.clone()),
            ..AuditIdentity::default()
        };
        let (event_type, status, code) = match event.kind {
            ShellEventKind::CommandStarted => {
                ("shell.command.started", AuditOutcomeStatus::Started, None)
            }
            ShellEventKind::CommandCompleted => (
                "shell.command.completed",
                AuditOutcomeStatus::Success,
                Some("exit_zero".to_string()),
            ),
            ShellEventKind::CommandFailed => (
                "shell.command.failed",
                AuditOutcomeStatus::Failed,
                Some("exit_nonzero".to_string()),
            ),
            _ => return,
        };
        let output_ref = event
            .terminal_output_ref
            .as_ref()
            .map(|_| format!("terminal-output://{}/{}", self.shell_session_id, command_id));
        let payload = AuditShellCommandData {
            program,
            command_hash: self.hash(command),
            cwd_hash: event.cwd.as_deref().map(|cwd| self.hash(cwd)),
            exit_code: event.exit_code,
            duration_ms: event.duration_ms,
            output_bytes: event.terminal_output_bytes,
            truncated: event
                .terminal_output_bytes
                .map(|bytes| bytes > COMMAND_OUTPUT_REF_MAX_BYTES as u64),
            output_ref,
        };
        let outcome = AuditEventOutcome {
            status,
            code,
            retryable: false,
        };
        // Only the source fields this projection actually changed may be claimed:
        // `cwd` is optional, and `program` survives verbatim unless the secret
        // scanner rewrote it.
        let mut redacted_fields = vec!["command"];
        if event.cwd.is_some() {
            redacted_fields.push("cwd");
        }
        if program_redacted {
            redacted_fields.push("program");
        }
        match AuditEventV1::shell(
            event_type,
            identity,
            outcome,
            AuditSubject {
                kind: "shell_command".to_string(),
                name: None,
            },
            &payload,
            AuditRedaction::dropped(&redacted_fields),
        ) {
            Ok(event) => {
                let event_id = event.event_id.clone();
                if self.append(event, false) {
                    self.command_refs.insert(command_id, event_id);
                }
            }
            Err(error) => self.mark_degraded(&error),
        }
    }

    fn record_session(&mut self, event_type: &str, status: AuditOutcomeStatus) {
        let event = AuditEventV1::shell(
            event_type,
            AuditIdentity {
                shell_session_id: Some(self.shell_session_id.clone()),
                ..AuditIdentity::default()
            },
            AuditEventOutcome {
                status,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "session".to_string(),
                name: None,
            },
            &serde_json::json!({}),
            AuditRedaction::clean(),
        );
        match event {
            Ok(event) => {
                self.append(event, true);
            }
            Err(error) => self.mark_degraded(&error),
        }
    }

    fn append(&mut self, mut event: AuditEventV1, durable: bool) -> bool {
        self.ensure_writer();
        let event_type = event.event_type.clone();
        let result = self
            .writer
            .as_mut()
            .ok_or_else(|| "audit writer is unavailable".to_string())
            .and_then(|writer| writer.append(&mut event, durable));
        match result {
            Ok(()) => {
                if self.degraded
                    && event_type != "audit.degraded"
                    && event_type != "audit.recovered"
                {
                    match self.append_recovery_markers() {
                        Ok(()) => {
                            self.degraded = false;
                            self.warning_emitted = false;
                            true
                        }
                        Err(error) => {
                            self.mark_degraded(&error);
                            false
                        }
                    }
                } else {
                    self.degraded = false;
                    true
                }
            }
            Err(error) => {
                self.mark_degraded(&error);
                false
            }
        }
    }

    fn append_recovery_markers(&mut self) -> Result<(), String> {
        let identity = AuditIdentity {
            shell_session_id: Some(self.shell_session_id.clone()),
            ..AuditIdentity::default()
        };
        let marker = |event_type: &str, status: AuditOutcomeStatus, operation: &str| {
            AuditEventV1::shell(
                event_type,
                identity.clone(),
                AuditEventOutcome {
                    status,
                    code: None,
                    retryable: false,
                },
                AuditSubject {
                    kind: "audit".to_string(),
                    name: None,
                },
                &serde_json::json!({ "operation": operation }),
                AuditRedaction::clean(),
            )
        };
        let mut degraded = marker(
            "audit.degraded",
            AuditOutcomeStatus::Failed,
            "write_gap_observed",
        )?;
        let mut recovered = marker(
            "audit.recovered",
            AuditOutcomeStatus::Recovered,
            "durability_recovered",
        )?;
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| "audit writer is unavailable".to_string())?;
        writer.append(&mut degraded, true)?;
        writer.append(&mut recovered, true)
    }

    fn ensure_writer(&mut self) {
        if self.writer.is_some() {
            return;
        }
        let Some(root) = self.writer_root.as_deref() else {
            return;
        };
        if let Ok(writer) = AuditSegmentWriter::create(root) {
            self.writer = Some(writer);
        }
    }

    fn mark_degraded(&mut self, error: &str) {
        self.degraded = true;
        if !self.warning_emitted {
            self.warning_emitted = true;
            let safe = crate::evidence::redact_sensitive_text(error).0;
            eprintln!("cosh-shell audit degraded; command execution continues: {safe}");
        }
    }

    fn hash(&self, value: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(self.hash_salt.as_bytes());
        digest.update(b"\0");
        digest.update(value.as_bytes());
        format!("sha256:{:x}", digest.finalize())
    }
}

impl Drop for ShellAuditRecorder {
    fn drop(&mut self) {
        self.record_session("session.ended", AuditOutcomeStatus::Success);
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.close();
        }
    }
}

fn approval_identity(request: &ShellApprovalAuditInput<'_>) -> AuditIdentity {
    AuditIdentity {
        shell_session_id: Some(request.session_id.to_string()),
        run_id: Some(request.run_id.to_string()),
        request_id: Some(request.request_id.unwrap_or(request.id).to_string()),
        tool_use_id: request.tool_use_id.map(str::to_string),
        ..AuditIdentity::default()
    }
}

#[path = "audit/host_execution.rs"]
mod host_execution;

#[cfg(test)]
#[path = "audit_tests.rs"]
mod tests;
