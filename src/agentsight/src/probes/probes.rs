// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Unified probes manager - manages sslsniff and proctrace probes
// with shared traced_processes map and shared ring buffer for coordinated process tracing

use anyhow::{Context, Result};
use libbpf_rs::{MapHandle, RingBufferBuilder};
use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use crate::config::{ChannelPolicy, RuntimeLimits};
use crate::event::Event;

use super::filewatch::{FileWatch, RawFileWatchEvent};
use super::filewrite::{FileWrite as FileWriteProbe, RawFileWriteEvent};
use super::procmon::{ProcMon, ProcMonEvent};
use super::proctrace::{ProcEventHeader, ProcTrace, VariableEvent};
use super::shared_maps::SharedMaps;
use super::sslsniff::SslSniff;
use super::tcpsniff::TcpSniff;
use super::udpdns::{RawUdpDnsEvent, UdpDns};
use crate::config::TcpTarget;

const POLL_TIMEOUT_MS: u64 = 100;

/// Snapshot of the probe event channel's byte accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelWatermarks {
    /// Approximate bytes of events queued in the channel.
    pub in_flight_bytes: usize,
    /// Configured budget; 0 means unlimited.
    pub budget_bytes: usize,
    /// Events rejected so far because the budget was exhausted.
    pub dropped_over_budget: usize,
}

/// Byte-accounted admission gate for the probe event channel.
///
/// The channel's slot count cannot bound memory on its own: one SSL record
/// carries up to `MAX_BUF_SIZE` (4 MiB), so the 10 000 default slots admit
/// gigabytes of in-flight payload (#2888).
///
/// `max_bytes == 0` disables *rejection* but keeps accounting, matching how
/// `retention_days` and `max_db_size_mb` already read 0 as "no limit" while
/// still reporting size — an operator following that convention must not end up
/// with everything dropped, nor lose the watermark readings.
#[derive(Clone)]
struct ChannelBudget {
    max_bytes: usize,
    in_flight: Arc<AtomicUsize>,
    dropped_over_budget: Arc<AtomicUsize>,
    dropped_total: Arc<AtomicUsize>,
}

impl ChannelBudget {
    fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            in_flight: Arc::new(AtomicUsize::new(0)),
            dropped_over_budget: Arc::new(AtomicUsize::new(0)),
            dropped_total: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Reserve `bytes` for an event about to be published.
    ///
    /// Must be called *before* the event becomes visible to the consumer: a
    /// post-send charge can lose the race against [`Self::release`] on the
    /// consumer side, and the saturating subtraction would then turn the late
    /// charge into a permanent phantom reservation that eventually wedges the
    /// budget shut. Returns false and counts a drop when over budget.
    fn reserve(&self, bytes: usize) -> bool {
        let max_bytes = self.max_bytes;
        let admitted = self
            .in_flight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                // Saturating so a pathological size cannot wrap into a false pass.
                let next = cur.saturating_add(bytes);
                (max_bytes == 0 || next <= max_bytes).then_some(next)
            })
            .is_ok();
        if !admitted {
            self.dropped_over_budget.fetch_add(1, Ordering::Relaxed);
            self.dropped_total.fetch_add(1, Ordering::Relaxed);
        }
        admitted
    }

    /// Give back a reservation whose event never reached the channel.
    fn refund(&self, bytes: usize) {
        self.give_back(bytes);
    }

    /// Count an event rejected after byte admission, such as a full slot queue
    /// or an intentional sampling decision.
    fn record_drop(&self) {
        self.dropped_total.fetch_add(1, Ordering::Relaxed);
    }

    /// Give back a reservation once the event has been delivered to the consumer.
    fn release(&self, bytes: usize) {
        self.give_back(bytes);
    }

    /// Saturating so an accounting mismatch cannot wrap the counter and lock out
    /// admission forever.
    fn give_back(&self, bytes: usize) {
        let _ = self
            .in_flight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |cur| {
                Some(cur.saturating_sub(bytes))
            });
    }

    fn watermarks(&self) -> ChannelWatermarks {
        ChannelWatermarks {
            in_flight_bytes: self.in_flight.load(Ordering::Relaxed),
            budget_bytes: self.max_bytes,
            dropped_over_budget: self.dropped_over_budget.load(Ordering::Relaxed),
        }
    }

    fn dropped_total(&self) -> usize {
        self.dropped_total.load(Ordering::Relaxed)
    }
}

