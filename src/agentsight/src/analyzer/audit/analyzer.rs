//! AuditAnalyzer - extracts audit records from aggregated results
//!
//! Pure logic layer, no IO. Converts `AggregatedResult` into `AuditRecord`.

use super::record::{AuditEventType, AuditExtra, AuditRecord};
use crate::aggregator::AggregatedProcess;
use crate::aggregator::AggregatedResult;
use crate::aggregator::HttpPair;
use crate::analyzer::HttpRecord;
use crate::analyzer::token::TokenRecord;

/// Analyzes aggregated results and extracts audit records
pub struct AuditAnalyzer;

impl AuditAnalyzer {
    /// Create a new AuditAnalyzer
    pub fn new() -> Self {
        AuditAnalyzer
    }

    /// Analyze an aggregated result and extract an audit record if applicable
    ///
    /// Note: For HTTP/HTTP2 requests, prefer using `analyze_http` method instead,
    /// which handles both HTTP/1.1 and HTTP/2 uniformly via HttpRecord.
    pub fn analyze(&self, result: &AggregatedResult) -> Option<AuditRecord> {
        match result {
            AggregatedResult::ProcessComplete(process) => {
                Some(self.extract_process_action(process))
            }
            // HTTP/HTTP2 results should be handled via analyze_http()
            // This legacy path is kept for backward compatibility
            AggregatedResult::SseComplete(pair) => {
                // Only create audit for SSE responses (LLM streaming calls)
                Some(self.extract_llm_call_legacy(pair, true))
            }
            _ => None,
        }
    }

    /// Extract audit record from HttpRecord
    ///
    /// Only creates llm_call for SSE responses, which are LLM streaming API calls.
    /// Non-SSE requests (like npm package queries) are filtered out.
    /// This method works for both HTTP/1.1 and HTTP/2 uniformly.
    pub fn analyze_http(
        &self,
        http_record: &HttpRecord,
        token_record: Option<&TokenRecord>,
    ) -> Option<AuditRecord> {
        // Create llm_call audit records for SSE responses AND non-streaming
        // LLM API calls (identified by path). Without this, non-streaming
        // completions (stream:false) are invisible in audit --type llm.
        let is_llm_path = http_record.path.contains("/chat/completions")
            || http_record.path.contains("/v1/messages")
            || http_record.path.contains("/v1/completions")
            || http_record
                .path
                .contains("/api/v1/copilot/generate_copilot");
        if !http_record.is_sse && !is_llm_path {
            return None;
        }

        // Parse the request body once; it feeds model, provider and
        // session_id extraction below.
        let request_json = http_record
            .request_body
            .as_ref()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());

        // Extract model from request body if available
        let model = request_json
            .as_ref()
            .and_then(|json| json.get("model")?.as_str().map(|s| s.to_string()));

        // Provider resolution: the parsed token usage is authoritative; fall
        // back to the endpoint path (compatible-mode completion paths still
        // resolve here because matching is substring-based).
        let provider = token_record
            .map(|t| t.provider.clone())
            .filter(|p| !p.is_empty() && p != "unknown")
            .or_else(|| {
                crate::analyzer::message::MessageParser::detect_provider(&http_record.path)
                    .map(|s| s.to_string())
            });

        // Session id: same first-layer source the GenAI pipeline uses —
        // clients that embed it in the request metadata get it at no cost.
        let session_id = request_json
            .as_ref()
            .and_then(|b| b.get("metadata"))
            .and_then(crate::analyzer::message::types::session_id_from_metadata);

        let (input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens) =
            match token_record {
                Some(t) => (
                    t.input_tokens,
                    t.output_tokens,
                    t.cache_creation_tokens.unwrap_or(0),
                    t.cache_read_tokens.unwrap_or(0),
                ),
                None => (0, 0, 0, 0),
            };

