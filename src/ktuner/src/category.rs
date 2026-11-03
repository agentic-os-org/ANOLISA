use crate::rules::{Category, Confidence, Recommendation};

pub fn is_runtime_dangerous(param: &str) -> bool {
    param == "vm.nr_hugepages"
}

pub fn param_subcategory(param: &str) -> &'static str {
    if param.starts_with("net.") || param.contains("conntrack") {
        "network"
    } else if param.starts_with("vm.") || param.starts_with("transparent_hugepage/") {
        // transparent_hugepage/ is memory management, not I/O: the kernel
        // creates the directory under the mm kobject (v6.6
        // mm/mm_init.c:209 and mm/huge_memory.c:413), and the documentation
        // writes the knob as /sys/kernel/mm/transparent_hugepage/enabled
        // (Documentation/admin-guide/mm/transhuge.rst:100). The io arm used
        // to claim the prefix, so --category mem silently dropped the THP
        // recommendation while --category io surfaced it next to
        // block-layer queues.
        "memory"
    } else if param.starts_with("block/") || param.starts_with("fs.inotify.") {
        "io"
    } else if param.starts_with("kernel.sched_")
        || param.starts_with("kernel.pid_")
        || param.starts_with("kernel.threads")
        || param.starts_with("kernel.numa_")
        || param.starts_with("kernel.perf_")
        || param.starts_with("kernel.nmi_")
        || param.starts_with("kernel.watchdog")
        || param.starts_with("kernel.hung_task")
        || param.starts_with("kernel.softlockup")
        || param.starts_with("kernel.hardlockup")
    {
        "cpu"
    } else if param.starts_with("kernel.") {
        match param {
            // SysV shared-memory sizing knobs are a memory resource (the mem
            // filter has always special-cased kernel.shmmax); they must not
            // fall through to the generic kernel.* "cpu" bucket. The same
            // holds for the other SysV IPC sizing knobs (semaphore sets,
            // message queues): they size IPC memory, not the scheduler, so
            // --category mem must surface their recommendations.
            "kernel.shmmax" | "kernel.shmall" | "kernel.shmmni" | "kernel.shm_rmid_forced" => {
                "memory"
            }
            "kernel.sem" | "kernel.msgmax" | "kernel.msgmnb" | "kernel.msgmni" => "memory",
            // The kernel/keys quota knobs are the same memory-sizing class:
            // the kernel charges every key's payload bytes and its entry count
            // against them (security/keys/key.c key_quota_maxbytes=20000 /
            // key_quota_maxkeys=200), and the keyring reference counts the
            // pair as a memory resource — "the total amount of description
            // and payload space that can be consumed"
            // (Documentation/security/keys/core.rst). They are what the
            // encrypted workloads the rules scale for consume, so the
            // kernel.* catch-all must not label the size quota "cpu" and hide
            // it from --category mem. The root_* pair belongs to the same
            // documented group and carries no built-in recommendation, like
            // kernel.msgmni above.
            "kernel.keys.maxkeys"
            | "kernel.keys.maxbytes"
            | "kernel.keys.root_maxkeys"
            | "kernel.keys.root_maxbytes" => "memory",
            "kernel.dmesg_restrict"
            | "kernel.kptr_restrict"
            | "kernel.yama.ptrace_scope"
            | "kernel.randomize_va_space"
            | "kernel.sysrq"
            | "kernel.modules_disabled"
            | "kernel.kexec_load_disabled"
            | "kernel.unprivileged_bpf_disabled" => "security",
            _ => "cpu",
        }
    } else if param.starts_with("fs.protected_") || param.starts_with("fs.suid_") {
        "security"
    } else if param.starts_with("fs.") {
        "io"
    } else {
        "other"
    }
}

pub fn validate_category(cat: &str) -> anyhow::Result<()> {
    let cat_lower = cat.to_lowercase();
    if !matches!(
        cat_lower.as_str(),
        "network"
            | "net"
            | "网络"
            | "内存"
            | "memory"
            | "mem"
            | "io"
            | "disk"
            | "磁盘"
            | "cpu"
            | "调度"
            | "security"
            | "sec"
            | "安全"
    ) {
        anyhow::bail!("未知分类: {cat}（支持: net, mem, io, cpu, security）");
    }
    Ok(())
}