/// Whether this cumulative drop count should be logged.
///
/// A saturated budget rejects events in bursts, so one line per drop would
/// itself become the load; powers of two keep the signal without the flood.
fn should_report_drop(dropped: usize) -> bool {
    dropped.is_power_of_two()
}

// Event source constants matching common.h event_source_t
const EVENT_SOURCE_PROC: u32 = 1;
const EVENT_SOURCE_SSL: u32 = 2;
const EVENT_SOURCE_PROCMON: u32 = 3;
const EVENT_SOURCE_FILEWATCH: u32 = 4;
const EVENT_SOURCE_FILEWRITE: u32 = 5;
const EVENT_SOURCE_UDPDNS: u32 = 6;

/// Unified probe manager that coordinates sslsniff and proctrace
///
/// This manager ensures both probes share the same traced_processes map
/// and the same ring buffer, allowing coordinated process tracing where:
/// - proctrace captures process creation events
/// - sslsniff captures SSL traffic from those processes
/// Both write to a single shared ring buffer to save memory.
pub struct Probes {
    /// Process trace probe (owns the traced_processes map and ring buffer)
    proctrace: ProcTrace,
    /// SSL sniff probe (reuses proctrace's traced_processes map and ring buffer)
    sslsniff: SslSniff,
    /// Process monitor probe (reuses ring buffer)
    procmon: ProcMon,
    /// File watch probe (reuses traced_processes map and ring buffer, optional)
    filewatch: Option<FileWatch>,
    /// File write probe (reuses traced_processes map and ring buffer, always enabled)
    filewrite: FileWriteProbe,
    /// UDP DNS probe (reuses ring buffer, captures domains from DNS queries, optional)
    udpdns: Option<UdpDns>,
    /// TCP sniff probe (captures plain HTTP traffic on configured ports, optional)
    tcpsniff: Option<TcpSniff>,
    /// Shared ring buffer handle (cloned from proctrace) for polling
    rb_handle: MapHandle,
    /// Unified event channel - events are converted to Event type inside the poller
    event_tx: crossbeam_channel::Sender<Event>,
    event_rx: crossbeam_channel::Receiver<Event>,
    /// Policy applied when the bounded event channel is full.
    event_channel_policy: ChannelPolicy,
    /// Byte-accounted admission gate; bounds memory the slot count cannot.
    budget: ChannelBudget,
}

impl Probes {
    /// Create a new unified probe manager
    ///
    /// # Arguments
    /// * `target_pids` - Initial PIDs to trace (empty means trace all matching UID)
    /// * `target_uid` - Optional UID filter
    /// * `enable_filewatch` - Enable filewatch probe
    /// * `enable_udpdns` - Enable udpdns probe
    /// * `tcp_targets` - TCP targets for plain HTTP capture
    pub fn new(
        target_pids: &[u32],
        target_uid: Option<u32>,
        enable_filewatch: bool,
        enable_udpdns: bool,
        tcp_targets: &[TcpTarget],
    ) -> Result<Self> {
        Self::new_with_cgroup_filter(
            target_pids,
            target_uid,
            enable_filewatch,
            enable_udpdns,
            tcp_targets,
            false,
            &RuntimeLimits::default(),
        )
    }

