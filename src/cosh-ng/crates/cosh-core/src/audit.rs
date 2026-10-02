//! Core-owned unified audit recorder and governed durability barriers.

use std::path::Path;

use chrono::Utc;
use cosh_platform::audit::config::{load_audit_settings, resolve_audit_root};
use cosh_platform::audit::state::{update_state, AuditOperationalState, AuditStateError};
use cosh_platform::audit::store::{AuditDurability, AuditSegmentWriter};
use cosh_types::audit::{
    AuditActor, AuditActorKind, AuditComponent, AuditComponentName, AuditControlData,
    AuditEventOutcome, AuditEventType, AuditEventV1, AuditIdentity, AuditMode, AuditOutcomeStatus,
    AuditRedaction, AuditRedactionStatus, AuditSettings, AuditSubject, KnownAuditEventType,
};
use serde::Serialize;
use uuid::Uuid;

mod orchestration;

pub(crate) use orchestration::CoreAuditScope;

enum CoreAuditSink {
    Real(AuditSegmentWriter),
    Unavailable,
    #[cfg(test)]
    Capture(Vec<AuditEventV1>),
    #[cfg(test)]
    Noop,
    #[cfg(test)]
    Failing,
    /// Captures every event except one type, whose writes fail.
    ///
    /// A sink that fails everything aborts a run at its first barrier, which
    /// cannot reach a later boundary under test.
    #[cfg(test)]
    CaptureExcept {
        events: Vec<AuditEventV1>,
        failing: KnownAuditEventType,
    },
}

/// Independent audit side path for Core semantic lifecycle boundaries.
pub(crate) struct CoreAuditRecorder {
    sink: CoreAuditSink,
    settings: AuditSettings,
    state: AuditOperationalState,
    root: Option<std::path::PathBuf>,
    session_id: String,
    degraded: bool,
    warned: bool,
    pending_terminal_gap: bool,
}

