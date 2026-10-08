use crate::detect::{read_sysctl_i64, read_sysctl_u64, DiskInfo, DiskType, SystemInfo};
use crate::profile::WorkloadType;
use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Confidence {
    #[default]
    High, // Hardware-deterministic or zero-tradeoff, guaranteed correct
    Medium, // Workload-deterministic, high probability
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Category {
    #[default]
    Performance,
    Security,
}

impl std::fmt::Display for Category {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Category::Performance => write!(f, "性能"),
            Category::Security => write!(f, "安全"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Recommendation {
    pub param: String,
    pub current_value: String,
    pub recommended_value: String,
    pub reason: String,
    pub confidence: Confidence,
    pub category: Category,
    pub writable: bool,
}

pub struct EvalResult {
    pub recommendations: Vec<Recommendation>,
    pub total_checked: usize,
}

impl Confidence {
    /// Score penalty of one recommendation: hardware-deterministic advice
    /// (`High`) costs more than workload-dependent advice (`Medium`), and the
    /// same weight drives `EvalResult::score` and the score predicted after
    /// tuning.
    pub fn weight(&self) -> usize {
        match self {
            Confidence::High => 3,
            Confidence::Medium => 2,
        }
    }
}

impl EvalResult {
    /// Total penalty the recommendations cost, in `Confidence::weight` units.
    pub fn penalty(&self) -> usize {
        self.recommendations
            .iter()
            .map(|r| r.confidence.weight())
            .sum()
    }

    /// Health score for this result: 100 minus the penalty of every finding,
    /// floored at 30 so a heavily untuned host stays on a readable scale.
    pub fn score(&self) -> usize {
        if self.recommendations.is_empty() {
            return 100;
        }
        100usize.saturating_sub(self.penalty()).max(30)
    }

    /// Score the system would report once every recommendation in `applied` has
    /// been applied: the penalty of what is *not* applied, floored exactly like
    /// `score`. `score() + applied weight` is not the same number — `score` is
    /// already floored at 30, so that sum counts the floor as a gain and
    /// overstates the result whenever the penalty exceeds 70.
    pub fn score_after_applying(&self, applied: &[Recommendation]) -> usize {
        let remaining: usize = self
            .recommendations
            .iter()
            .filter(|rec| !applied.iter().any(|a| a.param == rec.param))
            .map(|rec| rec.confidence.weight())
            .sum();
        100usize.saturating_sub(remaining).max(30)
    }
}

pub fn evaluate(info: &SystemInfo) -> Result<EvalResult> {
    let workload = crate::profile::classify(info);
    evaluate_with_workload(info, &workload)
}

pub fn evaluate_with_workload(info: &SystemInfo, workload: &WorkloadType) -> Result<EvalResult> {
    let mut recs = Vec::new();
    let mut checked: usize = 0;

    // Performance rules
    checked += eval_io_scheduler(info, &mut recs);
    checked += eval_swappiness(info, workload, &mut recs);
    checked += eval_thp(info, &mut recs);
    checked += eval_dirty_ratio(info, workload, &mut recs);
    checked += eval_somaxconn(info, workload, &mut recs);
    checked += eval_tcp_fastopen(info, &mut recs);
    checked += eval_min_free_kbytes(info, &mut recs);

    // Network performance rules
    checked += eval_netdev_max_backlog(info, &mut recs);
    checked += eval_tcp_max_syn_backlog(info, &mut recs);
    checked += eval_rmem_max(info, &mut recs);
    checked += eval_wmem_max(info, &mut recs);
    checked += eval_tcp_rmem(info, &mut recs);
    checked += eval_tcp_wmem(info, &mut recs);
    checked += eval_tcp_slow_start_after_idle(info, &mut recs);
    checked += eval_ip_local_port_range(info, &mut recs);
    checked += eval_default_qdisc(info, &mut recs);
    checked += eval_tcp_tw_reuse(info, &mut recs);
    checked += eval_tcp_fin_timeout(info, &mut recs);
    checked += eval_tcp_keepalive_time(info, &mut recs);
    checked += eval_tcp_keepalive_intvl(info, &mut recs);
    checked += eval_tcp_keepalive_probes(info, &mut recs);
    checked += eval_tcp_max_tw_buckets(info, &mut recs);
    checked += eval_tcp_mtu_probing(info, &mut recs);
    checked += eval_tcp_no_metrics_save(info, &mut recs);
    checked += eval_tcp_congestion_control(info, &mut recs);
    checked += eval_nf_conntrack_max(info, &mut recs);
    checked += eval_tcp_timestamps(info, &mut recs);
    checked += eval_tcp_window_scaling(info, &mut recs);
    checked += eval_tcp_ecn(info, &mut recs);
    checked += eval_tcp_sack(info, &mut recs);
    checked += eval_netdev_budget(info, &mut recs);
    checked += eval_busy_poll(info, &mut recs);
    checked += eval_busy_read(info, &mut recs);
    checked += eval_tcp_retries2(info, &mut recs);
    checked += eval_tcp_syn_retries(info, &mut recs);
    checked += eval_tcp_synack_retries(info, &mut recs);
    checked += eval_optmem_max(info, &mut recs);
    checked += eval_neigh_gc_thresh3(info, &mut recs);
    checked += eval_arp_announce(info, &mut recs);
    checked += eval_arp_ignore(info, &mut recs);

    // VM/Memory rules
    checked += eval_max_map_count(info, &mut recs);
    checked += eval_zone_reclaim_mode(info, &mut recs);
    checked += eval_vfs_cache_pressure(info, &mut recs);
    checked += eval_watermark_scale_factor(info, &mut recs);
    checked += eval_file_max(info, &mut recs);
    checked += eval_nr_open(info, &mut recs);
    checked += eval_inotify_max_user_watches(info, &mut recs);
    checked += eval_aio_max_nr(info, &mut recs);
    checked += eval_oom_kill_allocating_task(info, &mut recs);
    checked += eval_overcommit_memory(info, &mut recs);
    checked += eval_dirty_background_ratio(info, workload, &mut recs);
    checked += eval_dirty_expire_centisecs(info, &mut recs);
    checked += eval_dirty_writeback_centisecs(info, &mut recs);
    checked += eval_shmmax(info, &mut recs);

    // IO/Disk rules
    checked += eval_read_ahead_kb(info, &mut recs);
    checked += eval_nr_requests(info, &mut recs);
    checked += eval_rq_affinity(info, &mut recs);

    // CPU/Scheduler rules
    checked += eval_numa_balancing(info, &mut recs);
    checked += eval_sched_autogroup(info, &mut recs);
    checked += eval_pid_max(info, &mut recs);
    checked += eval_sched_migration_cost(info, &mut recs);
    checked += eval_sched_min_granularity(info, &mut recs);
    checked += eval_nmi_watchdog(info, &mut recs);
    checked += eval_stat_interval(info, &mut recs);
    checked += eval_hung_task_timeout(info, &mut recs);

    checked += eval_netdev_budget_usecs(info, &mut recs);
    checked += eval_dirty_bytes(info, &mut recs);
    checked += eval_sched_child_runs_first(info, &mut recs);
    checked += eval_page_cluster(info, &mut recs);
    checked += eval_rmem_default(info, &mut recs);
    checked += eval_wmem_default(info, &mut recs);
    checked += eval_sched_nr_migrate(info, &mut recs);
    checked += eval_tcp_notsent_lowat(info, &mut recs);
    checked += eval_unix_max_dgram_qlen(info, &mut recs);
    checked += eval_rps_sock_flow_entries(info, &mut recs);
    checked += eval_tcp_dsack(info, &mut recs);
    checked += eval_ip_no_pmtu_disc(info, &mut recs);
    checked += eval_sched_wakeup_granularity(info, &mut recs);
    checked += eval_extfrag_threshold(info, &mut recs);
    checked += eval_tcp_orphan_retries(info, &mut recs);
    checked += eval_tcp_early_retrans(info, &mut recs);
    checked += eval_tcp_tw_recycle(info, &mut recs);
    checked += eval_arp_filter(info, &mut recs);
    checked += eval_sched_cfs_bandwidth_slice(info, &mut recs);

    // Security rules (zero performance cost)
    checked += eval_aslr(info, &mut recs);
    checked += eval_dmesg_restrict(info, &mut recs);
    checked += eval_kptr_restrict(info, &mut recs);
    checked += eval_protected_links(info, &mut recs);
    checked += eval_accept_redirects(info, &mut recs);
    checked += eval_sysrq(info, &mut recs);
    checked += eval_tcp_syncookies(info, &mut recs);
    checked += eval_send_redirects(info, &mut recs);
    checked += eval_perf_event_paranoid(info, &mut recs);
    checked += eval_rp_filter(info, &mut recs);
    checked += eval_panic(info, &mut recs);
    checked += eval_panic_on_oom(info, &mut recs);
    checked += eval_ip_forward(info, &mut recs);
    checked += eval_unprivileged_bpf(info, &mut recs);
    checked += eval_core_uses_pid(info, &mut recs);
    checked += eval_yama_ptrace_scope(info, &mut recs);
    checked += eval_log_martians(info, &mut recs);
    checked += eval_icmp_echo_ignore_broadcasts(info, &mut recs);
    checked += eval_accept_source_route(info, &mut recs);
    checked += eval_tcp_rfc1337(info, &mut recs);
    checked += eval_secure_redirects(info, &mut recs);
    checked += eval_mmap_min_addr(info, &mut recs);
    checked += eval_default_accept_redirects(info, &mut recs);
    checked += eval_default_accept_source_route(info, &mut recs);
    checked += eval_sched_latency_ns(info, &mut recs);
    checked += eval_tcp_challenge_ack_limit(info, &mut recs);
    checked += eval_rp_filter_all(info, &mut recs);
    checked += eval_tcp_max_orphans(info, &mut recs);
    checked += eval_threads_max(info, &mut recs);
    checked += eval_nr_hugepages(info, &mut recs);
    checked += eval_suid_dumpable(info, &mut recs);
    checked += eval_icmp_ignore_bogus(info, &mut recs);
    checked += eval_default_log_martians(info, &mut recs);
    checked += eval_laptop_mode(info, &mut recs);
    checked += eval_tcp_adv_win_scale(info, &mut recs);
    checked += eval_sched_tunable_scaling(info, &mut recs);
    checked += eval_panic_on_oops(info, &mut recs);
    checked += eval_oom_dump_tasks(info, &mut recs);
    checked += eval_tcp_moderate_rcvbuf(info, &mut recs);
    checked += eval_flow_limit_table_len(info, &mut recs);
    checked += eval_tcp_l3mdev_accept(info, &mut recs);
    checked += eval_panic_on_warn(info, &mut recs);
    checked += eval_dirty_background_bytes(info, &mut recs);
    checked += eval_hardlockup_panic(info, &mut recs);
    checked += eval_sched_rt_runtime(info, &mut recs);
    checked += eval_tcp_thin_linear_timeouts(info, &mut recs);
    checked += eval_arp_notify(info, &mut recs);
    checked += eval_default_arp_announce(info, &mut recs);
    checked += eval_default_arp_ignore(info, &mut recs);
    checked += eval_default_send_redirects(info, &mut recs);
    checked += eval_neigh_gc_thresh1(info, &mut recs);
    checked += eval_neigh_gc_thresh2(info, &mut recs);
    checked += eval_tcp_retries1(info, &mut recs);
    checked += eval_tcp_limit_output_bytes(info, &mut recs);
    checked += eval_dev_weight(info, &mut recs);
    checked += eval_printk(info, &mut recs);
    checked += eval_watchdog_thresh(info, &mut recs);
    checked += eval_admin_reserve_kbytes(info, &mut recs);
    checked += eval_msgmax(info, &mut recs);
    checked += eval_msgmnb(info, &mut recs);
    checked += eval_protected_fifos(info, &mut recs);
    checked += eval_user_reserve_kbytes(info, &mut recs);
    checked += eval_shmmni(info, &mut recs);
    checked += eval_sem(info, &mut recs);
    checked += eval_gc_stale_time(info, &mut recs);
    checked += eval_shm_rmid_forced(info, &mut recs);
    checked += eval_tcp_fack(info, &mut recs);
    checked += eval_tcp_reordering(info, &mut recs);
    checked += eval_sched_energy_aware(info, &mut recs);
    checked += eval_percpu_pagelist_high_fraction(info, &mut recs);
    checked += eval_accept_ra(info, &mut recs);
    checked += eval_compact_memory(info, &mut recs);
    checked += eval_min_slab_ratio(info, &mut recs);
    checked += eval_tcp_autocorking(info, &mut recs);
    checked += eval_tcp_workaround_signed_windows(info, &mut recs);
    checked += eval_randomize_va_space_full(info, &mut recs);
    checked += eval_max_user_instances(info, &mut recs);
    checked += eval_keys_maxkeys(info, &mut recs);
    checked += eval_tcp_available_ulp(info, &mut recs);
    checked += eval_numa_stat(info, &mut recs);
    checked += eval_tcp_base_mss(info, &mut recs);
    checked += eval_tcp_min_tso_segs(info, &mut recs);
    checked += eval_neigh_default_gc_interval(info, &mut recs);
    checked += eval_neigh_default_gc_stale_time(info, &mut recs);
    checked += eval_tcp_fastopen_blackhole_timeout(info, &mut recs);
    checked += eval_max_queued_signals(info, &mut recs);
    checked += eval_keys_maxbytes(info, &mut recs);
    checked += eval_pipe_max_size(info, &mut recs);
    checked += eval_shmall(info, &mut recs, current_page_size, |path| {
        std::path::Path::new(path)
            .exists()
            .then(|| read_sysctl_u64(path))
    });
    checked += eval_tcp_app_win(info, &mut recs);
    checked += eval_ip_default_ttl(info, &mut recs);
    checked += eval_tcp_frto(info, &mut recs);
    checked += eval_icmp_ratelimit(info, &mut recs);
    checked += eval_igmp_max_memberships(info, &mut recs);
    checked += eval_tcp_recovery(info, &mut recs);
    checked += eval_tcp_comp_sack_delay(info, &mut recs);
    checked += eval_skb_frag_coalesce(info, &mut recs);
    checked += eval_neigh_proxy_delay(info, &mut recs);
    checked += eval_tcp_pacing_ca_ratio(info, &mut recs);
    checked += eval_tcp_pacing_ss_ratio(info, &mut recs);
    checked += eval_tcp_comp_sack_nr(info, &mut recs);
    checked += eval_tcp_thin_dupack(info, &mut recs);
    checked += eval_tcp_invalid_ratelimit(info, &mut recs);
    checked += eval_tcp_init_cwnd(info, &mut recs);
    checked += eval_tcp_tso_win_divisor(info, &mut recs);
    checked += eval_sched_schedstats(info, &mut recs);
    checked += eval_inotify_max_queued_events(info, &mut recs);
    checked += eval_tcp_max_reordering(info, &mut recs);
    checked += eval_tcp_retrans_collapse(info, &mut recs);
    checked += eval_protected_regular(info, &mut recs);
    checked += eval_bpf_jit_enable(info, &mut recs);
    checked += eval_bpf_jit_harden(info, &mut recs);
    checked += eval_tcp_available_congestion(info, &mut recs);
    checked += eval_somaxconn_large(info, &mut recs);
    checked += eval_promote_secondaries(info, &mut recs);
    checked += eval_unres_qlen_bytes(info, &mut recs);
    checked += eval_ip_nonlocal_bind(info, &mut recs);
    checked += eval_conntrack_tcp_timeout_established(info, &mut recs);
    checked += eval_softlockup_all_cpu_backtrace(info, &mut recs);
    checked += eval_compact_unevictable(info, &mut recs);
    checked += eval_perf_cpu_time_max_percent(info, &mut recs);
    checked += eval_hung_task_warnings(info, &mut recs);
    checked += eval_overcommit_ratio(info, &mut recs);

    // A few params are evaluated by more than one rule. Collapse duplicates so
    // each param appears once — otherwise it is shown twice and its score
    // penalty is double-counted.
    let mut recs = dedupe_recommendations(recs);

    // Check writability for each recommendation
    for rec in &mut recs {
        let path = crate::tuner::param_to_path(&rec.param);
        rec.writable = crate::detect::is_param_writable(&path);
    }

    recs.sort_by(|a, b| {
        let ca = match a.confidence {
            Confidence::High => 0u8,
            Confidence::Medium => 1,
        };
        let cb = match b.confidence {
            Confidence::High => 0u8,
            Confidence::Medium => 1,
        };
        ca.cmp(&cb).then_with(|| a.param.cmp(&b.param))
    });

    Ok(EvalResult {
        recommendations: recs,
        total_checked: checked,
    })
}

/// Collapse recommendations that target the same param to a single entry,
/// keeping the higher-confidence one (on a tie, the first seen, preserving
/// order). Prevents duplicate display lines and double-counted score penalties.
fn dedupe_recommendations(recs: Vec<Recommendation>) -> Vec<Recommendation> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut deduped: Vec<Recommendation> = Vec::with_capacity(recs.len());
    for rec in recs.into_iter() {
        if let Some(&idx) = seen.get(&rec.param) {
            if rec.confidence == Confidence::High && deduped[idx].confidence == Confidence::Medium {
                deduped[idx] = rec;
            }
        } else {
            seen.insert(rec.param.clone(), deduped.len());
            deduped.push(rec);
        }
    }
    deduped
}

// ─── Performance Rules ────────────────────────────────────────────────────────

/// Scheduler an NVMe disk runs after tuning: the pass-through scheduler
/// [`eval_io_scheduler`] recommends, or `None` when neither is offered.
fn nvme_scheduler_target(disk: &DiskInfo) -> Option<&'static str> {
    if disk.available_schedulers.iter().any(|s| s == "none") {
        Some("none")
    } else if disk.available_schedulers.iter().any(|s| s == "noop") {
        Some("noop")
    } else {
        None
    }
}

fn eval_io_scheduler(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let mut count = 0;
    for disk in &info.disks {
        match disk.disk_type {
            DiskType::NVMe => {
                count += 1;
                let Some(target) = nvme_scheduler_target(disk) else {
                    continue;
                };

                if disk.scheduler != target {
                    recs.push(Recommendation {
                        param: format!("block/{}/scheduler", disk.name),
                        current_value: disk.scheduler.clone(),
                        recommended_value: target.to_string(),
                        reason: "NVMe 磁盘无需软件 IO 调度，直通硬件队列延迟最低".to_string(),
                        confidence: Confidence::High,
                        category: Category::Performance,
                        writable: true,
                    });
                }
            }
            DiskType::SSD => {
                count += 1;
                if disk.scheduler == "cfq" {
                    let target = if disk.available_schedulers.contains(&"none".to_string()) {
                        "none"
                    } else if disk.available_schedulers.contains(&"noop".to_string()) {
                        "noop"
                    } else if disk.available_schedulers.contains(&"deadline".to_string()) {
                        "deadline"
                    } else {
                        continue;
                    };

                    recs.push(Recommendation {
                        param: format!("block/{}/scheduler", disk.name),
                        current_value: disk.scheduler.clone(),
                        recommended_value: target.to_string(),
                        reason: "SSD 随机读写性能强，cfq 的排队排序开销是不必要的".to_string(),
                        confidence: Confidence::High,
                        category: Category::Performance,
                        writable: true,
                    });
                }
            }
            _ => {}
        }
    }
    count
}

fn eval_swappiness(
    info: &SystemInfo,
    workload: &WorkloadType,
    recs: &mut Vec<Recommendation>,
) -> usize {
    let is_db = info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd — the same OLTP database as mysqld.
        || info.has_process("mariadbd")
        || info.has_process("mongod")
        || info.has_process("clickhouse")
        || info.has_process("redis-server");

    let (target, reason) = if is_db
        || *workload == WorkloadType::IoLatency
        || *workload == WorkloadType::MemoryIntensive
    {
        (
            1,
            "数据库/缓存场景，swap 会导致严重的延迟抖动，建议几乎禁用",
        )
    } else if info.memory_total_gb >= 64 {
        (
            10,
            "大内存机器（≥64GB）通常不需要积极 swap，降低可减少不必要的页面换出",
        )
    } else {
        return 1;
    };

    if info.sysctl.swappiness > target as u64 {
        recs.push(Recommendation {
            param: "vm.swappiness".to_string(),
            current_value: info.sysctl.swappiness.to_string(),
            recommended_value: target.to_string(),
            reason: reason.to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_thp(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    // Databases and caches whose latency profile THP's collapse/split stalls
    // hurt. mongod sits in every sibling db list (eval_swappiness,
    // eval_dirty_ratio, the readahead rule, the classify gate) and MongoDB's
    // production notes ask for THP to be off outright; etcd is the
    // coordination store of the same cache family redis/memcached belong to.
    let is_latency_sensitive = info.has_process("redis-server")
        || info.has_process("memcached")
        || info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd — the same OLTP database as mysqld.
        || info.has_process("mariadbd")
        || info.has_process("mongod")
        || info.has_process("clickhouse")
        || info.has_process("etcd");

    if is_latency_sensitive && info.sysctl.thp_enabled == "always" {
        recs.push(Recommendation {
            param: "transparent_hugepage/enabled".to_string(),
            current_value: "always".to_string(),
            recommended_value: "madvise".to_string(),
            reason: "检测到延迟敏感进程，THP 的合并/分裂操作会造成不可预测的延迟抖动".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_dirty_ratio(
    info: &SystemInfo,
    workload: &WorkloadType,
    recs: &mut Vec<Recommendation>,
) -> usize {
    let is_db = info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd — the same OLTP database as mysqld.
        || info.has_process("mariadbd")
        || info.has_process("mongod")
        || info.has_process("clickhouse");
    let is_latency_sensitive = is_db || *workload == WorkloadType::IoLatency;

    // dirty_ratio and dirty_bytes are mutually exclusive in the kernel (setting
    // one zeroes the other). Big-memory machines are handled by the bytes-based
    // rules (eval_dirty_bytes / eval_dirty_background_bytes, which require
    // >=64GB); cap the percentage-based advice to <64GB so no host ever gets
    // both a *_ratio and a *_bytes recommendation for the same dimension.
    if !is_latency_sensitive || info.memory_total_gb >= 64 {
        return 2;
    }

    let (target_ratio, target_bg) = (5, 3);

    if info.sysctl.dirty_ratio > target_ratio {
        recs.push(Recommendation {
            param: "vm.dirty_ratio".to_string(),
            current_value: info.sysctl.dirty_ratio.to_string(),
            recommended_value: target_ratio.to_string(),
            reason: "延迟敏感负载需要更低的 dirty_ratio，避免脏页积压导致的写入延迟尖刺"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }

    // The dirty limit this host ends up with: the write above only lowers it.
    let dirty_limit = info.sysctl.dirty_ratio.min(target_ratio);
    if info.sysctl.dirty_background_ratio > target_bg
        && dirty_background_takes_effect(dirty_limit, target_bg)
    {
        recs.push(Recommendation {
            param: "vm.dirty_background_ratio".to_string(),
            current_value: info.sysctl.dirty_background_ratio.to_string(),
            recommended_value: target_bg.to_string(),
            reason: "更早触发后台刷脏，平滑写入压力".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    2
}

/// Whether a `vm.dirty_background_ratio` target can change the effective
/// threshold at all.
///
/// `domain_dirty_limits()` (mm/page-writeback.c) replaces a background
/// threshold at or above the dirty threshold with half of it:
///
/// ```text
///     if (bg_thresh >= thresh)
///         bg_thresh = thresh / 2;
/// ```
///
/// A target that does not stay strictly below the dirty limit in force
/// therefore leaves the effective background threshold at `limit / 2` — on a
/// host that already sits there the write changes nothing, so a rule that
/// promises an earlier background flush stays quiet instead. `dirty_limit` is
/// the ratio this run's advice leaves in force, which may be lower than the
/// current one.
fn dirty_background_takes_effect(dirty_limit: u64, target: u64) -> bool {
    target < dirty_limit
}

fn eval_somaxconn(
    info: &SystemInfo,
    workload: &WorkloadType,
    recs: &mut Vec<Recommendation>,
) -> usize {
    if !info.has_listen_sockets() {
        return 1;
    }

    let threshold = if *workload == WorkloadType::NetworkIntensive {
        8192
    } else {
        4096
    };

    if info.sysctl.somaxconn < threshold as u64 {
        recs.push(Recommendation {
            param: "net.core.somaxconn".to_string(),
            current_value: info.sysctl.somaxconn.to_string(),
            recommended_value: "65535".to_string(),
            reason: "检测到监听端口，增大 listen backlog 避免高并发时连接被拒绝".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_fastopen(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_fastopen_at(
        info,
        recs,
        "/proc/sys/net/ipv4/tcp_fastopen",
        info.has_listen_sockets(),
    )
}

/// Path-injectable form of [`eval_tcp_fastopen`] (the `eval_*_at` idiom) so
/// the signed read is assertable against a temp file on any host, listener
/// or not.
///
/// The value is read SIGNED: net/ipv4/sysctl_net_ipv4.c registers
/// tcp_fastopen as a plain `proc_dointvec` int with no min/max in both v5.10
/// and v6.6, so `-1` is a legal, persistent whole-word value there — and it
/// means every flag bit set (client 0x1, server 0x2, the force-on-all-
/// listeners bit 0x400, ...): passive TFO fully enabled. The unsigned
/// reader parsed "-1" to `Err` and fell back to `0`, so the completion test
/// ran on a value the kernel never held and recommended `1027` on a host
/// whose TFO was already fully on — a write that would also clear every
/// administrator flag outside the required three. (The neighbouring
/// `tcp_fastopen_blackhole_timeout_sec` is not affected: its handler is
/// `proc_tfo_blackhole_detect_timeout` over `proc_dointvec_minmax` with
/// `.extra1 = SYSCTL_ZERO`, so the kernel itself rejects negative writes
/// there and the unsigned reader stays correct for it.)
fn eval_tcp_fastopen_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    has_listen_sockets: bool,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !has_listen_sockets {
        return 1;
    }
    if let Some(rec) = tcp_fastopen_recommendation(read_sysctl_i64(path)) {
        recs.push(rec);
    }
    1
}

/// Value-driven core of the `net.ipv4.tcp_fastopen` rule, split from the live
/// probe so every branch is assertable on any host (the sysctl is read from
/// the real `/proc`).
///
/// The sysctl is a bitmask (`include/net/tcp.h`): 0x1 enables the client,
/// 0x2 the server, and 0x400 forces TFO on all listeners, "i.e., not
/// requiring the TCP_FASTOPEN socket option". Passive TFO needs 0x2 AND
/// 0x400 together: `__inet_listen_sk` only fills a listener's
/// `fastopenq.max_qlen` when both are set, and `tcp_fastopen_queue_check`
/// then refuses every SYN that carries data while that length is still 0.
/// The rule used to recommend 3 and treat any value >= 3 as done, so on the
/// host it had just tuned, a listener that never called the TCP_FASTOPEN
/// socket option got no passive TFO at all — the opposite of what the reason
/// promised. The value is written as a whole word, so flags the
/// administrator set (0x4 `TFO_CLIENT_NO_COOKIE`, 0x200
/// `TFO_SERVER_COOKIE_NOT_REQD`, ...) are kept rather than cleared.
///
/// `current` is i64 because the registration above accepts `-1`: the
/// completion test `current & REQUIRED == REQUIRED` then recognizes the
/// every-flag-set host as already done, and the whole-word `current |
/// REQUIRED` write preserves the administrator's negative flag word
/// (`-2 | 0x403 == -1`) instead of collapsing it to `1027`.
fn tcp_fastopen_recommendation(current: i64) -> Option<Recommendation> {
    const REQUIRED: i64 = 0x1 | 0x2 | 0x400;
    if current & REQUIRED == REQUIRED {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.tcp_fastopen".to_string(),
        current_value: current.to_string(),
        recommended_value: (current | REQUIRED).to_string(),
        reason: "启用 TCP Fast Open 的客户端与服务端，并让未调用 TCP_FASTOPEN socket 选项的监听套接字也生效（内核要求 0x400，仅写 3 时服务端 TFO 实际不生效）".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_min_free_kbytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    if !info.param_exists("/proc/sys/vm/min_free_kbytes") {
        return 1;
    }

    let current = read_sysctl_u64("/proc/sys/vm/min_free_kbytes");
    let mem_kb = info.memory_total_gb * 1024 * 1024;
    let recommended = (mem_kb / 1000).min(2 * 1024 * 1024);

    if current < recommended / 2 {
        recs.push(Recommendation {
            param: "vm.min_free_kbytes".to_string(),
            current_value: current.to_string(),
            recommended_value: recommended.to_string(),
            reason: format!(
                "空闲页面水位偏低（{}GB 内存），突发分配时易触发直接回收造成延迟，适当调高更平稳",
                info.memory_total_gb
            ),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

// ─── Security Rules (zero performance cost) ───────────────────────────────────

fn eval_aslr(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/randomize_va_space";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 2 {
        recs.push(Recommendation {
            param: "kernel.randomize_va_space".to_string(),
            current_value: current.to_string(),
            recommended_value: "2".to_string(),
            reason: "ASLR 未完全启用，攻击者可预测内存地址布局实施代码注入".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_dmesg_restrict(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/dmesg_restrict";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.dmesg_restrict".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "普通用户可读取内核日志，可能泄露敏感信息（内存地址、硬件细节）".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_kptr_restrict(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/kptr_restrict";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.kptr_restrict".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "内核指针地址对普通用户可见，降低了内核漏洞利用的难度".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_protected_links(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let hardlinks_path = "/proc/sys/fs/protected_hardlinks";
    let symlinks_path = "/proc/sys/fs/protected_symlinks";

    if info.param_exists(hardlinks_path) {
        let current = read_sysctl_u64(hardlinks_path);
        if current == 0 {
            recs.push(Recommendation {
                param: "fs.protected_hardlinks".to_string(),
                current_value: "0".to_string(),
                recommended_value: "1".to_string(),
                reason: "未启用硬链接保护，非特权用户可能利用硬链接进行提权攻击".to_string(),
                confidence: Confidence::High,
                category: Category::Security,
                writable: true,
            });
        }
    }

    if info.param_exists(symlinks_path) {
        let current = read_sysctl_u64(symlinks_path);
        if current == 0 {
            recs.push(Recommendation {
                param: "fs.protected_symlinks".to_string(),
                current_value: "0".to_string(),
                recommended_value: "1".to_string(),
                reason: "未启用符号链接保护，存在 TOCTOU 竞态条件攻击风险".to_string(),
                confidence: Confidence::High,
                category: Category::Security,
                writable: true,
            });
        }
    }
    2
}

fn eval_accept_redirects(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/accept_redirects";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.accept_redirects".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "接受 ICMP 重定向可被用于中间人攻击，服务器通常不需要此功能".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_sysrq(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_sysrq_at(info, recs, "/proc/sys/kernel/sysrq")
}

/// Path-injectable form (the `eval_*_at` idiom) so the signed read is
/// unit-testable against a temp file. `drivers/tty/sysrq.c` registers
/// kernel.sysrq through `sysrq_sysctl_handler`, which copies the table
/// with no min/max and reads back through `sysrq_mask()` — and -1 is the
/// mask with every function enabled, a legal, maximally-open setting. The
/// unsigned reader parses "-1" to Err and falls back to 0, which here is
/// the *disabled* value, so the `current != 0 && current != 176` gate
/// skipped the hardening rule on exactly the most exposed hosts.
fn eval_sysrq_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // sysrq is a mask; -1 enables every function, so it must be read signed
    // or the unsigned fallback maps it to 0 (disabled) and skips the rule.
    let current = read_sysctl_i64(path);
    // sysrq=1 means all functions enabled; high values also enable all
    if current != 0 && current != 176 {
        // 176 = safe subset (sync + remount-ro + reboot)
        recs.push(Recommendation {
            param: "kernel.sysrq".to_string(),
            current_value: current.to_string(),
            recommended_value: "176".to_string(),
            reason: "SysRq 功能过于开放，限制为安全子集（同步+只读重挂载+重启）防止滥用"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

// ─── Network Performance Rules ───────────────────────────────────────────────

fn eval_netdev_max_backlog(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/netdev_max_backlog";
    if !info.param_exists(path) {
        return 1;
    }
    if info.max_net_speed() < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 10000 {
        recs.push(Recommendation {
            param: "net.core.netdev_max_backlog".to_string(),
            current_value: current.to_string(),
            recommended_value: "65536".to_string(),
            reason: "万兆网卡场景下增大网卡收包队列深度，避免高流量时软中断处理不及导致丢包"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_max_syn_backlog(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_max_syn_backlog_at(
        info,
        recs,
        "/proc/sys/net/ipv4/tcp_max_syn_backlog",
        "/proc/sys/net/ipv4/tcp_syncookies",
    )
}

/// Path-injectable form of [`eval_tcp_max_syn_backlog`] (the `eval_*_at`
/// idiom) so the syncookies gate is assertable against temp files on any
/// host, listener or not.
///
/// The syncookies input is read SIGNED: through v5.10 net/ipv4/
/// sysctl_net_ipv4.c registered tcp_syncookies as a plain `proc_dointvec`
/// int with no min/max, so -1 is a legal value there, and every consumer is
/// a truthiness test (net/ipv4/tcp_input.c: `if (syncookies)`,
/// `if (!syncookies && ...)`, plus the documented `syncookies == 2` mode).
/// The unsigned reader parsed "-1" to Err and fell back to 0 — the
/// *disabled* value — so the backlog rule sized the knob on exactly the
/// hosts where the kernel skips its only reader.
fn eval_tcp_max_syn_backlog_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    syncookies_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if let Some(rec) = syn_backlog_recommendation(
        read_sysctl_u64(path),
        read_sysctl_i64(syncookies_path),
        info.has_listen_sockets(),
    ) {
        recs.push(rec);
    }
    1
}

/// Value-driven core of the syn-backlog rule (the `*_recommendation` idiom):
/// every input the branch needs is a parameter, so the gate is assertable on
/// any host instead of only on one that happens to have a listener and
/// disabled syncookies.
///
/// The knob has exactly one reader in v6.6: the last-quarter reservation in
/// `tcp_conn_request()` (net/ipv4/tcp_input.c):
///
/// ```text
///    if (!want_cookie && !isn) {
///        int max_syn_backlog = READ_ONCE(net->ipv4.sysctl_max_syn_backlog);
///
///        /* Kill the following clause, if you dislike this way. */
///        if (!syncookies &&
///            (max_syn_backlog - inet_csk_reqsk_queue_len(sk) <
///             (max_syn_backlog >> 2)) &&
///            !tcp_peer_is_proven(req, dst)) {
///            ... goto drop_and_release;
///        }
/// ```
///
/// and the clause is skipped entirely while syncookies are enabled — the
/// kernel default (tcp_ipv4.c: `net->ipv4.sysctl_tcp_syncookies = 1`) and what
/// this engine's own `net.ipv4.tcp_syncookies` rule asks for. The request queue
/// itself is bounded by `sk_max_ack_backlog`
/// (`inet_csk_reqsk_queue_is_full`), i.e. the listener backlog capped by
/// `net.core.somaxconn`, so on a syncookie host this knob sizes nothing and the
/// reason's promise ("避免突发连接请求时 SYN 被丢弃") cannot be delivered by
/// writing it. A host that turned syncookies off still reads it — including a
/// kernel built without CONFIG_SYN_COOKIES, where the file is absent and the
/// read falls back to 0 — so only the enabled case is skipped.
///
/// `syncookies` is i64 because the sysctl held a signed int through v5.10
/// (plain `proc_dointvec`, no min/max) and -1 means enabled there: the check
/// is the kernel's own truthiness (`if (!syncookies …)`), not an equality.
fn syn_backlog_recommendation(
    current: u64,
    syncookies: i64,
    has_listen_sockets: bool,
) -> Option<Recommendation> {
    if !has_listen_sockets || syncookies != 0 || current >= 8192 {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.tcp_max_syn_backlog".to_string(),
        current_value: current.to_string(),
        recommended_value: "65536".to_string(),
        reason: "增大半连接队列，避免突发连接请求时 SYN 被丢弃".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_rmem_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_rmem_max_at(info, recs, "/proc/sys/net/core/rmem_max")
}

/// Path-injectable form of [`eval_rmem_max`] (the `eval_*_at` idiom) so the
/// reason's claim is assertable against a temp file on any host.
///
/// `net.core.rmem_max` bounds what an application can ask for with an
/// explicit `setsockopt(SO_RCVBUF)`: `sock_setsockopt()` clamps the request
/// with `min_t(u32, val, READ_ONCE(sysctl_rmem_max))`, and TCP disables
/// automatic tuning for that socket — the sysctl documentation spells the
/// resulting split out for `tcp_rmem`: "Calling setsockopt() with SO_RCVBUF
/// disables automatic tuning of that socket's receive buffer size, in which
/// case this value is ignored". The cap for TCP's *automatic* tuning is
/// `tcp_rmem[2]` alone — `tcp_rcv_space_adjust()` grows the buffer with
/// `min_t(u64, ..., READ_ONCE(sock_net(sk)->ipv4.sysctl_tcp_rmem[2]))` — and
/// `sysctl_rmem_max` has no reader in the TCP receive path at all (tree-wide
/// it is consumed by the SO_RCVBUF clamp, the window-scale guess in
/// `tcp_select_initial_window()` and IPVS). Raising it lets applications
/// request bigger explicit buffers; it is not what makes the `tcp_rmem` max
/// effective.
fn eval_rmem_max_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.max_net_speed() < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 16777216 {
        recs.push(Recommendation {
            param: "net.core.rmem_max".to_string(),
            current_value: current.to_string(),
            recommended_value: "16777216".to_string(),
            reason: "万兆网卡场景下，应用显式 setsockopt(SO_RCVBUF) 能申请的上限偏小；TCP 自动调优的上限由 net.ipv4.tcp_rmem 的 max 单独决定，不受此值约束"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_wmem_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_wmem_max_at(info, recs, "/proc/sys/net/core/wmem_max")
}

/// Path-injectable form of [`eval_wmem_max`] (the `eval_*_at` idiom) so the
/// reason's claim is assertable against a temp file on any host.
///
/// The send-side twin of [`eval_rmem_max_at`]: `net.core.wmem_max` clamps an
/// explicit `setsockopt(SO_SNDBUF)` (`sock_setsockopt()` uses
/// `min_t(u32, val, READ_ONCE(sysctl_wmem_max))`), while TCP's automatic
/// tuning is capped by `tcp_wmem[2]` alone — `tcp_sndbuf_expand()` writes
/// `min(sndmem, READ_ONCE(sock_net(sk)->ipv4.sysctl_tcp_wmem[2]))`. Raising
/// it lets applications request bigger explicit buffers; it is not what makes
/// the `tcp_wmem` max effective.
fn eval_wmem_max_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.max_net_speed() < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 16777216 {
        recs.push(Recommendation {
            param: "net.core.wmem_max".to_string(),
            current_value: current.to_string(),
            recommended_value: "16777216".to_string(),
            reason: "万兆网卡场景下，应用显式 setsockopt(SO_SNDBUF) 能申请的上限偏小；TCP 自动调优的上限由 net.ipv4.tcp_wmem 的 max 单独决定，不受此值约束"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_rmem(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_rmem";
    if !info.param_exists(path) {
        return 1;
    }
    if info.max_net_speed() < 10000 {
        return 1;
    }
    let content = read_sysctl_string(path);
    let max_val = content
        .split_whitespace()
        .nth(2)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if max_val < 16777216 {
        // Raise-only per field: keep a default the administrator raised
        // above the fixed tuple's middle value instead of lowering it.
        let vals: Vec<u64> = content
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let recommended = if vals.len() == 3 {
            per_field_max(&vals, &[4096, 131072, 16777216])
        } else {
            "4096 131072 16777216".to_string()
        };
        recs.push(Recommendation {
            param: "net.ipv4.tcp_rmem".to_string(),
            current_value: content,
            recommended_value: recommended,
            reason: "万兆网卡场景下增大 TCP 接收缓冲区上限，充分利用带宽-延迟积".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_wmem(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_wmem";
    if !info.param_exists(path) {
        return 1;
    }
    if info.max_net_speed() < 10000 {
        return 1;
    }
    let content = read_sysctl_string(path);
    let max_val = content
        .split_whitespace()
        .nth(2)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(0);
    if max_val < 16777216 {
        // Raise-only per field: keep a default the administrator raised
        // above the fixed tuple's middle value instead of lowering it.
        let vals: Vec<u64> = content
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        let recommended = if vals.len() == 3 {
            per_field_max(&vals, &[4096, 65536, 16777216])
        } else {
            "4096 65536 16777216".to_string()
        };
        recs.push(Recommendation {
            param: "net.ipv4.tcp_wmem".to_string(),
            current_value: content,
            recommended_value: recommended,
            reason: "万兆网卡场景下增大 TCP 发送缓冲区上限，避免大流量传输时发送端瓶颈".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_slow_start_after_idle(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_slow_start_after_idle_at(
        info,
        recs,
        "/proc/sys/net/ipv4/tcp_slow_start_after_idle",
        "/proc/sys/net/ipv4/tcp_congestion_control",
    )
}

/// Path-injectable form of [`eval_tcp_slow_start_after_idle`] (the `eval_*_at`
/// idiom) so the value and the congestion control both come from test files
/// instead of the live /proc.
fn eval_tcp_slow_start_after_idle_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    congestion_control_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if let Some(rec) = slow_start_after_idle_recommendation(
        read_sysctl_i64(path),
        &read_sysctl_string(congestion_control_path),
        info.has_listen_sockets(),
    ) {
        recs.push(rec);
    }
    1
}

/// Value-driven core of the slow-start-after-idle rule (the `*_recommendation`
/// idiom): the value, the congestion control and the listener flag are all
/// parameters, so the gate is assertable on any host instead of only on one
/// that listens on TCP and runs a control without `cong_control`.
///
/// Both readers of the knob skip congestion controls that define
/// `cong_control` — `tcp_output.c`'s starvation branch ends
/// `... && !ca_ops->cong_control`, and the `include/net/tcp.h` helper returns
/// early on `ca_ops->cong_control` — and BBR is the in-tree algorithm that
/// defines it (`net/ipv4/tcp_bbr.c`: `.cong_control = bbr_main`). BBR is also
/// what this engine's own `net.ipv4.tcp_congestion_control` rule asks for, so
/// on a BBR host the write cannot change any cwnd behavior and the reason's
/// promise ("禁用后保持已探测的拥塞窗口") already describes what BBR does by
/// design: it never restarts slow start after an idle period.
///
/// `current` is i64 because the knob is a truthiness flag, not an enum: it is
/// consumed as `if (READ_ONCE(...sysctl_tcp_slow_start_after_idle) && ...)` in
/// net/ipv4/tcp_output.c, and through v5.10 it was registered as a plain
/// `proc_dointvec` int with no min/max (net/ipv4/sysctl_net_ipv4.c), where -1
/// is a legal, enabled value; since v5.14 the u8 registration
/// (`proc_dou8vec_minmax`, no extra1/extra2) accepts any 0-255 value, and
/// every nonzero one is enabled. The old `!= 1` gate missed both shapes — a
/// -1 host read as the fallback 0, and a u8 host holding 2 was skipped
/// outright — so the gate is now the kernel's own truthiness: any nonzero
/// value restarts slow start after idle and is worth disabling for long-lived
/// connections.
fn slow_start_after_idle_recommendation(
    current: i64,
    congestion_control: &str,
    has_listen_sockets: bool,
) -> Option<Recommendation> {
    if !has_listen_sockets || congestion_control == "bbr" || current == 0 {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.tcp_slow_start_after_idle".to_string(),
        current_value: current.to_string(),
        recommended_value: "0".to_string(),
        reason: "长连接空闲后重新慢启动会造成突发延迟，禁用后保持已探测的拥塞窗口".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_ip_local_port_range(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/ip_local_port_range";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let content = read_sysctl_string(path);
    let parts: Vec<u64> = content
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    if parts.len() == 2 {
        port_range_recommendation(parts[0], parts[1], recs);
    }
    1
}

/// Value-driven core of the port-range rule, separated so tests can force
/// every branch on any host (the sysctl is read from the live /proc).
///
/// Raise-only range semantics: the low endpoint an administrator chose is
/// never lowered (the old fixed "1024 65535" rewrite collapsed a range
/// deliberately narrowed for hardening, e.g. "50000 60000", back to the
/// full span). The fix widens upward from the current low endpoint; when
/// even the maximum high endpoint (65535) cannot reach the 30000-port
/// threshold, the hardening choice wins and nothing is recommended — an
/// unsatisfiable recommendation that fires on every check would be worse.
fn port_range_recommendation(lo: u64, hi: u64, recs: &mut Vec<Recommendation>) {
    if hi.saturating_sub(lo) >= 30_000 {
        return;
    }
    // Widen upward to the maximum port; if even that cannot reach the
    // threshold from the current low endpoint, the administrator's
    // hardening choice wins (see the doc comment above).
    if 65_535_u64.saturating_sub(lo) < 30_000 {
        return;
    }
    recs.push(Recommendation {
        param: "net.ipv4.ip_local_port_range".to_string(),
        current_value: format!("{lo} {hi}"),
        recommended_value: format!("{lo} 65535"),
        reason: "可用临时端口范围过小，高并发短连接场景下可能耗尽端口导致连接失败；在不降低起始端口（可能是安全加固）的前提下向上扩展".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    });
}

fn eval_default_qdisc(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let cc_path = "/proc/sys/net/ipv4/tcp_congestion_control";
    let qdisc_path = "/proc/sys/net/core/default_qdisc";
    if !info.param_exists(cc_path) || !info.param_exists(qdisc_path) {
        return 1;
    }
    let cc = read_sysctl_string(cc_path);
    if cc != "bbr" {
        return 1;
    }
    let qdisc = read_sysctl_string(qdisc_path);
    if qdisc == "fq" || qdisc == "fq_codel" {
        return 1;
    }

    let fq_available = is_qdisc_module_available("sch_fq");
    let fq_codel_available = is_qdisc_module_available("sch_fq_codel");
    let (target, reason) = if fq_available {
        (
            "fq",
            "BBR 拥塞控制依赖 fq 队列实现精确 pacing，当前 qdisc 会降低 BBR 效果",
        )
    } else if fq_codel_available {
        (
            "fq_codel",
            "BBR 推荐 fq 但当前内核不支持，退而使用 fq_codel（支持部分 pacing）",
        )
    } else {
        return 1;
    };
    recs.push(Recommendation {
        param: "net.core.default_qdisc".to_string(),
        current_value: qdisc,
        recommended_value: target.to_string(),
        reason: reason.to_string(),
        confidence: Confidence::High,
        category: Category::Performance,
        writable: true,
    });
    1
}

/// Module file suffixes kmod can load. Distributions ship compressed modules
/// (`.ko.zst` since kmod 29 — Ubuntu 24.04, Fedora, Arch; `.ko.xz` on older
/// Fedora/Alinux; `.ko.gz` legacy), and modprobe accepts every suffix, so the
/// probe must accept them all or an available qdisc looks unavailable.
const MODULE_SUFFIXES: [&str; 4] = [".ko", ".ko.zst", ".ko.xz", ".ko.gz"];

fn is_qdisc_module_available(module: &str) -> bool {
    // Non-destructive checks only. A previous version probed availability by
    // writing the candidate qdisc to net.core.default_qdisc and reading it
    // back, which mutated live kernel state as a side effect of read-only
    // commands (check/status/list/why/tune --dry-run) — the same bug class
    // fixed for is_param_writable in e9425244. Trade-off: qdiscs built
    // statically into the kernel (no loadable module, no .ko file) are not
    // detected here and won't be recommended, same as before that qdisc
    // ever ships as non-modular on the kernels ktuner targets.
    if std::path::Path::new(&format!("/sys/module/{module}")).exists() {
        return true;
    }
    if crate::detect::detect_runtime_env() == crate::detect::RuntimeEnv::Container {
        return false;
    }
    let uname = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let sched_dir = std::path::Path::new("/lib/modules")
        .join(uname.trim())
        .join("kernel/net/sched");
    qdisc_module_in(&sched_dir, module)
}

/// Whether `module` is present in a `kernel/net/sched`-style directory under
/// any loadable suffix. Pure filesystem lookup over a caller-supplied
/// directory so tests can exercise the suffix set without a real
/// `/lib/modules` tree.
fn qdisc_module_in(sched_dir: &std::path::Path, module: &str) -> bool {
    MODULE_SUFFIXES
        .iter()
        .any(|suffix| sched_dir.join(format!("{module}{suffix}")).exists())
}

fn eval_tcp_tw_reuse(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_tw_reuse";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_tw_reuse".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason:
                "允许复用 TIME_WAIT 状态的 socket 建立新的出站连接，减少高并发短连接场景的端口耗尽"
                    .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_fin_timeout(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_fin_timeout";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 30 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_fin_timeout".to_string(),
            current_value: current.to_string(),
            recommended_value: "15".to_string(),
            reason: "缩短 FIN_WAIT_2 超时时间，加速断开连接的资源回收".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_keepalive_time(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_keepalive_time";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 1800 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_keepalive_time".to_string(),
            current_value: current.to_string(),
            recommended_value: "600".to_string(),
            reason: "默认 7200 秒太长，缩短 keepalive 间隔可更早检测失效连接释放资源".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_congestion_control(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_congestion_control";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_string(path);
    if current != "bbr" {
        let avail_path = "/proc/sys/net/ipv4/tcp_available_congestion_control";
        if info.param_exists(avail_path) {
            let available = read_sysctl_string(avail_path);
            if congestion_algo_available(&available, "bbr") {
                recs.push(Recommendation {
                    param: "net.ipv4.tcp_congestion_control".to_string(),
                    current_value: current,
                    recommended_value: "bbr".to_string(),
                    reason:
                        "BBR 拥塞控制在云网络环境下比 cubic 表现更好，尤其是高延迟和有丢包的链路"
                            .to_string(),
                    confidence: Confidence::Medium,
                    category: Category::Performance,
                    writable: true,
                });
            }
        }
    }
    1
}

fn eval_nf_conntrack_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    if !info.has_conntrack() {
        return 1;
    }
    let path = if info.param_exists("/proc/sys/net/netfilter/nf_conntrack_max") {
        "/proc/sys/net/netfilter/nf_conntrack_max"
    } else {
        "/proc/sys/net/nf_conntrack_max"
    };
    let current = read_sysctl_u64(path);
    let recommended = if info.memory_total_gb >= 128 {
        2097152
    } else if info.memory_total_gb >= 32 {
        1048576
    } else {
        262144
    };
    if current < recommended {
        recs.push(Recommendation {
            param: "net.netfilter.nf_conntrack_max".to_string(),
            current_value: current.to_string(),
            recommended_value: recommended.to_string(),
            reason: format!(
                "conntrack 表满会导致新连接被丢弃，{}GB 内存建议增大到 {}",
                info.memory_total_gb, recommended
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_max_tw_buckets(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_max_tw_buckets";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 200000 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_max_tw_buckets".to_string(),
            current_value: current.to_string(),
            recommended_value: "200000".to_string(),
            reason:
                "TIME_WAIT bucket 上限过低，高并发短连接场景可能导致 socket 被强制回收引发连接异常"
                    .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_keepalive_intvl(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_keepalive_intvl";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 30 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_keepalive_intvl".to_string(),
            current_value: current.to_string(),
            recommended_value: "15".to_string(),
            reason: "缩短 keepalive 探测间隔，配合 keepalive_time 更快检测失效连接".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_keepalive_probes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_keepalive_probes";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 5 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_keepalive_probes".to_string(),
            current_value: current.to_string(),
            recommended_value: "5".to_string(),
            reason: "减少 keepalive 探测次数，默认 9 次太多，5 次足以确认连接失效".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_no_metrics_save(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_no_metrics_save_at(info, recs, "/proc/sys/net/ipv4/tcp_no_metrics_save")
}

/// Path-injectable form of [`eval_tcp_no_metrics_save`] (the `eval_*_at`
/// idiom) so the signed read is unit-testable against a temp file.
///
/// Through v5.10 net/ipv4/sysctl_net_ipv4.c registered tcp_no_metrics_save as
/// a plain `proc_dointvec` int with no min/max, so -1 is a legal value there,
/// and its only consumer is a truthiness test (net/ipv4/tcp_metrics.c:
/// `if (READ_ONCE(net->ipv4.sysctl_tcp_nometrics_save) || !dst) return;`) —
/// -1 means "do not save metrics", i.e. the recommended state. The unsigned
/// reader parsed "-1" to Err and fell back to 0 — the *saving* value — so
/// the `== 0` gate invented a finding on a host that already discards
/// cached metrics. Since v5.14 the knob is u8 (`proc_dou8vec_minmax`), where
/// negatives are rejected at write time; the signed reader keeps the
/// truthiness contract correct on both registrations.
fn eval_tcp_no_metrics_save_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value disables metrics saving, so -1 must not read as 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_no_metrics_save".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason:
                "TCP 连接关闭后缓存的路由指标可能过时，新连接继承错误的拥塞窗口大小导致性能异常"
                    .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_mtu_probing(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_mtu_probing_at(info, recs, "/proc/sys/net/ipv4/tcp_mtu_probing")
}

/// Path-injectable form of [`eval_tcp_mtu_probing`] (the `eval_*_at` idiom).
/// Through v5.10 net/ipv4/sysctl_net_ipv4.c registered tcp_mtu_probing as a
/// plain `proc_dointvec` int with no min/max, so -1 is a legal, persistent
/// value there, and the black-hole detector consumes it as a truthiness test
/// (net/ipv4/tcp_timer.c: `if (!READ_ONCE(net->ipv4.sysctl_tcp_mtu_probing))`
/// skips the probe); only the always-probe mode is the documented `> 1` check
/// (net/ipv4/tcp_output.c). The unsigned reader parsed "-1" to Err and fell
/// back to 0 — the *disabled* value — so the `== 0` gate invented a finding
/// on a host whose black-hole probing is already armed. Since v5.14 the knob
/// is u8 (`proc_dou8vec_minmax`, no extra1/extra2), where negatives are
/// rejected at write time; the signed reader keeps the truthiness contract
/// correct on both registrations.
fn eval_tcp_mtu_probing_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_mtu_probing".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "启用 MTU 探测避免 PMTU 黑洞问题，某些网络路径会丢弃大包导致连接卡住"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

// ─── VM/Memory Rules ─────────────────────────────────────────────────────────

fn eval_max_map_count(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/max_map_count";
    if !info.param_exists(path) {
        return 1;
    }
    let needs_high = info.has_process("java")
        || info.has_process("elasticsearch")
        || info.has_process_exact("node"); // exact: avoid matching node_exporter
    if !needs_high {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 262144 {
        recs.push(Recommendation {
            param: "vm.max_map_count".to_string(),
            current_value: current.to_string(),
            recommended_value: "262144".to_string(),
            reason: "JVM/Node 进程需要大量内存映射，max_map_count 过低会导致 mmap 失败".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_zone_reclaim_mode(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/zone_reclaim_mode";
    if !info.param_exists(path) {
        return 1;
    }
    if info.numa_nodes <= 1 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "vm.zone_reclaim_mode".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "多 NUMA 节点下 zone_reclaim 会导致频繁本地回收而非跨节点分配，增大延迟抖动"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_vfs_cache_pressure(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/vfs_cache_pressure";
    if !info.param_exists(path) {
        return 1;
    }
    let is_db_or_cache = info.has_process("postgres")
        || info.has_process("mysqld")
        || info.has_process("clickhouse")
        || info.has_process("redis-server")
        || info.has_process("memcached")
        || info.has_process("etcd")
        || info.has_process("elasticsearch");
    if !is_db_or_cache {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 60 {
        recs.push(Recommendation {
            param: "vm.vfs_cache_pressure".to_string(),
            current_value: current.to_string(),
            recommended_value: "50".to_string(),
            reason: "数据库/缓存场景降低 VFS 缓存回收压力，保留更多 dentry/inode 缓存减少查找开销"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_watermark_scale_factor(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/watermark_scale_factor";
    if !info.param_exists(path) {
        return 1;
    }
    if info.memory_total_gb < 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 200 {
        recs.push(Recommendation {
            param: "vm.watermark_scale_factor".to_string(),
            current_value: current.to_string(),
            recommended_value: "200".to_string(),
            reason: "大内存机器增大水位线间距，让 kswapd 更早唤醒减少直接回收触发的概率"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_file_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/file-max";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 1000000 {
        recs.push(Recommendation {
            param: "fs.file-max".to_string(),
            current_value: current.to_string(),
            recommended_value: "2000000".to_string(),
            reason: "系统级文件描述符上限过低，高并发网络服务或大量文件操作可能触及限制"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_overcommit_memory(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/overcommit_memory";
    if !info.param_exists(path) {
        return 1;
    }
    let needs_overcommit = info.has_process("redis-server");
    if !needs_overcommit {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "vm.overcommit_memory".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "Redis 使用 fork 进行 RDB/AOF 持久化，overcommit_memory=0 可能导致 fork 失败"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

// ─── IO/Disk Rules ───────────────────────────────────────────────────────────

fn eval_read_ahead_kb(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let is_streaming = info.has_process("kafka")
        || info.has_process("flink")
        || info.has_process("spark")
        || info.has_process("hadoop");

    let is_db = info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd — the same OLTP database as mysqld.
        || info.has_process("mariadbd")
        || info.has_process("mongod")
        || info.has_process("clickhouse");

    for disk in &info.disks {
        match disk.disk_type {
            DiskType::HDD if is_streaming && disk.read_ahead_kb < 2048 => {
                recs.push(Recommendation {
                    param: format!("block/{}/read_ahead_kb", disk.name),
                    current_value: disk.read_ahead_kb.to_string(),
                    recommended_value: "2048".to_string(),
                    reason: "机械硬盘顺序读取场景，增大预读窗口提升吞吐（减少磁头寻道次数）"
                        .to_string(),
                    confidence: Confidence::Medium,
                    category: Category::Performance,
                    writable: true,
                });
            }
            DiskType::NVMe | DiskType::SSD if is_db && disk.read_ahead_kb > 128 => {
                recs.push(Recommendation {
                    param: format!("block/{}/read_ahead_kb", disk.name),
                    current_value: disk.read_ahead_kb.to_string(),
                    recommended_value: "128".to_string(),
                    reason:
                        "数据库随机 IO 为主，过大的预读会浪费内存和 IO 带宽，NVMe/SSD 延迟已经很低"
                            .to_string(),
                    confidence: Confidence::Medium,
                    category: Category::Performance,
                    writable: true,
                });
            }
            _ => {}
        }
    }
    1
}

fn eval_nr_requests(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    for disk in &info.disks {
        // Without an elevator the kernel caps nr_requests at the hardware tag
        // depth (larger writes fail with EINVAL), and switching to `none`
        // resets it to that depth, so under `none` the value cannot be raised.
        let scheduler = nvme_scheduler_target(disk).unwrap_or(&disk.scheduler);
        if disk.disk_type == DiskType::NVMe && disk.nr_requests < 256 && scheduler != "none" {
            recs.push(Recommendation {
                param: format!("block/{}/nr_requests", disk.name),
                current_value: disk.nr_requests.to_string(),
                recommended_value: "1024".to_string(),
                reason: "NVMe 硬件队列深度大，增大软件请求队列避免高并发 IO 时提前拥塞".to_string(),
                confidence: Confidence::High,
                category: Category::Performance,
                writable: true,
            });
        }
    }
    1
}

fn eval_rq_affinity(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    if info.numa_nodes <= 1 {
        return 1;
    }
    for disk in &info.disks {
        if disk.disk_type == DiskType::NVMe && disk.rq_affinity != 2 {
            recs.push(Recommendation {
                param: format!("block/{}/rq_affinity", disk.name),
                current_value: disk.rq_affinity.to_string(),
                recommended_value: "2".to_string(),
                reason: "多 NUMA 节点下强制 IO 完成中断在提交 CPU 上处理，减少跨节点内存访问"
                    .to_string(),
                confidence: Confidence::High,
                category: Category::Performance,
                writable: true,
            });
        }
    }
    1
}

// ─── CPU/Scheduler Rules ─────────────────────────────────────────────────────

fn eval_numa_balancing(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_numa_balancing_at(info, recs, "/proc/sys/kernel/numa_balancing")
}

/// Path-injectable form (the `eval_*_at` idiom) so the database gate is
/// unit-testable against a temp file instead of the live /proc.
fn eval_numa_balancing_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.numa_nodes <= 1 {
        return 1;
    }
    let is_db = info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd — the same OLTP database as mysqld.
        || info.has_process("mariadbd")
        || info.has_process("mongod")
        || info.has_process("clickhouse");
    if !is_db {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "kernel.numa_balancing".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "数据库进程自行管理内存亲和性，内核 NUMA balancing 的页面迁移会引发延迟抖动"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_autogroup(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_autogroup_enabled";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "kernel.sched_autogroup_enabled".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器环境下 autogroup 按 TTY 分组调度不适用，关闭后避免不合理的 CPU 带宽分配"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_pid_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/pid_max";
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores <= 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 4194304 {
        recs.push(Recommendation {
            param: "kernel.pid_max".to_string(),
            current_value: current.to_string(),
            recommended_value: "4194304".to_string(),
            reason: format!(
                "{}核 CPU 并发进程/线程量大，默认 pid_max 可能不够用",
                info.cpu_cores
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_migration_cost(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_sched_migration_cost_at(info, recs, "/proc/sys/kernel/sched_migration_cost_ns")
}

/// Path-injectable form (the `eval_*_at` idiom) so the signed read is
/// unit-testable against a temp file. `kernel/sched/fair.c` declares
/// `sysctl_sched_migration_cost` unsigned but registers it through
/// `proc_dointvec` with no min/max, so "-1" round-trips verbatim — and
/// `task_hot()` special-cases it: -1 keeps every task cache-hot (migration
/// effectively disabled) while 0 makes no task cache-hot (always migrate).
fn eval_sched_migration_cost_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores <= 16 {
        return 1;
    }
    // -1 is the kernel's "never migrate" sentinel, so it must be read signed
    // or the unsigned fallback maps it to 0, the opposite "always migrate"
    // policy, and the report misdiagnoses a pinned host.
    let current = read_sysctl_i64(path);
    if current < 5000000 {
        recs.push(Recommendation {
            param: "kernel.sched_migration_cost_ns".to_string(),
            current_value: current.to_string(),
            recommended_value: "5000000".to_string(),
            reason: format!(
                "{}核机器线程迁移开销大，增大 migration_cost 让调度器倾向于保持线程在同一 CPU 上运行",
                info.cpu_cores
            ),
            confidence: Confidence::Medium,
            category: Category::Performance, writable: true,
        });
    }
    1
}

// ─── Additional Security Rules ───────────────────────────────────────────────

fn eval_tcp_syncookies(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_syncookies_at(info, recs, "/proc/sys/net/ipv4/tcp_syncookies")
}

/// Path-injectable form of [`eval_tcp_syncookies`] (the `eval_*_at` idiom) so
/// the signed read is unit-testable against a temp file.
///
/// Through v5.10 net/ipv4/sysctl_net_ipv4.c registered tcp_syncookies as a
/// plain `proc_dointvec` int with no min/max, so -1 is a legal value there,
/// and every consumer is a truthiness test (net/ipv4/tcp_input.c:
/// `if (syncookies)` arms the cookie path, `if (!syncookies && ...)` guards
/// the last-quarter reservation, and `syncookies == 2` selects the documented
/// always-cookie mode) — so -1 means enabled, exactly what this rule asks
/// for. The unsigned reader parsed "-1" to Err and fell back to 0 — the
/// *disabled* value — so the `== 0` gate invented a SYN-flood finding on a
/// host that already answers floods with cookies. Since v5.14 the knob is
/// u8 (`proc_dou8vec_minmax`, no extra1/extra2), where negatives are
/// rejected at write time; the signed reader keeps the truthiness contract
/// correct on both registrations.
fn eval_tcp_syncookies_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    // Any nonzero value arms the cookie path, so -1 must not read as 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_syncookies".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未启用 SYN Cookie 防护，遭受 SYN Flood 时半连接队列会迅速溢出导致拒绝服务"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_send_redirects(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_send_redirects_at(
        info,
        recs,
        "/proc/sys/net/ipv4/conf/all/send_redirects",
        std::path::Path::new("/proc/sys/net/ipv4/conf"),
    )
}

/// Path-injectable form of [`eval_send_redirects`] (the `eval_*_at` idiom) so
/// the forwarding precondition is assertable against a synthetic conf tree.
///
/// The knob only decides whether this host *sends* an ICMP redirect, and a
/// redirect is sent from exactly one place: `ip_forward()` (net/ipv4/
/// ip_forward.c) calls `ip_rt_send_redirect()`, which re-checks
/// `IN_DEV_TX_REDIRECTS` before anything goes out. A host whose input
/// interface does not forward never reaches that code — `ip_route_input_slow()`
/// (net/ipv4/route.c) turns a non-local destination into an error route
/// instead:
///
/// ```text
///    if (!IN_DEV_FORWARD(in_dev)) {
///        err = -EHOSTUNREACH;
///        goto no_route;
///    }
/// ```
///
/// which is what the kernel documentation summarises as "Send redirects, if
/// router" (Documentation/networking/ip-sysctl.rst). This engine recommends
/// `net.ipv4.ip_forward = 0` on every host without containers/VPNs/routers, so
/// after its own plan the write cannot change anything there. An unreadable
/// conf tree counts as "no forwarding", so the rule stays quiet when it cannot
/// verify that a redirect could ever be sent.
///
/// The value is read SIGNED: `devinet_conf_proc` (net/ipv4/devinet.c) routes
/// every `conf/<iface>/…` entry through a plain `proc_dointvec` on an int
/// slot of `struct ipv4_devconf` with no min/max, so -1 is a legal value, and
/// `IN_DEV_TX_REDIRECTS` (include/linux/inetdevice.h) consumes it through
/// `IN_DEV_ORCONF` — a truthiness test. The old `== 1` gate skipped the rule
/// on a host whose redirects are enabled with -1, while the unsigned reader
/// parsed "-1" to Err and fell back to 0, the *disabled* value.
fn eval_send_redirects_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    conf_root: &std::path::Path,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !any_interface_forwards(conf_root) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.send_redirects".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器不应发送 ICMP 重定向，避免被利用进行网络拓扑探测或路由劫持".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

/// Whether any interface — every per-interface entry plus the `all` and
/// `default` templates — has forwarding enabled, which is what makes the
/// redirect path reachable at all (see [`eval_send_redirects_at`]). An
/// unreadable or missing tree reports `false`, so a rule that promises to stop
/// redirects it cannot prove are possible stays quiet.
fn any_interface_forwards(conf_root: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(conf_root) else {
        return false;
    };
    for entry in entries.filter_map(|entry| entry.ok()) {
        let forwarding = entry.path().join("forwarding");
        if read_sysctl_u64(&forwarding.to_string_lossy()) != 0 {
            return true;
        }
    }
    false
}

fn eval_perf_event_paranoid(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_perf_event_paranoid_at(info, recs, "/proc/sys/kernel/perf_event_paranoid")
}

/// Path-injectable form so the signed parse is unit-testable against a temp
/// file. The upstream perf_event_paranoid sysctl has no upper bound of 2;
/// -1 permits unprivileged perf events. An unsigned parse turns -1 into the
/// fallback 0, misreporting the current value and losing the original on
/// rollback. Read signed, mirroring eval_sched_rt_runtime.
fn eval_perf_event_paranoid_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse::<i64>()
        .unwrap_or(0);
    if current < 2 {
        recs.push(Recommendation {
            param: "kernel.perf_event_paranoid".to_string(),
            current_value: current.to_string(),
            recommended_value: "2".to_string(),
            reason: "perf_event 权限过于宽松，非特权用户可采集性能计数器信息辅助侧信道攻击"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_rp_filter(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/rp_filter";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.rp_filter".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未启用反向路径过滤，攻击者可伪造源 IP 进行欺骗（注意：非对称/多路径路由、部分 VPN 场景需保持 0）".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security, writable: true,
        });
    }
    1
}

fn eval_panic(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_panic_at(info, recs, "/proc/sys/kernel/panic")
}

/// Path-injectable form (the `eval_*_at` idiom) so the signed read is
/// unit-testable against a temp file. `kernel/reboot.c` registers
/// kernel.panic through plain `proc_dointvec`, which copies the table with
/// no min/max, and -1 is the documented "reboot immediately, without
/// syncing" setting (`panic=-1` in kernel-parameters.txt). The unsigned
/// reader parses "-1" to Err and falls back to 0, which here is the
/// *never-reboot* value, so the rule fired on exactly the most
/// crash-resilient hosts — and the current_value it recorded, "0", is what
/// a rollback would restore, silently discarding the immediate-reboot
/// policy.
fn eval_panic_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // panic is a plain int; -1 reboots immediately, so it must be read signed
    // or the unsigned fallback maps it to 0 (never reboot) and fires the rule.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.panic".to_string(),
            current_value: "0".to_string(),
            recommended_value: "10".to_string(),
            reason: "内核 panic 后不自动重启，服务器会一直挂起直到人工干预。设为 10 秒后自动重启"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_panic_on_oom(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/panic_on_oom";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "vm.panic_on_oom".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "OOM 时应让 OOM killer 杀进程而非触发 kernel panic，保持系统可用性".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_dirty_expire_centisecs(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/dirty_expire_centisecs";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 1500 {
        recs.push(Recommendation {
            param: "vm.dirty_expire_centisecs".to_string(),
            current_value: current.to_string(),
            recommended_value: "1500".to_string(),
            reason: "缩短脏页过期时间，避免长时间积压导致突发刷盘造成 IO 延迟尖刺".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_dirty_writeback_centisecs(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/dirty_writeback_centisecs";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 500 {
        recs.push(Recommendation {
            param: "vm.dirty_writeback_centisecs".to_string(),
            current_value: current.to_string(),
            recommended_value: "500".to_string(),
            reason: "缩短回写线程唤醒间隔，更及时地将脏页刷到磁盘".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// Whether the host appears to legitimately need IP forwarding (router,
/// hypervisor, container host, VPN/NAT gateway). Disabling forwarding on such a
/// host silently breaks guest/pod/tunnel traffic, so we must be conservative
/// and only suggest turning it off when we see no sign forwarding is in use.
fn host_needs_ip_forward(info: &SystemInfo) -> bool {
    const PROCS: &[&str] = &[
        // container runtimes
        "docker",
        "dockerd",
        "kubelet",
        "containerd",
        "podman",
        "conmon",
        "crio",
        "k3s",
        "lxc",
        "lxd",
        // virtualization
        "libvirtd",
        "qemu",
        "virtqemud",
        "vhost",
        // VPN / tunneling
        "openvpn",
        "wireguard",
        "charon",
        "strongswan",
        "tincd",
        "tailscaled",
        // routing daemons
        "bird",
        "bird2",
        "zebra",
        "bgpd",
        "ospfd",
        "frr",
        "quagga",
    ];
    if PROCS.iter().any(|p| info.has_process(p)) {
        return true;
    }
    // Bridge / tunnel / virtual interfaces are a strong signal of VM, container
    // or VPN networking. (info.network filters these out, so scan directly.)
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for e in entries.filter_map(|e| e.ok()) {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("virbr")
                || n.starts_with("docker")
                || n.starts_with("br-")
                || n.starts_with("cni")
                || n.starts_with("flannel")
                || n.starts_with("cali")
                || n.starts_with("tun")
                || n.starts_with("tap")
                || n.starts_with("wg")
                || n.starts_with("vxlan")
                || n == "br0"
                || n == "br1"
            {
                return true;
            }
        }
    }
    false
}

/// Whether a `/proc/net/bonding`-style directory actually contains a bond.
///
/// The bonding module creates this directory from its pernet init
/// (`bond_create_proc_dir`) as soon as it is loaded — even with
/// `max_bonds=0` and no bond interface — so the directory's mere existence
/// is not evidence of bonding. The kernel puts one file per bond into it,
/// so a non-empty directory is the signal.
fn proc_bonding_has_bonds(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// Whether the host uses link bonding. The ARP-tuning rules key off "2+ NICs",
/// but bond members are multiple NICs forming ONE logical link where settings
/// like arp_filter/arp_ignore can break the bond, so they must skip bonded hosts.
fn has_bond() -> bool {
    if proc_bonding_has_bonds(std::path::Path::new("/proc/net/bonding")) {
        return true;
    }
    dir_has_bond(std::path::Path::new("/sys/class/net"))
}

/// Skip guard shared by the whole ARP-tuning family — the four `conf/all`
/// rules and the two `conf/default` rules. Hosts qualify only with 2+
/// interfaces and no bond: a bonded host lists the bond master AND its
/// slaves, so `network.len() >= 2` holds trivially, yet per `has_bond` the
/// ARP tweaks must not be recommended there. `bond_present` is `has_bond()`
/// in production and injected in tests (via `dir_has_bond` on a synthetic
/// `/sys/class/net`).
fn arp_tuning_skipped(net_ifaces: usize, bond_present: bool) -> bool {
    net_ifaces < 2 || bond_present
}

/// Whether a `/sys/class/net`-style directory lists an actual bond interface.
///
/// The kernel's `bonding_masters` control file appears in this directory
/// whenever the bonding module is loaded — even with zero bonds configured —
/// so the exact name is not a bond. `detect::read_network_info` skips it for
/// the same reason.
fn dir_has_bond(dir: &std::path::Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.filter_map(|e| e.ok()) {
            let name = e.file_name();
            if name == *"bonding_masters" {
                continue;
            }
            if name.to_string_lossy().starts_with("bond") {
                return true;
            }
        }
    }
    false
}

fn eval_ip_forward(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_ip_forward_at(info, recs, "/proc/sys/net/ipv4/ip_forward")
}

/// Path-injectable form of [`eval_ip_forward`] (the `eval_*_at` idiom) so the
/// signed read is unit-testable against a temp file.
///
/// `ctl_forward_entry` (net/ipv4/devinet.c) routes net.ipv4.ip_forward
/// through `devinet_sysctl_forward`, which calls a plain `proc_dointvec` on
/// the int FORWARDING slot — its `extra1`/`extra2` are the devconf and net
/// context, not min/max bounds — so any int is accepted, -1 included. The
/// consumers read it as a truthiness flag (`IN_DEV_FORWARD` in
/// include/linux/inetdevice.h; `if (*valp)` in the handler's own fan-out),
/// so -1 means forwarding is on, exactly the state this rule asks a
/// non-router host to turn off. The unsigned reader parsed "-1" to Err and
/// fell back to 0, and the old `== 1` gate then skipped the rule on the
/// host that most looks like a jumpbox.
fn eval_ip_forward_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if let Some(rec) = ip_forward_recommendation(read_sysctl_i64(path), host_needs_ip_forward(info))
    {
        recs.push(rec);
    }
    1
}

/// Value-driven core of the ip_forward rule (the `*_recommendation` idiom):
/// the value and the router verdict are parameters, so the truthiness gate
/// is assertable on any host — `host_needs_ip_forward` reads the live
/// process list and `/sys/class/net`, which a unit test cannot control.
/// Any nonzero value forwards (see [`eval_ip_forward_at`] for the
/// registration and consumers), so -1 and 2 are as on as 1.
fn ip_forward_recommendation(current: i64, host_needs_forwarding: bool) -> Option<Recommendation> {
    if current == 0 || host_needs_forwarding {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.ip_forward".to_string(),
        current_value: current.to_string(),
        recommended_value: "0".to_string(),
        reason: "未检测到容器/虚拟化/VPN/路由用途，关闭 IP 转发可防止被用作中间人或跳板（若本机需转发请忽略）".to_string(),
        confidence: Confidence::Medium,
        category: Category::Security, writable: true,
    })
}

fn eval_unprivileged_bpf(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/unprivileged_bpf_disabled";
    if !info.param_exists(path) {
        return 1;
    }
    recommend_unprivileged_bpf(read_sysctl_u64(path), recs);
    1
}

/// Push the recommendation for a given `kernel.unprivileged_bpf_disabled` value.
///
/// Split from the probe so the rule is assertable on every host: 0 is the only
/// value worth changing, and the target has to be 2 rather than 1.
fn recommend_unprivileged_bpf(current: u64, recs: &mut Vec<Recommendation>) {
    if current != 0 {
        return;
    }
    recs.push(Recommendation {
        param: "kernel.unprivileged_bpf_disabled".to_string(),
        current_value: "0".to_string(),
        // 1 and 2 both deny unprivileged bpf(), but the kernel refuses to
        // clear a 1 for the rest of the boot, so only 2 lets
        // `ktuner rollback` restore the previous state.
        recommended_value: "2".to_string(),
        reason: "非特权用户可加载 BPF 程序存在提权风险，应禁止".to_string(),
        confidence: Confidence::High,
        category: Category::Security,
        writable: true,
    });
}

fn eval_core_uses_pid(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_core_uses_pid_at(
        info,
        recs,
        "/proc/sys/kernel/core_uses_pid",
        "/proc/sys/kernel/core_pattern",
    )
}

/// Path-injectable form of [`eval_core_uses_pid`] (the `eval_*_at` idiom).
/// `fs/coredump.c` registers kernel.core_uses_pid through plain
/// `proc_dointvec` with no min/max, and the same file consumes it as a
/// boolean (`if (!ispipe && !pid_in_pattern && core_uses_pid)`), so -1 is a
/// legal, already-enabled value. The unsigned reader turns "-1" into the
/// fallback 0 — the *not-enabled* value — so the `== 0` gate invented the
/// recommendation on a host that already appends the PID.
///
/// That same condition also says when there is nothing left to recommend:
/// `core_uses_pid` is only the backward-compatibility half of the core-file
/// name, and `pid_in_pattern` is set by a literal `%p` — the very specifier
/// whose absence the reason complains about. A piped pattern
/// (systemd-coredump ships `|/usr/lib/systemd/systemd-coredump %P %u ...`)
/// never reaches the filename logic at all. Where either holds, the write
/// cannot add a PID the kernel already records, so the reason's promise is
/// already met — the "never recommend a no-op" rule the hardlockup_panic and
/// page-cluster gates follow. An unreadable pattern keeps the recommendation:
/// the kernel's default "core", which does use the knob, is what a failed
/// read leaves behind.
fn eval_core_uses_pid_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    pattern_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if let Ok(pattern) = std::fs::read_to_string(pattern_path) {
        if core_pattern_records_the_pid(&pattern) {
            return 1;
        }
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.core_uses_pid".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "core dump 文件名应包含 PID，避免多进程崩溃时相互覆盖导致调试信息丢失"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

/// Whether `core_pattern` already records the crashing pid, so that
/// `core_uses_pid` has nothing to add. Mirrors `format_corename()`
/// (fs/coredump.c): a piped pattern bypasses the filename logic, and
/// `pid_in_pattern` is set by a literal `%p` specifier. `%P` (global pid) and
/// an escaped `%%` do not set it, so the scan walks the `%` escapes instead of
/// string-matching the raw text — `core.%%p` names the file with a literal
/// `%p` and still gets the `.PID` compatibility suffix.
fn core_pattern_records_the_pid(pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.starts_with('|') {
        return true;
    }
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('p') => return true,
                // `%%` emits one literal percent; every other specifier
                // consumes its own character and none of them marks the pid.
                Some(_) => {}
                None => break,
            }
        }
    }
    false
}

fn eval_yama_ptrace_scope(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/yama/ptrace_scope";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.yama.ptrace_scope".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "限制 ptrace 仅允许父进程调试子进程，防止任意进程注入攻击".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_log_martians(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_log_martians_at(info, recs, "/proc/sys/net/ipv4/conf/all/log_martians")
}

/// Path-injectable form of [`eval_log_martians`] (the `eval_*_at` idiom).
/// `net/ipv4/devinet.c`'s `devinet_conf_proc` routes every `conf/<iface>/…`
/// entry through a plain `proc_dointvec` on an `int` slot of
/// `struct ipv4_devconf` (include/linux/inetdevice.h), with no min/max, so -1
/// is a legal value; `IN_DEV_LOG_MARTIANS` (inetdevice.h) reads it through
/// `IN_DEV_ORCONF` — a truthiness test — and `net/ipv4/route.c` consumes it as
/// `if (IN_DEV_LOG_MARTIANS(in_dev))`. The unsigned reader parses "-1" to Err
/// and falls back to 0, the *disabled* value, so the `== 0` gate invented the
/// recommendation on a host that already logs martians.
fn eval_log_martians_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.log_martians".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "启用火星包日志记录，帮助检测 IP 地址欺骗和路由异常".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_tcp_max_orphans(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_max_orphans";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let recommended = if info.memory_total_gb >= 128 {
        262144
    } else if info.memory_total_gb >= 32 {
        131072
    } else {
        65536
    };
    if current < recommended {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_max_orphans".to_string(),
            current_value: current.to_string(),
            recommended_value: recommended.to_string(),
            reason: format!(
                "孤儿 TCP 连接上限过低（{}GB 内存建议 {}），超限后连接被直接 RST 导致服务中断",
                info.memory_total_gb, recommended
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_threads_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/threads-max";
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores <= 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let recommended = (info.cpu_cores as u64 * 8192).min(4194304);
    if current < recommended / 2 {
        recs.push(Recommendation {
            param: "kernel.threads-max".to_string(),
            current_value: current.to_string(),
            recommended_value: recommended.to_string(),
            reason: format!(
                "{}核机器最大线程数偏低，大量线程创建时可能触及限制导致 fork/clone 失败",
                info.cpu_cores
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// Huge pages covering a quarter of memory. `vm.nr_hugepages` counts pages of
/// the default huge page size, which is 2 MiB only on common x86 setups (512
/// MiB on 64K-page aarch64, 1 GiB with `default_hugepagesz=1G`). 0 when the
/// size is unknown, so no count is guessed.
fn quarter_memory_hugepages(memory_gb: u64, hugepage_kb: u64) -> u64 {
    if hugepage_kb == 0 {
        return 0;
    }
    memory_gb * 1024 * 1024 / 4 / hugepage_kb
}

fn eval_nr_hugepages(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_nr_hugepages_at(
        info,
        recs,
        "/proc/sys/vm/nr_hugepages",
        crate::detect::read_default_hugepage_kb(),
    )
}

/// Path- and page-size-injectable form (the `eval_*_at` idiom): the live probe
/// reads the count from /proc and the default huge-page size from
/// /proc/meminfo, so injecting both is the only way to assert either branch on
/// any host.
///
/// The gate is the shared boundary-aware database predicate, not the
/// hand-rolled `postgres || mysqld` list: MariaDB 10.4+ runs as `mariadbd`
/// (the daemon comm Debian and Ubuntu ship), so a MariaDB-only host missed
/// the rule while the sibling SysV IPC sizing rules already counted it.
fn eval_nr_hugepages_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    hugepage_kb: u64,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !is_database_present(info) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let recommended = quarter_memory_hugepages(info.memory_total_gb, hugepage_kb);
    if current == 0 && info.memory_total_gb >= 16 && recommended > 0 {
        recs.push(Recommendation {
            param: "vm.nr_hugepages".to_string(),
            current_value: "0".to_string(),
            recommended_value: recommended.to_string(),
            reason:
                "数据库未启用 HugePages，启用后可减少 TLB miss 和页表开销，提升内存密集型查询性能"
                    .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_shmmax(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_shmmax_at(info, recs, "/proc/sys/kernel/shmmax")
}

/// Path-injectable form (the `eval_*_at` idiom) so the database gate is
/// unit-testable against a temp file instead of the live /proc.
///
/// The gate is the shared boundary-aware database predicate, like the sibling
/// SysV IPC rules: `kernel.shmmax` sizes the very shared-memory segment MariaDB
/// and PostgreSQL keep their buffer pools in, and the hand-rolled
/// `postgres || mysqld` list skipped the MariaDB 10.4+ `mariadbd` daemon comm.
fn eval_shmmax_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !is_database_present(info) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let mem_bytes = info.memory_total_gb * 1024 * 1024 * 1024;
    let recommended = mem_bytes / 2;
    if current < recommended {
        recs.push(Recommendation {
            param: "kernel.shmmax".to_string(),
            current_value: current.to_string(),
            recommended_value: recommended.to_string(),
            reason: "数据库使用共享内存进行缓冲池管理，shmmax 过低会限制可用的共享内存段大小"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_timestamps(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_timestamps_at(info, recs, "/proc/sys/net/ipv4/tcp_timestamps")
}

/// Path-injectable form of [`eval_tcp_timestamps`] (the `eval_*_at` idiom).
/// Through v5.10 net/ipv4/sysctl_net_ipv4.c registered tcp_timestamps as a
/// plain `proc_dointvec` int with no min/max, so -1 is a legal, persistent
/// value there, and every consumer is a truthiness test (net/ipv4/
/// tcp_output.c: `if (likely(READ_ONCE(sock_net(sk)->ipv4.
/// sysctl_tcp_timestamps) && !*md5))` arms the SYN option; net/ipv4/
/// tcp_input.c reads the same shape). The unsigned reader parsed "-1" to
/// Err and fell back to 0 — the *disabled* value — so the `== 0` gate
/// invented a finding on a host whose timestamps are on. Since v5.14 the
/// knob is u8 (`proc_dou8vec_minmax`, no extra1/extra2), where negatives
/// are rejected at write time; the signed reader keeps the truthiness
/// contract correct on both registrations.
fn eval_tcp_timestamps_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_timestamps".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "TCP 时间戳用于精确 RTT 计算和 PAWS 防序号回绕，关闭会影响性能和可靠性"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_window_scaling(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_window_scaling_at(info, recs, "/proc/sys/net/ipv4/tcp_window_scaling")
}

/// Path-injectable form of [`eval_tcp_window_scaling`] (the `eval_*_at`
/// idiom). Through v5.10 net/ipv4/sysctl_net_ipv4.c registered
/// tcp_window_scaling as a plain `proc_dointvec` int with no min/max, so -1
/// is a legal, persistent value there, and every consumer is a truthiness
/// test (net/ipv4/tcp_output.c: `if (likely(READ_ONCE(sock_net(sk)->ipv4.
/// sysctl_tcp_window_scaling)))` arms the window-scale option; net/ipv4/
/// tcp_input.c reads the same shape). The unsigned reader parsed "-1" to
/// Err and fell back to 0 — the *disabled* value — so the `== 0` gate
/// invented a finding on a host whose window scaling is on. Since v5.14
/// the knob is u8 (`proc_dou8vec_minmax`, no extra1/extra2), where
/// negatives are rejected at write time; the signed reader keeps the
/// truthiness contract correct on both registrations.
fn eval_tcp_window_scaling_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_window_scaling".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "关闭窗口缩放会限制 TCP 窗口最大 64KB，无法利用高带宽网络".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_ecn(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_ecn";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 && info.has_listen_sockets() {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_ecn".to_string(),
            current_value: "0".to_string(),
            recommended_value: "2".to_string(),
            reason: "ECN（显式拥塞通知）可在不丢包的情况下感知拥塞，设 2 表示仅在对端请求时启用"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_sack(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_sack_at(info, recs, "/proc/sys/net/ipv4/tcp_sack")
}

/// Path-injectable form of [`eval_tcp_sack`] (the `eval_*_at` idiom).
/// Through v5.10 net/ipv4/sysctl_net_ipv4.c registered tcp_sack as a plain
/// `proc_dointvec` int with no min/max, so -1 is a legal, persistent value
/// there, and every consumer is a truthiness test (net/ipv4/tcp_input.c:
/// `!estab && READ_ONCE(net->ipv4.sysctl_tcp_sack)` permits the SACK-perm
/// option; net/ipv4/tcp_output.c: `if (likely(READ_ONCE(sock_net(sk)->ipv4.
/// sysctl_tcp_sack)))`). The unsigned reader parsed "-1" to Err and fell
/// back to 0 — the *disabled* value — so the `== 0` gate invented a
/// finding on a host whose SACK is on. Since v5.14 the knob is u8
/// (`proc_dou8vec_minmax`, no extra1/extra2), where negatives are rejected
/// at write time; the signed reader keeps the truthiness contract correct
/// on both registrations.
fn eval_tcp_sack_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_sack".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "选择性确认（SACK）允许接收方告知发送方哪些段已收到，大幅减少不必要的重传"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_netdev_budget(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/netdev_budget";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if info.max_net_speed() >= 10000 && current < 600 {
        recs.push(Recommendation {
            param: "net.core.netdev_budget".to_string(),
            current_value: current.to_string(),
            recommended_value: "600".to_string(),
            reason: "万兆网络每次 NAPI 轮询处理的最大包数不够，增大可降低软中断频率提升吞吐"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_busy_poll(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/busy_poll";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let is_latency_sensitive = info.has_process("redis-server")
        || info.has_process("memcached")
        || info.has_process("nginx")
        || info.has_process("clickhouse");
    if is_latency_sensitive && current == 0 {
        recs.push(Recommendation {
            param: "net.core.busy_poll".to_string(),
            current_value: "0".to_string(),
            recommended_value: "50".to_string(),
            reason: "延迟敏感服务开启 busy polling 可让 CPU 主动轮询网卡，减少中断延迟（微秒级）"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_retries2(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_retries2";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 8 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_retries2".to_string(),
            current_value: current.to_string(),
            recommended_value: "8".to_string(),
            reason: format!(
                "TCP 重传 {} 次才放弃（约 {}分钟），缩短到 8 次可更快检测断连释放资源",
                current,
                if current >= 15 { "13-30" } else { "6-13" }
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

// NOTE: tcp_abort_on_overflow rule removed. The kernel docs explicitly advise
// against enabling it ("in general it harms the clients"): the default 0
// (drop the SYN-ACK so the client retransmits) rides out transient accept-queue
// bursts, whereas 1 turns every burst into a hard RST that fails the client.

fn eval_tcp_syn_retries(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_syn_retries";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 3 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_syn_retries".to_string(),
            current_value: current.to_string(),
            recommended_value: "3".to_string(),
            reason: format!(
                "SYN 重试 {current} 次才放弃（超时约 30 秒），减少到 3 次（约 15 秒）可加速不可达主机的连接失败检测"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance, writable: true,
        });
    }
    1
}

fn eval_tcp_synack_retries(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_synack_retries";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 3 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_synack_retries".to_string(),
            current_value: current.to_string(),
            recommended_value: "3".to_string(),
            reason: format!(
                "SYNACK 重试 {current} 次太多，减少到 3 次可更快释放半开连接资源，降低 SYN flood 影响"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance, writable: true,
        });
    }
    1
}

fn eval_optmem_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/optmem_max";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 81920 {
        recs.push(Recommendation {
            param: "net.core.optmem_max".to_string(),
            current_value: current.to_string(),
            recommended_value: "81920".to_string(),
            reason: "套接字辅助缓冲区默认值偏小，增大可支持更多控制消息和套接字选项".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_oom_kill_allocating_task(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_oom_kill_allocating_task_at(info, recs, "/proc/sys/vm/oom_kill_allocating_task")
}

/// Path-injectable form of [`eval_oom_kill_allocating_task`] (the
/// `eval_*_at` idiom). `mm/oom_kill.c` registers vm.oom_kill_allocating_task
/// through plain `proc_dointvec` with no min/max, and the same file consumes
/// it as a boolean (`if (!is_memcg_oom(oc) && sysctl_oom_kill_allocating_task
/// && ...)`), so -1 is a legal, already-enabled value. The unsigned reader
/// turns "-1" into the fallback 0 — the *disabled* value — so the `== 0` gate
/// invented the recommendation on a host that already kills the allocating
/// task.
fn eval_oom_kill_allocating_task_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "vm.oom_kill_allocating_task".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "OOM 时优先杀死触发分配的进程而非遍历进程列表选择目标，响应更快更可预测"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_inotify_max_user_watches(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/inotify/max_user_watches";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 524288 {
        recs.push(Recommendation {
            param: "fs.inotify.max_user_watches".to_string(),
            current_value: current.to_string(),
            recommended_value: "524288".to_string(),
            reason: format!(
                "inotify watch 上限 {current} 偏低，文件监控/IDE/构建工具可能报 'no space left on device' 错误"
            ),
            confidence: Confidence::High,
            category: Category::Performance, writable: true,
        });
    }
    1
}

fn eval_aio_max_nr(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/aio-max-nr";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 1048576 {
        recs.push(Recommendation {
            param: "fs.aio-max-nr".to_string(),
            current_value: current.to_string(),
            recommended_value: "1048576".to_string(),
            reason: "异步 IO 请求上限偏低，数据库和高并发 IO 场景可能触及限制导致 IO 提交失败"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_dirty_background_ratio(
    info: &SystemInfo,
    workload: &WorkloadType,
    recs: &mut Vec<Recommendation>,
) -> usize {
    eval_dirty_background_ratio_at(info, workload, recs, "/proc/sys/vm/dirty_background_ratio")
}

/// Path-injectable form (the `eval_*_at` idiom) so the database gate is
/// unit-testable against a temp file instead of the live /proc.
fn eval_dirty_background_ratio_at(
    info: &SystemInfo,
    workload: &WorkloadType,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // >=64GB hosts use dirty_background_bytes instead (mutually exclusive with
    // the ratio form in the kernel); avoid recommending both.
    if info.memory_total_gb >= 64 {
        return 1;
    }
    let current = info.sysctl.dirty_background_ratio;
    let target = match workload {
        WorkloadType::IoLatency => 3,
        _ => 5,
    };
    // This rule leaves the dirty ratio alone, so the limit in force stays the
    // current one — see [`dirty_background_takes_effect`] for the clamp.
    let takes_effect = dirty_background_takes_effect(info.sysctl.dirty_ratio, target as u64);
    if takes_effect && current > target as u64 {
        let has_db = info.has_process("postgres")
            || info.has_process("mysqld")
            // MariaDB 10.4+ runs as mariadbd — the same OLTP database as mysqld.
            || info.has_process("mariadbd")
            || info.has_process("mongod")
            || info.has_process("clickhouse");
        if has_db || matches!(workload, WorkloadType::IoLatency) || current > 10 {
            recs.push(Recommendation {
                param: "vm.dirty_background_ratio".to_string(),
                current_value: current.to_string(),
                recommended_value: target.to_string(),
                reason: format!(
                    "后台回写触发阈值偏高（{current}%），脏页堆积后突发刷盘会造成 IO 延迟尖刺"
                ),
                confidence: Confidence::Medium,
                category: Category::Performance,
                writable: true,
            });
        }
    }
    1
}

fn eval_sched_min_granularity(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_sched_min_granularity_at(info, recs, "/proc/sys/kernel/sched_min_granularity_ns")
}

// Path-parameterized core so tests can force the absent branch with a probe
// path that exists on no host, instead of betting on the CI kernel lacking
// the real node.
fn eval_sched_min_granularity_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if info.cpu_cores > 32 && current < 10_000_000 {
        recs.push(Recommendation {
            param: "kernel.sched_min_granularity_ns".to_string(),
            current_value: current.to_string(),
            recommended_value: "10000000".to_string(),
            reason: format!(
                "{}核机器上调度器切换过频，增大最小调度粒度可减少上下文切换开销",
                info.cpu_cores
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_icmp_echo_ignore_broadcasts(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_icmp_echo_ignore_broadcasts_at(
        info,
        recs,
        "/proc/sys/net/ipv4/icmp_echo_ignore_broadcasts",
    )
}

/// Path-injectable form of [`eval_icmp_echo_ignore_broadcasts`] (the
/// `eval_*_at` idiom). Through v5.10 net/ipv4/sysctl_net_ipv4.c registered
/// icmp_echo_ignore_broadcasts as a plain `proc_dointvec` int with no
/// min/max, so -1 is a legal, persistent value there, and net/ipv4/icmp.c
/// consumes it as a truthiness test (`READ_ONCE(net->ipv4.
/// sysctl_icmp_echo_ignore_broadcasts)` guards the broadcast answer). The
/// unsigned reader parsed "-1" to Err and fell back to 0 — the *disabled*
/// value — so the `== 0` gate invented a finding on a host that already
/// ignores broadcast echoes. Since v5.14 the knob is u8
/// (`proc_dou8vec_minmax` bounded to [0,1]), where negatives are rejected at
/// write time; the signed reader keeps the truthiness contract correct on
/// both registrations.
fn eval_icmp_echo_ignore_broadcasts_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.icmp_echo_ignore_broadcasts".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未忽略广播 ICMP 请求，存在 Smurf 放大攻击风险".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_accept_source_route(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/accept_source_route";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.accept_source_route".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "允许源路由可被攻击者用于绕过网络安全策略和进行路由欺骗".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_tcp_rfc1337(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_tcp_rfc1337_at(info, recs, "/proc/sys/net/ipv4/tcp_rfc1337")
}

/// Path-injectable form of [`eval_tcp_rfc1337`] (the `eval_*_at` idiom).
/// Through v5.10 net/ipv4/sysctl_net_ipv4.c registered tcp_rfc1337 as a
/// plain `proc_dointvec` int with no min/max, so -1 is a legal, persistent
/// value there, and net/ipv4/tcp_minisocks.c consumes it as a truthiness
/// test (`if (!READ_ONCE(twsk_net(tw)->ipv4.sysctl_tcp_rfc1337))` lets a
/// forged RST kill the TIME_WAIT socket). The unsigned reader parsed "-1"
/// to Err and fell back to 0 — the *unprotected* value — so the `== 0`
/// gate invented a finding on a host that already keeps its TIME_WAIT
/// sockets. Since v5.14 the knob is u8 (`proc_dou8vec_minmax`, no
/// extra1/extra2), where negatives are rejected at write time; the signed
/// reader keeps the truthiness contract correct on both registrations.
fn eval_tcp_rfc1337_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_rfc1337".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "TIME_WAIT 状态的连接可被伪造的 RST 报文异常终止，启用此保护可防止此类攻击"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_secure_redirects(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_secure_redirects_at(
        info,
        recs,
        "/proc/sys/net/ipv4/conf/all/secure_redirects",
        std::path::Path::new("/proc/sys/net/ipv4/conf"),
    )
}

/// Path-injectable form of [`eval_secure_redirects`] (the `eval_*_at` idiom)
/// so the shared-media precondition is assertable against a synthetic conf
/// tree.
///
/// The `conf/all/secure_redirects` entry is a plain `proc_dointvec` int slot
/// (`devinet_conf_proc` in net/ipv4/devinet.c, no min/max), and
/// `IN_DEV_SEC_REDIRECTS` (include/linux/inetdevice.h) reads it through
/// `IN_DEV_ORCONF`, a truthiness test consumed by `net/ipv4/route.c` as
/// `if (IN_DEV_SEC_REDIRECTS(in_dev) && ...)`. -1 is therefore enabled and
/// must be flagged, but the unsigned reader parsed "-1" to Err — its fallback
/// 0 skipped the rule on exactly the host whose secure redirects are on.
///
/// That consumer sits in the branch `__ip_do_redirect()` reaches only when
/// shared-media redirects are disabled:
///
/// ```text
///    if (!IN_DEV_SHARED_MEDIA(in_dev)) {
///        if (!inet_addr_onlink(in_dev, new_gw, old_gw))
///            goto reject_redirect;
///        if (IN_DEV_SEC_REDIRECTS(in_dev) && ip_fib_check_default(new_gw, dev))
///            goto reject_redirect;
///    } else {
///        if (inet_addr_type(net, new_gw) != RTN_UNICAST)
///            goto reject_redirect;
///    }
/// ```
///
/// The sysctl documentation states the override outright — "shared_media ...
/// Overrides secure_redirects" and "Overridden by shared_media"
/// (Documentation/networking/ip-sysctl.rst) — and `IN_DEV_SHARED_MEDIA` is an
/// OR of the `all` template with the device's own value, so while every
/// interface runs the default (shared media on) the assignment cannot change
/// how a redirect is judged. The rule stays quiet then, and only a host that
/// already proved the knob reachable keeps the recommendation.
fn eval_secure_redirects_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    conf_root: &std::path::Path,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !shared_media_disabled_somewhere(conf_root) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.secure_redirects".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "即使来自默认网关的 ICMP 重定向也不应接受，服务器无需动态修改路由表"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

/// Whether any interface has shared-media redirects off, the only state in
/// which `net/ipv4/route.c` reads `IN_DEV_SEC_REDIRECTS` at all (see
/// [`eval_secure_redirects_at`]). `IN_DEV_SHARED_MEDIA` is an
/// `IN_DEV_ORCONF` of the `all` template and the device's own value
/// (include/linux/inetdevice.h), so a nonzero `all` settles every interface
/// at once, and only a device — or the `default` template that new devices
/// inherit — carrying an explicit zero makes the per-interface half
/// reachable. A value that is missing or unreadable does not count as off, so
/// an unprovable tree keeps the rule quiet.
fn shared_media_disabled_somewhere(conf_root: &std::path::Path) -> bool {
    let zero = |path: &std::path::Path| {
        std::fs::read_to_string(path)
            .map(|value| value.trim().parse::<i64>().unwrap_or(1) == 0)
            .unwrap_or(false)
    };
    if !zero(&conf_root.join("all").join("shared_media")) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(conf_root) else {
        return false;
    };
    entries
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.file_name() != "all" && zero(&entry.path().join("shared_media")))
}

fn eval_mmap_min_addr(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/mmap_min_addr";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 65536 {
        recs.push(Recommendation {
            param: "vm.mmap_min_addr".to_string(),
            current_value: current.to_string(),
            recommended_value: "65536".to_string(),
            reason:
                "最小 mmap 地址过低，用户态程序可映射低地址空间，增加 NULL 指针解引用漏洞利用风险"
                    .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_default_accept_redirects(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/accept_redirects";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.accept_redirects".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "新创建网络接口默认接受 ICMP 重定向，攻击者可借此劫持流量".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_default_accept_source_route(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/accept_source_route";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.accept_source_route".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "新创建网络接口默认允许源路由，攻击者可绕过网络安全策略".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_sched_latency_ns(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_latency_ns";
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores <= 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 12000000 {
        recs.push(Recommendation {
            param: "kernel.sched_latency_ns".to_string(),
            current_value: current.to_string(),
            recommended_value: "24000000".to_string(),
            reason: format!(
                "大核数 ({} 核) 服务器增大 CFS 调度周期可减少上下文切换开销",
                info.cpu_cores
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_challenge_ack_limit(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_challenge_ack_limit";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if let Some(rec) = challenge_ack_limit_recommendation(read_sysctl_u64(path)) {
        recs.push(rec);
    }
    1
}

/// Value-driven core of the challenge-ACK rule, split from the live probe so
/// the boundary and the value the kernel special-cases are assertable on any
/// host.
///
/// `tcp_send_challenge_ack` (`net/ipv4/tcp_input.c`) reads this sysctl into a
/// u32 and takes the unlimited path only at `INT_MAX`
/// (`if (ack_limit == INT_MAX) goto send_ack;`); every other value installs
/// the randomized per-second budget that the CVE-2016-5696 side channel
/// measures, and `tcp_ipv4.c` initializes the sysctl to `INT_MAX`. The reason
/// used to warn about a default of 100, which no kernel sets; the
/// recommendation was 999999999, which still installs the limiter.
fn challenge_ack_limit_recommendation(current: u64) -> Option<Recommendation> {
    if current > 100 {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.tcp_challenge_ack_limit".to_string(),
        current_value: current.to_string(),
        recommended_value: "2147483647".to_string(),
        reason: "该上限被设得很低：内核只把 INT_MAX 当作不限速，更小的值会启用每秒随机预算的 RFC 5961 限速，攻击者可据此推断 TCP 连接状态（CVE-2016-5696）；内核默认本就是 INT_MAX"
            .to_string(),
        confidence: Confidence::High,
        category: Category::Security,
        writable: true,
    })
}

fn eval_rp_filter_all(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/rp_filter";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.rp_filter".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未在所有接口启用反向路径过滤，攻击者可伪造源 IP 欺骗（注意：非对称/多路径路由场景需保持 0）".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_busy_read(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/busy_read";
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.core.busy_read".to_string(),
            current_value: "0".to_string(),
            recommended_value: "50".to_string(),
            reason: "万兆网络启用忙轮询读可降低网络延迟（用少量 CPU 换取更低的收包延迟）"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_nmi_watchdog(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/nmi_watchdog";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "kernel.nmi_watchdog".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "NMI watchdog 每核消耗一个 PMU 计数器和定期中断，服务器禁用可节省 CPU 资源"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_stat_interval(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/stat_interval";
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores < 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current <= 1 {
        recs.push(Recommendation {
            param: "vm.stat_interval".to_string(),
            current_value: current.to_string(),
            recommended_value: "5".to_string(),
            reason: "大核数机器上 vmstat 每秒更新开销大，增大间隔可减少 CPU 缓存行争用".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_hung_task_timeout(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/hung_task_timeout_secs";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.hung_task_timeout_secs".to_string(),
            current_value: "0".to_string(),
            recommended_value: "120".to_string(),
            reason: "hung_task 检测已禁用，无法发现卡死的内核任务（可能是 IO 阻塞或死锁）"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_netdev_budget_usecs(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/netdev_budget_usecs";
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 4000 {
        recs.push(Recommendation {
            param: "net.core.netdev_budget_usecs".to_string(),
            current_value: current.to_string(),
            recommended_value: "8000".to_string(),
            reason: "万兆网络下 NAPI 轮询时间预算不足，可能导致频繁退出轮询增加中断开销"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_dirty_bytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/dirty_bytes";
    if !info.param_exists(path) {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let ratio_path = "/proc/sys/vm/dirty_ratio";
    let ratio = if std::path::Path::new(ratio_path).exists() {
        read_sysctl_u64(ratio_path)
    } else {
        0
    };
    dirty_bytes_recommendation(info.memory_total_gb, current, ratio, recs);
    1
}

/// Value-driven core of the dirty-bytes rule, separated so tests can force
/// both branches of the twin read on any host (the sysctls are read from the
/// live /proc, which a test cannot control) — the same split
/// `dirty_background_bytes_recommendation` and `overcommit_ratio_recommendation`
/// already use for their twins.
///
/// `vm.dirty_bytes` and `vm.dirty_ratio` are mutually exclusive: writing one
/// zeroes the other (v6.6 mm/page-writeback.c:518-544 — `dirty_ratio_handler`
/// clears `vm_dirty_bytes` on every changed write, `dirty_bytes_handler`
/// clears `vm_dirty_ratio`), and vm.rst's dirty_bytes section states the pair
/// outright: "dirty_bytes is the counterpart of dirty_ratio. Only one of them
/// may be specified at a time. [...] the other appears as 0 when read."
///
/// `dirty_bytes == 0` therefore does not only mean "nothing is configured":
/// it also reads 0 on a host whose administrator deliberately pinned a low
/// `dirty_ratio` (that write is what zeroed the bytes twin). Recommending the
/// bytes form there is not the percentage-vs-bytes improvement the reason
/// promises — the write would silently disable the pinned ratio and replace
/// it with [`DIRTY_BYTES_TARGET`], the exact twin clearing the rollback
/// ledger has to record when ktuner itself writes one of these pairs. The
/// sibling rule (`dirty_background_bytes_recommendation`) already refuses the
/// switch while the ratio twin holds a deliberate low value
/// (`bytes == 0 && ratio > 5`); this rule applies the same guard.
fn dirty_bytes_recommendation(
    memory_total_gb: u64,
    bytes: u64,
    ratio: u64,
    recs: &mut Vec<Recommendation>,
) {
    // A ratio at or below the family's percentage floor is a deliberate low
    // tuning (the latency-sensitive target of `eval_dirty_ratio` is 5, and
    // `eval_dirty_background_ratio_at` uses the same 5 floor); the switch may
    // only replace a percentage that is actually high.
    if bytes != 0 || ratio <= 5 {
        return;
    }
    let recommended = DIRTY_BYTES_TARGET;
    recs.push(Recommendation {
        param: "vm.dirty_bytes".to_string(),
        current_value: "0".to_string(),
        recommended_value: recommended.to_string(),
        reason: format!("大内存服务器 ({} GB) 使用 dirty_ratio 百分比会导致脏页过多、IO 突刺，改用固定字节限制更平稳", memory_total_gb),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    });
}

/// Whether a fork-heavy server workload runs here: the web servers and
/// databases whose children immediately do useful work, so
/// `sched_child_runs_first=1` costs an unnecessary COW copy. Debian/Ubuntu
/// run the Apache binary as `apache2` — RHEL's `httpd` is the same server,
/// and the gate must not go quiet on a Debian Apache host.
fn fork_server_present(info: &SystemInfo) -> bool {
    info.has_process("nginx")
        || info.has_process("httpd")
        || info.has_process("apache2")
        || info.has_process("postgres")
        || info.has_process("mysqld")
}

fn eval_sched_child_runs_first(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_child_runs_first";
    if !info.param_exists(path) {
        return 1;
    }
    if !fork_server_present(info) {
        return 1;
    }
    if let Some(rec) =
        sched_child_runs_first_recommendation(read_sysctl_u64(path), &info.kernel_version)
    {
        recs.push(rec);
    }
    1
}

/// Emit the `kernel.sched_child_runs_first` recommendation for an already-read
/// value; split from the file probe so the version gate is testable anywhere.
fn sched_child_runs_first_recommendation(
    current: u64,
    kernel_version: &str,
) -> Option<Recommendation> {
    // Linux 6.6 merged EEVDF: commit e8f331bcc2 ("sched/smp: Use lag to
    // simplify cross-runqueue placement") removed the only reader of this
    // knob from task_fork_fair(). The sysctl node still exists and accepts
    // writes, but nothing consumes the value, so telling an administrator to
    // set it back to 0 promises COW-copy savings that can no longer happen.
    if kernel_at_least(kernel_version, 6, 6) || current == 0 {
        return None;
    }
    Some(Recommendation {
        param: "kernel.sched_child_runs_first".to_string(),
        current_value: current.to_string(),
        recommended_value: "0".to_string(),
        reason: "服务器场景下 fork 后父进程先运行更优，避免 COW 页面不必要的复制".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_page_cluster(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_page_cluster_at(info, recs, "/proc/sys/vm/page-cluster", "/proc/swaps")
}

/// Path-injectable form of [`eval_page_cluster`] (the `eval_*_at` idiom) so the
/// swap gate is assertable against a synthetic /proc/swaps.
///
/// The knob only sizes the swap-in readahead window: v6.6 reads `page_cluster`
/// solely in `mm/swap_state.c` (`swapin_nr_pages`, `swapin_readahead` and the
/// swap VMA readahead), and the kernel documentation calls it "the swap
/// counterpart to page cache readahead" (`sysctl/vm.rst`). A host with no swap
/// area has nothing to read in, so the recommendation's promised saving cannot
/// happen there — the same "never recommend a no-op" rule the hardlockup_panic
/// gate already follows for a disabled NMI watchdog.
fn eval_page_cluster_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    swaps_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let has_ssd = info
        .disks
        .iter()
        .any(|d| matches!(d.disk_type, DiskType::NVMe | DiskType::SSD));
    if !has_ssd {
        return 1;
    }
    if !swap_configured(swaps_path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 0 {
        recs.push(Recommendation {
            param: "vm.page-cluster".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "SSD 上关闭 swap 预读可避免不必要的 IO，SSD 的随机读取延迟极低无需预读优化"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// Whether `/proc/swaps` lists at least one swap area. The table always carries
/// its header line, so a configured area means a following non-empty line
/// (zram and file-backed areas are listed the same way). An unreadable table
/// counts as "no swap", so the rule stays quiet instead of promising a swap
/// saving it cannot verify.
fn swap_configured(path: &str) -> bool {
    std::fs::read_to_string(path)
        .is_ok_and(|content| content.lines().skip(1).any(|line| !line.trim().is_empty()))
}

fn eval_rmem_default(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/rmem_default";
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 262144 {
        recs.push(Recommendation {
            param: "net.core.rmem_default".to_string(),
            current_value: current.to_string(),
            recommended_value: "262144".to_string(),
            reason: "万兆网络默认 socket 接收缓冲区过小，新建连接可能需要动态扩展缓冲区增加延迟"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_wmem_default(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/wmem_default";
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 262144 {
        recs.push(Recommendation {
            param: "net.core.wmem_default".to_string(),
            current_value: current.to_string(),
            recommended_value: "262144".to_string(),
            reason: "万兆网络默认 socket 发送缓冲区过小，新建连接初始发送性能受限".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_nr_migrate(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_nr_migrate";
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores <= 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 128 {
        recs.push(Recommendation {
            param: "kernel.sched_nr_migrate".to_string(),
            current_value: current.to_string(),
            recommended_value: "128".to_string(),
            reason: format!(
                "{}核 CPU 每次负载均衡仅迁移 {} 个任务，增大可加速核间负载均衡收敛",
                info.cpu_cores, current
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_notsent_lowat(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_notsent_lowat";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 131072 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_notsent_lowat".to_string(),
            current_value: current.to_string(),
            recommended_value: "131072".to_string(),
            reason: "默认值过大导致每个 TCP 连接可能缓存大量未发送数据浪费内存，设为 128KB 可降低内存占用并减少延迟".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_dsack(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_dsack";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_dsack".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "D-SACK 帮助发送方精确识别虚假重传，关闭会导致不必要的重传和带宽浪费"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_unix_max_dgram_qlen(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/unix/max_dgram_qlen";
    if !info.param_exists(path) {
        return 1;
    }
    if let Some(rec) = max_dgram_qlen_recommendation(read_sysctl_u64(path)) {
        recs.push(rec);
    }
    1
}

/// Value-driven core of the unix datagram queue rule, split from the live
/// probe so the recommendation is assertable on any host.
///
/// The kernel default is 10, not the 512 the reason used to cite:
/// `unix_net_init` (`net/unix/af_unix.c`) sets `sysctl_max_dgram_qlen = 10`
/// for every net namespace.
fn max_dgram_qlen_recommendation(current: u64) -> Option<Recommendation> {
    if current >= 1024 {
        return None;
    }
    Some(Recommendation {
        param: "net.unix.max_dgram_qlen".to_string(),
        current_value: current.to_string(),
        recommended_value: "1024".to_string(),
        reason: "Unix socket 数据报队列上限默认只有 10，systemd/journald 等高负载下可能丢失消息"
            .to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_rps_sock_flow_entries(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_rps_sock_flow_entries_at(
        info,
        recs,
        "/proc/sys/net/core/rps_sock_flow_entries",
        std::path::Path::new("/sys/class/net"),
    )
}

/// Path-injectable form of [`eval_rps_sock_flow_entries`] (the `eval_*_at`
/// idiom) so the RFS precondition is assertable against a synthetic sysfs
/// tree instead of the live `/sys/class/net`.
///
/// The global flow table this rule sizes is read only by `get_rps_cpu()`, and
/// that read sits behind the per-queue flow table: net/core/dev.c does
/// `if (!flow_table && !map) goto done;` and consults `sock_flow_table` inside
/// `if (flow_table && sock_flow_table) { ... }`. The per-queue table is what
/// `rps_flow_cnt` sizes, so a receive queue that has only `rps_cpus` set takes
/// the plain-RPS branch and never reads the global table, while a queue with
/// `rps_flow_cnt` reaches it even when every `rps_cpus` is left at 0 —
/// `rps_sock_flow_sysctl()` raises the RPS gate itself when the global table
/// becomes non-zero (net/core/sysctl_net_core.c: `static_branch_inc(&rps_needed);
/// static_branch_inc(&rfs_needed);`). The kernel documentation states the
/// matching requirement next to the two flow-table knobs: "Both of these need
/// to be set before RFS is enabled for a receive queue"
/// (Documentation/networking/scaling.rst).
fn eval_rps_sock_flow_entries_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    net_root: &std::path::Path,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    if !rfs_flow_table_configured(net_root) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 32768 {
        recs.push(Recommendation {
            param: "net.core.rps_sock_flow_entries".to_string(),
            current_value: current.to_string(),
            recommended_value: "32768".to_string(),
            reason: "万兆网络启用 RFS 流分发表可将网络处理分散到多核，减少 CPU 热点提升吞吐"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// Whether any receive queue has its RFS flow table configured — a non-zero
/// `rps_flow_cnt` — the per-queue half of the pair `get_rps_cpu()` consults
/// before reading the global table this rule sizes (see
/// [`eval_rps_sock_flow_entries_at`]). A queue without the file, or with `0`,
/// holds no flow table and never reaches the global one; `rps_cpus` on its own
/// steers through plain RPS instead.
fn rfs_flow_table_configured(net_root: &std::path::Path) -> bool {
    let Ok(interfaces) = std::fs::read_dir(net_root) else {
        return false;
    };
    for interface in interfaces.filter_map(|entry| entry.ok()) {
        let Ok(queues) = std::fs::read_dir(interface.path().join("queues")) else {
            continue;
        };
        for queue in queues.filter_map(|entry| entry.ok()) {
            if !queue.file_name().to_string_lossy().starts_with("rx-") {
                continue;
            }
            let Ok(count) = std::fs::read_to_string(queue.path().join("rps_flow_cnt")) else {
                continue;
            };
            if count.trim().parse::<u64>().is_ok_and(|count| count > 0) {
                return true;
            }
        }
    }
    false
}

fn eval_neigh_gc_thresh3(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/gc_thresh3";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 4096 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.gc_thresh3".to_string(),
            current_value: current.to_string(),
            recommended_value: "8192".to_string(),
            reason: "ARP 表上限过低，大规模网络下可能触发 Neighbour table overflow 导致网络中断"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_neigh_gc_thresh1(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/gc_thresh1";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 2048 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.gc_thresh1".to_string(),
            current_value: current.to_string(),
            recommended_value: "2048".to_string(),
            reason: "ARP 表 GC 起始阈值过低，频繁触发垃圾回收影响网络性能".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_neigh_gc_thresh2(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/gc_thresh2";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 4096 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.gc_thresh2".to_string(),
            current_value: current.to_string(),
            recommended_value: "4096".to_string(),
            reason: "ARP 表软上限过低，超过后条目存活时间缩短为 5 秒，高连接数场景会频繁 ARP 解析"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_retries1(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_retries1";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 3 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_retries1".to_string(),
            current_value: current.to_string(),
            recommended_value: "3".to_string(),
            reason: "TCP 重传次数过多才通知网络层，延迟路由切换和 PMTU 发现".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_limit_output_bytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_limit_output_bytes";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed >= 10000 && current < 524288 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_limit_output_bytes".to_string(),
            current_value: current.to_string(),
            recommended_value: "1048576".to_string(),
            reason: "万兆网络下 TCP 输出限制过低，限制了单连接吞吐量".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_dev_weight(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/dev_weight";
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 128 {
        recs.push(Recommendation {
            param: "net.core.dev_weight".to_string(),
            current_value: current.to_string(),
            recommended_value: "128".to_string(),
            reason: "万兆网络下 NAPI 每次轮询 TX 处理包数过少，增大可提升发送吞吐量".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_printk(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/printk";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let level: u64 = content
        .split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(4);
    if level > 4 {
        recs.push(Recommendation {
            param: "kernel.printk".to_string(),
            current_value: level.to_string(),
            recommended_value: "4".to_string(),
            reason: "内核控制台日志级别过高，大量非关键消息输出到控制台影响性能".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_watchdog_thresh(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/watchdog_thresh";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores < 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 20 && current > 0 {
        recs.push(Recommendation {
            param: "kernel.watchdog_thresh".to_string(),
            current_value: current.to_string(),
            recommended_value: "30".to_string(),
            reason: "大核数系统负载高时 watchdog 阈值过低容易触发误报软死锁告警".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_admin_reserve_kbytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_admin_reserve_kbytes_at(
        info,
        recs,
        "/proc/sys/vm/admin_reserve_kbytes",
        "/proc/sys/vm/overcommit_memory",
    )
}

/// Path-injectable form of [`eval_admin_reserve_kbytes`] (the `eval_*_at`
/// idiom) so the overcommit-mode precondition is assertable against synthetic
/// files.
///
/// The reserve is only subtracted on the `OVERCOMMIT_NEVER` path of
/// `__vm_enough_memory()` (mm/util.c): `vm.overcommit_memory == 0` (the
/// default "guess" mode) returns from that function before either
/// `sysctl_admin_reserve_kbytes` or `sysctl_user_reserve_kbytes` is read, and
/// mode 1 returns even earlier. On such a host the reason's promised recovery
/// headroom does not exist, so the write cannot change any allocation —
/// the sibling [`overcommit_ratio_recommendation`] gate already keys on the
/// same `oc_mode == 2` condition.
fn eval_admin_reserve_kbytes_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    overcommit_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    if !overcommit_reserves_are_live(overcommit_path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 131072 {
        recs.push(Recommendation {
            param: "vm.admin_reserve_kbytes".to_string(),
            current_value: current.to_string(),
            recommended_value: "131072".to_string(),
            reason: "大内存机器管理员保留内存过少，OOM 时可能无法登录排查问题".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// Whether the kernel reads the commit reserves at all: in
/// `__vm_enough_memory()` (mm/util.c) both `sysctl_admin_reserve_kbytes` and
/// `sysctl_user_reserve_kbytes` are subtracted *after* the
/// `sysctl_overcommit_memory == OVERCOMMIT_GUESS` early return, so only a host
/// running `vm.overcommit_memory == 2` ("never overcommit") consults them.
/// An unreadable mode file counts as "not consulted", so the rules stay quiet
/// instead of promising recovery headroom nothing reads.
fn overcommit_reserves_are_live(overcommit_path: &str) -> bool {
    read_sysctl_u64(overcommit_path) == 2
}

fn eval_nr_open(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/nr_open";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 1048576 {
        recs.push(Recommendation {
            param: "fs.nr_open".to_string(),
            current_value: current.to_string(),
            recommended_value: "1048576".to_string(),
            reason: "进程级文件描述符硬上限过低，高并发服务可能无法设置足够大的 ulimit".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_arp_announce(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/arp_announce";
    if !info.param_exists(path) {
        return 1;
    }
    if arp_tuning_skipped(info.network.len(), has_bond()) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.arp_announce".to_string(),
            current_value: "0".to_string(),
            recommended_value: "2".to_string(),
            reason: "多网卡环境下 ARP 回复可能使用错误接口的 IP，导致通信异常和 ARP 表污染"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_arp_ignore(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/arp_ignore";
    if !info.param_exists(path) {
        return 1;
    }
    if arp_tuning_skipped(info.network.len(), has_bond()) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.arp_ignore".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "多网卡环境下默认回复所有接口的 ARP 请求，可能导致流量走错网卡和路由异常"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_default_log_martians(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_default_log_martians_at(info, recs, "/proc/sys/net/ipv4/conf/default/log_martians")
}

/// Path-injectable form of [`eval_default_log_martians`] (the `eval_*_at`
/// idiom). `conf/default/*` is the same `devinet_conf_proc` plain
/// `proc_dointvec` int slot as `conf/all/*` (net/ipv4/devinet.c, no
/// min/max), inherited by every new interface, and `IN_DEV_LOG_MARTIANS`
/// (include/linux/inetdevice.h) is an `IN_DEV_ORCONF` truthiness test — -1
/// is legal and enabled. The unsigned reader's fallback 0 made the `== 0`
/// gate report new interfaces as blind to martians.
fn eval_default_log_martians_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.log_martians".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "新创建的网络接口不会记录火星包日志，可能错过 IP 欺骗和路由异常".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_laptop_mode(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/laptop_mode";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 && info.memory_total_gb >= 16 {
        recs.push(Recommendation {
            param: "vm.laptop_mode".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器环境启用了笔记本省电模式，会延迟磁盘写入增加数据丢失风险".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_adv_win_scale(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_adv_win_scale";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse::<i64>()
        .unwrap_or(1);
    if let Some(rec) = tcp_adv_win_scale_recommendation(current, &info.kernel_version) {
        recs.push(rec);
    }
    1
}

/// Emit the `net.ipv4.tcp_adv_win_scale` recommendation for an already-read
/// value; split from the file probe so the version gate is testable anywhere.
fn tcp_adv_win_scale_recommendation(current: i64, kernel_version: &str) -> Option<Recommendation> {
    // Linux 6.6 replaced the sysctl with a per-socket scaling_ratio measured
    // from real skb overhead; the knob is documented as obsolete and the
    // receive window ignores it, so raising it there changes nothing.
    if kernel_at_least(kernel_version, 6, 6) || current >= 2 {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.tcp_adv_win_scale".to_string(),
        current_value: current.to_string(),
        recommended_value: "2".to_string(),
        reason: "TCP 接收缓冲区开销因子偏低，增大可让更多缓冲区用于应用数据提升吞吐".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_sched_tunable_scaling(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_tunable_scaling";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores <= 16 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.sched_tunable_scaling".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: format!(
                "{}核 CPU 禁用了调度器自动缩放，内核无法根据 CPU 数量调整调度参数",
                info.cpu_cores
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_panic_on_oops(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_panic_on_oops_at(info, recs, "/proc/sys/kernel/panic_on_oops")
}

/// Path-injectable form (the `eval_*_at` idiom) so the signed read is
/// unit-testable against a temp file. `kernel/sysctl.c` registers
/// kernel.panic_on_oops through plain `proc_dointvec`, which copies the table
/// with no min/max, so -1 is a legal value; `arch/x86/kernel/dumpstack.c`
/// consumes it as a boolean (`if (panic_on_oops) panic(...)`), and -1 is
/// already enabled. The unsigned reader parses "-1" to Err and falls back to
/// 0, the *not-enabled* value, so the `== 0` gate invented the recommendation
/// on a host that already panics on oops.
fn eval_panic_on_oops_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.panic_on_oops".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "内核 oops 后继续运行可能导致数据损坏或安全漏洞，建议 panic 后重启（代价：oops 时会重启）".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_oom_dump_tasks(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_oom_dump_tasks_at(info, recs, "/proc/sys/vm/oom_dump_tasks")
}

/// Path-injectable form of [`eval_oom_dump_tasks`] (the `eval_*_at` idiom).
/// `mm/oom_kill.c` registers vm.oom_dump_tasks through plain `proc_dointvec`
/// with no min/max, and its consumer is a boolean (`if (sysctl_oom_dump_tasks)`
/// in the same file), so -1 is a legal, already-enabled value. The unsigned
/// reader turns "-1" into the fallback 0 — the *disabled* value — so the
/// `== 0` gate invented the recommendation on a host that already dumps the
/// task list.
fn eval_oom_dump_tasks_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "vm.oom_dump_tasks".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "OOM 时不输出进程列表，无法排查内存泄漏根因，建议启用".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_moderate_rcvbuf(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_moderate_rcvbuf";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_moderate_rcvbuf".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "TCP 接收缓冲区自动调整被禁用，可能导致内存浪费或吞吐受限".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_flow_limit_table_len(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/flow_limit_table_len";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 8192 {
        recs.push(Recommendation {
            param: "net.core.flow_limit_table_len".to_string(),
            current_value: current.to_string(),
            recommended_value: "8192".to_string(),
            reason: format!(
                "{}Gbps 网络下流控表过小（{}），增大可改善高流量场景的公平性",
                max_speed / 1000,
                current
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_l3mdev_accept(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_l3mdev_accept";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_l3mdev_accept".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "启用 L3 master device 接受可能绕过 VRF 隔离，非 VRF 环境应禁用".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_panic_on_warn(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/panic_on_warn";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    recommend_panic_on_warn(read_sysctl_u64(path), recs);
    1
}

/// Emit the `kernel.panic_on_warn` recommendation for an already-read value.
///
/// Split out from the file probe so the recommendation's shape — its category in
/// particular — is testable on a host that does not boot with `panic_on_warn=1`.
fn recommend_panic_on_warn(current: u64, recs: &mut Vec<Recommendation>) {
    if current == 0 {
        return;
    }
    recs.push(Recommendation {
        param: "kernel.panic_on_warn".to_string(),
        current_value: current.to_string(),
        recommended_value: "0".to_string(),
        reason: "内核 WARN 即 panic 过于激进，正常运行中的 WARN 不应导致系统重启".to_string(),
        confidence: Confidence::High,
        // Availability policy about taking the host down, like panic,
        // panic_on_oops, panic_on_oom and hardlockup_panic: a security
        // recommendation is what `--category security` selects.
        category: Category::Security,
        writable: true,
    });
}

/// `vm.dirty_bytes` recommended for >=64GB hosts that still use the ratio form.
const DIRTY_BYTES_TARGET: u64 = 256 * 1024 * 1024;
/// Preferred `vm.dirty_background_bytes` when the dirty limit leaves room.
const DIRTY_BACKGROUND_BYTES_TARGET: u64 = 256 * 1024 * 1024;

/// Background threshold that stays below the dirty limit in effect after
/// tuning: the current `vm.dirty_bytes`, or [`DIRTY_BYTES_TARGET`] when it is
/// 0 because [`eval_dirty_bytes`] then recommends that value. The kernel
/// replaces a background threshold at or above the dirty threshold with half
/// of it (`domain_dirty_limits`), so a larger value would not mean what it says.
fn dirty_background_bytes_target(dirty_bytes: u64) -> u64 {
    let limit = if dirty_bytes == 0 {
        DIRTY_BYTES_TARGET
    } else {
        dirty_bytes
    };
    DIRTY_BACKGROUND_BYTES_TARGET.min(limit / 2)
}

fn eval_dirty_background_bytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/dirty_background_bytes";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    let bytes = read_sysctl_u64(path);
    let ratio_path = "/proc/sys/vm/dirty_background_ratio";
    let ratio = if std::path::Path::new(ratio_path).exists() {
        read_sysctl_u64(ratio_path)
    } else {
        0
    };
    let dirty_bytes = read_sysctl_u64("/proc/sys/vm/dirty_bytes");
    dirty_background_bytes_recommendation(info.memory_total_gb, bytes, ratio, dirty_bytes, recs);
    1
}

/// Value-driven core of the dirty-background-bytes rule, separated so
/// tests can force the `bytes == 0 && ratio > 5` branch on any host (the
/// sysctls are read from the live /proc, which a test cannot control).
fn dirty_background_bytes_recommendation(
    memory_total_gb: u64,
    bytes: u64,
    ratio: u64,
    dirty_bytes: u64,
    recs: &mut Vec<Recommendation>,
) {
    if bytes == 0 && ratio > 5 {
        recs.push(Recommendation {
            param: "vm.dirty_background_bytes".to_string(),
            // current_value feeds the rollback ledger's `previous` field,
            // which is written back verbatim on `ktuner rollback`. A
            // human-readable annotation here would be rejected by the
            // kernel as EINVAL, making the param permanently unrestorable.
            // The ratio detail already appears in the reason.
            current_value: "0".to_string(),
            // Keep the background threshold strictly below the dirty
            // limit in effect after tuning (400d0105f): the kernel
            // silently halves a background threshold that reaches the
            // dirty threshold, so a larger value would not mean what it
            // says.
            recommended_value: dirty_background_bytes_target(dirty_bytes).to_string(),
            reason: format!("{}GB 内存 dirty_background_ratio {}% = {}GB 脏页才开始后台刷盘，用 bytes 可精确控制",
                memory_total_gb, ratio, memory_total_gb * ratio / 100),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
}

fn eval_hardlockup_panic(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/hardlockup_panic";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    // Hard-lockup detection relies on the NMI watchdog. ktuner separately
    // recommends disabling nmi_watchdog for perf — if it is already off, this
    // panic setting can never fire, so don't recommend a no-op.
    let nmi = "/proc/sys/kernel/nmi_watchdog";
    if std::path::Path::new(nmi).exists() && read_sysctl_u64(nmi) == 0 {
        return 1;
    }
    let _ = info;
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.hardlockup_panic".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "CPU 硬锁死后不 panic 会导致系统假死无法自动恢复，应启用以触发自动重启"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

// NOTE: kernel.softlockup_panic rule removed. A soft lockup is frequently a
// transient false positive (heavy load, hypervisor steal, long-but-legitimate
// work); rebooting the whole server on one is too aggressive a default for the
// beginners ktuner targets.

fn eval_sched_rt_runtime(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_rt_runtime_us";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = std::fs::read_to_string(path)
        .unwrap_or_default()
        .trim()
        .parse::<i64>()
        .unwrap_or(950000);
    if current == -1 {
        recs.push(Recommendation {
            param: "kernel.sched_rt_runtime_us".to_string(),
            current_value: "-1".to_string(),
            recommended_value: "950000".to_string(),
            reason: "实时任务可无限占用 CPU（无上限），可能导致普通进程饿死系统无响应".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_tcp_thin_linear_timeouts(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_thin_linear_timeouts";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_thin_linear_timeouts".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "稀疏流线性超时模式已启用，可能导致连接在弱网环境下过快断开".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_arp_notify(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/arp_notify";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if arp_tuning_skipped(info.network.len(), has_bond()) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.arp_notify".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "多网卡环境未启用 ARP 通知，IP 变更或故障切换时对端 ARP 缓存可能不更新"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_default_arp_announce(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/arp_announce";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if arp_tuning_skipped(info.network.len(), has_bond()) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 2 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.arp_announce".to_string(),
            current_value: current.to_string(),
            recommended_value: "2".to_string(),
            reason: "新建网络接口的 ARP 通告策略不佳，可能导致 ARP 回复从错误的源地址发出"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_default_arp_ignore(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/arp_ignore";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if arp_tuning_skipped(info.network.len(), has_bond()) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.arp_ignore".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "新建网络接口对所有 ARP 请求都回复，多网卡时可能导致 ARP 冲突".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_default_send_redirects(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_default_send_redirects_at(
        info,
        recs,
        "/proc/sys/net/ipv4/conf/default/send_redirects",
        std::path::Path::new("/proc/sys/net/ipv4/conf"),
    )
}

/// Path-injectable form of [`eval_default_send_redirects`] (the `eval_*_at`
/// idiom). The `default` template is what new interfaces inherit, and a
/// redirect can still only be sent by an interface that forwards — see
/// [`eval_send_redirects_at`] for the kernel path and the documentation line.
/// The `default` forwarding template counts as forwarding here, so a host that
/// arms future interfaces keeps the recommendation. Like every
/// `devinet_conf_proc` entry the slot is a plain `proc_dointvec` int with no
/// min/max, so the value is read SIGNED and the gate is the consumer's own
/// truthiness (`IN_DEV_TX_REDIRECTS`): -1 is enabled and must be flagged.
fn eval_default_send_redirects_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    conf_root: &std::path::Path,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if !any_interface_forwards(conf_root) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.send_redirects".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "新建网络接口默认发送 ICMP 重定向，非路由器应禁用以防网络拓扑探测".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_suid_dumpable(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/suid_dumpable";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "fs.suid_dumpable".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "SUID 程序的 core dump 可能泄露敏感信息（如密码哈希），应禁用".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_icmp_ignore_bogus(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/icmp_ignore_bogus_error_responses";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.icmp_ignore_bogus_error_responses".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未忽略虚假 ICMP 错误响应，可能被利用来做网络探测或拒绝服务".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_arp_filter(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_arp_filter_at(info, recs, "/proc/sys/net/ipv4/conf/all/arp_filter")
}

/// Path-injectable form of [`eval_arp_filter`] (the `eval_*_at` idiom).
/// `conf/all/arp_filter` is a plain `proc_dointvec` int slot
/// (`devinet_conf_proc` in net/ipv4/devinet.c, no min/max) read through
/// `IN_DEV_ARPFILTER` (include/linux/inetdevice.h), an `IN_DEV_ORCONF`
/// truthiness test consumed by `net/ipv4/arp.c` as
/// `if (!dont_send && IN_DEV_ARPFILTER(in_dev))`. -1 is legal and enabled,
/// so the unsigned reader's fallback 0 made the `== 0` gate report a
/// multi-NIC host as unfiltered.
fn eval_arp_filter_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if arp_tuning_skipped(info.network.len(), has_bond()) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.arp_filter".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: format!(
                "多网卡（{}个）环境下未启用 ARP 过滤，可能导致 ARP 响应从错误的接口发出",
                info.network.len()
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_cfs_bandwidth_slice(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_cfs_bandwidth_slice_us";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores <= 16 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 4000 {
        recs.push(Recommendation {
            param: "kernel.sched_cfs_bandwidth_slice_us".to_string(),
            current_value: current.to_string(),
            recommended_value: "3000".to_string(),
            reason: format!(
                "{}核 CPU CFS 带宽分片 {}μs 偏大，减小可改善 cgroup 带宽限制的响应精度",
                info.cpu_cores, current
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_tw_recycle(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_tw_recycle";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_tw_recycle".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason:
                "tcp_tw_recycle 在 NAT 环境下会导致大量连接失败（已在 Linux 4.12 中移除），必须关闭"
                    .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_orphan_retries(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_orphan_retries";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    orphan_retries_recommendation(read_sysctl_u64(path), recs);
    1
}

/// Value-driven core of the orphan-retries rule, separated so tests can
/// force the `current == 0 || current > 3` branch on any host.
fn orphan_retries_recommendation(current: u64, recs: &mut Vec<Recommendation>) {
    if current == 0 || current > 3 {
        // Plain numeric current_value: rollback writes it back verbatim,
        // and the kernel-default-of-8 note for `0` belongs in the reason.
        let reason = if current == 0 {
            "孤儿连接重试次数为 0（内核实际按默认 8 次处理），显式收紧到 2 可加速资源回收"
                .to_string()
        } else {
            "孤儿连接（对端无响应）重试次数过多，占用资源时间过长，减少可加速资源回收".to_string()
        };
        recs.push(Recommendation {
            param: "net.ipv4.tcp_orphan_retries".to_string(),
            current_value: current.to_string(),
            recommended_value: "2".to_string(),
            reason,
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
}

fn eval_tcp_early_retrans(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_early_retrans";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_early_retrans".to_string(),
            current_value: "0".to_string(),
            recommended_value: "3".to_string(),
            reason: "TCP 早期重传被禁用，启用可减少丢包后的恢复延迟（ER + TLP）".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_ip_no_pmtu_disc(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/ip_no_pmtu_disc";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv4.ip_no_pmtu_disc".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "PMTU 发现被禁用，可能导致大包被静默丢弃造成连接卡死（黑洞路由）".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_wakeup_granularity(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_wakeup_granularity_ns";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores <= 16 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 3000000 {
        recs.push(Recommendation {
            param: "kernel.sched_wakeup_granularity_ns".to_string(),
            current_value: current.to_string(),
            recommended_value: "3000000".to_string(),
            reason: format!(
                "{}核 CPU 唤醒粒度 {}ms 过大，降低可减少调度延迟提升响应速度",
                info.cpu_cores,
                current / 1000000
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_extfrag_threshold(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/extfrag_threshold";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 300 {
        recs.push(Recommendation {
            param: "vm.extfrag_threshold".to_string(),
            current_value: current.to_string(),
            recommended_value: "100".to_string(),
            reason: format!(
                "大内存（{}GB）机器外部碎片化阈值过高，降低可更积极地进行内存整理避免大页分配失败",
                info.memory_total_gb
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_msgmax(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/msgmax";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 65536 {
        recs.push(Recommendation {
            param: "kernel.msgmax".to_string(),
            current_value: current.to_string(),
            recommended_value: "65536".to_string(),
            reason: "IPC 消息最大字节数过低，数据库和中间件进程间通信可能受限".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_msgmnb(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/msgmnb";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 65536 {
        recs.push(Recommendation {
            param: "kernel.msgmnb".to_string(),
            current_value: current.to_string(),
            recommended_value: "65536".to_string(),
            reason: "IPC 消息队列最大字节数过低，高吞吐场景下进程间通信可能阻塞".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

// NOTE: kernel.modules_disabled and kernel.kexec_load_disabled rules were
// removed deliberately. Both are one-way runtime latches: once set to 1 the
// kernel refuses to set them back to 0 until reboot, so `ktuner rollback`
// cannot undo them — violating ktuner's core safe/reversible promise — and
// they break on-demand module loading / kdump. Admins who truly want this
// hardening set it themselves; an auto-tuner aimed at beginners must not.

fn eval_user_reserve_kbytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_user_reserve_kbytes_at(
        info,
        recs,
        "/proc/sys/vm/user_reserve_kbytes",
        "/proc/sys/vm/overcommit_memory",
    )
}

/// Path-injectable form of [`eval_user_reserve_kbytes`] (the `eval_*_at`
/// idiom) so the overcommit-mode precondition is assertable against synthetic
/// files.
///
/// Same condition as its sibling, and the documented one: `user_reserve_kbytes`
/// is only consulted under overcommit "never" — the kernel documentation opens
/// with "When overcommit_memory is set to 2, 'never overcommit' mode, reserve
/// min(3% of current process size, user_reserve_kbytes) of free memory"
/// (sysctl/vm.rst) — and `__vm_enough_memory()` (mm/util.c) returns before the
/// reserve under the default guess mode. The reason's promise of a usable
/// login path therefore only holds where the kernel reads the value.
fn eval_user_reserve_kbytes_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    overcommit_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    if !overcommit_reserves_are_live(overcommit_path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    let recommended: u64 = 262144;
    if current > recommended {
        return 1;
    }
    if current < 65536 {
        recs.push(Recommendation {
            param: "vm.user_reserve_kbytes".to_string(),
            current_value: current.to_string(),
            recommended_value: recommended.to_string(),
            reason: format!(
                "大内存（{}GB）机器用户空间预留内存过低，OOM 时可能导致无法登录系统恢复",
                info.memory_total_gb
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_shm_rmid_forced(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/shm_rmid_forced";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.shm_rmid_forced".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "进程退出后孤儿共享内存段不会自动回收，长期运行可能导致共享内存泄漏"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sem(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_sem_at(info, recs, "/proc/sys/kernel/sem")
}

/// Path-injectable form (the `eval_*_at` idiom) so the database gate is
/// unit-testable against a temp file instead of the live /proc.
fn eval_sem_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    // The shared boundary-aware predicate, not raw name equality: worker
    // comms are role-suffixed ("postgres: writer") and collectors are not
    // the database — is_database_present already encodes both.
    let has_db = is_database_present(info);
    if !has_db {
        return 1;
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let vals: Vec<u64> = content
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    // Raise-only per field: the old fixed quadruple "1024 65536 256 4096"
    // lowered semmns/semopm/semmni an administrator deliberately raised
    // whenever any single checked field was low, and semopm was rewritten
    // without ever being checked.
    if let Some(recommended) = sem_recommendation(&vals) {
        let current_str = vals
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        recs.push(Recommendation {
            param: "kernel.sem".to_string(),
            current_value: current_str,
            recommended_value: recommended,
            reason: "数据库场景下信号量参数过低，可能导致连接数受限或 semget() 失败".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_gc_stale_time(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/gc_stale_time";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 120 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.gc_stale_time".to_string(),
            current_value: current.to_string(),
            recommended_value: "120".to_string(),
            reason: "ARP 缓存过期时间过长，网络拓扑变化后可能长时间使用过期的 MAC 地址".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_shmmni(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_shmmni_at(info, recs, "/proc/sys/kernel/shmmni")
}

/// Path-injectable form (the `eval_*_at` idiom) so the database gate is
/// unit-testable against a temp file instead of the live /proc.
fn eval_shmmni_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    // The shared boundary-aware predicate, not raw name equality: worker
    // comms are role-suffixed ("postgres: writer") and collectors are not
    // the database — is_database_present already encodes both.
    let has_db = is_database_present(info);
    if !has_db && info.memory_total_gb < 128 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 8192 {
        recs.push(Recommendation {
            param: "kernel.shmmni".to_string(),
            current_value: current.to_string(),
            recommended_value: "8192".to_string(),
            reason: "共享内存段数上限过低，数据库和大内存应用创建共享内存段时可能受限".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_protected_fifos(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/protected_fifos";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "fs.protected_fifos".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未启用 FIFO 文件保护，/tmp 等全局可写目录下存在 FIFO 劫持攻击风险".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_tcp_fack(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_fack";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    if let Some(rec) = tcp_fack_recommendation(read_sysctl_u64(path), &info.kernel_version) {
        recs.push(rec);
    }
    1
}

/// Emit the `net.ipv4.tcp_fack` recommendation for an already-read value.
///
/// Split out from the file probe so the version gate is testable on any host.
fn tcp_fack_recommendation(current: u64, kernel_version: &str) -> Option<Recommendation> {
    // FACK was removed from the TCP stack in Linux 4.15 (commit 95f5acbf3e12,
    // "net: tcp: remove FACK from the code"), replaced by RACK loss detection.
    // The sysctl file still exists on modern kernels but nothing reads it, so
    // recommending "1" there is dead advice: it claims fewer spurious
    // retransmits while changing no behavior at all.
    if kernel_at_least(kernel_version, 4, 15) {
        return None;
    }
    if current != 0 {
        return None;
    }
    Some(Recommendation {
        param: "net.ipv4.tcp_fack".to_string(),
        current_value: "0".to_string(),
        recommended_value: "1".to_string(),
        reason: "TCP Forward Acknowledgement 可改善丢包恢复效率，减少不必要的重传".to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    })
}

fn eval_tcp_reordering(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_reordering";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 3 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_reordering".to_string(),
            current_value: current.to_string(),
            recommended_value: "3".to_string(),
            reason: "TCP 乱序容忍度过低，容易误判丢包触发不必要的快速重传".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_energy_aware(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_energy_aware";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "kernel.sched_energy_aware".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器场景不需要节能调度，关闭可避免性能损失".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_percpu_pagelist_high_fraction(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/percpu_pagelist_high_fraction";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "vm.percpu_pagelist_high_fraction".to_string(),
            current_value: "0".to_string(),
            recommended_value: "8".to_string(),
            reason: format!(
                "大内存服务器（{}GB）设置 per-CPU 页面列表比例可减少跨 NUMA zone lock 竞争",
                info.memory_total_gb
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_accept_ra(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_accept_ra_at(info, recs, "/proc/sys/net/ipv6/conf/default/accept_ra")
}

/// Path-injectable form of [`eval_accept_ra`] (the `eval_*_at` idiom).
///
/// `accept_ra` is an `__s32` slot of `struct ipv6_devconf`
/// (`include/linux/ipv6.h`) registered through a plain `proc_dointvec` with no
/// min/max (`net/ipv6/addrconf.c`), so -1 is a legal value; `ipv6_accept_ra()`
/// (`include/net/ipv6.h`) reads the slot as a truthiness test when forwarding
/// is off, so -1 means Router Advertisements are accepted. The unsigned reader
/// parses "-1" to Err and falls back to 0 — the *disabled* value — so the old
/// `> 0` gate stayed silent on a host that accepts RAs. This is the IPv6
/// counterpart of the ipv4 devconf booleans read signed in f57a3c81a.
fn eval_accept_ra_at(info: &SystemInfo, recs: &mut Vec<Recommendation>, path: &str) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "net.ipv6.conf.default.accept_ra".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器不应接受 IPv6 路由通告，防止路由被外部覆盖导致网络异常".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_tcp_recovery(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_recovery";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_recovery".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未启用 RACK 丢包检测，RACK 比传统 dupthresh 更准确地检测丢包和乱序"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_comp_sack_delay(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_comp_sack_delay_ns";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 1000000 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_comp_sack_delay_ns".to_string(),
            current_value: current.to_string(),
            recommended_value: "1000000".to_string(),
            reason: format!("TCP 压缩 SACK 延迟 {current}ns 过大，减小可加速丢包恢复"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_skb_frag_coalesce(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/skb_defer_max";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 64 {
        recs.push(Recommendation {
            param: "net.core.skb_defer_max".to_string(),
            current_value: current.to_string(),
            recommended_value: "64".to_string(),
            reason: format!("SKB 延迟释放上限 {current} 偏低，增大可减少跨 CPU 的内存释放开销"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_neigh_proxy_delay(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/proxy_delay";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 80 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.proxy_delay".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: format!("ARP 代理延迟 {current}*10ms 过大，服务器通常不需要代理 ARP"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_pacing_ca_ratio(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_pacing_ca_ratio";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 120 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_pacing_ca_ratio".to_string(),
            current_value: current.to_string(),
            recommended_value: "120".to_string(),
            reason: format!("TCP pacing 拥塞避免阶段速率比 {current}% 偏低，增大可提升发送速率"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_pacing_ss_ratio(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_pacing_ss_ratio";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 200 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_pacing_ss_ratio".to_string(),
            current_value: current.to_string(),
            recommended_value: "200".to_string(),
            reason: format!(
                "TCP pacing 慢启动速率比 {current}% 偏低，增大可加速慢启动阶段的带宽探测"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_comp_sack_nr(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_comp_sack_nr";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 44 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_comp_sack_nr".to_string(),
            current_value: current.to_string(),
            recommended_value: "44".to_string(),
            reason: format!(
                "TCP 压缩 SACK 最大数量 {current} 过大，减小可让 SACK 更及时发送加速恢复"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_thin_dupack(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_thin_dupack";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_thin_dupack".to_string(),
            current_value: current.to_string(),
            recommended_value: "1".to_string(),
            reason: "启用 thin stream 快速重传优化，对低并发长连接场景减少重传等待时间".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_invalid_ratelimit(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_invalid_ratelimit";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 500 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_invalid_ratelimit".to_string(),
            current_value: current.to_string(),
            recommended_value: "500".to_string(),
            reason: format!(
                "TCP 无效段响应速率限制 {current} ms 过低，增大可防止攻击者利用无效报文探测"
            ),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_tcp_init_cwnd(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_init_cwnd";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 10 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_init_cwnd".to_string(),
            current_value: current.to_string(),
            recommended_value: "10".to_string(),
            reason: format!(
                "TCP 初始拥塞窗口 {current} 偏小，RFC 6928 推荐 10 以加速新连接首屏加载"
            ),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_tso_win_divisor(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_tso_win_divisor";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 8 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_tso_win_divisor".to_string(),
            current_value: current.to_string(),
            recommended_value: "3".to_string(),
            reason: format!("TSO 窗口分割因子 {current} 过大，会导致 TSO 段过小降低吞吐量"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_schedstats(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_schedstats";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores < 4 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "kernel.sched_schedstats".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "调度器统计信息收集已开启，每次上下文切换都有额外开销，生产环境建议关闭"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_inotify_max_queued_events(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/inotify/max_queued_events";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 65536 {
        recs.push(Recommendation {
            param: "fs.inotify.max_queued_events".to_string(),
            current_value: current.to_string(),
            recommended_value: "65536".to_string(),
            reason: format!(
                "inotify 事件队列上限 {current} 偏低，文件变更密集时可能丢失事件导致应用异常"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_max_reordering(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_max_reordering";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 300 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_max_reordering".to_string(),
            current_value: current.to_string(),
            recommended_value: "300".to_string(),
            reason: format!("TCP 最大重排序容忍度 {current} 偏低，高延迟网络中可能误判乱序为丢包触发不必要的重传"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_retrans_collapse(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_retrans_collapse";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_retrans_collapse".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "TCP 重传合并已启用，可能将多个小段合并为一个大段导致接收端解析异常，建议关闭"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_app_win(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_app_win";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 31 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_app_win".to_string(),
            current_value: current.to_string(),
            recommended_value: "31".to_string(),
            reason: format!(
                "TCP 应用窗口保留比例 1/{current} 过高，减小可让更多缓冲区用于实际传输"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_ip_default_ttl(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/ip_default_ttl";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 64 {
        recs.push(Recommendation {
            param: "net.ipv4.ip_default_ttl".to_string(),
            current_value: current.to_string(),
            recommended_value: "64".to_string(),
            reason: format!("IP 默认 TTL {current} 低于标准值 64，可能导致远端网络不可达"),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_frto(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_frto";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_frto".to_string(),
            current_value: "0".to_string(),
            recommended_value: "2".to_string(),
            reason: "未启用 F-RTO（Forward RTO-Recovery），无法区分真正的丢包和虚假超时重传"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_icmp_ratelimit(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/icmp_ratelimit";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.icmp_ratelimit".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1000".to_string(),
            reason: "ICMP 响应无速率限制，可能被利用进行反射放大攻击或信息探测".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_igmp_max_memberships(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/igmp_max_memberships";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 256 {
        recs.push(Recommendation {
            param: "net.ipv4.igmp_max_memberships".to_string(),
            current_value: current.to_string(),
            recommended_value: "256".to_string(),
            reason: format!("IGMP 组播成员上限 {current} 偏低，大量容器或微服务可能耗尽组播配额"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_randomize_va_space_full(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/randomize_va_space";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "kernel.randomize_va_space".to_string(),
            current_value: "1".to_string(),
            recommended_value: "2".to_string(),
            reason: "ASLR 仅部分启用（栈+库），建议设为 2 同时随机化堆地址，提供完整保护"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_max_user_instances(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/inotify/max_user_instances";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 512 {
        recs.push(Recommendation {
            param: "fs.inotify.max_user_instances".to_string(),
            current_value: current.to_string(),
            recommended_value: "1024".to_string(),
            reason: format!(
                "inotify 实例上限 {current} 偏低，容器或大量服务场景可能耗尽导致 watch 失败"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_keys_maxkeys(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/keys/maxkeys";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 2000 {
        recs.push(Recommendation {
            param: "kernel.keys.maxkeys".to_string(),
            current_value: current.to_string(),
            recommended_value: "2000".to_string(),
            reason: format!("内核密钥环上限 {current} 偏低，大量容器或服务可能耗尽密钥配额"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

#[allow(clippy::ptr_arg)]
fn eval_numa_stat(info: &SystemInfo, _recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/numa_stat";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.numa_nodes <= 1 {
        return 1;
    }
    1
}

// NOTE: eval_sched_cfs_bw removed — it recommended raising
// sched_cfs_bandwidth_slice_us to 5000 (the kernel default) while
// eval_sched_cfs_bandwidth_slice recommends lowering it to 3000 for throttling
// precision. Two rules pulling the same knob in opposite directions is
// incoherent; keep only the precision-oriented one.

fn eval_tcp_base_mss(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_base_mss";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 1024 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_base_mss".to_string(),
            current_value: current.to_string(),
            recommended_value: "1024".to_string(),
            reason: format!("TCP 基础 MSS {current} 过小，PMTU 探测起点过低会降低初始传输效率"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_min_tso_segs(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_min_tso_segs";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 2 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_min_tso_segs".to_string(),
            current_value: current.to_string(),
            recommended_value: "2".to_string(),
            reason: "TCP TSO 最小段数过低，增大可提高大包合并效率减少中断次数".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_neigh_default_gc_interval(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/gc_interval";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 30 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.gc_interval".to_string(),
            current_value: current.to_string(),
            recommended_value: "30".to_string(),
            reason: format!("ARP 垃圾回收间隔 {current}s 过短，频繁 GC 增加 CPU 开销"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_neigh_default_gc_stale_time(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/gc_stale_time";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 120 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.gc_stale_time".to_string(),
            current_value: current.to_string(),
            recommended_value: "120".to_string(),
            reason: format!("ARP 缓存过期时间 {current}s 偏短，增大可减少 ARP 请求频率"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_fastopen_blackhole_timeout(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_fastopen_blackhole_timeout_sec";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 3600 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_fastopen_blackhole_timeout_sec".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "TFO 黑洞超时过长，禁用超时可让每次连接都尝试 TFO 以获得最佳延迟".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_max_queued_signals(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/rtsig-max";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 4096 {
        recs.push(Recommendation {
            param: "kernel.rtsig-max".to_string(),
            current_value: current.to_string(),
            recommended_value: "4096".to_string(),
            reason: format!("实时信号队列上限 {current} 偏低，高并发 IO 场景可能溢出"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

#[allow(clippy::ptr_arg)]
fn eval_tcp_available_ulp(info: &SystemInfo, _recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_available_ulp";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    if let Ok(content) = std::fs::read_to_string(path) {
        let ulps = content.trim();
        if ulps.contains("tls") {
            return 1;
        }
    }
    1
}

fn eval_keys_maxbytes(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/keys/maxbytes";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 25000 {
        recs.push(Recommendation {
            param: "kernel.keys.maxbytes".to_string(),
            current_value: current.to_string(),
            recommended_value: "25000".to_string(),
            reason: format!("内核密钥环容量 {current} 字节偏低，大量加密操作可能耗尽配额"),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_pipe_max_size(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/pipe-max-size";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 1048576 {
        recs.push(Recommendation {
            param: "fs.pipe-max-size".to_string(),
            current_value: current.to_string(),
            recommended_value: "1048576".to_string(),
            reason: format!(
                "管道最大容量 {}KB 偏小，大数据管道传输可能阻塞",
                current / 1024
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// Query the running kernel's page size (bytes). `shmall` is measured in pages,
/// and the page size is architecture/kernel dependent — 4 KiB on x86_64 but up
/// to 64 KiB on arm64 — so it MUST be read at runtime, never hardcoded.
/// Falls back to 4096 if `sysconf` fails (a non-positive return).
fn current_page_size() -> u64 {
    // SAFETY: sysconf(_SC_PAGESIZE) is a pure, side-effect-free query.
    let sz = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if sz > 0 {
        sz as u64
    } else {
        4096
    }
}

/// Recommended `kernel.shmall` (in pages): half of physical RAM, converted to
/// pages using the given page size. Returns 0 for a zero page size (guards the
/// division). Kept pure so it can be tested across page sizes.
fn shmall_target_pages(memory_total_gb: u64, page_size: u64) -> u64 {
    if page_size == 0 {
        return 0;
    }
    (memory_total_gb * 1024 * 1024 * 1024 / page_size) / 2
}

fn eval_shmall(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    page_size: impl FnOnce() -> u64,
    read_current: impl FnOnce(&str) -> Option<u64>,
) -> usize {
    let Some(current) = read_current("/proc/sys/kernel/shmall") else {
        return 1;
    };
    let target_pages = shmall_target_pages(info.memory_total_gb, page_size());
    if current < target_pages && target_pages > 0 {
        recs.push(Recommendation {
            param: "kernel.shmall".to_string(),
            current_value: current.to_string(),
            recommended_value: target_pages.to_string(),
            reason: format!(
                "共享内存总页数上限偏低（当前 {} 页），{}GB 内存建议至少 {} 页（内存的一半）",
                current, info.memory_total_gb, target_pages
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_compact_memory(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/compact_memory";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let proactive_path = "/proc/sys/vm/compaction_proactiveness";
    if std::path::Path::new(proactive_path).exists() {
        let current = read_sysctl_u64(proactive_path);
        if current == 0 {
            recs.push(Recommendation {
                param: "vm.compaction_proactiveness".to_string(),
                current_value: "0".to_string(),
                recommended_value: "20".to_string(),
                reason: "未启用主动内存压缩，长时间运行后内存碎片化可能导致高阶分配失败"
                    .to_string(),
                confidence: Confidence::Medium,
                category: Category::Performance,
                writable: true,
            });
        }
        return 1;
    }
    1
}

fn eval_min_slab_ratio(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_min_slab_ratio_at(
        info,
        recs,
        "/proc/sys/vm/min_slab_ratio",
        "/proc/sys/vm/zone_reclaim_mode",
    )
}

/// Path-injectable form of [`eval_min_slab_ratio`] (the `eval_*_at` idiom) so
/// the node-reclaim precondition is assertable against synthetic files.
///
/// The knob feeds `pgdat->min_slab_pages` (`setup_min_slab_ratio()` in
/// mm/page_alloc.c), which only `node_reclaim()` reads — and the allocator
/// calls node reclaim only while `node_reclaim_enabled()` holds, i.e. while
/// `/proc/sys/vm/zone_reclaim_mode` is non-zero (include/linux/swap.h and
/// mm/page_alloc.c, v6.6). ktuner itself recommends turning that mode off on
/// every multi-NUMA host, so once its own plan is applied (or on any host that
/// keeps the default 0) the write this rule asks for cannot change any
/// behavior — the "never recommend a no-op" rule the hardlockup_panic and
/// page-cluster gates already follow. An unreadable mode file counts as "off",
/// so the rule stays quiet rather than promising a reclaim it cannot verify.
fn eval_min_slab_ratio_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
    zone_reclaim_path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    if read_sysctl_u64(zone_reclaim_path) == 0 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 5 {
        recs.push(Recommendation {
            param: "vm.min_slab_ratio".to_string(),
            current_value: current.to_string(),
            recommended_value: "5".to_string(),
            reason: format!(
                "大内存服务器（{}GB）应确保最低 slab 回收比例，防止 dentry/inode 缓存膨胀",
                info.memory_total_gb
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_autocorking(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_autocorking";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_autocorking".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "TCP 自动合包可减少小包数量提高网络效率，建议保持开启".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_workaround_signed_windows(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_workaround_signed_windows";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_workaround_signed_windows".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "此兼容选项限制 TCP 窗口大小，现代系统不需要，关闭可恢复大窗口传输".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_protected_regular(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/fs/protected_regular";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "fs.protected_regular".to_string(),
            current_value: "0".to_string(),
            recommended_value: "2".to_string(),
            reason:
                "未启用 regular 文件保护，攻击者可在 sticky 目录中利用符号链接创建文件进行权限提升"
                    .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_bpf_jit_enable(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/bpf_jit_enable";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.core.bpf_jit_enable".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "BPF JIT 编译器未启用，启用后 eBPF 程序和包过滤性能大幅提升（10-50 倍）"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_bpf_jit_harden(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_bpf_jit_harden_at("/proc/sys/net/core/bpf_jit_harden", recs)
}

/// Path-injectable form (the `eval_*_at` idiom) so the unreadable branch is
/// assertable on any host, without root.
///
/// `net.core.bpf_jit_harden` is created 0600 root-owned and its handler
/// (`proc_dointvec_minmax_bpf_restricted`, net/core/sysctl_net_core.c)
/// returns -EPERM unless the caller holds CAP_SYS_ADMIN, so the read fails
/// for every unprivileged `ktuner check`. `read_sysctl_u64` maps a failed
/// read to 0, which is this rule's firing value: the report claimed "BPF JIT
/// 加固未启用" for a host whose value was never read, and the same run marks
/// the parameter `"writable": false`. `why` already refuses to present a
/// failed read as a value ("A failed read is an error, never a value"); the
/// rule must not invent one either. A readable 0 is still a finding.
fn eval_bpf_jit_harden_at(path: &str, recs: &mut Vec<Recommendation>) -> usize {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return 1;
    };
    let Ok(current) = raw.trim().parse::<u64>() else {
        return 1;
    };
    if current == 0 {
        recs.push(Recommendation {
            param: "net.core.bpf_jit_harden".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "BPF JIT 编译器加固未启用，启用后可防止利用即时编译的代码注入攻击".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

#[allow(clippy::ptr_arg)]
fn eval_tcp_available_congestion(_info: &SystemInfo, _recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_available_congestion_control";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let content = read_sysctl_string(path);
    if !congestion_algo_available(&content, "bbr") {
        return 1;
    }
    let current_algo = read_sysctl_string("/proc/sys/net/ipv4/tcp_congestion_control");
    if current_algo.trim() != "bbr" {
        return 1;
    }
    1
}

fn eval_somaxconn_large(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/somaxconn";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if (4096..65535).contains(&current) && info.max_net_speed() >= 10000 {
        recs.push(Recommendation {
            param: "net.core.somaxconn".to_string(),
            current_value: current.to_string(),
            recommended_value: "65535".to_string(),
            reason: format!(
                "万兆网络下 somaxconn={current} 可能不够，建议增大到 65535 以应对突发连接"
            ),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_promote_secondaries(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    eval_promote_secondaries_at(
        info,
        recs,
        "/proc/sys/net/ipv4/conf/default/promote_secondaries",
    )
}

/// Path-injectable form of [`eval_promote_secondaries`] (the `eval_*_at`
/// idiom). `conf/default/promote_secondaries` is a plain `proc_dointvec` int
/// slot (`devinet_conf_proc` in net/ipv4/devinet.c, no min/max) read through
/// `IN_DEV_PROMOTE_SECONDARIES` (include/linux/inetdevice.h), an
/// `IN_DEV_ORCONF` truthiness test that `net/ipv4/devinet.c` consumes as
/// `int do_promote = IN_DEV_PROMOTE_SECONDARIES(in_dev)`. -1 is legal and
/// enabled, so the unsigned reader's fallback 0 made the `== 0` gate claim
/// secondary addresses are dropped on primary removal.
fn eval_promote_secondaries_at(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
    path: &str,
) -> usize {
    if !info.param_exists(path) {
        return 1;
    }
    // Any nonzero value is enabled, so -1 must not read as the value 0.
    let current = read_sysctl_i64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.promote_secondaries".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "未启用辅助地址自动提升，删除主 IP 地址时同网段的辅助地址也会被删除，可能导致网络中断".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_unres_qlen_bytes(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/neigh/default/unres_qlen_bytes";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 131072 && info.max_net_speed() >= 10000 {
        recs.push(Recommendation {
            param: "net.ipv4.neigh.default.unres_qlen_bytes".to_string(),
            current_value: current.to_string(),
            recommended_value: "262144".to_string(),
            reason: "万兆网络中 ARP 解析未完成时排队缓冲区偏小，突发新目标连接可能导致丢包"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_ip_nonlocal_bind(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/ip_nonlocal_bind";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    // HA / VIP setups (keepalived VRRP, HAProxy binding to floating IPs) require
    // ip_nonlocal_bind=1 on purpose — don't recommend disabling it there.
    if info.has_process("keepalived") || info.has_process("haproxy") {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.ip_nonlocal_bind".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "允许绑定非本地 IP 地址可能导致安全风险，除非使用高可用（VRRP/keepalived）否则应禁用".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_conntrack_tcp_timeout_established(
    info: &SystemInfo,
    recs: &mut Vec<Recommendation>,
) -> usize {
    let path = "/proc/sys/net/netfilter/nf_conntrack_tcp_timeout_established";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    conntrack_timeout_recommendation(read_sysctl_u64(path), recs);
    1
}

/// Value-driven core of the conntrack-timeout rule, separated so tests can
/// force the `current > 86400` branch on any host (the sysctl is read from
/// the live /proc, which a test cannot control).
fn conntrack_timeout_recommendation(current: u64, recs: &mut Vec<Recommendation>) {
    if current > 86400 {
        recs.push(Recommendation {
            param: "net.netfilter.nf_conntrack_tcp_timeout_established".to_string(),
            // Plain numeric: rollback writes this back verbatim; the
            // days annotation belongs in the reason, not the value.
            current_value: current.to_string(),
            recommended_value: "86400".to_string(),
            reason: "conntrack 已建立连接的超时默认 5 天太长，高并发下大量条目占满表导致丢包，缩短到 1 天".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
}

fn eval_softlockup_all_cpu_backtrace(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/softlockup_all_cpu_backtrace";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores <= 32 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.softlockup_all_cpu_backtrace".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "大核数机器出现 softlockup 时只打印触发 CPU 的栈，启用全 CPU backtrace 有助于定位问题".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_compact_unevictable(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/compact_unevictable_allowed";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 64 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "vm.compact_unevictable_allowed".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "允许内存整理不可驱逐页面，减少大内存机器的碎片化问题".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_perf_cpu_time_max_percent(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/perf_cpu_time_max_percent";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.cpu_cores <= 16 {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 5 {
        recs.push(Recommendation {
            param: "kernel.perf_cpu_time_max_percent".to_string(),
            current_value: current.to_string(),
            recommended_value: "5".to_string(),
            reason: "大核数机器限制 perf 采样最大 CPU 占比，避免性能分析工具本身成为瓶颈"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

/// `kernel/hung_task.c` registers hung_task_warnings with a range of
/// [-1, INT_MAX], where -1 means "unlimited warnings" and 0 disables them.
/// The unsigned reader turned the legal "-1" into 0, so a host with warnings
/// unlimited was reported as disabled and `tune` rewrote it to 10, silently
/// capping the log. Pure so tests can force every branch.
fn hung_task_warnings_recommendation(current: i64, recs: &mut Vec<Recommendation>) {
    if current != 0 {
        return;
    }
    recs.push(Recommendation {
        param: "kernel.hung_task_warnings".to_string(),
        current_value: "0".to_string(),
        recommended_value: "10".to_string(),
        reason: "hung task 警告被禁用，无法发现进程卡死问题，建议至少保留一定数量的告警"
            .to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    });
}

fn eval_hung_task_warnings(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/hung_task_warnings";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    hung_task_warnings_recommendation(read_sysctl_i64(path), recs);
    1
}

fn eval_overcommit_ratio(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let oc_path = "/proc/sys/vm/overcommit_memory";
    let ratio_path = "/proc/sys/vm/overcommit_ratio";
    let kbytes_path = "/proc/sys/vm/overcommit_kbytes";
    if !std::path::Path::new(ratio_path).exists() {
        return 1;
    }
    let oc_mode = read_sysctl_u64(oc_path);
    if oc_mode != 2 {
        return 1;
    }
    let ratio = read_sysctl_u64(ratio_path);
    // The ratio reads 0 while its counterpart holds the commit limit, so the
    // probe has to hand the value branch that counterpart to tell "disabled"
    // apart from "too low". A kernel without the kbytes file reports 0, which
    // leaves the ratio advice exactly as it was.
    let kbytes = if std::path::Path::new(kbytes_path).exists() {
        read_sysctl_u64(kbytes_path)
    } else {
        0
    };
    overcommit_ratio_recommendation(oc_mode, ratio, kbytes, is_database_present(info), recs);
    1
}

/// Whether the sampled process list contains a database server, matched at name
/// boundaries exactly like every other DB-gated rule (`has_process`).
///
/// The old inline predicate substring-matched the whole comm
/// (`p.name.contains("mysql")`), so MySQL client tools — an open `mysql` shell,
/// `mysqldump`, `mysqlrouter` — counted as the database itself and a host that
/// only runs them was tuned as if the DB lived there: the same false-positive
/// class #4100 removed from `has_process` (etcdctl satisfied "etcd"), left
/// behind at this direct-iteration site.
fn is_database_present(info: &SystemInfo) -> bool {
    info.has_process("postgres")
        || info.has_process("mysqld")
        // MariaDB 10.4+ runs as mariadbd — the same OLTP database.
        || info.has_process("mariadbd")
        || info.has_process("oracle")
        // MongoDB and ClickHouse are databases in every sibling gate
        // (eval_swappiness, eval_thp, eval_dirty_ratio, eval_readahead)
        // but were missing from this shared memory-oriented predicate.
        || info.has_process("mongod")
        || info.has_process("clickhouse")
}

/// Pure core of the `vm.overcommit_ratio` rule: a strict-mode
/// (`overcommit_memory == 2`) database host whose ratio sits below 80 gets
/// exactly one raise-to-80 recommendation — unless `vm.overcommit_kbytes`
/// holds the commit limit, in which case the ratio reads 0 because the kernel
/// disabled it rather than because it is low.
///
/// `overcommit_kbytes` is the ratio's counterpart: `overcommit_ratio_handler`
/// zeroes `sysctl_overcommit_kbytes` on every write and
/// `overcommit_kbytes_handler` zeroes `sysctl_overcommit_ratio`, so only one
/// of the pair is ever configured, and `vm_commit_limit()` prefers a non-zero
/// kbytes. vm.rst's overcommit_kbytes section states the pair outright:
/// "Only one of them may be specified at a time. Setting one disables the
/// other (which then appears as 0 when read)." Raising a disabled ratio to 80
/// therefore does not adjust the limit the administrator set — it replaces a
/// fixed commit limit with 80% of RAM, which is the opposite of what the
/// reason promises.
///
/// Split from the /proc probe so the branch is assertable on any host instead
/// of only on one already running `overcommit_memory=2`.
fn overcommit_ratio_recommendation(
    oc_mode: u64,
    ratio: u64,
    kbytes: u64,
    db_present: bool,
    recs: &mut Vec<Recommendation>,
) {
    if kbytes != 0 || !db_present || oc_mode != 2 || ratio >= 80 {
        return;
    }
    recs.push(Recommendation {
        param: "vm.overcommit_ratio".to_string(),
        current_value: ratio.to_string(),
        recommended_value: "80".to_string(),
        reason: "overcommit_memory=2 模式下 ratio 过低会限制可用内存，数据库建议设为 80-90"
            .to_string(),
        confidence: Confidence::Medium,
        category: Category::Performance,
        writable: true,
    });
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Formats `vals` with each field raised to at least its floor, preserving
/// any value an administrator deliberately set above the floor. Rewriting a
/// multi-field sysctl with fixed numbers would lower those fields instead
/// (e.g. kernel.sem "512 1024000000 500 32000" → "1024 65536 256 4096"
/// collapses semmns by four orders of magnitude).
fn per_field_max(vals: &[u64], floors: &[u64]) -> String {
    vals.iter()
        .zip(floors.iter().chain(std::iter::repeat(&0)))
        .map(|(v, floor)| v.max(floor).to_string())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Per-field floors for `kernel.sem` (semmsl, semmns, semopm, semmni) on a
/// database host. The recommendation raises each field to at least its floor
/// and otherwise keeps the current value. Returns `None` when every field
/// already meets its floor.
fn sem_recommendation(vals: &[u64]) -> Option<String> {
    const FLOORS: [u64; 4] = [1024, 65536, 256, 4096];
    if vals.len() < 4 || vals.iter().zip(FLOORS).all(|(v, floor)| *v >= floor) {
        return None;
    }
    Some(per_field_max(vals, &FLOORS))
}

fn read_sysctl_string(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// Whether a `uname -r`-style release string is at least `major.minor`.
///
/// Only the leading numeric `major.minor` is compared, so distro suffixes
/// ("6.8.0-40-generic", "5.15.0-microsoft-standard-WSL2") parse fine. Returns
/// false when the string does not start with two dot-separated numbers, in
/// which case the caller keeps its legacy (pre-gate) behavior.
fn kernel_at_least(version: &str, want_major: u64, want_minor: u64) -> bool {
    let mut parts = version
        .trim()
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty());
    let (Some(major), Some(minor)) = (
        parts.next().and_then(|s| s.parse().ok()),
        parts.next().and_then(|s| s.parse().ok()),
    ) else {
        return false;
    };
    (major, minor) >= (want_major, want_minor)
}

/// Whether the exact algorithm name `algo` appears in a whitespace-separated
/// `tcp_available_congestion_control` listing. Names must be compared as
/// whole tokens: a substring match accepts "bbr2"/"bbr_plus" when plain
/// "bbr" is not registered, and the kernel rejects writing an unregistered
/// name with ENOENT — a recommendation that can never be applied.
fn congestion_algo_available(available: &str, algo: &str) -> bool {
    available.split_whitespace().any(|name| name == algo)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbr_availability_requires_an_exact_algorithm_token() {
        // Plain bbr present, in any position.
        assert!(congestion_algo_available("reno cubic bbr", "bbr"));
        assert!(congestion_algo_available("bbr reno cubic", "bbr"));
        // Variants whose names merely contain "bbr" must NOT count: the
        // kernel rejects writing an unregistered algorithm name with ENOENT,
        // so a substring match produces a recommendation that can never be
        // applied.
        assert!(!congestion_algo_available("reno cubic bbr2", "bbr"));
        assert!(!congestion_algo_available("reno cubic bbr_plus", "bbr"));
        assert!(!congestion_algo_available("bbr2", "bbr"));
        // Absent entirely.
        assert!(!congestion_algo_available("reno cubic", "bbr"));
    }
    use crate::detect::*;

    #[test]
    fn tcp_fastopen_requires_the_all_listeners_flag() {
        // 0x1|0x2 is the value this rule used to write. Passive (server-side)
        // TFO needs 0x400 as well: __inet_listen_sk only fills a listener's
        // fastopenq.max_qlen when 0x2 AND 0x400 are both set, and
        // tcp_fastopen_queue_check refuses every SYN with data while that
        // length is 0 — so a listener that never called the TCP_FASTOPEN
        // socket option got no passive TFO from a host at 3.
        let rec = tcp_fastopen_recommendation(3).expect("3 does not enable server TFO");
        assert_eq!(rec.current_value, "3");
        assert_eq!(rec.recommended_value, "1027");
        assert_eq!(rec.param, "net.ipv4.tcp_fastopen");
    }

    #[test]
    fn tcp_fastopen_completes_the_kernel_default() {
        // Default 0x1 (client only): both the server flag and the
        // all-listeners flag are missing.
        let rec = tcp_fastopen_recommendation(1).expect("the kernel default is incomplete");
        assert_eq!(rec.recommended_value, "1027");
        // Server only.
        let rec = tcp_fastopen_recommendation(2).expect("server without 0x400 is incomplete");
        assert_eq!(rec.recommended_value, "1027");
    }

    #[test]
    fn tcp_fastopen_keeps_the_flags_the_host_set() {
        // 0x4 is TFO_CLIENT_NO_COOKIE and 0x200 is
        // TFO_SERVER_COOKIE_NOT_REQD; the sysctl is written as one word, so a
        // recommendation must not clear a flag the administrator chose.
        let rec = tcp_fastopen_recommendation(0x4).expect("0x4 alone is incomplete");
        assert_eq!(rec.recommended_value, "1031");
        // 0x201 | 0x1|0x2|0x400 = 0x603: the 0x200 flag survives too.
        let rec = tcp_fastopen_recommendation(0x200 | 0x1).expect("0x200|0x1 is incomplete");
        assert_eq!(rec.recommended_value, "1539");
    }

    #[test]
    fn tcp_fastopen_is_silent_once_every_required_flag_is_set() {
        assert!(tcp_fastopen_recommendation(0x1 | 0x2 | 0x400).is_none());
        // Extra flags on top of the required set stay complete.
        assert!(tcp_fastopen_recommendation(0x1 | 0x2 | 0x4 | 0x200 | 0x400).is_none());
    }

    #[test]
    fn tcp_fastopen_minus_one_is_already_fully_enabled() {
        // -1 is every flag bit set: the plain proc_dointvec registration
        // (no min/max, v5.10 and v6.6 alike) accepts it as a whole word, and
        // the kernel treats the host as fully TFO-enabled. The completion
        // test must see the signed value, not the unsigned fallback 0 that
        // used to make the rule recommend 1027 — a write that would clear
        // every administrator flag outside the required three.
        assert!(tcp_fastopen_recommendation(-1).is_none());
        // -2 keeps every administrator flag except 0x1; completing it must
        // set the required three without clearing anything: -2 | 0x403 == -1.
        let rec = tcp_fastopen_recommendation(-2).expect("-2 is missing the 0x1 flag");
        assert_eq!(rec.current_value, "-2");
        assert_eq!(rec.recommended_value, "-1");
    }

    #[test]
    fn tcp_fastopen_signed_read_survives_the_file_reader() {
        // The reader is the defect: read_sysctl_u64 parsed "-1" to Err and
        // fell back to 0, and the rule recommended enabling what was already
        // fully enabled. Feeding the path-injectable form a temp file keeps
        // the branch assertable on any host, listener or not.
        let path = std::env::temp_dir().join(format!("ktuner-tcp-fastopen-{}", std::process::id()));
        let path_str = path.to_str().unwrap();
        for (value, expect_rec) in [("-1", false), ("1027", false), ("3", true), ("-2", true)] {
            std::fs::write(&path, value).unwrap();
            let info = make_test_info();
            let mut recs = Vec::new();
            eval_tcp_fastopen_at(&info, &mut recs, path_str, true);
            let rec = recs.iter().find(|r| r.param == "net.ipv4.tcp_fastopen");
            assert_eq!(
                rec.is_some(),
                expect_rec,
                "value={value}: recommendation presence"
            );
            if let Some(rec) = rec {
                assert_eq!(
                    rec.current_value, value,
                    "current must be what the kernel holds"
                );
            }
        }
        // The listener gate still short-circuits on a host that never listens.
        std::fs::write(&path, "3").unwrap();
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_fastopen_at(&info, &mut recs, path_str, false);
        assert!(
            recs.iter().all(|r| r.param != "net.ipv4.tcp_fastopen"),
            "a host without listeners is not recommended TFO"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sem_recommendation_only_raises_fields() {
        // Everything at or above the floors: no recommendation.
        assert_eq!(sem_recommendation(&[32000, 1024000000, 500, 32000]), None);
        // One low field: raise it, keep every other field (the old fixed
        // quadruple collapsed semmns from 1024000000 to 65536 here).
        assert_eq!(
            sem_recommendation(&[512, 1024000000, 500, 32000]).as_deref(),
            Some("1024 1024000000 500 32000")
        );
        // semopm low alone is now detected (it was rewritten before without
        // ever being part of the trigger).
        assert_eq!(
            sem_recommendation(&[32000, 1024000000, 100, 32000]).as_deref(),
            Some("32000 1024000000 256 32000")
        );
        // All four low: the full floor tuple.
        assert_eq!(
            sem_recommendation(&[250, 32000, 32, 128]).as_deref(),
            Some("1024 65536 256 4096")
        );
        // Malformed input: no recommendation rather than a partial write.
        assert_eq!(sem_recommendation(&[1, 2, 3]), None);
    }

    #[test]
    fn overcommit_db_detection_matches_at_name_boundaries() {
        // The #4100 false-positive class, at the one site the boundary fix did
        // not reach: the old predicate substring-matched the whole comm, so
        // MySQL client tools counted as the database itself.
        fn info_with(names: &[&str]) -> SystemInfo {
            let mut info = make_test_info();
            info.processes = names
                .iter()
                .map(|n| ProcessInfo {
                    name: n.to_string(),
                })
                .collect();
            info
        }
        // MariaDB's daemon comm (10.4+) is the same OLTP database as
        // mysqld; without it in the shared predicate a MariaDB host got
        // none of the db-gated rules (sem, shmmni, overcommit_ratio).
        assert!(
            is_database_present(&info_with(&["mariadbd"])),
            "mariadbd is a database"
        );
        assert!(
            is_database_present(&info_with(&["mariadbd: writer"])),
            "a role-suffixed MariaDB worker is the database"
        );
        // The client tool is not the server.
        assert!(
            !is_database_present(&info_with(&["mariadb-dump"])),
            "mariadb-dump is not the database"
        );

        // Real database servers, including role-prefixed worker comms.
        // mongod and clickhouse are databases in every sibling gate
        // (eval_swappiness, eval_thp, eval_dirty_ratio, eval_readahead)
        // but were missing from this shared predicate.
        for name in [
            "postgres",
            "postgres: writer",
            "mysqld",
            "oracle",
            "mongod",
            "mongod: shard",
            "clickhouse",
            "clickhouse: writer",
        ] {
            assert!(
                is_database_present(&info_with(&[name])),
                "{name} is a database"
            );
        }
        // Client tools and proxies merely embed the server's name.
        for name in [
            "mysql",
            "mysqldump",
            "mysqlrouter",
            "mysqlbinlog",
            "pg_dump",
        ] {
            assert!(
                !is_database_present(&info_with(&[name])),
                "{name} is not the database"
            );
        }
        assert!(!is_database_present(&info_with(&[])));
    }

    #[test]
    fn sysv_ipc_rules_gate_on_boundary_aware_db_detection() {
        // The sem/shmmni gates re-implemented db detection with raw
        // `p.name ==` equality instead of is_database_present, so a process
        // list holding only role-suffixed worker comms ("postgres: writer",
        // the shape /proc shows for a busy PostgreSQL) skipped the SysV IPC
        // sizing recommendations entirely.
        fn info_with(names: &[&str]) -> SystemInfo {
            let mut info = make_test_info();
            info.processes = names
                .iter()
                .map(|n| ProcessInfo {
                    name: n.to_string(),
                })
                .collect();
            info
        }
        let dir = std::env::temp_dir().join(format!(
            "ktuner_ipc_gate_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sem_path = dir.join("sem");
        std::fs::write(&sem_path, b"250 32000 32 128\n").unwrap();
        let shmmni_path = dir.join("shmmni");
        std::fs::write(&shmmni_path, b"4096\n").unwrap();

        // Role-suffixed worker comms are the database: the recommendations
        // must fire.
        for name in ["postgres: writer", "mysqld: foo"] {
            let info = info_with(&[name]);
            let mut recs = Vec::new();
            eval_sem_at(&info, &mut recs, sem_path.to_str().unwrap());
            assert!(
                recs.iter().any(|r| r.param == "kernel.sem"),
                "{name} must gate the sem rule in"
            );
            let mut recs = Vec::new();
            eval_shmmni_at(&info, &mut recs, shmmni_path.to_str().unwrap());
            assert!(
                recs.iter().any(|r| r.param == "kernel.shmmni"),
                "{name} must gate the shmmni rule in"
            );
        }

        // Client tools are not the database (guard): nothing fires.
        for name in ["mysqldump", "pg_dump"] {
            let info = info_with(&[name]);
            let mut recs = Vec::new();
            eval_sem_at(&info, &mut recs, sem_path.to_str().unwrap());
            eval_shmmni_at(&info, &mut recs, shmmni_path.to_str().unwrap());
            assert!(recs.is_empty(), "{name} must not gate the IPC rules in");
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A process list holding exactly `names`, otherwise the standard fixture.
    fn info_with_processes(names: &[&str]) -> SystemInfo {
        let mut info = make_test_info();
        info.processes = names
            .iter()
            .map(|n| ProcessInfo {
                name: n.to_string(),
            })
            .collect();
        info
    }

    #[test]
    fn mariadb_daemon_gates_the_database_tuning_rules() {
        // MariaDB 10.4+ runs as mariadbd — the comm Debian and Ubuntu ship for
        // the same OLTP database `mysqld` names. The canonical predicate counts
        // it (73c675865), but these rules still hand-roll the list around
        // mysqld, so a MariaDB-only host got none of this advice.
        let mut info = info_with_processes(&["mariadbd"]);
        // Below the >=64GB branch of the swappiness and dirty rules, so only
        // the database gate can produce a recommendation here.
        info.memory_total_gb = 32;
        info.sysctl.swappiness = 60;
        info.sysctl.thp_enabled = "always".to_string();
        info.sysctl.dirty_ratio = 20;
        info.disks[0].disk_type = DiskType::NVMe;
        info.disks[0].read_ahead_kb = 512;

        let mut recs = Vec::new();
        eval_swappiness(&info, &WorkloadType::Mixed, &mut recs);
        assert!(
            recs.iter().any(|r| r.param == "vm.swappiness"),
            "mariadbd must gate the swappiness rule in"
        );

        let mut recs = Vec::new();
        eval_thp(&info, &mut recs);
        assert!(
            recs.iter()
                .any(|r| r.param == "transparent_hugepage/enabled"),
            "mariadbd must gate the THP rule in"
        );

        let mut recs = Vec::new();
        eval_dirty_ratio(&info, &WorkloadType::Mixed, &mut recs);
        assert!(
            recs.iter().any(|r| r.param == "vm.dirty_ratio"),
            "mariadbd must gate the dirty_ratio rule in"
        );

        let mut recs = Vec::new();
        eval_read_ahead_kb(&info, &mut recs);
        assert!(
            recs.iter()
                .any(|r| r.param == "block/nvme0n1/read_ahead_kb"),
            "mariadbd must gate the read-ahead rule in"
        );
    }

    #[test]
    fn mariadb_daemon_gates_the_shared_memory_rules() {
        // The SysV/shared-memory sizing gates are on the canonical predicate
        // since 5c878f7ea: kernel.shmmax and vm.nr_hugepages belong to the same
        // family as kernel.sem / kernel.shmmni, and the hand-rolled
        // `postgres || mysqld` list kept a MariaDB-only host out of both.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_mariadb_gates_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, content: &str| {
            let path = dir.join(name);
            std::fs::write(&path, content).unwrap();
            path
        };
        let shmmax = write("shmmax", "65536\n");
        let nr_hugepages = write("nr_hugepages", "0\n");
        let numa_balancing = write("numa_balancing", "1\n");
        let dirty_background_ratio = write("dirty_background_ratio", "10\n");

        let mut info = info_with_processes(&["mariadbd"]);
        info.memory_total_gb = 32;
        info.numa_nodes = 2;

        let mut recs = Vec::new();
        eval_shmmax_at(&info, &mut recs, shmmax.to_str().unwrap());
        assert!(
            recs.iter().any(|r| r.param == "kernel.shmmax"),
            "mariadbd must gate the shmmax rule in"
        );

        let mut recs = Vec::new();
        eval_nr_hugepages_at(&info, &mut recs, nr_hugepages.to_str().unwrap(), 2048);
        assert!(
            recs.iter().any(|r| r.param == "vm.nr_hugepages"),
            "mariadbd must gate the huge-page rule in"
        );

        let mut recs = Vec::new();
        eval_numa_balancing_at(&info, &mut recs, numa_balancing.to_str().unwrap());
        assert!(
            recs.iter().any(|r| r.param == "kernel.numa_balancing"),
            "mariadbd must gate the numa_balancing rule in"
        );

        let mut recs = Vec::new();
        eval_dirty_background_ratio_at(
            &info,
            &WorkloadType::Mixed,
            &mut recs,
            dirty_background_ratio.to_str().unwrap(),
        );
        assert!(
            recs.iter().any(|r| r.param == "vm.dirty_background_ratio"),
            "mariadbd must gate the dirty_background_ratio rule in"
        );

        // Boundary guard: the client tools are not the database.
        let mut client = info_with_processes(&["mariadb-dump"]);
        client.memory_total_gb = 32;
        client.numa_nodes = 2;
        let mut recs = Vec::new();
        eval_shmmax_at(&client, &mut recs, shmmax.to_str().unwrap());
        eval_nr_hugepages_at(&client, &mut recs, nr_hugepages.to_str().unwrap(), 2048);
        eval_numa_balancing_at(&client, &mut recs, numa_balancing.to_str().unwrap());
        eval_dirty_background_ratio_at(
            &client,
            &WorkloadType::Mixed,
            &mut recs,
            dirty_background_ratio.to_str().unwrap(),
        );
        assert!(recs.is_empty(), "mariadb-dump is not the database");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn overcommit_ratio_recommendation_requires_a_real_database() {
        // A strict-mode host running only client tooling must not be tuned:
        // under the old substring predicate mysqldump/mysqlrouter passed here.
        let mut recs = Vec::new();
        overcommit_ratio_recommendation(2, 50, 0, false, &mut recs);
        assert!(recs.is_empty(), "client tools are not a database workload");

        // The genuine case still fires with the documented value.
        let mut recs = Vec::new();
        overcommit_ratio_recommendation(2, 50, 0, true, &mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].param, "vm.overcommit_ratio");
        assert_eq!(recs[0].current_value, "50");
        assert_eq!(recs[0].recommended_value, "80");
        assert_eq!(recs[0].confidence, Confidence::Medium);

        // Non-strict overcommit modes stay silent even for a real database.
        for oc_mode in [0, 1] {
            let mut recs = Vec::new();
            overcommit_ratio_recommendation(oc_mode, 50, 0, true, &mut recs);
            assert!(recs.is_empty(), "mode {oc_mode} is not strict overcommit");
        }
        // A ratio already at or above the floor needs no change.
        let mut recs = Vec::new();
        overcommit_ratio_recommendation(2, 80, 0, true, &mut recs);
        assert!(recs.is_empty(), "80 already meets the floor");
    }

    #[test]
    fn overcommit_ratio_stays_quiet_while_kbytes_holds_the_limit() {
        // A host pinned to a fixed commit limit reads overcommit_ratio as 0:
        // the kernel's overcommit_ratio_handler zeroes sysctl_overcommit_kbytes
        // on every write and overcommit_kbytes_handler zeroes the ratio
        // (mm/util.c), so only one of the pair is ever configured — vm.rst's
        // overcommit_kbytes section says so outright ("Setting one disables
        // the other (which then appears as 0 when read)").
        //
        // That zero is the DISABLED counterpart, not a low ratio, and
        // vm_commit_limit() prefers the non-zero kbytes. Raising the ratio to
        // 80 there would not lift the limit the administrator set — it would
        // silently replace a fixed 8 GiB with 80% of RAM, the opposite of what
        // the rule's reason promises. The advice must not fire.
        let mut recs = Vec::new();
        overcommit_ratio_recommendation(2, 0, 8 * 1024 * 1024 * 1024, true, &mut recs);
        assert!(
            recs.is_empty(),
            "kbytes holds the commit limit, so the zero ratio is disabled, not low: {recs:?}"
        );

        // The same zero ratio with no kbytes twin is the "nothing configured"
        // case the rule exists for, and still fires.
        let mut recs = Vec::new();
        overcommit_ratio_recommendation(2, 0, 0, true, &mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].recommended_value, "80");
    }

    #[test]
    fn tcp_buffer_recommendations_keep_raised_defaults() {
        // A raised default (262144 / 131072) survives; only the max field
        // is lifted to the 10-GbE floor.
        assert_eq!(
            per_field_max(&[4096, 262144, 8388608], &[4096, 131072, 16777216]),
            "4096 262144 16777216"
        );
        assert_eq!(
            per_field_max(&[4096, 131072, 8388608], &[4096, 65536, 16777216]),
            "4096 131072 16777216"
        );
        // Untouched defaults below the floors are raised to them.
        assert_eq!(
            per_field_max(&[4096, 87380, 6291456], &[4096, 131072, 16777216]),
            "4096 131072 16777216"
        );
    }

    #[test]
    fn port_range_recommendation_never_lowers_the_low_endpoint() {
        // The reviewer's hardening case: "50000 60000" must NOT be
        // rewritten to "1024 65535" (the old fixed tuple lowered field 0).
        // Widening upward cannot reach the 30000-port threshold from
        // 50000, so the hardening choice wins: no recommendation at all,
        // rather than an unsatisfiable one that fires on every check.
        let mut recs = Vec::new();
        port_range_recommendation(50_000, 60_000, &mut recs);
        assert!(
            recs.is_empty(),
            "hardened range must not be lowered or nagged"
        );

        // A low-but-recoverable range widens upward only: the low endpoint
        // survives verbatim, the high endpoint extends to 65535.
        let mut recs = Vec::new();
        port_range_recommendation(32_768, 60_999, &mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].current_value, "32768 60999");
        assert_eq!(recs[0].recommended_value, "32768 65535");

        // The default narrow range gets the full-span widening.
        let mut recs = Vec::new();
        port_range_recommendation(1024, 20_000, &mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].recommended_value, "1024 65535");

        // An already-adequate range stays silent.
        let mut recs = Vec::new();
        port_range_recommendation(1024, 65_535, &mut recs);
        assert!(recs.is_empty());
    }

    fn rec(param: &str, conf: Confidence) -> Recommendation {
        Recommendation {
            param: param.to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: String::new(),
            confidence: conf,
            category: Category::Performance,
            writable: false,
        }
    }

    /// Synthetic `/sys/class/net`-style dir for `dir_has_bond` (std-only, no
    /// tempfile dependency).
    /// Drop guard over a synthetic `/sys/class/net`-style dir; removal
    /// survives a failing assert (cf. `SchedDir`).
    struct NetDir(std::path::PathBuf);

    impl NetDir {
        fn new(entries: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ktuner-has-bond-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            for name in entries {
                if name.ends_with('/') {
                    std::fs::create_dir(dir.join(name.trim_end_matches('/'))).unwrap();
                } else {
                    std::fs::write(dir.join(name), b"").unwrap();
                }
            }
            Self(dir)
        }
    }

    impl Drop for NetDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn dir_has_bond_ignores_bonding_masters_control_file() {
        // bonding module loaded, zero bonds: the kernel lists the regular
        // file `bonding_masters` alongside real interfaces.
        let dir = NetDir::new(&["bonding_masters", "eth0/", "eth1/", "lo/"]);
        assert!(!dir_has_bond(&dir.0));
    }

    #[test]
    fn dir_has_bond_detects_real_bond_interfaces() {
        let dir = NetDir::new(&["bonding_masters", "bond0/", "eth0/"]);
        assert!(dir_has_bond(&dir.0));

        let dir = NetDir::new(&["eth0/", "lo/"]);
        assert!(!dir_has_bond(&dir.0));
    }

    #[test]
    fn proc_bonding_dir_without_bonds_is_not_a_bond() {
        // `modprobe bonding` creates /proc/net/bonding even with no bond
        // (max_bonds=0). The directory alone must not skip ARP tuning; one
        // file per bond is the real signal.
        let dir = NetDir::new(&[]);
        assert!(!proc_bonding_has_bonds(&dir.0));

        let dir = NetDir::new(&["bond0"]);
        assert!(proc_bonding_has_bonds(&dir.0));
    }

    #[test]
    fn arp_tuning_guard_skips_bonded_hosts() {
        // A bonded host lists the bond master AND its slaves, so
        // network.len() >= 2 holds trivially — the guard must still skip.
        assert!(arp_tuning_skipped(2, true));
        assert!(arp_tuning_skipped(3, true));
        // Eligible only with 2+ interfaces and no bond.
        assert!(!arp_tuning_skipped(2, false));
        assert!(!arp_tuning_skipped(4, false));
        // Below 2 interfaces never qualifies, bond or not.
        assert!(arp_tuning_skipped(1, false));
        assert!(arp_tuning_skipped(0, true));
    }

    #[test]
    fn arp_tuning_guard_follows_synthetic_sysfs_bond() {
        // End-to-end through has_bond()'s injectable leg: a synthetic
        // /sys/class/net listing a real bond interface must flip the guard.
        let bonded = NetDir::new(&["bonding_masters", "bond0/", "eth0/", "eth1/"]);
        assert!(arp_tuning_skipped(3, dir_has_bond(&bonded.0)));

        // Same directory without the bond stays eligible at 2+ interfaces.
        let plain = NetDir::new(&["bonding_masters", "eth0/", "eth1/"]);
        assert!(!arp_tuning_skipped(2, dir_has_bond(&plain.0)));
    }

    #[test]
    fn test_dedupe_keeps_one_per_param() {
        let input = vec![
            rec("net.core.somaxconn", Confidence::Medium),
            rec("vm.swappiness", Confidence::Medium),
            rec("net.core.somaxconn", Confidence::Medium),
        ];
        let out = dedupe_recommendations(input);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.iter()
                .filter(|r| r.param == "net.core.somaxconn")
                .count(),
            1
        );
    }

    #[test]
    fn test_dedupe_prefers_high_confidence() {
        let input = vec![
            rec("kernel.randomize_va_space", Confidence::Medium),
            rec("kernel.randomize_va_space", Confidence::High),
        ];
        let out = dedupe_recommendations(input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].confidence, Confidence::High);
    }

    /// Synthetic `/lib/modules/<release>/kernel/net/sched`-style directory
    /// (std-only, no tempfile dependency). Removed when the guard is dropped.
    struct SchedDir(std::path::PathBuf);

    impl SchedDir {
        fn new(files: &[&str]) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "ktuner-qdisc-module-test-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            for name in files {
                std::fs::write(dir.join(name), b"").unwrap();
            }
            Self(dir)
        }
    }

    impl Drop for SchedDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn qdisc_probe_accepts_plain_and_compressed_modules() {
        // Plain uncompressed module, as shipped by older distributions.
        let plain = SchedDir::new(&["sch_fq.ko"]);
        assert!(qdisc_module_in(&plain.0, "sch_fq"));

        // One directory per compression format: kmod loads any of them, so
        // the probe must accept each suffix as the only evidence present.
        for suffix in [".ko.zst", ".ko.xz", ".ko.gz"] {
            let dir = SchedDir::new(&[format!("sch_fq{suffix}").as_str()]);
            assert!(
                qdisc_module_in(&dir.0, "sch_fq"),
                "compressed module {suffix} must be detected as available"
            );
        }
    }

    #[test]
    fn qdisc_probe_rejects_absent_and_unrelated_modules() {
        // Empty directory: nothing is available.
        let empty = SchedDir::new(&[]);
        assert!(!qdisc_module_in(&empty.0, "sch_fq"));

        // A different qdisc's module does not make sch_fq available.
        let other = SchedDir::new(&["sch_fq_codel.ko.zst"]);
        assert!(!qdisc_module_in(&other.0, "sch_fq"));
    }

    #[test]
    fn test_dedupe_preserves_order() {
        let input = vec![
            rec("a.b", Confidence::Medium),
            rec("c.d", Confidence::Medium),
            rec("a.b", Confidence::Medium),
            rec("e.f", Confidence::Medium),
        ];
        let out = dedupe_recommendations(input);
        let names: Vec<&str> = out.iter().map(|r| r.param.as_str()).collect();
        assert_eq!(names, vec!["a.b", "c.d", "e.f"]);
    }

    fn make_test_info() -> SystemInfo {
        SystemInfo {
            kernel_version: "5.4.0".to_string(),
            os_distro: "Test Linux".to_string(),
            cpu_model: "Test CPU".to_string(),
            cpu_cores: 8,
            numa_nodes: 1,
            memory_total_gb: 64,
            disks: vec![DiskInfo {
                name: "nvme0n1".to_string(),
                disk_type: DiskType::NVMe,
                scheduler: "mq-deadline".to_string(),
                available_schedulers: vec!["none".to_string(), "mq-deadline".to_string()],
                nr_requests: 256,
                read_ahead_kb: 128,
                rq_affinity: 1,
            }],
            network: vec![],
            sysctl: SysctlValues {
                swappiness: 60,
                dirty_ratio: 20,
                dirty_background_ratio: 10,
                somaxconn: 128,
                thp_enabled: "always".to_string(),
            },
            processes: vec![ProcessInfo {
                name: "postgres".to_string(),
            }],
        }
    }

    #[test]
    fn test_nvme_scheduler_recommendation() {
        let info = make_test_info();
        let recs = evaluate(&info).unwrap().recommendations;
        let sched_rec = recs.iter().find(|r| r.param.contains("scheduler"));
        assert!(sched_rec.is_some());
        assert_eq!(sched_rec.unwrap().recommended_value, "none");
        assert_eq!(sched_rec.unwrap().confidence, Confidence::High);
        assert_eq!(sched_rec.unwrap().category, Category::Performance);
    }

    #[test]
    fn test_swappiness_with_database() {
        let info = make_test_info();
        let recs = evaluate(&info).unwrap().recommendations;
        let swap_rec = recs.iter().find(|r| r.param == "vm.swappiness");
        assert!(swap_rec.is_some());
        assert_eq!(swap_rec.unwrap().recommended_value, "1");
    }

    #[test]
    fn test_thp_with_postgres() {
        let info = make_test_info();
        let recs = evaluate(&info).unwrap().recommendations;
        let thp_rec = recs.iter().find(|r| r.param.contains("hugepage"));
        assert!(thp_rec.is_some());
        assert_eq!(thp_rec.unwrap().recommended_value, "madvise");
    }

    #[test]
    fn test_thp_with_mongod_and_etcd() {
        // mongod is a database in every sibling list (swappiness, dirty_ratio,
        // classify) and MongoDB's production notes ask for THP off outright;
        // etcd is the coordination store of the same cache family. Both must
        // get the always->madvise recommendation; a streaming workload must not.
        for name in ["mongod", "etcd"] {
            let mut info = make_test_info();
            info.processes = vec![ProcessInfo {
                name: name.to_string(),
            }];
            let recs = evaluate(&info).unwrap().recommendations;
            let thp_rec = recs
                .iter()
                .find(|r| r.param.contains("hugepage"))
                .unwrap_or_else(|| panic!("{name} must get the THP recommendation"));
            assert_eq!(thp_rec.recommended_value, "madvise");
        }
        let mut info = make_test_info();
        info.processes = vec![ProcessInfo {
            name: "kafka".to_string(),
        }];
        let recs = evaluate(&info).unwrap().recommendations;
        assert!(
            recs.iter().all(|r| !r.param.contains("hugepage")),
            "a streaming workload is not latency-sensitive"
        );
    }

    #[test]
    fn test_no_false_positive_when_optimal() {
        let mut info = make_test_info();
        info.disks[0].scheduler = "none".to_string();
        info.disks[0].nr_requests = 1024;
        info.sysctl.swappiness = 10;
        info.sysctl.thp_enabled = "madvise".to_string();
        info.sysctl.dirty_ratio = 10;
        info.sysctl.dirty_background_ratio = 5;
        info.sysctl.somaxconn = 65535;
        info.processes = vec![];
        info.network = vec![];

        let recs = evaluate(&info).unwrap().recommendations;
        // Filter to rules fully controlled by SystemInfo (no live /proc reads)
        let live_params = [
            "tcp_slow_start",
            "default_qdisc",
            "tcp_max_syn_backlog",
            "ip_local_port_range",
            "watermark_scale_factor",
            "max_map_count",
            "sched_autogroup",
            "netdev_max_backlog",
            "tcp_rmem",
            "tcp_wmem",
            "rmem_max",
            "wmem_max",
            "tcp_tw_reuse",
            "tcp_fin_timeout",
            "tcp_keepalive_time",
            "file-max",
            "pid_max",
            "tcp_max_tw_buckets",
            "tcp_mtu_probing",
            "sched_migration_cost",
            "tcp_no_metrics_save",
            "overcommit_memory",
            "tcp_keepalive_intvl",
            "tcp_keepalive_probes",
            "panic",
            "panic_on_oom",
            "tcp_congestion_control",
            "nf_conntrack",
            "dirty_expire_centisecs",
            "dirty_writeback_centisecs",
            "tcp_timestamps",
            "tcp_window_scaling",
            "tcp_ecn",
            "ip_forward",
            "tcp_retries2",
            "tcp_abort_on_overflow",
            "log_martians",
            "shmmax",
            "tcp_max_orphans",
            "threads-max",
            "nr_hugepages",
            "tcp_syn_retries",
            "tcp_synack_retries",
            "optmem_max",
            "oom_kill_allocating_task",
            "inotify",
            "aio-max-nr",
            "dirty_background_ratio",
            "sched_min_granularity",
            "icmp_echo_ignore_broadcasts",
            "accept_source_route",
            "busy_read",
            "gc_thresh3",
            "nr_open",
            "arp_announce",
            "arp_ignore",
            "nmi_watchdog",
            "stat_interval",
            "hung_task_timeout",
            "tcp_rfc1337",
            "secure_redirects",
            "mmap_min_addr",
            "netdev_budget_usecs",
            "dirty_bytes",
            "sched_child_runs_first",
            "default.accept_redirects",
            "default.accept_source_route",
            "sched_latency_ns",
            "challenge_ack_limit",
            "conf.all.rp_filter",
            "page-cluster",
            "rmem_default",
            "wmem_default",
            "sched_nr_migrate",
            "tcp_notsent_lowat",
            "max_dgram_qlen",
            "rps_sock_flow_entries",
            "tcp_dsack",
            "kexec_load_disabled",
            "ip_no_pmtu_disc",
            "sched_wakeup_granularity",
            "extfrag_threshold",
            "tcp_tw_recycle",
            "tcp_orphan_retries",
            "tcp_early_retrans",
            "arp_filter",
            "cfs_bandwidth_slice",
            "suid_dumpable",
            "icmp_ignore_bogus",
            "default.log_martians",
            "laptop_mode",
            "tcp_adv_win_scale",
            "sched_tunable_scaling",
            "panic_on_oops",
            "oom_dump_tasks",
            "tcp_moderate_rcvbuf",
            "flow_limit_table_len",
            "tcp_l3mdev_accept",
            "panic_on_warn",
            "dirty_background_bytes",
            "hardlockup_panic",
            "softlockup_panic",
            "sched_rt_runtime",
            "tcp_thin_linear",
            "arp_notify",
            "default.arp_announce",
            "default.arp_ignore",
            "default.send_redirects",
            "gc_thresh1",
            "gc_thresh2",
            "tcp_retries1",
            "tcp_limit_output_bytes",
            "dev_weight",
            "printk",
            "watchdog_thresh",
            "admin_reserve_kbytes",
            "msgmax",
            "msgmnb",
            "protected_fifos",
            "modules_disabled",
            "user_reserve_kbytes",
            "shmmni",
            "kernel.sem",
            "gc_stale_time",
            "shm_rmid_forced",
            "tcp_fack",
            "tcp_reordering",
            "sched_energy_aware",
            "percpu_pagelist_high_fraction",
            "accept_ra",
            "compaction_proactiveness",
            "min_slab_ratio",
            "tcp_autocorking",
            "tcp_workaround_signed",
            "max_user_instances",
            "keys.maxkeys",
            "tcp_available_ulp",
            "numa_stat",
            "sched_cfs_bandwidth_slice_us",
            "tcp_base_mss",
            "tcp_min_tso_segs",
            "neigh.default.gc_interval",
            "neigh.default.gc_stale_time",
            "tcp_fastopen_blackhole",
            "tcp_fastopen",
            "rtsig-max",
            "keys.maxbytes",
            "pipe-max-size",
            "shmall",
            "tcp_app_win",
            "ip_default_ttl",
            "tcp_frto",
            "icmp_ratelimit",
            "igmp_max_memberships",
            "tcp_recovery",
            "tcp_comp_sack_delay",
            "skb_defer_max",
            "proxy_delay",
            "tcp_pacing_ca_ratio",
            "tcp_pacing_ss_ratio",
            "tcp_comp_sack_nr",
            "tcp_thin_dupack",
            "tcp_invalid_ratelimit",
            "tcp_init_cwnd",
            "tcp_tso_win_divisor",
            "sched_schedstats",
            "max_queued_events",
            "tcp_max_reordering",
            "tcp_retrans_collapse",
            "protected_regular",
            "bpf_jit_enable",
            "bpf_jit_harden",
            "promote_secondaries",
            "unres_qlen_bytes",
            "ip_nonlocal_bind",
            "conntrack_tcp_timeout_established",
            "softlockup_all_cpu_backtrace",
            "compact_unevictable",
            "perf_cpu_time_max_percent",
            "hung_task_warnings",
            "overcommit_ratio",
        ];
        let controllable_perf_recs: Vec<_> = recs
            .iter()
            .filter(|r| r.category == Category::Performance)
            .filter(|r| !live_params.iter().any(|p| r.param.contains(p)))
            .collect();
        assert!(
            controllable_perf_recs.is_empty(),
            "Should not recommend perf changes when optimal, got: {controllable_perf_recs:?}"
        );
    }

    #[test]
    fn test_nvme_nr_requests() {
        let mut info = make_test_info();
        info.disks[0].nr_requests = 128;
        // Only an elevator that stays in place can take a deeper queue.
        info.disks[0].available_schedulers = vec!["mq-deadline".to_string()];
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("nr_requests"));
        assert!(
            rec.is_some(),
            "Should recommend increasing nr_requests for NVMe"
        );
        assert_eq!(rec.unwrap().recommended_value, "1024");
        assert_eq!(rec.unwrap().confidence, Confidence::High);
    }

    #[test]
    fn test_nvme_nr_requests_skipped_under_none() {
        // Current `none`, or `none` recommended by the scheduler rule: either
        // way the queue ends at the hardware depth, so 1024 would hit EINVAL
        // or be reset by the elevator switch.
        for scheduler in ["none", "mq-deadline"] {
            let mut info = make_test_info();
            info.disks[0].scheduler = scheduler.to_string();
            info.disks[0].nr_requests = 127;
            let recs = evaluate(&info).unwrap().recommendations;
            assert!(
                !recs.iter().any(|r| r.param.contains("nr_requests")),
                "nr_requests cannot exceed the hardware depth under none (current {scheduler})"
            );
        }
    }

    #[test]
    fn test_nvme_nr_requests_ok_when_high() {
        let mut info = make_test_info();
        info.disks[0].nr_requests = 1024;
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("nr_requests"));
        assert!(
            rec.is_none(),
            "Should not recommend nr_requests when already high"
        );
    }

    #[test]
    fn test_rq_affinity_only_on_numa() {
        let mut info = make_test_info();
        info.numa_nodes = 1;
        info.disks[0].rq_affinity = 1;
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("rq_affinity"));
        assert!(
            rec.is_none(),
            "Should not recommend rq_affinity on single NUMA"
        );

        info.numa_nodes = 2;
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("rq_affinity"));
        assert!(
            rec.is_some(),
            "Should recommend rq_affinity=2 on multi-NUMA NVMe"
        );
        assert_eq!(rec.unwrap().recommended_value, "2");
    }

    #[test]
    fn test_dirty_ratio_with_database() {
        let mut info = make_test_info();
        info.memory_total_gb = 32; // <64GB uses the ratio form (>=64GB uses bytes)
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param == "vm.dirty_ratio");
        assert!(
            rec.is_some(),
            "Should recommend lower dirty_ratio with postgres"
        );
        assert_eq!(rec.unwrap().recommended_value, "5");

        let bg_rec = recs.iter().find(|r| r.param == "vm.dirty_background_ratio");
        assert!(bg_rec.is_some());
        assert_eq!(bg_rec.unwrap().recommended_value, "3");
    }

    #[test]
    fn test_ssd_scheduler() {
        let mut info = make_test_info();
        info.disks = vec![DiskInfo {
            name: "sda".to_string(),
            disk_type: DiskType::SSD,
            scheduler: "cfq".to_string(),
            available_schedulers: vec!["noop".to_string(), "cfq".to_string()],
            nr_requests: 256,
            read_ahead_kb: 128,
            rq_affinity: 1,
        }];
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("scheduler"));
        assert!(rec.is_some(), "Should recommend changing cfq on SSD");
        assert_eq!(rec.unwrap().recommended_value, "noop");
    }

    #[test]
    fn test_hdd_read_ahead_with_streaming() {
        let mut info = make_test_info();
        info.disks = vec![DiskInfo {
            name: "sdb".to_string(),
            disk_type: DiskType::HDD,
            scheduler: "cfq".to_string(),
            available_schedulers: vec!["cfq".to_string()],
            nr_requests: 128,
            read_ahead_kb: 128,
            rq_affinity: 1,
        }];
        info.processes = vec![ProcessInfo {
            name: "kafka".to_string(),
        }];
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("read_ahead_kb"));
        assert!(
            rec.is_some(),
            "Should recommend read_ahead_kb for HDD with kafka"
        );
        assert_eq!(rec.unwrap().recommended_value, "2048");
    }

    #[test]
    fn test_hdd_no_read_ahead_without_streaming() {
        let mut info = make_test_info();
        info.disks = vec![DiskInfo {
            name: "sdb".to_string(),
            disk_type: DiskType::HDD,
            scheduler: "cfq".to_string(),
            available_schedulers: vec!["cfq".to_string()],
            nr_requests: 128,
            read_ahead_kb: 128,
            rq_affinity: 1,
        }];
        info.processes = vec![];
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param.contains("read_ahead_kb"));
        assert!(
            rec.is_none(),
            "Should not recommend read_ahead_kb without streaming process"
        );
    }

    #[test]
    fn test_somaxconn_not_triggered_without_listen() {
        let mut info = make_test_info();
        info.sysctl.somaxconn = 128;
        info.processes = vec![];
        // has_listen_sockets() reads live /proc, so on this machine it may or may not trigger
        // We just verify the rule exists and has the right recommended value when triggered
        let recs = evaluate(&info).unwrap().recommendations;
        if let Some(rec) = recs.iter().find(|r| r.param == "net.core.somaxconn") {
            assert_eq!(rec.recommended_value, "65535");
        }
    }

    #[test]
    fn test_pid_max_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let recs = evaluate(&info).unwrap().recommendations;
        // pid_max reads /proc/sys/kernel/pid_max live, verify if triggered
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.pid_max") {
            assert_eq!(rec.recommended_value, "4194304");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_pid_max_not_triggered_small_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs.iter().find(|r| r.param == "kernel.pid_max");
        assert!(
            rec.is_none(),
            "Should not recommend pid_max for small machines"
        );
    }

    #[test]
    fn test_tcp_keepalive_values() {
        // These rules read live /proc state, just verify recommended values if triggered
        let info = make_test_info();
        let recs = evaluate(&info).unwrap().recommendations;
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_keepalive_time")
        {
            assert_eq!(rec.recommended_value, "600");
        }
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_fin_timeout") {
            assert_eq!(rec.recommended_value, "15");
        }
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_tw_reuse") {
            assert_eq!(rec.recommended_value, "1");
        }
    }

    #[test]
    fn test_total_rules_count_dynamic() {
        let info = make_test_info();
        let result = evaluate(&info).unwrap();
        assert!(
            result.total_checked >= 30,
            "Should check at least 30 rules, got {}",
            result.total_checked
        );
    }

    #[test]
    fn test_workload_io_latency_swappiness() {
        let mut info = make_test_info();
        info.sysctl.swappiness = 10;
        info.processes = vec![ProcessInfo {
            name: "postgres".to_string(),
        }];
        let recs = evaluate_with_workload(&info, &WorkloadType::IoLatency)
            .unwrap()
            .recommendations;
        let rec = recs.iter().find(|r| r.param == "vm.swappiness");
        assert!(
            rec.is_some(),
            "IoLatency workload should recommend swappiness=1 even when current is 10"
        );
        assert_eq!(rec.unwrap().recommended_value, "1");
    }

    #[test]
    fn test_dirty_background_bytes_stays_below_dirty_limit() {
        const MIB: u64 = 1024 * 1024;
        // dirty_bytes unset: ktuner recommends DIRTY_BYTES_TARGET (256 MiB), so
        // an equal background value would be halved by the kernel anyway.
        assert_eq!(dirty_background_bytes_target(0), 128 * MIB);
        assert!(dirty_background_bytes_target(0) < DIRTY_BYTES_TARGET);
        // An administrator's dirty_bytes is kept and bounds the background value.
        assert_eq!(dirty_background_bytes_target(128 * MIB), 64 * MIB);
        assert_eq!(dirty_background_bytes_target(4096 * MIB), 256 * MIB);
        for dirty in [2 * 4096, MIB, 300 * MIB, 512 * MIB, u64::MAX] {
            assert!(dirty_background_bytes_target(dirty) < dirty);
        }
    }

    #[test]
    fn test_dirty_ratio_bytes_mutually_exclusive_large_ram() {
        // dirty_ratio ⊥ dirty_bytes in the kernel. A >=64GB host must never be
        // told to set both for the same dimension, and must use the bytes form.
        let mut info = make_test_info();
        info.memory_total_gb = 128;
        info.sysctl.dirty_ratio = 20;
        info.sysctl.dirty_background_ratio = 10;
        info.processes = vec![ProcessInfo {
            name: "postgres".to_string(),
        }];
        let recs = evaluate_with_workload(&info, &WorkloadType::IoLatency)
            .unwrap()
            .recommendations;
        let has_ratio = recs.iter().any(|r| r.param == "vm.dirty_ratio");
        let has_bytes = recs.iter().any(|r| r.param == "vm.dirty_bytes");
        assert!(
            !(has_ratio && has_bytes),
            "must not recommend both dirty_ratio and dirty_bytes"
        );
        let has_bg_ratio = recs.iter().any(|r| r.param == "vm.dirty_background_ratio");
        let has_bg_bytes = recs.iter().any(|r| r.param == "vm.dirty_background_bytes");
        assert!(
            !(has_bg_ratio && has_bg_bytes),
            "must not recommend both bg ratio and bg bytes"
        );
        assert!(
            !has_ratio,
            "large-RAM host should use dirty_bytes, not dirty_ratio"
        );
    }

    #[test]
    fn test_current_values_stay_kernel_writable() {
        // Unconditional regression for the rollback ledger: current_value
        // is written back to the kernel verbatim by `ktuner rollback`, so
        // every annotated form ("0 (ratio=..%)" / "432000 (5天)" /
        // "0 (默认8)") made its param permanently unrestorable with EINVAL.
        // The value-driven rule cores are invoked directly so the branches
        // fire on any host, independent of the live /proc contents.

        // vm.dirty_background_bytes: bytes == 0 && ratio > 5 fires. The
        // dirty limit input exercises #4454's target computation: with
        // vm.dirty_bytes at 0 (the common ratio-form host), the target is
        // clamped to half of the 256MiB dirty limit.
        let mut recs = Vec::new();
        dirty_background_bytes_recommendation(128, 0, 10, 0, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "vm.dirty_background_bytes")
            .expect("forced branch must produce the recommendation");
        assert_eq!(
            rec.current_value.parse::<u64>().ok(),
            Some(0),
            "current_value must be the bare kernel-writable number"
        );
        assert_eq!(
            rec.recommended_value,
            dirty_background_bytes_target(0).to_string(),
            "the extracted core must keep #4454's below-the-limit target"
        );

        // conntrack timeout: current > 86400 fires.
        let mut recs = Vec::new();
        conntrack_timeout_recommendation(432_000, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "net.netfilter.nf_conntrack_tcp_timeout_established")
            .expect("forced branch must produce the recommendation");
        assert_eq!(
            rec.current_value.parse::<u64>().ok(),
            Some(432_000),
            "current_value must be the bare kernel-writable number"
        );

        // tcp_orphan_retries: both the 0 (kernel default) and > 3 arms.
        for current in [0_u64, 8] {
            let mut recs = Vec::new();
            orphan_retries_recommendation(current, &mut recs);
            let rec = recs
                .iter()
                .find(|r| r.param == "net.ipv4.tcp_orphan_retries")
                .unwrap_or_else(|| panic!("current={current} must produce the recommendation"));
            assert_eq!(
                rec.current_value.parse::<u64>().ok(),
                Some(current),
                "current={current} must stay a bare number (the old code annotated 0 as \"0 (默认8)\")"
            );
        }
    }

    #[test]
    fn test_value_cores_do_not_recommend_when_optimal() {
        // Boundary: at or below the thresholds nothing is recommended.
        let mut recs = Vec::new();
        dirty_background_bytes_recommendation(128, 268_435_456, 10, 0, &mut recs);
        assert!(recs.is_empty(), "bytes already set: no rec");

        conntrack_timeout_recommendation(86_400, &mut recs);
        assert!(recs.is_empty(), "timeout already 1 day: no rec");

        orphan_retries_recommendation(2, &mut recs);
        assert!(recs.is_empty(), "retries already 2: no rec");
    }

    #[test]
    fn test_workload_io_latency_dirty_ratio() {
        let mut info = make_test_info();
        info.memory_total_gb = 32; // <64GB uses the ratio form
        info.sysctl.dirty_ratio = 10;
        info.sysctl.dirty_background_ratio = 5;
        info.processes = vec![ProcessInfo {
            name: "postgres".to_string(),
        }];
        let recs = evaluate_with_workload(&info, &WorkloadType::IoLatency)
            .unwrap()
            .recommendations;
        let rec = recs.iter().find(|r| r.param == "vm.dirty_ratio");
        assert!(rec.is_some(), "IoLatency should recommend dirty_ratio=5");
        assert_eq!(rec.unwrap().recommended_value, "5");
        let bg = recs.iter().find(|r| r.param == "vm.dirty_background_ratio");
        assert!(bg.is_some());
        assert_eq!(bg.unwrap().recommended_value, "3");
    }

    #[test]
    fn test_score_empty() {
        let result = EvalResult {
            recommendations: vec![],
            total_checked: 40,
        };
        assert_eq!(result.score(), 100);
    }

    #[test]
    fn test_score_weighted() {
        let high_rec = Recommendation {
            param: "test".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "test".to_string(),
            confidence: Confidence::High,
            ..Default::default()
        };
        let medium_rec = Recommendation {
            confidence: Confidence::Medium,
            ..high_rec.clone()
        };

        let result = EvalResult {
            recommendations: vec![high_rec.clone(), medium_rec.clone()],
            total_checked: 40,
        };
        assert_eq!(result.score(), 95); // 100 - 3 - 2 = 95

        let result = EvalResult {
            recommendations: vec![high_rec; 20],
            total_checked: 40,
        };
        assert_eq!(result.score(), 40); // 100 - 60 = 40

        let result = EvalResult {
            recommendations: vec![medium_rec; 40],
            total_checked: 40,
        };
        assert_eq!(result.score(), 30); // 100 - 80 capped at 30
    }

    #[test]
    fn score_after_applying_keeps_the_floor() {
        // 20 high-confidence + 20 medium findings = 100 points of penalty, so
        // `score` sits on its 30-point floor.
        let recs: Vec<Recommendation> = (0..20)
            .map(|i| rec(&format!("test.high{i}"), Confidence::High))
            .chain((0..20).map(|i| rec(&format!("test.medium{i}"), Confidence::Medium)))
            .collect();
        let eval = EvalResult {
            recommendations: recs.clone(),
            total_checked: 40,
        };
        assert_eq!(eval.score(), 30);

        // Applying five findings leaves 85 points of penalty, still inside the
        // floor, so the score after tuning is 30 — not `score() + 15 = 45`,
        // which counts the floor as a gain.
        assert_eq!(eval.score_after_applying(&recs[..5]), 30);

        // On a host that is not floored the prediction moves by the applied
        // weight as usual: 10 findings cost 30 points, five of them 15.
        let light = EvalResult {
            recommendations: recs[..10].to_vec(),
            total_checked: 10,
        };
        assert_eq!(light.score(), 70);
        assert_eq!(light.score_after_applying(&recs[..5]), 85);
    }

    #[test]
    fn test_workload_mixed_no_aggressive_swappiness() {
        let mut info = make_test_info();
        info.sysctl.swappiness = 10;
        info.processes = vec![];
        let recs = evaluate_with_workload(&info, &WorkloadType::Mixed)
            .unwrap()
            .recommendations;
        let rec = recs.iter().find(|r| r.param == "vm.swappiness");
        assert!(
            rec.is_none(),
            "Mixed workload with 64GB RAM and swappiness=10 should not trigger"
        );
    }

    #[test]
    fn test_sched_migration_cost_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 64;
        let recs = evaluate(&info).unwrap().recommendations;
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "kernel.sched_migration_cost_ns")
        {
            assert_eq!(rec.recommended_value, "5000000");
        }
    }

    #[test]
    fn test_sched_migration_cost_not_triggered_small_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let recs = evaluate(&info).unwrap().recommendations;
        let rec = recs
            .iter()
            .find(|r| r.param == "kernel.sched_migration_cost_ns");
        assert!(
            rec.is_none(),
            "Should not recommend sched_migration_cost for small machines"
        );
    }

    #[test]
    fn test_sched_migration_cost_minus_one_reads_signed() {
        // -1 is task_hot()'s "every task stays cache-hot" sentinel: migration
        // is effectively disabled (kernel/sched/fair.c special-cases it next
        // to 0, the "always migrate" value). The unsigned reader parsed
        // "-1" to Err and fell back to 0 — the *opposite* migration policy —
        // so the report diagnosed a deliberately pinned host as aggressively
        // migrating, and current_value lied about the live kernel setting.
        let path = std::env::temp_dir().join(format!(
            "ktuner_sched_mig_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"-1\n").unwrap();
        let mut info = make_test_info();
        info.cpu_cores = 32;
        let mut recs = Vec::new();
        let checked = eval_sched_migration_cost_at(&info, &mut recs, path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert_eq!(checked, 1);
        assert_eq!(
            recs.len(),
            1,
            "-1 is far below 5000000, so it must be reported"
        );
        assert_eq!(recs[0].param, "kernel.sched_migration_cost_ns");
        assert_eq!(
            recs[0].current_value, "-1",
            "current must be faithful: 0 is the opposite migration policy"
        );
        assert_eq!(recs[0].recommended_value, "5000000");
    }

    #[test]
    fn test_sched_migration_cost_boundaries() {
        // 5000000 (the recommendation itself) and anything above is already
        // tuned; every lower value — including the -1 "never migrate"
        // sentinel, 0 ("always migrate") and the 500000 default — must be
        // reported with a faithful signed echo.
        let mut info = make_test_info();
        info.cpu_cores = 32;
        for (value, expects_rec) in [
            (-1, true),
            (0, true),
            (500000, true),
            (4999999, true),
            (5000000, false),
            (6000000, false),
        ] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_sched_mig_bound_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_sched_migration_cost_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only >= 5000000 is already tuned"
            );
            if expects_rec {
                assert_eq!(
                    recs[0].current_value,
                    value.to_string(),
                    "current_value must echo the signed value verbatim"
                );
                assert_eq!(recs[0].recommended_value, "5000000");
            }
        }
    }

    #[test]
    fn test_sched_migration_cost_absent_counts_as_checked() {
        // A path that never exists exercises the absent branch: 1 checked,
        // 0 recommendations, no filesystem dependency in CI.
        let mut info = make_test_info();
        info.cpu_cores = 32;
        let mut recs = Vec::new();
        let checked =
            eval_sched_migration_cost_at(&info, &mut recs, "/proc/sys/kernel/ktuner_absent_sched");
        assert_eq!(checked, 1);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_tcp_mtu_probing() {
        let info = make_test_info();
        let recs = evaluate(&info).unwrap().recommendations;
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_mtu_probing") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_tcp_retries2() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_retries2(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_retries2") {
            assert_eq!(rec.recommended_value, "8");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_file_max() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_file_max(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "fs.file-max") {
            assert_eq!(rec.recommended_value, "2000000");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_conntrack_max() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_nf_conntrack_max(&info, &mut recs);
        // Just verify it doesn't panic, result depends on system state
        let _ = recs;
    }

    #[test]
    fn test_panic_on_warn_is_a_security_recommendation() {
        // Every sibling panic knob (panic, panic_on_oops, panic_on_oom,
        // hardlockup_panic) is Category::Security, and `--category security`
        // selects on that field, so a Performance label hid the "kernel WARN
        // should not reboot the host" advice from the only filter that looks for
        // availability policy. Feed the value directly: a host that boots with
        // panic_on_warn=0 would otherwise make this assertion vacuous.
        let mut recs = Vec::new();
        recommend_panic_on_warn(1, &mut recs);
        let rec = recs
            .first()
            .expect("a non-zero panic_on_warn must be recommended");
        assert_eq!(rec.param, "kernel.panic_on_warn");
        assert_eq!(rec.recommended_value, "0");
        assert_eq!(
            rec.category,
            Category::Security,
            "kernel.panic_on_warn must be a security recommendation like its siblings"
        );

        // The same filter the CLI applies: the label decides visibility, so a
        // Performance-labelled recommendation would be dropped here.
        assert_eq!(
            crate::category::filter_by_category(recs.clone(), "security").len(),
            1,
            "the recommendation must survive --category security"
        );
        let mut wrong = recs;
        wrong[0].category = Category::Performance;
        assert!(
            crate::category::filter_by_category(wrong, "security").is_empty(),
            "a Performance-labelled panic_on_warn is invisible to --category security"
        );
    }

    #[test]
    fn test_log_martians() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_log_martians(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.log_martians")
        {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_shmmax_only_with_database() {
        let mut info = make_test_info();
        info.processes = vec![];
        let mut recs = Vec::new();
        eval_shmmax(&info, &mut recs);
        assert!(
            recs.is_empty(),
            "Should not recommend shmmax without database process"
        );

        info.processes = vec![ProcessInfo {
            name: "postgres".to_string(),
        }];
        let mut recs = Vec::new();
        eval_shmmax(&info, &mut recs);
        // Depends on current system shmmax value
        let _ = recs;
    }

    #[test]
    fn test_tcp_timestamps_must_be_on() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_timestamps(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_timestamps") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_tcp_window_scaling_must_be_on() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_window_scaling(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_window_scaling")
        {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_tcp_sack_must_be_on() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_sack(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_sack") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_overcommit_only_with_redis() {
        let mut info = make_test_info();
        info.processes = vec![];
        let mut recs = Vec::new();
        eval_overcommit_memory(&info, &mut recs);
        assert!(
            recs.is_empty(),
            "Should not recommend overcommit without redis"
        );

        info.processes = vec![ProcessInfo {
            name: "redis-server".to_string(),
        }];
        let mut recs = Vec::new();
        eval_overcommit_memory(&info, &mut recs);
        if let Some(rec) = recs.first() {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_thp_not_triggered_without_latency_process() {
        let mut info = make_test_info();
        info.processes = vec![];
        info.sysctl.thp_enabled = "always".to_string();
        let mut recs = Vec::new();
        eval_thp(&info, &mut recs);
        assert!(
            recs.is_empty(),
            "THP should not trigger without latency-sensitive processes"
        );
    }

    #[test]
    fn test_nr_hugepages_scale_with_default_page_size() {
        // A quarter of 742 GB: unchanged for 2 MiB pages.
        assert_eq!(quarter_memory_hugepages(742, 2048), 94976);
        // 512 MiB (64K-page aarch64) and 1 GiB pages reserve the same quarter
        // instead of 256x / 512x that much memory.
        assert_eq!(quarter_memory_hugepages(742, 512 * 1024), 371);
        assert_eq!(quarter_memory_hugepages(64, 1024 * 1024), 16);
        for kb in [2048, 512 * 1024, 1024 * 1024] {
            let reserved_kb = quarter_memory_hugepages(256, kb) * kb;
            assert!(reserved_kb <= 256 * 1024 * 1024 / 4);
        }
        // Unknown size: no recommendation rather than a 2 MiB guess.
        assert_eq!(quarter_memory_hugepages(742, 0), 0);
    }

    #[test]
    fn test_nr_hugepages_only_with_db() {
        let mut info = make_test_info();
        info.processes = vec![];
        info.memory_total_gb = 64;
        let mut recs = Vec::new();
        eval_nr_hugepages(&info, &mut recs);
        assert!(
            recs.is_empty(),
            "Should not recommend hugepages without db process"
        );
    }

    #[test]
    fn test_tcp_syn_retries() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_syn_retries(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_syn_retries") {
            assert_eq!(rec.recommended_value, "3");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_tcp_synack_retries() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_synack_retries(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_synack_retries")
        {
            assert_eq!(rec.recommended_value, "3");
        }
    }

    #[test]
    fn syn_backlog_is_only_worth_sizing_where_the_kernel_reads_it() {
        // tcp_input.c reads sysctl_max_syn_backlog in exactly one place: the
        // last-quarter reservation inside tcp_conn_request(), guarded by
        // `!syncookies`. Syncookies are on by default (tcp_ipv4.c sets 1) and
        // are what this engine's own tcp_syncookies rule asks for, so the
        // shape the rule used to fire on — a listener host with the default
        // 1024 — cannot read the value at all; the request queue is bounded by
        // sk_max_ack_backlog / net.core.somaxconn instead.
        assert!(
            syn_backlog_recommendation(1024, 0, true).is_some(),
            "a syncookie-less listener host still reads the knob"
        );
        for syncookies in [1, 2] {
            assert!(
                syn_backlog_recommendation(1024, syncookies, true).is_none(),
                "syncookies={syncookies} skips the only reader of the knob"
            );
        }
        assert!(
            syn_backlog_recommendation(1024, 0, false).is_none(),
            "no listener means no request queue to size"
        );
        assert!(
            syn_backlog_recommendation(8192, 0, true).is_none(),
            "an adequate backlog stays untouched"
        );

        let rec = syn_backlog_recommendation(1024, 0, true).expect("recommendation");
        assert_eq!(rec.param, "net.ipv4.tcp_max_syn_backlog");
        assert_eq!(rec.current_value, "1024");
        assert_eq!(rec.recommended_value, "65536");
    }

    #[test]
    fn test_tcp_syncookies_reads_truthiness_signed() {
        // tcp_syncookies was a plain proc_dointvec int with no min/max
        // through v5.10 (net/ipv4/sysctl_net_ipv4.c), and every consumer is a
        // truthiness test (net/ipv4/tcp_input.c: `if (syncookies)` arms the
        // cookie path, `if (!syncookies && ...)` guards the last-quarter
        // reservation, `syncookies == 2` selects the documented always-cookie
        // mode), so -1 is a legal, already-enabled value. The unsigned reader
        // parsed "-1" to Err and fell back to 0 — the *disabled* value — so
        // the `== 0` gate invented a SYN-flood finding on a host that already
        // answers floods with cookies.
        let info = make_test_info();
        // The rule only speaks on a host that listens on TCP; bind one so
        // the gate is exercised on any machine (the network_identity suite
        // uses the same trick).
        let _listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_syncookies_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_tcp_syncookies_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 leaves SYN cookies off"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.tcp_syncookies");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn syn_backlog_skips_when_syncookies_hold_minus_one() {
        // The syn-backlog rule reads tcp_syncookies as its gate, and that
        // knob was a plain proc_dointvec int through v5.10, so "-1" is a
        // legal, enabled value. The unsigned reader turned it into the
        // fallback 0 — disabled — and the rule sized the backlog on exactly
        // the hosts where the kernel skips the knob's only reader.
        let info = make_test_info();
        let _listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dir = std::env::temp_dir().join(format!(
            "ktuner_syn_backlog_signed_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let backlog = dir.join("tcp_max_syn_backlog");
        let syncookies = dir.join("tcp_syncookies");
        std::fs::write(&backlog, "1024\n").unwrap();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false), (2, false)] {
            std::fs::write(&syncookies, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_tcp_max_syn_backlog_at(
                &info,
                &mut recs,
                backlog.to_str().unwrap(),
                syncookies.to_str().unwrap(),
            );
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "syncookies {value}: any nonzero value skips the knob's only reader"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn slow_start_after_idle_is_not_read_by_bbr() {
        // Both readers of the knob skip congestion controls that define
        // `cong_control`: tcp_output.c's starvation branch ends
        // "... && !ca_ops->cong_control", and the include/net/tcp.h helper
        // returns early on `ca_ops->cong_control`. BBR defines it
        // (net/ipv4/tcp_bbr.c: `.cong_control = bbr_main`) and is exactly the
        // control this engine's own tcp_congestion_control rule asks for, so
        // on a BBR host the write cannot change any cwnd behavior.
        assert!(
            slow_start_after_idle_recommendation(1, "cubic", true).is_some(),
            "a control without cong_control still reads the knob"
        );
        assert!(
            slow_start_after_idle_recommendation(1, "bbr", true).is_none(),
            "BBR defines cong_control, so neither reader consults the knob"
        );
        assert!(
            slow_start_after_idle_recommendation(1, "", true).is_some(),
            "an unreadable congestion-control file keeps today's behavior"
        );
        assert!(
            slow_start_after_idle_recommendation(0, "cubic", true).is_none(),
            "an already-disabled knob stays untouched"
        );
        assert!(
            slow_start_after_idle_recommendation(1, "cubic", false).is_none(),
            "no listener means no connection to keep warm"
        );

        let rec = slow_start_after_idle_recommendation(1, "cubic", true).expect("recommendation");
        assert_eq!(rec.param, "net.ipv4.tcp_slow_start_after_idle");
        assert_eq!(rec.current_value, "1");
        assert_eq!(rec.recommended_value, "0");
    }

    #[test]
    fn slow_start_after_idle_treats_every_nonzero_value_as_enabled() {
        // tcp_slow_start_after_idle is a truthiness flag, not an enum:
        // tcp_output.c consumes it as
        // `if (READ_ONCE(...sysctl_tcp_slow_start_after_idle) && ...)`.
        // Through v5.10 it was a plain proc_dointvec int with no min/max
        // (net/ipv4/sysctl_net_ipv4.c), so -1 is a legal, enabled value; since
        // v5.14 the u8 registration (proc_dou8vec_minmax, no extra1/extra2)
        // accepts any 0-255 value, and every nonzero one is enabled. The old
        // `!= 1` gate missed both shapes: a -1 host read as the fallback 0,
        // and a u8 host holding 2 was skipped outright.
        for (value, expects_rec) in [(-1, true), (0, false), (1, true), (2, true)] {
            let rec = slow_start_after_idle_recommendation(value, "cubic", true);
            assert_eq!(
                rec.is_some(),
                expects_rec,
                "value {value}: any nonzero value restarts slow start after idle"
            );
            if let Some(rec) = rec {
                assert_eq!(
                    rec.current_value,
                    value.to_string(),
                    "the reported original is the value the kernel holds"
                );
                assert_eq!(rec.recommended_value, "0");
            }
        }
    }

    #[test]
    fn slow_start_after_idle_reads_the_live_value_signed() {
        // The reader itself must parse "-1" out of the file: the slot held a
        // signed int through v5.10, and the reported current has to be the
        // value the kernel actually holds, not the fallback of a failed
        // unsigned parse.
        let info = make_test_info();
        let _listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dir = std::env::temp_dir().join(format!(
            "ktuner_slow_start_signed_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let value = dir.join("tcp_slow_start_after_idle");
        let cc = dir.join("tcp_congestion_control");
        std::fs::write(&cc, "cubic\n").unwrap();
        for (raw, expects_rec) in [("-1", true), ("0", false), ("1", true), ("2", true)] {
            std::fs::write(&value, format!("{raw}\n")).unwrap();
            let mut recs = Vec::new();
            eval_tcp_slow_start_after_idle_at(
                &info,
                &mut recs,
                value.to_str().unwrap(),
                cc.to_str().unwrap(),
            );
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "raw {raw}: any nonzero value restarts slow start after idle"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.tcp_slow_start_after_idle");
                assert_eq!(recs[0].current_value, raw);
                assert_eq!(recs[0].recommended_value, "0");
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_tcp_no_metrics_save_reads_truthiness_signed() {
        // tcp_no_metrics_save was a plain proc_dointvec int with no min/max
        // through v5.10 (net/ipv4/sysctl_net_ipv4.c), and tcp_metrics.c
        // consumes it as
        // `if (READ_ONCE(net->ipv4.sysctl_tcp_nometrics_save) || !dst) return;`
        // — a truthiness test — so -1 is a legal value that already stops
        // metric caching. The unsigned reader parsed "-1" to Err and fell
        // back to 0, the *saving* value, so the `== 0` gate invented a
        // finding on a host that already discards stale metrics.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_no_metrics_save_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_tcp_no_metrics_save_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 keeps caching TCP metrics"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.tcp_no_metrics_save");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn ip_forward_treats_every_nonzero_value_as_enabled() {
        // net.ipv4.ip_forward is a plain proc_dointvec int behind
        // devinet_sysctl_forward (net/ipv4/devinet.c) — the handler's
        // extra1/extra2 are the devconf and net context, not min/max — and
        // IN_DEV_FORWARD (include/linux/inetdevice.h) is a truthiness test,
        // so -1 is a legal value that means forwarding is on. The unsigned
        // reader turned "-1" into the fallback 0 and the old `== 1` gate
        // skipped the rule on exactly that host.
        for (value, host_needs, expects_rec) in [
            (-1, false, true),
            (0, false, false),
            (1, false, true),
            (2, false, true),
            (-1, true, false),
            (1, true, false),
        ] {
            let rec = ip_forward_recommendation(value, host_needs);
            assert_eq!(
                rec.is_some(),
                expects_rec,
                "value {value}, host_needs {host_needs}"
            );
            if let Some(rec) = rec {
                assert_eq!(rec.param, "net.ipv4.ip_forward");
                assert_eq!(
                    rec.current_value,
                    value.to_string(),
                    "the reported original is the value the kernel holds"
                );
                assert_eq!(rec.recommended_value, "0");
            }
        }
    }

    #[test]
    fn test_inotify_watches() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_inotify_max_user_watches(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "fs.inotify.max_user_watches")
        {
            assert_eq!(rec.recommended_value, "524288");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_aio_max_nr() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_aio_max_nr(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "fs.aio-max-nr") {
            assert_eq!(rec.recommended_value, "1048576");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_oom_kill_allocating_task() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_oom_kill_allocating_task(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "vm.oom_kill_allocating_task")
        {
            assert_eq!(rec.recommended_value, "1");
        }
    }

    #[test]
    fn test_optmem_max() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_optmem_max(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.core.optmem_max") {
            assert_eq!(rec.recommended_value, "81920");
        }
    }

    #[test]
    fn test_score_bounds() {
        let result = EvalResult {
            recommendations: vec![
                Recommendation {
                    param: "a".into(),
                    current_value: "0".into(),
                    recommended_value: "1".into(),
                    reason: "test".into(),
                    confidence: Confidence::High,
                    category: Category::Performance,
                    writable: true,
                };
                50
            ],
            total_checked: 60,
        };
        let score = result.score();
        assert!(score >= 30, "Score should have floor of 30, got {score}");
    }

    #[test]
    fn test_dirty_background_ratio_with_db() {
        let mut info = make_test_info();
        info.memory_total_gb = 32; // <64GB uses the ratio form
        info.sysctl.dirty_background_ratio = 10;
        info.processes = vec![ProcessInfo {
            name: "postgres".to_string(),
        }];
        let mut recs = Vec::new();
        eval_dirty_background_ratio(&info, &WorkloadType::IoLatency, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.dirty_background_ratio");
        assert!(
            rec.is_some(),
            "Should recommend lower dirty_background_ratio for DB"
        );
        assert_eq!(rec.unwrap().recommended_value, "3");
    }

    #[test]
    fn test_dirty_background_ratio_needs_room_under_the_limit() {
        // domain_dirty_limits() (mm/page-writeback.c) replaces a background
        // threshold at or above the dirty threshold with half of it, so a
        // target at or above the dirty ratio in force cannot move the
        // effective threshold — it already is dirty_limit / 2.
        let mut info = make_test_info();
        info.memory_total_gb = 32; // <64GB uses the ratio form
        info.processes.clear(); // not a database host
        info.sysctl.dirty_ratio = 5;
        info.sysctl.dirty_background_ratio = 15;

        let mut recs = Vec::new();
        eval_dirty_background_ratio(&info, &WorkloadType::Mixed, &mut recs);
        assert!(
            recs.is_empty(),
            "a 5% target cannot move the effective threshold below 5% / 2"
        );

        // Room under the dirty limit keeps the advice.
        info.sysctl.dirty_ratio = 20;
        let mut recs = Vec::new();
        eval_dirty_background_ratio(&info, &WorkloadType::Mixed, &mut recs);
        assert_eq!(recs.len(), 1, "5% stays below the 20% dirty limit");
        assert_eq!(recs[0].recommended_value, "5");
    }

    #[test]
    fn test_dirty_ratio_rule_keeps_its_background_advice_reachable() {
        // The same clamp applies to the latency rule's background half: while
        // the dirty ratio stays at or below 3%, lowering the background
        // threshold to 3% changes nothing.
        let mut info = make_test_info();
        info.memory_total_gb = 32;
        info.processes.clear();
        info.sysctl.dirty_ratio = 3;
        info.sysctl.dirty_background_ratio = 15;

        let mut recs = Vec::new();
        eval_dirty_ratio(&info, &WorkloadType::IoLatency, &mut recs);
        assert!(
            recs.is_empty(),
            "a 3% dirty ratio already clamps the background threshold to 1.5%"
        );

        info.sysctl.dirty_ratio = 4;
        let mut recs = Vec::new();
        eval_dirty_ratio(&info, &WorkloadType::IoLatency, &mut recs);
        assert_eq!(recs.len(), 1, "3% still fits under a 4% dirty ratio");
        assert_eq!(recs[0].recommended_value, "3");
    }

    #[test]
    fn test_icmp_echo_ignore_broadcasts() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_icmp_echo_ignore_broadcasts(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.icmp_echo_ignore_broadcasts")
        {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_accept_source_route() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_accept_source_route(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.accept_source_route")
        {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.confidence, Confidence::High);
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_sched_min_granularity_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        eval_sched_min_granularity(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "kernel.sched_min_granularity_ns")
        {
            assert_eq!(rec.recommended_value, "10000000");
        }
    }

    #[test]
    fn test_sched_min_granularity_small_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let mut recs = Vec::new();
        eval_sched_min_granularity(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "kernel.sched_min_granularity_ns");
        assert!(
            rec.is_none(),
            "Should not recommend sched_min_granularity for small CPU count"
        );
    }

    #[test]
    fn test_neigh_gc_thresh3() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_neigh_gc_thresh3(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.neigh.default.gc_thresh3")
        {
            assert_eq!(rec.recommended_value, "8192");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_nr_open() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_nr_open(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "fs.nr_open") {
            assert_eq!(rec.recommended_value, "1048576");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_arp_announce_multi_nic() {
        let mut info = make_test_info();
        info.network = vec![
            NetInfo {
                name: "eth0".to_string(),
                speed_mbps: 10000,
            },
            NetInfo {
                name: "eth1".to_string(),
                speed_mbps: 10000,
            },
        ];
        let mut recs = Vec::new();
        eval_arp_announce(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.arp_announce")
        {
            assert_eq!(rec.recommended_value, "2");
        }
    }

    #[test]
    fn rfs_flow_table_is_only_sized_where_the_queue_flow_table_is() {
        // get_rps_cpu() consults the global table only after the receive
        // queue's own flow table exists (net/core/dev.c: `if (!flow_table &&
        // !map) goto done;` ... `if (flow_table && sock_flow_table) { ... }`),
        // and that per-queue table is what `rps_flow_cnt` sizes. A queue with
        // only `rps_cpus` set takes the plain-RPS branch and never reads the
        // global table; a queue with `rps_flow_cnt` reaches it even when every
        // `rps_cpus` mask is left at 0, because writing the global table
        // raises `rps_needed` itself (net/core/sysctl_net_core.c).
        let dir = std::env::temp_dir().join(format!(
            "ktuner_rfs_flow_cnt_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |relative: &str, content: &str| {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
            std::fs::write(&path, content).unwrap();
        };
        let param = dir.join("rps_sock_flow_entries");
        std::fs::write(&param, "4096\n").unwrap();

        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        // The shape RFS needs: a receive queue with its own flow table, even
        // though no queue carries an `rps_cpus` mask.
        write("eth0/queues/rx-0/rps_cpus", "0\n");
        write("eth0/queues/rx-0/rps_flow_cnt", " 2048 \n");
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries_at(&info, &mut recs, param.to_str().unwrap(), &dir);
        assert!(
            recs.iter()
                .any(|r| r.param == "net.core.rps_sock_flow_entries"),
            "rps_flow_cnt reaches the global table without any rps_cpus"
        );

        // `rps_cpus` on its own steers through plain RPS and never reads the
        // global table.
        write("eth0/queues/rx-0/rps_flow_cnt", "0\n");
        write("eth0/queues/rx-0/rps_cpus", "00000000,00000030\n");
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries_at(&info, &mut recs, param.to_str().unwrap(), &dir);
        assert!(recs.is_empty(), "rps_cpus alone reads no flow table");

        // A transmit queue's flow count must not count, and an unreadable tree
        // counts as unconfigured instead of panicking.
        write("eth0/queues/rx-0/rps_cpus", "0\n");
        write("eth0/queues/tx-0/rps_flow_cnt", "4096\n");
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries_at(&info, &mut recs, param.to_str().unwrap(), &dir);
        assert!(recs.is_empty(), "a transmit queue holds no rx flow table");
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries_at(
            &info,
            &mut recs,
            param.to_str().unwrap(),
            &dir.join("missing"),
        );
        assert!(recs.is_empty(), "no sysfs tree means nothing is steered");

        // The 10-GbE gate is unchanged.
        let mut slow = make_test_info();
        slow.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 1000,
        }];
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries_at(&slow, &mut recs, param.to_str().unwrap(), &dir);
        assert!(recs.is_empty(), "a slow NIC keeps the rule silent");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_arp_announce_single_nic() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_arp_announce(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.arp_announce");
        assert!(
            rec.is_none(),
            "Should not recommend arp_announce for single NIC"
        );
    }

    #[test]
    fn test_nmi_watchdog() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_nmi_watchdog(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.nmi_watchdog") {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.confidence, Confidence::Medium);
        }
    }

    #[test]
    fn test_stat_interval_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        eval_stat_interval(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.stat_interval") {
            assert_eq!(rec.recommended_value, "5");
        }
    }

    #[test]
    fn test_stat_interval_small_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let mut recs = Vec::new();
        eval_stat_interval(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.stat_interval");
        assert!(
            rec.is_none(),
            "Should not recommend stat_interval for small CPU count"
        );
    }

    #[test]
    fn test_hung_task_timeout() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_hung_task_timeout(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "kernel.hung_task_timeout_secs")
        {
            assert_eq!(rec.recommended_value, "120");
        }
    }

    #[test]
    fn test_arp_ignore_multi_nic() {
        let mut info = make_test_info();
        info.network = vec![
            NetInfo {
                name: "eth0".to_string(),
                speed_mbps: 10000,
            },
            NetInfo {
                name: "eth1".to_string(),
                speed_mbps: 10000,
            },
        ];
        let mut recs = Vec::new();
        eval_arp_ignore(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.arp_ignore")
        {
            assert_eq!(rec.recommended_value, "1");
        }
    }

    #[test]
    fn test_tcp_rfc1337() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_rfc1337(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_rfc1337") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_secure_redirects() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_secure_redirects(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.secure_redirects")
        {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_mmap_min_addr() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_mmap_min_addr(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.mmap_min_addr") {
            assert_eq!(rec.recommended_value, "65536");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_netdev_budget_usecs_10g() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_netdev_budget_usecs(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.core.netdev_budget_usecs")
        {
            assert_eq!(rec.recommended_value, "8000");
        }
    }

    #[test]
    fn test_netdev_budget_usecs_1g_skip() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 1000,
        }];
        let mut recs = Vec::new();
        eval_netdev_budget_usecs(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "net.core.netdev_budget_usecs");
        assert!(
            rec.is_none(),
            "Should not recommend netdev_budget_usecs for 1G network"
        );
    }

    #[test]
    fn test_dirty_bytes_large_ram() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;
        let mut recs = Vec::new();
        eval_dirty_bytes(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.dirty_bytes") {
            assert_eq!(rec.recommended_value, "268435456");
        }
    }

    #[test]
    fn test_dirty_bytes_small_ram_skip() {
        let mut info = make_test_info();
        info.memory_total_gb = 32;
        let mut recs = Vec::new();
        eval_dirty_bytes(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.dirty_bytes");
        assert!(
            rec.is_none(),
            "Should not recommend dirty_bytes for small RAM"
        );
    }

    #[test]
    fn dirty_bytes_stays_quiet_while_a_pinned_ratio_holds_the_limit() {
        // A >=64GB host whose administrator pinned vm.dirty_ratio reads
        // vm.dirty_bytes as 0, because the kernel zeroes the twin on every
        // changed ratio write (v6.6 mm/page-writeback.c:527) — vm.rst:
        // "the other appears as 0 when read". That 0 is the DISABLED
        // counterpart, not "nothing configured": the ratio in force is the
        // administrator's, and the bytes write would replace it with
        // DIRTY_BYTES_TARGET — the opposite of what the rule's reason
        // promises. The advice must not fire.
        let mut recs = Vec::new();
        dirty_bytes_recommendation(256, 0, 3, &mut recs);
        assert!(
            recs.is_empty(),
            "a pinned dirty_ratio of 3 holds the limit, so the zero dirty_bytes is disabled, not unconfigured: {recs:?}"
        );

        // The same zero bytes with the kernel-default 20 ratio is the
        // "nothing configured" case the rule exists for, and still fires.
        let mut recs = Vec::new();
        dirty_bytes_recommendation(256, 0, 20, &mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].param, "vm.dirty_bytes");
        assert_eq!(recs[0].current_value, "0");
        assert_eq!(recs[0].recommended_value, "268435456");

        // A host already on the bytes form has nothing to switch away from.
        let mut recs = Vec::new();
        dirty_bytes_recommendation(256, 512 * 1024 * 1024, 0, &mut recs);
        assert!(recs.is_empty(), "the bytes form already governs: {recs:?}");
    }

    #[test]
    fn test_sched_child_runs_first() {
        let mut info = make_test_info();
        info.processes = vec![ProcessInfo {
            name: "nginx".to_string(),
        }];
        let mut recs = Vec::new();
        eval_sched_child_runs_first(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "kernel.sched_child_runs_first")
        {
            assert_eq!(rec.recommended_value, "0");
        }
    }

    #[test]
    fn fork_server_present_covers_the_apache_names() {
        // The rule's gate, driven directly: whether a server name opens the
        // gate is pure, while the live sysctl read inside the rule is not.
        // Debian/Ubuntu run Apache as `apache2`; RHEL's `httpd` is the same
        // server, and the gate must open under both names.
        for name in ["nginx", "httpd", "apache2", "postgres", "mysqld"] {
            let mut info = make_test_info();
            info.processes = vec![ProcessInfo {
                name: name.to_string(),
            }];
            assert!(fork_server_present(&info), "{name} must open the gate");
        }
        // A client tool with a shared prefix is not the server...
        let mut info = make_test_info();
        info.processes = vec![ProcessInfo {
            name: "apache2ctl".to_string(),
        }];
        assert!(!fork_server_present(&info), "apache2ctl is a control tool");
        // ...and neither is an empty process list.
        let mut info = make_test_info();
        info.processes = vec![];
        assert!(!fork_server_present(&info));
    }

    #[test]
    fn sched_child_runs_first_recommendation_gates_on_kernel_version() {
        // Pre-6.6 kernels still consume the knob: task_fork_fair() reads it.
        let rec = sched_child_runs_first_recommendation(1, "5.15.0-91-generic")
            .expect("pre-6.6 kernels still honor sched_child_runs_first");
        assert_eq!(rec.recommended_value, "0");
        assert_eq!(rec.current_value, "1");
        assert!(sched_child_runs_first_recommendation(2, "6.5.0").is_some());
        // 6.6+ merged EEVDF and task_fork_fair() no longer reads the knob:
        // the recommendation can never change any behavior.
        assert!(sched_child_runs_first_recommendation(1, "6.6.0").is_none());
        assert!(sched_child_runs_first_recommendation(1, "6.8.0-40-generic").is_none());
        // Already at the target on any version.
        assert!(sched_child_runs_first_recommendation(0, "4.19.0").is_none());
    }

    #[test]
    fn test_default_accept_redirects() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_default_accept_redirects(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.default.accept_redirects")
        {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_default_accept_source_route() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_default_accept_source_route(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.default.accept_source_route")
        {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_sched_latency_ns_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        eval_sched_latency_ns(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.sched_latency_ns") {
            assert_eq!(rec.recommended_value, "24000000");
        }
    }

    #[test]
    fn test_sched_latency_ns_small_cpu_skip() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let mut recs = Vec::new();
        eval_sched_latency_ns(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "kernel.sched_latency_ns");
        assert!(
            rec.is_none(),
            "Should not recommend sched_latency_ns for small CPU count"
        );
    }

    #[test]
    fn test_tcp_challenge_ack_limit() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_challenge_ack_limit(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_challenge_ack_limit")
        {
            assert_eq!(rec.recommended_value, "2147483647");
            assert_eq!(rec.confidence, Confidence::High);
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn challenge_ack_limit_recommends_the_unlimited_sentinel() {
        // tcp_send_challenge_ack (net/ipv4/tcp_input.c) takes the unlimited
        // path only at INT_MAX (`if (ack_limit == INT_MAX) goto send_ack;`);
        // every other value installs the randomized per-second budget the
        // side channel measures. 999999999 still installs it, so the
        // recommendation is the value the kernel checks.
        let rec = challenge_ack_limit_recommendation(100).expect("a 100/second cap is a finding");
        assert_eq!(rec.current_value, "100");
        assert_eq!(rec.recommended_value, "2147483647");
        // The reason used to warn about a default of 100; tcp_ipv4.c
        // initializes the sysctl to INT_MAX, so no such default exists.
        assert!(
            !rec.reason.contains("默认值 100"),
            "reason must not cite a default the kernel does not have: {}",
            rec.reason
        );
        // The kernel default is already the recommendation.
        assert!(challenge_ack_limit_recommendation(2147483647).is_none());
        // The boundary itself is unchanged.
        assert!(challenge_ack_limit_recommendation(101).is_none());
        assert!(challenge_ack_limit_recommendation(0).is_some());
    }

    #[test]
    fn max_dgram_qlen_quotes_the_kernel_default() {
        // unix_net_init (net/unix/af_unix.c) sets sysctl_max_dgram_qlen = 10,
        // not the 512 the reason used to cite.
        let rec = max_dgram_qlen_recommendation(10).expect("the kernel default is a finding");
        assert_eq!(rec.recommended_value, "1024");
        assert!(
            !rec.reason.contains("默认 512"),
            "reason must not cite a default the kernel does not have: {}",
            rec.reason
        );
        assert!(max_dgram_qlen_recommendation(1024).is_none());
        assert!(max_dgram_qlen_recommendation(1023).is_some());
    }

    #[test]
    fn test_rp_filter_all() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_rp_filter_all(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.all.rp_filter")
        {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_page_cluster_with_ssd() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_page_cluster(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.page-cluster") {
            assert_eq!(rec.recommended_value, "0");
        }
    }

    #[test]
    fn page_cluster_needs_a_swap_area_to_be_worth_setting() {
        // vm.page-cluster sizes the swap-in readahead window: v6.6 reads it
        // only from mm/swap_state.c (swapin_nr_pages / swapin_readahead / the
        // swap VMA readahead). On an SSD host with no swap area the
        // recommendation's promised IO saving cannot happen, so the rule must
        // stay quiet — the same "don't recommend a no-op" rule the
        // hardlockup_panic gate follows for a disabled NMI watchdog.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_page_cluster_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let param = dir.join("page-cluster");
        std::fs::write(&param, b"3\n").unwrap();
        // /proc/swaps always carries its header, so a header-only table means
        // no configured area.
        let header_only = dir.join("swaps-header-only");
        std::fs::write(
            &header_only,
            b"Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n",
        )
        .unwrap();
        let with_swap = dir.join("swaps-active");
        std::fs::write(
            &with_swap,
            b"Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n/dev/sda2                               partition\t16776188\t0\t\t-2\n",
        )
        .unwrap();
        let missing = dir.join("swaps-missing");

        let info = make_test_info();

        for swaps in [&header_only, &missing] {
            let mut recs = Vec::new();
            eval_page_cluster_at(
                &info,
                &mut recs,
                param.to_str().unwrap(),
                swaps.to_str().unwrap(),
            );
            assert!(
                recs.is_empty(),
                "{}: no swap area means nothing to read in",
                swaps.display()
            );
        }

        let mut recs = Vec::new();
        eval_page_cluster_at(
            &info,
            &mut recs,
            param.to_str().unwrap(),
            with_swap.to_str().unwrap(),
        );
        assert!(
            recs.iter().any(|r| r.param == "vm.page-cluster"),
            "an active swap area keeps the rule"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_rmem_default_10g() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_rmem_default(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.core.rmem_default") {
            assert_eq!(rec.recommended_value, "262144");
        }
    }

    #[test]
    fn test_rmem_default_1g_skip() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 1000,
        }];
        let mut recs = Vec::new();
        eval_rmem_default(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "net.core.rmem_default");
        assert!(
            rec.is_none(),
            "Should not recommend rmem_default for 1G network"
        );
    }

    #[test]
    fn test_wmem_default_10g() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_wmem_default(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.core.wmem_default") {
            assert_eq!(rec.recommended_value, "262144");
        }
    }

    #[test]
    fn test_sched_nr_migrate_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        eval_sched_nr_migrate(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.sched_nr_migrate") {
            assert_eq!(rec.recommended_value, "128");
        }
    }

    #[test]
    fn test_sched_nr_migrate_small_cpu_skip() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let mut recs = Vec::new();
        eval_sched_nr_migrate(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "kernel.sched_nr_migrate");
        assert!(
            rec.is_none(),
            "Should not recommend sched_nr_migrate for small CPU count"
        );
    }

    #[test]
    fn test_tcp_notsent_lowat() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_notsent_lowat(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_notsent_lowat")
        {
            assert_eq!(rec.recommended_value, "131072");
        }
    }

    #[test]
    fn test_unix_max_dgram_qlen() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_unix_max_dgram_qlen(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.unix.max_dgram_qlen") {
            assert_eq!(rec.recommended_value, "1024");
        }
    }

    #[test]
    fn test_rps_sock_flow_entries_10g() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.core.rps_sock_flow_entries")
        {
            assert_eq!(rec.recommended_value, "32768");
        }
    }

    #[test]
    fn test_rps_sock_flow_entries_1g_skip() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 1000,
        }];
        let mut recs = Vec::new();
        eval_rps_sock_flow_entries(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "net.core.rps_sock_flow_entries");
        assert!(
            rec.is_none(),
            "Should not recommend rps_sock_flow_entries for 1G network"
        );
    }

    #[test]
    fn test_tcp_dsack() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_dsack(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_dsack") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_sched_wakeup_granularity_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        eval_sched_wakeup_granularity(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "kernel.sched_wakeup_granularity_ns")
        {
            assert_eq!(rec.recommended_value, "3000000");
        }
    }

    #[test]
    fn test_sched_wakeup_granularity_small_cpu_skip() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let mut recs = Vec::new();
        eval_sched_wakeup_granularity(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "kernel.sched_wakeup_granularity_ns");
        assert!(
            rec.is_none(),
            "Should not recommend sched_wakeup_granularity for small CPU count"
        );
    }

    #[test]
    fn test_extfrag_threshold_large_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;
        let mut recs = Vec::new();
        eval_extfrag_threshold(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.extfrag_threshold") {
            assert_eq!(rec.recommended_value, "100");
        }
    }

    #[test]
    fn test_extfrag_threshold_small_mem_skip() {
        let mut info = make_test_info();
        info.memory_total_gb = 16;
        let mut recs = Vec::new();
        eval_extfrag_threshold(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.extfrag_threshold");
        assert!(
            rec.is_none(),
            "Should not recommend extfrag_threshold for small memory"
        );
    }

    #[test]
    fn test_default_send_redirects() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_default_send_redirects(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.conf.default.send_redirects")
        {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn send_redirects_advice_needs_a_forwarding_interface() {
        // ip_route_input_slow() turns a non-local destination into an
        // EHOSTUNREACH error route when the input interface does not forward
        // ("if (!IN_DEV_FORWARD(in_dev)) { err = -EHOSTUNREACH; goto no_route; }"),
        // so ip_forward() — the only caller of ip_rt_send_redirect(), where
        // IN_DEV_TX_REDIRECTS is checked — never runs. The kernel documentation
        // states the same precondition: "Send redirects, if router."
        let dir = std::env::temp_dir().join(format!(
            "ktuner_send_redirects_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |relative: &str, content: &str| {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
            std::fs::write(&path, content).unwrap();
            path
        };

        let info = make_test_info();
        let all = write("all/send_redirects", "1\n");
        let default = write("default/send_redirects", "1\n");
        for interface in ["all", "default", "lo", "eth0"] {
            write(&format!("{interface}/forwarding"), "0\n");
        }

        // Nothing forwards, so no redirect can be sent on this host.
        let mut recs = Vec::new();
        eval_send_redirects_at(&info, &mut recs, all.to_str().unwrap(), &dir);
        eval_default_send_redirects_at(&info, &mut recs, default.to_str().unwrap(), &dir);
        assert!(
            recs.is_empty(),
            "a host that forwards nothing cannot send a redirect"
        );

        // One forwarding interface keeps both rules.
        write("eth0/forwarding", "1\n");
        let mut recs = Vec::new();
        eval_send_redirects_at(&info, &mut recs, all.to_str().unwrap(), &dir);
        eval_default_send_redirects_at(&info, &mut recs, default.to_str().unwrap(), &dir);
        assert_eq!(recs.len(), 2, "a router still gets both recommendations");

        // The `default` template alone arms future interfaces, so it counts.
        write("eth0/forwarding", "0\n");
        write("default/forwarding", "1\n");
        let mut recs = Vec::new();
        eval_default_send_redirects_at(&info, &mut recs, default.to_str().unwrap(), &dir);
        assert!(!recs.is_empty(), "the template arms new interfaces");

        // A missing tree cannot prove a redirect is possible.
        write("default/forwarding", "0\n");
        let mut recs = Vec::new();
        eval_send_redirects_at(
            &info,
            &mut recs,
            all.to_str().unwrap(),
            &dir.join("missing"),
        );
        eval_default_send_redirects_at(
            &info,
            &mut recs,
            default.to_str().unwrap(),
            &dir.join("missing"),
        );
        assert!(recs.is_empty(), "an unreadable tree stays quiet");

        // A disabled knob stays quiet even where a router exists.
        write("eth0/forwarding", "1\n");
        let off = write("default/send_redirects_off", "0\n");
        let mut recs = Vec::new();
        eval_default_send_redirects_at(&info, &mut recs, off.to_str().unwrap(), &dir);
        assert!(recs.is_empty(), "an already-disabled knob is satisfied");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn send_redirects_rules_read_truthiness_signed() {
        // conf/*/send_redirects is a devinet_conf_proc plain proc_dointvec int
        // slot with no min/max (net/ipv4/devinet.c) — the same registration
        // the five signed-read fixes already covered — and
        // IN_DEV_TX_REDIRECTS (include/linux/inetdevice.h) consumes it
        // through IN_DEV_ORCONF, a truthiness test. The old `== 1` gate
        // skipped a host whose redirects are enabled with -1, while the
        // unsigned reader parsed "-1" to Err and fell back to 0, the
        // *disabled* value.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_send_redirects_signed_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("all")).unwrap();
        std::fs::create_dir_all(dir.join("default")).unwrap();
        let all = dir.join("all/send_redirects");
        let default = dir.join("default/send_redirects");
        // One forwarding interface makes the redirect path reachable.
        std::fs::create_dir_all(dir.join("eth0")).unwrap();
        std::fs::write(dir.join("eth0/forwarding"), "1\n").unwrap();

        let info = make_test_info();
        for (value, expects_rec) in [(-1, true), (0, false), (1, true)] {
            std::fs::write(&all, format!("{value}\n")).unwrap();
            std::fs::write(&default, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_send_redirects_at(&info, &mut recs, all.to_str().unwrap(), &dir);
            eval_default_send_redirects_at(&info, &mut recs, default.to_str().unwrap(), &dir);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec) * 2,
                "value {value}: both templates track the same value"
            );
            if expects_rec {
                for rec in &recs {
                    assert_eq!(rec.current_value, value.to_string());
                    assert_eq!(rec.recommended_value, "0");
                }
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_neigh_gc_thresh1() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_neigh_gc_thresh1(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.neigh.default.gc_thresh1")
        {
            assert_eq!(rec.recommended_value, "2048");
        }
    }

    #[test]
    fn test_neigh_gc_thresh2() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_neigh_gc_thresh2(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.neigh.default.gc_thresh2")
        {
            assert_eq!(rec.recommended_value, "4096");
        }
    }

    #[test]
    fn test_tcp_retries1() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_retries1(&info, &mut recs);
        // retries1 default is 3, should not trigger
        let rec = recs.iter().find(|r| r.param == "net.ipv4.tcp_retries1");
        assert!(
            rec.is_none(),
            "Should not recommend tcp_retries1 when default is 3"
        );
    }

    #[test]
    fn test_tcp_limit_output_bytes_10g() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_tcp_limit_output_bytes(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_limit_output_bytes")
        {
            assert_eq!(rec.recommended_value, "1048576");
        }
    }

    #[test]
    fn test_tcp_limit_output_bytes_1g_skip() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 1000,
        }];
        let mut recs = Vec::new();
        eval_tcp_limit_output_bytes(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_limit_output_bytes");
        assert!(
            rec.is_none(),
            "Should not recommend tcp_limit_output_bytes for 1G network"
        );
    }

    #[test]
    fn test_dev_weight_10g() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_dev_weight(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.core.dev_weight") {
            assert_eq!(rec.recommended_value, "128");
        }
    }

    #[test]
    fn test_dev_weight_1g_skip() {
        let mut info = make_test_info();
        info.network = vec![NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 1000,
        }];
        let mut recs = Vec::new();
        eval_dev_weight(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "net.core.dev_weight");
        assert!(
            rec.is_none(),
            "Should not recommend dev_weight for 1G network"
        );
    }

    #[test]
    fn test_watchdog_thresh_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        eval_watchdog_thresh(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.watchdog_thresh") {
            assert_eq!(rec.recommended_value, "30");
        }
    }

    #[test]
    fn test_watchdog_thresh_small_cpu_skip() {
        let mut info = make_test_info();
        info.cpu_cores = 8;
        let mut recs = Vec::new();
        eval_watchdog_thresh(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "kernel.watchdog_thresh");
        assert!(
            rec.is_none(),
            "Should not recommend watchdog_thresh for small CPU count"
        );
    }

    #[test]
    fn test_admin_reserve_large_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;
        let mut recs = Vec::new();
        eval_admin_reserve_kbytes(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.admin_reserve_kbytes") {
            assert_eq!(rec.recommended_value, "131072");
        }
    }

    #[test]
    fn test_admin_reserve_small_mem_skip() {
        let mut info = make_test_info();
        info.memory_total_gb = 16;
        let mut recs = Vec::new();
        eval_admin_reserve_kbytes(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.admin_reserve_kbytes");
        assert!(
            rec.is_none(),
            "Should not recommend admin_reserve_kbytes for small memory"
        );
    }

    #[test]
    fn test_msgmax() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_msgmax(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.msgmax") {
            assert_eq!(rec.recommended_value, "65536");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_msgmnb() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_msgmnb(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.msgmnb") {
            assert_eq!(rec.recommended_value, "65536");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_protected_fifos() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_protected_fifos(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "fs.protected_fifos") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.confidence, Confidence::High);
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_user_reserve_kbytes_large_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;
        let mut recs = Vec::new();
        eval_user_reserve_kbytes(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.user_reserve_kbytes") {
            assert_eq!(rec.recommended_value, "262144");
        }
    }

    #[test]
    fn test_user_reserve_kbytes_small_mem_skip() {
        let mut info = make_test_info();
        info.memory_total_gb = 16;
        let mut recs = Vec::new();
        eval_user_reserve_kbytes(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.user_reserve_kbytes");
        assert!(
            rec.is_none(),
            "Should not recommend user_reserve_kbytes for small memory"
        );
    }

    #[test]
    fn commit_reserves_need_overcommit_never_to_be_read() {
        // `__vm_enough_memory()` (mm/util.c) returns inside the
        // OVERCOMMIT_GUESS branch before either reserve is subtracted:
        //
        //	if (sysctl_overcommit_memory == OVERCOMMIT_GUESS) { ...; return 0; }
        //	allowed = vm_commit_limit();
        //	if (!cap_sys_admin)
        //		allowed -= sysctl_admin_reserve_kbytes >> ...;
        //	if (mm) { ... sysctl_user_reserve_kbytes ... }
        //
        // So only vm.overcommit_memory == 2 reads them, which is why the
        // sibling overcommit_ratio rule already gates on oc_mode == 2. On any
        // other host the promised recovery headroom cannot exist.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_commit_reserves_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, content: &str| {
            let path = dir.join(name);
            std::fs::write(&path, content).unwrap();
            path
        };
        let admin = write("admin_reserve_kbytes", "8192\n");
        let user = write("user_reserve_kbytes", "16384\n");
        let mode_never = write("overcommit_memory.never", "2\n");
        let mode_guess = write("overcommit_memory.guess", "0\n");
        let mode_always = write("overcommit_memory.always", "1\n");
        let mode_missing = dir.join("overcommit_memory.missing");

        let mut info = make_test_info();
        info.memory_total_gb = 128;

        for mode in [&mode_guess, &mode_always, &mode_missing] {
            let mut recs = Vec::new();
            eval_admin_reserve_kbytes_at(
                &info,
                &mut recs,
                admin.to_str().unwrap(),
                mode.to_str().unwrap(),
            );
            eval_user_reserve_kbytes_at(
                &info,
                &mut recs,
                user.to_str().unwrap(),
                mode.to_str().unwrap(),
            );
            assert!(
                recs.is_empty(),
                "{}: the kernel never reads either reserve",
                mode.display()
            );
        }

        let mut recs = Vec::new();
        eval_admin_reserve_kbytes_at(
            &info,
            &mut recs,
            admin.to_str().unwrap(),
            mode_never.to_str().unwrap(),
        );
        assert!(
            recs.iter().any(|r| r.param == "vm.admin_reserve_kbytes"),
            "overcommit never reads the admin reserve"
        );
        eval_user_reserve_kbytes_at(
            &info,
            &mut recs,
            user.to_str().unwrap(),
            mode_never.to_str().unwrap(),
        );
        assert!(
            recs.iter().any(|r| r.param == "vm.user_reserve_kbytes"),
            "overcommit never reads the user reserve"
        );

        // Adequate reserves stay untouched, still under the mode that reads
        // them.
        let admin_ok = write("admin_reserve_kbytes_ok", "131072\n");
        let user_ok = write("user_reserve_kbytes_ok", "262144\n");
        let mut recs = Vec::new();
        eval_admin_reserve_kbytes_at(
            &info,
            &mut recs,
            admin_ok.to_str().unwrap(),
            mode_never.to_str().unwrap(),
        );
        eval_user_reserve_kbytes_at(
            &info,
            &mut recs,
            user_ok.to_str().unwrap(),
            mode_never.to_str().unwrap(),
        );
        assert!(recs.is_empty(), "an adequate reserve stays untouched");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_shmmni_with_database() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_shmmni(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.shmmni") {
            assert_eq!(rec.recommended_value, "8192");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_shmmni_skip_without_db_small_mem() {
        let mut info = make_test_info();
        info.processes = vec![];
        info.memory_total_gb = 32;
        let mut recs = Vec::new();
        eval_shmmni(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "kernel.shmmni");
        assert!(
            rec.is_none(),
            "Should not recommend shmmni without database on small memory"
        );
    }

    #[test]
    fn test_sem_with_database() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_sem(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.sem") {
            assert_eq!(rec.recommended_value, "1024 65536 256 4096");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_sem_skip_without_database() {
        let mut info = make_test_info();
        info.processes = vec![];
        let mut recs = Vec::new();
        eval_sem(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "kernel.sem");
        assert!(rec.is_none(), "Should not recommend sem without database");
    }

    #[test]
    fn test_gc_stale_time() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_gc_stale_time(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param.contains("gc_stale_time")) {
            assert_eq!(rec.recommended_value, "120");
        }
    }

    #[test]
    fn test_shm_rmid_forced() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_shm_rmid_forced(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.shm_rmid_forced") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_tcp_fack() {
        // Shape of the recommendation on a pre-4.15 kernel, where the knob
        // is still consumed by the TCP stack.
        let rec = tcp_fack_recommendation(0, "3.10.0-1160.el7")
            .expect("pre-4.15 kernel still consumes the tcp_fack knob");
        assert_eq!(rec.recommended_value, "1");
        assert_eq!(rec.category, Category::Performance);
    }

    #[test]
    fn kernel_at_least_parses_release_suffixes() {
        assert!(kernel_at_least("6.8.0-40-generic", 4, 15));
        assert!(kernel_at_least("5.15.0-microsoft-standard-WSL2", 5, 0));
        // The boundary itself counts: FACK was removed in 4.15.
        assert!(kernel_at_least("4.15.0", 4, 15));
        assert!(!kernel_at_least("4.14.209", 4, 15));
        assert!(!kernel_at_least("3.10.0-1160.el7", 4, 15));
        assert!(!kernel_at_least("5.4", 6, 0));
        // Unparseable strings keep the caller's legacy behavior.
        assert!(!kernel_at_least("", 4, 15));
        assert!(!kernel_at_least("custom-kernel", 4, 15));
    }

    #[test]
    fn tcp_fack_recommendation_gates_on_kernel_version() {
        // Pre-4.15 kernels still consume the knob.
        assert!(tcp_fack_recommendation(0, "3.10.0-1160.el7").is_some());
        assert!(tcp_fack_recommendation(0, "4.14.209").is_some());
        // 4.15+ removed FACK from the TCP stack (commit 95f5acbf3e12); the
        // sysctl file still exists on modern kernels but nothing reads it, so
        // the advice would promise loss-recovery gains that cannot happen.
        assert!(tcp_fack_recommendation(0, "4.15.0").is_none());
        assert!(tcp_fack_recommendation(0, "6.8.0-40-generic").is_none());
        assert!(tcp_fack_recommendation(0, "5.4.0").is_none());
        // Already enabled: no recommendation on any version.
        assert!(tcp_fack_recommendation(1, "3.10.0-1160.el7").is_none());
    }

    #[test]
    fn test_tcp_fack_not_recommended_on_modern_kernel() {
        // Red on main: make_test_info() reports kernel 5.4 with tcp_fack=0 on
        // the host, and the rule happily recommends a knob that no 4.15+
        // kernel reads anymore.
        let path = "/proc/sys/net/ipv4/tcp_fack";
        if !std::path::Path::new(path).exists() || read_sysctl_u64(path) != 0 {
            return; // host cannot exhibit the bug
        }
        let info = make_test_info();
        if !kernel_at_least(&info.kernel_version, 4, 15) {
            return; // pre-4.15 kernels still consume the knob, advice is correct
        }
        if !info.has_listen_sockets() {
            return; // the rule requires listening sockets to fire
        }
        let mut recs = Vec::new();
        eval_tcp_fack(&info, &mut recs);
        assert!(
            recs.iter().all(|r| r.param != "net.ipv4.tcp_fack"),
            "tcp_fack advice must not be issued for kernel {}: FACK was removed in Linux 4.15",
            info.kernel_version
        );
    }

    #[test]
    fn tcp_adv_win_scale_recommendation_gates_on_kernel_version() {
        // Before 6.6 the receive window is derived from the sysctl.
        let rec = tcp_adv_win_scale_recommendation(1, "5.10.134-16.an8.x86_64")
            .expect("pre-6.6 kernels still consume tcp_adv_win_scale");
        assert_eq!(rec.recommended_value, "2");
        assert_eq!(rec.current_value, "1");
        assert!(tcp_adv_win_scale_recommendation(-2, "6.5.0").is_some());
        // 6.6+ uses the per-socket scaling_ratio; the knob is obsolete.
        assert!(tcp_adv_win_scale_recommendation(1, "6.6.0").is_none());
        assert!(tcp_adv_win_scale_recommendation(1, "7.0.0-29-generic").is_none());
        // Already at or above the target on any version.
        assert!(tcp_adv_win_scale_recommendation(2, "5.10.0").is_none());
    }

    #[test]
    fn test_tcp_reordering() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_reordering(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_reordering") {
            assert_eq!(rec.recommended_value, "3");
        }
    }

    #[test]
    fn test_sched_energy_aware() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_sched_energy_aware(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.sched_energy_aware") {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_percpu_pagelist_high_fraction_large_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;
        let mut recs = Vec::new();
        eval_percpu_pagelist_high_fraction(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "vm.percpu_pagelist_high_fraction")
        {
            assert_eq!(rec.recommended_value, "8");
        }
    }

    #[test]
    fn test_percpu_pagelist_skip_small_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 16;
        let mut recs = Vec::new();
        eval_percpu_pagelist_high_fraction(&info, &mut recs);
        let rec = recs
            .iter()
            .find(|r| r.param == "vm.percpu_pagelist_high_fraction");
        assert!(
            rec.is_none(),
            "Should not recommend percpu_pagelist for small memory"
        );
    }

    #[test]
    fn test_accept_ra() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_accept_ra(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv6.conf.default.accept_ra")
        {
            assert_eq!(rec.recommended_value, "0");
            assert_eq!(rec.category, Category::Security);
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn accept_ra_reads_the_signed_devconf_value() {
        // accept_ra is an __s32 slot of struct ipv6_devconf behind a plain
        // proc_dointvec (net/ipv6/addrconf.c), and ipv6_accept_ra()
        // (include/net/ipv6.h) reads it as a truthiness test when forwarding
        // is off — so -1 means enabled. The unsigned reader parsed "-1" to
        // the disabled value 0, and the rule then stayed silent on a host
        // that accepts Router Advertisements.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_accept_ra_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("accept_ra");
        let info = make_test_info();

        for (content, expected) in [
            ("-1\n", Some("-1")),
            ("1\n", Some("1")),
            ("2\n", Some("2")),
            ("0\n", None),
        ] {
            std::fs::write(&path, content).unwrap();
            let mut recs = Vec::new();
            eval_accept_ra_at(&info, &mut recs, path.to_str().unwrap());
            let rec = recs
                .iter()
                .find(|r| r.param == "net.ipv6.conf.default.accept_ra");
            assert_eq!(
                rec.map(|r| r.current_value.as_str()),
                expected,
                "accept_ra={content}"
            );
            if let Some(rec) = rec {
                assert_eq!(rec.recommended_value, "0");
            }
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_tcp_autocorking() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_autocorking(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_autocorking") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_tcp_workaround_signed_windows() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_workaround_signed_windows(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_workaround_signed_windows")
        {
            assert_eq!(rec.recommended_value, "0");
        }
    }

    #[test]
    fn test_compact_memory() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_compact_memory(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "vm.compaction_proactiveness")
        {
            assert_eq!(rec.recommended_value, "20");
        }
    }

    #[test]
    fn test_min_slab_ratio_large_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;
        let mut recs = Vec::new();
        eval_min_slab_ratio(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "vm.min_slab_ratio") {
            assert_eq!(rec.recommended_value, "5");
        }
    }

    #[test]
    fn test_min_slab_ratio_skip_small_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 16;
        let mut recs = Vec::new();
        eval_min_slab_ratio(&info, &mut recs);
        let rec = recs.iter().find(|r| r.param == "vm.min_slab_ratio");
        assert!(
            rec.is_none(),
            "Should not recommend min_slab_ratio for small memory"
        );
    }

    #[test]
    fn min_slab_ratio_needs_node_reclaim_enabled() {
        // vm.min_slab_ratio feeds pgdat->min_slab_pages, which only
        // node_reclaim() reads, and the allocator only calls node reclaim
        // while vm.zone_reclaim_mode is non-zero (include/linux/swap.h,
        // mm/page_alloc.c). ktuner's own advice turns that mode off on
        // multi-NUMA hosts, so without the gate the rule keeps asking for a
        // write that cannot change anything.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_min_slab_ratio_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let param = dir.join("min_slab_ratio");
        std::fs::write(&param, b"3\n").unwrap();
        let mode_off = dir.join("zone_reclaim_mode.off");
        std::fs::write(&mode_off, b"0\n").unwrap();
        let mode_on = dir.join("zone_reclaim_mode.on");
        std::fs::write(&mode_on, b"1\n").unwrap();
        let mode_missing = dir.join("zone_reclaim_mode.missing");

        let mut info = make_test_info();
        info.memory_total_gb = 256;

        for mode in [&mode_off, &mode_missing] {
            let mut recs = Vec::new();
            eval_min_slab_ratio_at(
                &info,
                &mut recs,
                param.to_str().unwrap(),
                mode.to_str().unwrap(),
            );
            assert!(
                recs.is_empty(),
                "{}: node reclaim is off, the knob cannot act",
                mode.display()
            );
        }

        let mut recs = Vec::new();
        eval_min_slab_ratio_at(
            &info,
            &mut recs,
            param.to_str().unwrap(),
            mode_on.to_str().unwrap(),
        );
        assert!(
            recs.iter().any(|r| r.param == "vm.min_slab_ratio"),
            "node reclaim enabled keeps the rule"
        );

        // The memory gate still applies first.
        info.memory_total_gb = 16;
        let mut recs = Vec::new();
        eval_min_slab_ratio_at(
            &info,
            &mut recs,
            param.to_str().unwrap(),
            mode_on.to_str().unwrap(),
        );
        assert!(recs.is_empty(), "small memory still skips the rule");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_max_user_instances() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_max_user_instances(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "fs.inotify.max_user_instances")
        {
            assert_eq!(rec.recommended_value, "1024");
            assert_eq!(rec.category, Category::Performance);
        }
    }

    #[test]
    fn test_keys_maxkeys() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_keys_maxkeys(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.keys.maxkeys") {
            assert_eq!(rec.recommended_value, "2000");
        }
    }

    #[test]
    fn test_tcp_base_mss() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_base_mss(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_base_mss") {
            assert_eq!(rec.recommended_value, "1024");
        }
    }

    #[test]
    fn test_neigh_gc_stale_time() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_neigh_default_gc_stale_time(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.neigh.default.gc_stale_time")
        {
            assert_eq!(rec.recommended_value, "120");
        }
    }

    #[test]
    fn test_keys_maxbytes() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_keys_maxbytes(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.keys.maxbytes") {
            assert_eq!(rec.recommended_value, "25000");
        }
    }

    #[test]
    fn test_pipe_max_size() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_pipe_max_size(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "fs.pipe-max-size") {
            assert_eq!(rec.recommended_value, "1048576");
        }
    }

    #[test]
    fn test_shmall_target_pages_scales_with_page_size() {
        let mem_gb = 256;
        let half_bytes = mem_gb * 1024 * 1024 * 1024 / 2;

        // 4 KiB pages (x86_64): half of RAM expressed in 4K pages.
        assert_eq!(shmall_target_pages(mem_gb, 4096), half_bytes / 4096);
        // 64 KiB pages (arm64): same memory, so 16x FEWER pages. This is the
        // discriminating check — the old hardcoded-4096 code produced the 4K
        // count on every arch, over-recommending ~16x on 64K-page kernels.
        assert_eq!(shmall_target_pages(mem_gb, 65536), half_bytes / 65536);
        assert_eq!(
            shmall_target_pages(mem_gb, 4096),
            shmall_target_pages(mem_gb, 65536) * 16
        );
        // Zero page size must not divide-by-zero.
        assert_eq!(shmall_target_pages(mem_gb, 0), 0);
    }

    #[test]
    fn test_eval_shmall_recommends_using_injected_page_size() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;

        for (page_size, expected) in [(4096, "33554432"), (65536, "2097152")] {
            let mut recs = Vec::new();
            let checked = eval_shmall(
                &info,
                &mut recs,
                || page_size,
                |path| {
                    assert_eq!(path, "/proc/sys/kernel/shmall");
                    Some(1024)
                },
            );
            assert_eq!(checked, 1);
            assert_eq!(recs.len(), 1, "page_size={page_size}");
            assert_eq!(recs[0].param, "kernel.shmall");
            assert_eq!(recs[0].current_value, "1024");
            assert_eq!(recs[0].recommended_value, expected, "page_size={page_size}");
        }
    }

    #[test]
    fn test_eval_shmall_does_not_recommend_at_or_above_target() {
        let mut info = make_test_info();
        info.memory_total_gb = 256;

        for current in [2_097_152, 2_097_153] {
            let mut recs = Vec::new();
            assert_eq!(
                eval_shmall(&info, &mut recs, || 65536, |_| Some(current)),
                1
            );
            assert!(recs.is_empty(), "current={current}");
        }
    }

    #[test]
    fn test_eval_shmall_skips_missing_parameter() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_shmall(
            &info,
            &mut recs,
            || panic!("参数不存在时不应查询页大小"),
            |path| {
                assert_eq!(path, "/proc/sys/kernel/shmall");
                None
            },
        );
        assert_eq!(checked, 1);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_current_page_size_is_plausible() {
        // Whatever the host arch, the page size must be a non-zero power of two.
        let ps = current_page_size();
        assert!(ps >= 4096, "page size {ps} implausibly small");
        assert!(ps.is_power_of_two(), "page size {ps} not a power of two");
    }

    #[test]
    fn test_tcp_frto() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_frto(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_frto") {
            assert_eq!(rec.recommended_value, "2");
        }
    }

    #[test]
    fn test_icmp_ratelimit() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_icmp_ratelimit(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.icmp_ratelimit") {
            assert_eq!(rec.recommended_value, "1000");
            assert_eq!(rec.category, Category::Security);
        }
    }

    #[test]
    fn test_ip_default_ttl() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_ip_default_ttl(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.ip_default_ttl") {
            assert_eq!(rec.recommended_value, "64");
        }
    }

    #[test]
    fn test_tcp_recovery() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_recovery(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_recovery") {
            assert_eq!(rec.recommended_value, "1");
        }
    }

    #[test]
    fn test_tcp_pacing_ca_ratio() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_pacing_ca_ratio(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_pacing_ca_ratio")
        {
            assert_eq!(rec.recommended_value, "120");
        }
    }

    #[test]
    fn test_tcp_pacing_ss_ratio() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_pacing_ss_ratio(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_pacing_ss_ratio")
        {
            assert_eq!(rec.recommended_value, "200");
        }
    }

    #[test]
    fn test_tcp_comp_sack_nr() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_comp_sack_nr(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_comp_sack_nr") {
            assert_eq!(rec.recommended_value, "44");
        }
    }

    #[test]
    fn test_tcp_thin_dupack() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_thin_dupack(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_thin_dupack") {
            assert_eq!(rec.recommended_value, "1");
        }
    }

    #[test]
    fn test_tcp_invalid_ratelimit() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_invalid_ratelimit(&info, &mut recs);
        assert!(
            recs.iter()
                .all(|r| r.param != "net.ipv4.tcp_invalid_ratelimit"),
            "Should not trigger when ratelimit is already >= 500"
        );
    }

    #[test]
    fn test_tcp_init_cwnd() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_init_cwnd(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_init_cwnd") {
            assert_eq!(rec.recommended_value, "10");
            assert_eq!(rec.confidence, Confidence::High);
        }
    }

    #[test]
    fn test_sched_schedstats() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_sched_schedstats(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "kernel.sched_schedstats") {
            assert_eq!(rec.recommended_value, "0");
        }
    }

    #[test]
    fn test_inotify_max_queued_events() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_inotify_max_queued_events(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "fs.inotify.max_queued_events")
        {
            assert_eq!(rec.recommended_value, "65536");
        }
    }

    #[test]
    fn test_tcp_retrans_collapse() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_retrans_collapse(&info, &mut recs);
        if let Some(rec) = recs
            .iter()
            .find(|r| r.param == "net.ipv4.tcp_retrans_collapse")
        {
            assert_eq!(rec.recommended_value, "0");
        }
    }

    #[test]
    fn test_tcp_max_reordering() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_max_reordering(&info, &mut recs);
        let triggered = recs
            .iter()
            .any(|r| r.param == "net.ipv4.tcp_max_reordering");
        if std::path::Path::new("/proc/sys/net/ipv4/tcp_max_reordering").exists() {
            let val = read_sysctl_u64("/proc/sys/net/ipv4/tcp_max_reordering");
            if val >= 300 {
                assert!(
                    !triggered,
                    "Should not trigger when tcp_max_reordering >= 300"
                );
            }
        }
    }

    #[test]
    fn test_protected_regular() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_protected_regular(&info, &mut recs);
        if std::path::Path::new("/proc/sys/fs/protected_regular").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/fs/protected_regular");
            let triggered = recs.iter().any(|r| r.param == "fs.protected_regular");
            if val == 0 {
                assert!(triggered, "Should trigger when protected_regular=0");
                assert_eq!(recs.last().unwrap().category, Category::Security);
            } else {
                assert!(!triggered);
            }
        }
    }

    #[test]
    fn test_bpf_jit_enable() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_bpf_jit_enable(&info, &mut recs);
        if std::path::Path::new("/proc/sys/net/core/bpf_jit_enable").exists() {
            assert!(checked >= 1);
        }
    }

    #[test]
    fn test_unprivileged_bpf_recommends_the_reversible_value() {
        // 1 and 2 both deny unprivileged bpf(), but the kernel will not clear a
        // 1 for the rest of the boot ("Once set to 1, this can't be cleared"),
        // so a 1 recommendation can never be rolled back. The rule is fed the
        // value directly so the assertion holds on a host that already reports
        // 1 or 2; the literal is asserted on purpose, so the rule cannot
        // silently regress to 1.
        let mut recs = Vec::new();
        recommend_unprivileged_bpf(0, &mut recs);
        assert_eq!(recs.len(), 1, "0 must be recommended against");
        assert_eq!(
            recs[0].recommended_value, "2",
            "kernel.unprivileged_bpf_disabled must be recommended as 2: the kernel \
             cannot clear a 1, which would make the recommendation irreversible"
        );
        assert_eq!(recs[0].current_value, "0");
        assert_eq!(recs[0].category, Category::Security);
        assert_eq!(recs[0].confidence, Confidence::High);
        assert!(recs[0].writable);

        // Already denied: nothing to recommend, whichever way it was set.
        for current in [1u64, 2] {
            let mut recs = Vec::new();
            recommend_unprivileged_bpf(current, &mut recs);
            assert!(
                recs.is_empty(),
                "unprivileged bpf is already denied by {current}, so there is \
                 nothing to change"
            );
        }
    }

    #[test]
    fn test_bpf_jit_harden() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_bpf_jit_harden(&info, &mut recs);
        assert_eq!(checked, 1);
        let triggered = recs.iter().any(|r| r.param == "net.core.bpf_jit_harden");
        // The sysctl is 0600 root-owned and its handler needs CAP_SYS_ADMIN,
        // so an unprivileged run cannot read it at all. Distinguish a
        // readable 0 (a finding) from a failed read (no value, no finding) —
        // read_sysctl_u64 maps the failure to 0 and cannot tell them apart.
        match std::fs::read_to_string("/proc/sys/net/core/bpf_jit_harden") {
            Ok(raw) => {
                assert_eq!(triggered, raw.trim() == "0", "readable value {raw:?}");
                if triggered {
                    assert_eq!(recs.last().unwrap().category, Category::Security);
                }
            }
            Err(_) => assert!(
                !triggered,
                "an unreadable sysctl is not the value 0: {recs:?}"
            ),
        }
    }

    #[test]
    fn bpf_jit_harden_skips_an_unreadable_sysctl() {
        // A directory is the portable stand-in for "exists but cannot be
        // read" (EISDIR), which works for root and unprivileged runs alike.
        let mut recs = Vec::new();
        assert_eq!(eval_bpf_jit_harden_at("/", &mut recs), 1);
        assert!(
            recs.is_empty(),
            "a failed read must not report a hardening gap: {recs:?}"
        );
    }

    #[test]
    fn bpf_jit_harden_reads_real_values() {
        let dir = std::env::temp_dir().join(format!("ktuner-bpf-harden-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("bpf_jit_harden");
        let path = path.to_str().expect("utf-8 temp path");

        std::fs::write(path, "0\n").expect("write fixture");
        let mut recs = Vec::new();
        eval_bpf_jit_harden_at(path, &mut recs);
        assert_eq!(recs.len(), 1, "a readable 0 is still a finding");
        assert_eq!(recs[0].param, "net.core.bpf_jit_harden");
        assert_eq!(recs[0].current_value, "0");
        assert_eq!(recs[0].recommended_value, "1");

        std::fs::write(path, "1\n").expect("write fixture");
        let mut recs = Vec::new();
        eval_bpf_jit_harden_at(path, &mut recs);
        assert!(recs.is_empty(), "a hardened host has no finding");

        // Garbage is a failed parse, not a value either.
        std::fs::write(path, "not-a-number\n").expect("write fixture");
        let mut recs = Vec::new();
        eval_bpf_jit_harden_at(path, &mut recs);
        assert!(recs.is_empty(), "unparseable content is not the value 0");
        std::fs::remove_file(path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn test_promote_secondaries() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_promote_secondaries(&info, &mut recs);
        if std::path::Path::new("/proc/sys/net/ipv4/conf/default/promote_secondaries").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/net/ipv4/conf/default/promote_secondaries");
            let triggered = recs.iter().any(|r| r.param.contains("promote_secondaries"));
            if val == 0 {
                assert!(triggered, "Should trigger when promote_secondaries=0");
            }
        }
    }

    #[test]
    fn test_unres_qlen_bytes_10g() {
        let mut info = make_test_info();
        info.network = vec![crate::detect::NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let mut recs = Vec::new();
        eval_unres_qlen_bytes(&info, &mut recs);
        if std::path::Path::new("/proc/sys/net/ipv4/neigh/default/unres_qlen_bytes").exists() {
            let val = read_sysctl_u64("/proc/sys/net/ipv4/neigh/default/unres_qlen_bytes");
            if val < 131072 {
                assert!(recs.iter().any(|r| r.param.contains("unres_qlen_bytes")));
            }
        }
    }

    #[test]
    fn test_core_mem_max_reasons_name_the_explicit_buffer_cap() {
        // net.core.rmem_max/wmem_max bound an explicit setsockopt(SO_RCVBUF /
        // SO_SNDBUF) request (sock_setsockopt clamps it with sysctl_*mem_max),
        // while TCP's automatic tuning is capped by tcp_rmem[2] / tcp_wmem[2]
        // (tcp_rcv_space_adjust / tcp_sndbuf_expand). The core maxima cannot
        // unlock the triples, so the reason must not claim that they do.
        let mut info = make_test_info();
        info.network = vec![crate::detect::NetInfo {
            name: "eth0".to_string(),
            speed_mbps: 10000,
        }];
        let dir = std::env::temp_dir().join(format!(
            "ktuner_core_mem_max_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let rmem = dir.join("rmem_max");
        let wmem = dir.join("wmem_max");
        std::fs::write(&rmem, "212992\n").unwrap();
        std::fs::write(&wmem, "212992\n").unwrap();

        let mut recs = Vec::new();
        eval_rmem_max_at(&info, &mut recs, rmem.to_str().unwrap());
        eval_wmem_max_at(&info, &mut recs, wmem.to_str().unwrap());
        let r = recs
            .iter()
            .find(|r| r.param == "net.core.rmem_max")
            .expect("a 10G host under 16MB gets the recommendation");
        let w = recs
            .iter()
            .find(|r| r.param == "net.core.wmem_max")
            .expect("a 10G host under 16MB gets the recommendation");
        assert!(r.reason.contains("SO_RCVBUF"), "{}", r.reason);
        assert!(w.reason.contains("SO_SNDBUF"), "{}", w.reason);
        assert!(
            !r.reason.contains("tcp_rmem 的 max 值不会生效"),
            "{}",
            r.reason
        );
        assert!(
            !w.reason.contains("tcp_wmem 的 max 值不会生效"),
            "{}",
            w.reason
        );
    }

    #[test]
    fn test_ip_nonlocal_bind() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_ip_nonlocal_bind(&info, &mut recs);
        if std::path::Path::new("/proc/sys/net/ipv4/ip_nonlocal_bind").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/net/ipv4/ip_nonlocal_bind");
            if val == 1 {
                assert!(recs.iter().any(|r| r.param == "net.ipv4.ip_nonlocal_bind"));
                assert_eq!(recs.last().unwrap().category, Category::Security);
            }
        }
    }

    #[test]
    fn test_conntrack_tcp_timeout_established() {
        // Host-dependent path check only; the kernel-writability of
        // current_value is pinned unconditionally by
        // test_current_values_stay_kernel_writable through the extracted
        // value-driven core.
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_conntrack_tcp_timeout_established(&info, &mut recs);
        if std::path::Path::new("/proc/sys/net/netfilter/nf_conntrack_tcp_timeout_established")
            .exists()
        {
            assert!(checked >= 1);
        }
    }

    #[test]
    fn test_softlockup_all_cpu_backtrace_large_cpu() {
        let mut info = make_test_info();
        info.cpu_cores = 96;
        let mut recs = Vec::new();
        let checked = eval_softlockup_all_cpu_backtrace(&info, &mut recs);
        if std::path::Path::new("/proc/sys/kernel/softlockup_all_cpu_backtrace").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/kernel/softlockup_all_cpu_backtrace");
            if val == 0 {
                assert!(recs
                    .iter()
                    .any(|r| r.param == "kernel.softlockup_all_cpu_backtrace"));
            }
        }
    }

    #[test]
    fn test_compact_unevictable_large_mem() {
        let mut info = make_test_info();
        info.memory_total_gb = 128;
        let mut recs = Vec::new();
        let checked = eval_compact_unevictable(&info, &mut recs);
        if std::path::Path::new("/proc/sys/vm/compact_unevictable_allowed").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/vm/compact_unevictable_allowed");
            if val == 0 {
                assert!(recs
                    .iter()
                    .any(|r| r.param == "vm.compact_unevictable_allowed"));
            }
        }
    }

    #[test]
    fn test_perf_cpu_time_max_percent() {
        let mut info = make_test_info();
        info.cpu_cores = 64;
        let mut recs = Vec::new();
        let checked = eval_perf_cpu_time_max_percent(&info, &mut recs);
        if std::path::Path::new("/proc/sys/kernel/perf_cpu_time_max_percent").exists() {
            assert_eq!(checked, 1);
        }
    }

    #[test]
    fn test_perf_event_paranoid_minus_one_reads_signed() {
        // -1 is the kernel's "all users may use perf events" value, common on
        // profiling fleets. The unsigned reader parsed "-1" to 0, so the
        // recommendation reported current 0 and the rollback ledger later
        // restored 0 instead of -1 — a silent wrong "Full" restore.
        let path = std::env::temp_dir().join(format!(
            "ktuner_perf_event_paranoid_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"-1\n").unwrap();
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_perf_event_paranoid_at(&info, &mut recs, path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert_eq!(checked, 1);
        assert_eq!(recs.len(), 1, "-1 is looser than 2, so it must be flagged");
        assert_eq!(recs[0].param, "kernel.perf_event_paranoid");
        assert_eq!(recs[0].current_value, "-1", "current must be faithful");
        assert_eq!(recs[0].recommended_value, "2");
    }

    #[test]
    fn test_perf_event_paranoid_boundaries() {
        let info = make_test_info();
        for (value, expects_rec) in [
            (-2, true),
            (-1, true),
            (0, true),
            (1, true),
            (2, false),
            (3, false),
        ] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_perf_event_paranoid_bound_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_perf_event_paranoid_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: below 2 must be flagged, 2 and above are already hardened"
            );
            if expects_rec {
                assert_eq!(
                    recs[0].current_value,
                    value.to_string(),
                    "current_value must echo the signed value verbatim"
                );
            }
        }
    }

    #[test]
    fn test_perf_event_paranoid_absent_counts_as_checked() {
        // A path that never exists exercises the absent branch: 1 checked,
        // 0 recommendations, no filesystem dependency in CI.
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked =
            eval_perf_event_paranoid_at(&info, &mut recs, "/proc/sys/kernel/ktuner_absent_pep");
        assert_eq!(checked, 1);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_sysrq_minus_one_reads_signed() {
        // -1 is the kernel's "every sysrq function enabled" mask (sysrq_mask()
        // with all bits set). The unsigned reader parsed "-1" to Err, fell
        // back to 0 — the *disabled* value — so the gate skipped the
        // hardening recommendation on exactly the maximally-open host, and
        // current_value lied about what a rollback would restore.
        let path = std::env::temp_dir().join(format!(
            "ktuner_sysrq_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"-1\n").unwrap();
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_sysrq_at(&info, &mut recs, path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert_eq!(checked, 1);
        assert_eq!(recs.len(), 1, "-1 is fully open, so it must be flagged");
        assert_eq!(recs[0].param, "kernel.sysrq");
        assert_eq!(recs[0].current_value, "-1", "current must be faithful");
        assert_eq!(recs[0].recommended_value, "176");
    }

    #[test]
    fn test_sysrq_boundaries() {
        // 0 (sysrq fully disabled) and 176 (the safe subset itself:
        // sync + remount-ro + reboot) are already hardened and must NOT be
        // flagged; every other value — including the signed -1 mask and the
        // common 1/16/438 masks — must recommend 176.
        let info = make_test_info();
        for (value, expects_rec) in [
            (-1, true),
            (0, false),
            (1, true),
            (16, true),
            (176, false),
            (438, true),
        ] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_sysrq_bound_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_sysrq_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 (disabled) and 176 (safe subset) are hardened"
            );
            if expects_rec {
                assert_eq!(
                    recs[0].current_value,
                    value.to_string(),
                    "current_value must echo the signed value verbatim"
                );
                assert_eq!(recs[0].recommended_value, "176");
            }
        }
    }

    #[test]
    fn test_sysrq_absent_counts_as_checked() {
        // A path that never exists exercises the absent branch: 1 checked,
        // 0 recommendations, no filesystem dependency in CI.
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_sysrq_at(&info, &mut recs, "/proc/sys/kernel/ktuner_absent_sysrq");
        assert_eq!(checked, 1);
        assert!(recs.is_empty());
    }

    #[test]
    fn test_panic_minus_one_reads_signed() {
        // `panic=-1` is the kernel's documented "reboot immediately, without
        // syncing" setting (kernel-parameters.txt; the sysctl is a plain
        // proc_dointvec int with no bounds). The unsigned reader parsed "-1"
        // to Err, fell back to 0 — the *never-reboot* value — so the rule
        // fired on exactly the most crash-resilient hosts and the
        // current_value it recorded, "0", is what a rollback would restore.
        let path = std::env::temp_dir().join(format!(
            "ktuner_panic_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, b"-1\n").unwrap();
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_panic_at(&info, &mut recs, path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert_eq!(checked, 1);
        assert!(
            recs.is_empty(),
            "-1 reboots immediately, so the host needs no recommendation: {recs:?}"
        );
    }

    #[test]
    fn test_panic_boundaries() {
        // 0 (loop forever after a panic) is the only unhardened value; a
        // positive timeout and the immediate-reboot -1 are already reboot
        // policies and must not be flagged.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (10, false), (60, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_panic_bound_{}_{:?}_{}",
                std::process::id(),
                std::thread::current().id(),
                value
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_panic_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 (never reboot) is unhardened"
            );
        }
    }

    #[test]
    fn test_panic_on_oops_reads_truthiness_signed() {
        // kernel.panic_on_oops is a plain proc_dointvec int with no min/max
        // (kernel/sysctl.c), consumed as a boolean by
        // arch/x86/kernel/dumpstack.c ("if (panic_on_oops) panic(...)"): any
        // nonzero value, -1 included, is enabled. The unsigned reader parsed
        // "-1" to Err, fell back to 0 — the *not-enabled* value — so the
        // `== 0` gate invented the recommendation on a host that already
        // panics on oops.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_panic_on_oops_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_panic_on_oops_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 is unhardened; any nonzero value is enabled"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "kernel.panic_on_oops");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_core_uses_pid_reads_truthiness_signed() {
        // kernel.core_uses_pid is a plain proc_dointvec int with no min/max
        // (fs/coredump.c), consumed by the same file as
        // "if (!ispipe && !pid_in_pattern && core_uses_pid)". -1 is legal and
        // enabled, so the unsigned reader's fallback 0 made the `== 0` gate
        // report a PID-less core pattern on a host that appends the PID.
        let info = make_test_info();
        // A file pattern with no %p of its own is where the knob still acts,
        // so the truthiness cases keep deciding here.
        let pattern = std::env::temp_dir().join(format!(
            "ktuner_core_pattern_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&pattern, "core\n").unwrap();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_core_uses_pid_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_core_uses_pid_at(
                &info,
                &mut recs,
                path.to_str().unwrap(),
                pattern.to_str().unwrap(),
            );
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 leaves core files unnamed"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "kernel.core_uses_pid");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
        std::fs::remove_file(&pattern).ok();
    }

    #[test]
    fn core_uses_pid_needs_a_core_pattern_that_leaves_it_something_to_do() {
        // fs/coredump.c appends the compatibility `.PID` only under
        // "if (!ispipe && !pid_in_pattern && core_uses_pid)": a piped pattern
        // (systemd-coredump) never reaches the filename logic, and a pattern
        // carrying its own %p already records the pid. On those hosts the
        // reason's promise is already met, so the rule must stay quiet.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_core_uses_pid_pattern_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, content: &str| {
            let path = dir.join(name);
            std::fs::write(&path, content).unwrap();
            path
        };
        let param = write("core_uses_pid", "0\n");
        let plain = write("pattern-plain", "core\n");
        // A literal %p written with an escape: the kernel emits "%p" as text
        // and still appends .PID, so the knob keeps its job.
        let escaped = write("pattern-escaped", "core.%%p\n");
        let with_pid = write("pattern-pid", "core.%p\n");
        let piped = write(
            "pattern-pipe",
            "|/usr/lib/systemd/systemd-coredump %P %u %g %s %t %c %h\n",
        );
        let missing = dir.join("pattern-missing");

        let info = make_test_info();

        for pattern in [&with_pid, &piped] {
            let mut recs = Vec::new();
            eval_core_uses_pid_at(
                &info,
                &mut recs,
                param.to_str().unwrap(),
                pattern.to_str().unwrap(),
            );
            assert!(
                recs.is_empty(),
                "{}: the kernel already records the pid",
                pattern.display()
            );
        }

        for pattern in [&plain, &escaped, &missing] {
            let mut recs = Vec::new();
            eval_core_uses_pid_at(
                &info,
                &mut recs,
                param.to_str().unwrap(),
                pattern.to_str().unwrap(),
            );
            assert!(
                recs.iter().any(|r| r.param == "kernel.core_uses_pid"),
                "{}: the knob still names the core file",
                pattern.display()
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_oom_kill_allocating_task_reads_truthiness_signed() {
        // vm.oom_kill_allocating_task is a plain proc_dointvec int with no
        // min/max (mm/oom_kill.c), consumed by the same file as
        // "if (!is_memcg_oom(oc) && sysctl_oom_kill_allocating_task && ...)".
        // -1 is legal and enabled, so the unsigned reader's fallback 0 made
        // the `== 0` gate report the opposite OOM policy.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_oom_kill_allocating_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked =
                eval_oom_kill_allocating_task_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 selects a victim from the task list"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "vm.oom_kill_allocating_task");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_oom_dump_tasks_reads_truthiness_signed() {
        // vm.oom_dump_tasks is a plain proc_dointvec int with no min/max
        // (mm/oom_kill.c), consumed by the same file as
        // "if (sysctl_oom_dump_tasks)". -1 is legal and enabled, so the
        // unsigned reader's fallback 0 made the `== 0` gate claim the OOM
        // report is missing on a host that dumps it.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_oom_dump_tasks_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_oom_dump_tasks_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 suppresses the OOM task dump"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "vm.oom_dump_tasks");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_log_martians_reads_truthiness_signed() {
        // net/ipv4/devinet.c's devinet_conf_proc routes this entry through a
        // plain proc_dointvec on an int slot (no min/max), and
        // IN_DEV_LOG_MARTIANS (include/linux/inetdevice.h) reads it through
        // IN_DEV_ORCONF, a truthiness test consumed by net/ipv4/route.c. Any
        // nonzero value is enabled, so -1 must not read as the value 0.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_log_martians_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_log_martians_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 leaves martians unlogged"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.conf.all.log_martians");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_default_log_martians_reads_truthiness_signed() {
        // Same devinet_conf_proc slot as conf/all, inherited by new
        // interfaces; -1 is legal and enabled.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_default_log_martians_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_default_log_martians_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 leaves new interfaces blind to martians"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.conf.default.log_martians");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_promote_secondaries_reads_truthiness_signed() {
        // devinet_conf_proc int slot; IN_DEV_PROMOTE_SECONDARIES is an
        // IN_DEV_ORCONF truthiness test used as `int do_promote = ...`.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_promote_secondaries_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_promote_secondaries_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 drops secondary addresses"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.conf.default.promote_secondaries");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_secure_redirects_reads_truthiness_signed() {
        // The gate is `!= 0`, so the unsigned reader's collapse of -1 to 0
        // silently skipped the rule on a host whose secure redirects are on
        // (IN_DEV_SEC_REDIRECTS is an ORCONF truthiness test).
        let info = make_test_info();
        let conf = std::env::temp_dir().join(format!(
            "ktuner_secure_redirects_conf_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&conf);
        for relative in ["all/shared_media", "eth0/shared_media"] {
            let path = conf.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
            // Shared media off so the shared_media gate keeps the knob
            // reachable and this test still observes the signed read.
            std::fs::write(&path, "0\n").unwrap();
        }
        for (value, expects_rec) in [(-1, true), (0, false), (1, true)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_secure_redirects_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_secure_redirects_at(&info, &mut recs, path.to_str().unwrap(), &conf);
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: any nonzero value accepts secure redirects"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.conf.all.secure_redirects");
                assert_eq!(
                    recs[0].current_value,
                    value.to_string(),
                    "current_value must echo the signed value verbatim"
                );
                assert_eq!(recs[0].recommended_value, "0");
            }
        }
    }

    #[test]
    fn secure_redirects_advice_needs_shared_media_off() {
        // __ip_do_redirect() (net/ipv4/route.c) reads IN_DEV_SEC_REDIRECTS only
        // inside its `if (!IN_DEV_SHARED_MEDIA(in_dev))` branch, and the sysctl
        // documentation states the override outright — "shared_media ...
        // Overrides secure_redirects" / "Overridden by shared_media". Shared
        // media defaults to on and IN_DEV_SHARED_MEDIA is an OR of conf/all
        // and the device value, so the default host never reaches the branch
        // and writing secure_redirects cannot change how a redirect is judged.
        let info = make_test_info();
        let dir = std::env::temp_dir().join(format!(
            "ktuner_secure_redirects_media_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |relative: &str, content: &str| {
            let path = dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent")).unwrap();
            std::fs::write(&path, content).unwrap();
            path
        };
        let knob = write("all/secure_redirects", "1\n");
        for interface in ["default", "lo", "eth0"] {
            write(&format!("{interface}/shared_media"), "0\n");
        }

        // conf/all keeps shared media on for every interface (ORCONF), so the
        // per-device zero cannot make secure_redirects reachable.
        write("all/shared_media", "1\n");
        let mut recs = Vec::new();
        eval_secure_redirects_at(&info, &mut recs, knob.to_str().unwrap(), &dir);
        assert!(
            recs.is_empty(),
            "no redirect is judged by secure_redirects while shared media is on"
        );

        // Both halves off on one interface: the kernel reads the knob again.
        write("all/shared_media", "0\n");
        let mut recs = Vec::new();
        eval_secure_redirects_at(&info, &mut recs, knob.to_str().unwrap(), &dir);
        assert_eq!(
            recs.len(),
            1,
            "one interface with shared media off makes the write reachable"
        );
        assert_eq!(recs[0].param, "net.ipv4.conf.all.secure_redirects");

        // Every device back on the default leaves nothing reading the knob,
        // even with conf/all off.
        for interface in ["default", "lo", "eth0"] {
            write(&format!("{interface}/shared_media"), "1\n");
        }
        let mut recs = Vec::new();
        eval_secure_redirects_at(&info, &mut recs, knob.to_str().unwrap(), &dir);
        assert!(
            recs.is_empty(),
            "no interface reads secure_redirects once shared media is back on"
        );
    }

    #[test]
    fn test_arp_filter_reads_truthiness_signed() {
        // devinet_conf_proc int slot; IN_DEV_ARPFILTER is an ORCONF
        // truthiness test consumed by net/ipv4/arp.c. The multi-NIC gate
        // needs two interfaces and no bond on the host, so the 0 case is
        // asserted only when that gate lets the rule run.
        let mut info = make_test_info();
        info.network = vec![
            NetInfo {
                name: "eth0".to_string(),
                speed_mbps: 10000,
            },
            NetInfo {
                name: "eth1".to_string(),
                speed_mbps: 10000,
            },
        ];
        let bonded = has_bond();
        for (value, expects_rec) in [(-1, false), (0, !bonded), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_arp_filter_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked = eval_arp_filter_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked, 1);
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 leaves multi-NIC ARP unfiltered"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.conf.all.arp_filter");
                assert_eq!(recs[0].current_value, "0");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn tcp_option_bools_read_truthiness_signed() {
        // tcp_window_scaling, tcp_sack and tcp_timestamps were all plain
        // proc_dointvec ints with no min/max through v5.10 (net/ipv4/
        // sysctl_net_ipv4.c), so "-1" is a legal, persistent value, and
        // every kernel consumer is a truthiness test (tcp_output.c / tcp_input.c
        // arm the window-scale, SACK-perm and timestamp SYN options with a
        // bare READ_ONCE). The unsigned reader parsed "-1" to Err and fell
        // back to 0, the *disabled* value, so all three `== 0` gates
        // invented a finding on hosts that already run the option.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false), (2, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_tcp_opt_bool_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            let checked_ws = eval_tcp_window_scaling_at(&info, &mut recs, path.to_str().unwrap());
            let mut sack_recs = Vec::new();
            let checked_sack = eval_tcp_sack_at(&info, &mut sack_recs, path.to_str().unwrap());
            let mut ts_recs = Vec::new();
            let checked_ts = eval_tcp_timestamps_at(&info, &mut ts_recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(checked_ws, 1);
            assert_eq!(checked_sack, 1);
            assert_eq!(checked_ts, 1);
            for (label, recs) in [
                ("window_scaling", &recs),
                ("sack", &sack_recs),
                ("timestamps", &ts_recs),
            ] {
                assert_eq!(
                    recs.len(),
                    usize::from(expects_rec),
                    "{label} value {value}: only 0 disables the option"
                );
            }
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.tcp_window_scaling");
                assert_eq!(sack_recs[0].param, "net.ipv4.tcp_sack");
                assert_eq!(ts_recs[0].param, "net.ipv4.tcp_timestamps");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn tcp_rfc1337_reads_truthiness_signed() {
        // tcp_rfc1337 was a plain proc_dointvec int through v5.10, and
        // tcp_minisocks.c consumes it as `if (!READ_ONCE(...sysctl_tcp_
        // rfc1337))` — letting a forged RST kill the TIME_WAIT socket only
        // when the knob is 0. The unsigned reader parsed "-1" to the
        // fallback 0, so the `== 0` gate invented a finding on a host that
        // already keeps its TIME_WAIT sockets.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_rfc1337_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_tcp_rfc1337_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 exposes TIME_WAIT to forged RSTs"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.tcp_rfc1337");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn tcp_mtu_probing_reads_truthiness_signed() {
        // tcp_mtu_probing was a plain proc_dointvec int through v5.10, and
        // the black-hole detector consumes it as a truthiness test
        // (net/ipv4/tcp_timer.c: `if (!READ_ONCE(...sysctl_tcp_mtu_probing))`
        // skips the probe) — only the always-probe mode is the documented
        // `> 1` check in tcp_output.c. The unsigned reader parsed "-1" to
        // the fallback 0, so the `== 0` gate invented a finding on a host
        // whose black-hole probing is already armed.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false), (2, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_mtu_probing_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_tcp_mtu_probing_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 leaves the black-hole detector off"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.tcp_mtu_probing");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn icmp_echo_ignore_broadcasts_reads_truthiness_signed() {
        // icmp_echo_ignore_broadcasts was a plain proc_dointvec int through
        // v5.10, and net/ipv4/icmp.c consumes it as a truthiness test that
        // guards the broadcast answer. The unsigned reader parsed "-1" to
        // the fallback 0, so the `== 0` gate invented a Smurf finding on a
        // host that already ignores broadcast echoes.
        let info = make_test_info();
        for (value, expects_rec) in [(-1, false), (0, true), (1, false)] {
            let path = std::env::temp_dir().join(format!(
                "ktuner_icmp_bcast_{}_{:?}_{value}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::write(&path, format!("{value}\n")).unwrap();
            let mut recs = Vec::new();
            eval_icmp_echo_ignore_broadcasts_at(&info, &mut recs, path.to_str().unwrap());
            std::fs::remove_file(&path).ok();
            assert_eq!(
                recs.len(),
                usize::from(expects_rec),
                "value {value}: only 0 answers broadcast echoes"
            );
            if expects_rec {
                assert_eq!(recs[0].param, "net.ipv4.icmp_echo_ignore_broadcasts");
                assert_eq!(recs[0].recommended_value, "1");
            }
        }
    }

    #[test]
    fn test_hung_task_warnings() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_hung_task_warnings(&info, &mut recs);
        if std::path::Path::new("/proc/sys/kernel/hung_task_warnings").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/kernel/hung_task_warnings");
            if val == 0 {
                assert!(recs.iter().any(|r| r.param == "kernel.hung_task_warnings"));
            }
        }
    }

    #[test]
    fn test_overcommit_ratio_skip_without_mode2() {
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_overcommit_ratio(&info, &mut recs);
        let oc = read_sysctl_u64("/proc/sys/vm/overcommit_memory");
        if oc != 2 {
            assert!(recs.is_empty());
        }
    }

    #[test]
    fn hung_task_warnings_unlimited_is_not_disabled() {
        // kernel/hung_task.c registers the sysctl with a range of
        // [-1, INT_MAX]; -1 means unlimited warnings, 0 disables them. The
        // unsigned reader collapsed -1 to 0 and recommended "fixing" it to 10.
        let mut recs = Vec::new();
        hung_task_warnings_recommendation(-1, &mut recs);
        assert!(recs.is_empty(), "-1 means unlimited, not disabled");

        hung_task_warnings_recommendation(10, &mut recs);
        assert!(recs.is_empty(), "a positive budget needs no change");

        hung_task_warnings_recommendation(0, &mut recs);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].param, "kernel.hung_task_warnings");
        assert_eq!(recs[0].recommended_value, "10");
        assert_eq!(recs[0].current_value, "0");
    }

    #[test]
    fn test_absent_param_counts_as_checked() {
        // Each eval_* function should return 1 even when the param does not
        // exist, so that total_checked reflects all rules attempted. The probe
        // path lives under /proc/sys, which is never user-writable, so it is
        // guaranteed absent on every host — the absent branch is exercised
        // deterministically regardless of kernel version.
        let path = "/proc/sys/kernel/ktuner_test_absent_probe";
        assert!(!std::path::Path::new(path).exists());
        let info = make_test_info();
        let mut recs = Vec::new();
        let count = eval_sched_min_granularity_at(&info, &mut recs, path);
        assert_eq!(
            count, 1,
            "eval_* must return 1 regardless of param presence"
        );
        assert!(
            recs.is_empty(),
            "absent param must not emit a recommendation"
        );
    }
}
