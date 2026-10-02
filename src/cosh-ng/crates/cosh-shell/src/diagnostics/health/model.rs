use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum HealthSeverity {
    Ok,
    Unavailable,
    Degraded,
    Warning,
    Critical,
}

impl HealthSeverity {
    pub(crate) fn precedence(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Unavailable => 1,
            Self::Degraded => 2,
            Self::Warning => 3,
            Self::Critical => 4,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unavailable => "unavailable",
            Self::Degraded => "degraded",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "ok" => Some(Self::Ok),
            "unavailable" => Some(Self::Unavailable),
            "degraded" => Some(Self::Degraded),
            "warning" => Some(Self::Warning),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HealthScanReport {
    pub(crate) scan_id: String,
    pub(crate) host: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) started_at_ms: u128,
    pub(crate) elapsed_ms: u128,
    pub(crate) overall_severity: HealthSeverity,
    pub(crate) health_score: Option<u8>,
    pub(crate) facts: Vec<HealthFact>,
    pub(crate) findings: Vec<HealthFinding>,
    pub(crate) unavailable: Vec<UnavailableCollector>,
    pub(crate) checks_done: Vec<String>,
    pub(crate) try_items: Vec<HealthTryItem>,
}

impl HealthScanReport {
    pub(crate) fn new(scan_id: impl Into<String>, started_at_ms: u128) -> Self {
        Self {
            scan_id: scan_id.into(),
            host: None,
            role: None,
            started_at_ms,
            elapsed_ms: 0,
            overall_severity: HealthSeverity::Ok,
            health_score: None,
            facts: Vec::new(),
            findings: Vec::new(),
            unavailable: Vec::new(),
            checks_done: Vec::new(),
            try_items: Vec::new(),
        }
    }

