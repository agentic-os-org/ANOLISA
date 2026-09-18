//! The hook metric allowlist, derived from the typed record definitions.
//!
//! Migrated from v1 `observability/metrics.py`. The allowlist is *derived* from
//! the record schema, not a runtime config file: changing accepted metrics
//! changes the public wire contract, so it must go through [`crate::hook`].

use std::collections::BTreeMap;

use crate::hook::{OBSERVABILITY_HOOKS, ObservabilityHook};

/// Returns hook name to allowed metric names, mirroring v1
/// `HOOK_METRIC_ALLOWLIST`.
///
/// The inner slices keep v1 declaration order; callers that only need
/// membership can ignore it.
#[must_use]
pub fn hook_metric_allowlist() -> BTreeMap<&'static str, &'static [&'static str]> {
    OBSERVABILITY_HOOKS
        .iter()
        .map(|hook| (hook.as_str(), hook.metric_names()))
        .collect()
}

/// Returns the metric names allowed for `hook`, or an empty slice.
///
/// Mirrors v1 `allowed_metrics_for_hook`.
#[must_use]
pub fn allowed_metrics_for_hook(hook: &str) -> &'static [&'static str] {
    ObservabilityHook::parse(hook).map_or(&[], ObservabilityHook::metric_names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_covers_every_hook() {
        let allowlist = hook_metric_allowlist();
        assert_eq!(allowlist.len(), OBSERVABILITY_HOOKS.len());
        for hook in OBSERVABILITY_HOOKS {
            assert_eq!(
                allowlist.get(hook.as_str()).copied(),
                Some(hook.metric_names())
            );
        }
    }

    #[test]
    fn unknown_hook_has_no_allowed_metrics() {
        assert!(allowed_metrics_for_hook("nope").is_empty());
        assert_eq!(
            allowed_metrics_for_hook("before_tool_call"),
            &["tool_name", "parameters", "pii_scan_input_sha256"]
        );
    }
}
