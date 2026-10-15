// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
// Copyright (c) 2025 AgentSight Project
//
// TCP plain-text traffic probe - captures HTTP traffic to configured IP/port targets
// by hooking tcp_sendmsg (fentry) and tcp_recvmsg (fentry+fexit).
//
// Filters by destination IP/port only (no process-level filtering).
// Emits probe_SSL_data_t events (same format as sslsniff) so the entire
// downstream pipeline (parser, aggregator, analyzer, storage) works unchanged.
//
// Multi-kernel support:
//   - Kernel 5.18+: tcp_recvmsg(sk, msg, size, flags, addr_len)
//   - Kernel 5.8–5.17: tcp_recvmsg(sk, msg, size, nonblock, flags, addr_len)
//   Userspace tries the new signature first and falls back to old on attach failure.

use crate::config::{self, TcpTarget};
use anyhow::{Context, Result};
use libbpf_rs::{
    Link, MapFlags,
    skel::{OpenSkel, SkelBuilder},
};
use std::{mem::MaybeUninit, net::Ipv4Addr};

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
    include!(concat!(env!("OUT_DIR"), "/tcpsniff.skel.rs"));
}
use bpf::*;

/// TCP plain-text traffic probe
pub struct TcpSniff {
    _open_object: Box<MaybeUninit<libbpf_rs::OpenObject>>,
    skel: Box<TcpsniffSkel<'static>>,
    _links: Vec<Link>,
    use_old_sig: bool,
}

/// Maps tcpsniff reuses from the shared bundle. Filtering is by destination
/// IP/port only, so it shares the ring buffer alone (no process / cgroup filter).
const SHARED_MAPS: &[MapKind] = &[MapKind::Rb, MapKind::InternalMetrics];

