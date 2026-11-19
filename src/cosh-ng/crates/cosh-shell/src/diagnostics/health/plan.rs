use super::model::{HealthCollector, HealthMessageId};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub(super) enum SampleSource {
    Load,
    Meminfo,
    Host,
    Filesystems,
    Kernel,
    Services,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FinishPolicy {
    ReuseOnSuccess,
    RetireAfterSource,
}

pub(super) struct SourceDefinition {
    pub(super) source: SampleSource,
    pub(super) passive_budget_ms: u64,
    pub(super) finish_policy: FinishPolicy,
}

pub(super) struct CheckDefinition {
    pub(super) id: &'static str,
    pub(super) collector: HealthCollector,
    pub(super) source: SampleSource,
    pub(super) judgement_dependencies: &'static [SampleSource],
    pub(super) fact_keys: &'static [&'static str],
    pub(super) passive: bool,
}

const fn check(
    id: &'static str,
    collector: HealthCollector,
    source: SampleSource,
    judgement_dependencies: &'static [SampleSource],
    fact_keys: &'static [&'static str],
    passive: bool,
) -> CheckDefinition {
    CheckDefinition {
        id,
        collector,
        source,
        judgement_dependencies,
        fact_keys,
        passive,
    }
}

use HealthCollector as C;
use SampleSource as S;

const fn source(
    source: SampleSource,
    passive_budget_ms: u64,
    finish_policy: FinishPolicy,
) -> SourceDefinition {
    SourceDefinition {
        source,
        passive_budget_ms,
        finish_policy,
    }
}

const LOAD_SOURCE: SourceDefinition = source(S::Load, 30, FinishPolicy::ReuseOnSuccess);
const MEMINFO_SOURCE: SourceDefinition = source(S::Meminfo, 30, FinishPolicy::ReuseOnSuccess);
const HOST_SOURCE: SourceDefinition = source(S::Host, 30, FinishPolicy::ReuseOnSuccess);
const FILESYSTEMS_SOURCE: SourceDefinition =
    source(S::Filesystems, 100, FinishPolicy::RetireAfterSource);
const KERNEL_SOURCE: SourceDefinition = source(S::Kernel, 120, FinishPolicy::RetireAfterSource);
const SERVICES_SOURCE: SourceDefinition = source(S::Services, 800, FinishPolicy::RetireAfterSource);
const ENVIRONMENT_SOURCE: SourceDefinition =
    source(S::Environment, 100, FinishPolicy::ReuseOnSuccess);

pub(super) const SOURCES: &[SourceDefinition] = &[
    LOAD_SOURCE,
    MEMINFO_SOURCE,
    HOST_SOURCE,
    FILESYSTEMS_SOURCE,
    KERNEL_SOURCE,
    SERVICES_SOURCE,
    ENVIRONMENT_SOURCE,
];

pub(super) fn source_definition(source: SampleSource) -> &'static SourceDefinition {
    match source {
        S::Load => &LOAD_SOURCE,
        S::Meminfo => &MEMINFO_SOURCE,
        S::Host => &HOST_SOURCE,
        S::Filesystems => &FILESYSTEMS_SOURCE,
        S::Kernel => &KERNEL_SOURCE,
        S::Services => &SERVICES_SOURCE,
        S::Environment => &ENVIRONMENT_SOURCE,
    }
}