    /// Create a new unified probe manager with explicit cgroup-level filtering toggle.
    ///
    /// When `cgroup_filter_enabled` is true, every probe (except procmon, which
    /// keeps full audit coverage) gates its events behind the shared
    /// `cgroup_filter` map. Cgroup ids can then be registered at runtime via
    /// `add_traced_cgroup`. When false (default), every probe behaves exactly
    /// like before this feature existed.
    pub fn new_with_cgroup_filter(
        target_pids: &[u32],
        target_uid: Option<u32>,
        enable_filewatch: bool,
        enable_udpdns: bool,
        tcp_targets: &[TcpTarget],
        cgroup_filter_enabled: bool,
        runtime_limits: &RuntimeLimits,
    ) -> Result<Self> {
        // Create proctrace first - it owns the traced_processes map, the ring
        // buffer, and (when enabled) the cgroup_filter map.
        let proctrace = ProcTrace::new_with_target_and_maps(
            target_pids,
            target_uid,
            None,
            None,
            cgroup_filter_enabled,
            runtime_limits.ring_buffer_mb,
        )
        .context("failed to create proctrace")?;

        // Ring buffer handle kept for the polling thread (see `run`).
        let rb_handle = proctrace.rb_handle().context("failed to get rb handle")?;

        // Bundle the maps proctrace owns so every follower probe can reuse them
        // through a single `&SharedMaps` argument instead of a growing list of
        // individual `&MapHandle` parameters.
        //
        // The cgroup_filter handle is only fetched when the feature is on; when
        // off, each probe loads its own private (unused) cgroup_filter map so we
        // never burn an extra fd in the steady state.
        let mut shared = SharedMaps::new(proctrace.rb_handle().context("failed to get rb handle")?)
            .with_internal_metrics(
                proctrace
                    .internal_metrics_handle()
                    .context("failed to get internal_metrics handle")?,
            )
            .with_traced_processes(
                proctrace
                    .traced_processes_handle()
                    .context("failed to get traced_processes handle")?,
            )
            .with_cgroup_filter_enabled(cgroup_filter_enabled);
        if cgroup_filter_enabled {
            shared = shared.with_cgroup_filter(
                proctrace
                    .cgroup_filter_handle()
                    .context("failed to get cgroup_filter handle")?,
            );
        }

        // Create sslsniff - reuses the shared ring buffer and process filter.
        let sslsniff = SslSniff::new_with_shared(&shared).context("failed to create sslsniff")?;

        // Create procmon - reuses the ring buffer only (no cgroup filter: full audit)
        let procmon = ProcMon::new_with_shared(&shared).context("failed to create procmon")?;

        // Optionally create filewatch - reuses ring buffer + process/cgroup filters
        let filewatch = if enable_filewatch {
            let fw = FileWatch::new_with_shared(&shared).context("failed to create filewatch")?;
            Some(fw)
        } else {
            log::info!("FileWatch probe disabled");
            None
        };

        // Create filewrite - reuses ring buffer + process/cgroup filters (always enabled)
        let filewrite =
            FileWriteProbe::new_with_shared(&shared).context("failed to create filewrite")?;

        // Optionally create udpdns - reuses ring buffer + process filter
        // Skips already-traced processes to avoid redundant discovery events
        let udpdns = if enable_udpdns {
            let dns = UdpDns::new_with_shared(&shared).context("failed to create udpdns")?;
            Some(dns)
        } else {
            log::info!("UDP DNS probe disabled (no https/http domain rules configured)");
            None
        };

        // Optionally create tcpsniff - captures plain HTTP traffic to configured IP/port targets
        let tcpsniff = if !tcp_targets.is_empty() {
            let mut tcp =
                TcpSniff::new_with_shared(&shared).context("failed to create tcpsniff")?;
            tcp.set_targets(tcp_targets)
                .context("failed to set tcp targets")?;
            Some(tcp)
        } else {
            log::info!("TcpSniff probe disabled (no tcp_targets configured)");
            None
        };

        let capacity = runtime_limits.event_channel_capacity.max(1);
        let (event_tx, event_rx) = crossbeam_channel::bounded(capacity);

        Ok(Self {
            proctrace,
            sslsniff,
            procmon,
            filewatch,
            filewrite,
            udpdns,
            tcpsniff,
            rb_handle,
            event_tx,
            event_rx,
            event_channel_policy: runtime_limits.event_channel_policy,
            budget: ChannelBudget::new(runtime_limits.event_channel_max_bytes),
        })
    }

