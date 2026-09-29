use crate::detect::{read_sysctl_u64, DiskType, SystemInfo};
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

impl EvalResult {
    pub fn score(&self) -> usize {
        if self.recommendations.is_empty() {
            return 100;
        }
        let penalty: usize = self
            .recommendations
            .iter()
            .map(|r| match r.confidence {
                Confidence::High => 3,
                Confidence::Medium => 2,
            })
            .sum();
        100usize.saturating_sub(penalty).max(30)
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

fn eval_io_scheduler(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let mut count = 0;
    for disk in &info.disks {
        match disk.disk_type {
            DiskType::NVMe => {
                count += 1;
                let target = if disk.available_schedulers.contains(&"none".to_string()) {
                    "none"
                } else if disk.available_schedulers.contains(&"noop".to_string()) {
                    "noop"
                } else {
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
    let is_latency_sensitive = info.has_process("redis-server")
        || info.has_process("memcached")
        || info.has_process("postgres")
        || info.has_process("mysqld")
        || info.has_process("clickhouse");

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

    if info.sysctl.dirty_background_ratio > target_bg {
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
    if !info.param_exists("/proc/sys/net/ipv4/tcp_fastopen") {
        return 1;
    }

    if info.has_listen_sockets() && info.sysctl.tcp_fastopen < 3 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_fastopen".to_string(),
            current_value: info.sysctl.tcp_fastopen.to_string(),
            recommended_value: "3".to_string(),
            reason: "启用 TCP Fast Open（客户端+服务端）减少连接建立延迟".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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
    let path = "/proc/sys/kernel/sysrq";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/tcp_max_syn_backlog";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current < 8192 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_max_syn_backlog".to_string(),
            current_value: current.to_string(),
            recommended_value: "65536".to_string(),
            reason: "增大半连接队列，避免突发连接请求时 SYN 被丢弃".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_rmem_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/rmem_max";
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
            reason: "rmem_max 是 TCP 接收缓冲区的硬上限，不调大它 tcp_rmem 的 max 值不会生效"
                .to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_wmem_max(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/wmem_max";
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
            reason: "wmem_max 是 TCP 发送缓冲区的硬上限，不调大它 tcp_wmem 的 max 值不会生效"
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
        recs.push(Recommendation {
            param: "net.ipv4.tcp_rmem".to_string(),
            current_value: content,
            recommended_value: "4096 131072 16777216".to_string(),
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
        recs.push(Recommendation {
            param: "net.ipv4.tcp_wmem".to_string(),
            current_value: content,
            recommended_value: "4096 65536 16777216".to_string(),
            reason: "万兆网卡场景下增大 TCP 发送缓冲区上限，避免大流量传输时发送端瓶颈".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_tcp_slow_start_after_idle(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_slow_start_after_idle";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_slow_start_after_idle".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "长连接空闲后重新慢启动会造成突发延迟，禁用后保持已探测的拥塞窗口".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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
    if parts.len() == 2 && parts[1] - parts[0] < 30000 {
        recs.push(Recommendation {
            param: "net.ipv4.ip_local_port_range".to_string(),
            current_value: content,
            recommended_value: "1024 65535".to_string(),
            reason: "可用临时端口范围过小，高并发短连接场景下可能耗尽端口导致连接失败".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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
    let ko_path = format!(
        "/lib/modules/{}/kernel/net/sched/{}.ko",
        uname.trim(),
        module
    );
    std::path::Path::new(&ko_path).exists()
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
            if available.contains("bbr") {
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
    let path = "/proc/sys/net/ipv4/tcp_no_metrics_save";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/tcp_mtu_probing";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
        if disk.disk_type == DiskType::NVMe && disk.nr_requests < 256 {
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
    let path = "/proc/sys/kernel/numa_balancing";
    if !info.param_exists(path) {
        return 1;
    }
    if info.numa_nodes <= 1 {
        return 1;
    }
    let is_db = info.has_process("postgres")
        || info.has_process("mysqld")
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
    let path = "/proc/sys/kernel/sched_migration_cost_ns";
    if !info.param_exists(path) {
        return 1;
    }
    if info.cpu_cores <= 16 {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/tcp_syncookies";
    if !info.param_exists(path) {
        return 1;
    }
    if !info.has_listen_sockets() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/conf/all/send_redirects";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.all.send_redirects".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器不应发送 ICMP 重定向，避免被利用进行网络拓扑探测或路由劫持".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_perf_event_paranoid(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/perf_event_paranoid";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/kernel/panic";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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

/// Whether the host uses link bonding. The ARP-tuning rules key off "2+ NICs",
/// but bond members are multiple NICs forming ONE logical link where settings
/// like arp_filter/arp_ignore can break the bond, so they must skip bonded hosts.
fn has_bond() -> bool {
    if std::path::Path::new("/proc/net/bonding").is_dir() {
        return true;
    }
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for e in entries.filter_map(|e| e.ok()) {
            if e.file_name().to_string_lossy().starts_with("bond") {
                return true;
            }
        }
    }
    false
}

fn eval_ip_forward(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/ip_forward";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 && !host_needs_ip_forward(info) {
        recs.push(Recommendation {
            param: "net.ipv4.ip_forward".to_string(),
            current_value: "1".to_string(),
            recommended_value: "0".to_string(),
            reason: "未检测到容器/虚拟化/VPN/路由用途，关闭 IP 转发可防止被用作中间人或跳板（若本机需转发请忽略）".to_string(),
            confidence: Confidence::Medium,
            category: Category::Security, writable: true,
        });
    }
    1
}

fn eval_unprivileged_bpf(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/unprivileged_bpf_disabled";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "kernel.unprivileged_bpf_disabled".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "非特权用户可加载 BPF 程序存在提权风险，应禁止".to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
}

fn eval_core_uses_pid(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/core_uses_pid";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/conf/all/log_martians";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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

fn eval_nr_hugepages(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/nr_hugepages";
    if !info.param_exists(path) {
        return 1;
    }
    let needs_hugepages = info.has_process("postgres") || info.has_process("mysqld");
    if !needs_hugepages {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 && info.memory_total_gb >= 16 {
        let recommended = info.memory_total_gb * 1024 / 4 / 2;
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
    let path = "/proc/sys/kernel/shmmax";
    if !info.param_exists(path) {
        return 1;
    }
    let needs_shm = info.has_process("postgres") || info.has_process("mysqld");
    if !needs_shm {
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
    let path = "/proc/sys/net/ipv4/tcp_timestamps";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/tcp_window_scaling";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/net/ipv4/tcp_sack";
    if !info.param_exists(path) {
        return 1;
    }
    let current = read_sysctl_u64(path);
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

fn eval_oom_kill_allocating_task(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/oom_kill_allocating_task";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let path = "/proc/sys/vm/dirty_background_ratio";
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
    if current > target as u64 {
        let has_db = info.has_process("postgres")
            || info.has_process("mysqld")
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

fn eval_icmp_echo_ignore_broadcasts(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/icmp_echo_ignore_broadcasts";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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

fn eval_tcp_rfc1337(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/tcp_rfc1337";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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

fn eval_secure_redirects(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/all/secure_redirects";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let current = read_sysctl_u64(path);
    if current <= 100 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_challenge_ack_limit".to_string(),
            current_value: current.to_string(),
            recommended_value: "999999999".to_string(),
            reason:
                "默认值 100 存在 CVE-2016-5696 边信道攻击风险，攻击者可推断 TCP 连接状态并注入数据"
                    .to_string(),
            confidence: Confidence::High,
            category: Category::Security,
            writable: true,
        });
    }
    1
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
    if current == 0 {
        let recommended = 256 * 1024 * 1024; // 256MB
        recs.push(Recommendation {
            param: "vm.dirty_bytes".to_string(),
            current_value: "0".to_string(),
            recommended_value: recommended.to_string(),
            reason: format!("大内存服务器 ({} GB) 使用 dirty_ratio 百分比会导致脏页过多、IO 突刺，改用固定字节限制更平稳", info.memory_total_gb),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_sched_child_runs_first(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/sched_child_runs_first";
    if !info.param_exists(path) {
        return 1;
    }
    let has_server = info.has_process("nginx")
        || info.has_process("httpd")
        || info.has_process("postgres")
        || info.has_process("mysqld");
    if !has_server {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "kernel.sched_child_runs_first".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "服务器场景下 fork 后父进程先运行更优，避免 COW 页面不必要的复制".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_page_cluster(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/page-cluster";
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
    let current = read_sysctl_u64(path);
    if current < 1024 {
        recs.push(Recommendation {
            param: "net.unix.max_dgram_qlen".to_string(),
            current_value: current.to_string(),
            recommended_value: "1024".to_string(),
            reason: "Unix socket 数据报队列默认 512 太小，systemd/journald 等高负载下可能丢失消息"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
}

fn eval_rps_sock_flow_entries(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/core/rps_sock_flow_entries";
    if !info.param_exists(path) {
        return 1;
    }
    let max_speed = info.network.iter().map(|n| n.speed_mbps).max().unwrap_or(0);
    if max_speed < 10000 {
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
    let path = "/proc/sys/vm/admin_reserve_kbytes";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 64 {
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
    if info.network.len() < 2 || has_bond() {
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
    if info.network.len() < 2 || has_bond() {
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

fn eval_default_log_martians(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/log_martians";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    if current < 2 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_adv_win_scale".to_string(),
            current_value: current.to_string(),
            recommended_value: "2".to_string(),
            reason: "TCP 接收缓冲区开销因子偏低，增大可让更多缓冲区用于应用数据提升吞吐"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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

fn eval_panic_on_oops(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/panic_on_oops";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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

fn eval_oom_dump_tasks(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/vm/oom_dump_tasks";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let current = read_sysctl_u64(path);
    if current != 0 {
        recs.push(Recommendation {
            param: "kernel.panic_on_warn".to_string(),
            current_value: current.to_string(),
            recommended_value: "0".to_string(),
            reason: "内核 WARN 即 panic 过于激进，正常运行中的 WARN 不应导致系统重启".to_string(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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
    if bytes == 0 && ratio > 5 {
        let recommended_mb = 256;
        recs.push(Recommendation {
            param: "vm.dirty_background_bytes".to_string(),
            current_value: format!("0 (ratio={ratio}%)"),
            recommended_value: format!("{}", recommended_mb * 1024 * 1024),
            reason: format!("{}GB 内存 dirty_background_ratio {}% = {}GB 脏页才开始后台刷盘，用 bytes 可精确控制",
                info.memory_total_gb, ratio, info.memory_total_gb * ratio / 100),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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
    if info.network.len() < 2 || has_bond() {
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
    if info.network.len() < 2 {
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
    if info.network.len() < 2 {
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

fn eval_default_send_redirects(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/send_redirects";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 1 {
        recs.push(Recommendation {
            param: "net.ipv4.conf.default.send_redirects".to_string(),
            current_value: "1".to_string(),
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
    let path = "/proc/sys/net/ipv4/conf/all/arp_filter";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.network.len() <= 1 || has_bond() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let current = read_sysctl_u64(path);
    if current == 0 || current > 3 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_orphan_retries".to_string(),
            current_value: if current == 0 {
                "0 (默认8)".to_string()
            } else {
                current.to_string()
            },
            recommended_value: "2".to_string(),
            reason: "孤儿连接（对端无响应）重试次数过多，占用资源时间过长，减少可加速资源回收"
                .to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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
    let path = "/proc/sys/vm/user_reserve_kbytes";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 64 {
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
    let path = "/proc/sys/kernel/sem";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let has_db = info
        .processes
        .iter()
        .any(|p| p.name == "postgres" || p.name == "mysqld" || p.name == "oracle");
    if !has_db {
        return 1;
    }
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let vals: Vec<u64> = content
        .split_whitespace()
        .filter_map(|s| s.parse().ok())
        .collect();
    if vals.len() >= 4 && (vals[0] < 1024 || vals[1] < 65536 || vals[3] < 4096) {
        let current_str = vals
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        recs.push(Recommendation {
            param: "kernel.sem".to_string(),
            current_value: current_str,
            recommended_value: "1024 65536 256 4096".to_string(),
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
    let path = "/proc/sys/kernel/shmmni";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let has_db = info
        .processes
        .iter()
        .any(|p| p.name == "postgres" || p.name == "mysqld" || p.name == "oracle");
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
    let current = read_sysctl_u64(path);
    if current == 0 {
        recs.push(Recommendation {
            param: "net.ipv4.tcp_fack".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            reason: "TCP Forward Acknowledgement 可改善丢包恢复效率，减少不必要的重传".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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

fn eval_accept_ra(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv6/conf/default/accept_ra";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current > 0 {
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
    let path = "/proc/sys/vm/min_slab_ratio";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    if info.memory_total_gb < 64 {
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
    let path = "/proc/sys/net/core/bpf_jit_harden";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    if !content.contains("bbr") {
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

fn eval_promote_secondaries(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/net/ipv4/conf/default/promote_secondaries";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
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
    let current = read_sysctl_u64(path);
    if current > 86400 {
        recs.push(Recommendation {
            param: "net.netfilter.nf_conntrack_tcp_timeout_established".to_string(),
            current_value: format!("{} ({}天)", current, current / 86400),
            recommended_value: "86400".to_string(),
            reason: "conntrack 已建立连接的超时默认 5 天太长，高并发下大量条目占满表导致丢包，缩短到 1 天".to_string(),
            confidence: Confidence::Medium,
            category: Category::Performance,
            writable: true,
        });
    }
    1
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

fn eval_hung_task_warnings(_info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let path = "/proc/sys/kernel/hung_task_warnings";
    if !std::path::Path::new(path).exists() {
        return 1;
    }
    let current = read_sysctl_u64(path);
    if current == 0 {
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
    1
}

fn eval_overcommit_ratio(info: &SystemInfo, recs: &mut Vec<Recommendation>) -> usize {
    let oc_path = "/proc/sys/vm/overcommit_memory";
    let ratio_path = "/proc/sys/vm/overcommit_ratio";
    if !std::path::Path::new(ratio_path).exists() {
        return 1;
    }
    let oc_mode = read_sysctl_u64(oc_path);
    if oc_mode != 2 {
        return 1;
    }
    let ratio = read_sysctl_u64(ratio_path);
    let db_present = info.processes.iter().any(|p| {
        p.name.contains("postgres") || p.name.contains("mysql") || p.name.contains("oracle")
    });
    if db_present && ratio < 80 {
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
    1
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn read_sysctl_string(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::*;

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
                tcp_fastopen: 1,
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
    fn test_no_false_positive_when_optimal() {
        let mut info = make_test_info();
        info.disks[0].scheduler = "none".to_string();
        info.disks[0].nr_requests = 1024;
        info.sysctl.swappiness = 10;
        info.sysctl.thp_enabled = "madvise".to_string();
        info.sysctl.dirty_ratio = 10;
        info.sysctl.dirty_background_ratio = 5;
        info.sysctl.somaxconn = 65535;
        info.sysctl.tcp_fastopen = 3;
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
            assert_eq!(rec.recommended_value, "999999999");
            assert_eq!(rec.confidence, Confidence::High);
            assert_eq!(rec.category, Category::Security);
        }
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
        let info = make_test_info();
        let mut recs = Vec::new();
        eval_tcp_fack(&info, &mut recs);
        if let Some(rec) = recs.iter().find(|r| r.param == "net.ipv4.tcp_fack") {
            assert_eq!(rec.recommended_value, "1");
            assert_eq!(rec.category, Category::Performance);
        }
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
    fn test_bpf_jit_harden() {
        let info = make_test_info();
        let mut recs = Vec::new();
        let checked = eval_bpf_jit_harden(&info, &mut recs);
        if std::path::Path::new("/proc/sys/net/core/bpf_jit_harden").exists() {
            assert_eq!(checked, 1);
            let val = read_sysctl_u64("/proc/sys/net/core/bpf_jit_harden");
            let triggered = recs.iter().any(|r| r.param == "net.core.bpf_jit_harden");
            if val == 0 {
                assert!(triggered, "Should trigger when bpf_jit_harden=0");
                assert_eq!(recs.last().unwrap().category, Category::Security);
            }
        }
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
