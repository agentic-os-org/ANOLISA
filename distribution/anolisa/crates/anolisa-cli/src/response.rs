//! Unified CLI response envelope, error model, and renderer.
//!
//! Both human-readable and `--json` output flow through the same
//! [`CliResponse`] envelope (see launch spec §4). Handlers may render
//! their own human text directly to stdout, and on `--json` they hand a
//! payload to [`render_json`] / [`render_error`] so the on-the-wire
//! shape stays consistent across surfaces.
//!
//! Exit codes:
//! - `NOT_IMPLEMENTED` -> 64 (reserved CLI code for "command exists but
//!   handler is not wired"; chosen because POSIX `EX_USAGE` is 64 and is
//!   the closest established sentinel — launch spec §4 does not pin an
//!   exact value, so we pick a non-zero reserved code and document it
//!   here for future tightening).
//! - `INVALID_ARGUMENT` -> 2 (POSIX convention shared with clap).
//! - `NOT_INSTALLED` -> 2 (shares `INVALID_ARGUMENT`'s exit code only so
//!   shell callers already branching on 2 keep working; `--json` callers
//!   read the distinction off `error.code`). Reports a validated identity
//!   with nothing installed: the name passed identity resolution (exact
//!   local state or the component index) and the operation found nothing
//!   to act on. Unknown names never land here — a name the index rejects
//!   is `INVALID_ARGUMENT`, and a name nothing could validate because no
//!   index was available is `EXECUTION_FAILED`. What to do about a real
//!   `NOT_INSTALLED` is the caller's call and depends on the command: an
//!   absent `uninstall` target is already the desired end state, an
//!   absent `restart` target is a stale assumption.
//! - `EXECUTION_FAILED` -> 1 (generic non-zero "the command ran but the
//!   underlying operation failed at runtime"). Distinct from
//!   `INVALID_ARGUMENT` so callers can tell "I gave you bad input" apart
//!   from "you tried and something on the machine refused": download
//!   IO, install IO, state-write IO, log-write IO, lock IO. Plan-time
//!   refusals (e.g. a blocked plan) stay `INVALID_ARGUMENT` — they tell
//!   the caller to fix the input or the environment before retrying.

use std::process::ExitCode;

use serde::Serialize;

use crate::color::Palette;
use crate::context::CliContext;

/// JSON schema version for the CLI response envelope. Bump when the
/// envelope shape changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Common envelope shared by human and JSON output paths.
#[derive(Debug, Serialize)]
pub struct CliResponse<T: Serialize> {
    pub ok: bool,
    pub schema_version: u32,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<CliErrorPayload>,
}

#[derive(Debug, Serialize)]
pub struct CliErrorPayload {
    pub code: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Errors a handler can surface. The dispatcher converts these into the
/// process exit code via [`render_error`].
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Command exists in the surface but no real implementation yet.
    #[error("command '{command}' is not implemented")]
    NotImplemented {
        command: String,
        hint: Option<String>,
    },

    /// Caller-supplied arguments violated a contract.
    #[error("invalid argument: {reason}")]
    InvalidArgument { command: String, reason: String },

    /// The target is absent from ANOLISA state, so the operation had
    /// nothing to act on.
    ///
    /// Kept separate from [`CliError::InvalidArgument`] so a scripted
    /// caller can tell a state precondition from a bad invocation —
    /// enabled adapters, a quarantined record, or
    /// `--remove-system-package` with several targets are all things the
    /// caller got wrong, whereas this one may be nothing to fix at all.
    ///
    /// The name itself was validated first: identity resolution accepts only
    /// exact local-state identities and names the component index recognizes,
    /// so this code means "a supported component with nothing installed".
    /// Unknown names fail as `INVALID_ARGUMENT` (index rejected the name) or
    /// `EXECUTION_FAILED` (no index available to validate it) instead.
    ///
    /// Shares exit code 2 with [`CliError::InvalidArgument`] for backward
    /// compatibility alone — shell callers already branch on 2. It does not
    /// imply the two call for the same response; see the module docs.
    #[error("not installed: {reason}")]
    NotInstalled { command: String, reason: String },

