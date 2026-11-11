use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::model::{
    HealthCheckResult, HealthCheckStatus, HealthCollector, HealthFact, HealthFactCategory,
    HealthFactCoverage, HealthFactMetadata, HealthFactScope, HealthFactSource, HealthFactValue,
    HealthFinding, HealthProgress, HealthScanReport, HealthSeverity, HealthUnavailableReason,
    UnavailableCollector,
};

pub(crate) fn health_scan_id(started_at_ms: u128) -> String {
    format!("health-{started_at_ms}")
}

struct CheckContext {
    id: String,
    scope: HealthFactScope,
    coverage: HealthFactCoverage,
    started_at_ms: u128,
    started: Instant,
}

pub(crate) struct HealthReportBuilder {
    report: HealthScanReport,
    fact_key_counts: BTreeMap<String, usize>,
    pending: BTreeSet<String>,
    current: Option<CheckContext>,
    progress: Option<Box<dyn FnMut(HealthProgress) -> bool + Send>>,
    progress_failed: bool,
}

impl HealthReportBuilder {
    pub(crate) fn new(scan_id: impl Into<String>, started_at_ms: u128) -> Self {
        Self {
            report: HealthScanReport::new(scan_id, started_at_ms),
            fact_key_counts: BTreeMap::new(),
            pending: BTreeSet::new(),
            current: None,
            progress: None,
            progress_failed: false,
        }
    }

    pub(crate) fn for_started_at(started_at_ms: u128) -> Self {
        Self::new(health_scan_id(started_at_ms), started_at_ms)
    }

    pub(crate) fn with_progress(
        mut self,
        progress: impl FnMut(HealthProgress) -> bool + Send + 'static,
    ) -> Self {
        self.progress = Some(Box::new(progress));
        self
    }

    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub(super) fn is_pending(&self, id: &str) -> bool {
        self.pending.contains(id)
    }

    pub(super) fn set_scope(&mut self, scope: HealthFactScope) {
        if let Some(context) = self.current.as_mut() {
            context.scope = scope;
        }
    }

    pub(super) fn set_coverage(&mut self, coverage: HealthFactCoverage) {
        if let Some(context) = self.current.as_mut() {
            context.coverage = coverage;
        }
    }

    pub(super) fn set_sampled_at(&mut self, sampled_at_ms: u128) {
        if let Some(context) = self.current.as_mut() {
            context.started_at_ms = sampled_at_ms;
        }
    }

    pub(super) fn current_check_id(&self) -> Option<&str> {
        self.current.as_ref().map(|context| context.id.as_str())
    }

    pub(crate) fn progress_failed(&self) -> bool {
        self.progress_failed
    }

    pub(super) fn set_login_config(&mut self, config: crate::config::HealthLoginConfig) {
        self.report.login_config = Some(config);
    }

    pub(super) fn clear_login_config(&mut self) {
        self.report.login_config = None;
    }

