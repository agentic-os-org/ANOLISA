use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HealthBannerTitle => "Health check",
        MessageId::HealthStartupLabel => "Health",
        MessageId::HealthBannerTryLabel => "Prompt",
        MessageId::HealthBannerFindingLabel => "Finding",
        MessageId::HealthBannerEvidenceLabel => "Evidence",
        MessageId::HealthBannerMoreFindingsLabel => "{count} more finding(s)",
        MessageId::HealthBannerUnavailableLabel => "Unavailable",
        MessageId::HealthBannerFindingsSection => "Findings",
        MessageId::HealthBannerSuggestedPromptSection => "Suggested Prompts",
        MessageId::HealthBannerSuggestedPromptIntro => "You can type these prompts to the agent:",
        MessageId::HealthSeverityOk => "ok",
        MessageId::HealthSeverityWarning => "warning",
        MessageId::HealthSeverityCritical => "critical",
        MessageId::HealthSeverityDegraded => "degraded",
        MessageId::HealthSeverityUnavailable => "unavailable",
        MessageId::HealthMetricCpu => "CPU",
        MessageId::HealthMetricCpuLoadPerCore => "Load 1m",
        MessageId::HealthMetricLoad1mShort => "1m",
        MessageId::HealthMetricLoad5mShort => "5m",
        MessageId::HealthMetricLoadValue => "{load} / {cores} cores ({ratio}x)",
        MessageId::HealthMetricCpuPerCoreUnit => "x",
        MessageId::HealthMetricCpuUsed => "CPU used",
        MessageId::HealthMetricHost => "Host",
        MessageId::HealthMetricMemory => "Memory",
        MessageId::HealthMetricMemoryAvailable => "Mem avail",
        MessageId::HealthMetricMemoryUsed => "Mem used",
        MessageId::HealthMetricSwap => "Swap",
        MessageId::HealthMetricSwapUsed => "Swap used",
        MessageId::HealthMetricDisk => "Disk",
        MessageId::HealthMetricDiskUsed => "Disk used",
        MessageId::HealthMetricDiskMountUsed => "Disk {mount} used",
        MessageId::HealthMetricPressure => "Load",
        MessageId::HealthMetricLevels => "Resources",
        MessageId::HealthMetricSignal => "Signal",
        MessageId::HealthMetricService => "Service",
        MessageId::HealthEvidenceDiskAvailable => "{gib} GiB available",
        MessageId::HealthEvidenceMount => "mount {mount}",
        MessageId::HealthEvidenceOomKilledProcess => "killed {process}",
        MessageId::HealthEvidenceOomCgroup => "cgroup {cgroup}",
        MessageId::HealthEvidenceOomOneHourCount => "1h OOM {count}",
        MessageId::HealthEvidenceOomTwentyFourHourCount => "24h OOM {count}",
        MessageId::HealthEvidenceOomVictimKilledAgo => "{subject} killed {age} ago",
        MessageId::HealthEvidenceOomVictimKilled => "{subject} killed",
        MessageId::HealthEvidenceOomAge => "OOM {age} ago",
        MessageId::HealthOomScopeMemcg => "cgroup memory limit",
        MessageId::HealthOomScopeHost => "host memory pressure",
        MessageId::HealthOomScopeCpuset => "cpuset/NUMA memory pressure",
        MessageId::HealthOomScopeMemoryPolicy => "memory policy pressure",
        MessageId::HealthOomScopeUnknown => "unknown OOM scope",
        MessageId::HealthTryAnalyzeMemoryPressure => "analyze memory pressure",
        MessageId::HealthTryCheckSwapPressure => "check active swap pressure",
        MessageId::HealthTryCheckRecentOom => "analyze latest OOM cause",
        MessageId::HealthTryInspectDiskUsage => "inspect disk usage",
        MessageId::HealthTryInspectServiceStatus => "inspect service status",
        MessageId::HealthTryInspectHighLoad => "inspect high load",
        MessageId::HealthTryInspectProcessMemory => "inspect {process} memory",
        MessageId::HealthTryReviewUnavailableChecks => "review unavailable checks",
        MessageId::HealthPromptAnalyzeMemoryPressure => {
            "Analyze memory pressure and identify top consumers that may affect this shell."
        }
        MessageId::HealthPromptCheckSwapPressure => {
            "Check whether swap pressure is active and which processes are driving it."
        }
        MessageId::HealthPromptCheckRecentOom => {
            "Help me analyze the cause of the most recent OOM, focusing on the killed process, cgroup, and memory state around the event."
        }
        MessageId::HealthPromptInspectDiskUsage => {
            "Inspect the risky mount and suggest safe disk cleanup targets."
        }
        MessageId::HealthPromptInspectServiceStatus => {
            "Inspect the configured service state and likely failure cause."
        }
        MessageId::HealthPromptInspectHighLoad => {
            "Analyze high load and identify CPU or IO pressure sources."
        }
        MessageId::HealthPromptInspectProcessMemory => {
            "Help me analyze why the latest OOM killed {process}, focusing on cgroup scope and memory limits."
        }
        MessageId::HealthPromptReviewUnavailableChecks => {
            "Explain why startup health checks were unavailable and how to restore them."
        }
        MessageId::HealthInsightMemoryAvailableLow => {
            "available memory is low; this can slow commands or make new processes fail"
        }
        MessageId::HealthInsightDiskHigh => {
            "disk usage is high on the riskiest mount; writes or builds may fail soon"
        }
        MessageId::HealthInsightRecentOom => {
            "the latest OOM has already happened; review the killed process, cgroup, and memory state around the event"
        }
        MessageId::HealthInsightCpuLoadHigh => {
            "load stayed high across recent windows; commands may be delayed"
        }
        MessageId::HealthInsightSwapPressure => {
            "swap usage is high with memory pressure context; paging can slow command response"
        }
        MessageId::HealthInsightServiceState => {
            "service unit {service} observed {observed}, expected {expected}"
        }
        MessageId::HealthInsightGeneric => "startup health found a signal worth checking",
        MessageId::HealthUnavailablePermissionDenied => "permission denied",
        MessageId::HealthUnavailableCommandMissing => "command missing",
        MessageId::HealthUnavailableTimeout => "timed out",
        MessageId::HealthUnavailableUnsupported => "unsupported",
        MessageId::HealthUnavailableParseError => "parse error",
        MessageId::HealthFindingPlatformUnsupported => "platform unsupported",
        MessageId::HealthFindingCoreCollectorUnavailable => "core check unavailable",
        MessageId::HealthFindingCpuLoadHigh => "system load high",
        MessageId::HealthFindingMemoryAvailableLow => "available memory low",
        MessageId::HealthFindingSwapPressure => "swap pressure with context",
        MessageId::HealthFindingDiskHigh => "disk usage high",
        MessageId::HealthFindingRecentOom => "recent OOM signal",
        MessageId::HealthFindingKernelPanic => "recent kernel panic",
        MessageId::HealthFindingServiceFailed => "service failed",
        MessageId::HealthFindingServiceInactive => "service inactive",
        MessageId::HealthCollectorProvider => "Provider",
        MessageId::HealthCollectorConfig => "Config",
        MessageId::HealthCollectorHooks => "Hooks",
        MessageId::HealthCollectorPty => "PTY",
        MessageId::HealthCollectorPermissions => "Permissions",
        MessageId::DoctorTitle => "cosh-shell doctor",
        MessageId::DoctorStatusLabel => "status",
        MessageId::DoctorChecksLabel => "checks",
        MessageId::DoctorRemediationLabel => "remediation",
        MessageId::DoctorAllHealthy => "all checks passed",
        MessageId::HealthFindingProviderUnconfigured => "AI provider not ready",
        MessageId::HealthFindingConfigUnavailable => "configuration unavailable",
        MessageId::HealthFindingHooksUntrusted => "project hooks not trusted",
        MessageId::HealthFindingPtyUnavailable => "PTY support unavailable",
        MessageId::HealthFindingPermissionsUnwritable => "config directory not writable",
        MessageId::HealthRemediationProvider => {
            "configure credentials for adapter '{adapter}' (env or config.toml) or run /auth"
        }
        MessageId::HealthRemediationUnknownAdapter => {
            "'{adapter}' is not a supported adapter; set adapter_default to one of: fake, claude-code, qwen-cli, cosh-core"
        }
        MessageId::HealthRemediationConfig => {
            "set HOME so cosh-shell can resolve ~/.copilot-shell and load config"
        }
        MessageId::HealthRemediationConfigUnreadable => {
            "make ~/.copilot-shell/config.toml a readable file (not a directory) and fix its permissions"
        }
        MessageId::HealthRemediationConfigInvalid => {
            "repair ~/.copilot-shell/config.toml so cosh-shell can load it (valid TOML or recognized key=value entries)"
        }
        MessageId::HealthRemediationHooks => {
            "review and trust project hooks under {path} before they can run"
        }
        MessageId::HealthRemediationPty => {
            "run cosh-shell from a real terminal; interactive shell needs a PTY (/dev/ptmx)"
        }
        MessageId::HealthRemediationPermissions => {
            "fix permissions on {path} so cosh-shell can write config, logs and state"
        }
        MessageId::HealthTryReasonMemoryLow => "available memory is low",
        MessageId::HealthTryReasonSwapWithContext => "swap is high with pressure context",
        MessageId::HealthTryReasonRecentOom => "recent OOM is worth reviewing",
        MessageId::HealthTryReasonDiskHigh => "disk space is constrained",
        MessageId::HealthTryReasonServiceState => "configured service state is unexpected",
        MessageId::HealthTryReasonHighLoad => "load is elevated across recent windows",
        MessageId::HealthTryReasonMissingCoreCheck => "a core health check is unavailable",
        _ => return None,
    })
}
