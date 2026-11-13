// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2023 Yusheng Zheng
//
// SSL/TLS sniffer built on libbpf-rs.
// Exposes a `SslSniff` struct with a builder-style API.

use crate::config;
use crate::utils::procfs::{proc_pid_entry, proc_pid_rooted};
use anyhow::{Context, Result};
use libbpf_rs::{
    Link, RingBufferBuilder, UprobeOpts,
    skel::{OpenSkel, SkelBuilder},
};

use super::pidns::proc_root_is_init_pidns;
use super::shared_maps::{MapKind, SharedMaps};
use std::{
    collections::{HashMap, HashSet},
    fs,
    mem::{self, MaybeUninit},
    path::Path,
    slice,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

mod late_retry;
mod scan;
mod scan_jobs;
use crate::utils::cpu_budget::{CpuBudget, CpuPermit, WorkClass};
pub(crate) use late_retry::IdentityStatus;
use late_retry::{LateRetries, ProcessIdentity};
use scan::{collect_pattern_hits, fill_buf};
#[cfg(test)]
use scan::{file_has_ssl_markers, ssl_exe_markers};
use scan_jobs::{ScanMode, SubmitError};

// ─── Generated skeleton ───────────────────────────────────────────────────────
#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    non_snake_case
)]
pub mod bpf {
    include!(concat!(env!("OUT_DIR"), "/sslsniff.skel.rs"));
    include!(concat!(env!("OUT_DIR"), "/sslsniff.rs"));
}
use bpf::*;

// ─── Constants ────────────────────────────────────────────────────────────────
const MAX_BUF_SIZE: usize = bpf::MAX_BUF_SIZE as usize;

/// How long a global uprobe attachment is trusted across repeated discovery
/// of the same PID. The kernel can deregister uprobe consumers without any
/// userspace-visible error (observed on serverless/overlayfs hosts), leaving
/// the `Link` objects alive but the probes silent. Userspace cannot query
/// liveness, so a newly discovered PID always re-attaches and TTL expiry is
/// the recovery path for later discoveries of that PID.
///
/// Keeping the TTL for repeat discoveries prevents DNS traffic from causing
/// continuous uprobe churn, while the new-PID path closes the 30-second window
/// for short-lived processes (#3034, #3494).
const STALE_REATTACH_TTL: Duration = Duration::from_secs(30);

/// Effective stale re-attach TTL, resolved once per `SslSniff` at
/// construction. `AGENTSIGHT_SSL_REATTACH_TTL_SECS` overrides the default
/// (0 forces a re-attach on every attach attempt; used by tests).
/// Parse the TTL from an optional env-var string, falling back to the
/// default on absence or parse failure. Extracted so tests never touch the
/// process environment (which would be unsound under the parallel harness).
fn parse_reattach_ttl(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(STALE_REATTACH_TTL)
}

fn stale_reattach_ttl() -> Duration {
    parse_reattach_ttl(
        std::env::var("AGENTSIGHT_SSL_REATTACH_TTL_SECS")
            .ok()
            .as_deref(),
    )
}

/// Per-file uprobe attachment state used for stale re-attach.
struct InodeAttach {
    /// Held for Drop (detaches the uprobes); never read directly.
    _links: Vec<Link>,
    attached_at: Instant,
}

fn should_reattach(
    attachment: &InodeAttach,
    now: Instant,
    ttl: Duration,
    first_attach_for_pid: bool,
) -> bool {
    first_attach_for_pid || now.duration_since(attachment.attached_at) >= ttl
}

/// Outcome of a single uprobe attach attempt for one library.
enum AttachOutcome {
    /// Probes attached successfully.
    Attached(Vec<Link>),
    /// No known SSL entry points in this binary; retrying is pointless, so
    /// the file stays marked in `traced_files` without an attachment.
    Untraceable,
    /// Transient attach failure; the caller unmarks the file so a later
    /// sweep can retry.
    Failed,
    /// The bounded scan queue is full; retry after a short backoff.
    ScanQueueFull,
    /// Keep the inode association while a bounded worker inspects its bytes.
    Deferred,
}

/// User-space SslEvent - lightweight version of BPF probe_SSL_data_t
///
/// Unlike the BPF version which has a 512KB fixed-size buffer, this struct
/// only stores the actual data received, significantly reducing memory usage.
#[derive(Debug, Clone)]
pub struct SslEvent {
    pub source: u32,
    pub timestamp_ns: u64,
    pub delta_ns: u64,
    pub pid: u32,
    pub tid: u32,
    pub uid: u32,
    pub len: u32,
    pub rw: i32,
    pub comm: String,
    /// Actual data buffer (only contains received data, not full 512KB)
    pub buf: Vec<u8>,
    pub is_handshake: bool,
    pub ssl_ptr: u64,
}

impl SslEvent {
    /// Preserve provenance without retaining or copying captured payload bytes.
    pub(crate) fn metadata_only(&self) -> Self {
        Self {
            source: self.source,
            timestamp_ns: self.timestamp_ns,
            delta_ns: self.delta_ns,
            pid: self.pid,
            tid: self.tid,
            uid: self.uid,
            len: 0,
            rw: self.rw,
            comm: self.comm.clone(),
            buf: Vec::new(),
            is_handshake: self.is_handshake,
            ssl_ptr: self.ssl_ptr,
        }
    }

    /// Create SslEvent from a raw ring-buffer sample of VARIABLE length.
    ///
    /// SSL records are tiered: the BPF side reserves only
    /// `offsetof(probe_SSL_data_t, buf) + <tier>` bytes, so a sample is the
    /// header prefix followed by `buf_size` payload bytes — NOT the full
    /// fixed-size struct. We therefore (1) gate on the header size, (2) read each
    /// scalar field from the prefix at its real (bindgen) offset — NOT via a
    /// full-struct cast, which is UB on a short sample and would also put the
    /// 4 MiB `buf` array on the stack if materialized — and (3) slice the payload
    /// by the `buf_size` FIELD, never by `data.len()` (which over-counts for any
    /// still-full-size record such as tcpsniff's 4 MiB reservation).
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        type R = bpf::probe_SSL_data_t;
        let hdr = mem::offset_of!(R, buf);
        if data.len() < hdr {
            return None;
        }
        // Build the byte arrays directly (no `.unwrap()`; see AGENTS.md §0). Every `off`
        // is a header field offset < `hdr`, and the guard above proves `data.len() >= hdr`,
        // so each index is in-bounds (`buf` is the last struct member).
        let u32_at = |off: usize| {
            u32::from_ne_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };
        let u64_at = |off: usize| {
            u64::from_ne_bytes([
                data[off],
                data[off + 1],
                data[off + 2],
                data[off + 3],
                data[off + 4],
                data[off + 5],
                data[off + 6],
                data[off + 7],
            ])
        };
        let i32_at = |off: usize| {
            i32::from_ne_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
        };

        let len = u32_at(mem::offset_of!(R, len)) as usize;
        let buf_size = (u32_at(mem::offset_of!(R, buf_size)) as usize)
            .min(MAX_BUF_SIZE)
            .min(data.len() - hdr);
        // Warn only on a genuine over-cap capture. The `len > MAX_BUF_SIZE` guard is
        // defense-in-depth: every EVENT_SOURCE_SSL producer shares this header and
        // bpf_ringbuf_reserve does not zero the reservation, so a producer that omits
        // `truncated` cannot trip a false warning on stale ring bytes.
        if i32_at(mem::offset_of!(R, truncated)) != 0 && len > MAX_BUF_SIZE {
            log::warn!(
                "SSL payload exceeded {}-byte capture cap; captured {} of {} bytes (pid={})",
                MAX_BUF_SIZE,
                buf_size,
                len,
                u32_at(mem::offset_of!(R, pid)),
            );
        }
        let buf = data[hdr..hdr + buf_size].to_vec();

        let comm_off = mem::offset_of!(R, comm);
        let mut comm_arr = [0u8; 16];
        comm_arr.copy_from_slice(&data[comm_off..comm_off + 16]);

