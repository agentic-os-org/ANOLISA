// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Shared BPF maps bundle.
//
// Several probes coordinate by reusing the same BPF maps that proctrace owns:
// the shared ring buffer (`rb`), internal counters (`internal_metrics`), the
// process filter (`traced_processes`), and (optionally) the cgroup filter.
// Historically each map was threaded through every probe constructor as a
// separate `&MapHandle` argument, so adding one more shared map meant editing
// the signature of every probe.
//
// `SharedMaps` replaces that growing argument list with a single bundle. A probe
// now takes one `&SharedMaps` and declares which maps it consumes as a
// `&[MapKind]` array; `reuse_into` wires them up by name. Adding a new shared
// map is a localized change here (one field + one builder method) plus listing
// the new `MapKind` in the probes that want it — no probe signature changes.

use anyhow::{Context, Result, anyhow};
use libbpf_rs::{MapHandle, OpenObject};
use std::os::fd::AsFd;

/// Logical identity of a BPF map shared across probes.
///
/// The string returned by [`MapKind::as_str`] MUST equal the map's name in the
/// BPF C source, because [`SharedMaps::reuse_into`] looks maps up by name.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MapKind {
    /// Shared ring buffer every probe writes events into.
    Rb,
    /// Per-CPU counters shared by every ring-buffer producer.
    InternalMetrics,
    /// Process filter map (`pid -> traced`).
    TracedProcesses,
    /// Cgroup filter map; only consulted when cgroup filtering is enabled.
    CgroupFilter,
}

impl MapKind {
    /// Name of the map as declared in the BPF object.
    pub const fn as_str(self) -> &'static str {
        match self {
            MapKind::Rb => "rb",
            MapKind::InternalMetrics => "internal_metrics",
            MapKind::TracedProcesses => "traced_processes",
            MapKind::CgroupFilter => "cgroup_filter",
        }
    }
}

/// A bundle of BPF maps shared between probes.
///
/// `rb` is always present; the other maps are optional because standalone
/// probes may own private maps and cgroup filtering is off by default. Build
/// one with [`SharedMaps::new`] plus the `with_*` methods, then hand
/// `&SharedMaps` to each probe.
pub struct SharedMaps {
    rb: MapHandle,
    internal_metrics: Option<MapHandle>,
    traced_processes: Option<MapHandle>,
    cgroup_filter: Option<MapHandle>,
    cgroup_filter_enabled: bool,
}

impl SharedMaps {
    /// Create a bundle that shares the ring buffer only.
    pub fn new(rb: MapHandle) -> Self {
        Self {
            rb,
            internal_metrics: None,
            traced_processes: None,
            cgroup_filter: None,
            cgroup_filter_enabled: false,
        }
    }

    /// Also share the per-CPU internal metrics map.
    pub fn with_internal_metrics(mut self, internal_metrics: MapHandle) -> Self {
        self.internal_metrics = Some(internal_metrics);
        self
    }

    /// Also share the `traced_processes` process-filter map.
    pub fn with_traced_processes(mut self, traced_processes: MapHandle) -> Self {
        self.traced_processes = Some(traced_processes);
        self
    }

    /// Also share the `cgroup_filter` map.
    pub fn with_cgroup_filter(mut self, cgroup_filter: MapHandle) -> Self {
        self.cgroup_filter = Some(cgroup_filter);
        self
    }

    /// Set the cgroup-filter rodata flag baked into cgroup-aware probes.
    pub fn with_cgroup_filter_enabled(mut self, enabled: bool) -> Self {
        self.cgroup_filter_enabled = enabled;
        self
    }

    /// Whether cgroup-level filtering is active. Probes that support it copy
    /// this into their BPF `filter_cgroup_enabled` rodata flag.
    pub fn cgroup_filter_enabled(&self) -> bool {
        self.cgroup_filter_enabled
    }

    /// The handle for a given kind, if this bundle holds it.
    fn handle(&self, kind: MapKind) -> Option<&MapHandle> {
        match kind {
            MapKind::Rb => Some(&self.rb),
            MapKind::InternalMetrics => self.internal_metrics.as_ref(),
            MapKind::TracedProcesses => self.traced_processes.as_ref(),
            MapKind::CgroupFilter => self.cgroup_filter.as_ref(),
        }
    }

    /// The kinds this bundle currently holds (`rb` is always present).
    pub fn available_kinds(&self) -> Vec<MapKind> {
        available_from_presence(
            self.internal_metrics.is_some(),
            self.traced_processes.is_some(),
            self.cgroup_filter.is_some(),
        )
    }