pub(super) const CHECKS: &[CheckDefinition] = &[
    check("resource.load", C::Cpu, S::Load, &[], &["cpu.*"], true),
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
            "memory.total_kib",
            "memory.available_kib",
        ],
        true,
    ),
    check(
        "host.uptime",
        C::Host,
        S::Host,
        &[],
        &["host.*", "os.*"],
        true,
    ),
    check(
        "filesystem.root",
        C::Disk,
        S::Filesystems,
        &[],
        ROOT_FILESYSTEM_KEYS,
        true,
    ),
    check(
        "filesystem.mounts",
        C::Disk,
        S::Filesystems,
        &[],
        &["filesystem.*"],
        true,
    ),
    check(
        "kernel.oom",
        C::KernelSignal,
        S::Kernel,
        &[],
        &["kernel.*"],
        true,
    ),
    check(
        "service.status",
        C::ConfiguredService,
        S::Services,
        &[],
        &["service.*"],
        false,
    ),
    check(
        "resource.swap",
        C::Memory,
        S::Meminfo,
        &[S::Meminfo, S::Kernel, S::Services],
        &["memory.swap_*"],
        true,
    ),
    check(
        "environment.provider",
        C::Provider,
        S::Environment,
        &[],
        &["provider.*"],
        false,
    ),
    check(
        "environment.config",
        C::Config,
        S::Environment,
        &[],
        &["config.*"],
        false,
    ),
    check(
        "environment.hooks",
        C::Hooks,
        S::Environment,
        &[],
        &["hooks.*"],
        false,
    ),
    check(
        "environment.pty",
        C::Pty,
        S::Environment,
        &[],
        &["pty.*"],
        false,
    ),
    check(
        "environment.permissions",
        C::Permissions,
        S::Environment,
        &[],
        &["permissions.*"],
        false,
    ),
];

pub(super) fn definition(id: &str) -> Option<&'static CheckDefinition> {
    CHECKS.iter().find(|check| check.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AdviceKind {
    MemoryPressure,
    SwapPressure,
    RecentOom,
    DiskUsage,
    ServiceStatus,
    HighLoad,
    ProcessMemory,
    UnavailableChecks,
    SystemCapacity,
}

pub(crate) struct AdviceDefinition {
    pub(crate) kind: AdviceKind,
    pub(crate) legacy_id: &'static str,
    pub(crate) prompt_id: HealthMessageId,
    pub(crate) label_id: HealthMessageId,
    pub(crate) reason_id: HealthMessageId,
    pub(crate) bash_surface: bool,
}

const MEMORY_PRESSURE_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::MemoryPressure,
    legacy_id: "T01",
    prompt_id: HealthMessageId::HealthPromptAnalyzeMemoryPressure,
    label_id: HealthMessageId::HealthTryAnalyzeMemoryPressure,
    reason_id: HealthMessageId::HealthTryReasonMemoryLow,
    bash_surface: true,
};
const SWAP_PRESSURE_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::SwapPressure,
    legacy_id: "T02",
    prompt_id: HealthMessageId::HealthPromptCheckSwapPressure,
    label_id: HealthMessageId::HealthTryCheckSwapPressure,
    reason_id: HealthMessageId::HealthTryReasonSwapWithContext,
    bash_surface: true,
};
const RECENT_OOM_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::RecentOom,
    legacy_id: "T03",
    prompt_id: HealthMessageId::HealthPromptCheckRecentOom,
    label_id: HealthMessageId::HealthTryCheckRecentOom,
    reason_id: HealthMessageId::HealthTryReasonRecentOom,
    bash_surface: true,
};
const DISK_USAGE_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::DiskUsage,
    legacy_id: "T04",
    prompt_id: HealthMessageId::HealthPromptInspectDiskUsage,
    label_id: HealthMessageId::HealthTryInspectDiskUsage,
    reason_id: HealthMessageId::HealthTryReasonDiskHigh,
    bash_surface: true,
};
const SERVICE_STATUS_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::ServiceStatus,
    legacy_id: "T05",
    prompt_id: HealthMessageId::HealthPromptInspectServiceStatus,
    label_id: HealthMessageId::HealthTryInspectServiceStatus,
    reason_id: HealthMessageId::HealthTryReasonServiceState,
    bash_surface: false,
};
const HIGH_LOAD_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::HighLoad,
    legacy_id: "T06",
    prompt_id: HealthMessageId::HealthPromptInspectHighLoad,
    label_id: HealthMessageId::HealthTryInspectHighLoad,
    reason_id: HealthMessageId::HealthTryReasonHighLoad,
    bash_surface: true,
};
const PROCESS_MEMORY_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::ProcessMemory,
    legacy_id: "T07",
    prompt_id: HealthMessageId::HealthPromptInspectProcessMemory,
    label_id: HealthMessageId::HealthTryInspectProcessMemory,
    reason_id: HealthMessageId::HealthTryReasonRecentOom,
    bash_surface: true,
};
const UNAVAILABLE_CHECKS_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::UnavailableChecks,
    legacy_id: "T08",
    prompt_id: HealthMessageId::HealthPromptReviewUnavailableChecks,
    label_id: HealthMessageId::HealthTryReviewUnavailableChecks,
    reason_id: HealthMessageId::HealthTryReasonMissingCoreCheck,
    bash_surface: false,
};