    pub(crate) fn set_host(&mut self, host: impl Into<String>) -> &mut Self {
        let host = host.into();
        self.report.host = Some(host.clone());
        self.emit(HealthProgress::Host(host));
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
    pub(crate) fn report(&self) -> &HealthScanReport {
        &self.report
    }

    pub(crate) fn declare_check(&mut self, collector: HealthCollector, enabled: bool) -> &mut Self {
        self.declare_check_id(collector.check_id(), collector, enabled)
    }

    pub(super) fn declare_check_id(
        &mut self,
        id: &str,
        collector: HealthCollector,
        enabled: bool,
    ) -> &mut Self {
        let result = self.check_mut(id);
        result.collector = Some(collector);
        result.status = if enabled {
            HealthCheckStatus::Partial
        } else {
            HealthCheckStatus::Disabled
        };
        if enabled {
            self.pending.insert(id.to_string());
        }
        self.report.sync_check_projections();
        self
    }

    pub(crate) fn start_check(
        &mut self,
        collector: HealthCollector,
        scope: HealthFactScope,
        coverage: HealthFactCoverage,
        started_at_ms: u128,
    ) -> &mut Self {
        self.start_check_id(
            collector.check_id(),
            collector,
            scope,
            coverage,
            started_at_ms,
        )
    }

    pub(super) fn start_check_id(
        &mut self,
        id: &str,
        collector: HealthCollector,
        scope: HealthFactScope,
        coverage: HealthFactCoverage,
        started_at_ms: u128,
    ) -> &mut Self {
        self.declare_check_id(id, collector, true);
        self.current = Some(CheckContext {
            id: id.to_string(),
            scope,
            coverage,
            started_at_ms,
            started: Instant::now(),
        });
        self
    }

    pub(crate) fn complete_check(&mut self) -> &mut Self {
        let Some(context) = self.current.as_ref() else {
            return self;
        };
        let id = context.id.clone();
        let elapsed_ms = context.started.elapsed().as_millis();
        let incomplete = self
            .report
            .check_results
            .iter()
            .find(|check| check.id == id)
            .is_some_and(|check| {
                check.status == HealthCheckStatus::Partial && check.gaps.is_empty()
            });
        if incomplete {
            if let Some(collector) = HealthCollector::from_check_id(&id) {
                self.add_unavailable(
                    collector,
                    HealthUnavailableReason::ParseError,
                    HealthSeverity::Unavailable,
                    elapsed_ms,
                );
            }
        }
        self.check_mut(&id).elapsed_ms = elapsed_ms;
        self.pending.remove(&id);
        self.current = None;
        self.report.sync_check_projections();
        let check = self.check_mut(&id).clone();
        self.emit(HealthProgress::Check(check));
        self
    }

    pub(crate) fn add_check_done(&mut self, check: impl Into<String>) -> &mut Self {
        let supplied = check.into();
        let id = self
            .current
            .as_ref()
            .map(|context| context.id.clone())
            .unwrap_or(supplied);
        let check = self.check_mut(&id);
        check.status = if check.gaps.is_empty() {
            HealthCheckStatus::Ok
        } else if check.fact_ids.is_empty() {
            HealthCheckStatus::Unavailable
        } else {
            HealthCheckStatus::Partial
        };
        self.report.sync_check_projections();
        self
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
        let metadata = match self.current.as_ref() {
            Some(context) => HealthFactMetadata {
                scope: context.scope.clone(),
                coverage: context.coverage.clone(),
                sampled_at_ms: context.started_at_ms.saturating_add(elapsed_ms),
            },
            None => HealthFactMetadata {
                scope: if source == HealthFactSource::Fixture {
                    HealthFactScope::Fixture
                } else {
                    HealthFactScope::Unknown
                },
                coverage: HealthFactCoverage::Unknown,
                sampled_at_ms: self.report.started_at_ms.saturating_add(elapsed_ms),
            },
        };
        let check_id = self
            .current
            .as_ref()
            .map(|context| context.id.clone())
            .or_else(|| {
                self.report
                    .check_results
                    .iter()
                    .find(|check| {
                        check
                            .collector
                            .is_some_and(|collector| collector_category(collector) == category)
                    })
                    .map(|check| check.id.clone())
            });
        self.push_fact(
            check_id.as_deref(),
            HealthFact {
                id,
                category,
                key,
                value,
                unit,
                source,
                elapsed_ms,
            },
            metadata,
        );
        self
    }

    pub(crate) fn add_unavailable(
        &mut self,
        collector: HealthCollector,
        reason: HealthUnavailableReason,
        severity: HealthSeverity,
        elapsed_ms: u128,
    ) -> &mut Self {
        let id = self
            .current
            .as_ref()
            .map(|context| context.id.clone())
            .unwrap_or_else(|| collector.check_id().to_string());
        self.add_check_gap(&id, collector, reason, severity, elapsed_ms)
    }

    pub(super) fn add_check_gap(
        &mut self,
        id: &str,
        collector: HealthCollector,
        reason: HealthUnavailableReason,
        severity: HealthSeverity,
        elapsed_ms: u128,
    ) -> &mut Self {
        let check = self.check_mut(id);
        check.collector = Some(collector);
        check.elapsed_ms = check.elapsed_ms.max(elapsed_ms);
        check.gaps.push(UnavailableCollector {
            collector,
            reason,
            severity,
            elapsed_ms,
        });
        check.status = if check.fact_ids.is_empty() {
            HealthCheckStatus::Unavailable
        } else {
            HealthCheckStatus::Partial
        };
        self.report.sync_check_projections();
        self
    }

    pub(crate) fn add_finding(&mut self, finding: HealthFinding) -> &mut Self {
        self.report.findings.push(finding.clone());
        self.emit(HealthProgress::Finding(finding));
        self
    }

    pub(crate) fn apply_progress(&mut self, progress: HealthProgress) -> Result<(), &'static str> {
        match progress {
            HealthProgress::Host(host) => {
                self.report.host = Some(host);
            }
            HealthProgress::Fact {
                check_id,
                fact,
                metadata,
            } => {
                if self
                    .report
                    .facts
                    .iter()
                    .any(|existing| existing.id == fact.id)
                {
                    return Err("duplicate health fact");
                }
                if self.check_mut(&check_id).status == HealthCheckStatus::Disabled {
                    return Err("fact belongs to disabled check");
                }
                *self.fact_key_counts.entry(fact.key.clone()).or_insert(0) += 1;
                self.push_fact(Some(&check_id), fact, metadata);
            }
            HealthProgress::Finding(finding) => {
                if finding
                    .evidence_fact_ids
                    .iter()
                    .any(|id| !self.report.facts.iter().any(|fact| fact.id == *id))
                {
                    return Err("health finding has missing evidence");
                }
                if self
                    .report
                    .findings
                    .iter()
                    .any(|existing| existing.id == finding.id)
                {
                    return Err("duplicate health finding");
                }
                self.report.findings.push(finding);
            }
            HealthProgress::Check(check) => {
                if check
                    .fact_ids
                    .iter()
                    .any(|id| !self.report.facts.iter().any(|fact| fact.id == *id))
                {
                    return Err("health check has missing facts");
                }
                let id = check.id.clone();
                if self.check_mut(&id).fact_ids != check.fact_ids {
                    return Err("health check changed fact ownership");
                }
                self.pending.remove(&id);
                *self.check_mut(&id) = check;
            }
        }
        self.report.sync_check_projections();
        Ok(())
    }