        Some(Self {
            source: u32_at(mem::offset_of!(R, source)),
            timestamp_ns: config::ktime_to_unix_ns(u64_at(mem::offset_of!(R, timestamp_ns)))
                .inspect_err(|error| config::report_clock_error("sslsniff", error))
                .ok()?,
            delta_ns: u64_at(mem::offset_of!(R, delta_ns)),
            pid: u32_at(mem::offset_of!(R, pid)),
            tid: u32_at(mem::offset_of!(R, tid)),
            uid: u32_at(mem::offset_of!(R, uid)),
            len: len as u32,
            rw: i32_at(mem::offset_of!(R, rw)),
            comm: Self::parse_comm(&comm_arr),
            buf,
            is_handshake: i32_at(mem::offset_of!(R, is_handshake)) != 0,
            ssl_ptr: u64_at(mem::offset_of!(R, ssl_ptr)),
        })
    }

    /// Parse comm from the BPF struct field (layout matches C `char comm[16]`; generated
    /// bindings may use `[i8; 16]` or `[u8; 16]` depending on target / libbpf-cargo version).
    fn parse_comm<T>(comm: &[T; 16]) -> String {
        debug_assert_eq!(mem::size_of::<T>(), 1);
        let bytes = unsafe { slice::from_raw_parts(comm.as_ptr() as *const u8, 16) };
        let bytes: Vec<u8> = bytes.iter().copied().take_while(|&b| b != 0).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// Get payload as string (if valid UTF-8)
    pub fn payload(&self) -> Option<&str> {
        std::str::from_utf8(&self.buf).ok()
    }

    /// Check if payload starts with "HTTP" (case-insensitive) without converting to string
    /// This is useful for detecting HTTP responses without UTF-8 validation overhead
    pub fn is_http(&self) -> bool {
        self.is_http_request() || self.is_http_response()
    }

    pub fn is_http_request(&self) -> bool {
        const METHODS: &[&[u8]] = &[
            b"GET ", b"POST", b"PUT ", b"DELE", b"HEAD", b"OPTI", b"PATC",
        ];
        METHODS.iter().any(|m| self.buf.starts_with(m))
    }

    pub fn is_http_response(&self) -> bool {
        self.buf.starts_with(b"HTTP")
    }

    /// Check if payload is an HTTP/2 connection preface
    pub fn is_http2_preface(&self) -> bool {
        self.buf.starts_with(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
    }

    /// Heuristic check for HTTP/2 binary frame data
    pub fn is_http2_frame(&self) -> bool {
        if self.buf.len() < 9 {
            return false;
        }
        // Parse 3-byte frame length
        let length =
            ((self.buf[0] as usize) << 16) | ((self.buf[1] as usize) << 8) | (self.buf[2] as usize);
        // Frame type must be a known type (0..=9)
        let frame_type = self.buf[3];
        if frame_type > 9 {
            return false;
        }
        // Reserved bit of stream ID must be 0
        if self.buf[5] & 0x80 != 0 {
            return false;
        }
        // Frame payload must fit in the buffer
        9 + length <= self.buf.len()
    }

    /// Check if payload is HTTP/2 (preface or binary frames)
    pub fn is_http2(&self) -> bool {
        self.is_http2_preface() || self.is_http2_frame()
    }

    /// Get comm as a String
    pub fn comm_str(&self) -> String {
        self.comm.clone()
    }

    /// Get the SSL connection pointer for connection tracking
    pub fn ssl_ptr(&self) -> u64 {
        self.ssl_ptr
    }

    /// Get the connection ID (pid, ssl_ptr) for unique connection identification
    pub fn connection_id(&self) -> (u32, u64) {
        (self.pid, self.ssl_ptr)
    }

    /// Get buf_size (convenience method for compatibility)
    pub fn buf_size(&self) -> u32 {
        self.buf.len() as u32
    }
}

// ─── Main struct ──────────────────────────────────────────────────────────────

/// Identity of a mapped file: `(device, inode)` from its maps entry.
///
/// Inode numbers are unique per superblock, not globally: two different files
/// on different filesystems can share one, so the bare inode is not a valid
/// dedup key. The device encodes the maps `maj:min` field, major in the high
/// 32 bits.
type FileKey = (u64, u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SslAdmission {
    identity: ProcessIdentity,
}

impl SslAdmission {
    pub(super) fn pid(self) -> u32 {
        self.identity.pid
    }

    pub(super) fn status(self) -> IdentityStatus {
        self.identity.status()
    }
}

/// Bounds deferred queue-full retries so the scanner cannot grow without limit.
const MAX_SCAN_RETRIES: usize = 32;

struct ScanRetry {
    deadline: Instant,
    path: String,
    mode: ScanMode,
}

pub struct SslSniff {
    // We store the skel behind a Box so we can hold it alongside the
    // links without lifetime trouble.  The MaybeUninit holds the
    // OpenObject allocation that the skeleton borrows from.
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<SslsniffSkel<'static>>,
    /// Per-file uprobe links with attach metadata (for stale re-attach).
    attachments: HashMap<FileKey, InodeAttach>,
    /// Resolved static offsets and whether to attach the handshake probes.
    /// Avoids repeating whole-binary scans for each newly discovered PID.
    static_attach_offsets: HashMap<FileKey, (StaticSslOffsets, bool)>,
    /// Offsets learned by earlier scans, persisted across restarts so a
    /// previously scanned binary attaches synchronously at discovery (#9762).
    learned_offsets: LearnedOffsets,
    traced_files: HashSet<FileKey>,
    /// Maps pid -> files that were attached for this pid.
    /// Used to clean up traced_files when the process exits.
    pid_inodes: HashMap<u32, Vec<FileKey>>,
    /// Start-time identities remain after retry completion to detect PID reuse.
    pid_identities: HashMap<u32, ProcessIdentity>,
    /// Stale re-attach TTL, resolved once at construction from the
    /// `AGENTSIGHT_SSL_REATTACH_TTL_SECS` override (or the default).
    reattach_ttl: Duration,
    /// How many stale attachments have been re-attached (diagnostics/tests).
    stale_reattachs: u64,
    scans: scan_jobs::ScanJobs,
    /// Retry deadlines for files that arrived while the bounded scan queue was full.
    scan_retry_after: HashMap<FileKey, ScanRetry>,
    /// Unclassified pids waiting for a deferred scan to install its uprobes.
    pending_pid_admissions: HashMap<u32, ProcessIdentity>,
    /// Processes whose exec-time maps did not yield a live SSL attachment.
    late_retries: LateRetries,
    /// Rate-limits procfs liveness sweeps to once per second.
    last_pid_prune: Instant,
    // Channel for user-space SslEvent (lightweight, no need for Box)
    tx: crossbeam_channel::Sender<SslEvent>,
    rx: crossbeam_channel::Receiver<SslEvent>,
}

/// Maps sslsniff reuses from the shared bundle: ring buffer + process filter.
const SHARED_MAPS: &[MapKind] = &[
    MapKind::Rb,
    MapKind::InternalMetrics,
    MapKind::TracedProcesses,
];

impl SslSniff {
    /// Create a new SslSniff with its own (unshared) maps.
    pub fn new() -> Result<Self> {
        Self::build(None)
    }

    /// Reuse the shared ring buffer, internal counters, and process filter.
    pub fn new_with_shared(shared: &SharedMaps) -> Result<Self> {
        Self::build(Some(shared))
    }

    /// Open + load the skeleton (optionally reusing shared maps) and build `Self`.
    fn build(shared: Option<&SharedMaps>) -> Result<Self> {
        // ── Open + load skeleton ───────────────────────────────────────
        let mut builder = SslsniffSkelBuilder::default();
        builder.obj_builder.debug(config::verbose());
        // Keep MaybeUninit on the heap so its address is stable.
        let open_object = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let mut open_skel = builder.open().context("failed to open BPF object")?;

        // Tell BPF which namespace to report event pids in.
        open_skel.rodata_mut().observer_pidns_is_init = proc_root_is_init_pidns();

        // Reuse shared maps when running under the unified manager.
        if let Some(shared) = shared {
            shared
                .reuse_into(SHARED_MAPS, open_skel.open_object_mut())
                .context("failed to reuse shared maps for sslsniff")?;
        }

        let skel = open_skel.load().context("failed to load BPF object")?;

        // SAFETY: skel borrows open_object which lives in a Box<MaybeUninit>
        // on the heap.  We pin both together inside Self and never move either,
        // so the 'static lifetime cast is sound for the lifetime of Self.
        let skel =
            unsafe { Box::from_raw(Box::into_raw(Box::new(skel)) as *mut SslsniffSkel<'static>) };

        let (tx, rx) = crossbeam_channel::unbounded();
        Ok(Self {
            _open_object: open_object,
            skel,
            attachments: HashMap::default(),
            static_attach_offsets: HashMap::default(),
            learned_offsets: LearnedOffsets::load(crate::config::ssl_offsets_learned_path()),
            traced_files: HashSet::default(),
            pid_inodes: HashMap::default(),
            pid_identities: HashMap::default(),
            reattach_ttl: stale_reattach_ttl(),
            stale_reattachs: 0,
            scans: scan_jobs::ScanJobs::new()?,
            scan_retry_after: HashMap::default(),
            pending_pid_admissions: HashMap::default(),
            late_retries: LateRetries::default(),
            last_pid_prune: Instant::now(),
            tx,
            rx,
        })
    }

    /// Attach SSL probes to a running process by reading its `/proc/<pid>/maps`.
    ///
    /// Detects which SSL libraries the process has mapped (OpenSSL, GnuTLS, NSS,
    /// statically-linked SSL — BoringSSL/OpenSSL — or rustls), attaches uprobes,
    /// and skips any library whose `(device, inode)` identity has already been
    /// traced (dedup via `traced_files`) — unless this is the PID's first
    /// attach or the attachment is stale, in which case the probes are
    /// re-attached and the old links are dropped only after the replacement
    /// succeeds.
    pub fn attach_process(&mut self, pid: i32) -> Result<()> {
        self.attach_process_inner(pid, None, false)
    }

    /// Attach an observed exec and retain a bounded retry while it has no probes.
    pub(super) fn attach_observed_process(
        &mut self,
        pid: i32,
        admit: bool,
    ) -> Result<Option<SslAdmission>> {
        let identity = match ProcessIdentity::read(pid) {
            Ok(identity) => identity,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.detach_process(pid as u32);
                return Ok(None);
            }
            Err(error) => {
                log::debug!("cannot establish SSL process identity for pid {pid}: {error}");
                return Ok(None);
            }
        };
        if !self.identity_state_matches(identity) {
            self.detach_process(identity.pid);
        }

        let result = self.attach_process_inner(pid, Some(identity), false);
        let mut admission = None;
        match identity.status() {
            IdentityStatus::Stale => self.detach_process_for(identity),
            IdentityStatus::Unavailable => self.schedule_late_retry(identity, admit),
            IdentityStatus::Current if self.has_process_attachments(identity.pid) => {
                if admit {
                    self.pending_pid_admissions.insert(identity.pid, identity);
                    self.schedule_late_retry(identity, true);
                    if result.is_ok() {
                        admission = Some(SslAdmission { identity });
                    }
                } else {
                    self.clear_admission_state(identity);
                }
            }
            IdentityStatus::Current => self.schedule_late_retry(identity, admit),
        }
        result.map(|_| admission)
    }

    fn schedule_late_retry(&mut self, identity: ProcessIdentity, admit: bool) {
        if !self.late_retries.schedule(identity, admit, Instant::now()) {
            log::debug!(
                "late SSL retry capacity reached; not tracking pid {}",
                identity.pid
            );
        }
    }

    fn attach_process_inner(
        &mut self,
        pid: i32,
        expected: Option<ProcessIdentity>,
        retry: bool,
    ) -> Result<()> {
        if expected.is_some_and(|identity| identity.status() != IdentityStatus::Current) {
            return Ok(());
        }
        let mut libs = ssl_libs_from_maps(pid)?;
        if libs.is_empty() {
            if expected.is_some_and(|identity| identity.status() != IdentityStatus::Current) {
                return Ok(());
            }
            // No SSL library recognizable by name in maps: the process may
            // statically link SSL under an unknown binary name (opencode and
            // other Bun-compiled executables). The bounded worker performs the
            // marker prefilter and byte-pattern scan; the normal Static attach
            // path (symbols → offset table → queued scan) takes over from here.
            let exe = proc_pid_entry(pid, "exe");
            if let Some(entry) =
                probe_exe_static_ssl(pid, &exe, &self.traced_files, &self.attachments)
            {
                libs.push(entry);
            }
        }
        if libs.is_empty() {
            if retry {
                log::debug!("[attach_process] pid={pid}: SSL libraries are not mapped yet");
            } else {
                log::warn!("[attach_process] pid={pid}: no SSL libraries found in maps");
            }
            return Ok(());
        }

        // Debug: print all libs found
        log::debug!(
            "[attach_process] pid={pid}: found {} libs: {:?}",
            libs.len(),
            libs.iter()
                .map(|(p, f, k, m)| (p.as_str(), *f, format!("{k:?}"), m.as_str()))
                .collect::<Vec<_>>()
        );

        let now = Instant::now();
        let first_attach_for_pid = !self.pid_inodes.contains_key(&(pid as u32));
        let mut attached_files: Vec<FileKey> = Vec::new();
        for (path, file, kind, mapped_path) in libs {
            if expected.is_some_and(|identity| identity.status() != IdentityStatus::Current) {
                break;
            }
            // Dedup by (device, inode): with pid=-1 global attach each library
            // only needs to be attached once — the kernel's uprobe_mmap
            // mechanism installs breakpoints for new processes that map an
            // already-registered file, including statically-linked SSL
            // binaries (codex, node, etc). But the kernel can also silently
            // deregister uprobe consumers (observed on serverless/overlayfs
            // hosts), leaving the probes silent with no error. Userspace
            // cannot query liveness, so re-attach for each newly discovered
            // PID and when the attachment expires on later discoveries.
            if !self.traced_files.insert(file) {
                let reattach = match self.attachments.get(&file) {
                    Some(a) => should_reattach(a, now, self.reattach_ttl, first_attach_for_pid),
                    // Marked but never attached: a queued scan or an Untraceable
                    // binary. The tick retry scheduler handles the queued case;
                    // rescanning it on every sweep is wasted work.
                    None => false,
                };
                if !reattach {
                    log::debug!("[attach_process] pid={pid}: skipping already-traced {path}");
                    // Still record the pid→file association so detach_process
                    // can track all pids referencing this file.
                    attached_files.push(file);
                    continue;
                }
                // Build the replacement BEFORE dropping the old links: if the
                // re-attach fails, the previous (possibly still working) probes
                // stay active instead of leaving the library untraced.
                match self.build_attach(pid, file, &path, kind, &mapped_path) {
                    AttachOutcome::Attached(links) => {
                        if first_attach_for_pid {
                            log::debug!(
                                "[attach_process] pid={pid}: re-attached {kind:?} uprobe on {path} for new process"
                            );
                        } else {
                            let age_secs = self
                                .attachments
                                .get(&file)
                                .map(|a| now.duration_since(a.attached_at).as_secs());
                            log::warn!(
                                "[attach_process] pid={pid}: re-attached stale {kind:?} uprobe on {path} ({}s old)",
                                age_secs.unwrap_or(0)
                            );
                        }
                        self.record_attach(file, links);
                        self.scan_retry_after.remove(&file);
                        self.stale_reattachs += 1;
                        attached_files.push(file);
                    }
                    AttachOutcome::ScanQueueFull => {
                        self.defer_scan_retry(file, &path, scan_mode(kind));
                        attached_files.push(file);
                    }
                    AttachOutcome::Deferred => {
                        self.scan_retry_after.remove(&file);
                        attached_files.push(file);
                    }
                    AttachOutcome::Untraceable | AttachOutcome::Failed => {
                        log::warn!(
                            "[attach_process] pid={pid}: re-attach failed for {path}; keeping previous links"
                        );
                        attached_files.push(file);
                    }
                }
                continue;
            }

            match self.build_attach(pid, file, &path, kind, &mapped_path) {
                AttachOutcome::Attached(links) => {
                    self.record_attach(file, links);
                    self.scan_retry_after.remove(&file);
                    attached_files.push(file);
                }
                AttachOutcome::Untraceable => {
                    // Not traceable on this host; leave the file marked so we
                    // do not repeat the (expensive) detection on every sweep.
                    // No attachment means no pending admission to satisfy, but
                    // a retained stale attachment still needs exit ownership.
                    self.forget_owners_unless_attached(file);
                }
                AttachOutcome::Failed => {
                    // Attach failed: drop the file so a later retry can succeed.
                    self.traced_files.remove(&file);
                    self.scan_retry_after.remove(&file);
                    self.static_attach_offsets.remove(&file);
                    self.forget_owners_unless_attached(file);
                }
                AttachOutcome::Deferred => {
                    self.scan_retry_after.remove(&file);
                    attached_files.push(file);
                }
                AttachOutcome::ScanQueueFull => {
                    self.defer_scan_retry(file, &path, scan_mode(kind));
                    attached_files.push(file);
                }
            }
        }

        // Record files attached for this pid so we can clean up on process exit.
        // Merge instead of replacing: a later attach for the same pid must not
        // drop a still-pending scan association created by an earlier sweep.
        if !attached_files.is_empty() {
            if let Some(identity) = expected {
                self.pid_identities.insert(identity.pid, identity);
            }
            let entry = self.pid_inodes.entry(pid as u32).or_default();
            for file in attached_files {
                if !entry.contains(&file) {
                    entry.push(file);
                }
            }
        }

        Ok(())
    }

    /// Reports whether a process has at least one live SSL uprobe attachment.
    pub(super) fn has_process_attachments(&self, pid: u32) -> bool {
        self.pid_inodes
            .get(&pid)
            .is_some_and(|files| files.iter().any(|file| self.attachments.contains_key(file)))
    }

    /// Charge executable scans to the same process budget as capture delivery.
    pub(crate) fn set_cpu_budget(&self, budget: Option<Arc<crate::utils::cpu_budget::CpuBudget>>) {
        self.scans.set_budget(budget);
    }

    /// Install completed scans and claim still-live unclassified pids.
    pub fn poll_pending_scans(&mut self) -> Vec<u32> {
        let admissions = self.poll_pending_scan_admissions();
        for admission in &admissions {
            self.complete_admission(*admission);
        }
        admissions.into_iter().map(SslAdmission::pid).collect()
    }

    /// Install completed scans and return identity-bearing admissions.
    pub(crate) fn poll_pending_scan_admissions(&mut self) -> Vec<SslAdmission> {
        let mut admitted = Vec::new();
        // Drain every completed scan, not a fixed quota: the result channel is
        // bounded and the single scan worker blocks on send once it fills. A
        // per-tick quota let results pile up under process churn, stalling the
        // worker and delaying installs past short-lived agents' exit (#9762).
        // Installing already-computed results is cheap, so drain to empty.
        loop {
            let Some(scanned) = self.scans.try_recv() else {
                break;
            };
            let file = scanned.key;
            let path = scanned.path();
            let has_owner = self.pid_inodes.values().any(|files| files.contains(&file));
            if !has_owner {
                // The originating process exited before its scan was installed.
                // A verified offset set is inode-global, so keep it cached and
                // attach the global uprobe anyway: the next process mapping this
                // binary is then captured without re-racing the scan. Discarding
                // here made every short-lived agent re-scan and lose its first
                // call under load (#9762).
                if let ExecutableScan::Offsets(offsets) = &scanned.outcome {
                    if scanned.unchanged_at_attach() {
                        self.static_attach_offsets
                            .insert(file, (offsets.clone(), false));
                        match attach_static_ssl_by_offset(&mut self.skel, &path, offsets, false, -1)
                        {
                            Ok(links) => {
                                log::debug!(
                                    "Installing static SSL offsets for inode {file:?} after its owner exited"
                                );
                                if self.attachments.contains_key(&file) {
                                    self.stale_reattachs += 1;
                                }
                                self.record_attach(file, links);
                                self.traced_files.insert(file);
                                self.scan_retry_after.remove(&file);
                                // The owner is gone, but the verified offsets
                                // must outlive it: persist them so the next
                                // process mapping this binary — even after a
                                // service restart — attaches synchronously.
                                self.learned_offsets.record(&path, offsets);
                            }
                            Err(error) => {
                                log::warn!(
                                    "Attach asynchronously detected SSL inode {file:?}: {error:#}"
                                );
                                // Unmark for a later retry: a marked file with no
                                // attachment is negatively cached and would never
                                // be probed again.
                                self.traced_files.remove(&file);
                                self.scan_retry_after.remove(&file);
                                if !self.attachments.contains_key(&file) {
                                    self.static_attach_offsets.remove(&file);
                                }
                            }
                        }
                        continue;
                    }
                }
                log::debug!("Discarding scan result for inode {file:?} with no remaining owner");
                self.traced_files.remove(&file);
                self.scan_retry_after.remove(&file);
                // No owner remains to route this through detach_process, so
                // release any retained stale attachment explicitly.
                self.attachments.remove(&file);
                self.static_attach_offsets.remove(&file);
                self.forget_file_for_pids(file);
                continue;
            }
            // The worker's revision check happened before the result was
            // queued. Re-fstat the pinned descriptor immediately before acting
            // on it so an in-place rewrite cannot install stale offsets or
            // cache a stale negative result.
            if !scanned.unchanged_at_attach() {
                self.traced_files.remove(&file);
                self.scan_retry_after.remove(&file);
                self.forget_owners_unless_attached(file);
                log::warn!(
                    "SSL scan input changed before attach; retry discovery for inode {file:?}"
                );
                continue;
            }
            match scanned.outcome {
                ExecutableScan::Offsets(offsets) => {
                    // Offsets belong to this verified inode revision, not to the
                    // process or to a successful uprobe attach.
                    self.static_attach_offsets
                        .insert(file, (offsets.clone(), false));
                    match attach_static_ssl_by_offset(&mut self.skel, &path, &offsets, false, -1) {
                        Ok(links) => {
                            if self.attachments.contains_key(&file) {
                                self.stale_reattachs += 1;
                            }
                            self.record_attach(file, links);
                            self.traced_files.insert(file);
                            self.scan_retry_after.remove(&file);
                            admitted.extend(self.take_ready_pids(file));
                            // Persist after admission so learning never delays
                            // routing the installed offsets to waiting pids.
                            self.learned_offsets.record(&path, &offsets);
                        }
                        Err(error) => {
                            log::warn!(
                                "Attach asynchronously detected SSL inode {file:?}: {error:#}"
                            );
                            self.traced_files.remove(&file);
                            self.scan_retry_after.remove(&file);
                            if !self.attachments.contains_key(&file) {
                                self.static_attach_offsets.remove(&file);
                            }
                            self.forget_owners_unless_attached(file);
                        }
                    }
                }
                ExecutableScan::NoMarkers => {
                    // Negative cache: the marker set is an immutable property of
                    // this file revision. Keep it marked so later discoveries do
                    // not queue the same scan again.
                    self.traced_files.insert(file);
                    self.scan_retry_after.remove(&file);
                    self.forget_owners_unless_attached(file);
                    log::debug!("No SSL markers in {path}; skipping content probe");
                }
                ExecutableScan::NoOffsets => {
                    self.traced_files.insert(file);
                    self.scan_retry_after.remove(&file);
                    self.forget_owners_unless_attached(file);
                    log::warn!("SSL byte-pattern detection failed for {path}; skipping");
                }
                ExecutableScan::Failed => {
                    self.traced_files.remove(&file);
                    self.scan_retry_after.remove(&file);
                    self.forget_owners_unless_attached(file);
                    log::warn!("SSL executable scan failed for {path}; retry on a later discovery");
                }
            }
        }
        self.drain_scan_retries();
        self.prune_exited_pid_inodes();
        admitted
    }

    /// Retry late library discovery and return identity-bearing admissions.
    pub(super) fn poll_late_retries(&mut self) -> Vec<SslAdmission> {
        let batch = self.late_retries.poll(Instant::now());
        let mut admitted = Vec::new();
        for request in batch.expired {
            let identity = request.identity;
            let pid = identity.pid;
            match identity.status() {
                IdentityStatus::Stale => self.detach_process_for(identity),
                IdentityStatus::Unavailable => {}
                // Retry expiry must not cancel an independently bounded static scan.
                IdentityStatus::Current if self.pid_has_pending_scan(pid) => {}
                IdentityStatus::Current if self.has_process_attachments(pid) => {
                    if request.admit {
                        self.pending_pid_admissions.insert(pid, identity);
                        admitted.push(SslAdmission { identity });
                    } else {
                        self.clear_admission_state(identity);
                    }
                }
                IdentityStatus::Current => self.detach_process_for(identity),
            }
        }

        for request in batch.due {
            let identity = request.identity;
            let pid = identity.pid;
            if self.late_retries.identity(pid) != Some(identity) {
                continue;
            }
            let result = self.attach_process_inner(pid as i32, Some(identity), true);
            match identity.status() {
                IdentityStatus::Stale => {
                    self.detach_process_for(identity);
                    continue;
                }
                IdentityStatus::Unavailable => {
                    self.late_retries.reschedule(identity, Instant::now());
                    continue;
                }
                IdentityStatus::Current => {}
            }
            if let Err(error) = result {
                log::debug!("late SSL attach retry failed for pid {pid}: {error:#}");
            }
            if self.has_process_attachments(pid) {
                if request.admit {
                    self.pending_pid_admissions.insert(pid, identity);
                    admitted.push(SslAdmission { identity });
                } else {
                    self.clear_admission_state(identity);
                }
            } else if !self.late_retries.reschedule(identity, Instant::now()) {
                self.detach_process_for(identity);
            }
        }
        admitted
    }

    /// Whether `pid` still maps a file whose scan is queued or awaiting retry.
    pub(super) fn pid_has_pending_scan(&self, pid: u32) -> bool {
        self.pid_inodes.get(&pid).is_some_and(|files| {
            files.iter().any(|file| {
                self.scans.is_pending(*file) || self.scan_retry_after.contains_key(file)
            })
        })
    }

    /// Retain an unclassified pid until its deferred attachment completes.
    pub(super) fn defer_pid_admission(&mut self, pid: u32) {
        let identity = self
            .pid_identities
            .get(&pid)
            .copied()
            .or_else(|| self.late_retries.identity(pid));
        let Some(identity) = identity else {
            return;
        };
        match identity.status() {
            IdentityStatus::Current => {
                self.pending_pid_admissions.insert(pid, identity);
            }
            IdentityStatus::Stale => self.detach_process_for(identity),
            IdentityStatus::Unavailable => {}
        }
    }

    /// Number of SSL library files with live uprobe attachments (diagnostics).
    pub fn traced_inode_count(&self) -> usize {
        self.attachments.len()
    }

    /// How many existing attachments have been replaced by a re-attach
    /// (diagnostics/tests). The name is retained for API compatibility.
    pub fn stale_reattach_count(&self) -> u64 {
        self.stale_reattachs
    }

    /// Record a successful attach for `file`, replacing any prior entry
    /// (whose links are dropped, detaching the old probes).
    fn record_attach(&mut self, file: FileKey, links: Vec<Link>) {
        self.attachments.insert(
            file,
            InodeAttach {
                _links: links,
                attached_at: Instant::now(),
            },
        );
    }

    /// Drop `file` from every pid's association list.
    ///
    /// Used once a scan result can no longer produce an attachment (successful
    /// install, Untraceable, failure, or changed input), so `pid_inodes` does
    /// not keep dead references for pids that will never be registered.
    fn forget_file_for_pids(&mut self, file: FileKey) {
        forget_file_associations(&mut self.pid_inodes, &mut self.pending_pid_admissions, file);
    }

    /// Drop file ownership only when no retained attachment needs an exit path.
    ///
    /// A stale byte-pattern reattach keeps its old links while the replacement
    /// is queued. If the replacement fails, those links must stay owned until
    /// the last mapping process exits so `detach_process` can release them;
    /// removing every association here would orphan the global uprobe.
    fn forget_owners_unless_attached(&mut self, file: FileKey) {
        forget_owners_after_failed_result(
            self.attachments.contains_key(&file),
            &mut self.pid_inodes,
            &mut self.pending_pid_admissions,
            file,
        );
    }

    /// Return admissions whose original process identity is still current.
    fn take_ready_pids(&mut self, file: FileKey) -> Vec<SslAdmission> {
        let (ready, stale) = ready_pids_for_file(
            &self.pid_inodes,
            &self.pending_pid_admissions,
            file,
            ProcessIdentity::status,
        );
        for identity in stale {
            self.detach_process_for(identity);
        }
        ready
            .into_iter()
            .map(|identity| SslAdmission { identity })
            .collect()
    }

    pub(super) fn retry_admission(&mut self, admission: SslAdmission) -> bool {
        self.identity_state_matches(admission.identity)
            && self
                .late_retries
                .reschedule(admission.identity, Instant::now())
    }

    pub(super) fn complete_admission(&mut self, admission: SslAdmission) {
        self.clear_admission_state(admission.identity);
    }

    pub(super) fn detach_stale_admission(&mut self, admission: SslAdmission) {
        self.detach_process_for(admission.identity);
    }

    fn clear_admission_state(&mut self, identity: ProcessIdentity) {
        if self.pending_pid_admissions.get(&identity.pid) == Some(&identity) {
            self.pending_pid_admissions.remove(&identity.pid);
        }
        self.late_retries.remove_if(identity);
    }

    fn identity_state_matches(&self, identity: ProcessIdentity) -> bool {
        stored_identity_matches(
            identity,
            self.pid_identities.get(&identity.pid).copied(),
            self.pending_pid_admissions.get(&identity.pid).copied(),
            self.late_retries.identity(identity.pid),
        )
    }

    fn detach_process_for(&mut self, identity: ProcessIdentity) {
        if self.identity_state_matches(identity) {
            self.detach_process(identity.pid);
        }
    }

    /// Reap `pid_inodes` entries whose process exited without a BPF exit event
    /// because it was never registered in `traced_processes`.
    fn prune_exited_pid_inodes(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.last_pid_prune) < Duration::from_secs(1) {
            return;
        }
        self.last_pid_prune = now;
        if !proc_pid_entry(1, "").is_dir() {
            return;
        }
        let dead: Vec<u32> = self
            .pid_inodes
            .keys()
            .copied()
            .filter(|pid| pid_definitely_exited(*pid))
            .collect();
        for pid in dead {
            self.detach_process(pid);
        }
    }

    fn defer_scan_retry(&mut self, file: FileKey, path: &str, mode: ScanMode) {
        let now = Instant::now();
        let should_warn = self
            .scan_retry_after
            .get(&file)
            .is_none_or(|retry| retry.deadline <= now);
        if should_warn {
            log::warn!("SSL scan queue is full; deferring byte-pattern detection for {path}");
        }
        if !self.scan_retry_after.contains_key(&file)
            && self.scan_retry_after.len() >= MAX_SCAN_RETRIES
        {
            if let Some(oldest) = self
                .scan_retry_after
                .iter()
                .min_by_key(|(_, retry)| retry.deadline)
                .map(|(file, _)| *file)
            {
                self.scan_retry_after.remove(&oldest);
                self.traced_files.remove(&oldest);
                self.forget_owners_unless_attached(oldest);
                log::warn!("SSL scan retry queue full; dropped retry for inode {oldest:?}");
            }
        }
        self.scan_retry_after.insert(
            file,
            ScanRetry {
                deadline: now + Duration::from_secs(1),
                path: path.to_owned(),
                mode,
            },
        );
    }

    /// Submit due queue-full retries without waiting for another discovery.
    ///
    /// Runs from the runtime tick after completed results have been drained, so
    /// a file that overflowed the queue is eventually scanned even when no
    /// later attach sweep observes it.
    fn drain_scan_retries(&mut self) {
        if !self.scans.has_capacity() {
            return;
        }
        let now = Instant::now();
        let due: Vec<(FileKey, String, ScanMode)> = due_retries(now, &self.scan_retry_after);
        for (file, path, mode) in due {
            if !self.scans.has_capacity() {
                break;
            }
            if !self.traced_files.contains(&file) {
                self.scan_retry_after.remove(&file);
                continue;
            }
            if !self.pid_inodes.values().any(|files| files.contains(&file)) {
                self.traced_files.remove(&file);
                self.scan_retry_after.remove(&file);
                self.attachments.remove(&file);
                continue;
            }
            match self.scans.submit(&path, file, mode) {
                Ok(()) => {
                    self.scan_retry_after.remove(&file);
                }
                Err(SubmitError::QueueFull) => {}
                Err(SubmitError::Replaced) => {
                    self.traced_files.remove(&file);
                    self.scan_retry_after.remove(&file);
                    self.forget_owners_unless_attached(file);
                    log::warn!(
                        "SSL scan input for inode {file:?} was replaced; retry on a later discovery"
                    );
                }
                Err(SubmitError::Failed(error)) => {
                    self.traced_files.remove(&file);
                    self.scan_retry_after.remove(&file);
                    self.forget_owners_unless_attached(file);
                    log::warn!("Schedule deferred SSL scan for {path}: {error:#}");
                }
            }
        }
    }

    /// Attach uprobes for a single SSL library.
    ///
    /// `mapped_path` is the pathname the process actually mapped, which is what
    /// operator-facing diagnostics must key off: the attach `path` is often a
    /// generic `<pid>/exe` that carries no library identity.
    ///
    /// Does not touch `traced_files` or `attachments`; the caller decides how
    /// to record or retry the outcome.
    fn build_attach(
        &mut self,
        pid: i32,
        file: FileKey,
        path: &str,
        kind: SslLibKind,
        mapped_path: &str,
    ) -> AttachOutcome {
        log::debug!("[attach_process] pid={pid}: attaching {kind:?} → {path}");

        // Use pid=-1 for global attach (all processes), avoiding per-process duplicate attaches
        let result = match kind {
            SslLibKind::OpenSsl => attach_openssl(&mut self.skel, path, -1),
            SslLibKind::Python => {
                return attach_python_ssl(pid, path, || attach_openssl(&mut self.skel, path, -1));
            }
            SslLibKind::GnuTls => attach_gnutls(&mut self.skel, path, -1),
            SslLibKind::Nss => attach_nss(&mut self.skel, path, -1),
            SslLibKind::ExplicitTap => {
                // A successful attach proves the symbol is exported; avoid
                // scanning the whole binary again for each new process.
                if !self.attachments.contains_key(&file) && !cosh_tap_present(path) {
                    // A pre-tap cosh-ng can be deployed alongside an upgraded
                    // AgentSight, and then its LLM calls are simply not
                    // capturable: there is no TLS-layer API to fall back to.
                    // Say so once per file instead of staying silent, which
                    // would be indistinguishable from a probe bug. The
                    // decision uses the mapped pathname: `path` is the
                    // `<pid>/exe` symlink for every ExplicitTap binary, so it
                    // can never identify cosh-core.
                    if missing_tap_is_a_coverage_gap(mapped_path) {
                        log::warn!(
                            "[attach_process] pid={pid}: {path} exports no {COSH_TAP_SYMBOL}; \
                             this cosh-ng predates the plaintext tap, so its LLM traffic \
                             cannot be captured -- upgrade cosh-ng to restore coverage"
                        );
                    } else {
                        log::debug!(
                            "[attach_process] pid={pid}: {path} exports no {COSH_TAP_SYMBOL}; \
                             only the LLM-issuing binary carries the tap"
                        );
                    }
                    return AttachOutcome::Untraceable;
                }
                attach_cosh_tap(&mut self.skel, path, -1)
            }
            SslLibKind::Static | SslLibKind::StaticExeProbe => {
                if let Some((off, handshake)) = self.static_attach_offsets.get(&file).cloned() {
                    attach_static_ssl_by_offset(&mut self.skel, path, &off, handshake, -1)
                } else {
                    match attach_static_ssl_by_symbol(&mut self.skel, path, -1) {
                        Ok(ls) => Ok(ls),
                        Err(sym_err) => {
                            log::debug!(
                                "[attach_process] pid={pid}: Static SSL symbol attach failed for {path} ({sym_err:#}), falling back to the offset table"
                            );
                            // Consult the offset table before the byte-pattern scan.
                            // A lookup costs a stat() plus, only when the size already
                            // matches a known build, a 64 KiB head read; the scan reads
                            // the whole binary. Probing in the opposite order charged
                            // every unsupported agent binary its own size in resident
                            // memory before the table could answer, which exhausted the
                            // service memory cap during startup attach on hosts running
                            // several large harnesses (#2981).
                            if let Some(off) = CODEX_OFFSET_TABLE
                                .as_ref()
                                .and_then(|table| table.lookup(path))
                            {
                                log::info!(
                                    "[attach_process] pid={pid}: codex offset table matched for {path} \
                                     (write=0x{:x}, read=0x{:x}, handshake=0x{:x})",
                                    off.ssl_write,
                                    off.ssl_read,
                                    off.ssl_do_handshake
                                );
                                self.static_attach_offsets.insert(file, (off.clone(), true));
                                return match attach_static_ssl_by_offset(
                                    &mut self.skel,
                                    path,
                                    &off,
                                    true,
                                    -1,
                                ) {
                                    Ok(links) => AttachOutcome::Attached(links),
                                    Err(e) => {
                                        log::warn!(
                                            "[attach_process] pid={pid}: attach failed for {path}: {e:#}"
                                        );
                                        AttachOutcome::Failed
                                    }
                                };
                            }
                            // A previously learned offset set attaches
                            // synchronously, exactly like the shipped table: a
                            // warm agent's first TLS call can precede the
                            // asynchronous scan of its binary (#9762). Learned
                            // entries come from scans, so they attach with the
                            // scan-install ABI (no handshake probes), not the
                            // codex-table one.
                            if let Some(off) = self.learned_offsets.lookup(path) {
                                log::info!(
                                    "[attach_process] pid={pid}: learned offset cache matched for {path} \
                                     (write=0x{:x}, read=0x{:x}, handshake=0x{:x})",
                                    off.ssl_write,
                                    off.ssl_read,
                                    off.ssl_do_handshake
                                );
                                self.static_attach_offsets
                                    .insert(file, (off.clone(), false));
                                return match attach_static_ssl_by_offset(
                                    &mut self.skel,
                                    path,
                                    &off,
                                    false,
                                    -1,
                                ) {
                                    Ok(links) => AttachOutcome::Attached(links),
                                    Err(e) => {
                                        log::warn!(
                                            "[attach_process] pid={pid}: attach failed for {path}: {e:#}"
                                        );
                                        AttachOutcome::Failed
                                    }
                                };
                            }
                            return match self.scans.submit(path, file, scan_mode(kind)) {
                                Ok(()) => AttachOutcome::Deferred,
                                Err(SubmitError::QueueFull) => AttachOutcome::ScanQueueFull,
                                Err(SubmitError::Replaced) => {
                                    log::warn!(
                                        "SSL scan input for pid={pid}, {path} changed before submission; retry discovery"
                                    );
                                    AttachOutcome::Failed
                                }
                                Err(SubmitError::Failed(error)) => {
                                    log::warn!(
                                        "Schedule SSL byte-pattern detection for pid={pid}, {path}: {error:#}"
                                    );
                                    AttachOutcome::Failed
                                }
                            };
                        }
                    }
                }
            }
        };

        finish_ssl_attach(pid, path, result)
    }

    /// Detach SSL probes for a process and clean up traced files.
    ///
    /// When a process exits, its `(device, inode)` keys are removed from
    /// `traced_files` (and their `attachments` entries dropped, detaching the
    /// global uprobes) **only if no other traced pid still references the same
    /// file**. Uprobes are attached globally (`pid=-1`), so the link remains
    /// valid for other processes using the same library; removing the entry
    /// prematurely would cause the scanner to re-attach on the next sweep,
    /// producing duplicate uprobe fds.
    pub fn detach_process(&mut self, pid: u32) {
        self.pending_pid_admissions.remove(&pid);
        self.late_retries.remove(pid);
        self.pid_identities.remove(&pid);
        if let Some(files) = self.pid_inodes.remove(&pid) {
            let mut removed = 0;
            for file in &files {
                // Check whether another pid still maps this file.
                let still_used = self
                    .pid_inodes
                    .values()
                    .flatten()
                    .any(|other_file| other_file == file);
                if !still_used {
                    self.traced_files.remove(file);
                    self.scan_retry_after.remove(file);
                    // Drop the attachment too: releasing the Links detaches the
                    // (now unneeded) global uprobes and keeps `attachments`
                    // consistent with `traced_files`. A later process mapping
                    // this file re-attaches through the fresh-attach path.
                    self.attachments.remove(file);
                    self.static_attach_offsets.remove(file);
                    removed += 1;
                }
            }
            log::debug!(
                "[detach_process] pid={pid}: removed {}/{} files from traced_files",
                removed,
                files.len()
            );
        }
    }

    /// Spawn a background thread that polls the BPF ring buffer and sends
    /// decoded [`SslEvent`]s through an internal channel.
    ///
    /// Returns a [`SslPoller`] handle.  Drop it (or call [`SslPoller::stop`])
    /// to signal the poll thread to exit.
    pub fn run(&self) -> Result<SslPoller> {
        config::initialize_event_clock().context("failed to initialize event clock")?;
        let tx = self.tx.clone();
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_inner = Arc::clone(&stop_flag);

        // We need the ring-buffer map fd, which is owned by `self.skel`.
        // Build the RingBuffer here (on the calling thread) then move it
        // into the poll thread.
        let mut rb_builder = RingBufferBuilder::new();
        let binding = self.skel.maps();
        rb_builder
            .add(binding.rb(), move |data: &[u8]| {
                // SSL records are variable-length (tiered reservation): decode by
                // header prefix + buf_size, not a full-struct cast.
                if let Some(event) = SslEvent::from_bytes(data) {
                    let _ = tx.send(event);
                }
                0
            })
            .context("failed to add ring buffer")?;
        let rb = rb_builder.build().context("failed to build ring buffer")?;

        // Read the configured poll timeout on this thread before spawning so
        // the poller observes exactly the value `AgentSight::new` published
        // (crate::config::set_poll_timeout_ms), not whatever the global holds
        // when the thread happens to get scheduled.
        let poll_timeout = Duration::from_millis(crate::config::poll_timeout_ms());

        let handle = thread::Builder::new()
            .name("sslsniff-poll".into())
            .spawn(move || {
                let timeout = poll_timeout;
                let outcome = super::drive_poll_loop(timeout, &stop_flag_inner, |timeout| {
                    rb.poll(timeout)
                        .map(|_| ())
                        .map_err(|error| match error.kind() {
                            libbpf_rs::ErrorKind::Interrupted => super::PollFailure::Interrupted,
                            _ => super::PollFailure::Fatal(format!("{error:#}")),
                        })
                });
                if let super::PollEnd::Failed(message) = outcome {
                    eprintln!("sslsniff poll error: {message}");
                }
            })
            .context("failed to spawn poll thread")?;

        Ok(SslPoller {
            handle: Some(handle),
            stop_flag,
        })
    }

    /// Receive the next [`SslEvent`] from the background poll thread.
    ///
    /// Blocks until an event arrives or the sender is disconnected.
    pub fn recv(&self) -> Option<SslEvent> {
        self.rx.recv().ok()
    }

    /// Non-blocking variant of [`recv`](Self::recv).
    pub fn try_recv(&self) -> Option<SslEvent> {
        self.rx.try_recv().ok()
    }
}

// ─── Poll thread handle ─────────────────────────────────────────────────────

/// Handle returned by [`SslSniff::run`].
///
/// The background poll thread runs until this handle is dropped or
/// [`SslPoller::stop`] is called explicitly.
pub struct SslPoller {
    handle: Option<thread::JoinHandle<()>>,
    stop_flag: Arc<AtomicBool>,
}

impl SslPoller {
    /// Signal the poll thread to stop and wait for it to finish.
    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for SslPoller {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ─── Static SSL pattern detection ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub(super) struct StaticSslOffsets {
    pub ssl_write: usize,
    pub ssl_read: usize,
    pub ssl_do_handshake: usize,
    /// True when `ssl_write` points to `SSL_write_ex` (returns 0/1 + *written),
    /// rather than `SSL_write` (returns byte count). Required for OpenSSL 3.x
    /// where the `_ex` variant is preferred by native-tls / openssl-sys.
    pub write_is_ex: bool,
    /// True when `ssl_read` points to `SSL_read_ex` rather than `SSL_read`.
    pub read_is_ex: bool,
}

/// Bytes read per window by [`scan_file_patterns`].
///
/// Caps the scan's peak memory. Agent binaries reach hundreds of megabytes (a
/// Codex build is ~264 MiB, node ~120 MiB), more than the whole budget the
/// service runs under, so the image must never be held in one piece (#2981).
pub(super) const SCAN_CHUNK_BYTES: usize = 4 * 1024 * 1024;

/// Absolute offsets of every occurrence of each pattern in the file at `path`.
///
/// Returns one vector per entry of `patterns`, in the same order and each
/// ascending. The file is streamed in [`SCAN_CHUNK_BYTES`] windows overlapping
/// by `max_pattern_len - 1` bytes, so a match straddling a window boundary is
/// still reported, and reported only once.
///
/// # Errors
///
/// Returns `None` when `patterns` is empty, or the file cannot be opened or read.
pub(super) fn scan_file_patterns(path: &str, patterns: &[&[u8]]) -> Option<Vec<Vec<usize>>> {
    scan_file_patterns_admitted(path, patterns, || Some(()))
}

fn scan_file_patterns_admitted<G>(
    path: &str,
    patterns: &[&[u8]],
    mut admit: impl FnMut() -> Option<G>,
) -> Option<Vec<Vec<usize>>> {
    let overlap = patterns.iter().map(|p| p.len()).max()?.checked_sub(1)?;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; overlap + SCAN_CHUNK_BYTES];
    let mut hits = vec![Vec::new(); patterns.len()];
    // Absolute offset that `buf[0]` maps to, and how many leading bytes of `buf`
    // are the previous window's carried-over tail.
    let mut base = 0usize;
    let mut carry = 0usize;
    loop {
        let _permit = admit()?;
        let read = fill_buf(&mut file, &mut buf[carry..])?;
        let filled = carry + read;
        if filled == 0 {
            break;
        }
        // A match beginning inside the trailing `overlap` bytes may run past this
        // window, so defer it to the next round; on the final window nothing is
        // left to wait for, so every start counts.
        let last = filled < buf.len();
        let start_limit = if last { filled } else { filled - overlap };
        collect_pattern_hits(&buf[..filled], patterns, base, start_limit, &mut hits);
        if last {
            break;
        }
        buf.copy_within(filled - overlap..filled, 0);
        base += filled - overlap;
        carry = overlap;
    }
    Some(hits)
}

#[cfg(test)]
pub(super) fn find_static_ssl_offsets(path: &str) -> Option<StaticSslOffsets> {
    match find_static_ssl_offsets_with(path, scan_file_patterns) {
        StaticSslScan::Offsets(offsets) => Some(offsets),
        StaticSslScan::NoOffsets | StaticSslScan::Failed => None,
    }
}

/// Outcome of one bounded executable inspection.
#[derive(Debug)]
pub(super) enum ExecutableScan {
    /// No static-SSL marker exists in this file revision.
    NoMarkers,
    /// Markers exist but no complete signature set was found.
    NoOffsets,
    /// The complete signature set was resolved.
    Offsets(StaticSslOffsets),
    /// The file could not be read; retry on a later discovery.
    Failed,
}

enum StaticSslScan {
    Offsets(StaticSslOffsets),
    NoOffsets,
    Failed,
}

/// Admit one 4 MiB executable window against the shared process CPU budget.
fn admit_probe<'a>(
    path: &str,
    budget: Option<&'a CpuBudget>,
    stop: &AtomicBool,
) -> Option<Option<CpuPermit<'a>>> {
    if stop.load(Ordering::Relaxed) {
        return None;
    }
    match budget {
        None => Some(None),
        Some(budget) => {
            match budget.acquire_while(WorkClass::Probe, || !stop.load(Ordering::Relaxed)) {
                Ok(Some(permit)) => Some(Some(permit)),
                Ok(None) => None,
                Err(error) => {
                    log::error!("SSL scan CPU admission failed for {path}: {error}");
                    None
                }
            }
        }
    }
}

/// Run the marker prefilter and byte-pattern scan on the scan worker.
///
/// Returns `None` only when shutdown cancelled the scan; caller errors are
/// reported as [`ExecutableScan::Failed`] so a later discovery can retry.
pub(super) fn scan_static_ssl_executable_paced(
    path: &str,
    budget: Option<&CpuBudget>,
    stop: &AtomicBool,
    mode: ScanMode,
) -> Option<ExecutableScan> {
    // The BoringSSL marker is a heuristic for unknown exe-probe candidates.
    // Known static inputs (for example static OpenSSL/Codex) may legitimately
    // lack it and must still reach the byte-pattern scan.
    if mode == ScanMode::BoringSslCandidate {
        let markers =
            match scan::file_has_ssl_markers_admitted(path, || admit_probe(path, budget, stop)) {
                Some(markers) => markers,
                None if stop.load(Ordering::Relaxed) => return None,
                None => return Some(ExecutableScan::Failed),
            };
        if !markers {
            return Some(ExecutableScan::NoMarkers);
        }
    }
    let result = find_static_ssl_offsets_with(path, |path, patterns| {
        scan_file_patterns_admitted(path, patterns, || admit_probe(path, budget, stop))
    });
    match result {
        StaticSslScan::Offsets(offsets) => Some(ExecutableScan::Offsets(offsets)),
        StaticSslScan::NoOffsets => Some(ExecutableScan::NoOffsets),
        StaticSslScan::Failed if stop.load(Ordering::Relaxed) => None,
        StaticSslScan::Failed => Some(ExecutableScan::Failed),
    }
}

/// Maximum distance between SSL_read and SSL_write in the same compilation unit.
const ADJACENCY_THRESHOLD: usize = 0x1000; // 4KB

/// Resolve the Bun 2026 BoringSSL generation (Claude Code >= 2.1.315, #9762).
///
/// That rebuild re-emitted the `SSL_read` / `SSL_do_handshake` prologues and
/// reordered `ssl_lib.cc` so `SSL_write` now *precedes* `SSL_read`. The legacy
/// path assumes the old prologues and a write-after-read layout, so it cannot
/// see this generation. Here the prologues are unique per build and `SSL_write`
/// may sit on either side of `SSL_read` within [`ADJACENCY_THRESHOLD`]; the
/// handshake follows the read, so the nearest match wins.
fn resolve_bun2_ssl_offsets(
    path: &str,
    read_matches: &[usize],
    handshake_matches: &[usize],
    write_matches: &[usize],
    verbose: bool,
) -> StaticSslScan {
    let [read_off] = read_matches else {
        if verbose {
            eprintln!(
                "Static SSL: Bun2 SSL_read pattern has {} matches, expected 1 in {path}",
                read_matches.len()
            );
        }
        return StaticSslScan::NoOffsets;
    };
    let read_off = *read_off;
    let Some(&hs_off) = handshake_matches
        .iter()
        .min_by_key(|&&o| o.abs_diff(read_off))
    else {
        if verbose {
            eprintln!("Static SSL: Bun2 SSL_do_handshake pattern not found in {path}");
        }
        return StaticSslScan::NoOffsets;
    };
    let Some(&wr_off) = write_matches
        .iter()
        .filter(|&&o| o.abs_diff(read_off) < ADJACENCY_THRESHOLD)
        .min_by_key(|&&o| o.abs_diff(read_off))
    else {
        if verbose {
            eprintln!(
                "Static SSL: Bun2 SSL_write has {} matches but none within {}B of SSL_read ({read_off:#x})",
                write_matches.len(),
                ADJACENCY_THRESHOLD
            );
        }
        return StaticSslScan::NoOffsets;
    };
    log::debug!("Static SSL (Bun2) detected in {path}:");
    log::debug!("  SSL_do_handshake: {hs_off:#x}");
    log::debug!("  SSL_read:         {read_off:#x}");
    log::debug!("  SSL_write:        {wr_off:#x}");
    StaticSslScan::Offsets(StaticSslOffsets {
        ssl_write: wr_off,
        ssl_read: read_off,
        ssl_do_handshake: hs_off,
        write_is_ex: false,
        read_is_ex: false,
    })
}

fn find_static_ssl_offsets_with(
    path: &str,
    mut scan: impl FnMut(&str, &[&[u8]]) -> Option<Vec<Vec<usize>>>,
) -> StaticSslScan {
    // SSL function prologue byte patterns (x86_64), stable across versions
    // because they encode the fixed parameter-saving and state-setup logic of
    // the POSIX SSL API. SSL_do_handshake needs one pattern per toolchain
    // flavor because register allocation differs:
    // - clang builds (Node.js, Chrome): spills the SSL* into r12.
    // - Bun single-file executables (Claude Code >= 2.1.113): loads ssl->s3
    //   into r15 and inlines the `initial_handshake_done` early-return check.
    const HANDSHAKE_PATS: &[&[u8]] = &[
        &[
            0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x53, 0x48,
            0x83, 0xec, 0x28, 0x49, 0x89, 0xfc, 0x48, 0x8b, 0x47, 0x30,
        ],
        &[
            0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x53, 0x50, 0x4c, 0x8b, 0x7f, 0x30,
            0x49, 0x83, 0xbf, 0x18, 0x01, 0x00, 0x00, 0x00, 0x74,
        ],
    ];
    const READ_PAT: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x53, 0x50, 0x48, 0x83, 0xbf, 0x98, 0x00,
        0x00, 0x00, 0x00, 0x74,
    ];
    const WRITE_PAT: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x53, 0x48, 0x83,
        0xec, 0x18, 0x41, 0x89, 0xd7, 0x49, 0x89, 0xf6, 0x48, 0x89, 0xfb,
    ];
    // Bun's 2026 BoringSSL rebuild (Claude Code >= 2.1.315): SSL_read saves r12
    // instead of rax and branches with a near jne; SSL_do_handshake shrinks to a
    // two-register save. SSL_write keeps WRITE_PAT. Verified unique per build and
    // absent from every earlier generation (#9762).
    const READ_PAT_BUN2: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x54, 0x53, 0x48, 0x83, 0xbf, 0x98,
        0x00, 0x00, 0x00, 0x00, 0x0f, 0x85,
    ];
    const HANDSHAKE_PAT_BUN2: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x56, 0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x47, 0x30, 0xc7,
        0x80, 0xc4, 0x00, 0x00, 0x00, 0x00,
    ];
    let verbose = config::verbose();

    // One streaming pass collects every pattern at once: the file is read in
    // bounded windows, so a hundreds-of-megabytes binary costs a few MiB here
    // instead of its own size.
    let mut patterns: Vec<&[u8]> = vec![READ_PAT, WRITE_PAT];
    patterns.extend_from_slice(HANDSHAKE_PATS);
    patterns.push(READ_PAT_BUN2);
    patterns.push(HANDSHAKE_PAT_BUN2);
    let mut hits = match scan(path, &patterns) {
        Some(hits) => hits,
        None => return StaticSslScan::Failed,
    };
    // Drain the fixed slots high-index-first so the remaining vectors are
    // exactly the legacy handshake variants.
    let handshake_bun2 = hits.remove(5);
    let read_bun2 = hits.remove(4);
    let write_matches = hits.remove(1);
    let read_matches = hits.remove(0);
    let handshake_hits = hits;

    // --- SSL_read: expect unique match ---
    if read_matches.is_empty() {
        // No legacy SSL_read prologue: try the Bun 2026 generation before giving
        // up, so a reordered ssl_lib.cc still resolves (#9762).
        return resolve_bun2_ssl_offsets(
            path,
            &read_bun2,
            &handshake_bun2,
            &write_matches,
            verbose,
        );
    }
    let read_off = if read_matches.len() == 1 {
        read_matches[0]
    } else {
        if verbose {
            eprintln!(
                "Static SSL: SSL_read pattern has {} matches, expected 1",
                read_matches.len()
            );
        }
        return StaticSslScan::NoOffsets;
    };

    // --- SSL_do_handshake: collect matches across all known variants ---
    let mut hs_matches: Vec<usize> = handshake_hits.into_iter().flatten().collect();
    if hs_matches.is_empty() {
        if verbose {
            eprintln!("Static SSL: SSL_do_handshake pattern not found in {path}");
        }
        return StaticSslScan::NoOffsets;
    }
    hs_matches.sort_unstable();
    hs_matches.dedup();
    // Pick the match closest to (and before) SSL_read.
    let hs_off = if hs_matches.len() == 1 {
        hs_matches[0]
    } else {
        // Multiple matches: choose the one closest before read_off.
        match hs_matches.iter().rfind(|&&o| o < read_off) {
            Some(&o) => o,
            None => {
                if verbose {
                    eprintln!(
                        "Static SSL: SSL_do_handshake has {} matches, none before SSL_read",
                        hs_matches.len()
                    );
                }
                return StaticSslScan::NoOffsets;
            }
        }
    };

    // --- SSL_write: adjacency verification ---
    if write_matches.is_empty() {
        if verbose {
            eprintln!("Static SSL: SSL_write pattern not found in {path}");
        }
        return StaticSslScan::NoOffsets;
    }
    // Pick the first match after SSL_read within ADJACENCY_THRESHOLD.
    let wr_off = write_matches
        .iter()
        .filter(|&&o| o > read_off && o - read_off < ADJACENCY_THRESHOLD)
        .copied()
        .next()
        .or_else(|| {
            if verbose {
                eprintln!(
                    "Static SSL: SSL_write has {} matches but none within {}B after SSL_read ({:#x})",
                    write_matches.len(),
                    ADJACENCY_THRESHOLD,
                    read_off
                );
            }
            None
        });
    let Some(wr_off) = wr_off else {
        return StaticSslScan::NoOffsets;
    };

    log::debug!("Static SSL detected in {path}:");
    log::debug!("  SSL_do_handshake: {hs_off:#x}");
    log::debug!("  SSL_read:         {read_off:#x}");
    log::debug!("  SSL_write:        {wr_off:#x}");

    StaticSslScan::Offsets(StaticSslOffsets {
        ssl_write: wr_off,
        ssl_read: read_off,
        ssl_do_handshake: hs_off,
        write_is_ex: false,
        read_is_ex: false,
    })
}