    /// Attach all probes
    pub fn attach(&mut self) -> Result<()> {
        // Attach procmon for process monitoring
        self.procmon.attach().context("failed to attach procmon")?;
        self.proctrace
            .attach()
            .context("failed to attach proctrace")?;
        // Attach filewatch for .jsonl file monitoring (if enabled)
        if let Some(ref mut fw) = self.filewatch {
            fw.attach().context("failed to attach filewatch")?;
        }
        // Attach filewrite for JSON write monitoring (always enabled)
        self.filewrite
            .attach()
            .context("failed to attach filewrite")?;
        // Attach udpdns for DNS query capture (if enabled)
        if let Some(ref mut dns) = self.udpdns {
            dns.attach().context("failed to attach udpdns")?;
        }
        // Attach tcpsniff for plain HTTP traffic capture (if enabled)
        if let Some(ref mut tcp) = self.tcpsniff {
            tcp.attach().context("failed to attach tcpsniff")?;
        }
        // sslsniff uses uprobes attached per-process via attach_process()
        Ok(())
    }

    pub fn attach_process(&mut self, pid: i32) -> Result<()> {
        if let Err(e) = self.attach_ssl_to_process(pid) {
            // SSL attach may fail when the process exits before
            // `<procfs root>/<pid>/maps` is readable, but a previously-attached
            // global uprobe (pid=-1) is still valid.  Register the pid in the
            // traced map so BPF events from the global uprobe are not silently
            // dropped.
            log::warn!(
                "[attach_process] pid={pid}: SSL attach failed ({e:#}); registering pid anyway for global uprobe coverage"
            );
        }
        self.add_traced_pid(pid as u32)
    }

    /// Attach SSL probes to a specific process
    pub fn attach_ssl_to_process(&mut self, pid: i32) -> Result<()> {
        self.sslsniff
            .attach_process(pid)
            .context("failed to attach sslsniff to process")?;
        Ok(())
    }