    /// The command was well-formed but the underlying operation failed
    /// at runtime (download IO, install IO, state-write IO, log-write
    /// IO, install-lock contention/IO, etc.). Surfaced as exit code 1
    /// so wrapping scripts can distinguish "bad input" (exit 2) from
    /// "the machine refused" (exit 1).
    #[error("execution failed: {reason}")]
    Runtime { command: String, reason: String },

    /// The command completed but the resulting state is degraded
    /// (e.g. sandbox install where one or more phases emitted warnings
    /// rather than hard failure). Maps to exit code 2 so wrapping
    /// scripts can distinguish "clean success" (0) from "installed
    /// but needs attention" (2). Phase-level failures are still
    /// surfaced as `Runtime` (exit 1).
    #[error("degraded: {reason}")]
    Degraded { command: String, reason: String },

    /// The command requires elevated privileges that the process lacks.
    /// Maps to exit code 5 so callers can distinguish permission issues
    /// from other failures.
    #[error("permission denied: {reason}")]
    PermissionDenied {
        command: String,
        reason: String,
        hint: Option<String>,
    },

    /// Batch command (e.g. `install --all`) finished with one or more
    /// component failures. The handler has **already** rendered the
    /// batch summary to stdout (human text or JSON envelope). This
    /// variant exists solely to propagate a non-zero exit code without
    /// triggering a second JSON render in [`render_error`].
    #[error("batch completed with failures")]
    BatchPartial { command: String },

    /// Diagnostic command rendered its complete report and found problems.
    ///
    /// Like [`CliError::BatchPartial`], this carries only the exit status
    /// signal. The handler has already printed the structured diagnostics, so
    /// top-level error rendering must not emit a second envelope.
    #[error("diagnostics found issues")]
    DiagnosticsFound { command: String },
}

impl CliError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotImplemented { .. } => "NOT_IMPLEMENTED",
            Self::InvalidArgument { .. } => "INVALID_ARGUMENT",
            Self::NotInstalled { .. } => "NOT_INSTALLED",
            Self::Runtime { .. } => "EXECUTION_FAILED",
            Self::Degraded { .. } => "DEGRADED",
            Self::PermissionDenied { .. } => "PERMISSION_DENIED",
            Self::BatchPartial { .. } => "BATCH_PARTIAL",
            Self::DiagnosticsFound { .. } => "DIAGNOSTICS_FOUND",
        }
    }

    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NotImplemented { .. } => 64,
            Self::InvalidArgument { .. } => 2,
            Self::NotInstalled { .. } => 2,
            Self::Runtime { .. } => 1,
            Self::Degraded { .. } => 2,
            Self::PermissionDenied { .. } => 5,
            Self::BatchPartial { .. } => 1,
            Self::DiagnosticsFound { .. } => 2,
        }
    }

    pub fn command(&self) -> &str {
        match self {
            Self::NotImplemented { command, .. } => command,
            Self::InvalidArgument { command, .. } => command,
            Self::NotInstalled { command, .. } => command,
            Self::Runtime { command, .. } => command,
            Self::Degraded { command, .. } => command,
            Self::PermissionDenied { command, .. } => command,
            Self::BatchPartial { command } => command,
            Self::DiagnosticsFound { command } => command,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::NotImplemented { hint, .. } => hint.as_deref(),
            Self::InvalidArgument { .. } => None,
            Self::NotInstalled { .. } => None,
            Self::Runtime { .. } => None,
            Self::Degraded { .. } => None,
            Self::PermissionDenied { hint, .. } => hint.as_deref(),
            Self::BatchPartial { .. } => None,
            Self::DiagnosticsFound { .. } => None,
        }
    }

    pub fn reason(&self) -> String {
        match self {
            Self::NotImplemented { command, .. } => {
                format!("command '{command}' is not implemented")
            }
            Self::InvalidArgument { reason, .. } => reason.clone(),
            Self::NotInstalled { reason, .. } => reason.clone(),
            Self::Runtime { reason, .. } => reason.clone(),
            Self::Degraded { reason, .. } => reason.clone(),
            Self::PermissionDenied { reason, .. } => reason.clone(),
            Self::BatchPartial { .. } => "batch completed with failures".to_string(),
            Self::DiagnosticsFound { .. } => "diagnostics found issues".to_string(),
        }
    }

    pub fn not_implemented(command: impl Into<String>) -> Self {
        Self::NotImplemented {
            command: command.into(),
            hint: None,
        }
    }

    pub fn not_implemented_with_hint(command: impl Into<String>, hint: impl Into<String>) -> Self {
        Self::NotImplemented {
            command: command.into(),
            hint: Some(hint.into()),
        }
    }

    /// Override the command label, preserving the variant and payload.
    ///
    /// Used when a helper shared across commands (e.g. install's raw resolver
    /// reused by `update`) returns an error tagged with the wrong command
    /// verb; the calling command re-stamps it so the JSON envelope and message
    /// name the command the user actually ran.
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        let command = command.into();
        match &mut self {
            Self::NotImplemented { command: c, .. }
            | Self::InvalidArgument { command: c, .. }
            | Self::NotInstalled { command: c, .. }
            | Self::Runtime { command: c, .. }
            | Self::Degraded { command: c, .. }
            | Self::PermissionDenied { command: c, .. }
            | Self::BatchPartial { command: c }
            | Self::DiagnosticsFound { command: c } => *c = command,
        }
        self
    }
}