/// Symbol cosh-ng exports as its plaintext observability attach point.
///
/// Defined by `cosh-core`'s `provider::observe` module. Resolving it by name is
/// what makes this robust: an earlier attempt located rustls' own plaintext
/// functions by byte pattern, which broke as soon as the release build moved to
/// a different rustc (#3115), because those patterns encode register allocation.
/// A symbol address is resolved by libbpf at attach time, so the same name keeps
/// working across toolchains.
pub(super) const COSH_TAP_SYMBOL: &str = "cosh_llm_plaintext_tap";

/// Whether `path` exports the plaintext tap at all.
///
/// Only `cosh-core` issues LLM requests; `cosh-shell`, `cosh-cli` and
/// `cosh-gateway` match the same name rule but carry no tap. Attaching to them
/// would fail on every discovery sweep and be retried forever, so they are
/// recognised as legitimately untraceable instead.
///
/// The check looks for the symbol name in the file rather than parsing
/// `.dynsym`: the streaming scan is bounded and already available, and a name
/// that appears nowhere in the binary certainly is not in its symbol table. A
/// false positive merely lets the attach proceed and report the real error.
pub(super) fn cosh_tap_present(path: &str) -> bool {
    scan_file_patterns(path, &[COSH_TAP_SYMBOL.as_bytes()])
        .is_some_and(|hits| hits.first().is_some_and(|h| !h.is_empty()))
}

