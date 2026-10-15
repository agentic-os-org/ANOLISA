//! Debug-only activation of the standalone approval UI slice.
//!
//! Activation is deliberately all-or-nothing: a missing key, an unparsable
//! value, a relative worker path, or a width other than the slice constant all
//! refuse activation, and the shell keeps its existing inline behaviour. There
//! is no partial activation and no production threshold here.

use std::path::PathBuf;
use std::time::Duration;

use crate::ui::worker_protocol::ProtocolLimits;

#[cfg(debug_assertions)]
use crate::ui::surface_frame::SLICE_COLUMNS;

// Gated with the only reader so a release binary cannot carry the key names at
// all, rather than relying on the optimiser to strip unreferenced constants.
#[cfg(debug_assertions)]
const ACTIVATION_KEY: &str = "COSH_UI_APPROVAL_SLICE_TEST";
#[cfg(debug_assertions)]
const WORKER_PATH_KEY: &str = "COSH_UI_WORKER_PATH";
#[cfg(debug_assertions)]
const WIDTH_KEY: &str = "COSH_SHELL_WIDTH";

/// State of the debug approval UI slice, independent from the approval facts
/// themselves so a slice failure can never take an approval decision with it.
#[derive(Default)]
pub(crate) struct ApprovalSliceState {
    pub(crate) capture_representation_reset: crate::shell_host::CaptureRepresentationReset,
    /// Parsed debug activation, `None` when the slice was not requested or the
    /// request was incomplete.
    pub(crate) activation: Option<crate::runtime::approval_ui::ApprovalSliceActivation>,
    /// Live worker session, present only between spawn and release.
    pub(crate) session: Option<Box<crate::approval::surface_session::TtyApprovalSurfaceSession>>,
    /// Host session-level presentation revision allocator; strictly monotonic and
    /// never reset by a worker rebuild.
    pub(crate) revision: u64,
    /// Set after any slice failure so the shell stops retrying the worker and
    /// keeps the inline path for the rest of the session.
    pub(crate) disabled: bool,
    pub(crate) geometry_required: bool,
    pub(crate) fresh_region_required: bool,
    pub(crate) inline_geometry: Option<(u16, u16)>,
    pub(crate) observer_event_end: usize,
    pub(crate) audit_event_cursor: crate::runtime::events::ShellEventCursor,
    pub(crate) invalidated_card_events_before: usize,
    pub(crate) observed_focus: Option<(String, crate::ui::ApprovalPanelAction)>,
}