    /// Start polling for events from the shared ring buffer
    ///
    /// A single background thread polls the shared ring buffer and dispatches
    /// events as unified Event type to the channel.
    pub fn run(&self) -> Result<ProbesPoller> {
        crate::config::initialize_event_clock().context("failed to initialize event clock")?;
        let proc_min_sz = mem::size_of::<ProcEventHeader>();
        let procmon_event_size = mem::size_of::<ProcMonEvent>();
        let filewatch_event_size = mem::size_of::<RawFileWatchEvent>();
        let filewrite_event_size = mem::size_of::<RawFileWriteEvent>();
        let udpdns_event_size = mem::size_of::<RawUdpDnsEvent>();

        let event_tx = self.event_tx.clone();
        let event_policy = self.event_channel_policy;
        let budget = self.budget.clone();
        let sample_counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_inner = Arc::clone(&stop_flag);

        // Build ring buffer from the shared rb handle
        let mut rb_builder = RingBufferBuilder::new();
        rb_builder
            .add(&self.rb_handle, move |data: &[u8]| {
                // Read the first u32 to determine event source (common_event_hdr.source)
                if data.len() < 4 {
                    return 0;
                }
                let source = u32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
                let event = match source {
                    EVENT_SOURCE_PROC => {
                        // Process event - variable size, starts with proc_event_header
                        if data.len() >= proc_min_sz {
                            VariableEvent::from_bytes(data).map(Event::Proc)
                        } else {
                            None
                        }
                    }
                    EVENT_SOURCE_SSL => {
                        // SSL records are variable-length (tiered reservation):
                        // decode by header prefix + buf_size, not a full-struct cast.
                        crate::probes::sslsniff::SslEvent::from_bytes(data).map(Event::Ssl)
                    }
                    EVENT_SOURCE_PROCMON => {
                        // Process monitor event
                        if data.len() >= procmon_event_size {
                            super::procmon::Event::from_bytes(data).map(Event::ProcMon)
                        } else {
                            None
                        }
                    }
                    EVENT_SOURCE_FILEWATCH => {
                        // File watch event
                        if data.len() >= filewatch_event_size {
                            super::filewatch::FileWatchEvent::from_bytes(data).map(Event::FileWatch)
                        } else {
                            None
                        }
                    }
                    EVENT_SOURCE_FILEWRITE => {
                        // File write event (JSON content)
                        if data.len() >= filewrite_event_size {
                            super::filewrite::FileWriteEvent::from_bytes(data).map(Event::FileWrite)
                        } else {
                            None
                        }
                    }
                    EVENT_SOURCE_UDPDNS => {
                        // UDP DNS event (domain name from DNS query)
                        if data.len() >= udpdns_event_size {
                            super::udpdns::UdpDnsEvent::from_bytes(data).map(Event::UdpDns)
                        } else {
                            None
                        }
                    }
                    _ => {
                        // Unknown source - ignore
                        log::warn!("probes: unknown event source {source}");
                        None
                    }
                };

                if let Some(e) = event {
                    // Admission is gated on bytes as well as slots: the capacity
                    // counts events, but a single SSL record carries up to 4 MiB,
                    // so the slot bound alone cannot keep the tracer inside its
                    // memory budget (#2888). Reserve before publishing — see
                    // ChannelBudget::reserve for why the order matters.
                    let bytes = e.approx_bytes();
                    if !budget.reserve(bytes) {
                        let marks = budget.watermarks();
                        if should_report_drop(marks.dropped_over_budget) {
                            log::warn!(
                                "Probes event channel byte budget exhausted ({} / {} bytes in flight); \
                                 dropped {} events so far",
                                marks.in_flight_bytes,
                                marks.budget_bytes,
                                marks.dropped_over_budget
                            );
                        }
                        return 0;
                    }

                    let sent = match event_policy {
                        ChannelPolicy::Backpressure => {
                            if event_tx.send(e).is_err() {
                                log::warn!("Probes event channel closed");
                                budget.record_drop();
                                false
                            } else {
                                true
                            }
                        }
                        ChannelPolicy::DropNewest => {
                            if event_tx.try_send(e).is_err() {
                                log::warn!(
                                    "Probes event channel full (capacity={}); dropping event",
                                    event_tx.capacity().unwrap_or(0)
                                );
                                budget.record_drop();
                                false
                            } else {
                                true
                            }
                        }
                        ChannelPolicy::Sample(n) => {
                            let idx = sample_counter.fetch_add(1, Ordering::Relaxed);
                            if idx.is_multiple_of(n) {
                                if event_tx.try_send(e).is_err() {
                                    log::warn!(
                                        "Probes event channel full (capacity={}); dropping sampled event",
                                        event_tx.capacity().unwrap_or(0)
                                    );
                                    budget.record_drop();
                                    false
                                } else {
                                    true
                                }
                            } else {
                                budget.record_drop();
                                false
                            }
                        }
                    };

                    // Refund what never reached the channel, so a dropped or
                    // sampled-out event cannot leak the reservation.
                    if !sent {
                        budget.refund(bytes);
                    }
                }
                0
            })
            .context("failed to add shared ring buffer")?;
        let rb = rb_builder.build().context("failed to build ring buffer")?;

        let handle = thread::Builder::new()
            .name("probes-poll".into())
            .spawn(move || {
                let timeout = Duration::from_millis(POLL_TIMEOUT_MS);
                let outcome = super::drive_poll_loop(timeout, &stop_flag_inner, |timeout| {
                    rb.poll(timeout)
                        .map(|_| ())
                        .map_err(|error| match error.kind() {
                            libbpf_rs::ErrorKind::Interrupted => super::PollFailure::Interrupted,
                            _ => super::PollFailure::Fatal(format!("{error:#}")),
                        })
                });
                if let super::PollEnd::Failed(message) = outcome {
                    eprintln!("probes poll error: {message}");
                }
            })
            .context("failed to spawn poll thread")?;

        Ok(ProbesPoller {
            handle: Some(handle),
            stop_flag,
        })
    }

    /// Receive the next event from any probe (blocking)
    pub fn recv(&self) -> Option<Event> {
        let event = self.event_rx.recv().ok()?;
        self.budget.release(event.approx_bytes());
        Some(event)
    }

    /// Try to receive an event from any probe (non-blocking)
    pub fn try_recv(&self) -> Option<Event> {
        let event = self.event_rx.try_recv().ok()?;
        self.budget.release(event.approx_bytes());
        Some(event)
    }

    /// Current byte accounting of the probe event channel.
    pub fn channel_watermarks(&self) -> ChannelWatermarks {
        self.budget.watermarks()
    }

    /// Number of events currently waiting in the userspace event channel.
    pub fn channel_length(&self) -> usize {
        self.event_rx.len()
    }