/// Probe the main executable of `pid` for statically-linked SSL when maps
/// named no known SSL library.
///
/// Returns a synthetic `StaticExeProbe` lib entry so the bounded worker can run
/// the marker prefilter and byte-pattern scan away from the event consumer. An
/// already attached executable remains a candidate for the normal re-attach
/// policy; queued scans and Untraceable-marked files stay negatively cached.
/// Returns `None` when the exe is unreadable, is a Python interpreter, or is
/// already present in `traced` without an attachment.
fn probe_exe_static_ssl(
    pid: i32,
    exe: &Path,
    traced: &HashSet<FileKey>,
    attachments: &HashMap<FileKey, InodeAttach>,
) -> Option<(String, FileKey, SslLibKind, String)> {
    // Never treat Python interpreters as statically-linked BoringSSL via exe
    // content probing: Python stubs/binaries may accidentally contain marker
    // strings and must go through the normal OpenSSL/uv-python paths instead.
    if let Ok(link) = fs::read_link(exe) {
        let name = Path::new(&link).file_name()?.to_string_lossy();
        if name.starts_with("python") {
            return None;
        }
    }
    let meta = fs::metadata(exe).ok()?;
    use std::os::unix::fs::MetadataExt;
    let key = (stat_dev_to_key(meta.dev()), meta.ino());
    // An attached executable must reach the re-attach decision for each new
    // PID; all other marked files either have a queued scan or are Untraceable.
    if traced.contains(&key) && !attachments.contains_key(&key) {
        return None;
    }
    let exe_str = exe.to_string_lossy().into_owned();
    log::debug!("[attach_process] pid={pid}: probing main executable for static SSL: {exe_str}");
    // Prefer the resolved on-disk path over `<pid>/exe` as the uprobe
    // target: detection can take seconds (byte-pattern scan), and a
    // short-lived process (a one-shot `opencode run`) exits before the
    // attach completes, taking `<pid>/exe` down with it and failing the
    // uprobe. The resolved path survives the exit, and the probe is
    // inode-global anyway. Fall back to `<pid>/exe` when readlink fails or
    // the file was replaced underneath (inode no longer matches).
    let attach_path = match fs::read_link(exe) {
        Ok(target) => {
            let resolved = target.to_string_lossy().into_owned();
            match fs::metadata(&resolved) {
                Ok(m) if (stat_dev_to_key(m.dev()), m.ino()) == key => resolved,
                _ => exe_str.clone(),
            }
        }
        Err(_) => exe_str.clone(),
    };
    Some((attach_path, key, SslLibKind::StaticExeProbe, exe_str))
}

/// Translate a `stat(2)` `st_dev` into the `FileKey` device encoding used by
/// [`parse_maps_dev`] (major in the high 32 bits), so the same binary keys
/// identically whether discovered through maps or through metadata.
fn stat_dev_to_key(dev: u64) -> u64 {
    // glibc major()/minor() unpacking of a dev_t.
    let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff_u64);
    let minor = (dev & 0xff) | ((dev >> 12) & !0xff_u64);
    (major << 32) | minor
}

/// Select retries whose deadline elapsed, copying the path for submission.
fn due_retries(
    now: Instant,
    retries: &HashMap<FileKey, ScanRetry>,
) -> Vec<(FileKey, String, ScanMode)> {
    retries
        .iter()
        .filter(|(_, retry)| retry.deadline <= now)
        .map(|(file, retry)| (*file, retry.path.clone(), retry.mode))
        .collect()
}

