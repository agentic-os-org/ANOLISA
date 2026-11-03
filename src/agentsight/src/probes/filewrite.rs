// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// File write probe - monitors vfs_write for JSON data from traced processes

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
    include!(concat!(env!("OUT_DIR"), "/filewrite.skel.rs"));
    include!(concat!(env!("OUT_DIR"), "/filewrite.rs"));
}
use bpf::*;

// Re-export raw type for size calculation in probes.rs
pub type RawFileWriteEvent = bpf::filewrite_event;

/// User-space file write event
#[derive(Debug, Clone)]
pub struct FileWriteEvent {
    pub pid: u32,
    pub tid: u32,
    pub uid: u32,
    pub timestamp_ns: u64,
    pub write_size: u32,
    pub comm: String,
    pub filename: String,
    pub cgroup_id: u64,
    pub buf: Vec<u8>,
}

impl FileWriteEvent {
    /// Parse event from raw ring buffer data
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let event_size = std::mem::size_of::<RawFileWriteEvent>();
        if data.len() < event_size {
            return None;
        }

        // SAFETY: BPF guarantees proper alignment and layout
        let raw = unsafe { &*(data.as_ptr() as *const RawFileWriteEvent) };

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

        // Copy only the actual data, not the full 16KB buffer
        let buf_size = raw.buf_size as usize;
        let buf_size = buf_size.min(raw.buf.len());
        let buf = raw.buf[..buf_size].to_vec();

        Some(FileWriteEvent {
            pid: raw.pid,
            tid: raw.tid,
            uid: raw.uid,
            timestamp_ns: config::ktime_to_unix_ns(raw.timestamp_ns)
                .inspect_err(|error| config::report_clock_error("filewrite", error))
                .ok()?,
            write_size: raw.write_size,
            comm,
            filename,
            cgroup_id: raw.cgroup_id,
            buf,
        })
    }
}

// ─── Main struct ──────────────────────────────────────────────────────────────
pub struct FileWrite {
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<FilewriteSkel<'static>>,
    _links: Vec<Link>,
}

/// Maps filewrite reuses from the shared bundle: ring buffer, process filter,
/// and (when cgroup filtering is enabled) the cgroup filter.
const SHARED_MAPS: &[MapKind] = &[
    MapKind::Rb,
    MapKind::InternalMetrics,
    MapKind::TracedProcesses,
    MapKind::CgroupFilter,
];

impl FileWrite {
    /// Create a new FileWrite that reuses the shared maps bundle.
    ///
    /// Reuses the ring buffer, internal counters, and process filter; the
    /// cgroup filter is reused only when cgroup filtering is enabled.
    pub fn new_with_shared(shared: &SharedMaps) -> Result<Self> {
        let mut builder = FilewriteSkelBuilder::default();
        builder.obj_builder.debug(config::verbose());

        let open_object = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let mut open_skel = builder
            .open()
            .context("failed to open filewrite BPF object")?;

        // Cgroup filter flag
        open_skel.rodata_mut().filter_cgroup_enabled = shared.cgroup_filter_enabled();

        // Tell BPF which namespace to report event pids in.
        open_skel.rodata_mut().observer_pidns_is_init = proc_root_is_init_pidns();

        // Detect cgroup v2 and pass to BPF via rodata.
        open_skel.rodata_mut().cgroup_v2_mode =
            std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists();

        // Reuse the shared maps (cgroup_filter is skipped when not shared).
        shared
            .reuse_into(SHARED_MAPS, open_skel.open_object_mut())
            .context("failed to reuse shared maps for filewrite")?;

        let skel = open_skel
            .load()
            .context("failed to load filewrite BPF object")?;

        // SAFETY: skel borrows open_object which lives in a Box<MaybeUninit>
        let skel =
            unsafe { Box::from_raw(Box::into_raw(Box::new(skel)) as *mut FilewriteSkel<'static>) };

        Ok(Self {
            _open_object: open_object,
            skel,
            _links: Vec::new(),
        })
    }

    /// Attach fentry program for vfs_write monitoring
    pub fn attach(&mut self) -> Result<()> {
        let mut links = Vec::new();

        let link = self
            .skel
            .progs_mut()
            .trace_vfs_write()
            .attach()
            .context("failed to attach fentry/vfs_write")?;
        links.push(link);

        self._links = links;
        Ok(())
    }
}