/// Whether `param`, recorded under the rule layer's own `category`, belongs
/// to the category `cat_lower` names, already lowercased.
/// The mem/io/cpu retention predicates are derived from `param_subcategory` —
/// the same classifier that labels every recommendation in `ktuner check`
/// output and buckets `RecCounts::from_recs` — so a category never drops a
/// recommendation the engine itself labels with it, nor keeps one it labels
/// differently. The `Category::Security` guard mirrors `RecCounts::from_recs`:
/// a security recommendation only ever surfaces under `security`.
///
/// Taking the parameter and its rule category instead of a whole
/// [`Recommendation`] is what lets a stored record ask the same question: the
/// rollback ledger keeps exactly these two facts per entry, and the answer
/// must be the one the same parameter would get from the live plan.
///
/// Every value the vocabulary accepts has an arm of its own (the aliases of
/// one category share theirs, and [`validate_category`] gates the set), so the
/// fallback only ever answers for a value that names no category at all — and
/// nothing belongs to a category that does not exist. It answered `true` once:
/// an unrecognized value then kept every entry, and the rollback selection
/// runs on this same predicate, so a typo selected the whole categorized
/// ledger instead of nothing.
pub fn category_matches(param: &str, category: &Category, cat_lower: &str) -> bool {
    match cat_lower {
        "network" | "net" | "网络" => {
            *category != Category::Security
                && (param.starts_with("net.") || param.contains("conntrack"))
        }
        "memory" | "mem" | "内存" => {
            *category != Category::Security && param_subcategory(param) == "memory"
        }
        "io" | "disk" | "磁盘" => {
            *category != Category::Security && param_subcategory(param) == "io"
        }
        "cpu" | "调度" => *category != Category::Security && param_subcategory(param) == "cpu",
        "security" | "sec" | "安全" => *category == Category::Security,
        _ => false,
    }
}

/// Whether `param`, recorded under the rule layer's own `category`, belongs
/// to any of `cats` — the union a repeated `--category` asks for.
///
/// [`filter_by_categories`] and this function answer with the same
/// [`category_matches`] predicate, so the set a command selects and the set a
/// plan keeps cannot drift; that one lowers the names once for a whole plan,
/// this one per recorded entry. Values are validated by
/// [`validate_category`] before they get here.
pub fn matches_any_category(param: &str, category: &Category, cats: &[String]) -> bool {
    cats.iter()
        .any(|cat| category_matches(param, category, &cat.to_lowercase()))
}

/// Keep only the recommendations whose parameter belongs to `cat`. See
/// `category_matches` for the per-category predicates.
pub fn filter_by_category(mut recs: Vec<Recommendation>, cat: &str) -> Vec<Recommendation> {
    let cat_lower = cat.to_lowercase();
    recs.retain(|r| category_matches(&r.param, &r.category, &cat_lower));
    recs
}

/// Keep only the recommendations belonging to any of `cats`: several
/// `--category` values filter to the union, so an entry surfaces when at
/// least one named category keeps it. An alias repeated across the values
/// keeps nothing twice (the union has no duplicates), and an empty slice
/// keeps every recommendation — a command without `--category` must not
/// filter. Values are validated by `validate_category` before they get here.
pub fn filter_by_categories(mut recs: Vec<Recommendation>, cats: &[String]) -> Vec<Recommendation> {
    if cats.is_empty() {
        return recs;
    }
    let lowered: Vec<String> = cats.iter().map(|cat| cat.to_lowercase()).collect();
    recs.retain(|r| {
        lowered
            .iter()
            .any(|cat| category_matches(&r.param, &r.category, cat))
    });
    recs
}

