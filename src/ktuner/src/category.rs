use crate::rules::{Category, Confidence, Recommendation};

pub fn is_runtime_dangerous(param: &str) -> bool {
    param == "vm.nr_hugepages"
}

pub fn param_subcategory(param: &str) -> &'static str {
    if param.starts_with("net.") || param.contains("conntrack") {
        "network"
    } else if param.starts_with("vm.") {
        "memory"
    } else if param.starts_with("block/")
        || param.starts_with("transparent_hugepage/")
        || param.starts_with("fs.inotify.")
    {
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

pub fn filter_by_category(mut recs: Vec<Recommendation>, cat: &str) -> Vec<Recommendation> {
    let cat_lower = cat.to_lowercase();
    recs.retain(|r| match cat_lower.as_str() {
        "network" | "net" | "网络" => {
            r.category != Category::Security
                && (r.param.starts_with("net.") || r.param.contains("conntrack"))
        }
        "memory" | "mem" | "内存" => {
            r.category != Category::Security
                && (r.param.starts_with("vm.")
                    || (r.param.starts_with("fs.")
                        && !r.param.starts_with("fs.inotify.")
                        && !r.param.starts_with("fs.aio-"))
                    || r.param == "kernel.shmmax")
        }
        "io" | "disk" | "磁盘" => {
            r.param.starts_with("block/")
                || r.param.starts_with("transparent_hugepage/")
                || r.param.starts_with("fs.inotify.")
                || r.param.starts_with("fs.aio-")
        }
        "cpu" | "调度" => {
            r.param.starts_with("kernel.sched")
                || r.param.contains("pid_max")
                || r.param.contains("numa")
                || r.param == "kernel.threads-max"
        }
        "security" | "sec" | "安全" => r.category == Category::Security,
        _ => true,
    });
    recs
}

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
        assert_eq!(param_subcategory("transparent_hugepage/enabled"), "io");
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