/// True only when procfs proves the process is gone.
///
/// Permission and I/O errors do not prove exit, matching the traced-filter
/// reaper's policy.
fn pid_definitely_exited(pid: u32) -> bool {
    matches!(
        fs::metadata(proc_pid_entry(pid, "")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

fn stored_identity_matches(
    identity: ProcessIdentity,
    owner: Option<ProcessIdentity>,
    pending: Option<ProcessIdentity>,
    retry: Option<ProcessIdentity>,
) -> bool {
    [owner, pending, retry]
        .into_iter()
        .all(|stored| stored.is_none_or(|stored| stored == identity))
}

/// Select current admissions and stale owners; unavailable identities stay pending.
fn ready_pids_for_file(
    pid_inodes: &HashMap<u32, Vec<FileKey>>,
    pending: &HashMap<u32, ProcessIdentity>,
    file: FileKey,
    status: impl Fn(ProcessIdentity) -> IdentityStatus,
) -> (Vec<ProcessIdentity>, Vec<ProcessIdentity>) {
    let mut ready = Vec::new();
    let mut stale = Vec::new();
    for (pid, files) in pid_inodes {
        if let Some(identity) = pending.get(pid).copied()
            && files.contains(&file)
        {
            match status(identity) {
                IdentityStatus::Current => ready.push(identity),
                IdentityStatus::Stale => stale.push(identity),
                IdentityStatus::Unavailable => {}
            }
        }
    }
    (ready, stale)
}

/// Drop `file` from every pid association, removing pids left with no files.
fn forget_file_associations(
    pid_inodes: &mut HashMap<u32, Vec<FileKey>>,
    pending: &mut HashMap<u32, ProcessIdentity>,
    file: FileKey,
) {
    let mut empty = Vec::new();
    for (pid, files) in pid_inodes.iter_mut() {
        files.retain(|entry| *entry != file);
        if files.is_empty() {
            empty.push(*pid);
        }
    }
    for pid in empty {
        pid_inodes.remove(&pid);
        pending.remove(&pid);
    }
}

/// Drop file ownership only when no retained attachment needs an exit path.
///
/// `detach_process` can only release `attachments[file]` while at least one
/// owner association remains. A failed stale reattachment therefore keeps its
/// owners even though `traced_files` is cleared for retry.
fn forget_owners_after_failed_result(
    has_attachment: bool,
    pid_inodes: &mut HashMap<u32, Vec<FileKey>>,
    pending: &mut HashMap<u32, ProcessIdentity>,
    file: FileKey,
) {
    if !has_attachment {
        forget_file_associations(pid_inodes, pending, file);
    }
}

/// Whether a missing tap on this binary means lost LLM coverage.
///
/// `path` is the mapped pathname from `<pid>/maps`, not the attach path: every
/// `ExplicitTap` binary attaches through `<pid>/exe`, whose file name is always
/// "exe" and therefore identifies nothing. Only `cosh-core` issues LLM calls,
/// so only there does an absent symbol mean a pre-tap build whose traffic goes
/// unseen. The sibling binaries are mapped in the same process and never carry
/// the tap, so their absence is expected and must not be reported as a problem.
fn missing_tap_is_a_coverage_gap(path: &str) -> bool {
    Path::new(path.strip_suffix(" (deleted)").unwrap_or(path))
        .file_name()
        .is_some_and(|name| name == "cosh-core")
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// SSL library kind detected from `/proc/<pid>/maps`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SslLibKind {
    /// libssl.so  (OpenSSL / LibreSSL)
    OpenSsl,
    /// Python may embed OpenSSL in its executable or in a shared libpython.
    Python,
    /// libgnutls.so
    GnuTls,
    /// libnspr4.so (NSS / Firefox)
    Nss,
    /// Statically-linked SSL (BoringSSL or OpenSSL, e.g. Node.js, Chrome, Codex CLI)
    Static,
    /// Unknown executable discovered by content probing. Unlike a known
    /// [`Self::Static`] input, its byte-pattern scan is gated on the
    /// BoringSSL-only marker prefilter because the probe cannot trust the
    /// binary name.
    StaticExeProbe,
    /// A binary that carries an explicit observability tap and therefore needs
    /// no TLS-layer probe at all (cosh-ng today, via cosh-core's
    /// `cosh_llm_plaintext_tap`). Distinct from `Static` because there is no
    /// SSL_* API to hook: the uprobe goes on the exported tap symbol instead.
    ExplicitTap,
}

/// Classify a mapped file path into an `SslLibKind`, if it is an SSL library.
fn classify_ssl_lib(path: &str) -> Option<SslLibKind> {
    // Strip " (deleted)" suffix that the kernel appends when the backing file
    // has been unlinked while the process is still running.
    let raw_path = path.strip_suffix(" (deleted)").unwrap_or(path);
    let name = Path::new(raw_path).file_name()?.to_string_lossy();
    if name.starts_with("libssl.so") || name.starts_with("libssl-") {
        return Some(SslLibKind::OpenSsl);
    }
    if name.starts_with("libgnutls.so") || name.starts_with("libgnutls-") {
        return Some(SslLibKind::GnuTls);
    }
    if name.starts_with("libnspr4.so") || name.starts_with("libnspr4-") {
        return Some(SslLibKind::Nss);
    }
    // Statically-linked SSL (BoringSSL in node/chrome, OpenSSL in codex).
    // Detect common binary names that are known to statically link SSL.
    if matches!(
        name.as_ref(),
        "node"
            | "nodejs"
            | "bun"
            | "deno"
            | "chrome"
            | "chromium"
            | "google-chrome"
            | "google-chrome-stable"
            | "claude"
            | "claude.exe"
    ) {
        return Some(SslLibKind::Static);
    }
    // Bun-packaged Qoder CLI embeds BoringSSL, including versioned executables.
    if name.starts_with("qodercli") {
        return Some(SslLibKind::Static);
    }
    // Codex CLI statically links OpenSSL 3.x (via openssl-sys / native-tls).
    if name.starts_with("codex") && !name.contains('.') {
        return Some(SslLibKind::Static);
    }
    // cosh-ng links rustls, so nothing SSL-shaped appears in its maps at all
    // and there is no library name to key off -- only the binary itself (#3042).
    if matches!(
        name.as_ref(),
        "cosh-core" | "cosh-shell" | "cosh-cli" | "cosh-gateway"
    ) {
        return Some(SslLibKind::ExplicitTap);
    }
    // uv builds differ in whether OpenSSL lives in the executable or libpython.
    // These are candidates: inspect the image before attempting symbol attach.
    if name.starts_with("python3.") || (name.starts_with("libpython3.") && name.contains(".so")) {
        return Some(SslLibKind::Python);
    }
    None
}

/// Select the byte-pattern scanner for a static-SSL input.
///
/// The BoringSSL marker is a heuristic for unknown executables, not a
/// precondition for known static-OpenSSL inputs such as Codex.
fn scan_mode(kind: SslLibKind) -> ScanMode {
    match kind {
        SslLibKind::StaticExeProbe => ScanMode::BoringSslCandidate,
        _ => ScanMode::KnownStatic,
    }
}

// A missing symbol name rules out this image; libbpf resolves the actual
// function when present. Scan only after inode dedup, so both positive and
// negative results are shared by workers mapping the same Python build.
fn python_ssl_present(path: &str) -> Result<bool> {
    let hits = scan_file_patterns(path, &[b"SSL_write\0"])
        .with_context(|| format!("failed to scan Python SSL image {path}"))?;
    Ok(hits.first().is_some_and(|matches| !matches.is_empty()))
}

fn attach_python_ssl(
    pid: i32,
    path: &str,
    attach: impl FnOnce() -> Result<Vec<Link>>,
) -> AttachOutcome {
    match python_ssl_present(path) {
        Ok(true) => finish_ssl_attach(pid, path, attach()),
        Ok(false) => AttachOutcome::Untraceable,
        Err(error) => {
            log::warn!(
                "[attach_process] pid={pid}: cannot inspect Python SSL image {path}: {error:#}"
            );
            AttachOutcome::Failed
        }
    }
}

fn finish_ssl_attach(pid: i32, path: &str, result: Result<Vec<Link>>) -> AttachOutcome {
    match result {
        Ok(links) => AttachOutcome::Attached(links),
        Err(error) => {
            log::warn!("[attach_process] pid={pid}: attach failed for {path}: {error:#}");
            AttachOutcome::Failed
        }
    }
}

/// Parse a maps `dev` field (`maj:min`, both hex) into one stable value.
///
/// The device is what keeps equal inode numbers on different filesystems
/// apart, so it has to survive into the dedup key. `major` takes the high 32
/// bits, `minor` the low 32.
fn parse_maps_dev(dev: &str) -> Option<u64> {
    let (major, minor) = dev.split_once(':')?;
    let major = u32::from_str_radix(major, 16).ok()?;
    let minor = u32::from_str_radix(minor, 16).ok()?;
    Some(((major as u64) << 32) | minor as u64)
}

/// Parse a maps `start-end` field (both hex) into its address range.
///
/// The range is what identifies the mapping inside `<pid>/map_files`, which is
/// the only handle on an unlinked shared library that is still mapped.
fn parse_maps_range(field: &str) -> Option<(u64, u64)> {
    let (start, end) = field.split_once('-')?;
    Some((
        u64::from_str_radix(start, 16).ok()?,
        u64::from_str_radix(end, 16).ok()?,
    ))
}

/// Split one `maps` line into `((start, end), (device, inode), pathname)`.
///
/// The line is `start-end perms offset dev inode pathname`, and the pathname is
/// taken as the verbatim remainder rather than another whitespace token: it can
/// contain spaces and carries a literal `" (deleted)"` suffix for unlinked files,
/// which the caller keys off. Anonymous and pseudo mappings (`[heap]`, `[stack]`)
/// return an empty pathname and are filtered by the caller.
fn parse_maps_line(line: &str) -> Option<((u64, u64), FileKey, &str)> {
    let mut rest = line;
    let mut range = None;
    let mut dev = None;
    let mut inode = None;
    for field in 0..5 {
        rest = rest.trim_start();
        match rest.find(char::is_whitespace) {
            Some(end) => {
                if field == 0 {
                    range = parse_maps_range(&rest[..end]);
                }
                if field == 3 {
                    dev = parse_maps_dev(&rest[..end]);
                }
                if field == 4 {
                    inode = rest[..end].parse::<u64>().ok();
                }
                rest = &rest[end..];
            }
            // Anonymous mappings end at the inode with neither a pathname nor
            // trailing whitespace; surface them with an empty path instead of
            // dropping the line.
            None if field == 4 => return Some((range?, (dev?, rest.parse::<u64>().ok()?), "")),
            None => return None,
        }
    }
    Some((range?, (dev?, inode?), rest.trim_start()))
}

/// Parse `<procfs root>/<pid>/maps` and return
/// `(attach_path, (device, inode), SslLibKind, mapped_path)` for every
/// SSL-related library found.
///
/// Each unique file is returned at most once. Parsed by hand rather than via the
/// `procfs` crate so the read honours the configured procfs root -- that crate
/// hardcodes `/proc`, which an observer reading a bind-mounted host procfs cannot
/// use.
fn ssl_libs_from_maps(pid: i32) -> Result<Vec<(String, FileKey, SslLibKind, String)>> {
    let maps_path = proc_pid_entry(pid, "maps");
    let maps =
        fs::read(&maps_path).with_context(|| format!("failed to read {}", maps_path.display()))?;
    Ok(ssl_libs_from_maps_text(&maps, pid))
}

/// Classify the SSL libraries of a process from the raw bytes of its
/// `<pid>/maps` file, returning
/// `(attach_path, (device, inode), SslLibKind, mapped_path)` per library.
///
/// `mapped_path` is the verbatim pathname from maps (including a
/// `" (deleted)"` suffix): the attach path can be a generic `<pid>/exe`, so
/// diagnostics that need to know *which* file this is must use the mapped path.
///
/// Split from [`ssl_libs_from_maps`] so tests can feed synthetic maps without a
/// real procfs.
fn ssl_libs_from_maps_text(maps: &[u8], pid: i32) -> Vec<(String, FileKey, SslLibKind, String)> {
    // Linux pathnames are arbitrary bytes: one non-UTF-8 pathname must not
    // fail the whole maps read and silently drop every SSL library in the
    // process. Decode lossily -- the pathname only has to be classified and
    // passed to libbpf, never round-tripped -- mirroring
    // `enforcement::target::read_proc_text_lossy`.
    let maps = String::from_utf8_lossy(maps);

    let mut seen_files: HashSet<FileKey> = HashSet::new();
    let mut results: Vec<(String, FileKey, SslLibKind, String)> = Vec::new();

    for line in maps.lines() {
        let Some((range, file, path_str)) = parse_maps_line(line) else {
            continue;
        };
        // Only care about file-backed mappings.
        if file.1 == 0 || !path_str.starts_with('/') || seen_files.contains(&file) {
            continue;
        }
        if let Some(kind) = classify_ssl_lib(path_str) {
            seen_files.insert(file);
            let attach_path = if matches!(kind, SslLibKind::Static | SslLibKind::ExplicitTap) {
                // Statically-linked SSL binary (codex, node, cosh-core, etc).
                // <pid>/exe is a kernel-maintained symlink that stays valid
                // even when the backing file has been replaced or unlinked,
                // which is common for npm-installed binaries that get updated
                // while old processes are still running.
                proc_pid_entry(pid, "exe")
            } else if path_str.ends_with(" (deleted)") {
                // An unlinked *shared* library: its pathname no longer exists,
                // and <pid>/exe is the executable, which exports none of the
                // library's SSL symbols -- attaching there fails on every
                // sweep and leaves all of that process's LLM traffic
                // invisible. The maps range names the still-mapped inode
                // through <pid>/map_files, which the kernel keeps reachable
                // exactly for this case.
                proc_pid_entry(pid, &format!("map_files/{:x}-{:x}", range.0, range.1))
            } else {
                // Prefix with `<pid>/root` so that the uprobe target resolves
                // through the process's own mount namespace. This is
                // intentional: `canonicalize()` would resolve overlayfs paths
                // to the host's lower/upper dirs, which libbpf cannot always
                // map back to an inode for uprobe attachment. The kernel's
                // uprobe mechanism natively understands `<pid>/root/<path>`
                // because it follows the process's mount namespace, making
                // this safe for both host and container processes -- and it
                // keeps working when the `<pid>` entry itself comes from a
                // bind-mounted host procfs.
                proc_pid_rooted(pid, path_str)
            };
            results.push((
                attach_path.to_string_lossy().into_owned(),
                file,
                kind,
                path_str.to_string(),
            ));
        }
    }

    results
}

// ─── uprobe helpers ───────────────────────────────────────────────────────────

#[allow(clippy::field_reassign_with_default)]
fn make_sym_opts(sym: &str, retprobe: bool) -> UprobeOpts {
    let mut o = UprobeOpts::default();
    o.func_name = sym.to_string();
    o.retprobe = retprobe;
    o
}

macro_rules! up {
    ($prog:expr, $pid:expr, $path:expr, $sym:expr) => {
        $prog
            .attach_uprobe_with_opts($pid, $path, 0, make_sym_opts($sym, false))
            .with_context(|| format!("uprobe {}@{}", $sym, $path))
    };
}
macro_rules! ur {
    ($prog:expr, $pid:expr, $path:expr, $sym:expr) => {
        $prog
            .attach_uprobe_with_opts($pid, $path, 0, make_sym_opts($sym, true))
            .with_context(|| format!("uretprobe {}@{}", $sym, $path))
    };
}
macro_rules! up_off {
    ($prog:expr, $pid:expr, $path:expr, $off:expr) => {
        $prog
            .attach_uprobe(false, $pid, $path, $off)
            .with_context(|| format!("uprobe offset {:#x}@{}", $off, $path))
    };
}
macro_rules! ur_off {
    ($prog:expr, $pid:expr, $path:expr, $off:expr) => {
        $prog
            .attach_uprobe(true, $pid, $path, $off)
            .with_context(|| format!("uretprobe offset {:#x}@{}", $off, $path))
    };
}

fn attach_openssl(skel: &mut SslsniffSkel<'_>, lib: &str, pid: i32) -> Result<Vec<Link>> {
    Ok(vec![
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "SSL_write")?,
        ur!(
            skel.progs_mut().probe_SSL_write_exit(),
            pid,
            lib,
            "SSL_write"
        )?,
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "SSL_read")?,
        ur!(skel.progs_mut().probe_SSL_read_exit(), pid, lib, "SSL_read")?,
        up!(
            skel.progs_mut().probe_SSL_write_ex_enter(),
            pid,
            lib,
            "SSL_write_ex"
        )?,
        ur!(
            skel.progs_mut().probe_SSL_write_ex_exit(),
            pid,
            lib,
            "SSL_write_ex"
        )?,
        up!(
            skel.progs_mut().probe_SSL_read_ex_enter(),
            pid,
            lib,
            "SSL_read_ex"
        )?,
        ur!(
            skel.progs_mut().probe_SSL_read_ex_exit(),
            pid,
            lib,
            "SSL_read_ex"
        )?,
        up!(
            skel.progs_mut().probe_SSL_do_handshake_enter(),
            pid,
            lib,
            "SSL_do_handshake"
        )?,
        ur!(
            skel.progs_mut().probe_SSL_do_handshake_exit(),
            pid,
            lib,
            "SSL_do_handshake"
        )?,
    ])
}

fn attach_gnutls(skel: &mut SslsniffSkel<'_>, lib: &str, pid: i32) -> Result<Vec<Link>> {
    Ok(vec![
        up!(
            skel.progs_mut().probe_SSL_rw_enter(),
            pid,
            lib,
            "gnutls_record_send"
        )?,
        ur!(
            skel.progs_mut().probe_SSL_write_exit(),
            pid,
            lib,
            "gnutls_record_send"
        )?,
        up!(
            skel.progs_mut().probe_SSL_rw_enter(),
            pid,
            lib,
            "gnutls_record_recv"
        )?,
        ur!(
            skel.progs_mut().probe_SSL_read_exit(),
            pid,
            lib,
            "gnutls_record_recv"
        )?,
    ])
}

fn attach_nss(skel: &mut SslsniffSkel<'_>, lib: &str, pid: i32) -> Result<Vec<Link>> {
    Ok(vec![
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "PR_Write")?,
        ur!(
            skel.progs_mut().probe_SSL_write_exit(),
            pid,
            lib,
            "PR_Write"
        )?,
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "PR_Send")?,
        ur!(skel.progs_mut().probe_SSL_write_exit(), pid, lib, "PR_Send")?,
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "PR_Read")?,
        ur!(skel.progs_mut().probe_SSL_read_exit(), pid, lib, "PR_Read")?,
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "PR_Recv")?,
        ur!(skel.progs_mut().probe_SSL_read_exit(), pid, lib, "PR_Recv")?,
    ])
}

fn attach_static_ssl_by_symbol(
    skel: &mut SslsniffSkel<'_>,
    lib: &str,
    pid: i32,
) -> Result<Vec<Link>> {
    Ok(vec![
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "SSL_write")?,
        ur!(
            skel.progs_mut().probe_SSL_write_exit(),
            pid,
            lib,
            "SSL_write"
        )?,
        up!(skel.progs_mut().probe_SSL_rw_enter(), pid, lib, "SSL_read")?,
        ur!(skel.progs_mut().probe_SSL_read_exit(), pid, lib, "SSL_read")?,
        up!(
            skel.progs_mut().probe_SSL_do_handshake_enter(),
            pid,
            lib,
            "SSL_do_handshake"
        )?,
        ur!(
            skel.progs_mut().probe_SSL_do_handshake_exit(),
            pid,
            lib,
            "SSL_do_handshake"
        )?,
    ])
}

fn attach_static_ssl_by_offset(
    skel: &mut SslsniffSkel<'_>,
    lib: &str,
    off: &StaticSslOffsets,
    handshake: bool,
    pid: i32,
) -> Result<Vec<Link>> {
    let mut links = Vec::new();

    if off.write_is_ex {
        // SSL_write_ex: returns 0/1, byte count in *written (4th arg).
        // Use the _ex BPF programs which read from the output pointer.
        links.push(up_off!(
            skel.progs_mut().probe_SSL_write_ex_enter(),
            pid,
            lib,
            off.ssl_write
        )?);
        links.push(ur_off!(
            skel.progs_mut().probe_SSL_write_ex_exit(),
            pid,
            lib,
            off.ssl_write
        )?);
    } else {
        links.push(up_off!(
            skel.progs_mut().probe_SSL_rw_enter(),
            pid,
            lib,
            off.ssl_write
        )?);
        links.push(ur_off!(
            skel.progs_mut().probe_SSL_write_exit(),
            pid,
            lib,
            off.ssl_write
        )?);
    }

    if off.read_is_ex {
        links.push(up_off!(
            skel.progs_mut().probe_SSL_read_ex_enter(),
            pid,
            lib,
            off.ssl_read
        )?);
        links.push(ur_off!(
            skel.progs_mut().probe_SSL_read_ex_exit(),
            pid,
            lib,
            off.ssl_read
        )?);
    } else {
        links.push(up_off!(
            skel.progs_mut().probe_SSL_rw_enter(),
            pid,
            lib,
            off.ssl_read
        )?);
        links.push(ur_off!(
            skel.progs_mut().probe_SSL_read_exit(),
            pid,
            lib,
            off.ssl_read
        )?);
    }

    if handshake {
        links.push(up_off!(
            skel.progs_mut().probe_SSL_do_handshake_enter(),
            pid,
            lib,
            off.ssl_do_handshake
        )?);
        links.push(ur_off!(
            skel.progs_mut().probe_SSL_do_handshake_exit(),
            pid,
            lib,
            off.ssl_do_handshake
        )?);
    }
    Ok(links)
}