const SYSTEM_CAPACITY_ADVICE: AdviceDefinition = AdviceDefinition {
    kind: AdviceKind::SystemCapacity,
    legacy_id: "T09",
    prompt_id: HealthMessageId::HealthPromptInspectSystemCapacity,
    label_id: HealthMessageId::HealthTryInspectSystemCapacity,
    reason_id: HealthMessageId::HealthTryReasonSystemCapacity,
    bash_surface: true,
};

pub(crate) const ADVICE_DEFINITIONS: &[AdviceDefinition] = &[
    MEMORY_PRESSURE_ADVICE,
    SWAP_PRESSURE_ADVICE,
    RECENT_OOM_ADVICE,
    DISK_USAGE_ADVICE,
    SERVICE_STATUS_ADVICE,
    HIGH_LOAD_ADVICE,
    PROCESS_MEMORY_ADVICE,
    UNAVAILABLE_CHECKS_ADVICE,
    SYSTEM_CAPACITY_ADVICE,
];

pub(crate) fn advice_definition(kind: AdviceKind) -> &'static AdviceDefinition {
    match kind {
        AdviceKind::MemoryPressure => &MEMORY_PRESSURE_ADVICE,
        AdviceKind::SwapPressure => &SWAP_PRESSURE_ADVICE,
        AdviceKind::RecentOom => &RECENT_OOM_ADVICE,
        AdviceKind::DiskUsage => &DISK_USAGE_ADVICE,
        AdviceKind::ServiceStatus => &SERVICE_STATUS_ADVICE,
        AdviceKind::HighLoad => &HIGH_LOAD_ADVICE,
        AdviceKind::ProcessMemory => &PROCESS_MEMORY_ADVICE,
        AdviceKind::UnavailableChecks => &UNAVAILABLE_CHECKS_ADVICE,
        AdviceKind::SystemCapacity => &SYSTEM_CAPACITY_ADVICE,
    }
}

pub(crate) const RULE_ADVICE: &[(&str, &[AdviceKind])] = &[
    ("J01", &[]),
    ("J02", &[AdviceKind::UnavailableChecks]),
    ("J03", &[AdviceKind::HighLoad]),
    ("J04", &[AdviceKind::HighLoad]),
    ("J05", &[AdviceKind::MemoryPressure]),
    ("J06", &[AdviceKind::MemoryPressure]),
    ("J07", &[AdviceKind::SwapPressure]),
    ("J08", &[AdviceKind::SwapPressure]),
    ("J09", &[AdviceKind::DiskUsage]),
    ("J10", &[AdviceKind::DiskUsage]),
    ("J17", &[AdviceKind::DiskUsage]),
    ("J18", &[AdviceKind::DiskUsage]),
    ("J11", &[AdviceKind::RecentOom, AdviceKind::ProcessMemory]),
    ("J12", &[AdviceKind::RecentOom, AdviceKind::ProcessMemory]),
    ("J13", &[AdviceKind::RecentOom, AdviceKind::ProcessMemory]),
    ("J14", &[]),
    ("J15", &[AdviceKind::ServiceStatus]),
    ("J16", &[AdviceKind::ServiceStatus]),
    ("J19", &[AdviceKind::MemoryPressure]),
    ("J20", &[AdviceKind::MemoryPressure]),
    ("J21", &[AdviceKind::DiskUsage]),
    ("J22", &[AdviceKind::HighLoad]),
    ("J23", &[AdviceKind::SystemCapacity]),
    ("J24", &[AdviceKind::DiskUsage]),
    ("J25", &[AdviceKind::DiskUsage]),
    ("J26", &[AdviceKind::DiskUsage]),
    ("J27", &[AdviceKind::DiskUsage]),
    ("J28", &[AdviceKind::DiskUsage]),
    ("J29", &[AdviceKind::DiskUsage]),
];