/// The counts a `check` report carries for its recommendations. `net`, `mem`,
/// `io` and `cpu` count the performance findings [`category_matches`] keeps for
/// that category, and `sec` the security findings, so each is the length of
/// the report `--category` would print.
pub struct RecCounts {
    pub perf: usize,
    pub sec: usize,
    pub high: usize,
    pub writable: usize,
    pub net: usize,
    pub mem: usize,
    pub io: usize,
    pub cpu: usize,
}

impl RecCounts {
    pub fn from_recs(recs: &[Recommendation]) -> Self {
        let perf = recs
            .iter()
            .filter(|r| r.category == Category::Performance)
            .count();
        let sec = recs
            .iter()
            .filter(|r| r.category == Category::Security)
            .count();
        let high = recs
            .iter()
            .filter(|r| r.confidence == Confidence::High)
            .count();
        let writable = recs.iter().filter(|r| r.writable).count();
        let net = recs
            .iter()
            .filter(|r| {
                r.category != Category::Security && param_subcategory(&r.param) == "network"
            })
            .count();
        let mem = recs
            .iter()
            .filter(|r| r.category != Category::Security && param_subcategory(&r.param) == "memory")
            .count();
        let io = recs
            .iter()
            .filter(|r| r.category != Category::Security && param_subcategory(&r.param) == "io")
            .count();
        let cpu = recs
            .iter()
            .filter(|r| r.category != Category::Security && param_subcategory(&r.param) == "cpu")
            .count();
        Self {
            perf,
            sec,
            high,
            writable,
            net,
            mem,
            io,
            cpu,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_runtime_dangerous() {
        assert!(is_runtime_dangerous("vm.nr_hugepages"));
        assert!(!is_runtime_dangerous("vm.swappiness"));
        assert!(!is_runtime_dangerous("net.core.somaxconn"));
    }

    #[test]
    fn test_param_subcategory() {
        assert_eq!(param_subcategory("net.ipv4.tcp_fastopen"), "network");
        assert_eq!(
            param_subcategory("net.netfilter.nf_conntrack_max"),
            "network"
        );
        assert_eq!(param_subcategory("vm.swappiness"), "memory");
        assert_eq!(param_subcategory("block/sda/scheduler"), "io");
        assert_eq!(param_subcategory("transparent_hugepage/enabled"), "memory");
        assert_eq!(param_subcategory("fs.inotify.max_user_watches"), "io");
        assert_eq!(param_subcategory("kernel.sched_migration_cost_ns"), "cpu");
        assert_eq!(param_subcategory("kernel.pid_max"), "cpu");
        assert_eq!(param_subcategory("kernel.dmesg_restrict"), "security");
        assert_eq!(param_subcategory("kernel.randomize_va_space"), "security");
        assert_eq!(param_subcategory("fs.protected_hardlinks"), "security");
        assert_eq!(param_subcategory("fs.suid_dumpable"), "security");
        assert_eq!(param_subcategory("fs.file-max"), "io");
        assert_eq!(param_subcategory("unknown.param"), "other");
    }

    #[test]
    fn test_validate_category_valid() {
        for cat in [
            "net", "network", "mem", "memory", "io", "disk", "cpu", "security", "sec",
        ] {
            assert!(validate_category(cat).is_ok(), "{cat} should be valid");
        }
    }

    #[test]
    fn test_validate_category_cn_aliases() {
        // Every alias filter_by_category accepts must also pass validation,
        // or the CLI rejects the category before filtering can run.
        for cat in ["网络", "内存", "磁盘", "调度", "安全"] {
            assert!(validate_category(cat).is_ok(), "{cat} should be valid");
        }
    }

    #[test]
    fn test_validate_category_invalid() {
        assert!(validate_category("garbage").is_err());
        assert!(validate_category("").is_err());
    }

    #[test]
    fn test_filter_by_category_net() {
        let recs = vec![
            Recommendation {
                param: "net.core.somaxconn".into(),
                current_value: "128".into(),
                recommended_value: "4096".into(),
                reason: "".into(),
                confidence: Confidence::High,
                category: Category::Performance,
                writable: true,
            },
            Recommendation {
                param: "vm.swappiness".into(),
                current_value: "60".into(),
                recommended_value: "10".into(),
                reason: "".into(),
                confidence: Confidence::Medium,
                category: Category::Performance,
                writable: true,
            },
            Recommendation {
                param: "kernel.dmesg_restrict".into(),
                current_value: "0".into(),
                recommended_value: "1".into(),
                reason: "".into(),
                confidence: Confidence::High,
                category: Category::Security,
                writable: true,
            },
        ];
        let filtered = filter_by_category(recs, "net");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].param, "net.core.somaxconn");
    }

