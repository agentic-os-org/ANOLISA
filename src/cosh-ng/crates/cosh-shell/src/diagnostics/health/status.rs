//! Three-level doctor status derived from the internal 5-level
//! [`HealthSeverity`], plus the stable exit-code contract shared by the
//! `cosh-shell doctor` CLI and the `/health` slash command.

use crate::I18n;

use super::model::{HealthFinding, HealthScanReport, HealthSeverity};

/// Public, stable 3-level classification presented to users and automation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DoctorStatus {
    Healthy,
    Warning,
    Error,
}

impl DoctorStatus {
    /// Stable exit-code contract: healthy=0, warning=1, error=2.
    pub(crate) fn exit_code(self) -> i32 {
        match self {
            Self::Healthy => 0,
            Self::Warning => 1,
            Self::Error => 2,
        }
    }

    /// Stable ASCII token presented to users and automation. Language-neutral
    /// on purpose: this is part of the public doctor/health contract.
    pub(crate) fn token(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Collapse the internal 5-level severity into the public 3-level status.
///
/// `Unavailable`/`Degraded`/`Warning` all surface as `Warning` (the run
/// succeeded but something needs attention, including checks that could not
/// run). Only `Critical` escalates to `Error`.
pub(crate) fn status_from_severity(severity: HealthSeverity) -> DoctorStatus {
    match severity {
        HealthSeverity::Ok => DoctorStatus::Healthy,
        HealthSeverity::Unavailable | HealthSeverity::Degraded | HealthSeverity::Warning => {
            DoctorStatus::Warning
        }
        HealthSeverity::Critical => DoctorStatus::Error,
    }
}

/// Overall 3-level status for a completed report.
pub(crate) fn doctor_status(report: &HealthScanReport) -> DoctorStatus {
    status_from_severity(report.overall_severity)
}

/// Convenience: exit code for a completed report.
pub(crate) fn report_exit_code(report: &HealthScanReport) -> i32 {
    doctor_status(report).exit_code()
}

/// Short, actionable remediation text attached to a failed check.
///
/// Remediation is carried on the finding's `detail_id`/`detail_args`; findings
/// without a detail id have no dedicated remediation string.
pub(crate) fn finding_remediation(finding: &HealthFinding, i18n: I18n) -> Option<String> {
    let detail_id = finding.detail_id?;
    let args: Vec<(&str, &str)> = finding
        .detail_args
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    Some(i18n.format(detail_id.to_i18n(), &args))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config::Language;
    use crate::diagnostics::health::{
        HealthFinding, HealthFindingCategory, HealthMessageId, HealthScanReport, HealthSeverity,
    };

    #[test]
    fn severity_maps_to_three_level_status() {
        assert_eq!(
            status_from_severity(HealthSeverity::Ok),
            DoctorStatus::Healthy
        );
        assert_eq!(
            status_from_severity(HealthSeverity::Unavailable),
            DoctorStatus::Warning
        );
        assert_eq!(
            status_from_severity(HealthSeverity::Degraded),
            DoctorStatus::Warning
        );
        assert_eq!(
            status_from_severity(HealthSeverity::Warning),
            DoctorStatus::Warning
        );
        assert_eq!(
            status_from_severity(HealthSeverity::Critical),
            DoctorStatus::Error
        );
    }

    #[test]
    fn exit_codes_are_stable() {
        assert_eq!(DoctorStatus::Healthy.exit_code(), 0);
        assert_eq!(DoctorStatus::Warning.exit_code(), 1);
        assert_eq!(DoctorStatus::Error.exit_code(), 2);
    }

    #[test]
    fn status_tokens_are_stable() {
        assert_eq!(DoctorStatus::Healthy.token(), "healthy");
        assert_eq!(DoctorStatus::Warning.token(), "warning");
        assert_eq!(DoctorStatus::Error.token(), "error");
    }

    #[test]
    fn report_exit_code_uses_overall_severity() {
        let mut report = HealthScanReport::new("health-1", 0);
        report.overall_severity = HealthSeverity::Critical;
        assert_eq!(report_exit_code(&report), 2);
        report.overall_severity = HealthSeverity::Ok;
        assert_eq!(report_exit_code(&report), 0);
    }

    #[test]
    fn finding_remediation_formats_detail_with_args() {
        let mut detail_args = BTreeMap::new();
        detail_args.insert("adapter".to_string(), "cosh-core".to_string());
        let finding = HealthFinding {
            id: "env-provider".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Observation,
            title_id: HealthMessageId::HealthFindingProviderUnconfigured,
            detail_id: Some(HealthMessageId::HealthRemediationProvider),
            detail_args,
            evidence_fact_ids: Vec::new(),
            suggested_try_ids: Vec::new(),
        };

        let remediation =
            finding_remediation(&finding, I18n::new(Language::EnUs)).expect("remediation text");
        assert!(remediation.contains("cosh-core"), "{remediation}");
    }

    #[test]
    fn finding_without_detail_has_no_remediation() {
        let finding = HealthFinding {
            id: "no-detail".to_string(),
            severity: HealthSeverity::Warning,
            category: HealthFindingCategory::Observation,
            title_id: HealthMessageId::HealthFindingCpuLoadHigh,
            detail_id: None,
            detail_args: BTreeMap::new(),
            evidence_fact_ids: Vec::new(),
            suggested_try_ids: Vec::new(),
        };
        assert!(finding_remediation(&finding, I18n::new(Language::EnUs)).is_none());
    }
}
