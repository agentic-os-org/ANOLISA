// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// File watch probe - monitors openat syscalls for .jsonl files from traced processes

use crate::config;
use anyhow::{Context, Result};
use libbpf_rs::{
    Link,
    skel::{OpenSkel, SkelBuilder},
};
use std::mem::MaybeUninit;

use super::pidns::proc_root_is_init_pidns;
use super::shared_maps::{MapKind, SharedMaps};

// ─── Generated skeleton ───────────────────────────────────────────────────────
#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    non_snake_case
)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/filewatch.skel.rs"));
    include!(concat!(env!("OUT_DIR"), "/filewatch.rs"));
}
use bpf::*;

// Re-export raw type for size calculation in probes.rs
pub type RawFileWatchEvent = bpf::filewatch_event;

/// User-space file watch event
#[derive(Debug, Clone)]
pub struct FileWatchEvent {
    pub pid: u32,
    pub tid: u32,
    pub uid: u32,
    pub timestamp_ns: u64,
    pub flags: i32,
    pub comm: String,
    pub filename: String,
    pub cgroup_id: u64,
}

impl FileWatchEvent {
    /// Parse event from raw ring buffer data
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let event_size = std::mem::size_of::<RawFileWatchEvent>();
        if data.len() < event_size {
            return None;
        }

        // SAFETY: BPF guarantees proper alignment and layout
        let raw = unsafe { &*(data.as_ptr() as *const RawFileWatchEvent) };

        // Parse comm (null-terminated)
        let comm = raw
            .comm
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect::<Vec<u8>>();
        let comm = String::from_utf8_lossy(&comm).into_owned();

        // Parse filename (null-terminated)
        let filename = raw
            .filename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect::<Vec<u8>>();
        let filename = String::from_utf8_lossy(&filename).into_owned();

        Some(FileWatchEvent {
            pid: raw.pid,
            tid: raw.tid,
            uid: raw.uid,
            timestamp_ns: config::ktime_to_unix_ns(raw.timestamp_ns)
                .inspect_err(|error| config::report_clock_error("filewatch", error))
                .ok()?,
            flags: raw.flags,
            comm,
            filename,
            cgroup_id: raw.cgroup_id,
        })
    }
}

// ─── Main struct ──────────────────────────────────────────────────────────────
pub struct FileWatch {
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<FilewatchSkel<'static>>,
    _links: Vec<Link>,
}

/// Maps filewatch reuses from the shared bundle: ring buffer, process filter,
/// and (when cgroup filtering is enabled) the cgroup filter.
const SHARED_MAPS: &[MapKind] = &[
    MapKind::Rb,
    MapKind::InternalMetrics,
    MapKind::TracedProcesses,
    MapKind::CgroupFilter,
];

impl FileWatch {
    /// Create a new FileWatch that reuses the shared maps bundle.
    ///
    /// Reuses the ring buffer, internal counters, and process filter; the
    /// cgroup filter is reused only when cgroup filtering is enabled.
    pub fn new_with_shared(shared: &SharedMaps) -> Result<Self> {
        let mut builder = FilewatchSkelBuilder::default();
        builder.obj_builder.debug(config::verbose());

        let open_object = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let mut open_skel = builder
            .open()
            .context("failed to open filewatch BPF object")?;

        // Mirror the cgroup-filter rodata flag.
        open_skel.rodata_mut().filter_cgroup_enabled = shared.cgroup_filter_enabled();

        // Tell BPF which namespace to report event pids in.
        open_skel.rodata_mut().observer_pidns_is_init = proc_root_is_init_pidns();

        // Detect cgroup v2 and pass to BPF via rodata.
        open_skel.rodata_mut().cgroup_v2_mode =
            std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists();

        // Reuse the shared maps (cgroup_filter is skipped when not shared).
        shared
            .reuse_into(SHARED_MAPS, open_skel.open_object_mut())
            .context("failed to reuse shared maps for filewatch")?;

        let skel = open_skel
            .load()
            .context("failed to load filewatch BPF object")?;

        // SAFETY: skel borrows open_object which lives in a Box<MaybeUninit>
        let skel =
            unsafe { Box::from_raw(Box::into_raw(Box::new(skel)) as *mut FilewatchSkel<'static>) };

        Ok(Self {
            _open_object: open_object,
            skel,
            _links: Vec::new(),
        })
    }

    /// Attach tracepoint for file monitoring
    pub fn attach(&mut self) -> Result<()> {
        let mut links = Vec::new();

        let link = self
            .skel
            .progs_mut()
            .trace_openat_enter()
            .attach()
            .context("failed to attach openat tracepoint")?;
        links.push(link);

        self._links = links;
        Ok(())
    }
}