    pub(crate) fn recompute_overall_severity(&mut self) {
        self.overall_severity = self
            .findings
            .iter()
            .map(|finding| finding.severity)
            .chain(self.unavailable.iter().map(|item| item.severity))
            .max_by_key(|severity| severity.precedence())
            .unwrap_or(HealthSeverity::Ok);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HealthFact {
    pub(crate) id: String,
    pub(crate) category: HealthFactCategory,
    pub(crate) key: String,
    pub(crate) value: HealthFactValue,
    pub(crate) unit: Option<String>,
    pub(crate) source: HealthFactSource,
    pub(crate) elapsed_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HealthFactCategory {
    Host,
    Cpu,
    Memory,
    Disk,
    Kernel,
    Service,
    Provider,
    Config,
    Hooks,
    Pty,
    Permissions,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum HealthFactValue {
    String(String),
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HealthFactSource {
    Hostname,
    OsRelease,
    ProcUptime,
    ProcLoadavg,
    ProcMeminfo,
    DfP,
    JournalctlK,
    Dmesg,
    Systemctl,
    Derived,
    Fixture,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HealthFinding {
    pub(crate) id: String,
    pub(crate) severity: HealthSeverity,
    pub(crate) category: HealthFindingCategory,
    pub(crate) title_id: HealthMessageId,
    pub(crate) detail_id: Option<HealthMessageId>,
    pub(crate) detail_args: BTreeMap<String, String>,
    pub(crate) evidence_fact_ids: Vec<String>,
    pub(crate) suggested_try_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HealthFindingCategory {
    RootCause,
    Anomaly,
    Observation,
    CollectionGap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnavailableCollector {
    pub(crate) collector: HealthCollector,
    pub(crate) reason: HealthUnavailableReason,
    pub(crate) severity: HealthSeverity,
    pub(crate) elapsed_ms: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HealthCollector {
    Host,
    Cpu,
    Memory,
    Disk,
    KernelSignal,
    ConfiguredService,
    Provider,
    Config,
    Hooks,
    Pty,
    Permissions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HealthUnavailableReason {
    Unsupported,
    PermissionDenied,
    CommandMissing,
    Timeout,
    ParseError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HealthTryItem {
    pub(crate) id: String,
    pub(crate) label_id: HealthMessageId,
    pub(crate) label_args: BTreeMap<String, String>,
    pub(crate) prompt_id: Option<HealthMessageId>,
    pub(crate) prompt_args: BTreeMap<String, String>,
    pub(crate) kind: HealthTryKind,
    pub(crate) command: Option<String>,
    pub(crate) reason_id: HealthMessageId,
    pub(crate) reason_args: BTreeMap<String, String>,
    pub(crate) score: i32,
    pub(crate) finding_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HealthTryKind {
    AskAgent,
    DisplayCommand,
}

#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum HealthMessageId {
    HealthBannerTitle,
    HealthBannerTryLabel,
    HealthBannerFindingLabel,
    HealthBannerEvidenceLabel,
    HealthBannerMoreFindingsLabel,
    HealthBannerUnavailableLabel,
    HealthSeverityOk,
    HealthSeverityWarning,
    HealthSeverityCritical,
    HealthSeverityDegraded,
    HealthSeverityUnavailable,
    HealthMetricCpu,
    HealthMetricMemory,
    HealthMetricSwap,
    HealthMetricDisk,
    HealthMetricSignal,
    HealthMetricService,
    HealthTryAnalyzeMemoryPressure,
    HealthTryCheckSwapPressure,
    HealthTryCheckRecentOom,
    HealthTryInspectDiskUsage,
    HealthTryInspectServiceStatus,
    HealthTryInspectHighLoad,
    HealthTryInspectProcessMemory,
    HealthTryReviewUnavailableChecks,
    HealthPromptAnalyzeMemoryPressure,
    HealthPromptCheckSwapPressure,
    HealthPromptCheckRecentOom,
    HealthPromptInspectDiskUsage,
    HealthPromptInspectServiceStatus,
    HealthPromptInspectHighLoad,
    HealthPromptInspectProcessMemory,
    HealthPromptReviewUnavailableChecks,
    HealthInsightMemoryAvailableLow,
    HealthInsightDiskHigh,
    HealthInsightRecentOom,
    HealthInsightCpuLoadHigh,
    HealthInsightSwapPressure,
    HealthInsightServiceState,
    HealthInsightGeneric,
    HealthUnavailablePermissionDenied,
    HealthUnavailableCommandMissing,
    HealthUnavailableTimeout,
    HealthUnavailableUnsupported,
    HealthUnavailableParseError,
    HealthFindingPlatformUnsupported,
    HealthFindingCoreCollectorUnavailable,
    HealthFindingCpuLoadHigh,
    HealthFindingMemoryAvailableLow,
    HealthFindingSwapPressure,
    HealthFindingDiskHigh,
    HealthFindingRecentOom,
    HealthFindingKernelPanic,
    HealthFindingServiceFailed,
    HealthFindingServiceInactive,
    HealthFindingProviderUnconfigured,
    HealthFindingConfigUnavailable,
    HealthFindingHooksUntrusted,
    HealthFindingPtyUnavailable,
    HealthFindingPermissionsUnwritable,
    HealthRemediationProvider,
    HealthRemediationUnknownAdapter,
    HealthRemediationConfig,
    HealthRemediationConfigUnreadable,
    HealthRemediationConfigInvalid,
    HealthRemediationHooks,
    HealthRemediationPty,
    HealthRemediationPermissions,
    HealthTryReasonMemoryLow,
    HealthTryReasonSwapWithContext,
    HealthTryReasonRecentOom,
    HealthTryReasonDiskHigh,
    HealthTryReasonServiceState,
    HealthTryReasonHighLoad,
    HealthTryReasonMissingCoreCheck,
}

impl HealthMessageId {
    pub(crate) fn to_i18n(self) -> crate::MessageId {
        match self {
            Self::HealthBannerTitle => crate::MessageId::HealthBannerTitle,
            Self::HealthBannerTryLabel => crate::MessageId::HealthBannerTryLabel,
            Self::HealthBannerFindingLabel => crate::MessageId::HealthBannerFindingLabel,
            Self::HealthBannerEvidenceLabel => crate::MessageId::HealthBannerEvidenceLabel,
            Self::HealthBannerMoreFindingsLabel => crate::MessageId::HealthBannerMoreFindingsLabel,
            Self::HealthBannerUnavailableLabel => crate::MessageId::HealthBannerUnavailableLabel,
            Self::HealthSeverityOk => crate::MessageId::HealthSeverityOk,
            Self::HealthSeverityWarning => crate::MessageId::HealthSeverityWarning,
            Self::HealthSeverityCritical => crate::MessageId::HealthSeverityCritical,
            Self::HealthSeverityDegraded => crate::MessageId::HealthSeverityDegraded,
            Self::HealthSeverityUnavailable => crate::MessageId::HealthSeverityUnavailable,
            Self::HealthMetricCpu => crate::MessageId::HealthMetricCpu,
            Self::HealthMetricMemory => crate::MessageId::HealthMetricMemory,
            Self::HealthMetricSwap => crate::MessageId::HealthMetricSwap,
            Self::HealthMetricDisk => crate::MessageId::HealthMetricDisk,
            Self::HealthMetricSignal => crate::MessageId::HealthMetricSignal,
            Self::HealthMetricService => crate::MessageId::HealthMetricService,
            Self::HealthTryAnalyzeMemoryPressure => {
                crate::MessageId::HealthTryAnalyzeMemoryPressure
            }
            Self::HealthTryCheckSwapPressure => crate::MessageId::HealthTryCheckSwapPressure,
            Self::HealthTryCheckRecentOom => crate::MessageId::HealthTryCheckRecentOom,
            Self::HealthTryInspectDiskUsage => crate::MessageId::HealthTryInspectDiskUsage,
            Self::HealthTryInspectServiceStatus => crate::MessageId::HealthTryInspectServiceStatus,
            Self::HealthTryInspectHighLoad => crate::MessageId::HealthTryInspectHighLoad,
            Self::HealthTryInspectProcessMemory => crate::MessageId::HealthTryInspectProcessMemory,
            Self::HealthTryReviewUnavailableChecks => {
                crate::MessageId::HealthTryReviewUnavailableChecks
            }
            Self::HealthPromptAnalyzeMemoryPressure => {
                crate::MessageId::HealthPromptAnalyzeMemoryPressure
            }
            Self::HealthPromptCheckSwapPressure => crate::MessageId::HealthPromptCheckSwapPressure,
            Self::HealthPromptCheckRecentOom => crate::MessageId::HealthPromptCheckRecentOom,
            Self::HealthPromptInspectDiskUsage => crate::MessageId::HealthPromptInspectDiskUsage,
            Self::HealthPromptInspectServiceStatus => {
                crate::MessageId::HealthPromptInspectServiceStatus
            }
            Self::HealthPromptInspectHighLoad => crate::MessageId::HealthPromptInspectHighLoad,
            Self::HealthPromptInspectProcessMemory => {
                crate::MessageId::HealthPromptInspectProcessMemory
            }
            Self::HealthPromptReviewUnavailableChecks => {
                crate::MessageId::HealthPromptReviewUnavailableChecks
            }
            Self::HealthInsightMemoryAvailableLow => {
                crate::MessageId::HealthInsightMemoryAvailableLow
            }
            Self::HealthInsightDiskHigh => crate::MessageId::HealthInsightDiskHigh,
            Self::HealthInsightRecentOom => crate::MessageId::HealthInsightRecentOom,
            Self::HealthInsightCpuLoadHigh => crate::MessageId::HealthInsightCpuLoadHigh,
            Self::HealthInsightSwapPressure => crate::MessageId::HealthInsightSwapPressure,
            Self::HealthInsightServiceState => crate::MessageId::HealthInsightServiceState,
            Self::HealthInsightGeneric => crate::MessageId::HealthInsightGeneric,
            Self::HealthUnavailablePermissionDenied => {
                crate::MessageId::HealthUnavailablePermissionDenied
            }
            Self::HealthUnavailableCommandMissing => {
                crate::MessageId::HealthUnavailableCommandMissing
            }
            Self::HealthUnavailableTimeout => crate::MessageId::HealthUnavailableTimeout,
            Self::HealthUnavailableUnsupported => crate::MessageId::HealthUnavailableUnsupported,
            Self::HealthUnavailableParseError => crate::MessageId::HealthUnavailableParseError,
            Self::HealthFindingPlatformUnsupported => {
                crate::MessageId::HealthFindingPlatformUnsupported
            }
            Self::HealthFindingCoreCollectorUnavailable => {
                crate::MessageId::HealthFindingCoreCollectorUnavailable
            }
            Self::HealthFindingCpuLoadHigh => crate::MessageId::HealthFindingCpuLoadHigh,
            Self::HealthFindingMemoryAvailableLow => {
                crate::MessageId::HealthFindingMemoryAvailableLow
            }
            Self::HealthFindingSwapPressure => crate::MessageId::HealthFindingSwapPressure,
            Self::HealthFindingDiskHigh => crate::MessageId::HealthFindingDiskHigh,
            Self::HealthFindingRecentOom => crate::MessageId::HealthFindingRecentOom,
            Self::HealthFindingKernelPanic => crate::MessageId::HealthFindingKernelPanic,
            Self::HealthFindingServiceFailed => crate::MessageId::HealthFindingServiceFailed,
            Self::HealthFindingServiceInactive => crate::MessageId::HealthFindingServiceInactive,
            Self::HealthFindingProviderUnconfigured => {
                crate::MessageId::HealthFindingProviderUnconfigured
            }
            Self::HealthFindingConfigUnavailable => {
                crate::MessageId::HealthFindingConfigUnavailable
            }
            Self::HealthFindingHooksUntrusted => crate::MessageId::HealthFindingHooksUntrusted,
            Self::HealthFindingPtyUnavailable => crate::MessageId::HealthFindingPtyUnavailable,
            Self::HealthFindingPermissionsUnwritable => {
                crate::MessageId::HealthFindingPermissionsUnwritable
            }
            Self::HealthRemediationProvider => crate::MessageId::HealthRemediationProvider,
            Self::HealthRemediationUnknownAdapter => {
                crate::MessageId::HealthRemediationUnknownAdapter
            }
            Self::HealthRemediationConfig => crate::MessageId::HealthRemediationConfig,
            Self::HealthRemediationConfigUnreadable => {
                crate::MessageId::HealthRemediationConfigUnreadable
            }
            Self::HealthRemediationConfigInvalid => {
                crate::MessageId::HealthRemediationConfigInvalid
            }
            Self::HealthRemediationHooks => crate::MessageId::HealthRemediationHooks,
            Self::HealthRemediationPty => crate::MessageId::HealthRemediationPty,
            Self::HealthRemediationPermissions => crate::MessageId::HealthRemediationPermissions,
            Self::HealthTryReasonMemoryLow => crate::MessageId::HealthTryReasonMemoryLow,
            Self::HealthTryReasonSwapWithContext => {
                crate::MessageId::HealthTryReasonSwapWithContext
            }
            Self::HealthTryReasonRecentOom => crate::MessageId::HealthTryReasonRecentOom,
            Self::HealthTryReasonDiskHigh => crate::MessageId::HealthTryReasonDiskHigh,
            Self::HealthTryReasonServiceState => crate::MessageId::HealthTryReasonServiceState,
            Self::HealthTryReasonHighLoad => crate::MessageId::HealthTryReasonHighLoad,
            Self::HealthTryReasonMissingCoreCheck => {
                crate::MessageId::HealthTryReasonMissingCoreCheck
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_precedence_matches_sdd() {
        let ordered = [
            HealthSeverity::Ok,
            HealthSeverity::Unavailable,
            HealthSeverity::Degraded,
            HealthSeverity::Warning,
            HealthSeverity::Critical,
        ];
        assert!(ordered
            .windows(2)
            .all(|pair| pair[0].precedence() < pair[1].precedence()));
    }

    #[test]
    fn report_recomputes_overall_from_findings_and_unavailable() {
        let mut report = HealthScanReport::new("health-1", 0);
        report.unavailable.push(UnavailableCollector {
            collector: HealthCollector::KernelSignal,
            reason: HealthUnavailableReason::PermissionDenied,
            severity: HealthSeverity::Unavailable,
            elapsed_ms: 10,
        });
        report.findings.push(HealthFinding {
            id: "health-1-memory".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Anomaly,
            title_id: HealthMessageId::HealthFindingMemoryAvailableLow,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: vec!["memory.available_ratio".to_string()],
            suggested_try_ids: vec!["try-memory".to_string()],
        });

        report.recompute_overall_severity();

        assert_eq!(report.overall_severity, HealthSeverity::Warning);
    }

    #[test]
    fn health_message_ids_map_to_global_i18n_catalog() {
        assert_eq!(
            HealthMessageId::HealthTryAnalyzeMemoryPressure.to_i18n(),
            crate::MessageId::HealthTryAnalyzeMemoryPressure
        );
        assert_eq!(
            HealthMessageId::HealthFindingDiskHigh.to_i18n(),
            crate::MessageId::HealthFindingDiskHigh
        );
    }
}