/// Why a requested activation was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActivationError {
    /// A required policy key was absent or empty.
    MissingKey(&'static str),
    /// A byte/count ceiling did not parse.
    NotUsize(&'static str),
    /// A count or limit did not parse.
    NotU64(&'static str),
    /// A row or column count did not parse.
    NotU16(&'static str),
    /// A millisecond timeout parsed to zero.
    ZeroTimeout(&'static str),
    /// The worker binary was not an absolute path.
    WorkerPathNotAbsolute,
    /// The terminal width is not the slice width.
    WidthMismatch(u16),
    /// The heartbeat interval must be strictly below its timeout.
    HeartbeatNotBelowTimeout,
    /// A millisecond timeout was beyond the largest bound the Host will wait.
    TimeoutTooLarge(&'static str),
}

/// Everything the Host needs to spawn and supervise one worker generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApprovalSliceActivation {
    /// Pinned absolute path of the `cosh-ui-worker` candidate.
    pub(crate) worker_path: PathBuf,
    /// Frame and payload ceilings handed to the protocol codec.
    pub(crate) limits: ProtocolLimits,
    /// Rows the Host is willing to allocate to the surface.
    pub(crate) max_surface_rows: u16,
    /// Columns the Host owns; always [`SLICE_COLUMNS`].
    pub(crate) columns: u16,
    /// Bound for the Hello/Ready exchange.
    pub(crate) handshake_timeout: Duration,
    /// Bound for one render request to come back prepared.
    pub(crate) render_timeout: Duration,
    /// Cadence the worker must beat to stay considered alive.
    pub(crate) heartbeat_interval: Duration,
    /// Silence after which the worker is treated as hung.
    pub(crate) heartbeat_timeout: Duration,
    /// Bound for the Host's own outbound write backpressure.
    pub(crate) backpressure_timeout: Duration,
    /// Bound for a graceful shutdown before the worker is killed.
    pub(crate) shutdown_timeout: Duration,
    /// Injected failures tolerated before the interaction ends.
    pub(crate) failure_limit: u64,
}

impl ApprovalSliceActivation {
    /// Reads the activation contract through `lookup`.
    ///
    /// Returns `Ok(None)` when the slice is not requested at all, so an
    /// ordinary shell never pays for parsing. Returns `Err` when activation was
    /// requested but is not fully and validly specified; callers must then keep
    /// the inline path rather than activating partially.
    #[cfg(debug_assertions)]
    pub(crate) fn from_lookup(
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ActivationError> {
        match lookup(ACTIVATION_KEY).as_deref() {
            None | Some("") => return Ok(None),
            Some("1") => {}
            Some(_) => return Err(ActivationError::MissingKey(ACTIVATION_KEY)),
        }
        let worker_path = required(lookup, WORKER_PATH_KEY)?;
        let worker_path = PathBuf::from(worker_path);
        if !worker_path.is_absolute() {
            return Err(ActivationError::WorkerPathNotAbsolute);
        }
        let width = u16_value(lookup, WIDTH_KEY)?;
        if width != SLICE_COLUMNS {
            return Err(ActivationError::WidthMismatch(width));
        }
        let limits = ProtocolLimits {
            max_payload_bytes: usize_value(lookup, "COSH_UI_TEST_MAX_PAYLOAD_BYTES")?,
            max_draw_ops: usize_value(lookup, "COSH_UI_TEST_MAX_DRAW_OPS")?,
            max_text_bytes: usize_value(lookup, "COSH_UI_TEST_MAX_TEXT_BYTES")?,
        };
        let max_surface_rows = u16_value(lookup, "COSH_UI_TEST_MAX_SURFACE_ROWS")?;
        if max_surface_rows == 0 {
            return Err(ActivationError::NotU16("COSH_UI_TEST_MAX_SURFACE_ROWS"));
        }
        let handshake_timeout = millis(lookup, "COSH_UI_TEST_HANDSHAKE_TIMEOUT_MS")?;
        let render_timeout = millis(lookup, "COSH_UI_TEST_RENDER_TIMEOUT_MS")?;
        let heartbeat_interval = millis(lookup, "COSH_UI_TEST_HEARTBEAT_INTERVAL_MS")?;
        let heartbeat_timeout = millis(lookup, "COSH_UI_TEST_HEARTBEAT_TIMEOUT_MS")?;
        if heartbeat_interval >= heartbeat_timeout {
            return Err(ActivationError::HeartbeatNotBelowTimeout);
        }
        Ok(Some(Self {
            worker_path,
            limits,
            max_surface_rows,
            columns: SLICE_COLUMNS,
            handshake_timeout,
            render_timeout,
            heartbeat_interval,
            heartbeat_timeout,
            backpressure_timeout: millis(lookup, "COSH_UI_TEST_BACKPRESSURE_TIMEOUT_MS")?,
            shutdown_timeout: millis(lookup, "COSH_UI_TEST_SHUTDOWN_TIMEOUT_MS")?,
            failure_limit: u64_value(lookup, "COSH_UI_TEST_FAILURE_LIMIT")?,
        }))
    }

    /// Coordinator policy derived from this activation.
    ///
    /// Not debug-gated: it maps fields the struct already holds and contains none
    /// of the activation key strings, so compiling it into a release build leaks
    /// nothing. Gating it would break release callers that merely need to name the
    /// type.
    pub(crate) fn coordinator_policy(&self) -> crate::runtime::ui_coordinator::CoordinatorPolicy {
        crate::runtime::ui_coordinator::CoordinatorPolicy {
            protocol_limits: self.limits,
            handshake_timeout: self.handshake_timeout,
            heartbeat_interval: self.heartbeat_interval,
            heartbeat_timeout: self.heartbeat_timeout,
            backpressure_timeout: self.backpressure_timeout,
            shutdown_timeout: self.shutdown_timeout,
            failure_limit: self.failure_limit,
            pending_revision_capacity: 1,
        }
    }
}

fn required(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<String, ActivationError> {
    match lookup(key) {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(ActivationError::MissingKey(key)),
    }
}

fn usize_value(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<usize, ActivationError> {
    required(lookup, key)?
        .trim()
        .parse::<usize>()
        .map_err(|_| ActivationError::NotUsize(key))
}

fn u64_value(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<u64, ActivationError> {
    required(lookup, key)?
        .trim()
        .parse::<u64>()
        .map_err(|_| ActivationError::NotU64(key))
}

fn u16_value(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<u16, ActivationError> {
    required(lookup, key)?
        .trim()
        .parse::<u16>()
        .map_err(|_| ActivationError::NotU16(key))
}

/// Largest accepted timeout.
///
/// `Instant + Duration` panics on overflow instead of saturating, and these
/// values come from the environment straight into deadline arithmetic on the
/// controller thread, so the bound is checked where the value is parsed rather
/// than at every addition site.
const MAX_TIMEOUT_MS: u64 = 3_600_000;

fn millis(
    lookup: &dyn Fn(&str) -> Option<String>,
    key: &'static str,
) -> Result<Duration, ActivationError> {
    let value = u64_value(lookup, key)?;
    if value == 0 {
        return Err(ActivationError::ZeroTimeout(key));
    }
    if value > MAX_TIMEOUT_MS {
        return Err(ActivationError::TimeoutTooLarge(key));
    }
    Ok(Duration::from_millis(value))
}
