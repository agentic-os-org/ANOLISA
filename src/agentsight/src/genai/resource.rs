//! Build bounded abandonment records without parsing bodies or computing tokens.

use super::{GenAIBuilder, helpers::PidAgentNameCache};
use crate::{analyzer::AnalysisResult, response_map::ResponseSessionMapper};

impl GenAIBuilder {
    /// Preserve process and request correlation without constructing a semantic payload.
    pub(super) fn build_resource_metadata(
        &self,
        results: &[AnalysisResult],
        mapper: &ResponseSessionMapper,
        agents: &impl PidAgentNameCache,
    ) -> Option<super::semantic::LLMCall> {
        use super::semantic::{LLMCall, LLMRequest};
        let http = results.iter().find_map(|r| match r {
            AnalysisResult::Http(http) => Some(http),
            _ => None,
        })?;
        if !crate::parser::llm::is_llm_api_path(&http.path) {
            return None;
        }
        let request = LLMRequest {
            messages: Vec::new(),
            temperature: None,
            max_tokens: None,
            frequency_penalty: None,
            presence_penalty: None,
            top_p: None,
            top_k: None,
            seed: None,
            stop_sequences: None,
            stream: http.is_sse,
            tools: None,
            raw_body: None,
        };
        let mut call = LLMCall::new(
            self.generate_id(),
            http.timestamp_ns,
            "unknown".into(),
            "unknown".into(),
            request,
            http.pid as i32,
            http.comm.clone(),
        );
        call.end_timestamp_ns = http.timestamp_ns.saturating_add(http.duration_ns);
        call.duration_ns = http.duration_ns;
        call.response.streamed = http.is_sse;
        call.agent_name = agents.get_agent_name(&http.pid).cloned();
        for (key, value) in [
            ("method", http.method.clone()),
            ("path", http.path.clone()),
            ("status_code", http.status_code.to_string()),
            ("metadata_only", "true".into()),
            ("is_sse", http.is_sse.to_string()),
        ] {
            call.metadata.insert(key.into(), value);
        }
        if let Some(session) = mapper.get_session_by_pid(http.pid) {
            call.metadata
                .insert("session_id".into(), session.to_string());
        }
        if let Ok(headers) =
            serde_json::from_str::<std::collections::HashMap<String, String>>(&http.request_headers)
        {
            for (name, value) in headers {
                match name.to_ascii_lowercase().as_str() {
                    "x-request-id" => {
                        call.metadata.insert("request_id".into(), value);
                    }
                    "host" | ":authority" => {
                        call.metadata.insert("server.address".into(), value);
                    }
                    ":scheme" => {
                        call.metadata.insert("url.scheme".into(), value);
                    }
                    _ => {}
                }
            }
        }
        Some(call)
    }
}