/// Print a successful JSON envelope to stdout. Callers should only invoke
/// this on the `--json` branch (human path stays plain `println!`).
///
/// A serialization failure surfaces as `CliError::Runtime` so the
/// caller's exit code reflects the failure instead of silently
/// returning `Ok(())`.
pub fn render_json<T: Serialize>(command: &str, data: T) -> Result<(), CliError> {
    let response = CliResponse {
        ok: true,
        schema_version: SCHEMA_VERSION,
        command: command.to_string(),
        data: Some(data),
        warnings: Vec::new(),
        error: None,
    };
    write_json(&response).map_err(|e| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to serialize JSON response: {e}"),
    })
}

/// Print a JSON envelope whose `ok` field reflects `ok`.  Used by batch
/// commands (e.g. `install --all`) that need to report partial success
/// without triggering a second error envelope from the top-level
/// `main` handler.
///
/// Callers should only invoke this on the `--json` branch.
pub fn render_json_with_status<T: Serialize>(
    command: &str,
    ok: bool,
    data: T,
) -> Result<(), CliError> {
    let response = CliResponse {
        ok,
        schema_version: SCHEMA_VERSION,
        command: command.to_string(),
        data: Some(data),
        warnings: Vec::new(),
        error: None,
    };
    write_json(&response).map_err(|e| CliError::Runtime {
        command: command.to_string(),
        reason: format!("failed to serialize JSON response: {e}"),
    })
}

/// Render an error and return the process exit code to surface.
///
/// On `--json` we emit a `CliResponse` envelope on stdout (so machine
/// callers always get parseable output, error or not). On the human path
/// we write to stderr per launch spec §4 ("warnings/debug to stderr").
///
/// If the error envelope itself fails to serialize, we fall back to
/// stderr but still return the original error's exit code so callers
/// see the failure they expected.
pub fn render_error(ctx: &CliContext, err: &CliError) -> ExitCode {
    // BatchPartial / DiagnosticsFound mean the handler already rendered the
    // complete report (JSON or human). Skip the error render entirely and just
    // propagate the non-zero exit code.
    if matches!(
        err,
        CliError::BatchPartial { .. } | CliError::DiagnosticsFound { .. }
    ) {
        return ExitCode::from(err.exit_code());
    }
    if ctx.json {
        let payload = CliErrorPayload {
            code: err.code().to_string(),
            reason: err.reason(),
            hint: err.hint().map(|s| s.to_string()),
        };
        let response: CliResponse<()> = CliResponse {
            ok: false,
            schema_version: SCHEMA_VERSION,
            command: err.command().to_string(),
            data: None,
            warnings: Vec::new(),
            error: Some(payload),
        };
        if let Err(serialize_err) = write_json(&response) {
            eprintln!(
                "internal: failed to serialize error envelope: {serialize_err}; original error: {} (code: {})",
                err.reason(),
                err.code()
            );
        }
    } else {
        let color = Palette::new(ctx.no_color);
        eprintln!("{} {}", color.err("error:"), err.reason());
        if let Some(hint) = err.hint() {
            eprintln!("{} {}", color.warn("hint:"), hint);
        }
    }
    ExitCode::from(err.exit_code())
}