    /// Total userspace-channel events rejected by byte admission, slot
    /// pressure, or sampling policy.
    pub fn channel_dropped(&self) -> usize {
        self.budget.dropped_total()
    }

    /// Total ring-buffer reservations rejected by the kernel probes.
    pub fn ring_buffer_dropped(&self) -> Result<u64> {
        self.proctrace.ring_buffer_dropped()
    }

    /// Add a PID to the traced_processes map at runtime
    pub fn add_traced_pid(&mut self, pid: u32) -> Result<()> {
        self.proctrace
            .add_traced_pid(pid)
            .context("failed to add traced pid")
    }

    /// Remove a PID from the traced_processes map at runtime
    pub fn remove_traced_pid(&mut self, pid: u32) -> Result<()> {
        self.proctrace
            .remove_traced_pid(pid)
            .context("failed to remove traced pid")
    }

    /// Detach SSL probes for a process and clean up traced inodes.
    pub fn detach_ssl_probes(&mut self, pid: u32) {
        self.sslsniff.detach_process(pid);
    }

    pub fn add_tcp_target(&mut self, target: &TcpTarget) -> Result<()> {
        if let Some(ref mut tcp) = self.tcpsniff {
            tcp.add_target(target)
        } else {
            log::warn!("TcpSniff not enabled, cannot add runtime target {target:?}");
            Ok(())
        }
    }

    /// Get a handle to the traced_processes map
    pub fn traced_processes_handle(&self) -> Result<MapHandle> {
        self.proctrace.traced_processes_handle()
    }

    /// Add a cgroup inode id to the shared cgroup_filter map at runtime.
    ///
    /// Has no observable effect unless probes were created with
    /// `cgroup_filter_enabled = true`; in that case, only events from
    /// processes whose cgroup id is registered here will be emitted by
    /// proctrace / filewatch / filewrite. sslsniff, udpdns, and procmon are
    /// unaffected.
    pub fn add_traced_cgroup(&mut self, cgroup_id: u64) -> Result<()> {
        self.proctrace
            .add_traced_cgroup(cgroup_id)
            .context("failed to add traced cgroup")
    }

    /// Remove a cgroup inode id from the shared cgroup_filter map at runtime.
    pub fn remove_traced_cgroup(&mut self, cgroup_id: u64) -> Result<()> {
        self.proctrace
            .remove_traced_cgroup(cgroup_id)
            .context("failed to remove traced cgroup")
    }
}

