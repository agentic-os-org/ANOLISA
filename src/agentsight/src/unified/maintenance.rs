//! Both runtime modes maintain connection ownership even without capture events.

use super::*;

/// Share one CPU allowance across startup probes, maintenance and capture.
pub(super) fn configure_cpu_budget(
    probes: &Probes,
    percent: u8,
) -> Result<Option<Arc<crate::utils::cpu_budget::CpuBudget>>> {
    let budget = (percent != 0)
        .then(|| crate::utils::cpu_budget::CpuBudget::new(percent))
        .transpose()?
        .map(Arc::new);
    probes.set_cpu_budget(budget.clone());
    Ok(budget)
}

impl AgentSight {
    pub(crate) fn tick(&mut self) {
        self.probes.poll_pending_ssl();
        self.drain_one_completion(false);
        if self.last_drain_check.elapsed() < std::time::Duration::from_secs(1) {
            return;
        }
        self.drain_and_persist_dead_connections();
        self.drain_and_persist_idle_connections();
        self.flush_expired_pending_genai();
        let results = self.core.tick(std::time::Instant::now());
        self.process_aggregated_results(results);
        self.maybe_export_runtime_metrics();
    }
}