fn write_json<T: Serialize>(response: &CliResponse<T>) -> Result<(), serde_json::Error> {
    let s = serde_json::to_string_pretty(response)?;
    println!("{s}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::ser::{Error as SerError, Serializer};

    /// A payload whose `Serialize` impl always fails. Used to prove
    /// `render_json` surfaces serialization failures as `CliError`
    /// instead of silently returning `Ok(())`.
    struct AlwaysFails;

    impl Serialize for AlwaysFails {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: Serializer,
        {
            Err(S::Error::custom("intentional test failure"))
        }
    }

    #[test]
    fn render_json_returns_runtime_error_when_payload_fails_to_serialize() {
        let err = render_json("status", AlwaysFails).expect_err("serialization must fail");
        match err {
            CliError::Runtime { command, reason } => {
                assert_eq!(command, "status");
                assert!(
                    reason.contains("intentional test failure"),
                    "reason should carry the underlying serde error, got: {reason}"
                );
            }
            other => panic!("expected CliError::Runtime, got {other:?}"),
        }
    }

    /// `NOT_INSTALLED` is the whole point of the variant: a caller that
    /// wants to skip an absent target must be able to key off `code`
    /// alone. The exit code deliberately stays 2, shared with
    /// `INVALID_ARGUMENT`, so existing shell callers keep working.
    #[test]
    fn not_installed_has_its_own_code_but_keeps_exit_two() {
        let err = CliError::NotInstalled {
            command: "uninstall cosh".to_string(),
            reason: "component 'cosh' is not installed — nothing to uninstall".to_string(),
        };

        assert_eq!(err.code(), "NOT_INSTALLED");
        assert_ne!(
            err.code(),
            CliError::InvalidArgument {
                command: "uninstall".to_string(),
                reason: "bad".to_string(),
            }
            .code(),
            "an absent target must not be indistinguishable from a bad argument",
        );
        assert_eq!(err.exit_code(), 2);
        assert!(err.reason().contains("not installed"));
    }

    /// `with_command` re-stamps every variant; a missing arm would silently
    /// leave the wrong command verb in the envelope.
    #[test]
    fn not_installed_survives_command_restamping() {
        let err = CliError::NotInstalled {
            command: "install".to_string(),
            reason: "component 'cosh' is not installed".to_string(),
        }
        .with_command("update cosh");

        assert_eq!(err.command(), "update cosh");
        assert_eq!(err.code(), "NOT_INSTALLED");
    }

    #[test]
    fn diagnostics_found_propagates_exit_without_error_render() {
        let sandbox = crate::test_support::TestSandbox::new();
        let ctx = sandbox.context_with(
            crate::context::InstallMode::User,
            crate::test_support::TestContextOptions {
                json: true,
                quiet: false,
                ..Default::default()
            },
        );
        let err = CliError::DiagnosticsFound {
            command: "doctor".to_string(),
        };

        assert_eq!(err.code(), "DIAGNOSTICS_FOUND");
        assert_eq!(err.exit_code(), 2);
        assert_eq!(render_error(&ctx, &err), ExitCode::from(2));
    }
}