    fn rec(param: &str) -> Recommendation {
        Recommendation {
            param: param.into(),
            current_value: "0".into(),
            recommended_value: "1".into(),
            reason: String::new(),
            confidence: Confidence::High,
            category: Category::Performance,
            writable: true,
        }
    }

    #[test]
    fn fs_param_filter_matches_subcategory() {
        // fs.file-max is labeled "io" by param_subcategory, so it must surface
        // under --category io — not mem, where it used to land.
        assert_eq!(param_subcategory("fs.file-max"), "io");
        assert!(!filter_by_category(vec![rec("fs.file-max")], "io").is_empty());
        assert!(filter_by_category(vec![rec("fs.file-max")], "mem").is_empty());
    }

    #[test]
    fn cpu_filter_keeps_subcategory_cpu_params() {
        for param in [
            "kernel.nmi_watchdog",
            "kernel.watchdog_thresh",
            "kernel.hung_task_timeout_secs",
            "kernel.perf_event_paranoid",
        ] {
            assert_eq!(param_subcategory(param), "cpu");
            assert!(
                !filter_by_category(vec![rec(param)], "cpu").is_empty(),
                "{param} is labeled cpu by param_subcategory but dropped by --category cpu"
            );
        }
    }

    #[test]
    fn filter_keeps_established_members_of_each_category() {
        // Regression guard for the subcategory-derived predicates: the params
        // every branch has always kept must not be lost in the derivation.
        assert_eq!(
            filter_by_category(vec![rec("vm.swappiness")], "mem").len(),
            1
        );
        assert_eq!(
            filter_by_category(vec![rec("kernel.shmmax")], "mem").len(),
            1
        );
        assert_eq!(
            filter_by_category(vec![rec("block/sda/scheduler")], "io").len(),
            1
        );
        assert_eq!(
            filter_by_category(vec![rec("fs.inotify.max_user_watches")], "io").len(),
            1
        );
        assert_eq!(
            filter_by_category(vec![rec("kernel.sched_latency_ns")], "cpu").len(),
            1
        );
    }

    #[test]
    fn filter_by_categories_keeps_the_union() {
        // `--category net --category mem` must keep both categories' entries
        // and nothing else: an agent that names two categories reads the
        // union, not the last value and not the intersection.
        let recs = vec![
            rec("net.core.somaxconn"),
            rec("vm.swappiness"),
            rec("kernel.sched_latency_ns"),
            rec("fs.file-max"),
        ];
        let filtered = filter_by_categories(recs, &["net".to_string(), "mem".to_string()]);
        let params: Vec<&str> = filtered.iter().map(|r| r.param.as_str()).collect();
        assert_eq!(params, ["net.core.somaxconn", "vm.swappiness"]);
    }

    #[test]
    fn filter_by_categories_empty_slice_keeps_everything() {
        // A command without --category passes no values; the filter must be a
        // no-op rather than dropping every recommendation.
        let recs = vec![rec("net.core.somaxconn"), rec("vm.swappiness")];
        let unfiltered = filter_by_categories(recs, &[]);
        assert_eq!(unfiltered.len(), 2);
        assert_eq!(unfiltered[0].param, "net.core.somaxconn");
        assert_eq!(unfiltered[1].param, "vm.swappiness");
    }