    /// Reuse each wanted-and-available shared map into `obj`, matching maps by
    /// name ([`MapKind::as_str`]).
    ///
    /// Returns the kinds actually reused — the intersection of `want` and what
    /// the bundle holds, in `want` order. A wanted map the bundle does not hold
    /// is silently skipped (e.g. `cgroup_filter` when cgroup filtering is off),
    /// which is exactly how the previous `Option<&MapHandle>` parameters
    /// behaved. A wanted map that the bundle holds but the BPF object does not
    /// declare is a programming error and returns `Err`.
    pub fn reuse_into(&self, want: &[MapKind], obj: &mut OpenObject) -> Result<Vec<MapKind>> {
        let plan = plan_reuse(want, &self.available_kinds());
        for &kind in &plan {
            // `plan_reuse` only yields kinds present in `available_kinds`, so the
            // handle is guaranteed to exist.
            let handle = self
                .handle(kind)
                .expect("plan_reuse only yields available kinds");
            let name = kind.as_str();
            let map = obj
                .map_mut(name)
                .ok_or_else(|| anyhow!("BPF object has no shared map named '{name}'"))?;
            map.reuse_fd(handle.as_fd())
                .with_context(|| format!("failed to reuse shared '{name}' map"))?;
        }
        Ok(plan)
    }
}

/// Which kinds are available given the presence of the optional maps.
///
/// Split out as a pure function so the selection logic is unit-testable without
/// constructing real BPF map handles.
fn available_from_presence(
    has_internal_metrics: bool,
    has_traced_processes: bool,
    has_cgroup_filter: bool,
) -> Vec<MapKind> {
    let mut kinds = vec![MapKind::Rb];
    if has_internal_metrics {
        kinds.push(MapKind::InternalMetrics);
    }
    if has_traced_processes {
        kinds.push(MapKind::TracedProcesses);
    }
    if has_cgroup_filter {
        kinds.push(MapKind::CgroupFilter);
    }
    kinds
}

/// The maps to reuse: those in `want` that are also `available`, in `want`
/// order, with duplicates removed.
///
/// Pure (no BPF), so it can be exhaustively unit-tested.
fn plan_reuse(want: &[MapKind], available: &[MapKind]) -> Vec<MapKind> {
    let mut planned = Vec::new();
    for &kind in want {
        if available.contains(&kind) && !planned.contains(&kind) {
            planned.push(kind);
        }
    }
    planned
}

#[cfg(test)]
mod tests {
    use super::MapKind::*;
    use super::*;

    #[test]
    fn map_kind_names_match_bpf_map_names() {
        // These strings are load-bearing: reuse_into looks maps up by name.
        assert_eq!(Rb.as_str(), "rb");
        assert_eq!(InternalMetrics.as_str(), "internal_metrics");
        assert_eq!(TracedProcesses.as_str(), "traced_processes");
        assert_eq!(CgroupFilter.as_str(), "cgroup_filter");
    }

    #[test]
    fn available_includes_rb_and_present_optionals() {
        assert_eq!(available_from_presence(false, false, false), vec![Rb]);
        assert_eq!(
            available_from_presence(false, true, false),
            vec![Rb, TracedProcesses]
        );
        assert_eq!(
            available_from_presence(false, false, true),
            vec![Rb, CgroupFilter]
        );
        assert_eq!(
            available_from_presence(true, true, true),
            vec![Rb, InternalMetrics, TracedProcesses, CgroupFilter]
        );
        assert_eq!(
            available_from_presence(false, true, true),
            vec![Rb, TracedProcesses, CgroupFilter]
        );
    }

    #[test]
    fn plan_keeps_only_available_preserving_want_order() {
        let available = [Rb, TracedProcesses];
        // cgroup_filter wanted but unavailable -> dropped; want order preserved.
        assert_eq!(
            plan_reuse(&[Rb, TracedProcesses, CgroupFilter], &available),
            vec![Rb, TracedProcesses]
        );
        // Result follows `want` order, not `available` order.
        assert_eq!(
            plan_reuse(&[TracedProcesses, Rb], &available),
            vec![TracedProcesses, Rb]
        );
    }

    #[test]
    fn plan_rb_only_probe() {
        // A standalone consumer may still request only the ring buffer.
        assert_eq!(
            plan_reuse(&[Rb], &[Rb, TracedProcesses, CgroupFilter]),
            vec![Rb]
        );
    }

    #[test]
    fn plan_dedups_repeated_want() {
        assert_eq!(
            plan_reuse(&[Rb, Rb, TracedProcesses, Rb], &[Rb, TracedProcesses]),
            vec![Rb, TracedProcesses]
        );
    }

    #[test]
    fn plan_empty_want_yields_nothing() {
        assert!(plan_reuse(&[], &[Rb, TracedProcesses]).is_empty());
    }

    #[test]
    fn plan_yields_nothing_when_nothing_available() {
        assert!(plan_reuse(&[Rb, TracedProcesses], &[]).is_empty());
    }
}
