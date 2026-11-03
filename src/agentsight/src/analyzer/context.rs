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
        if let Some(metadata) = resource_abandonment(result) {
            return metadata;
        }
        if let AggregatedResult::Abandoned { partial, reason } = result {
            let mut results = self.analyze_for_delivery(partial, retain_raw);
            results.push(AnalysisResult::Abandoned(*reason));
            return results;
        }
        let Some(context) = context(result, true) else {
            return self.analyze_aggregated(result);
        };
        self.analyze_with_context(result, retain_raw, context)
    }

    /// Consume compact SSE values so semantic delivery avoids cloning full responses.
    pub(crate) fn analyze_for_owned_delivery(
        &self,
        mut result: AggregatedResult,
        retain_raw: bool,
    ) -> Vec<AnalysisResult> {
        if let Some(metadata) = resource_abandonment(&result) {
            return metadata;
        }
        if let AggregatedResult::Abandoned { partial, reason } = result {
            let mut results = self.analyze_for_owned_delivery(*partial, retain_raw);
            results.push(AnalysisResult::Abandoned(reason));
            return results;
        }
        let path = match &result {
            AggregatedResult::SseComplete(pair) => pair.request.path.clone(),
            AggregatedResult::Http2StreamComplete(stream)
                if stream.reduced_sse_events.is_some() =>
            {
                stream.path()
            }
            _ => return self.analyze_for_delivery(&result, retain_raw),
        };
        if retain_raw || !crate::parser::llm::is_llm_api_path(&path) {
            return self.analyze_for_delivery(&result, retain_raw);
        }
        let Some(mut context) = context(&result, false) else {
            return self.analyze_for_delivery(&result, retain_raw);
        };
        if context.http.status_code >= 400 || context.request.is_none() {
            return self.analyze_for_delivery(&result, retain_raw);
        }
        let events = match &mut result {
            AggregatedResult::SseComplete(pair) => std::mem::take(&mut pair.response.sse_events),
            AggregatedResult::Http2StreamComplete(stream) => {
                stream.reduced_sse_events.take().unwrap_or_default()
            }
            _ => Vec::new(),
        };
        context.response = Some(Value::Array(
            events
                .into_iter()
                .filter_map(crate::parser::sse::ParsedSseEvent::into_json_body)
                .collect(),
        ));
        self.analyze_with_context(&result, retain_raw, context)
    }

    fn analyze_with_context(
        &self,
        result: &AggregatedResult,
        retain_raw: bool,
        mut context: HttpContext,
    ) -> Vec<AnalysisResult> {
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

/// Resource removals bypass payload analysis, including token estimation and raw fallback parsing.
pub(super) fn resource_abandonment(result: &AggregatedResult) -> Option<Vec<AnalysisResult>> {
    let AggregatedResult::Abandoned { reason, partial } = result else {
        return None;
    };
    if !reason.metadata_only() {
        return None;
    }
    let (id, request, response, is_sse) = match partial.as_ref() {
        AggregatedResult::RequestOnly {
            connection_id,
            request,
        } => (*connection_id, Some(request), None, false),
        AggregatedResult::HttpComplete(pair) | AggregatedResult::SseComplete(pair) => (
            pair.connection_id,
            Some(&pair.request),
            Some(&pair.response),
            matches!(partial.as_ref(), AggregatedResult::SseComplete(_)),
        ),
        AggregatedResult::ResponseOnly {
            connection_id,
            response,
        } => (
            *connection_id,
            None,
            Some(response),
            response.parsed.is_sse(),
        ),
        AggregatedResult::Http2StreamComplete(stream) => {
            return Some(vec![
                AnalysisResult::Http(HttpRecord {
                    timestamp_ns: stream.start_timestamp_ns,
                    pid: stream.stream_id.connection_id.pid,
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
                    first_output_timestamp_ns: None,
                    is_sse: stream.is_response_sse(),
                    sse_event_count: 0,
                }),
                AnalysisResult::Abandoned(*reason),
            ]);
        }
        _ => return None,
    };
    let source = request
        .map(|r| r.source_event.as_ref())
        .or_else(|| response.map(|r| r.parsed.source_event.as_ref()))?;
    Some(vec![
        AnalysisResult::Http(HttpRecord {
            timestamp_ns: source.timestamp_ns,
            pid: id.pid,
            comm: source.comm_str(),
            method: request.map_or_else(String::new, |r| r.method.clone()),
            path: request.map_or_else(String::new, |r| r.path.clone()),
            status_code: response.map_or(0, |r| r.parsed.status_code),
            request_headers: request
                .and_then(|r| serde_json::to_string(&r.headers).ok())
                .unwrap_or_default(),
            response_headers: String::new(),
            request_body: None,
            response_body: None,
            duration_ns: response.map_or(0, |r| {
                r.parsed
                    .source_event
                    .timestamp_ns
                    .saturating_sub(source.timestamp_ns)
            }),
            first_output_timestamp_ns: None,
            is_sse,
            sse_event_count: 0,
        }),
        AnalysisResult::Abandoned(*reason),
    ])
}

fn context(result: &AggregatedResult, parse_sse: bool) -> Option<HttpContext> {
    let (request, response, sse) = match result {
        AggregatedResult::HttpComplete(pair) => (&pair.request, Some(&pair.response), false),
        AggregatedResult::SseComplete(pair) => (&pair.request, Some(&pair.response), true),
        AggregatedResult::RequestOnly { request, .. } => (request, None, false),
        AggregatedResult::Http2StreamComplete(stream) => {
            let sse_response = parse_sse
                .then(|| stream.response_sse_json_array())
                .flatten();
            let sse = stream.is_response_sse() || sse_response.is_some();
            let response = if sse && !parse_sse {
                None
            } else {
                sse_response.or_else(|| stream.response_json_body())
            };
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
                parse_sse.then(|| Value::Array(response.json_body()))
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
