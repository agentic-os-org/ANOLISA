pub(crate) mod builder;
pub(crate) mod collectors;
pub(crate) mod context;
pub(crate) mod env_collectors;
pub(crate) mod model;
mod prompt_selection;
pub(crate) mod recommendation;
pub(crate) mod rules;
pub(crate) mod runtime;
pub(crate) mod status;
pub(crate) mod suppression;

pub(crate) use builder::{health_scan_id, HealthReportBuilder};
pub(crate) use collectors::{
    collect_configured_services, collect_cpu, collect_disk, collect_host, collect_kernel_signals,
    collect_memory, parse_df_p, parse_kernel_signals, parse_os_release_pretty_name,
    parse_proc_loadavg, parse_proc_meminfo, parse_proc_stat_cpu_cores, parse_proc_uptime_seconds,
    parse_systemctl_is_active, run_health_command, CpuLoadFacts, DiskFacts, HealthCommandConfig,
    HealthCommandError, HealthCommandOutput, KernelSignalFacts, MemoryFacts, ServiceStatusFact,
};
pub(crate) use context::health_context_hint;
pub(crate) use env_collectors::run_env_collectors;
pub(crate) use model::{
    HealthCollector, HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue,
    HealthFinding, HealthFindingCategory, HealthMessageId, HealthScanReport, HealthSeverity,
    HealthTryItem, HealthTryKind, HealthUnavailableReason, UnavailableCollector,
};
pub(crate) use prompt_selection::{sorted_findings, sorted_try_items, startup_prompt_suggestions};
pub(crate) use recommendation::{apply_try_recommendations, generate_try_recommendations};
pub(crate) use rules::{apply_judgement_rules, evaluate_judgement_rules};
pub(crate) use runtime::{
    health_scan_mode_from_env, record_startup_health_recommendations, run_health_scan,
    run_health_scan_with_options, spawn_startup_health_scan, startup_health_scan_enabled_for_env,
    HealthScanMode, HealthScanOptions,
};
pub(crate) use status::{doctor_status, finding_remediation, report_exit_code, DoctorStatus};
pub(crate) use suppression::{
    current_time_ms, health_suppression_store_path, health_suppression_store_path_in_dir,
    host_id_for_report, HealthSuppressionEntry, HealthSuppressionStore, SuppressionDecision,
};