        Some(AuditRecord {
            id: None,
            event_type: AuditEventType::LlmCall,
            timestamp_ns: http_record.timestamp_ns,
            pid: http_record.pid,
            ppid: None,
            comm: http_record.comm.clone(),
            duration_ns: http_record.duration_ns,
            extra: AuditExtra::LlmCall {
                provider,
                model,
                request_method: Some(http_record.method.clone()),
                request_path: Some(http_record.path.clone()),
                response_status: Some(http_record.status_code),
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                is_sse: http_record.is_sse,
            },
            session_id,
        })
    }

    /// Extract an LLM call audit record from an HTTP pair (legacy method)
    fn extract_llm_call_legacy(&self, pair: &HttpPair, is_sse: bool) -> AuditRecord {
        let request = &pair.request;
        let pid = request.source_event.pid;
        let comm = request.source_event.comm_str();

        // Extract request info
        let request_method = Some(request.method.clone());
        let request_path = Some(request.path.clone());
        let request_ts = request.source_event.timestamp_ns;

        // Extract response info
        let response_status = Some(pair.response.parsed.status_code);

        // Extract model from request body
        let model = detect_model_from_request(pair);

        // Calculate duration
        let response_end_ts = pair.response.end_timestamp_ns();
        let duration_ns = response_end_ts.saturating_sub(request_ts);

        AuditRecord {
            id: None,
            event_type: AuditEventType::LlmCall,
            timestamp_ns: request_ts,
            pid,
            ppid: None,
            comm,
            duration_ns,
            extra: AuditExtra::LlmCall {
                provider: None,
                model,
                request_method,
                request_path,
                response_status,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_tokens: 0,
                cache_read_tokens: 0,
                is_sse,
            },
            session_id: None,
        }
    }

    /// Extract a process action audit record from an aggregated process
    fn extract_process_action(&self, process: &AggregatedProcess) -> AuditRecord {
        AuditRecord {
            id: None,
            event_type: AuditEventType::ProcessAction,
            timestamp_ns: process.start_timestamp_ns,
            pid: process.pid,
            ppid: Some(process.ppid),
            comm: process.comm.clone(),
            duration_ns: process.duration_ns(),
            extra: AuditExtra::ProcessAction {
                filename: process.filename.clone(),
                args: process.args.clone(),
                exit_code: None,
            },
            session_id: process.session_id.clone(),
        }
    }
}

