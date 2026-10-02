//! Cross-owner status tokens emitted by adapters and consumed by Agent UI.

/// Wire phase reported while a tool's arguments are still streaming.
pub(crate) const TOOL_ARGUMENTS_STATUS_PHASE: &str = "tool_arguments";

/// Marker prefix carrying the tool name during argument generation.
///
/// The status crosses the adapter boundary as a stable English string and is
/// localized at render time, matching the other neutral status markers.
pub(crate) const TOOL_ARGUMENTS_STATUS_PREFIX: &str = "generating tool arguments: ";
