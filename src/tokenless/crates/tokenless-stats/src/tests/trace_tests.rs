/// Canonical W3C example traceparent: version 00, sampled.
const VALID: &str = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const SPAN_ID: &str = "00f067aa0ba902b7";

/// Builds an environment lookup over a fixed variable list.
fn lookup(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let owned: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    move |key: &str| {
        owned
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }
}

#[test]
fn test_parse_valid_version_00() {
    let context = TraceContext::parse(VALID).expect("canonical traceparent must parse");
    assert_eq!(context.trace_id, TRACE_ID);
    assert_eq!(context.span_id, SPAN_ID);
}

#[test]
fn test_parse_accepts_unsampled_flags() {
    // Sampling is the host's decision about span export; tokenless keeps the
    // identity either way so savings stay attributable.
    let unsampled = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-00";
    let context = TraceContext::parse(unsampled).expect("unsampled traceparent must parse");
    assert_eq!(context.trace_id, TRACE_ID);
}

#[test]
fn test_parse_higher_version_ignores_trailing_fields() {
    let future = format!("01-{}-{}-01-vendor-extension", TRACE_ID, SPAN_ID);
    let context = TraceContext::parse(&future).expect("forward-compatible version must parse");
    assert_eq!(context.trace_id, TRACE_ID);
    assert_eq!(context.span_id, SPAN_ID);
}

#[test]
fn test_parse_rejects_version_00_with_trailing_field() {
    // Version 00 is fully specified: extra fields mean the sender disagrees
    // about the layout.
    assert!(TraceContext::parse(&format!("{VALID}-extra")).is_none());
}

#[test]
fn test_parse_rejects_reserved_version() {
    let reserved = "ff-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    assert!(TraceContext::parse(reserved).is_none());
}

#[test]
fn test_parse_rejects_uppercase_hex() {
    let uppercase = "00-4BF92F3577B34DA6A3CE929D0E0E4736-00F067AA0BA902B7-01";
    assert!(TraceContext::parse(uppercase).is_none());
}

#[test]
fn test_parse_rejects_all_zero_ids() {
    let zero_trace = "00-00000000000000000000000000000000-00f067aa0ba902b7-01";
    let zero_span = "00-4bf92f3577b34da6a3ce929d0e0e4736-0000000000000000-01";
    assert!(TraceContext::parse(zero_trace).is_none());
    assert!(TraceContext::parse(zero_span).is_none());
}

#[test]
fn test_parse_rejects_wrong_field_lengths() {
    let short_trace = "00-4bf92f3577b34da6a3ce929d0e0e473-00f067aa0ba902b7-01";
    let long_span = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b70-01";
    let short_flags = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-1";
    let short_version = "0-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    assert!(TraceContext::parse(short_trace).is_none());
    assert!(TraceContext::parse(long_span).is_none());
    assert!(TraceContext::parse(short_flags).is_none());
    assert!(TraceContext::parse(short_version).is_none());
}

#[test]
fn test_parse_rejects_non_hex_characters() {
    let non_hex = "00-zbf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    let spaced = " 00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
    assert!(TraceContext::parse(non_hex).is_none());
    assert!(TraceContext::parse(spaced).is_none());
}

#[test]
fn test_parse_rejects_incomplete_values() {
    assert!(TraceContext::parse("").is_none());
    assert!(TraceContext::parse("00").is_none());
    assert!(TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736").is_none());
    assert!(
        TraceContext::parse("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7").is_none(),
        "trace-flags are mandatory"
    );
}

#[test]
fn test_from_lookup_prefers_tokenless_override() {
    let override_value = "00-11111111111111111111111111111111-2222222222222222-01";
    let vars = [
        (TOKENLESS_TRACEPARENT_ENV, override_value),
        (STANDARD_TRACEPARENT_ENV, VALID),
    ];
    let context = TraceContext::from_lookup(lookup(&vars)).expect("override must win");
    assert_eq!(context.trace_id, "11111111111111111111111111111111");
    assert_eq!(context.span_id, "2222222222222222");
}

#[test]
fn test_from_lookup_falls_back_when_override_unparsable() {
    let vars = [
        (TOKENLESS_TRACEPARENT_ENV, "00-not-a-trace-id"),
        (STANDARD_TRACEPARENT_ENV, VALID),
    ];
    let context = TraceContext::from_lookup(lookup(&vars)).expect("must fall back");
    assert_eq!(context.trace_id, TRACE_ID);
}

#[test]
fn test_from_lookup_treats_empty_as_unset() {
    let vars = [
        (TOKENLESS_TRACEPARENT_ENV, ""),
        (STANDARD_TRACEPARENT_ENV, VALID),
    ];
    let context = TraceContext::from_lookup(lookup(&vars)).expect("empty override must be skipped");
    assert_eq!(context.trace_id, TRACE_ID);
}

#[test]
fn test_from_lookup_none_when_absent_or_invalid() {
    assert!(TraceContext::from_lookup(lookup(&[])).is_none());

    let invalid = [
        (TOKENLESS_TRACEPARENT_ENV, "garbage"),
        (STANDARD_TRACEPARENT_ENV, "00-00000000000000000000000000000000-0000000000000000-01"),
    ];
    assert!(TraceContext::from_lookup(lookup(&invalid)).is_none());
}