    pub(crate) fn seal_pending(
        &mut self,
        reason: HealthUnavailableReason,
        elapsed_ms: u128,
    ) -> &mut Self {
        let ids: Vec<_> = self.pending.iter().cloned().collect();
        self.current = None;
        for id in ids {
            if let Some(collector) = HealthCollector::from_check_id(&id) {
                self.add_check_gap(
                    &id,
                    collector,
                    reason,
                    legacy_gap_severity(collector),
                    elapsed_ms,
                );
            }
        }
        self.pending.clear();
        self
    }

    pub(crate) fn finish(mut self, finished_at_ms: u128) -> HealthScanReport {
        let elapsed_ms = finished_at_ms.saturating_sub(self.report.started_at_ms);
        self.seal_pending(HealthUnavailableReason::Timeout, elapsed_ms);
        self.report.elapsed_ms = elapsed_ms;
        self.report.recompute_overall_severity();
        self.report
    }

    fn check_mut(&mut self, id: &str) -> &mut HealthCheckResult {
        if let Some(index) = self
            .report
            .check_results
            .iter()
            .position(|check| check.id == id)
        {
            return &mut self.report.check_results[index];
        }
        self.report.check_results.push(HealthCheckResult {
            id: id.to_string(),
            collector: HealthCollector::from_check_id(id),
            status: HealthCheckStatus::Partial,
            fact_ids: Vec::new(),
            gaps: Vec::new(),
            elapsed_ms: 0,
        });
        let index = self.report.check_results.len() - 1;
        &mut self.report.check_results[index]
    }

    fn push_fact(
        &mut self,
        check_id: Option<&str>,
        fact: HealthFact,
        metadata: HealthFactMetadata,
    ) {
        if let Some(id) = check_id {
            let check = self.check_mut(id);
            check.fact_ids.push(fact.id.clone());
            if !check.gaps.is_empty() {
                check.status = HealthCheckStatus::Partial;
            }
        }
        self.report
            .fact_metadata
            .insert(fact.id.clone(), metadata.clone());
        self.report.facts.push(fact.clone());
        if let Some(id) = check_id {
            self.emit(HealthProgress::Fact {
                check_id: id.to_string(),
                fact,
                metadata,
            });
        }
    }

    fn emit(&mut self, progress: HealthProgress) {
        if !self.progress_failed {
            if let Some(sink) = self.progress.as_mut() {
                self.progress_failed = !sink(progress);
            }
        }
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

pub(super) fn legacy_gap_severity(collector: HealthCollector) -> HealthSeverity {
    match collector {
        HealthCollector::Cpu
        | HealthCollector::Memory
        | HealthCollector::Disk
        | HealthCollector::ConfiguredService => HealthSeverity::Degraded,
        _ => HealthSeverity::Unavailable,
    }
}

fn collector_category(collector: HealthCollector) -> HealthFactCategory {
    match collector {
        HealthCollector::Host => HealthFactCategory::Host,
        HealthCollector::Cpu => HealthFactCategory::Cpu,
        HealthCollector::Memory => HealthFactCategory::Memory,
        HealthCollector::Disk => HealthFactCategory::Disk,
        HealthCollector::KernelSignal => HealthFactCategory::Kernel,
        HealthCollector::ConfiguredService => HealthFactCategory::Service,
        HealthCollector::Provider => HealthFactCategory::Provider,
        HealthCollector::Config => HealthFactCategory::Config,
        HealthCollector::Hooks => HealthFactCategory::Hooks,
        HealthCollector::Pty => HealthFactCategory::Pty,
        HealthCollector::Permissions => HealthFactCategory::Permissions,
    }
}

#[cfg(test)]
mod tests;