    #[test]
    fn filter_by_categories_alias_repeats_count_once() {
        // `net`, `network` and `网络` name the same category: repeating the
        // aliases must keep exactly the single-value result, once.
        let recs = vec![
            rec("net.core.somaxconn"),
            rec("vm.swappiness"),
            rec("kernel.dmesg_restrict"),
        ];
        let single = filter_by_category(recs.clone(), "net");
        let repeated = filter_by_categories(
            recs,
            &["net".to_string(), "network".to_string(), "网络".to_string()],
        );
        let single_params: Vec<&str> = single.iter().map(|r| r.param.as_str()).collect();
        let repeated_params: Vec<&str> = repeated.iter().map(|r| r.param.as_str()).collect();
        assert_eq!(repeated_params, single_params);
    }

    #[test]
    fn filter_by_categories_security_only_under_security() {
        // The Category::Security guard holds for the union too: a security
        // recommendation never surfaces from a perf-only category value,
        // whether that value stands alone or shares the union with another.
        let mut security = rec("kernel.dmesg_restrict");
        security.category = Category::Security;
        assert!(filter_by_categories(vec![security.clone()], &["net".to_string()]).is_empty());
        assert!(filter_by_categories(
            vec![security.clone()],
            &["net".to_string(), "mem".to_string()]
        )
        .is_empty());
        assert_eq!(
            filter_by_categories(vec![security.clone()], &["sec".to_string()]).len(),
            1
        );
        assert_eq!(
            filter_by_categories(
                vec![security.clone()],
                &["net".to_string(), "安全".to_string()]
            )
            .len(),
            1
        );
        assert_eq!(
            filter_by_categories(vec![security], &["security".to_string()]).len(),
            1
        );
    }

    #[test]
    fn unknown_category_matches_nothing() {
        // Every value the vocabulary accepts has its own arm above
        // (`validate_category` gates it), so the fallback only ever answers
        // for a value that names no category at all — and a value that names
        // no category holds no entry. The fallback used to answer `true`: an
        // unknown category kept every entry, and the rollback selection runs
        // on this same predicate, so a typo selected (and restored) every
        // categorized ledger entry instead of none.
        let performance = rec("vm.swappiness");
        for cat in ["garbage", "", "netwok", "sec urity", "网络 "] {
            assert!(
                !category_matches(&performance.param, &performance.category, cat),
                "{cat:?} names no category, so nothing belongs to it"
            );
            assert!(
                !matches_any_category(
                    &performance.param,
                    &performance.category,
                    &[cat.to_string()]
                ),
                "{cat:?} in a union selects nothing"
            );
            assert!(
                filter_by_categories(vec![performance.clone()], &[cat.to_string()]).is_empty(),
                "{cat:?} keeps no entry"
            );
            assert!(
                filter_by_category(vec![performance.clone()], cat).is_empty(),
                "{cat:?} keeps no entry"
            );
        }
        // The fallback carries no documented value: a category the validator
        // accepts keeps its own arm's answer, unchanged.
        assert!(category_matches(
            &performance.param,
            &performance.category,
            "mem"
        ));
        assert_eq!(
            filter_by_categories(vec![performance], &["mem".to_string()]).len(),
            1
        );
    }

    #[test]
    fn shm_params_subcategory_is_memory() {
        // SysV shared-memory knobs are a memory resource, not cpu: the mem
        // filter special-cases kernel.shmmax, so the classifier must agree
        // instead of falling through the kernel.* catch-all to "cpu".
        assert_eq!(param_subcategory("kernel.shmmax"), "memory");
        assert_eq!(param_subcategory("kernel.shmall"), "memory");
        assert_eq!(param_subcategory("kernel.shmmni"), "memory");
        assert_eq!(param_subcategory("kernel.shm_rmid_forced"), "memory");
    }

    #[test]
    fn sysv_ipc_sizing_knobs_subcategory_is_memory() {
        // kernel.sem / kernel.msgmax / kernel.msgmnb are the same class of
        // SysV IPC sizing knob the classifier already routes to "memory"
        // (kernel.shmmax & co). They have Performance recommendations, so
        // under the old kernel.* catch-all they were counted and filtered
        // as "cpu" — `--category mem` silently dropped them while
        // `--category cpu` surfaced IPC queue sizing next to scheduler
        // knobs.
        for param in ["kernel.sem", "kernel.msgmax", "kernel.msgmnb"] {
            assert_eq!(param_subcategory(param), "memory", "{param}");
            assert_eq!(
                filter_by_category(vec![rec(param)], "mem").len(),
                1,
                "{param} must surface under --category mem"
            );
            assert!(
                filter_by_category(vec![rec(param)], "cpu").is_empty(),
                "{param} is not a cpu knob"
            );
        }
    }

