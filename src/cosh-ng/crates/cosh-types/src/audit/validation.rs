//! Validation for canonical audit events.

use super::event::*;
use serde_json::Value;

impl AuditEventV1 {
    /// Validates schema, identity minima, bounded strings, and payload size.
    ///
    /// # Errors
    ///
    /// Returns the first stable field-level validation failure.
    pub fn validate(&self) -> Result<(), AuditValidationError> {
        if self.schema != AUDIT_EVENT_SCHEMA {
            return Err(AuditValidationError::new("schema", "unsupported value"));
        }
        if self.schema_version != AUDIT_EVENT_SCHEMA_VERSION {
            return Err(AuditValidationError::new(
                "schema_version",
                "unsupported value",
            ));
        }
        validate_token("event_id", &self.event_id, 128)?;
        validate_token("component.version", &self.component.version, 64)?;
        validate_token("subject.kind", &self.subject.kind, 64)?;
        if let Some(name) = &self.subject.name {
            validate_text("subject.name", name, 128)?;
        }
        if let Some(code) = &self.outcome.code {
            validate_token("outcome.code", code, 96)?;
        }
        for value in identity_values(&self.identity) {
            validate_token("identity", value, 256)?;
        }
        for field in &self.redaction.fields {
            validate_token("redaction.fields", field, 128)?;
        }
        if !self.data.is_object() {
            return Err(AuditValidationError::new("data", "must be an object"));
        }
        let data_len = serde_json::to_vec(&self.data)
            .map_err(|_| AuditValidationError::new("data", "serialization failed"))?
            .len();
        if data_len > MAX_UNKNOWN_DATA_BYTES {
            return Err(AuditValidationError::new(
                "data",
                "payload exceeds 4096 bytes",
            ));
        }
        validate_payload_shape(&self.event_type, &self.data)?;
        validate_identity_minimum(&self.event_type, &self.identity)
    }
}

fn validate_payload_shape(
    event_type: &AuditEventType,
    data: &Value,
) -> Result<(), AuditValidationError> {
    let AuditEventType::Known(known) = event_type else {
        return Ok(());
    };
    match known {
        KnownAuditEventType::SessionStarted
        | KnownAuditEventType::SessionEnded
        | KnownAuditEventType::TurnStarted
        | KnownAuditEventType::TurnCompleted
        | KnownAuditEventType::TurnFailed => validate_payload::<AuditLifecycleData>(data),
        KnownAuditEventType::ProviderRequestStarted
        | KnownAuditEventType::ProviderRequestCompleted
        | KnownAuditEventType::ProviderRequestFailed
        | KnownAuditEventType::ProviderRequestCancelled => {
            validate_payload::<AuditProviderData>(data)
        }
        KnownAuditEventType::PolicyDecision | KnownAuditEventType::HookDecision => {
            validate_payload::<AuditDecisionData>(data)
        }
        KnownAuditEventType::ToolRequested
        | KnownAuditEventType::ToolExecutionStarted
        | KnownAuditEventType::ToolCompleted
        | KnownAuditEventType::ToolFailed
        | KnownAuditEventType::ToolCancelled => validate_payload::<AuditToolData>(data),
        KnownAuditEventType::ApprovalRequested | KnownAuditEventType::ApprovalResolved => {
            validate_payload::<AuditApprovalData>(data)
        }
        KnownAuditEventType::ShellCommandStarted
        | KnownAuditEventType::ShellCommandCompleted
        | KnownAuditEventType::ShellCommandFailed => {
            validate_payload::<AuditShellCommandData>(data)
        }
        KnownAuditEventType::EvidenceAccessed => validate_payload::<AuditEvidenceData>(data),
        KnownAuditEventType::AuditDegraded
        | KnownAuditEventType::AuditRecovered
        | KnownAuditEventType::RetentionPruned
        | KnownAuditEventType::ExportCreated => validate_payload::<AuditControlData>(data),
    }
}

fn validate_payload<T: serde::de::DeserializeOwned>(
    data: &Value,
) -> Result<(), AuditValidationError> {
    serde_json::from_value::<T>(data.clone())
        .map(|_| ())
        .map_err(|_| AuditValidationError::new("data", "does not match event payload schema"))
}

fn validate_identity_minimum(
    event_type: &AuditEventType,
    identity: &AuditIdentity,
) -> Result<(), AuditValidationError> {
    let name = event_type.as_str();
    if name.starts_with("tool.") && (identity.run_id.is_none() || identity.tool_use_id.is_none()) {
        return Err(AuditValidationError::new(
            "identity",
            "tool events require run_id and tool_use_id",
        ));
    }
    if name.starts_with("approval.") && (identity.run_id.is_none() || identity.request_id.is_none())
    {
        return Err(AuditValidationError::new(
            "identity",
            "approval events require run_id and request_id",
        ));
    }
    if name.starts_with("shell.command.")
        && (identity.shell_session_id.is_none() || identity.command_id.is_none())
    {
        return Err(AuditValidationError::new(
            "identity",
            "shell command events require shell_session_id and command_id",
        ));
    }
    Ok(())
}

fn identity_values(identity: &AuditIdentity) -> impl Iterator<Item = &str> {
    [
        identity.installation_id.as_deref(),
        identity.shell_session_id.as_deref(),
        identity.provider_session_id.as_deref(),
        identity.run_id.as_deref(),
        identity.turn_id.as_deref(),
        identity.request_id.as_deref(),
        identity.tool_use_id.as_deref(),
        identity.command_id.as_deref(),
    ]
    .into_iter()
    .flatten()
}

pub(super) fn validate_token(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), AuditValidationError> {
    if value.is_empty() {
        return Err(AuditValidationError::new(field, "must not be empty"));
    }
    if value.len() > maximum {
        return Err(AuditValidationError::new(field, "exceeds byte limit"));
    }
    if value
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte.is_ascii_whitespace())
    {
        return Err(AuditValidationError::new(
            field,
            "contains whitespace or control bytes",
        ));
    }
    Ok(())
}

fn validate_text(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), AuditValidationError> {
    if value.len() > maximum {
        return Err(AuditValidationError::new(field, "exceeds byte limit"));
    }
    if value.chars().any(char::is_control) {
        return Err(AuditValidationError::new(
            field,
            "contains control characters",
        ));
    }
    Ok(())
}