impl CoreAuditRecorder {
    /// Initializes the production recorder and emits `session.started`.
    pub(crate) fn initialize(session_id: &str, workspace: Option<&Path>) -> Self {
        #[cfg(test)]
        if std::env::var_os("COSH_AUDIT_DIR").is_none() {
            return Self::test_noop(session_id);
        }

        let (settings, config_failed) = match load_audit_settings(workspace) {
            Ok(loaded) => {
                for warning in loaded.warnings {
                    tracing::warn!(target: "cosh_audit", "{warning}");
                }
                (loaded.settings, false)
            }
            Err(error) => {
                tracing::error!(target: "cosh_audit", "audit configuration failed: {error}");
                // Unknown system authority must not silently become best-effort.
                let settings = AuditSettings {
                    mode: AuditMode::Required,
                    ..AuditSettings::default()
                };
                (settings, true)
            }
        };
        let state = AuditOperationalState::new(settings.clone());
        let (root, sink) = if config_failed {
            (None, CoreAuditSink::Unavailable)
        } else {
            match resolve_audit_root() {
                Ok(root) => {
                    match AuditSegmentWriter::create(&root.path, AuditComponentName::CoshCore) {
                        Ok(writer) => (Some(root.path), CoreAuditSink::Real(writer)),
                        Err(error) => {
                            tracing::warn!(target: "cosh_audit", "audit writer unavailable: {error}");
                            (Some(root.path), CoreAuditSink::Unavailable)
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(target: "cosh_audit", "audit root unavailable: {error}");
                    (None, CoreAuditSink::Unavailable)
                }
            }
        };
        let mut recorder = Self {
            sink,
            settings,
            state,
            root,
            session_id: session_id.to_string(),
            degraded: false,
            warned: false,
            pending_terminal_gap: false,
        };
        #[cfg(not(test))]
        if let Some(root) = recorder.root.clone() {
            cosh_platform::audit::retention::schedule_retention(
                root,
                recorder.settings.clone(),
                AuditComponentName::CoshCore,
            );
        }
        let identity = recorder.identity(None, None, None, None);
        let _ = recorder.emit(
            KnownAuditEventType::SessionStarted,
            identity,
            AuditOutcomeStatus::Started,
            "session",
            None,
            &cosh_types::audit::AuditLifecycleData::default(),
            AuditDurability::Ordinary,
            false,
        );
        recorder
    }

    /// Returns the effective failure mode.
    pub(crate) fn mode(&self) -> AuditMode {
        self.settings.mode
    }

    /// Creates correlation identities rooted in this Provider session.
    pub(crate) fn identity(
        &self,
        run_id: Option<&str>,
        turn_id: Option<&str>,
        request_id: Option<&str>,
        tool_use_id: Option<&str>,
    ) -> AuditIdentity {
        AuditIdentity {
            provider_session_id: Some(self.session_id.clone()),
            run_id: run_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            request_id: request_id.map(str::to_string),
            tool_use_id: tool_use_id.map(str::to_string),
            ..AuditIdentity::default()
        }
    }

    /// Emits an ordinary lifecycle fact; failures become a visible gap.
    pub(crate) fn ordinary<T: Serialize>(
        &mut self,
        event_type: KnownAuditEventType,
        identity: AuditIdentity,
        status: AuditOutcomeStatus,
        subject_kind: &str,
        subject_name: Option<&str>,
        payload: &T,
    ) {
        let _ = self.emit(
            event_type,
            identity,
            status,
            subject_kind,
            subject_name,
            payload,
            AuditDurability::Ordinary,
            true,
        );
    }

    /// Emits an ordinary fact and returns its real event ID when accepted.
    pub(crate) fn ordinary_ref<T: Serialize>(
        &mut self,
        event_type: KnownAuditEventType,
        identity: AuditIdentity,
        status: AuditOutcomeStatus,
        subject_kind: &str,
        subject_name: Option<&str>,
        payload: &T,
    ) -> Option<String> {
        let event_id = Uuid::new_v4().to_string();
        self.emit_with_id(
            event_id.clone(),
            event_type,
            identity,
            status,
            subject_kind,
            subject_name,
            payload,
            AuditDurability::Ordinary,
            true,
        )
        .ok()
        .filter(|persisted| *persisted)
        .map(|_| event_id)
    }

    /// Durably records a governed boundary before the action may start.
    ///
    /// # Errors
    ///
    /// Returns a stable recoverable error in required mode when durability is
    /// unavailable. Best-effort mode records degradation and continues.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn barrier<T: Serialize>(
        &mut self,
        event_type: KnownAuditEventType,
        identity: AuditIdentity,
        status: AuditOutcomeStatus,
        subject_kind: &str,
        subject_name: Option<&str>,
        payload: &T,
    ) -> Result<(), String> {
        if self.pending_terminal_gap && self.settings.mode == AuditMode::Required {
            self.try_recover()?;
        }
        self.emit(
            event_type,
            identity,
            status,
            subject_kind,
            subject_name,
            payload,
            AuditDurability::SecurityBoundary,
            false,
        )
    }

    fn try_recover(&mut self) -> Result<(), String> {
        let identity = self.identity(None, None, None, None);
        let payload = AuditControlData {
            operation: Some("durability_probe".to_string()),
            ..AuditControlData::default()
        };
        self.emit(
            KnownAuditEventType::AuditRecovered,
            identity,
            AuditOutcomeStatus::Recovered,
            "audit",
            None,
            &payload,
            AuditDurability::SecurityBoundary,
            false,
        )?;
        self.pending_terminal_gap = false;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit<T: Serialize>(
        &mut self,
        event_type: KnownAuditEventType,
        identity: AuditIdentity,
        status: AuditOutcomeStatus,
        subject_kind: &str,
        subject_name: Option<&str>,
        payload: &T,
        durability: AuditDurability,
        after_side_effect: bool,
    ) -> Result<(), String> {
        self.emit_with_id(
            Uuid::new_v4().to_string(),
            event_type,
            identity,
            status,
            subject_kind,
            subject_name,
            payload,
            durability,
            after_side_effect,
        )
        .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_with_id<T: Serialize>(
        &mut self,
        event_id: String,
        event_type: KnownAuditEventType,
        identity: AuditIdentity,
        status: AuditOutcomeStatus,
        subject_kind: &str,
        subject_name: Option<&str>,
        payload: &T,
        durability: AuditDurability,
        after_side_effect: bool,
    ) -> Result<bool, String> {
        self.ensure_writer();
        let now = Utc::now();
        let event_result = AuditEventV1::new(
            event_id,
            AuditEventType::from(event_type),
            now,
            now,
            0,
            AuditComponent {
                name: AuditComponentName::CoshCore,
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            identity,
            AuditActor {
                kind: AuditActorKind::Agent,
                uid: Some(nix::unistd::Uid::current().as_raw()),
                euid: Some(nix::unistd::Uid::effective().as_raw()),
            },
            AuditEventOutcome {
                status,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: subject_kind.to_string(),
                name: subject_name.map(str::to_string),
            },
            payload,
            AuditRedaction {
                policy_version: "audit-redaction-v1".to_string(),
                status: AuditRedactionStatus::Clean,
                fields: Vec::new(),
            },
        );
        let mut event = match event_result {
            Ok(event) => event,
            Err(error) => {
                self.degraded = true;
                self.state.last_write_error = Some(AuditStateError {
                    operation: "construct".to_string(),
                    code: "invalid_record".to_string(),
                    occurred_at: Utc::now(),
                });
                self.persist_state();
                if !self.warned {
                    tracing::warn!(target: "cosh_audit", "audit event rejected: {error}");
                    self.warned = true;
                }
                if after_side_effect {
                    self.pending_terminal_gap = true;
                }
                return if self.settings.mode == AuditMode::Required
                    && durability == AuditDurability::SecurityBoundary
                {
                    Err("AUDIT_REQUIRED_UNAVAILABLE: governed action was not started; retry after audit storage recovers".to_string())
                } else {
                    Ok(false)
                };
            }
        };

        let (write_result, persisted) = match &mut self.sink {
            CoreAuditSink::Real(writer) => (writer.append(&mut event, durability), true),
            CoreAuditSink::Unavailable => (
                Err(cosh_types::error::CoshError::new(
                    cosh_types::error::ErrorCode::AuditUnavailable,
                    "audit writer is unavailable",
                    "audit",
                )),
                false,
            ),
            #[cfg(test)]
            CoreAuditSink::Capture(events) => {
                events.push(event);
                (Ok(()), true)
            }
            #[cfg(test)]
            CoreAuditSink::Noop => (Ok(()), false),
            #[cfg(test)]
            CoreAuditSink::Failing => (
                Err(cosh_types::error::CoshError::new(
                    cosh_types::error::ErrorCode::AuditUnavailable,
                    "injected audit failure",
                    "audit",
                )),
                false,
            ),
            #[cfg(test)]
            CoreAuditSink::CaptureExcept { events, failing } => {
                if event_type == *failing {
                    (
                        Err(cosh_types::error::CoshError::new(
                            cosh_types::error::ErrorCode::AuditUnavailable,
                            "injected audit failure",
                            "audit",
                        )),
                        false,
                    )
                } else {
                    events.push(event);
                    (Ok(()), true)
                }
            }
        };

        match write_result {
            Ok(()) => {
                self.state.last_successful_write = Some(Utc::now());
                self.state.last_write_error = None;
                let was_degraded = self.degraded;
                if was_degraded && event_type == KnownAuditEventType::AuditRecovered {
                    self.degraded = false;
                    self.warned = false;
                    self.pending_terminal_gap = false;
                } else if was_degraded && event_type != KnownAuditEventType::AuditDegraded {
                    self.degraded = false;
                    self.warned = false;
                    let identity = self.identity(None, None, None, None);
                    let payload = AuditControlData {
                        operation: Some("write_gap_observed".to_string()),
                        ..AuditControlData::default()
                    };
                    self.emit(
                        KnownAuditEventType::AuditDegraded,
                        identity.clone(),
                        AuditOutcomeStatus::Failed,
                        "audit",
                        None,
                        &payload,
                        AuditDurability::SecurityBoundary,
                        false,
                    )?;
                    if !self.degraded {
                        self.emit(
                            KnownAuditEventType::AuditRecovered,
                            identity,
                            AuditOutcomeStatus::Recovered,
                            "audit",
                            None,
                            &AuditControlData {
                                operation: Some("durability_recovered".to_string()),
                                ..AuditControlData::default()
                            },
                            AuditDurability::SecurityBoundary,
                            false,
                        )?;
                        if !self.degraded {
                            self.pending_terminal_gap = false;
                        }
                    }
                }
                self.persist_state();
                Ok(persisted)
            }
            Err(error) => {
                self.degraded = true;
                self.state.last_write_error = Some(AuditStateError {
                    operation: "write".to_string(),
                    code: format!("{:?}", error.code).to_ascii_lowercase(),
                    occurred_at: Utc::now(),
                });
                self.persist_state();
                if !self.warned {
                    tracing::warn!(target: "cosh_audit", "audit degraded: {error}");
                    self.warned = true;
                }
                if after_side_effect {
                    self.pending_terminal_gap = true;
                }
                if self.settings.mode == AuditMode::Required
                    && durability == AuditDurability::SecurityBoundary
                {
                    Err("AUDIT_REQUIRED_UNAVAILABLE: governed action was not started; retry after audit storage recovers".to_string())
                } else {
                    Ok(false)
                }
            }
        }
    }

    fn ensure_writer(&mut self) {
        if !matches!(self.sink, CoreAuditSink::Unavailable) {
            return;
        }
        let root = match self
            .root
            .clone()
            .or_else(|| resolve_audit_root().ok().map(|root| root.path))
        {
            Some(root) => root,
            None => return,
        };
        if let Ok(writer) = AuditSegmentWriter::create(&root, AuditComponentName::CoshCore) {
            self.root = Some(root);
            self.sink = CoreAuditSink::Real(writer);
        }
    }

    fn persist_state(&self) {
        if let Some(root) = &self.root {
            let observed = self.state.clone();
            if let Err(error) = update_state(root, self.settings.clone(), |state| {
                state.settings = observed.settings.clone();
                state.last_successful_write = observed.last_successful_write;
                state.last_write_error = observed.last_write_error.clone();
            }) {
                tracing::warn!(target: "cosh_audit", "audit state update failed: {error}");
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn test_noop(session_id: &str) -> Self {
        let settings = AuditSettings::default();
        Self {
            sink: CoreAuditSink::Noop,
            state: AuditOperationalState::new(settings.clone()),
            settings,
            root: None,
            session_id: session_id.to_string(),
            degraded: false,
            warned: false,
            pending_terminal_gap: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_real(session_id: &str, root: &Path) -> Self {
        let settings = AuditSettings::default();
        Self {
            sink: CoreAuditSink::Real(
                AuditSegmentWriter::create(root, AuditComponentName::CoshCore).unwrap(),
            ),
            state: AuditOperationalState::new(settings.clone()),
            settings,
            root: Some(root.to_path_buf()),
            session_id: session_id.to_string(),
            degraded: false,
            warned: false,
            pending_terminal_gap: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn test_capture(session_id: &str) -> Self {
        let settings = AuditSettings::default();
        Self {
            sink: CoreAuditSink::Capture(Vec::new()),
            state: AuditOperationalState::new(settings.clone()),
            settings,
            root: None,
            session_id: session_id.to_string(),
            degraded: false,
            warned: false,
            pending_terminal_gap: false,
        }
    }

    /// Captured event types for one tool call, in emission order.
    ///
    /// Lets a test assert the per-call lifecycle rather than only that some event
    /// of a type exists somewhere in the run.
    #[cfg(test)]
    pub(crate) fn captured_tool_event_types(&self, tool_use_id: &str) -> Vec<&str> {
        match &self.sink {
            CoreAuditSink::Capture(events) | CoreAuditSink::CaptureExcept { events, .. } => events
                .iter()
                .filter(|event| event.identity.tool_use_id.as_deref() == Some(tool_use_id))
                .map(|event| event.event_type.as_str())
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A required-mode recorder that fails only `failing` writes.
    ///
    /// Lets a test drive the second failure that matters at a security
    /// boundary — the control transport is gone *and* that boundary's terminal
    /// record cannot be persisted — without aborting the run at an earlier one.
    #[cfg(test)]
    pub(crate) fn test_capture_except(session_id: &str, failing: KnownAuditEventType) -> Self {
        let settings = AuditSettings {
            mode: AuditMode::Required,
            ..AuditSettings::default()
        };
        Self {
            sink: CoreAuditSink::CaptureExcept {
                events: Vec::new(),
                failing,
            },
            state: AuditOperationalState::new(settings.clone()),
            settings,
            root: None,
            session_id: session_id.to_string(),
            degraded: false,
            warned: false,
            pending_terminal_gap: false,
        }
    }

    /// Captured events with their payloads.
    ///
    /// Lets a test assert what an event says (decision, reason code) and not
    /// only that its type was emitted.
    #[cfg(test)]
    pub(crate) fn captured_events(&self) -> &[AuditEventV1] {
        match &self.sink {
            CoreAuditSink::Capture(events) => events,
            CoreAuditSink::CaptureExcept { events, .. } => events,
            _ => &[],
        }
    }

    #[cfg(test)]
    pub(crate) fn captured_event_types(&self) -> Vec<&str> {
        match &self.sink {
            CoreAuditSink::Capture(events) | CoreAuditSink::CaptureExcept { events, .. } => events
                .iter()
                .map(|event| event.event_type.as_str())
                .collect(),
            _ => Vec::new(),
        }
    }
}

impl Drop for CoreAuditRecorder {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let identity = self.identity(None, None, None, None);
            let _ = self.emit(
                KnownAuditEventType::SessionEnded,
                identity,
                AuditOutcomeStatus::Success,
                "session",
                None,
                &cosh_types::audit::AuditLifecycleData::default(),
                AuditDurability::Ordinary,
                false,
            );
        }
        if let CoreAuditSink::Real(writer) = &mut self.sink {
            let _ = writer.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_sink_is_test_only_and_does_not_block() {
        let mut recorder = CoreAuditRecorder::test_noop("session");
        recorder
            .barrier(
                KnownAuditEventType::ProviderRequestStarted,
                recorder.identity(Some("run"), Some("turn"), Some("request"), None),
                AuditOutcomeStatus::Started,
                "provider",
                Some("mock"),
                &cosh_types::audit::AuditProviderData {
                    provider: "mock".to_string(),
                    ..cosh_types::audit::AuditProviderData::default()
                },
            )
            .unwrap();
    }

    #[test]
    fn successful_write_closes_a_degraded_episode_with_durable_markers() {
        let mut recorder = CoreAuditRecorder::test_capture("session");
        recorder.degraded = true;
        recorder.warned = true;
        recorder.pending_terminal_gap = true;
        recorder
            .barrier(
                KnownAuditEventType::ProviderRequestStarted,
                recorder.identity(Some("run"), Some("turn"), Some("request"), None),
                AuditOutcomeStatus::Started,
                "provider",
                Some("mock"),
                &cosh_types::audit::AuditProviderData {
                    provider: "mock".to_string(),
                    ..cosh_types::audit::AuditProviderData::default()
                },
            )
            .unwrap();
        let CoreAuditSink::Capture(events) = &recorder.sink else {
            panic!("capture sink changed during recovery")
        };
        let event_types = events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            [
                "provider.request.started",
                "audit.degraded",
                "audit.recovered"
            ]
        );
        assert!(!recorder.degraded);
        assert!(!recorder.pending_terminal_gap);
    }

    #[test]
    fn failed_best_effort_write_never_returns_a_fabricated_reference() {
        let mut recorder = CoreAuditRecorder::test_noop("session");
        recorder.sink = CoreAuditSink::Failing;

        let audit_ref = recorder.ordinary_ref(
            KnownAuditEventType::ApprovalRequested,
            recorder.identity(Some("run"), None, Some("request"), None),
            AuditOutcomeStatus::Started,
            "approval",
            Some("tool"),
            &cosh_types::audit::AuditApprovalData::default(),
        );

        assert!(audit_ref.is_none());
        assert!(recorder.degraded);
    }
}
