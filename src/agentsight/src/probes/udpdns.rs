// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// UDP DNS probe - captures domain names from DNS query packets
// by hooking udp_sendmsg and filtering for destination port 53.
//
// Design: BPF kernel side only does minimal filtering and raw payload capture.
// All DNS QNAME parsing and deduplication is done here in userspace.

use crate::config;
use anyhow::{Context, Result};
use libbpf_rs::{
    Link,
    skel::{OpenSkel, SkelBuilder},
};
use std::mem::MaybeUninit;

use super::pidns::proc_root_is_init_pidns;
use super::shared_maps::{MapKind, SharedMaps};

// --- Generated skeleton ---
#[allow(
    non_camel_case_types,
    non_upper_case_globals,
    dead_code,
    non_snake_case
)]
mod bpf {
    include!(concat!(env!("OUT_DIR"), "/udpdns.skel.rs"));
    include!(concat!(env!("OUT_DIR"), "/udpdns.rs"));
}
use bpf::*;

// Re-export raw type for size calculation in probes.rs
pub type RawUdpDnsEvent = bpf::udpdns_event;

/// DNS header length in bytes
const DNS_HEADER_LEN: usize = 12;
/// Maximum domain name length (RFC 1035: 253 chars for FQDN)
const MAX_DOMAIN_LEN: usize = 253;
/// Maximum label length per RFC 1035
const MAX_LABEL_LEN: usize = 63;

/// User-space UDP DNS event
#[derive(Debug, Clone)]
pub struct UdpDnsEvent {
    pub pid: u32,
    pub tid: u32,
    pub uid: u32,
    pub timestamp_ns: u64,
    pub comm: String,
    pub domain: String,
}

/// Parse DNS wire-format QNAME from raw payload into dotted domain string.
///
/// DNS wire format: sequence of (length_byte, label_bytes...) terminated by 0x00.
/// Example: \x03api\x06openai\x03com\x00 → "api.openai.com"
fn parse_dns_qname(payload: &[u8], payload_len: usize) -> Option<String> {
    if payload_len < DNS_HEADER_LEN + 2 {
        return None;
    }

    let data = &payload[..payload_len];
    let mut off = DNS_HEADER_LEN; // QNAME starts after 12-byte DNS header
    let mut domain = String::with_capacity(64);

    loop {
        if off >= data.len() {
            break;
        }

        let label_len = data[off] as usize;

        // Root label (terminator)
        if label_len == 0 {
            break;
        }

        // Pointer (compression) — not expected in queries but bail out safely
        if label_len & 0xC0 != 0 {
            break;
        }

        // RFC 1035: label max 63 bytes
        if label_len > MAX_LABEL_LEN {
            break;
        }

        off += 1;

        // Check we have enough bytes for this label
        if off + label_len > data.len() {
            break;
        }

        // Add dot separator between labels
        if !domain.is_empty() {
            domain.push('.');
        }

        // Append label bytes
        let label_bytes = &data[off..off + label_len];
        // DNS labels should be ASCII; use lossy conversion for safety
        for &b in label_bytes {
            domain.push(b as char);
        }

        off += label_len;

        // Safety: prevent infinite/oversized domains
        if domain.len() > MAX_DOMAIN_LEN {
            break;
        }
    }

    if domain.is_empty() {
        None
    } else {
        Some(domain)
    }
}

impl UdpDnsEvent {
    /// Parse event from raw ring buffer data.
    /// Performs DNS QNAME extraction from the raw payload in userspace.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let event_size = std::mem::size_of::<RawUdpDnsEvent>();
        if data.len() < event_size {
            return None;
        }

        // SAFETY: BPF guarantees proper alignment and layout
        let raw = unsafe { &*(data.as_ptr() as *const RawUdpDnsEvent) };

        // Parse comm (null-terminated)
        let comm = raw
            .comm
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect::<Vec<u8>>();
        let comm = String::from_utf8_lossy(&comm).into_owned();

        // Parse DNS QNAME from raw payload (userspace parsing — no BPF verifier limits)
        let payload_len = raw.payload_len as usize;
        let payload_len = payload_len.min(raw.payload.len());
        let domain = parse_dns_qname(&raw.payload, payload_len)?;

        Some(UdpDnsEvent {
            pid: raw.pid,
            tid: raw.tid,
            uid: raw.uid,
            timestamp_ns: config::ktime_to_unix_ns(raw.timestamp_ns)
                .inspect_err(|error| config::report_clock_error("udpdns", error))
                .ok()?,
            comm,
            domain,
        })
    }
}

// --- Main struct ---
pub struct UdpDns {
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<UdpdnsSkel<'static>>,
    _links: Vec<Link>,
}

/// Maps udpdns reuses from the shared bundle: ring buffer + process filter
/// (used to skip already-traced processes). No cgroup filter.
const SHARED_MAPS: &[MapKind] = &[
    MapKind::Rb,
    MapKind::InternalMetrics,
    MapKind::TracedProcesses,
];

impl UdpDns {
    /// Reuse the shared ring buffer, internal counters, and process filter.
    ///
    /// # Arguments
    /// * `shared` - Bundle with ring buffer, counters, and process filter
    pub fn new_with_shared(shared: &SharedMaps) -> Result<Self> {
        let mut builder = UdpdnsSkelBuilder::default();
        builder.obj_builder.debug(config::verbose());

        let open_object = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let mut open_skel = builder.open().context("failed to open udpdns BPF object")?;

        // Tell BPF which namespace to report event pids in.
        open_skel.rodata_mut().observer_pidns_is_init = proc_root_is_init_pidns();

        // Reuse shared ring buffer, counters, and process filter.
        shared
            .reuse_into(SHARED_MAPS, open_skel.open_object_mut())
            .context("failed to reuse shared maps for udpdns")?;

        let skel = open_skel
            .load()
            .context("failed to load udpdns BPF object")?;

        // SAFETY: skel borrows open_object which lives in a Box<MaybeUninit>
        let skel =
            unsafe { Box::from_raw(Box::into_raw(Box::new(skel)) as *mut UdpdnsSkel<'static>) };

        Ok(Self {
            _open_object: open_object,
            skel,
            _links: Vec::new(),
        })
    }

    /// Attach fentry hook for udp_sendmsg
    pub fn attach(&mut self) -> Result<()> {
        let mut links = Vec::new();

        let link = self
            .skel
            .progs_mut()
            .trace_udp_sendmsg()
            .attach()
            .context("failed to attach udp_sendmsg fentry")?;
        links.push(link);

        self._links = links;
        Ok(())
    }
}
