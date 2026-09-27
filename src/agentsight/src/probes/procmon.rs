// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// Process monitor probe - lightweight process creation and exit monitoring

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
    include!(concat!(env!("OUT_DIR"), "/procmon.skel.rs"));
    include!(concat!(env!("OUT_DIR"), "/procmon.rs"));
}
use bpf::*;

// Re-export type from generated bindings
pub type ProcMonEvent = bpf::procmon_event;

// Event type constants
pub const PROCMON_EVENT_EXEC: u32 = 1;
pub const PROCMON_EVENT_EXIT: u32 = 2;

/// Parsed event from ring buffer
#[derive(Debug)]
pub enum Event {
    Exec {
        pid: u32,
        tid: u32,
        ppid: u32,
        uid: u32,
        timestamp_ns: u64,
        comm: String,
    },
    Exit {
        pid: u32,
        tid: u32,
        uid: u32,
        timestamp_ns: u64,
        comm: String,
        /// Raw `task_struct->exit_code` in wait(2) encoding; decode with
        /// [`crate::interruption::ProcessExitStatus::decode`].
        exit_code: u32,
    },
}

impl Event {
    /// Parse event from raw ring buffer data
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let event_size = std::mem::size_of::<ProcMonEvent>();
        if data.len() < event_size {
            return None;
        }

        // SAFETY: BPF guarantees proper alignment and layout
        let raw = unsafe { &*(data.as_ptr() as *const ProcMonEvent) };

        // Parse comm (null-terminated)
        let comm = raw
            .comm
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect::<Vec<u8>>();
        let comm = String::from_utf8_lossy(&comm).into_owned();

        match raw.event_type {
            PROCMON_EVENT_EXEC => Some(Event::Exec {
                pid: raw.pid,
                tid: raw.tid,
                ppid: raw.ppid,
                uid: raw.uid,
                timestamp_ns: config::ktime_to_unix_ns(raw.timestamp_ns)
                    .inspect_err(|error| config::report_clock_error("procmon", error))
                    .ok()?,
                comm,
            }),
            PROCMON_EVENT_EXIT => Some(Event::Exit {
                pid: raw.pid,
                tid: raw.tid,
                uid: raw.uid,
                timestamp_ns: config::ktime_to_unix_ns(raw.timestamp_ns)
                    .inspect_err(|error| config::report_clock_error("procmon", error))
                    .ok()?,
                comm,
                exit_code: raw.exit_code,
            }),
            _ => None,
        }
    }

    /// Get event type as string
    pub fn event_type_str(&self) -> &'static str {
        match self {
            Event::Exec { .. } => "exec",
            Event::Exit { .. } => "exit",
        }
    }

    /// Get process ID
    pub fn pid(&self) -> u32 {
        match self {
            Event::Exec { pid, .. } => *pid,
            Event::Exit { pid, .. } => *pid,
        }
    }

    /// Get process name
    pub fn comm(&self) -> &str {
        match self {
            Event::Exec { comm, .. } => comm,
            Event::Exit { comm, .. } => comm,
        }
    }
}

// ─── Main struct ──────────────────────────────────────────────────────────────
pub struct ProcMon {
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<ProcmonSkel<'static>>,
    _links: Vec<Link>,
}

/// Maps procmon reuses from the shared bundle. procmon keeps full audit
/// coverage, so it omits the process and cgroup filters.
const SHARED_MAPS: &[MapKind] = &[MapKind::Rb, MapKind::InternalMetrics];

impl ProcMon {
    /// Create a new ProcMon that reuses the shared ring buffer and counters.
    ///
    /// # Arguments
    /// * `shared` - Bundle providing the ring buffer and internal counters
    pub fn new_with_shared(shared: &SharedMaps) -> Result<Self> {
        // Open + load skeleton
        let mut builder = ProcmonSkelBuilder::default();
        builder.obj_builder.debug(config::verbose());

        let open_object = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let mut open_skel = builder.open().context("failed to open BPF object")?;

        // Tell BPF which namespace to report event pids in.
        open_skel.rodata_mut().observer_pidns_is_init = proc_root_is_init_pidns();

        // Reuse the shared ring buffer and its producer counters.
        shared
            .reuse_into(SHARED_MAPS, open_skel.open_object_mut())
            .context("failed to reuse shared maps for procmon")?;

        let skel = open_skel.load().context("failed to load BPF object")?;

        // SAFETY: skel borrows open_object which lives in a Box<MaybeUninit>
        let skel =
            unsafe { Box::from_raw(Box::into_raw(Box::new(skel)) as *mut ProcmonSkel<'static>) };

        Ok(Self {
            _open_object: open_object,
            skel,
            _links: Vec::new(),
        })
    }

    /// Attach tracepoints for process monitoring
    pub fn attach(&mut self) -> Result<()> {
        let mut links = Vec::new();

        // Attach execve exit tracepoint (after execve completes)
        let link = self
            .skel
            .progs_mut()
            .trace_execve_exit()
            .attach()
            .context("failed to attach execve exit tracepoint")?;
        links.push(link);

        // Attach process exit tracepoint
        let link = self
            .skel
            .progs_mut()
            .trace_process_exit()
            .attach()
            .context("failed to attach process exit tracepoint")?;
        links.push(link);

        self._links = links;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Locks the C/Rust shared layout: exit_code fills the tail padding after
    // comm[16], so the struct size must stay 56 bytes and any accidental
    // mid-struct insertion (which would shift exit_code away from offset 52)
    // fails here.
    #[test]
    fn test_procmon_event_layout() {
        assert_eq!(std::mem::size_of::<ProcMonEvent>(), 56);
        assert_eq!(std::mem::offset_of!(ProcMonEvent, comm), 36);
        assert_eq!(std::mem::offset_of!(ProcMonEvent, exit_code), 52);
    }

    #[test]
    fn test_exit_event_from_bytes_carries_exit_code() {
        // SAFETY: procmon_event is a plain-old-data C struct; all-zero is valid.
        let mut raw: ProcMonEvent = unsafe { std::mem::zeroed() };
        raw.timestamp_ns = 1;
        raw.pid = 42;
        raw.tid = 42;
        raw.ppid = 1;
        raw.event_type = PROCMON_EVENT_EXIT;
        raw.exit_code = 0x8b;
        // c_char is i8 on x86_64 but u8 on aarch64; let inference pick.
        for (i, &b) in b"cosh".iter().enumerate() {
            raw.comm[i] = b as _;
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&raw as *const ProcMonEvent) as *const u8,
                std::mem::size_of::<ProcMonEvent>(),
            )
        };
        match Event::from_bytes(bytes) {
            Some(Event::Exit {
                pid,
                comm,
                exit_code,
                ..
            }) => {
                assert_eq!(pid, 42);
                assert_eq!(comm, "cosh");
                assert_eq!(exit_code, 0x8b);
            }
            other => panic!("expected Exit event, got {other:?}"),
        }
    }
}