impl TcpSniff {
    /// Build and load the BPF skeleton, selecting the correct tcp_recvmsg
    /// program variant for the running kernel.
    ///
    /// `use_old_sig`: true → load old (5.8-5.17) programs, false → new (5.18+)
    fn load_skel(
        shared: &SharedMaps,
        use_old_sig: bool,
    ) -> Result<(
        Box<MaybeUninit<libbpf_rs::OpenObject>>,
        Box<TcpsniffSkel<'static>>,
    )> {
        let mut builder = TcpsniffSkelBuilder::default();
        builder.obj_builder.debug(config::verbose());

        let open_object = Box::new(MaybeUninit::<libbpf_rs::OpenObject>::uninit());
        let mut open_skel = builder
            .open()
            .context("failed to open tcpsniff BPF object")?;

        // Tell BPF which namespace to report event pids in.
        open_skel.rodata_mut().observer_pidns_is_init = proc_root_is_init_pidns();

        // Reuse the shared ring buffer and its producer counters.
        shared
            .reuse_into(SHARED_MAPS, open_skel.open_object_mut())
            .context("failed to reuse shared maps for tcpsniff")?;

        // Selectively enable programs:
        // tcp_sendmsg fentry: always enabled (signature unchanged across kernels)
        // tcp_recvmsg fentry + fexit: enable either new or old variant
        if use_old_sig {
            // Disable new-signature programs
            open_skel
                .progs_mut()
                .trace_tcp_recvmsg_entry()
                .set_autoload(false)
                .context("failed to disable new recvmsg fentry")?;
            open_skel
                .progs_mut()
                .trace_tcp_recvmsg_exit()
                .set_autoload(false)
                .context("failed to disable new recvmsg fexit")?;
        } else {
            // Disable old-signature programs
            open_skel
                .progs_mut()
                .trace_tcp_recvmsg_entry_old()
                .set_autoload(false)
                .context("failed to disable old recvmsg fentry")?;
            open_skel
                .progs_mut()
                .trace_tcp_recvmsg_exit_old()
                .set_autoload(false)
                .context("failed to disable old recvmsg fexit")?;
        }

        let skel = open_skel
            .load()
            .context("failed to load tcpsniff BPF object")?;

        // SAFETY: skel borrows open_object which lives in a Box<MaybeUninit>
        let skel =
            unsafe { Box::from_raw(Box::into_raw(Box::new(skel)) as *mut TcpsniffSkel<'static>) };

        Ok((open_object, skel))
    }

    /// Reuse the shared ring buffer and internal counters.
    /// Automatically detects the tcp_recvmsg signature for the running kernel.
    /// Does NOT require traced_processes — filtering is by destination IP/port only.
    pub fn new_with_shared(shared: &SharedMaps) -> Result<Self> {
        // Try new signature first (5.18+), fall back to old (5.8-5.17) on load failure
        let (open_object, skel, use_old_sig) = match Self::load_skel(shared, false) {
            Ok((obj, skel)) => {
                log::info!("TcpSniff: loaded with new tcp_recvmsg signature (5.18+)");
                (obj, skel, false)
            }
            Err(e) => {
                log::info!(
                    "TcpSniff: new tcp_recvmsg signature failed ({e}), trying old (5.8-5.17)"
                );
                let (obj, skel) = Self::load_skel(shared, true)
                    .context("failed to load tcpsniff with old tcp_recvmsg signature")?;
                log::info!("TcpSniff: loaded with old tcp_recvmsg signature (5.8-5.17)");
                (obj, skel, true)
            }
        };

        Ok(Self {
            _open_object: open_object,
            skel,
            _links: Vec::new(),
            use_old_sig,
        })
    }

    /// Populate the BPF tcp_targets map with the given targets.
    /// Must be called after new_with_shared() and before attach().
    ///
    /// Key layout (8 bytes): ip (4 bytes BE) | port (2 bytes BE) | pad (2 bytes zero)
    pub fn set_targets(&mut self, targets: &[TcpTarget]) -> Result<()> {
        let binding = self.skel.maps();
        let map = binding.tcp_targets();
        let dummy: u8 = 1;

        let mut wildcard_all = false;
        for target in targets {
            let ip_be: u32 = match target.ip {
                Some(Ipv4Addr::UNSPECIFIED) | None => 0u32,
                Some(ip) => u32::from(ip).to_be(),
            };
            let port_be: u16 = match target.port {
                None => 0u16,
                Some(p) => p.to_be(),
            };
            if ip_be == 0 && port_be == 0 {
                wildcard_all = true;
            }
            // Serialize key as [ip_be(4)] [port_be(2)] [pad(2)]
            let mut key = [0u8; 8];
            key[0..4].copy_from_slice(&ip_be.to_ne_bytes());
            key[4..6].copy_from_slice(&port_be.to_ne_bytes());
            // key[6..8] = 0 (pad)

            map.update(&key, &[dummy], MapFlags::ANY)
                .with_context(|| format!("failed to add target {target:?} to tcp_targets map"))?;
        }

        if wildcard_all {
            log::warn!(
                "TcpSniff: full wildcard target (any IP, any port) configured — \
                 ALL outgoing TCP traffic will be captured. This has noticeable overhead; \
                 prefer narrowing by IP/port for production use."
            );
        }
        log::info!(
            "TcpSniff: configured {} target(s): {:?}",
            targets.len(),
            targets
        );
        Ok(())
    }

    pub fn add_target(&mut self, target: &TcpTarget) -> Result<()> {
        let binding = self.skel.maps();
        let map = binding.tcp_targets();
        let dummy: u8 = 1;

        let ip_be: u32 = match target.ip {
            Some(Ipv4Addr::UNSPECIFIED) | None => 0u32,
            Some(ip) => u32::from(ip).to_be(),
        };
        let port_be: u16 = match target.port {
            None => 0u16,
            Some(p) => p.to_be(),
        };
        let mut key = [0u8; 8];
        key[0..4].copy_from_slice(&ip_be.to_ne_bytes());
        key[4..6].copy_from_slice(&port_be.to_ne_bytes());

        map.update(&key, &[dummy], MapFlags::ANY)
            .with_context(|| format!("failed to add target {target:?} to tcp_targets map"))?;

        log::info!("TcpSniff: added runtime target {target:?}");
        Ok(())
    }

    /// Attach fentry/fexit hooks for tcp_sendmsg and tcp_recvmsg.
    /// Attaches whichever tcp_recvmsg variant was loaded.
    pub fn attach(&mut self) -> Result<()> {
        let mut links = Vec::new();

        // tcp_sendmsg fentry — always present
        let link = self
            .skel
            .progs_mut()
            .trace_tcp_sendmsg()
            .attach()
            .context("failed to attach tcp_sendmsg fentry")?;
        links.push(link);

        // tcp_recvmsg — attach the variant that was loaded
        if self.use_old_sig {
            let entry_link = self
                .skel
                .progs_mut()
                .trace_tcp_recvmsg_entry_old()
                .attach()
                .context("failed to attach tcp_recvmsg fentry (old signature)")?;
            links.push(entry_link);

            let exit_link = self
                .skel
                .progs_mut()
                .trace_tcp_recvmsg_exit_old()
                .attach()
                .context("failed to attach tcp_recvmsg fexit (old signature)")?;
            links.push(exit_link);
        } else {
            let entry_link = self
                .skel
                .progs_mut()
                .trace_tcp_recvmsg_entry()
                .attach()
                .context("failed to attach tcp_recvmsg fentry")?;
            links.push(entry_link);

            let exit_link = self
                .skel
                .progs_mut()
                .trace_tcp_recvmsg_exit()
                .attach()
                .context("failed to attach tcp_recvmsg fexit")?;
            links.push(exit_link);
        }

        let n = links.len();
        self._links = links;
        log::info!(
            "TcpSniff: attached {n} BPF programs (tcp_sendmsg fentry, tcp_recvmsg fentry+fexit)"
        );
        Ok(())
    }
}
