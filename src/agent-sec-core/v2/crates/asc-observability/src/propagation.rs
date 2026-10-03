use crate::fields::{FIELDS, agent_values, bounded};
use opentelemetry::trace::TraceContextExt as _;
use opentelemetry::{
    Context, KeyValue,
    baggage::BaggageExt as _,
    propagation::{Extractor, Injector, TextMapPropagator as _},
};
use opentelemetry_sdk::propagation::TraceContextPropagator;
use std::collections::{HashMap, HashSet};

/// Extracts from a clean Context, never the worker's ambient request. Malformed
/// W3C contents are ignored independently; malformed envelope schemas belong to
/// the protocol layer. Unknown baggage never reaches business consumers.
pub fn extract_parent(carrier: &dyn Extractor) -> Context {
    let mut headers = HashMap::<String, String>::new();
    for (key, limit) in [("traceparent", 512), ("tracestate", 512)] {
        if let Some(value) = carrier.get(key).filter(|v| v.len() <= limit) {
            headers.insert(key.into(), value.into());
        }
    }
    let parent = TraceContextPropagator::new().extract_with_context(&Context::new(), &headers);
    let mut reasons = Vec::new();
    if carrier.get("traceparent").is_some() && !parent.span().span_context().is_valid() {
        reasons.push("invalid_traceparent");
    }
    if carrier
        .get("tracestate")
        .is_some_and(|value| !value.is_empty())
        && parent
            .span()
            .span_context()
            .trace_state()
            .header()
            .is_empty()
    {
        reasons.push("invalid_tracestate");
    }
    let parent = if let Some(baggage) = carrier.get("baggage") {
        if let Some(values) = parse_baggage(baggage) {
            parent.with_baggage(values)
        } else {
            reasons.push("invalid_baggage");
            parent
        }
    } else {
        parent
    };
    parent.with_value(PropagationIssues(reasons))
}

fn parse_baggage(baggage: &str) -> Option<Vec<KeyValue>> {
    if baggage.len() > 16384 {
        return None;
    }
    if baggage.is_empty() {
        return Some(Vec::new());
    }
    let members: Vec<_> = baggage.split(',').collect();
    if members.len() > 32 {
        return None;
    }
    let mut seen = HashSet::new();
    let mut values = Vec::new();
    for member in members {
        let mut sections = member.split(';');
        let (key, value) = sections.next()?.split_once('=')?;
        for property in sections {
            let (key, value) = property.trim_matches([' ', '\t']).split_once('=').map_or(
                (property.trim_matches([' ', '\t']), None),
                |(key, value)| {
                    (
                        key.trim_matches([' ', '\t']),
                        Some(value.trim_matches([' ', '\t'])),
                    )
                },
            );
            if !valid_key(key) || value.is_some_and(|value| !valid_value(value)) {
                return None;
            }
        }
        let key = key.trim_matches([' ', '\t']);
        if !valid_key(key) {
            return None;
        }
        let raw = value.trim_matches([' ', '\t']);
        if !valid_value(raw) {
            return None;
        }
        if FIELDS.iter().any(|(allowed, _, _)| *allowed == key) {
            let decoded = percent_encoding::percent_decode_str(raw)
                .decode_utf8()
                .ok()?;
            if !seen.insert(key) {
                return None;
            }
            // Preserve metadata's significant whitespace. V1 trace-context
            // normalization happens once at its own ingress, before propagation.
            values.push(KeyValue::new(key.to_owned(), bounded(&decoded)));
        }
    }
    Some(values)
}

/// Injects only W3C identity and bounded Agent baggage, even when ambient baggage
/// contains unrelated application data. The caller controls the output carrier.
pub fn inject_context(context: &Context, carrier: &mut dyn Injector) {
    // Escape original values once: SDK injection trims significant whitespace.
    // Non-ASCII bytes are always escaped, including with this smaller ASCII set.
    const VALUE: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b';')
        .add(b',')
        .add(b'=')
        .add(b'\\')
        .add(b'%');
    TraceContextPropagator::new().inject_context(context, carrier);
    let members: Vec<_> = agent_values(context)
        .into_iter()
        .map(|item| {
            format!(
                "{}={}",
                item.key,
                percent_encoding::utf8_percent_encode(&item.value.to_string(), VALUE)
            )
        })
        .collect();
    if !members.is_empty() {
        carrier.set("baggage", members.join(","));
    }
}

#[derive(Debug, Clone)]
struct PropagationIssues(Vec<&'static str>);

/// Emits content-validation reasons only after entering the new request span,
/// never against a worker's previous ambient context or with raw carrier values.
pub fn report_propagation_issues() {
    if let Some(issues) = Context::current().get::<PropagationIssues>() {
        for reason in &issues.0 {
            crate::diagnostic(reason);
        }
    }
}

fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}
fn valid_value(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' {
            if i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit()
            {
                return false;
            }
            i += 3;
        } else {
            if !(0x21..=0x7e).contains(&b) || b"\";,\\".contains(&b) {
                return false;
            }
            i += 1;
        }
    }
    true
}
