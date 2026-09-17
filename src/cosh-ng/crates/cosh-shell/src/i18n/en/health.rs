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
        MessageId::HealthCollectorRuntime => "Runtime",
        MessageId::HealthCollectorLogs => "Logs",
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
        MessageId::HealthFindingOrphanCore => "orphaned cosh-core process (pid {pid})",
        MessageId::HealthFindingStaleEntry => {
            "{kind} entry left behind (pid {pid}, started {time})"
        }
        MessageId::HealthFindingCrash => "{kind} panicked at {time}: {panic}",
        MessageId::HealthFindingRecentErrors => {
            "{count} ERROR entries in {file} within 24h (last {time})"
        }
        MessageId::HealthFindingWarnFlood => {
            "{count} WARN entries in {file} within 24h (retry loop?)"
        }
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
        MessageId::HealthRemediationOrphanCore => {
            "no shell owns this core; stop it with: kill {pid}"
        }
        MessageId::HealthRemediationStaleEntry => {
            "correlate with crash records; run `cosh-shell diagnostics export` to collect evidence"
        }
        MessageId::HealthRemediationCrash => {
            "run `cosh-shell diagnostics export` and inspect the crashes section"
        }
        MessageId::HealthRemediationLogs => {
            "run `cosh-shell diagnostics export` and inspect the logs section"
        }
        MessageId::HealthTryReasonMemoryLow => "available memory is low",
        MessageId::HealthTryReasonSwapWithContext => "swap is high with pressure context",
        MessageId::HealthTryReasonRecentOom => "recent OOM is worth reviewing",
        MessageId::HealthTryReasonDiskHigh => "disk space is constrained",
        MessageId::HealthTryReasonServiceState => "configured service state is unexpected",
        MessageId::HealthTryReasonHighLoad => "load is elevated across recent windows",
        MessageId::HealthTryReasonMissingCoreCheck => "a core health check is unavailable",
        MessageId::HealthLiveSectionTitle => "live session",
        MessageId::HealthLiveCoreAlive => "core: alive (live registry response)",
        MessageId::HealthLiveCoreNoResponse => "core: no response ({reason})",
        MessageId::HealthLiveCoreNoRuntime => "core: no persistent runtime",
        MessageId::HealthLiveRecovery => "recovery: {state}",
        MessageId::HealthLiveRoutingFacts => {
            "routing: ai={ai}, assistance={assistance}, integration={integration}, marker generation={generation}"
        }
        MessageId::HealthLiveCnfHandler => "command-not-found handler: {ownership}",
        MessageId::HealthLiveLastRoute => "last route: {route}",
        MessageId::HealthLiveRoutingHint => {
            "no routing anomaly in live facts; if natural-language input still does not route, check wrapper coverage and marker generation"
        }
        MessageId::HealthFindingRouteFallback => {
            "routing compatibility fallback, not a provider failure: {reason}"
        }
        MessageId::HealthFindingCoreNoResponse => {
            "core did not respond to the live probe: {reason}"
        }
        MessageId::HealthFindingRecoveryFailed => {
            "session recovery is in an abnormal state ({state})"
        }
        MessageId::HealthRemediationRouteFallback => {
            "see the troubleshooting guide section on input routing"
        }
        MessageId::HealthRemediationCoreNoResponse => {
            "run `cosh-shell diagnostics export` to collect evidence, then restart cosh-shell"
        }
        MessageId::HealthLiveReasonAiDisabled => "AI is disabled",
        MessageId::HealthLiveReasonAssistanceOff => "assistance (routing) is off",
        MessageId::HealthLiveReasonUserCnf => {
            "a user command-not-found handler takes precedence"
        }
        MessageId::HealthLiveReasonCnfOverridden => {
            "the command-not-found handler was overridden after startup"
        }
        MessageId::HealthLiveReasonCnfMissing => {
            "the command-not-found handler was removed"
        }
        MessageId::DoctorVersionLine => {
            "version: cosh-shell {shell_version}, cosh-core {core_version}"
        }
        MessageId::DoctorHostLine => "host: {host}",
        MessageId::DoctorRuntimeLine => "runtime: {summary}",
        MessageId::DoctorRuntimeSummaryNone => "no active sessions",
        MessageId::DoctorRoutingLine => {
            "routing: ai={ai}, integration={integration}, command-not-found handler={cnf}, last route={route}"
        }
        MessageId::DoctorRoutingUnavailable => {
            "routing: live probe unavailable; run /health inside the affected session"
        }
        MessageId::DoctorLogsLine => {
            "logs: level={level}, last write {last}; 24h errors: {errors}"
        }
        MessageId::DoctorLogsNoFiles => "no log files",
        MessageId::DoctorCrashesLine => "crashes: {count} in the last 24h{detail}",
        MessageId::DoctorExportHint => {
            "next step: run `cosh-shell diagnostics export` to collect a redacted evidence bundle (see docs/user-guide/en/user-entrypoint/cosh-ng/troubleshooting.md)"
        }
        MessageId::HelpDiagnosticsHint => {
            "Troubleshooting: run /health inside the session, or `cosh-shell doctor` after exit; see docs/user-guide/en/user-entrypoint/cosh-ng/troubleshooting.md"
        }
        _ => return None,
    })
}
