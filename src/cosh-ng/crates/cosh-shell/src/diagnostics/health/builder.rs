use std::collections::BTreeMap;

use super::model::{
    HealthCollector, HealthFact, HealthFactCategory, HealthFactSource, HealthFactValue,
    HealthFinding, HealthScanReport, HealthSeverity, HealthUnavailableReason, UnavailableCollector,
};

pub(crate) fn health_scan_id(started_at_ms: u128) -> String {
    format!("health-{started_at_ms}")
}

#[derive(Debug)]
pub(crate) struct HealthReportBuilder {
    report: HealthScanReport,
    fact_key_counts: BTreeMap<String, usize>,
}

impl HealthReportBuilder {
    pub(crate) fn new(scan_id: impl Into<String>, started_at_ms: u128) -> Self {
        Self {
            report: HealthScanReport::new(scan_id, started_at_ms),
            fact_key_counts: BTreeMap::new(),
        }
    }

    pub(crate) fn for_started_at(started_at_ms: u128) -> Self {
        Self::new(health_scan_id(started_at_ms), started_at_ms)
    }

    pub(crate) fn set_host(&mut self, host: impl Into<String>) -> &mut Self {
        self.report.host = Some(host.into());
        self
    }

    pub(crate) fn set_role(&mut self, role: Option<String>) -> &mut Self {
        self.report.role = role;
        self
    }

    pub(crate) fn set_health_score(&mut self, score: Option<u8>) -> &mut Self {
        self.report.health_score = score;
        self
    }

    pub(crate) fn add_check_done(&mut self, check: impl Into<String>) -> &mut Self {
        self.report.checks_done.push(check.into());
        self
    }

    pub(crate) fn report(&self) -> &HealthScanReport {
        &self.report
    }

    pub(crate) fn add_fact(
        &mut self,
        category: HealthFactCategory,
        key: impl Into<String>,
        value: HealthFactValue,
        unit: Option<String>,
        source: HealthFactSource,
        elapsed_ms: u128,
    ) -> &mut Self {
        let key = key.into();
        let id = self.next_fact_id(&key);
        self.report.facts.push(HealthFact {
            id,
            category,
            key,
            value,
            unit,
            source,
            elapsed_ms,
        });
        self
    }

    pub(crate) fn add_unavailable(
        &mut self,
        collector: HealthCollector,
        reason: HealthUnavailableReason,
        severity: HealthSeverity,
        elapsed_ms: u128,
    ) -> &mut Self {
        self.report.unavailable.push(UnavailableCollector {
            collector,
            reason,
            severity,
            elapsed_ms,
        });
        self
    }

    pub(crate) fn add_finding(&mut self, finding: HealthFinding) -> &mut Self {
        self.report.findings.push(finding);
        self
    }

    pub(crate) fn merge_report(&mut self, report: HealthScanReport) -> &mut Self {
        if self.report.host.is_none() {
            self.report.host = report.host;
        }
        if self.report.role.is_none() {
            self.report.role = report.role;
        }
        if self.report.health_score.is_none() {
            self.report.health_score = report.health_score;
        }
        for fact in report.facts {
            self.add_fact(
                fact.category,
                fact.key,
                fact.value,
                fact.unit,
                fact.source,
                fact.elapsed_ms,
            );
        }
        self.report.unavailable.extend(report.unavailable);
        self.report.checks_done.extend(report.checks_done);
        self.report.findings.extend(report.findings);
        self.report.try_items.extend(report.try_items);
        self
    }

    pub(crate) fn finish(mut self, finished_at_ms: u128) -> HealthScanReport {
        self.report.elapsed_ms = finished_at_ms.saturating_sub(self.report.started_at_ms);
        self.report.recompute_overall_severity();
        self.report
    }

    fn next_fact_id(&mut self, key: &str) -> String {
        let count = self.fact_key_counts.entry(key.to_string()).or_insert(0);
        *count += 1;
        if *count == 1 {
            key.to_string()
        } else {
            format!("{key}#{count}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_scan_id_is_stable_and_local() {
        assert_eq!(health_scan_id(12345), "health-12345");
    }

    #[test]
    fn builder_assigns_fact_ids_and_keeps_partial_results() {
        let mut builder = HealthReportBuilder::for_started_at(100);
        builder
            .set_host("db-1")
            .set_role(Some("mysql-primary".to_string()))
            .set_health_score(Some(73))
            .add_check_done("memory")
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_mib",
                HealthFactValue::Unsigned(700),
                Some("MiB".to_string()),
                HealthFactSource::ProcMeminfo,
                4,
            )
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_mib",
                HealthFactValue::Unsigned(690),
                Some("MiB".to_string()),
                HealthFactSource::Derived,
                5,
            )
            .add_unavailable(
                HealthCollector::KernelSignal,
                HealthUnavailableReason::PermissionDenied,
                HealthSeverity::Unavailable,
                7,
            );

        let report = builder.finish(150);

        assert_eq!(report.scan_id, "health-100");
        assert_eq!(report.host.as_deref(), Some("db-1"));
        assert_eq!(report.role.as_deref(), Some("mysql-primary"));
        assert_eq!(report.health_score, Some(73));
        assert_eq!(report.elapsed_ms, 50);
        assert_eq!(report.checks_done, vec!["memory"]);
        assert_eq!(report.facts[0].id, "memory.available_mib");
        assert_eq!(report.facts[1].id, "memory.available_mib#2");
        assert_eq!(report.unavailable.len(), 1);
        assert_eq!(report.overall_severity, HealthSeverity::Unavailable);
    }

    #[test]
    fn finish_uses_saturating_elapsed_time() {
        let report = HealthReportBuilder::for_started_at(200).finish(150);

        assert_eq!(report.elapsed_ms, 0);
    }

    #[test]
    fn merge_report_preserves_partial_facts_with_new_ids() {
        let mut partial = HealthReportBuilder::for_started_at(10);
        partial.set_host("db-1").add_check_done("memory").add_fact(
            HealthFactCategory::Memory,
            "memory.available_mib",
            HealthFactValue::Unsigned(700),
            Some("MiB".to_string()),
            HealthFactSource::ProcMeminfo,
            1,
        );

        let mut builder = HealthReportBuilder::for_started_at(10);
        builder
            .add_fact(
                HealthFactCategory::Memory,
                "memory.available_mib",
                HealthFactValue::Unsigned(800),
                Some("MiB".to_string()),
                HealthFactSource::Fixture,
                0,
            )
            .merge_report(partial.finish(11));
        let report = builder.finish(12);

        assert_eq!(report.host.as_deref(), Some("db-1"));
        assert_eq!(report.checks_done, vec!["memory"]);
        assert_eq!(report.facts[0].id, "memory.available_mib");
        assert_eq!(report.facts[1].id, "memory.available_mib#2");
    }
}