/// Attach the plaintext tap cosh-ng exports for observability.
///
/// One uprobe covers both directions: the tap's first argument says which way
/// the payload is going. It fires at function entry, where the buffer is already
/// an argument, so no uretprobe is needed. There is no handshake probe either —
/// the tap sits at the application layer and never sees TLS records, so a
/// cosh-ng process reports no handshake timings.
///
/// # Errors
///
/// Fails when the symbol is absent, which means the binary was built without
/// `-Wl,--export-dynamic`: `strip = true` erases `.symtab`, so only `.dynsym`
/// survives, and the symbol has to be promoted there deliberately.
fn attach_cosh_tap(skel: &mut SslsniffSkel<'_>, lib: &str, pid: i32) -> Result<Vec<Link>> {
    Ok(vec![up!(
        skel.progs_mut().probe_cosh_plaintext_tap(),
        pid,
        lib,
        COSH_TAP_SYMBOL
    )?])
}

// ─── Codex offset table (Tier 3) ────────────────────────────────────────────

use super::codex_offsets::{LearnedOffsets, OffsetTable};

static CODEX_OFFSET_TABLE: std::sync::LazyLock<Option<OffsetTable>> =
    std::sync::LazyLock::new(|| {
        let json = include_str!("../../agentsight.json");
        OffsetTable::load(json)
    });