    #[test]
    fn keyring_quota_knobs_subcategory_is_memory() {
        // kernel/keys/<quota> sizes the memory-resident keyring, the same
        // sizing class as the SysV IPC knobs above: the kernel charges a
        // key's payload bytes and its entry count against these limits
        // (security/keys/key.c: key_quota_maxbytes / key_quota_maxkeys), and
        // the keyring's own reference calls them "the total amount of
        // description and payload space that can be consumed"
        // (Documentation/security/keys/core.rst). They have Performance
        // recommendations, so under the kernel.* catch-all they were counted
        // and filtered as "cpu" — `--category mem` silently dropped both
        // quotas while `--category cpu` surfaced them next to scheduler
        // knobs. The root_* pair is part of the same documented group ("Four
        // new sysctl files ... for the purpose of controlling the quota
        // limits on keys") and no rule recommends it, exactly like
        // kernel.msgmni in the SysV arm above.
        for param in [
            "kernel.keys.maxkeys",
            "kernel.keys.maxbytes",
            "kernel.keys.root_maxkeys",
            "kernel.keys.root_maxbytes",
        ] {
            assert_eq!(param_subcategory(param), "memory", "{param}");
            assert_eq!(
                filter_by_category(vec![rec(param)], "mem").len(),
                1,
                "{param} must surface under --category mem"
            );
            assert!(
                filter_by_category(vec![rec(param)], "cpu").is_empty(),
                "{param} is not a cpu knob"
            );
        }
    }

    #[test]
    fn transparent_hugepage_knobs_subcategory_is_memory() {
        // transparent_hugepage/ is the memory-management interface: the
        // kernel creates the directory under the mm kobject — mm_kobj is
        // /sys/kernel/mm (v6.6 mm/mm_init.c:209) and hugepage_init_sysfs()
        // hangs transparent_hugepage off it (v6.6 mm/huge_memory.c:413),
        // next to the enabled/defrag/shmem_enabled attributes
        // (v6.6 mm/huge_memory.c:394-403) the documentation writes as
        // /sys/kernel/mm/transparent_hugepage/enabled
        // (Documentation/admin-guide/mm/transhuge.rst:100). The io arm used
        // to claim the prefix, so `--category mem` silently dropped the THP
        // recommendation while `--category io` surfaced a memory knob next
        // to block-device queues.
        for param in [
            "transparent_hugepage/enabled",
            "transparent_hugepage/defrag",
            "transparent_hugepage/shmem_enabled",
        ] {
            assert_eq!(param_subcategory(param), "memory", "{param}");
            assert_eq!(
                filter_by_category(vec![rec(param)], "mem").len(),
                1,
                "{param} must surface under --category mem"
            );
            assert!(
                filter_by_category(vec![rec(param)], "io").is_empty(),
                "{param} is not a disk knob"
            );
        }
    }

    #[test]
    fn test_rec_counts() {
        let recs = vec![
            Recommendation {
                param: "net.core.somaxconn".into(),
                current_value: "128".into(),
                recommended_value: "4096".into(),
                reason: "".into(),
                confidence: Confidence::High,
                category: Category::Performance,
                writable: true,
            },
            Recommendation {
                param: "kernel.dmesg_restrict".into(),
                current_value: "0".into(),
                recommended_value: "1".into(),
                reason: "".into(),
                confidence: Confidence::Medium,
                category: Category::Security,
                writable: false,
            },
        ];
        let c = RecCounts::from_recs(&recs);
        assert_eq!(c.perf, 1);
        assert_eq!(c.sec, 1);
        assert_eq!(c.high, 1);
        assert_eq!(c.writable, 1);
        assert_eq!(c.net, 1);
    }
}
