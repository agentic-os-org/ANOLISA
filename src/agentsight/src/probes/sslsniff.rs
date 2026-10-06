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
    io::Read,
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
const POLL_TIMEOUT_MS: u64 = 100;

/// How long a global uprobe attachment is trusted before the next matching
/// process triggers a re-attach. The kernel can deregister uprobe consumers
/// without any userspace-visible error (observed on serverless/overlayfs
/// hosts), leaving the `Link` objects alive but the probes silent. Userspace
/// cannot query liveness, so re-attach on TTL expiry is the recovery path.
///
/// 300s was too coarse for short-lived processes (e.g. a `qwen -p` headless
/// call lives for seconds): a process that execs within the window after a
/// silent deregistration saw the stale attachment and skipped the re-attach,
/// so its traffic was silently missed (#3034). 30s shrinks the window by 10x;
/// the churn cost is bounded by the number of distinct SSL library inodes,
/// not by the exec rate.
const STALE_REATTACH_TTL: Duration = Duration::from_secs(30);

/// Effective stale re-attach TTL, resolved once per `SslSniff` at
/// construction. `AGENTSIGHT_SSL_REATTACH_TTL_SECS` overrides the default
/// (0 forces a re-attach on every matching exec; used by tests).
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

pub struct SslSniff {
    // We store the skel behind a Box so we can hold it alongside the
    // links without lifetime trouble.  The MaybeUninit holds the
    // OpenObject allocation that the skeleton borrows from.
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<SslsniffSkel<'static>>,
    /// Per-file uprobe links with attach metadata (for stale re-attach).
    attachments: HashMap<FileKey, InodeAttach>,
    traced_files: HashSet<FileKey>,
    /// Maps pid -> files that were attached for this pid.
    /// Used to clean up traced_files when the process exits.
    pid_inodes: HashMap<u32, Vec<FileKey>>,
    /// Stale re-attach TTL, resolved once at construction from the
    /// `AGENTSIGHT_SSL_REATTACH_TTL_SECS` override (or the default).
    reattach_ttl: Duration,
    /// How many stale attachments have been re-attached (diagnostics/tests).
    stale_reattachs: u64,
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
            traced_files: HashSet::default(),
            pid_inodes: HashMap::default(),
            reattach_ttl: stale_reattach_ttl(),
            stale_reattachs: 0,
            tx,
            rx,
        })
    }

    /// Attach SSL probes to a running process by reading its `/proc/<pid>/maps`.
    ///
    /// Detects which SSL libraries the process has mapped (OpenSSL, GnuTLS, NSS,
    /// statically-linked SSL — BoringSSL/OpenSSL — or rustls), attaches uprobes,
    /// and skips any library whose `(device, inode)` identity has already been
    /// traced (dedup via `traced_files`) —
    /// unless that attachment is stale (older than the re-attach TTL), in
    /// which case the probes are re-attached and the old links are dropped
    /// only after the replacement succeeds.
    pub fn attach_process(&mut self, pid: i32) -> Result<()> {
        let libs = ssl_libs_from_maps(pid)?;
        if libs.is_empty() {
            log::warn!("[attach_process] pid={pid}: no SSL libraries found in maps");
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
        let mut attached_files: Vec<FileKey> = Vec::new();
        for (path, file, kind, mapped_path) in libs {
            // Dedup by (device, inode): with pid=-1 global attach each library
            // only needs to be attached once — the kernel's uprobe_mmap
            // mechanism installs breakpoints for new processes that map an
            // already-registered file, including statically-linked SSL
            // binaries (codex, node, etc). But the kernel can also silently
            // deregister uprobe consumers (observed on serverless/overlayfs
            // hosts), leaving the probes silent with no error. Userspace
            // cannot query liveness, so treat attachments older than the TTL
            // as stale and re-attach.
            if !self.traced_files.insert(file) {
                let stale = match self.attachments.get(&file) {
                    Some(a) => now.duration_since(a.attached_at) >= self.reattach_ttl,
                    // Marked but never attached: an Untraceable binary. Nothing
                    // to re-attach; rescanning it every sweep is wasted work.
                    None => false,
                };
                if !stale {
                    log::debug!("[attach_process] pid={pid}: skipping already-traced {path}");
                    // Still record the pid→file association so detach_process
                    // can track all pids referencing this file.
                    attached_files.push(file);
                    continue;
                }
                let age_secs = self
                    .attachments
                    .get(&file)
                    .map(|a| now.duration_since(a.attached_at).as_secs());
                // Build the replacement BEFORE dropping the old links: if the
                // re-attach fails, the previous (possibly still working) probes
                // stay active instead of leaving the library untraced.
                match self.build_attach(pid, &path, kind, &mapped_path) {
                    AttachOutcome::Attached(links) => {
                        log::warn!(
                            "[attach_process] pid={pid}: re-attached stale {kind:?} uprobe on {path} ({}s old)",
                            age_secs.unwrap_or(0)
                        );
                        self.record_attach(file, links);
                        self.stale_reattachs += 1;
                        attached_files.push(file);
                    }
                    AttachOutcome::Untraceable | AttachOutcome::Failed => {
                        log::warn!(
                            "[attach_process] pid={pid}: stale re-attach failed for {path}; keeping previous links"
                        );
                        attached_files.push(file);
                    }
                }
                continue;
            }

            match self.build_attach(pid, &path, kind, &mapped_path) {
                AttachOutcome::Attached(links) => {
                    self.record_attach(file, links);
                    attached_files.push(file);
                }
                AttachOutcome::Untraceable => {
                    // Not traceable on this host; leave the file marked so we
                    // do not repeat the (expensive) detection on every sweep.
                }
                AttachOutcome::Failed => {
                    // Attach failed: drop the file so a later retry can succeed.
                    self.traced_files.remove(&file);
                }
            }
        }

        // Record files attached for this pid so we can clean up on process exit
        if !attached_files.is_empty() {
            self.pid_inodes.insert(pid as u32, attached_files);
        }

        Ok(())
    }

    /// Number of SSL library files with live uprobe attachments (diagnostics).
    pub fn traced_inode_count(&self) -> usize {
        self.attachments.len()
    }

    /// How many stale attachments have been replaced by a re-attach
    /// (diagnostics/tests).
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
        path: &str,
        kind: SslLibKind,
        mapped_path: &str,
    ) -> AttachOutcome {
        log::debug!("[attach_process] pid={pid}: attaching {kind:?} → {path}");

        // Use pid=-1 for global attach (all processes), avoiding per-process duplicate attaches
        let result = match kind {
            SslLibKind::OpenSsl => attach_openssl(&mut self.skel, path, -1),
            SslLibKind::GnuTls => attach_gnutls(&mut self.skel, path, -1),
            SslLibKind::Nss => attach_nss(&mut self.skel, path, -1),
            SslLibKind::ExplicitTap => {
                if !cosh_tap_present(path) {
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
            SslLibKind::Static => {
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
                        match find_static_ssl_offsets(path) {
                            Some(off) => {
                                attach_static_ssl_by_offset(&mut self.skel, path, &off, false, -1)
                            }
                            None => {
                                log::warn!(
                                    "[attach_process] pid={pid}: SSL detection failed for {path} \
                                 (no SSL_* in .dynsym, not in the codex offset table, and no byte-pattern match), skipping"
                                );
                                return AttachOutcome::Untraceable;
                            }
                        }
                    }
                }
            }
        };

        match result {
            Ok(links) => AttachOutcome::Attached(links),
            Err(e) => {
                log::warn!("[attach_process] pid={pid}: attach failed for {path}: {e:#}");
                AttachOutcome::Failed
            }
        }
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
                    // Drop the attachment too: releasing the Links detaches the
                    // (now unneeded) global uprobes and keeps `attachments`
                    // consistent with `traced_files`. A later process mapping
                    // this file re-attaches through the fresh-attach path.
                    self.attachments.remove(file);
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

        let handle = thread::Builder::new()
            .name("sslsniff-poll".into())
            .spawn(move || {
                let timeout = Duration::from_millis(POLL_TIMEOUT_MS);
                loop {
                    if stop_flag_inner.load(Ordering::Relaxed) {
                        break;
                    }
                    match rb.poll(timeout) {
                        Ok(_) => {}
                        Err(e) if e.kind() == libbpf_rs::ErrorKind::Interrupted => break,
                        Err(e) => {
                            eprintln!("sslsniff poll error: {e:#}");
                            break;
                        }
                    }
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

/// Appends the absolute offset of every pattern match starting in `window`
/// below `limit`, one vector per entry of `patterns`, in ascending order.
///
/// Walks the window once and tests every pattern at each position, rather than
/// walking it once per pattern. The signatures are all function prologues that
/// share a leading byte run, so each comparison rejects a position on its first
/// byte and the extra signatures cost close to nothing. Walking per pattern
/// instead means a binary matching none of them is scanned once for every
/// signature before it can be rejected, on the synchronous attach path.
fn collect_pattern_hits(
    window: &[u8],
    patterns: &[&[u8]],
    base: usize,
    limit: usize,
    hits: &mut [Vec<usize>],
) {
    for pos in 0..limit.min(window.len()) {
        let tail = &window[pos..];
        for (slot, pattern) in hits.iter_mut().zip(patterns) {
            if tail.len() >= pattern.len() && &tail[..pattern.len()] == *pattern {
                slot.push(base + pos);
            }
        }
    }
}

/// Bytes read per window by [`scan_file_patterns`].
///
/// Caps the scan's peak memory. Agent binaries reach hundreds of megabytes (a
/// Codex build is ~264 MiB, node ~120 MiB), more than the whole budget the
/// service runs under, so the image must never be held in one piece (#2981).
const SCAN_CHUNK_BYTES: usize = 4 * 1024 * 1024;

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
fn scan_file_patterns(path: &str, patterns: &[&[u8]]) -> Option<Vec<Vec<usize>>> {
    let overlap = patterns.iter().map(|p| p.len()).max()?.checked_sub(1)?;
    let mut file = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; overlap + SCAN_CHUNK_BYTES];
    let mut hits = vec![Vec::new(); patterns.len()];
    // Absolute offset that `buf[0]` maps to, and how many leading bytes of `buf`
    // are the previous window's carried-over tail.
    let mut base = 0usize;
    let mut carry = 0usize;
    loop {
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

/// Reads until `dst` is full or the file ends, returning the byte count.
///
/// `Read::read` is free to return short of the request before EOF, and a short
/// window would split a pattern that is actually present, so the fill has to be
/// driven to completion rather than trusted to one call.
///
/// # Errors
///
/// Returns `None` on any I/O error other than an interrupted call.
fn fill_buf(file: &mut fs::File, mut dst: &mut [u8]) -> Option<usize> {
    let mut total = 0;
    while !dst.is_empty() {
        match file.read(dst) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                dst = &mut dst[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(total)
}

fn find_static_ssl_offsets(path: &str) -> Option<StaticSslOffsets> {
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
    // Maximum distance between SSL_read and SSL_write in the same compilation unit.
    const ADJACENCY_THRESHOLD: usize = 0x1000; // 4KB
    let verbose = config::verbose();

    // One streaming pass collects every pattern at once: the file is read in
    // bounded windows, so a hundreds-of-megabytes binary costs a few MiB here
    // instead of its own size.
    let mut patterns: Vec<&[u8]> = vec![READ_PAT, WRITE_PAT];
    patterns.extend_from_slice(HANDSHAKE_PATS);
    let mut hits = scan_file_patterns(path, &patterns)?;
    // Drain the fixed slots high-index-first so the remaining vectors are
    // exactly the handshake variants.
    let write_matches = hits.remove(1);
    let read_matches = hits.remove(0);
    let handshake_hits = hits;

    // --- SSL_read: expect unique match ---
    if read_matches.is_empty() {
        if verbose {
            eprintln!("Static SSL: SSL_read pattern not found in {path}");
        }
        return None;
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
        return None;
    };

    // --- SSL_do_handshake: collect matches across all known variants ---
    let mut hs_matches: Vec<usize> = handshake_hits.into_iter().flatten().collect();
    if hs_matches.is_empty() {
        if verbose {
            eprintln!("Static SSL: SSL_do_handshake pattern not found in {path}");
        }
        return None;
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
                return None;
            }
        }
    };

    // --- SSL_write: adjacency verification ---
    if write_matches.is_empty() {
        if verbose {
            eprintln!("Static SSL: SSL_write pattern not found in {path}");
        }
        return None;
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
        })?;

    log::debug!("Static SSL detected in {path}:");
    log::debug!("  SSL_do_handshake: {hs_off:#x}");
    log::debug!("  SSL_read:         {read_off:#x}");
    log::debug!("  SSL_write:        {wr_off:#x}");

    Some(StaticSslOffsets {
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
const COSH_TAP_SYMBOL: &str = "cosh_llm_plaintext_tap";

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
fn cosh_tap_present(path: &str) -> bool {
    scan_file_patterns(path, &[COSH_TAP_SYMBOL.as_bytes()])
        .is_some_and(|hits| hits.first().is_some_and(|h| !h.is_empty()))
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
    /// libgnutls.so
    GnuTls,
    /// libnspr4.so (NSS / Firefox)
    Nss,
    /// Statically-linked SSL (BoringSSL or OpenSSL, e.g. Node.js, Chrome, Codex CLI)
    Static,
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
    // uv Python statically links OpenSSL into the binary. The ELF .symtab contains
    // SSL_write/SSL_read/SSL_do_handshake as LOCAL symbols, so attach_openssl()
    // (symbol-name uprobe) works directly. Only match python3.<ver> (with version
    // suffix) to avoid matching bare "python3" symlinks from system Python.
    if name.starts_with("python3.") {
        return Some(SslLibKind::OpenSsl);
    }
    None
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

use super::codex_offsets::OffsetTable;

static CODEX_OFFSET_TABLE: std::sync::LazyLock<Option<OffsetTable>> =
    std::sync::LazyLock::new(|| {
        let json = include_str!("../../agentsight.json");
        OffsetTable::load(json)
    });

#[cfg(test)]
mod tests {
    use super::*;

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

    /// The default re-attach TTL must cover short-lived processes (#3034).
    /// Discriminating: reverting the constant to 300s makes this fail.
    /// The env override must still work after the default change.
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
