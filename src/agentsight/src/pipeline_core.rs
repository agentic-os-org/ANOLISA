//! Shared parser, aggregator, and analyzer ownership without attaching probes.
//! Aggregation and analysis stay separate so runtime CPU admission precedes expansion.

use crate::aggregator::{AggregatedResult, Aggregator};
use crate::analyzer::{AnalysisResult, Analyzer};
use crate::config::AgentsightConfig;
use crate::event::Event;
use crate::parser::Parser;
use std::time::Instant;

/// Probe-independent processing shared by the runtime and event replay tests.
pub struct PipelineCore {
    parser: Parser,
    aggregator: Aggregator,
    analyzer: Analyzer,
}

impl PipelineCore {
    /// Applies capture limits while preserving the caller's configured analyzer.
    pub fn new(config: &AgentsightConfig, analyzer: Analyzer) -> Self {
        Self {
            parser: Parser::with_max_headers(config.max_headers),
            // Both initial parsing and assembled-header re-parsing need the same cap.
            aggregator: Aggregator::with_limits(config.connection_capacity, &config.runtime_limits)
                .with_http_max_headers(config.max_headers),
            analyzer,
        }
    }

    /// Parses and correlates one event, retaining owned evidence for later CPU admission.
    pub fn process_event(&mut self, event: Event) -> Vec<AggregatedResult> {
        if let Event::Ssl(ref ssl) = event
            && let Some(results) = self.aggregator.process_sse_capture(ssl)
        {
            return results;
        }
        self.aggregator
            .process_result(self.parser.parse_event(event))
    }

    /// Analyzes borrowed evidence for callers that must keep the original aggregate.
    pub fn analyze_aggregated(&self, result: &AggregatedResult) -> Vec<AnalysisResult> {
        self.analyzer.analyze_aggregated(result)
    }

    /// Consumes an admitted completion, optionally retaining raw HTTP evidence.
    pub fn analyze_for_owned_delivery(
        &self,
        result: AggregatedResult,
        retain_raw: bool,
    ) -> Vec<AnalysisResult> {
        self.analyzer.analyze_for_owned_delivery(result, retain_raw)
    }

    /// Advances correlation maintenance even when no captured events arrive.
    pub fn tick(&mut self, now: Instant) -> Vec<AggregatedResult> {
        self.aggregator.tick(now)
    }

    /// Ends remaining correlations with the existing shutdown reason.
    pub fn drain_pending(&mut self) -> Vec<AggregatedResult> {
        self.aggregator.drain_pending()
    }

    /// Exposes correlation state for runtime diagnostics and compatibility APIs.
    pub fn aggregator(&self) -> &Aggregator {
        &self.aggregator
    }

    /// Preserves existing PID cleanup and FFI retention configuration hooks.
    pub fn aggregator_mut(&mut self) -> &mut Aggregator {
        &mut self.aggregator
    }

    /// Exposes the configured analyzer through the existing runtime API.
    pub fn analyzer(&self) -> &Analyzer {
        &self.analyzer
    }
}
