//! Both runtime modes maintain connection ownership even without capture events.

use super::*;

impl AgentSight {
    pub(crate) fn tick(&mut self) {
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