#[cfg(test)]
mod python_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn pid_identity(pid: u32) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            start_time: 1000 + u64::from(pid),
        }
    }

    /// Build a raw ring-buffer sample: the header prefix (each field written at its
    /// real bindgen offset) followed by `payload`, with buf_size/len/truncated/
    /// is_handshake set explicitly. Each scalar header field gets a DISTINCT
    /// sentinel (pid≠tid≠uid, a marker delta_ns/comm) so any field/offset swap among
    /// the adjacent u32s or on rw/comm/is_handshake fails a test. `tail_pad` appends
    /// bytes AFTER the payload to simulate a still-full-size record (e.g. tcpsniff's
    /// 4 MiB reservation) whose data.len() exceeds buf_size.
    fn make_record(
        payload: &[u8],
        buf_size: u32,
        len: u32,
        truncated: i32,
        tail_pad: usize,
        is_handshake: i32,
    ) -> Vec<u8> {
        type R = bpf::probe_SSL_data_t;
        let hdr = std::mem::offset_of!(R, buf);
        let mut v = vec![0u8; hdr + payload.len() + tail_pad];
        let put_u32 = |v: &mut [u8], off: usize, val: u32| {
            v[off..off + 4].copy_from_slice(&val.to_ne_bytes())
        };
        let put_u64 = |v: &mut [u8], off: usize, val: u64| {
            v[off..off + 8].copy_from_slice(&val.to_ne_bytes())
        };
        let put_i32 = |v: &mut [u8], off: usize, val: i32| {
            v[off..off + 4].copy_from_slice(&val.to_ne_bytes())
        };
        put_u32(&mut v, std::mem::offset_of!(R, source), 2); // EVENT_SOURCE_SSL
        put_u64(&mut v, std::mem::offset_of!(R, timestamp_ns), 0);
        put_u64(&mut v, std::mem::offset_of!(R, delta_ns), 0xDEAD_BEEF);
        put_u32(&mut v, std::mem::offset_of!(R, pid), 1234);
        put_u32(&mut v, std::mem::offset_of!(R, tid), 5678);
        put_u32(&mut v, std::mem::offset_of!(R, uid), 4321);
        put_u32(&mut v, std::mem::offset_of!(R, len), len);
        put_u32(&mut v, std::mem::offset_of!(R, buf_size), buf_size);
        put_i32(&mut v, std::mem::offset_of!(R, rw), 1);
        put_i32(&mut v, std::mem::offset_of!(R, is_handshake), is_handshake);
        put_i32(&mut v, std::mem::offset_of!(R, truncated), truncated);
        put_u64(&mut v, std::mem::offset_of!(R, ssl_ptr), 0xABCD);
        let comm_off = std::mem::offset_of!(R, comm);
        v[comm_off..comm_off + 4].copy_from_slice(b"node");
        v[hdr..hdr + payload.len()].copy_from_slice(payload);
        v
    }

    #[test]
    fn from_bytes_decodes_small_record() {
        // A small (16 KiB-tier) record — the whole point of the fix. Under the old
        // `data.len() >= size_of::<full struct>()` (~4 MiB) gate this sample was
        // DROPPED, so reverting to that gate makes this test fail.
        let payload = b"POST /v1/messages HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let rec = make_record(payload, payload.len() as u32, payload.len() as u32, 0, 0, 0);
        assert!(
            rec.len() < MAX_BUF_SIZE,
            "record is far smaller than the full struct"
        );
        let ev = SslEvent::from_bytes(&rec).expect("small record must decode");
        assert_eq!(&ev.buf[..], &payload[..]);
        assert_eq!(ev.len as usize, payload.len());
        assert_eq!(ev.ssl_ptr, 0xABCD);
        // Each header scalar decodes from its OWN offset (distinct sentinels: a
        // pid/tid offset swap, or an unset uid/delta_ns/rw/comm, fails here).
        assert_eq!(ev.pid, 1234);
        assert_eq!(ev.tid, 5678, "tid decodes from its own offset, not pid's");
        assert_eq!(ev.uid, 4321);
        assert_eq!(ev.delta_ns, 0xDEAD_BEEF);
        assert_eq!(ev.rw, 1);
        assert_eq!(ev.comm, "node");
        assert!(!ev.is_handshake);
    }

    #[test]
    fn from_bytes_decodes_handshake_header_only() {
        // A handshake record carries NO payload: the BPF reserves header-only, so
        // data.len() == offset_of!(buf) EXACTLY. The decoder must ACCEPT this
        // boundary (a `<`→`<=` off-by-one at the header-length check would reject it)
        // and decode is_handshake. Complements the reject-at-hdr-1 test below, so the
        // header boundary is pinned on both sides.
        let rec = make_record(&[], 0, 0, 0, 0, 1);
        assert_eq!(
            rec.len(),
            std::mem::offset_of!(bpf::probe_SSL_data_t, buf),
            "handshake record is exactly the header prefix"
        );
        let ev = SslEvent::from_bytes(&rec).expect("header-only handshake must decode");
        assert!(ev.is_handshake, "is_handshake decodes true");
        assert!(ev.buf.is_empty(), "no payload");
    }

    #[test]
    fn from_bytes_preserves_intermediate_tier_payloads() {
        let header = std::mem::offset_of!(bpf::probe_SSL_data_t, buf);
        assert_eq!(
            header,
            std::mem::offset_of!(bpf::probe_SSL_data_xlarge, buf)
        );
        assert_eq!(
            header,
            std::mem::offset_of!(bpf::probe_SSL_data_xxlarge, buf)
        );

        let cases = [
            (1024 * 1024 + 9, bpf::SSL_TIER_XLARGE as usize),
            (bpf::SSL_TIER_XLARGE as usize, bpf::SSL_TIER_XLARGE as usize),
            (
                bpf::SSL_TIER_XLARGE as usize + 1,
                bpf::SSL_TIER_XXLARGE as usize,
            ),
            (
                bpf::SSL_TIER_XXLARGE as usize,
                bpf::SSL_TIER_XXLARGE as usize,
            ),
        ];
        for (length, capacity) in cases {
            let payload: Vec<_> = (0..length).map(|index| (index % 251) as u8).collect();
            let record = make_record(
                &payload,
                length as u32,
                length as u32,
                0,
                capacity - length,
                0,
            );
            let event = SslEvent::from_bytes(&record).expect("intermediate tier must decode");
            assert_eq!(event.buf, payload);
            assert_eq!(event.len as usize, length);
            assert_eq!(event.ssl_ptr, 0xABCD);
            assert_eq!(event.pid, 1234);
            assert_eq!(event.comm, "node");
        }
    }

    #[test]
    fn from_bytes_uses_buf_size_field_not_data_len() {
        // A still-full-size record (e.g. tcpsniff's 4 MiB reservation): data carries
        // many bytes after the payload, but buf_size says only `n` are real. The
        // decoder MUST take buf_size bytes, never data.len()-hdr — otherwise it would
        // read the padding tail. Reverting to a data.len()-derived length fails this.
        let payload = b"hi there";
        let rec = make_record(
            payload,
            payload.len() as u32,
            payload.len() as u32,
            0,
            4096,
            0,
        );
        let ev = SslEvent::from_bytes(&rec).expect("full-size record must decode");
        assert_eq!(
            &ev.buf[..],
            &payload[..],
            "buf is the buf_size bytes, not the padded tail"
        );
    }

    #[test]
    fn from_bytes_rejects_short_header() {
        // A sample shorter than the header prefix is rejected (no UB, no full cast).
        let hdr = std::mem::offset_of!(bpf::probe_SSL_data_t, buf);
        assert!(SslEvent::from_bytes(&vec![0u8; hdr - 1]).is_none());
    }

    #[test]
    fn from_bytes_decodes_truncated_record() {
        // A truncated record (payload clamped to the cap): buf_size < len, truncated=1,
        // len > MAX_BUF_SIZE. The decoder still returns the captured bytes; len reports
        // the true size. Also exercises the warn guard's positive case.
        let captured = vec![b'x'; 64];
        let rec = make_record(&captured, captured.len() as u32, 9_000_000, 1, 0, 0);
        let ev = SslEvent::from_bytes(&rec).expect("truncated record must still decode");
        assert_eq!(ev.buf.len(), 64);
        assert_eq!(
            ev.len, 9_000_000,
            "len reports the true (pre-truncation) size"
        );
    }

    #[test]
    fn from_bytes_clamps_oversized_buf_size_to_available() {
        // Defense: a buf_size larger than the bytes present must not read past the
        // sample (clamped to data.len()-hdr).
        let payload = b"abc";
        let rec = make_record(payload, 1000, 1000, 0, 0, 0);
        let ev = SslEvent::from_bytes(&rec).expect("decode");
        assert_eq!(
            ev.buf.len(),
            payload.len(),
            "buf_size clamped to available bytes"
        );
    }

    // ─── find_static_ssl_offsets regression tests ────────────────────────────
    // Prologue literals mirror the ones inside find_static_ssl_offsets. If a
    // production pattern is edited without updating these fixtures, detection
    // on the synthetic image fails and the tests trip — that is the point.
    const HS_CLANG_PAT: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x53, 0x48, 0x83,
        0xec, 0x28, 0x49, 0x89, 0xfc, 0x48, 0x8b, 0x47, 0x30,
    ];
    const HS_BUN_PAT: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x53, 0x50, 0x4c, 0x8b, 0x7f, 0x30, 0x49,
        0x83, 0xbf, 0x18, 0x01, 0x00, 0x00, 0x00, 0x74,
    ];
    const READ_PAT_T: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x53, 0x50, 0x48, 0x83, 0xbf, 0x98, 0x00,
        0x00, 0x00, 0x00, 0x74,
    ];
    const WRITE_PAT_T: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x55, 0x41, 0x54, 0x53, 0x48, 0x83,
        0xec, 0x18, 0x41, 0x89, 0xd7, 0x49, 0x89, 0xf6, 0x48, 0x89, 0xfb,
    ];
    const READ_BUN2_PAT: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x57, 0x41, 0x56, 0x41, 0x54, 0x53, 0x48, 0x83, 0xbf, 0x98,
        0x00, 0x00, 0x00, 0x00, 0x0f, 0x85,
    ];
    const HS_BUN2_PAT: &[u8] = &[
        0x55, 0x48, 0x89, 0xe5, 0x41, 0x56, 0x53, 0x48, 0x89, 0xfb, 0x48, 0x8b, 0x47, 0x30, 0xc7,
        0x80, 0xc4, 0x00, 0x00, 0x00, 0x00,
    ];

    /// Zero-filled image for the Bun 2026 generation: SSL_write precedes
    /// SSL_read and the handshake follows it (the reordered ssl_lib.cc layout).
    fn build_bun2_ssl_image(write: usize, read: usize, hs: usize) -> Vec<u8> {
        let mut img = vec![0u8; 0x5000];
        img[write..write + WRITE_PAT_T.len()].copy_from_slice(WRITE_PAT_T);
        img[read..read + READ_BUN2_PAT.len()].copy_from_slice(READ_BUN2_PAT);
        img[hs..hs + HS_BUN2_PAT.len()].copy_from_slice(HS_BUN2_PAT);
        img
    }

    /// Zero-filled synthetic binary with the given prologues planted at file
    /// offsets; zeros cannot match any pattern, so matches are unambiguous.
    fn build_static_ssl_image(
        hs: Option<(usize, &[u8])>,
        read: Option<usize>,
        write: Option<usize>,
    ) -> Vec<u8> {
        let mut img = vec![0u8; 0x5000];
        if let Some((off, pat)) = hs {
            img[off..off + pat.len()].copy_from_slice(pat);
        }
        if let Some(off) = read {
            img[off..off + READ_PAT_T.len()].copy_from_slice(READ_PAT_T);
        }
        if let Some(off) = write {
            img[off..off + WRITE_PAT_T.len()].copy_from_slice(WRITE_PAT_T);
        }
        img
    }

    fn with_static_ssl_fixture(name: &str, img: &[u8], f: impl FnOnce(&str)) {
        let path = std::env::temp_dir().join(format!(
            "agentsight-static-ssl-{}-{name}",
            std::process::id()
        ));
        fs::write(&path, img).expect("write synthetic binary fixture");
        let path_str = path.to_str().expect("temp path is valid UTF-8");
        f(path_str);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn scan_outcome_classifies_markers_and_offsets() {
        let stop = AtomicBool::new(false);
        let plain = vec![0u8; 0x200];
        with_static_ssl_fixture("scan-no-markers", &plain, |path| {
            assert!(matches!(
                scan_static_ssl_executable_paced(path, None, &stop, ScanMode::BoringSslCandidate),
                Some(ExecutableScan::NoMarkers)
            ));
        });

        let mut marker_only = vec![0u8; 0x200];
        marker_only[0x20..0x20 + b"boringssl".len()].copy_from_slice(b"boringssl");
        with_static_ssl_fixture("scan-marker-only", &marker_only, |path| {
            assert!(matches!(
                scan_static_ssl_executable_paced(path, None, &stop, ScanMode::BoringSslCandidate),
                Some(ExecutableScan::NoOffsets)
            ));
        });

        let mut complete =
            build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        complete[0x20..0x20 + b"boringssl".len()].copy_from_slice(b"boringssl");
        with_static_ssl_fixture("scan-complete", &complete, |path| {
            assert!(matches!(
                scan_static_ssl_executable_paced(path, None, &stop, ScanMode::BoringSslCandidate),
                Some(ExecutableScan::Offsets(_))
            ));
        });

        let cancelled = AtomicBool::new(true);
        with_static_ssl_fixture("scan-cancelled", &complete, |path| {
            assert!(
                scan_static_ssl_executable_paced(
                    path,
                    None,
                    &cancelled,
                    ScanMode::BoringSslCandidate,
                )
                .is_none()
            );
        });
    }

    #[test]
    fn known_static_scan_does_not_require_the_boringssl_marker() {
        // Regression: a stripped static-OpenSSL/Codex build has valid SSL
        // prologues but no BoringSSL marker. Known Static inputs must skip the
        // exe-probe marker prefilter and reach the byte-pattern scan.
        let stop = AtomicBool::new(false);
        let complete =
            build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        with_static_ssl_fixture("known-static-no-marker", &complete, |path| {
            assert!(matches!(
                scan_static_ssl_executable_paced(path, None, &stop, ScanMode::KnownStatic),
                Some(ExecutableScan::Offsets(_))
            ));
            assert!(matches!(
                scan_static_ssl_executable_paced(path, None, &stop, ScanMode::BoringSslCandidate),
                Some(ExecutableScan::NoMarkers)
            ));
        });
    }

    fn fixture_file_key(path: &Path) -> FileKey {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).expect("stat fixture");
        (stat_dev_to_key(meta.dev()), meta.ino())
    }

    /// Serializes tests that republish the process-wide learned-cache path.
    static LEARNED_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Build an `SslSniff` whose learned-offset cache points at a per-test
    /// temp directory. The caller must hold [`LEARNED_PATH_LOCK`]: the path
    /// is process-global and read at construction time.
    fn sniffer_with_isolated_learned_cache(
        _guard: &std::sync::MutexGuard<'_, ()>,
        dir: &tempfile::TempDir,
    ) -> SslSniff {
        crate::config::set_ssl_offsets_learned_path(dir.path().join("learned.json"));
        SslSniff::new().expect("sslsniff BPF should load on this kernel")
    }

    /// Regression for #9762: one drain tick must install every completed scan,
    /// not a fixed quota. The result channel is bounded and the single worker
    /// blocks on send once it fills; a per-tick quota let results pile up under
    /// process churn and stalled the worker. Reverting to a quota below the
    /// fixture count makes this test fail.
    #[test]
    #[ignore] // constructs SslSniff: requires CAP_BPF + CAP_PERFMON (root)
    fn scan_drain_installs_all_completed_results_in_one_tick() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: uprobe attach requires root");
            return;
        }
        let guard = LEARNED_PATH_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let learned_dir = tempfile::tempdir().unwrap();
        let mut sniffer = sniffer_with_isolated_learned_cache(&guard, &learned_dir);
        let owner = std::process::id();
        let image = build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        // More fixtures than the historical per-tick quota of four.
        let fixtures: Vec<_> = (0..6)
            .map(|_| {
                let file = tempfile::NamedTempFile::new().expect("fixture");
                fs::write(file.path(), &image).expect("write fixture");
                file
            })
            .collect();
        let mut keys = Vec::new();
        for file in &fixtures {
            let key = fixture_file_key(file.path());
            sniffer.traced_files.insert(key);
            sniffer
                .scans
                .submit(file.path().to_str().unwrap(), key, ScanMode::KnownStatic)
                .expect("submit scan");
            keys.push(key);
        }
        sniffer.pid_inodes.insert(owner, keys);
        // The worker scans 20 KiB fixtures in milliseconds; wait generously so
        // every result is queued before the single drain tick under test.
        std::thread::sleep(Duration::from_secs(3));
        let _ = sniffer.poll_pending_scan_admissions();
        assert_eq!(
            sniffer.attachments.len(),
            fixtures.len(),
            "one tick must install every completed scan"
        );
    }

    /// Regression for #9762: when the originating process exits before its
    /// scan result is installed, the verified offsets are inode-global and
    /// must still be cached and attached, so the next process mapping the
    /// binary is captured without re-racing the scan. Reverting to the
    /// ownerless discard makes this test fail.
    #[test]
    #[ignore] // constructs SslSniff: requires CAP_BPF + CAP_PERFMON (root)
    fn exited_owner_offsets_are_installed_and_cached() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: uprobe attach requires root");
            return;
        }
        let guard = LEARNED_PATH_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let learned_dir = tempfile::tempdir().unwrap();
        let mut sniffer = sniffer_with_isolated_learned_cache(&guard, &learned_dir);
        let owner = std::process::id();
        let image = build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        let file = tempfile::NamedTempFile::new().expect("fixture");
        fs::write(file.path(), &image).expect("write fixture");
        let path = file.path().to_str().unwrap().to_owned();
        let key = fixture_file_key(file.path());
        sniffer.traced_files.insert(key);
        sniffer
            .scans
            .submit(&path, key, ScanMode::KnownStatic)
            .expect("submit scan");
        sniffer.pid_inodes.insert(owner, vec![key]);
        // The owner exits while the scan is still in flight.
        sniffer.detach_process(owner);
        std::thread::sleep(Duration::from_secs(1));
        let admitted = sniffer.poll_pending_scan_admissions();
        assert!(admitted.is_empty(), "no live owner remains to admit");
        assert!(
            sniffer.attachments.contains_key(&key),
            "ownerless result must still install the global uprobe"
        );
        assert!(
            sniffer.static_attach_offsets.contains_key(&key),
            "verified offsets must stay cached for the next process"
        );
        assert!(sniffer.traced_files.contains(&key));
        // The next process attaching this inode must reuse the cached offsets
        // instead of queueing another whole-binary scan.
        let outcome = sniffer.build_attach(owner as i32, key, &path, SslLibKind::Static, &path);
        assert!(
            matches!(outcome, AttachOutcome::Attached(_)),
            "cached offsets must attach without a re-scan"
        );
        assert!(!sniffer.scans.is_pending(key), "no re-scan may be queued");
    }

    /// Regression for #9762: offsets learned in one service lifetime must
    /// make the next lifetime attach synchronously at discovery — a warm
    /// agent's first TLS call completes long before an asynchronous
    /// whole-binary scan would install. Reverting the learned cache (or its
    /// persistence) makes this test fail.
    #[test]
    #[ignore] // constructs SslSniff: requires CAP_BPF + CAP_PERFMON (root)
    fn learned_offsets_survive_restart_and_attach_synchronously() {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("skipping: uprobe attach requires root");
            return;
        }
        let guard = LEARNED_PATH_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let learned_dir = tempfile::tempdir().unwrap();
        let image = build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        let file = tempfile::NamedTempFile::new().expect("fixture");
        fs::write(file.path(), &image).expect("write fixture");
        let path = file.path().to_str().unwrap().to_owned();
        let key = fixture_file_key(file.path());
        let owner = std::process::id();

        {
            let mut sniffer = sniffer_with_isolated_learned_cache(&guard, &learned_dir);
            sniffer.traced_files.insert(key);
            sniffer
                .scans
                .submit(&path, key, ScanMode::KnownStatic)
                .expect("submit scan");
            sniffer.pid_inodes.insert(owner, vec![key]);
            std::thread::sleep(Duration::from_secs(1));
            let _ = sniffer.poll_pending_scan_admissions();
            assert!(
                sniffer.attachments.contains_key(&key),
                "first lifetime must install the scanned offsets"
            );
        } // Dropping the sniffer detaches its links, simulating a service stop.

        // "Restart": a fresh instance must attach from the persisted cache
        // without queueing another whole-binary scan.
        let mut restarted = sniffer_with_isolated_learned_cache(&guard, &learned_dir);
        let outcome = restarted.build_attach(owner as i32, key, &path, SslLibKind::Static, &path);
        assert!(
            matches!(outcome, AttachOutcome::Attached(_)),
            "learned cache must attach synchronously after a restart"
        );
        assert!(
            !restarted.scans.is_pending(key),
            "a cache hit must not queue another whole-binary scan"
        );
    }

    #[test]
    fn retry_selection_only_returns_elapsed_deadlines() {
        let now = Instant::now();
        let mut retries = HashMap::new();
        retries.insert(
            (1, 1),
            ScanRetry {
                deadline: now - Duration::from_secs(1),
                path: "/old".into(),
                mode: ScanMode::KnownStatic,
            },
        );
        retries.insert(
            (2, 2),
            ScanRetry {
                deadline: now + Duration::from_secs(1),
                path: "/future".into(),
                mode: ScanMode::BoringSslCandidate,
            },
        );
        assert_eq!(
            due_retries(now, &retries),
            vec![((1, 1), "/old".to_owned(), ScanMode::KnownStatic)]
        );
    }

    #[test]
    fn deferred_admission_rejects_a_reused_pid() {
        let file = (7, 7);
        let current = ProcessIdentity::read(std::process::id() as i32).unwrap();
        let pid_inodes = HashMap::from([(current.pid, vec![file])]);
        let reused = ProcessIdentity {
            start_time: current.start_time + 1,
            ..current
        };
        let pending = HashMap::from([(current.pid, reused)]);
        let (ready, stale) =
            ready_pids_for_file(&pid_inodes, &pending, file, ProcessIdentity::status);
        assert!(ready.is_empty());
        assert_eq!(stale, vec![reused]);

        let pending = HashMap::from([(current.pid, current)]);
        let (ready, stale) =
            ready_pids_for_file(&pid_inodes, &pending, file, ProcessIdentity::status);
        assert_eq!(ready, vec![current]);
        assert!(stale.is_empty());
    }

    #[test]
    fn successful_owner_identity_still_detects_pid_reuse() {
        let current = ProcessIdentity::read(std::process::id() as i32).unwrap();
        let reused = ProcessIdentity {
            start_time: current.start_time + 1,
            ..current
        };
        assert!(!stored_identity_matches(reused, Some(current), None, None));
        assert!(stored_identity_matches(current, Some(current), None, None));
    }

    #[test]
    fn ready_pid_selection_defers_unavailable_and_drops_stale() {
        let file = (7, 7);
        let mut pid_inodes = HashMap::new();
        pid_inodes.insert(10, vec![file]);
        pid_inodes.insert(11, vec![file]);
        pid_inodes.insert(12, vec![(8, 8)]);
        pid_inodes.insert(13, vec![file]);
        let pending = HashMap::from([
            (10, pid_identity(10)),
            (11, pid_identity(11)),
            (13, pid_identity(13)),
        ]);

        let (ready, stale) =
            ready_pids_for_file(&pid_inodes, &pending, file, |identity| match identity.pid {
                10 => IdentityStatus::Current,
                11 => IdentityStatus::Stale,
                _ => IdentityStatus::Unavailable,
            });
        assert_eq!(ready, vec![pid_identity(10)]);
        assert_eq!(stale, vec![pid_identity(11)]);
    }

    #[test]
    fn forgetting_a_file_removes_empty_pid_associations_and_admissions() {
        let file = (7, 7);
        let mut pid_inodes = HashMap::new();
        pid_inodes.insert(10, vec![file]);
        pid_inodes.insert(11, vec![file, (8, 8)]);
        pid_inodes.insert(12, vec![file]);
        let mut pending = HashMap::from([
            (10, pid_identity(10)),
            (11, pid_identity(11)),
            (12, pid_identity(12)),
        ]);

        forget_file_associations(&mut pid_inodes, &mut pending, file);

        assert_eq!(pid_inodes.get(&11), Some(&vec![(8, 8)]));
        assert!(!pid_inodes.contains_key(&10));
        assert!(!pid_inodes.contains_key(&12));
        assert_eq!(pending, HashMap::from([(11, pid_identity(11))]));
    }

    #[test]
    fn failed_stale_reattach_preserves_owner_until_exit() {
        // Regression: a stale byte-pattern attachment keeps its old links while
        // the replacement is queued. If that replacement fails, the owner
        // association must remain so detach_process can release the links when
        // the last mapping process exits.
        let file = (7, 7);
        let mut pid_inodes = HashMap::new();
        pid_inodes.insert(10, vec![file]);
        let mut pending = HashMap::from([(10, pid_identity(10))]);

        forget_owners_after_failed_result(true, &mut pid_inodes, &mut pending, file);
        assert_eq!(pid_inodes.get(&10), Some(&vec![file]));
        assert!(pending.contains_key(&10));

        // Without a retained attachment there is no exit path to preserve.
        forget_owners_after_failed_result(false, &mut pid_inodes, &mut pending, file);
        assert!(pid_inodes.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn static_ssl_offsets_matches_bun_handshake_variant() {
        // Regression for Claude Code >= 2.1.113: the Bun-built binary's
        // SSL_do_handshake prologue must be detected alongside read/write.
        let img = build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        with_static_ssl_fixture("bun", &img, |path| {
            let off = find_static_ssl_offsets(path).expect("Bun variant must be detected");
            assert_eq!(off.ssl_do_handshake, 0x100);
            assert_eq!(off.ssl_read, 0x2000);
            assert_eq!(off.ssl_write, 0x2100);
        });
    }

    #[test]
    fn static_ssl_offsets_matches_clang_handshake_variant() {
        let img = build_static_ssl_image(Some((0x100, HS_CLANG_PAT)), Some(0x2000), Some(0x2100));
        with_static_ssl_fixture("clang", &img, |path| {
            let off = find_static_ssl_offsets(path).expect("clang variant must be detected");
            assert_eq!(off.ssl_do_handshake, 0x100);
            assert_eq!(off.ssl_read, 0x2000);
            assert_eq!(off.ssl_write, 0x2100);
        });
    }

    #[test]
    fn static_ssl_offsets_matches_bun2_generation() {
        // Regression for Claude Code >= 2.1.315 (#9762): the 2026 Bun rebuild
        // changed the read/handshake prologues and put SSL_write before SSL_read.
        // Offsets mirror the real 2.1.319 layout (write 0x1a0 before read,
        // handshake 0x470 after), validated with kernel uprobes on live TLS.
        let img = build_bun2_ssl_image(0x2000, 0x21a0, 0x2610);
        with_static_ssl_fixture("bun2", &img, |path| {
            let off = find_static_ssl_offsets(path).expect("Bun2 generation must be detected");
            assert_eq!(off.ssl_write, 0x2000);
            assert_eq!(off.ssl_read, 0x21a0);
            assert_eq!(off.ssl_do_handshake, 0x2610);
            assert!(!off.write_is_ex && !off.read_is_ex);
        });
    }

    #[test]
    fn static_ssl_offsets_bun2_rejects_write_beyond_adjacency() {
        // SSL_write must sit within ADJACENCY_THRESHOLD of SSL_read on either
        // side; a distant write must not resolve.
        let img = build_bun2_ssl_image(0x4000, 0x2000, 0x2100);
        with_static_ssl_fixture("bun2-write-far", &img, |path| {
            assert!(find_static_ssl_offsets(path).is_none());
        });
    }

    #[test]
    fn static_ssl_offsets_bun2_requires_handshake() {
        // Read + write present but no handshake prologue: must not resolve.
        let mut img = vec![0u8; 0x5000];
        img[0x2000..0x2000 + WRITE_PAT_T.len()].copy_from_slice(WRITE_PAT_T);
        img[0x21a0..0x21a0 + READ_BUN2_PAT.len()].copy_from_slice(READ_BUN2_PAT);
        with_static_ssl_fixture("bun2-no-hs", &img, |path| {
            assert!(find_static_ssl_offsets(path).is_none());
        });
    }

    #[test]
    #[ignore = "needs a real Claude Code binary: AGENTSIGHT_CLAUDE_BIN=/path/claude cargo test --lib real_claude_binary_resolves -- --ignored --nocapture"]
    fn real_claude_binary_resolves() {
        // Maintainer aid for the next Bun rebuild: point this at a real
        // claude.exe and confirm the detector still resolves all three offsets,
        // then validate them with kernel uprobes on live TLS traffic (#9762).
        let Ok(path) = std::env::var("AGENTSIGHT_CLAUDE_BIN") else {
            eprintln!("set AGENTSIGHT_CLAUDE_BIN to a claude.exe to run this check");
            return;
        };
        let off = find_static_ssl_offsets(&path).expect("real Claude binary must resolve");
        eprintln!(
            "resolved {}: write={:#x} read={:#x} handshake={:#x}",
            path, off.ssl_write, off.ssl_read, off.ssl_do_handshake
        );
    }

    #[test]
    fn static_ssl_offsets_rejects_handshake_only_after_read() {
        // With multiple handshake matches, selection requires one strictly
        // before SSL_read; here both candidates sit after it.
        let mut img = build_static_ssl_image(None, Some(0x2000), Some(0x2100));
        img[0x3000..0x3000 + HS_BUN_PAT.len()].copy_from_slice(HS_BUN_PAT);
        img[0x4000..0x4000 + HS_BUN_PAT.len()].copy_from_slice(HS_BUN_PAT);
        with_static_ssl_fixture("hs-after-read", &img, |path| {
            assert!(find_static_ssl_offsets(path).is_none());
        });
    }

    #[test]
    fn static_ssl_offsets_prefers_handshake_before_read_on_ambiguity() {
        // Multiple matches: the closest one strictly before SSL_read wins.
        let mut img =
            build_static_ssl_image(Some((0x1000, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        img[0x1800..0x1800 + HS_BUN_PAT.len()].copy_from_slice(HS_BUN_PAT);
        img[0x3000..0x3000 + HS_BUN_PAT.len()].copy_from_slice(HS_BUN_PAT);
        with_static_ssl_fixture("hs-ambiguous", &img, |path| {
            let off = find_static_ssl_offsets(path).expect("ambiguous matches must resolve");
            assert_eq!(off.ssl_do_handshake, 0x1800);
        });
    }

    #[test]
    fn static_ssl_offsets_rejects_write_beyond_adjacency() {
        // SSL_write must sit within ADJACENCY_THRESHOLD (0x1000) after SSL_read.
        let img = build_static_ssl_image(
            Some((0x100, HS_BUN_PAT)),
            Some(0x2000),
            Some(0x2000 + 0x2000),
        );
        with_static_ssl_fixture("write-far", &img, |path| {
            assert!(find_static_ssl_offsets(path).is_none());
        });
    }

    #[test]
    fn static_ssl_offsets_still_resolves_past_one_scan_window() {
        // Guards Claude Code: its Bun single-file executable is the case the
        // byte-pattern path exists for, and it is far larger than one window.
        // The patterns sit in the final window, so a resolution here proves the
        // scan neither stops early nor loses the absolute offset.
        let img = build_static_ssl_image(Some((0x100, HS_BUN_PAT)), Some(0x2000), Some(0x2100));
        let pad = SCAN_CHUNK_BYTES + 0x3000;
        let mut padded = vec![0u8; pad];
        padded.extend_from_slice(&img);
        with_static_ssl_fixture("past-window", &padded, |path| {
            let off = find_static_ssl_offsets(path).expect("large image must still resolve");
            assert_eq!(off.ssl_do_handshake, pad + 0x100);
            assert_eq!(off.ssl_read, pad + 0x2000);
            assert_eq!(off.ssl_write, pad + 0x2100);
        });
    }

    #[test]
    fn scan_file_patterns_finds_match_straddling_window_boundary() {
        // A pattern laid across the window edge is only found if the windows
        // overlap; without the carry it would be split and silently missed.
        let start = SCAN_CHUNK_BYTES - READ_PAT_T.len() / 2;
        let mut img = vec![0u8; SCAN_CHUNK_BYTES + 0x1000];
        img[start..start + READ_PAT_T.len()].copy_from_slice(READ_PAT_T);
        with_static_ssl_fixture("straddle", &img, |path| {
            let hits = scan_file_patterns(path, &[READ_PAT_T]).expect("scan must succeed");
            assert_eq!(hits[0], vec![start]);
        });
    }

    #[test]
    fn collect_pattern_hits_finds_every_signature_in_one_walk() {
        // Distinct signatures at distinct positions must all be reported from a
        // single walk, and a window matching none of them must yield nothing —
        // the rejection case that used to cost one walk per signature.
        let mut window = vec![0u8; 0x400];
        window[0x10..0x10 + READ_PAT_T.len()].copy_from_slice(READ_PAT_T);
        window[0x200..0x200 + WRITE_PAT_T.len()].copy_from_slice(WRITE_PAT_T);
        let patterns: [&[u8]; 2] = [READ_PAT_T, WRITE_PAT_T];
        let mut hits = vec![Vec::new(); patterns.len()];
        collect_pattern_hits(&window, &patterns, 0x1000, window.len(), &mut hits);
        assert_eq!(hits[0], vec![0x1000 + 0x10]);
        assert_eq!(hits[1], vec![0x1000 + 0x200]);

        let mut empty = vec![Vec::new(); patterns.len()];
        collect_pattern_hits(&vec![0u8; 0x400], &patterns, 0, 0x400, &mut empty);
        assert!(empty.iter().all(Vec::is_empty));
    }

    #[test]
    fn scan_file_patterns_reports_each_match_once() {
        // Overlapping windows must not double-count a match that lands inside
        // the carried-over tail.
        let offsets = [
            0usize,
            SCAN_CHUNK_BYTES - READ_PAT_T.len(),
            SCAN_CHUNK_BYTES,
            SCAN_CHUNK_BYTES + 0x800,
        ];
        let mut img = vec![0u8; SCAN_CHUNK_BYTES * 2];
        for &off in &offsets {
            img[off..off + READ_PAT_T.len()].copy_from_slice(READ_PAT_T);
        }
        with_static_ssl_fixture("once", &img, |path| {
            let hits = scan_file_patterns(path, &[READ_PAT_T]).expect("scan must succeed");
            assert_eq!(hits[0], offsets.to_vec());
        });
    }

    // ── parse_maps_line ─────────────────────────────────────────────────────
    //
    // Replaces the procfs crate's maps parsing, so it carries the coverage the
    // crate used to provide. Fixtures match the kernel's show_map_vma output:
    // five whitespace-separated fields (addr, perms, offset, dev, inode), then
    // an optional pathname that is the *entire remainder* of the line and may
    // itself contain spaces.

    #[test]
    fn maps_line_file_backed_mapping() {
        let line = "7f2f0a000000-7f2f0a028000 r--p 00000000 fd:01 2621443 \
                    /usr/lib64/libssl.so.1.1.1k";
        assert_eq!(
            parse_maps_line(line),
            Some((
                (0x7f2f0a000000, 0x7f2f0a028000),
                (0xfd_0000_0001, 2621443),
                "/usr/lib64/libssl.so.1.1.1k"
            ))
        );
    }

    #[test]
    fn maps_line_anonymous_mapping_ends_at_inode() {
        // Anonymous vmas print neither a pathname nor trailing whitespace.
        let line = "7f2f09e00000-7f2f09e21000 rw-p 00000000 00:00 0";
        assert_eq!(
            parse_maps_line(line),
            Some(((0x7f2f09e00000, 0x7f2f09e21000), (0, 0), ""))
        );
        // Trailing whitespace without a pathname behaves the same.
        let padded = "7f2f09e00000-7f2f09e21000 rw-p 00000000 00:00 0   ";
        assert_eq!(
            parse_maps_line(padded),
            Some(((0x7f2f09e00000, 0x7f2f09e21000), (0, 0), ""))
        );
    }

    #[test]
    fn maps_line_deleted_suffix_is_part_of_the_path() {
        // The suffix belongs to the pathname verbatim; the caller strips it
        // when choosing the attach target.
        let line = "7f2f09c00000-7f2f09c28000 r-xp 00028000 fd:01 2621443 \
                    /usr/lib64/libssl.so.1.1.1k (deleted)";
        assert_eq!(
            parse_maps_line(line),
            Some((
                (0x7f2f09c00000, 0x7f2f09c28000),
                (0xfd_0000_0001, 2621443),
                "/usr/lib64/libssl.so.1.1.1k (deleted)"
            ))
        );
    }

    #[test]
    fn maps_line_pseudo_path_survives_untouched() {
        // [stack]/[heap]-style entries parse with a non-slash path; the caller
        // filters them out, the parser must not mangle them.
        let line = "7fffb6c40000-7fffb6c61000 rw-p 00000000 00:00 0   [stack]";
        assert_eq!(
            parse_maps_line(line),
            Some(((0x7fffb6c40000, 0x7fffb6c61000), (0, 0), "[stack]"))
        );
    }

    #[test]
    fn maps_line_path_with_spaces_kept_whole() {
        let line = "7f2f09a00000-7f2f09a10000 r--p 00000000 fd:01 1048577 \
                    /opt/my app (copy)/libssl.so";
        assert_eq!(
            parse_maps_line(line),
            Some((
                (0x7f2f09a00000, 0x7f2f09a10000),
                (0xfd_0000_0001, 1048577),
                "/opt/my app (copy)/libssl.so"
            ))
        );
    }

    #[test]
    fn maps_line_malformed_input_yields_none() {
        // Fewer than five fields.
        assert_eq!(
            parse_maps_line("7f2f0a000000-7f2f0a028000 r--p 00000000"),
            None
        );
        // Garbage in the inode field.
        let bad_inode = "7f2f0a000000-7f2f0a028000 r--p 00000000 fd:01 xx /usr/lib/libssl.so";
        assert_eq!(parse_maps_line(bad_inode), None);
        // Garbage in the device field: without a valid device the key cannot
        // tell filesystems apart, so the line is unusable.
        let bad_dev = "7f2f0a000000-7f2f0a028000 r--p 00000000 zz:01 42 /usr/lib/libssl.so";
        assert_eq!(parse_maps_line(bad_dev), None);
        // Not a maps line at all.
        assert_eq!(parse_maps_line("rubbish"), None);
        assert_eq!(parse_maps_line(""), None);
    }

    #[test]
    fn ssl_libs_from_maps_survives_non_utf8_pathname() {
        // Linux pathnames are arbitrary bytes. One non-UTF-8 mapped pathname
        // must not fail the whole maps read: the pre-fix `read_to_string` made
        // `attach_process` return Err and silently drop every SSL library for
        // the process, losing all of its coverage.
        let mut maps = Vec::new();
        maps.extend_from_slice(
            b"7f2f09000000-7f2f09010000 r--p 00000000 08:01 4242 /opt/\xff\xfe-agent\n",
        );
        maps.extend_from_slice(
            b"7f2f0a000000-7f2f0a028000 r-xp 00028000 fd:01 2621443 /usr/lib64/libssl.so.3\n",
        );

        let libs = ssl_libs_from_maps_text(&maps, 4242);
        assert_eq!(libs.len(), 1, "the libssl mapping must still be discovered");
        assert_eq!(libs[0].2, SslLibKind::OpenSsl);
        assert!(
            libs[0].0.ends_with("/usr/lib64/libssl.so.3"),
            "attach path resolves through the pid root: {}",
            libs[0].0
        );
    }

    fn exe_file_key(path: &Path) -> FileKey {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).expect("stat synthetic executable");
        (stat_dev_to_key(meta.dev()), meta.ino())
    }

    #[test]
    fn attached_static_exe_remains_a_reattach_candidate() {
        with_static_ssl_fixture(
            "reattach-candidate",
            b"junk\x00../../vendor/boringssl/ssl/mem_internal.h\x00tail",
            |path| {
                let exe = Path::new(path);
                let key = exe_file_key(exe);
                let traced = HashSet::from([key]);
                let attachments = HashMap::from([(
                    key,
                    InodeAttach {
                        _links: Vec::new(),
                        attached_at: Instant::now(),
                    },
                )]);
                fs::write(exe, b"positive cache must avoid another marker scan")
                    .expect("replace fixture contents");

                let entry = probe_exe_static_ssl(4242, exe, &traced, &attachments)
                    .expect("an attached static executable must reach the re-attach decision");
                assert_eq!(entry.0, path);
                assert_eq!(entry.1, key);
                assert_eq!(entry.2, SslLibKind::StaticExeProbe);
            },
        );
    }

    #[test]
    fn untraceable_static_exe_stays_negatively_cached() {
        with_static_ssl_fixture(
            "negative-cache",
            b"junk\x00../../vendor/boringssl/ssl/mem_internal.h\x00tail",
            |path| {
                let exe = Path::new(path);
                let key = exe_file_key(exe);
                let traced = HashSet::from([key]);
                let attachments = HashMap::new();
                assert!(probe_exe_static_ssl(4242, exe, &traced, &attachments).is_none());
            },
        );
    }

    #[test]
    fn exe_markers_are_boringssl_only() {
        // Policy decision: the exe content probe only admits BoringSSL
        // shapes (Bun/Node-family binaries). Statically-linked OpenSSL
        // stays opt-in via the name-based fast path. Pin the marker set so
        // widening it is a visible, deliberate change. The marker itself is
        // stored reversed in non-test code so the production binary never
        // embeds the contiguous string (self-match guard).
        assert_eq!(ssl_exe_markers(), vec![b"boringssl".to_vec()]);
    }

    #[test]
    fn exe_marker_prefilter_detects_ssl_strings() {
        // BoringSSL builds keep vendor paths in .rodata even when stripped
        // (verified against a real Bun-compiled opencode binary): the
        // prefilter must accept them without knowing the binary's name.
        let dir = std::env::temp_dir();
        let hit_path = dir.join(format!("agentsight-marker-hit-{}", std::process::id()));
        let miss_path = dir.join(format!("agentsight-marker-miss-{}", std::process::id()));
        fs::write(
            &hit_path,
            b"junk\x00../../vendor/boringssl/ssl/mem_internal.h\x00tail",
        )
        .unwrap();
        fs::write(&miss_path, b"just a plain binary blob, no tls here").unwrap();

        let hit = hit_path.to_string_lossy().into_owned();
        let miss = miss_path.to_string_lossy().into_owned();
        assert!(
            file_has_ssl_markers(&hit),
            "boringssl vendor path must match"
        );
        assert!(!file_has_ssl_markers(&miss), "plain file must not match");
        assert!(
            !file_has_ssl_markers("/nonexistent/agentsight-marker"),
            "unreadable file must be a miss, not a panic"
        );

        let _ = fs::remove_file(&hit_path);
        let _ = fs::remove_file(&miss_path);
    }

    #[test]
    fn exe_marker_prefilter_sees_marker_straddling_window_boundary() {
        // The scan carries only max_marker_len - 1 bytes across windows: a
        // marker placed exactly on the SCAN_CHUNK_BYTES boundary must still
        // be found, otherwise the prefilter silently drops real SSL binaries
        // depending on where the linker happened to place .rodata.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("agentsight-marker-edge-{}", std::process::id()));
        let marker = b"boringssl";
        let mut data = vec![b'x'; SCAN_CHUNK_BYTES + marker.len()];
        let split = SCAN_CHUNK_BYTES - marker.len() / 2;
        data[split..split + marker.len()].copy_from_slice(marker);
        fs::write(&path, &data).unwrap();

        assert!(
            file_has_ssl_markers(&path.to_string_lossy()),
            "marker straddling the window boundary must match"
        );
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn stat_dev_to_key_matches_maps_encoding() {
        // The exe-probe FileKey comes from stat(2), the lib FileKeys from
        // maps maj:min fields: if the encodings diverge, the same binary is
        // deduped twice and its byte-pattern scan runs once per attach.
        for (majmin, dev_t) in [
            ("8:1", libc::makedev(8, 1) as u64),
            ("fd:1", libc::makedev(0xfd, 0x1) as u64),
            ("0:2a", libc::makedev(0, 0x2a) as u64),
            ("108:3", libc::makedev(0x108, 0x3) as u64),
        ] {
            assert_eq!(
                stat_dev_to_key(dev_t),
                parse_maps_dev(majmin).unwrap(),
                "stat dev_t and maps encoding disagree for {majmin}"
            );
        }
    }

    #[test]
    fn same_inode_on_different_devices_is_not_deduped() {
        // Inode numbers are unique per superblock, not globally: two files on
        // different filesystems sharing an inode number are different files.
        // Keying dedup on the bare inode treats the second one as already
        // traced and silently skips its uprobes, losing capture for every
        // process that maps only the second file.
        let maps: &[u8] =
            b"7f2f01000000-7f2f01010000 r-xp 00000000 08:01 42 /usr/lib64/libssl.so.3\n\
7f2f02000000-7f2f02010000 r-xp 00000000 fd:01 42 /opt/other/libssl.so.3\n";
        let libs = ssl_libs_from_maps_text(maps, 4242);
        assert_eq!(
            libs.len(),
            2,
            "same inode on different devices must not be deduped"
        );
        let keys: Vec<FileKey> = libs.iter().map(|(_, key, _, _)| *key).collect();
        assert_eq!(
            keys,
            vec![(0x08_0000_0001, 42), (0xfd_0000_0001, 42)],
            "each library keeps its own (device, inode) identity"
        );
    }

    #[test]
    fn deleted_shared_library_attaches_through_its_mapped_range() {
        // A shared libssl unlinked while its processes keep running cannot be
        // attached via <pid>/exe: that file exports no SSL_* symbols, so the
        // attach failed, the file was unmarked, and every later sweep retried
        // the doomed attach while all TLS traffic of those processes stayed
        // invisible. <pid>/map_files/<start>-<end> keeps the mapped (unlinked)
        // inode reachable for exactly this case.
        let maps: &[u8] = b"7f2f0a000000-7f2f0a028000 r-xp 00028000 fd:01 2621443 \
                            /usr/lib64/libssl.so.3 (deleted)\n";
        let libs = ssl_libs_from_maps_text(maps, 4242);
        assert_eq!(libs.len(), 1);
        assert_eq!(libs[0].2, SslLibKind::OpenSsl);
        let attach_path = &libs[0].0;
        assert!(
            attach_path.contains("/map_files/"),
            "deleted shared library must attach through its mapped range, got {attach_path}"
        );
        assert!(
            attach_path.contains("map_files/7f2f0a000000-7f2f0a028000"),
            "the attach target must be the mapping's own range, got {attach_path}"
        );
        assert!(
            !attach_path.ends_with("/exe"),
            "the exe fallback is only for statically-linked binaries, got {attach_path}"
        );
    }

    // ─── cosh-ng tap detection tests (#3042, #3115) ──────────────────────

    #[test]
    fn classify_recognises_cosh_binaries_as_rustls() {
        for path in [
            "/usr/libexec/anolisa/cosh-ng/cosh-core",
            "/usr/libexec/anolisa/cosh-ng/cosh-shell",
            "/usr/bin/cosh-cli",
            "/usr/libexec/anolisa/cosh-ng/cosh-gateway",
        ] {
            assert_eq!(
                classify_ssl_lib(path),
                Some(SslLibKind::ExplicitTap),
                "{path} must be probed through its exported plaintext tap"
            );
        }
    }

    #[test]
    fn classify_keeps_existing_kinds_unchanged() {
        for path in ["/usr/bin/qodercli", "/opt/qoder/qodercli-1.0.14"] {
            assert_eq!(classify_ssl_lib(path), Some(SslLibKind::Static));
        }
        // Guards against the rustls arm shadowing an established classification.
        assert_eq!(
            classify_ssl_lib("/usr/lib64/libssl.so.3"),
            Some(SslLibKind::OpenSsl)
        );
        assert_eq!(
            classify_ssl_lib("/usr/lib64/libgnutls.so.30"),
            Some(SslLibKind::GnuTls)
        );
        assert_eq!(classify_ssl_lib("/usr/bin/node"), Some(SslLibKind::Static));
        assert_eq!(classify_ssl_lib("/usr/bin/codex"), Some(SslLibKind::Static));
        // `/usr/bin/cosh` is a symlink; maps always reports the resolved target,
        // so the bare name is deliberately NOT matched.
        assert_eq!(classify_ssl_lib("/usr/bin/cosh"), None);
        assert_eq!(classify_ssl_lib("/usr/lib64/libc.so.6"), None);
    }

    #[test]
    fn cosh_tap_symbol_matches_the_exported_contract() {
        // The name is a cross-component contract with cosh-core's
        // `provider::observe`; renaming either side silently stops capture, so
        // pin the literal here rather than only in the attach call.
        assert_eq!(COSH_TAP_SYMBOL, "cosh_llm_plaintext_tap");
    }

    /// A pre-tap cosh-ng deployed under an upgraded AgentSight loses coverage
    /// with no TLS-layer fallback available, so only `cosh-core` may report the
    /// missing symbol as a problem. Warning for the siblings, which never carry
    /// the tap, would emit a line per process start on every healthy host.
    #[test]
    fn only_the_llm_binary_treats_a_missing_tap_as_a_coverage_gap() {
        assert!(missing_tap_is_a_coverage_gap(
            "/usr/libexec/anolisa/cosh-ng/cosh-core"
        ));
        // Still the LLM binary once its file has been unlinked mid-run.
        assert!(missing_tap_is_a_coverage_gap(
            "/usr/libexec/anolisa/cosh-ng/cosh-core (deleted)"
        ));
        for sibling in [
            "/usr/libexec/anolisa/cosh-ng/cosh-shell",
            "/usr/bin/cosh-cli",
            "/usr/libexec/anolisa/cosh-ng/cosh-gateway",
        ] {
            assert!(
                !missing_tap_is_a_coverage_gap(sibling),
                "{sibling} never carries the tap; a missing symbol is expected there"
            );
        }
    }

    #[test]
    fn cosh_core_keeps_its_coverage_gap_flag_through_the_attach_path() {
        // The coverage-gap warning fires on the value the attach caller holds.
        // Discovery resolves every ExplicitTap binary to <pid>/exe (the kernel
        // symlink that stays valid even when the backing file is replaced), so
        // the attach path alone can never identify cosh-core: its file name is
        // always "exe" and the operator-facing warning never fired. The mapped
        // pathname must therefore travel alongside the attach path.
        let maps: &[u8] =
            b"55f0c0d00000-55f0c0e00000 r-xp 00000000 fd:01 42 /opt/cosh-ng/cosh-core\n";
        let libs = ssl_libs_from_maps_text(maps, 4242);
        assert_eq!(libs.len(), 1);
        let (attach_path, _file, kind, mapped_path) = &libs[0];
        assert_eq!(*kind, SslLibKind::ExplicitTap);
        assert!(
            attach_path.ends_with("/exe"),
            "cosh-core attaches through <pid>/exe, got {attach_path}"
        );
        assert!(
            !missing_tap_is_a_coverage_gap(attach_path),
            "the generic exe attach path must not be mistaken for an identity"
        );
        assert!(
            missing_tap_is_a_coverage_gap(mapped_path),
            "the mapped pathname is what the coverage-gap decision must use, got {mapped_path}"
        );
    }

    #[test]
    fn first_pid_attach_forces_reattach_before_ttl() {
        let attached_at = Instant::now();
        let attachment = InodeAttach {
            _links: Vec::new(),
            attached_at,
        };
        let recent = attached_at + Duration::from_secs(1);
        assert!(!should_reattach(
            &attachment,
            recent,
            Duration::from_secs(30),
            false
        ));
        assert!(should_reattach(
            &attachment,
            recent,
            Duration::from_secs(30),
            true
        ));
        assert!(should_reattach(
            &attachment,
            attached_at + Duration::from_secs(30),
            Duration::from_secs(30),
            false
        ));
        assert!(should_reattach(
            &attachment,
            recent,
            Duration::from_secs(0),
            false
        ));
    }

    #[test]
    fn stale_reattach_ttl_default_and_override() {
        assert_eq!(parse_reattach_ttl(None), Duration::from_secs(30));
        assert_eq!(parse_reattach_ttl(Some("60")), Duration::from_secs(60));
        assert_eq!(parse_reattach_ttl(Some("0")), Duration::from_secs(0));
        assert_eq!(
            parse_reattach_ttl(Some("not-a-number")),
            Duration::from_secs(30)
        );
    }
}

#[cfg(test)]
mod paced_scan_tests {
    use super::*;

    #[test]
    fn cancelled_admission_stops_before_reading_a_window() {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), b"signature").unwrap();
        assert!(
            scan_file_patterns_admitted(
                input.path().to_str().unwrap(),
                &[b"signature"],
                || None::<()>
            )
            .is_none()
        );
    }
}
