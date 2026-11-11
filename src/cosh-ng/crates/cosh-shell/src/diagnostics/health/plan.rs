use super::model::HealthCollector;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum SampleSource {
    Load,
    Meminfo,
    Host,
    Filesystems,
    Kernel,
    Services,
    Environment,
}

pub(super) struct CheckDefinition {
    pub(super) id: &'static str,
    pub(super) collector: HealthCollector,
    pub(super) source: SampleSource,
    pub(super) judgement_dependencies: &'static [SampleSource],
    pub(super) fact_keys: &'static [&'static str],
    pub(super) passive: bool,
    pub(super) budget_ms: u64,
}

const fn check(
    id: &'static str,
    collector: HealthCollector,
    source: SampleSource,
    judgement_dependencies: &'static [SampleSource],
    fact_keys: &'static [&'static str],
    passive: bool,
    budget_ms: u64,
) -> CheckDefinition {
    CheckDefinition {
        id,
        collector,
        source,
        judgement_dependencies,
        fact_keys,
        passive,
        budget_ms,
    }
}

use HealthCollector as C;
use SampleSource as S;

pub(super) const CHECKS: &[CheckDefinition] = &[
    check("resource.load", C::Cpu, S::Load, &[], &["cpu.*"], true, 30),
    check(
        "resource.memory",
        C::Memory,
        S::Meminfo,
        &[],
        &[
            "memory.total_mib",
            "memory.available_mib",
            "memory.available_ratio",
            "memory.used_ratio",
        ],
        true,
        30,
    ),
    check(
        "host.uptime",
        C::Host,
        S::Host,
        &[],
        &["host.*", "os.*"],
        true,
        30,
    ),
    check(
        "filesystem.root",
        C::Disk,
        S::Filesystems,
        &[],
        ROOT_FILESYSTEM_KEYS,
        true,
        100,
    ),
    check(
        "filesystem.mounts",
        C::Disk,
        S::Filesystems,
        &[],
        &["filesystem.*"],
        true,
        100,
    ),
    check(
        "kernel.oom",
        C::KernelSignal,
        S::Kernel,
        &[],
        &["kernel.*"],
        true,
        120,
    ),
    check(
        "service.status",
        C::ConfiguredService,
        S::Services,
        &[],
        &["service.*"],
        false,
        800,
    ),
    check(
        "resource.swap",
        C::Memory,
        S::Meminfo,
        &[S::Meminfo, S::Kernel, S::Services],
        &["memory.swap_*"],
        true,
        30,
    ),
    check(
        "environment.provider",
        C::Provider,
        S::Environment,
        &[],
        &["provider.*"],
        false,
        100,
    ),
    check(
        "environment.config",
        C::Config,
        S::Environment,
        &[],
        &["config.*"],
        false,
        100,
    ),
    check(
        "environment.hooks",
        C::Hooks,
        S::Environment,
        &[],
        &["hooks.*"],
        false,
        100,
    ),
    check(
        "environment.pty",
        C::Pty,
        S::Environment,
        &[],
        &["pty.*"],
        false,
        100,
    ),
    check(
        "environment.permissions",
        C::Permissions,
        S::Environment,
        &[],
        &["permissions.*"],
        false,
        100,
    ),
];

pub(super) fn definition(id: &str) -> Option<&'static CheckDefinition> {
    CHECKS.iter().find(|check| check.id == id)
}

const ROOT_FILESYSTEM_KEYS: &[&str] = &[
    "filesystem.root_used_ratio",
    "filesystem.root_available_gib",
    "filesystem.mount./.path",
    "filesystem.mount./.used_ratio",
    "filesystem.mount./.available_gib",
];

pub(super) fn owns_fact(id: &str, key: &str) -> bool {
    if id == "filesystem.mounts"
        && (key.starts_with("filesystem.root_") || ROOT_FILESYSTEM_KEYS.contains(&key))
    {
        return false;
    }
    definition(id).is_some_and(|check| {
        check.fact_keys.iter().any(|pattern| {
            pattern
                .strip_suffix('*')
                .map_or(*pattern == key, |prefix| key.starts_with(prefix))
        })
    })
}

pub(super) fn judgement_context_available(
    id: &str,
    report: &super::model::HealthScanReport,
    config: &crate::config::HealthConfig,
) -> bool {
    use super::model::{HealthCheckStatus, HealthFactCoverage};
    let complete = |collector| {
        report.check_results.iter().any(|check| {
            check.collector == Some(collector) && check.status == HealthCheckStatus::Ok
        })
    };
    definition(id).is_some_and(|definition| {
        definition
            .judgement_dependencies
            .iter()
            .all(|source| match source {
                S::Meminfo => report
                    .facts
                    .iter()
                    .any(|fact| fact.key == "memory.swap_memory_pressure"),
                S::Kernel => {
                    complete(C::KernelSignal)
                        && report
                            .facts
                            .iter()
                            .filter(|fact| {
                                fact.category == super::model::HealthFactCategory::Kernel
                            })
                            .any(|fact| {
                                report.fact_metadata.get(&fact.id).is_some_and(|metadata| {
                                    matches!(
                                        metadata.coverage,
                                        HealthFactCoverage::ReadWindow { complete: true, .. }
                                    )
                                })
                            })
                }
                S::Services => config.services.is_empty() || complete(C::ConfiguredService),
                _ => true,
            })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn root_mount_facts_do_not_overlap_hidden_mount_paths() {
        for suffix in ["path", "used_ratio", "available_gib"] {
            let root = format!("filesystem.mount./.{suffix}");
            assert!(super::owns_fact("filesystem.root", &root));
            assert!(!super::owns_fact("filesystem.mounts", &root));
            for path in ["/.cz-init", "/.data", "/cache"] {
                let key = format!("filesystem.mount.{path}.{suffix}");
                assert!(super::owns_fact("filesystem.mounts", &key), "{key}");
                assert!(!super::owns_fact("filesystem.root", &key), "{key}");
            }
        }
        assert!(!super::owns_fact(
            "filesystem.mounts",
            "filesystem.root_used_ratio"
        ));
        assert!(!super::owns_fact(
            "filesystem.mounts",
            "filesystem.root_available_gib"
        ));
    }

    use super::*;

    #[test]
    fn public_fact_ownership_is_disjoint_and_rejects_unknown_checks() {
        assert!(!owns_fact("unknown", "memory.available_mib"));
        for key in [
            "filesystem.root_used_ratio",
            "filesystem.root_available_gib",
            "filesystem.mount./.used_ratio",
        ] {
            assert!(owns_fact("filesystem.root", key));
            assert!(!owns_fact("filesystem.mounts", key), "{key}");
        }
        assert!(owns_fact(
            "filesystem.mounts",
            "filesystem.mount./data.used_ratio"
        ));
        assert!(!owns_fact("resource.swap", "memory.available_mib"));
        assert!(!owns_fact("resource.memory", "memory.swap_used_mib"));
    }
}