impl Default for AuditAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Try to detect model from request body JSON
fn detect_model_from_request(pair: &HttpPair) -> Option<String> {
    let body = pair.request.body();
    if body.is_empty() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(body).ok()?;
    json.get("model")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::AggregatedProcess;
    use crate::analyzer::token::TokenRecord;

    #[test]
    fn test_extract_process_action_propagates_session_id() {
        let mut proc = AggregatedProcess::new(100, 100, 50, 50, "bash".to_string(), 1000);
        proc.session_id = Some("test-session-xyz".to_string());
        proc.add_exec("/bin/bash".to_string(), "echo hi".to_string(), 2000);

        let analyzer = AuditAnalyzer::new();
        let record = analyzer.extract_process_action(&proc);

        assert_eq!(
            record.session_id.as_deref(),
            Some("test-session-xyz"),
            "session_id must propagate from AggregatedProcess to AuditRecord"
        );
    }

    #[test]
    fn test_extract_process_action_none_session_id() {
        let mut proc = AggregatedProcess::new(100, 100, 50, 50, "bash".to_string(), 1000);
        proc.add_exec("/bin/bash".to_string(), "echo hi".to_string(), 2000);

        let analyzer = AuditAnalyzer::new();
        let record = analyzer.extract_process_action(&proc);

        assert_eq!(record.session_id, None);
    }

    fn make_http_record(path: &str, is_sse: bool, response_body: Option<&str>) -> HttpRecord {
        HttpRecord {
            timestamp_ns: 0,
            pid: 1,
            comm: "test".into(),
            method: "POST".into(),
            path: path.into(),
            status_code: 200,
            request_headers: "{}".into(),
            request_body: Some(r#"{"model":"test-model"}"#.into()),
            response_headers: "{}".into(),
            response_body: response_body.map(|s| s.to_string()),
            duration_ns: 1000,
            first_output_timestamp_ns: None,
            is_sse,
            sse_event_count: 0,
        }
    }

    #[test]
    fn test_nonsse_llm_path_produces_audit() {
        let analyzer = AuditAnalyzer::new();
        let record = make_http_record("/v1/chat/completions", false, None);
        let token = TokenRecord::new(1, "test".into(), "openai".into(), 100, 20);
        let result = analyzer.analyze_http(&record, Some(&token));
        assert!(
            result.is_some(),
            "non-SSE LLM path must produce audit record"
        );
        let audit = result.unwrap();
        if let AuditExtra::LlmCall {
            is_sse,
            input_tokens,
            output_tokens,
            ..
        } = &audit.extra
        {
            assert!(!is_sse, "non-SSE call must have is_sse=false");
            assert_eq!(*input_tokens, 100);
            assert_eq!(*output_tokens, 20);
        } else {
            panic!("expected LlmCall extra");
        }
    }

    #[test]
    fn test_nonsse_nonllm_path_no_audit() {
        let analyzer = AuditAnalyzer::new();
        let record = make_http_record("/api/health", false, None);
        let result = analyzer.analyze_http(&record, None);
        assert!(
            result.is_none(),
            "non-LLM path must NOT produce audit record"
        );
    }

    #[test]
    fn test_sse_path_produces_audit_with_sse_true() {
        let analyzer = AuditAnalyzer::new();
        let record = make_http_record("/v1/chat/completions", true, None);
        let token = TokenRecord::new(1, "test".into(), "openai".into(), 50, 10);
        let result = analyzer.analyze_http(&record, Some(&token));
        assert!(result.is_some());
        if let AuditExtra::LlmCall { is_sse, .. } = &result.unwrap().extra {
            assert!(*is_sse, "SSE call must have is_sse=true");
        }
    }

    #[test]
    fn test_analyze_http_fills_provider_from_token_record() {
        let analyzer = AuditAnalyzer::new();
        let record = make_http_record("/v1/chat/completions", false, None);
        let token = TokenRecord::new(1, "test".into(), "anthropic".into(), 1, 1);
        let audit = analyzer.analyze_http(&record, Some(&token)).unwrap();
        if let AuditExtra::LlmCall { provider, .. } = &audit.extra {
            assert_eq!(provider.as_deref(), Some("anthropic"));
        } else {
            panic!("expected LlmCall extra");
        }
    }

    #[test]
    fn test_analyze_http_falls_back_to_path_provider() {
        let analyzer = AuditAnalyzer::new();
        // No token record and an "unknown" provider must both fall back to
        // path detection, including for compatible-mode paths.
        let record = make_http_record("/compatible-mode/v1/chat/completions", false, None);
        let audit = analyzer.analyze_http(&record, None).unwrap();
        if let AuditExtra::LlmCall { provider, .. } = &audit.extra {
            assert_eq!(provider.as_deref(), Some("openai"));
        } else {
            panic!("expected LlmCall extra");
        }

        let record = make_http_record("/v1/messages", false, None);
        let token = TokenRecord::new(1, "test".into(), "unknown".into(), 1, 1);
        let audit = analyzer.analyze_http(&record, Some(&token)).unwrap();
        if let AuditExtra::LlmCall { provider, .. } = &audit.extra {
            assert_eq!(provider.as_deref(), Some("anthropic"));
        } else {
            panic!("expected LlmCall extra");
        }
    }

    #[test]
    fn test_analyze_http_extracts_session_id_from_request_metadata() {
        let analyzer = AuditAnalyzer::new();
        let mut record = make_http_record("/v1/messages", true, None);
        record.request_body =
            Some(r#"{"model":"test-model","metadata":{"session_id":"sess-42"}}"#.to_string());
        let audit = analyzer.analyze_http(&record, None).unwrap();
        assert_eq!(audit.session_id.as_deref(), Some("sess-42"));

        // No metadata → session_id stays None.
        let record = make_http_record("/v1/messages", true, None);
        let audit = analyzer.analyze_http(&record, None).unwrap();
        assert_eq!(audit.session_id, None);
    }
}
