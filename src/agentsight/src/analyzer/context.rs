//! Semantic delivery shares parsed JSON without a full body serialization round trip.

use super::super::result::{AnalysisResult, HttpContext, HttpRecord};
use super::super::token::{TokenParser, TokenRecord, merge_usage_from_event};
use super::Analyzer;
use crate::aggregator::AggregatedResult;
use serde_json::Value;

impl Analyzer {
    pub(crate) fn analyze_for_delivery(
        &self,
        result: &AggregatedResult,
        retain_raw: bool,
    ) -> Vec<AnalysisResult> {
        if let AggregatedResult::Abandoned { partial, reason } = result {
            let mut results = self.analyze_for_delivery(partial, retain_raw);
            results.push(AnalysisResult::Abandoned(*reason));
            return results;
        }
        let Some(mut context) = context(result) else {
            return self.analyze_aggregated(result);
        };
        if context.http.status_code >= 400
            || (retain_raw && (context.request.is_none() || context.response.is_none()))
        {
            return self.analyze_aggregated(result);
        }
        let message = self.message.parse_by_path(
            &context.http.path,
            context.request.as_ref(),
            context.response.as_ref(),
        );
        if message.is_none() && !crate::parser::llm::is_llm_api_path(&context.http.path) {
            return self.analyze_aggregated(result);
        }
        if retain_raw {
            context.http.request_body = context
                .request
                .as_ref()
                .and_then(|value| serde_json::to_string(value).ok());
            context.http.response_body = context
                .response
                .as_ref()
                .and_then(|value| serde_json::to_string(value).ok());
            context.retain_raw = true;
        }
        let timer = crate::runtime_metrics::StageTimer::start("analyzer");
        let parser = TokenParser::new();
        let usage =
            match context.response.as_ref() {
                Some(Value::Array(events)) => events.iter().fold(None, |usage, event| match parser
                    .parse_json(event)
                {
                    Some(parsed) => merge_usage_from_event(usage, parsed, event),
                    None => usage,
                }),
                Some(value) => parser.parse_json(value),
                None => None,
            };
        let mut token = usage
            .map(|usage| {
                TokenRecord::new(
                    context.http.pid,
                    context.http.comm.clone(),
                    usage.provider.to_string(),
                    usage.input_tokens,
                    usage.output_tokens,
                )
                .with_model(usage.model.unwrap_or_default())
                .with_cache_tokens(
                    usage.cache_creation_input_tokens.unwrap_or(0),
                    usage.cache_read_input_tokens.unwrap_or(0),
                )
            })
            .filter(|token| token.total_tokens() > 0)
            .or_else(|| {
                if !matches!(
                    result,
                    AggregatedResult::SseComplete(_) | AggregatedResult::Http2StreamComplete(_)
                ) {
                    return None;
                }
                self.compute_tokens_from_json(
                    context.http.pid,
                    &context.http.comm,
                    &context.http.path,
                    context.request.as_ref()?,
                    context
                        .response
                        .as_ref()
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            });
        if let Some(token) = &mut token {
            if token.model.as_deref().unwrap_or_default().is_empty() {
                token.model = context
                    .request
                    .as_ref()
                    .and_then(|request| request.get("model")?.as_str())
                    .map(str::to_owned);
            }
        }
        let mut results = Vec::new();
        if let Some(audit) = self.audit.analyze_http_with_json(
            &context.http,
            token.as_ref(),
            context.request.as_ref(),
        ) {
            results.push(AnalysisResult::Audit(audit));
        }
        if let Some(message) = message {
            results.push(AnalysisResult::Message(message));
        }
        if let Some(token) = token {
            results.push(AnalysisResult::Token(token));
        }
        results.push(AnalysisResult::HttpContext(context));
        timer.record_outputs(results.len());
        results
    }
}

fn context(result: &AggregatedResult) -> Option<HttpContext> {
    let (request, response, sse) = match result {
        AggregatedResult::HttpComplete(pair) => (&pair.request, Some(&pair.response), false),
        AggregatedResult::SseComplete(pair) => (&pair.request, Some(&pair.response), true),
        AggregatedResult::RequestOnly { request, .. } => (request, None, false),
        AggregatedResult::Http2StreamComplete(stream) => {
            let sse_response = stream.response_sse_json_array();
            let sse = stream.is_response_sse() || sse_response.is_some();
            let response = sse_response.or_else(|| stream.response_json_body());
            return Some(HttpContext {
                retain_raw: false,
                request: stream.request_json_body(),
                response,
                http: HttpRecord {
                    timestamp_ns: stream.start_timestamp_ns,
                    pid: stream.pid(),
                    comm: stream.comm(),
                    method: stream.method(),
                    path: stream.path(),
                    status_code: stream.status_code(),
                    request_headers: stream.request_headers_json(),
                    response_headers: stream.response_headers_json(),
                    request_body: None,
                    response_body: None,
                    duration_ns: stream
                        .end_timestamp_ns
                        .saturating_sub(stream.start_timestamp_ns),
                    first_output_timestamp_ns: stream.first_output_timestamp_ns(),
                    is_sse: sse,
                    sse_event_count: stream.response_sse_event_count(),
                },
            });
        }
        _ => return None,
    };
    Some(HttpContext {
        retain_raw: false,
        request: request.json_body(),
        response: response.and_then(|response| {
            if sse {
                Some(Value::Array(response.json_body()))
            } else {
                response.parsed.json_body()
            }
        }),
        http: HttpRecord {
            timestamp_ns: request.source_event.timestamp_ns,
            pid: request.source_event.pid,
            comm: request.source_event.comm_str(),
            method: request.method.clone(),
            path: request.path.clone(),
            status_code: response.map_or(0, |response| response.parsed.status_code),
            request_headers: serde_json::to_string(&request.headers).unwrap_or_default(),
            response_headers: response
                .map(|response| serde_json::to_string(&response.parsed.headers).unwrap_or_default())
                .unwrap_or_default(),
            request_body: None,
            response_body: None,
            duration_ns: response.map_or(0, |response| {
                response
                    .end_timestamp_ns()
                    .saturating_sub(request.source_event.timestamp_ns)
            }),
            first_output_timestamp_ns: response
                .and_then(|response| response.first_output_timestamp_ns()),
            is_sse: sse,
            sse_event_count: response.map_or(0, |response| {
                response
                    .captured_sse_event_count
                    .unwrap_or(response.sse_events.len())
            }),
        },
    })
}
