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
    /// Creates llm_call for LLM API paths (streaming or not), and for streams
    /// on unrecognized paths that carry parsed provider/usage evidence.
    /// Everything else (npm package queries, MCP HTTP+SSE transport, metrics
    /// streams) is filtered out. Works for both HTTP/1.1 and HTTP/2 uniformly.
    pub fn analyze_http(
        &self,
        http_record: &HttpRecord,
        token_record: Option<&TokenRecord>,
    ) -> Option<AuditRecord> {
        // Create llm_call audit records for LLM API calls identified by path,
        // and for streaming calls whose parsed events yielded provider/usage
        // evidence. `is_sse` alone must not decide: MCP's legacy HTTP+SSE
        // transport and metrics event streams are SSE but never LLM calls, so
        // admitting every SSE response creates empty llm_call rows.
        //
        // Use the shared parser-layer path set that decides whether the
        // GenAI pipeline creates a row at all: a private copy here
        // drifted from it, and /v1/responses (plus the DashScope native
        // endpoints) were parsed into trajectories yet never audited.
        let is_llm_path = crate::parser::llm::is_llm_api_path(&http_record.path);
        let has_usage_evidence = token_record.is_some_and(|record| {
            record.input_tokens > 0
                || record.output_tokens > 0
                || (!record.provider.is_empty() && record.provider != "unknown")
        });
        if !is_llm_path && !has_usage_evidence {
            return None;
        }

        // Parse the request body once; it feeds model, provider and
        // session_id extraction below.
        let request_json = http_record
            .request_body
            .as_ref()
            .and_then(|body| serde_json::from_str::<serde_json::Value>(body).ok());

        // Extract model. The request body is the primary source (the name the
        // caller asked for), but not every protocol puts it there: Gemini
        // embeds the model in the URL path (`/models/{model}:generateContent`)
        // and its request body carries contents/generationConfig only. When
        // the body never arrived (truncated capture), the paired token record
        // still holds the model the server reported — the provider resolution
        // below already treats it as a source.
        let model = request_json
            .as_ref()
            .and_then(|json| json.get("model")?.as_str().map(|s| s.to_string()))
            .or_else(|| {
                crate::analyzer::message::MessageParser::gemini_model_from_path(&http_record.path)
                    .map(|s| s.to_string())
            })
            .or_else(|| {
                token_record
                    .and_then(|t| t.model.clone())
                    .filter(|m| !m.is_empty())
            });

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
    fn test_sse_nonllm_path_no_audit_without_usage_evidence() {
        // MCP's legacy HTTP+SSE transport and metrics event streams are SSE
        // too, so `is_sse` alone does not prove an LLM call. Without parsed
        // usage evidence the stream must not create an llm_call audit row.
        let analyzer = AuditAnalyzer::new();
        let record = make_http_record("/mcp/sse", true, Some("event: message\ndata: {}\n\n"));
        let result = analyzer.analyze_http(&record, None);
        assert!(
            result.is_none(),
            "non-LLM SSE path must NOT produce an audit record without usage evidence"
        );
    }

    #[test]
    fn test_sse_nonllm_path_with_usage_evidence_still_audited() {
        // A real streaming call served from a gateway path outside the shared
        // LLM path set still carries provider/usage evidence and must stay in
        // the audit; the path gate is not allowed to drop it.
        let analyzer = AuditAnalyzer::new();
        let record = make_http_record("/custom/gateway/stream", true, None);
        let token = TokenRecord::new(1, "test".into(), "openai".into(), 100, 20);
        let result = analyzer.analyze_http(&record, Some(&token));
        assert!(
            result.is_some(),
            "SSE with parsed usage evidence must still produce an audit record"
        );
    }

    #[test]
    fn test_nonsse_responses_and_dashscope_paths_produce_audit() {
        // Both endpoints are parsed into trajectories by the GenAI pipeline
        // (is_llm_api_path), so a non-streaming call must not vanish from
        // audit --type llm.
        let analyzer = AuditAnalyzer::new();
        for path in &[
            "/v1/responses",
            "/api/v1/services/aigc/text-generation/generation",
            "/api/v1/services/aigc/multimodal-generation/generation",
            "/proxy/chat/completions",
        ] {
            let record = make_http_record(path, false, None);
            let result = analyzer.analyze_http(&record, None);
            assert!(result.is_some(), "{path} must produce an audit record");
        }

        // Token counting is not an inference call and stays out of the audit.
        let record = make_http_record("/v1/messages/count_tokens", false, None);
        assert!(analyzer.analyze_http(&record, None).is_none());
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

    #[test]
    fn test_analyze_http_labels_gemini_streams_from_the_path() {
        // A Gemini streamGenerateContent call: the model rides in the URL
        // path and the request body carries contents/generationConfig only,
        // so neither the body `model` read nor the three-parser path set can
        // label the row.
        let analyzer = AuditAnalyzer::new();
        let mut record = make_http_record(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse",
            true,
            None,
        );
        record.request_body = Some(
            r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}],"generationConfig":{}}"#
                .to_string(),
        );
        let audit = analyzer.analyze_http(&record, None).unwrap();
        if let AuditExtra::LlmCall {
            provider, model, ..
        } = &audit.extra
        {
            assert_eq!(provider.as_deref(), Some("gemini"));
            assert_eq!(model.as_deref(), Some("gemini-2.5-pro"));
        } else {
            panic!("expected LlmCall extra");
        }
    }

    #[test]
    fn test_analyze_http_gemini_model_prefers_the_requested_name() {
        // The token record carries the server-reported `modelVersion`
        // snapshot; the audit row describes the call the client made, so the
        // requested name from the path wins.
        let analyzer = AuditAnalyzer::new();
        let mut record = make_http_record(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent",
            true,
            None,
        );
        record.request_body =
            Some(r#"{"contents":[{"role":"user","parts":[{"text":"hi"}]}]}"#.to_string());
        let token = TokenRecord::new(1, "test".into(), "gemini".into(), 120, 34)
            .with_model("gemini-2.5-pro-002");
        let audit = analyzer.analyze_http(&record, Some(&token)).unwrap();
        if let AuditExtra::LlmCall { model, .. } = &audit.extra {
            assert_eq!(model.as_deref(), Some("gemini-2.5-pro"));
        } else {
            panic!("expected LlmCall extra");
        }
    }

    #[test]
    fn test_analyze_http_model_falls_back_to_the_token_record() {
        // The request body never arrived (truncated capture) but the response
        // chunks carried `model`; the paired token record is the only
        // remaining model source, and the provider resolution in this very
        // function already trusts it.
        let analyzer = AuditAnalyzer::new();
        let mut record = make_http_record("/v1/chat/completions", true, None);
        record.request_body = None;
        let token = TokenRecord::new(1, "test".into(), "openai".into(), 10, 5)
            .with_model("qwen3-coder-plus");
        let audit = analyzer.analyze_http(&record, Some(&token)).unwrap();
        if let AuditExtra::LlmCall { model, .. } = &audit.extra {
            assert_eq!(model.as_deref(), Some("qwen3-coder-plus"));
        } else {
            panic!("expected LlmCall extra");
        }
    }
}