pub(crate) fn advice_for_rule(finding_id: &str) -> &'static [AdviceKind] {
    let base = finding_id.split(':').next().unwrap_or(finding_id);
    RULE_ADVICE
        .iter()
        .find(|(rule, _)| *rule == base)
        .map(|(_, kinds)| *kinds)
        .unwrap_or(&[])
}

pub(crate) fn advice_for_prompt(prompt_id: HealthMessageId) -> Option<&'static AdviceDefinition> {
    ADVICE_DEFINITIONS
        .iter()
        .find(|definition| definition.prompt_id == prompt_id)
}

const ROOT_FILESYSTEM_KEYS: &[&str] = &[
    "filesystem.root_used_ratio",
    "filesystem.root_available_gib",
    "filesystem.mount./.path",
    "filesystem.mount./.used_ratio",
    "filesystem.mount./.available_gib",
    "filesystem.mount./.volume_proven",
    "filesystem.mount./.volume_unknown_reason",
    "filesystem.mount./.volume_fs_type",
    "filesystem.mount./.volume_device",
    "filesystem.mount./.volume_fsid",
    "filesystem.mount./.volume_bavail_bytes",
    "filesystem.mount./.volume_bfree_bytes",
    "filesystem.mount./.volume_used_bytes",
    "filesystem.mount./.volume_files",
    "filesystem.mount./.volume_favail",
    "filesystem.mount./.volume_inode_applicable",
    "filesystem.mount./.volume_readonly",
    "filesystem.mount./.volume_expected_writable",
    "filesystem.volume_*",
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
library_tests! {
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
    fn source_definitions_cover_every_sample_source_once() {
        for source in [
            S::Load,
            S::Meminfo,
            S::Host,
            S::Filesystems,
            S::Kernel,
            S::Services,
            S::Environment,
        ] {
            let count = SOURCES.iter().filter(|item| item.source == source).count();
            assert_eq!(count, 1, "{source:?}");
        }
        assert_eq!(SOURCES.len(), 7);
    }

    #[test]
    fn passive_budgets_derive_from_source_definitions() {
        for check in CHECKS {
            let budget = source_definition(check.source).passive_budget_ms;
            let expected = match check.source {
                S::Load | S::Meminfo | S::Host => 30,
                S::Filesystems => 100,
                S::Kernel => 120,
                S::Services => 800,
                S::Environment => 100,
            };
            assert_eq!(budget, expected, "{}", check.id);
        }
    }

    #[test]
    fn command_sources_retire_and_file_sources_reuse() {
        for source in [S::Filesystems, S::Kernel, S::Services] {
            assert_eq!(
                source_definition(source).finish_policy,
                FinishPolicy::RetireAfterSource,
                "{source:?}"
            );
        }
        for source in [S::Load, S::Meminfo, S::Host, S::Environment] {
            assert_eq!(
                source_definition(source).finish_policy,
                FinishPolicy::ReuseOnSuccess,
                "{source:?}"
            );
        }
    }

    #[test]
    fn rule_advice_declares_every_rule_exactly_once() {
        let mut declared: Vec<&str> = RULE_ADVICE.iter().map(|(rule, _)| *rule).collect();
        declared.sort_unstable();
        let expected: Vec<String> = (1..=29).map(|index| format!("J{index:02}")).collect();
        assert_eq!(declared, expected);
        assert!(advice_for_rule("J01").is_empty());
        assert!(advice_for_rule("J14").is_empty());
        assert!(advice_for_rule("J99").is_empty());
    }

    #[test]
    fn dynamic_rule_ids_resolve_base_associations() {
        assert_eq!(advice_for_rule("J09:/data"), &[AdviceKind::DiskUsage]);
        assert_eq!(advice_for_rule("J10:/"), &[AdviceKind::DiskUsage]);
        assert_eq!(advice_for_rule("J15:nginx"), &[AdviceKind::ServiceStatus]);
        assert_eq!(advice_for_rule("J16:sshd"), &[AdviceKind::ServiceStatus]);
        assert_eq!(
            advice_for_rule("J11"),
            &[AdviceKind::RecentOom, AdviceKind::ProcessMemory]
        );
    }

    #[test]
    fn root_check_owns_every_volume_fact_it_records() {
        for key in [
            "filesystem.mount./.volume_proven",
            "filesystem.mount./.volume_unknown_reason",
            "filesystem.mount./.volume_fs_type",
            "filesystem.mount./.volume_device",
            "filesystem.mount./.volume_fsid",
            "filesystem.mount./.volume_bavail_bytes",
            "filesystem.mount./.volume_bfree_bytes",
            "filesystem.mount./.volume_used_bytes",
            "filesystem.mount./.volume_files",
            "filesystem.mount./.volume_favail",
            "filesystem.mount./.volume_inode_applicable",
            "filesystem.volume_over_limit",
            "filesystem.volume_mnt_ns",
        ] {
            assert!(owns_fact("filesystem.root", key), "{key}");
        }
        assert!(owns_fact("filesystem.mounts", "filesystem.mount./data.volume_proven"));
        assert!(!owns_fact("filesystem.mounts", "filesystem.mount./.volume_proven"));
        assert!(owns_fact("filesystem.mounts", "filesystem.volume_mnt_ns"));
        assert!(owns_fact("filesystem.mounts", "filesystem.volume_over_limit"));
    }

    #[test]
    fn advice_definitions_keep_legacy_ids_and_bash_scope() {
        let ids: Vec<&str> = ADVICE_DEFINITIONS
            .iter()
            .map(|item| item.legacy_id)
            .collect();
        assert_eq!(
            ids,
            ["T01", "T02", "T03", "T04", "T05", "T06", "T07", "T08", "T09"]
        );
        let bash: Vec<&str> = ADVICE_DEFINITIONS
            .iter()
            .filter(|item| item.bash_surface)
            .map(|item| item.legacy_id)
            .collect();
        assert_eq!(bash, ["T01", "T02", "T03", "T04", "T06", "T07", "T09"]);
        for definition in ADVICE_DEFINITIONS {
            assert_eq!(
                advice_definition(definition.kind).legacy_id,
                definition.legacy_id
            );
        }
    }

    #[test]
    fn prompt_ids_resolve_advice_definitions() {
        assert_eq!(
            advice_for_prompt(HealthMessageId::HealthPromptAnalyzeMemoryPressure)
                .unwrap()
                .legacy_id,
            "T01"
        );
        assert!(
            advice_for_prompt(HealthMessageId::HealthPromptInspectServiceStatus)
                .is_some_and(|definition| !definition.bash_surface)
        );
        assert!(advice_for_prompt(HealthMessageId::HealthFindingRecentOom).is_none());
    }

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
        assert!(owns_fact("resource.memory", "memory.total_kib"));
        assert!(owns_fact("resource.memory", "memory.available_kib"));
    }
}
}