/// Poller handle for the unified ring buffer thread
pub struct ProbesPoller {
    handle: Option<thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

impl ProbesPoller {
    /// Stop the poller thread
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for ProbesPoller {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_admits_until_exhausted() {
        let budget = ChannelBudget::new(1024);
        assert!(budget.reserve(1000));
        assert!(budget.reserve(24));
        assert!(!budget.reserve(1));
        let marks = budget.watermarks();
        assert_eq!(marks.in_flight_bytes, 1024);
        assert_eq!(marks.dropped_over_budget, 1);
        assert_eq!(budget.dropped_total(), 1);
    }

    #[test]
    fn budget_rejects_single_oversized_event() {
        // A 4 MiB SSL record must not be admitted into a smaller budget: this is
        // the case the slot-count bound alone let through (#2888).
        let budget = ChannelBudget::new(64 * 1024);
        assert!(!budget.reserve(4 * 1024 * 1024));
        assert_eq!(budget.watermarks().in_flight_bytes, 0);
    }

    #[test]
    fn rejected_reservation_does_not_consume_budget() {
        // A rejected event must leave room for the next, smaller one; otherwise a
        // single oversized record would wedge the channel shut.
        let budget = ChannelBudget::new(1024);
        assert!(!budget.reserve(2048));
        assert!(budget.reserve(1024));
    }

    #[test]
    fn refund_returns_unsent_reservation() {
        let budget = ChannelBudget::new(1024);
        assert!(budget.reserve(512));
        budget.refund(512);
        assert_eq!(budget.watermarks().in_flight_bytes, 0);
        // Full budget available again after the refund.
        assert!(budget.reserve(1024));
    }

    #[test]
    fn post_admission_drop_is_counted_and_refunded_independently() {
        let budget = ChannelBudget::new(1024);
        assert!(budget.reserve(512));
        budget.record_drop();
        budget.refund(512);
        let marks = budget.watermarks();
        assert_eq!(marks.in_flight_bytes, 0);
        assert_eq!(marks.dropped_over_budget, 0);
        assert_eq!(budget.dropped_total(), 1);
    }

    #[test]
    fn zero_budget_means_unlimited_but_still_accounts() {
        // Operators read 0 as "no limit" from retention_days / max_db_size_mb;
        // treating it as a 1-byte budget would silently drop every event. The
        // watermark must stay readable so the gate can be diagnosed while off.
        let budget = ChannelBudget::new(0);
        assert!(budget.reserve(4 * 1024 * 1024));
        assert!(budget.reserve(4 * 1024 * 1024));
        let marks = budget.watermarks();
        assert_eq!(marks.in_flight_bytes, 8 * 1024 * 1024);
        assert_eq!(marks.dropped_over_budget, 0);
        assert_eq!(budget.dropped_total(), 0);
        assert_eq!(marks.budget_bytes, 0);
    }

    #[test]
    fn accounting_saturates_instead_of_wrapping() {
        let budget = ChannelBudget::new(usize::MAX);
        assert!(budget.reserve(usize::MAX));
        // Would wrap to a tiny value and falsely pass without saturation.
        assert!(budget.reserve(8));
        budget.release(usize::MAX);
        budget.release(usize::MAX);
        assert_eq!(budget.watermarks().in_flight_bytes, 0);
    }

    #[test]
    fn sustained_load_never_exceeds_byte_budget() {
        const BUDGET: usize = 64 * 1024;
        const EVENT_BYTES: usize = 4096;
        const ATTEMPTS: usize = 100_000;

        let budget = ChannelBudget::new(BUDGET);
        let mut admitted = 0;
        for _ in 0..ATTEMPTS {
            if budget.reserve(EVENT_BYTES) {
                admitted += 1;
            }
        }

        let marks = budget.watermarks();
        assert!(marks.in_flight_bytes <= BUDGET);
        assert_eq!(admitted, BUDGET / EVENT_BYTES);
        assert_eq!(marks.dropped_over_budget, ATTEMPTS - admitted);
        assert_eq!(budget.dropped_total(), ATTEMPTS - admitted);

        for _ in 0..admitted {
            budget.release(EVENT_BYTES);
        }
        assert_eq!(budget.watermarks().in_flight_bytes, 0);
    }

    #[test]
    fn drop_reports_are_rate_limited_to_powers_of_two() {
        assert!(should_report_drop(1));
        assert!(should_report_drop(2));
        assert!(should_report_drop(1024));
        assert!(!should_report_drop(3));
        assert!(!should_report_drop(1000));
    }

    /// Reserve-then-publish must survive the consumer draining concurrently.
    ///
    /// Charging after the send loses the race when the consumer receives and
    /// releases first: the saturating subtraction floors at zero and the late
    /// charge becomes a permanent phantom reservation, which eventually reports
    /// the budget as exhausted and drops valid events forever.
    #[test]
    fn concurrent_drain_leaves_no_phantom_reservation() {
        const EVENTS: usize = 2_000;
        const BYTES: usize = 4096;

        let budget = ChannelBudget::new(EVENTS * BYTES);
        let (tx, rx) = crossbeam_channel::bounded::<usize>(4);

        let consumer_budget = budget.clone();
        let consumer = thread::spawn(move || {
            let mut received = 0usize;
            while let Ok(bytes) = rx.recv() {
                consumer_budget.release(bytes);
                received += 1;
            }
            received
        });

        for _ in 0..EVENTS {
            assert!(budget.reserve(BYTES));
            tx.send(BYTES).expect("consumer alive");
        }
        drop(tx);

        assert_eq!(consumer.join().expect("consumer thread"), EVENTS);
        let marks = budget.watermarks();
        assert_eq!(
            marks.in_flight_bytes, 0,
            "every reservation must be released once the channel is drained"
        );
        assert_eq!(marks.dropped_over_budget, 0);
        assert_eq!(budget.dropped_total(), 0);

        // The budget is fully reusable, i.e. no reservation leaked.
        assert!(budget.reserve(EVENTS * BYTES));
    }
}
