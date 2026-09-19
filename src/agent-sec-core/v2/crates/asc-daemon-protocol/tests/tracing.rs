use asc_daemon_protocol::{
    BUSINESS_FRAME_BYTES, CONTEXT_FRAME_BYTES, DaemonRequest, request_fits_budget,
};

#[test]
fn versioned_envelope_is_strict_without_breaking_absent_context() {
    for context in [
        "null",
        r#"{"version":1}"#,
        r#"{"version":1,"traceparent":"bad-but-string"}"#,
    ] {
        assert!(
            serde_json::from_str::<DaemonRequest>(&format!(
                r#"{{"method":"policy.list","traceContext":{context}}}"#
            ))
            .is_ok()
        );
    }
    for context in [
        r"{}",
        r#"{"version":2}"#,
        r#"{"version":1,"traceparent":null}"#,
        r#"{"version":1,"baggage":4}"#,
        r#"{"version":1,"secret":"x"}"#,
        r#"{"version":1,"version":1}"#,
        r#"{"version":1,"baggage":"a","baggage":"b"}"#,
    ] {
        assert!(
            serde_json::from_str::<DaemonRequest>(&format!(
                r#"{{"method":"policy.list","traceContext":{context}}}"#
            ))
            .is_err(),
            "{context}"
        );
    }
    assert!(
        serde_json::from_str::<DaemonRequest>(
            r#"{"method":"policy.list","traceContext":null,"traceContext":null}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<DaemonRequest>(
            r#"{"method":"policy.list","compatibility":{"version":1,"traceId":null}}"#
        )
        .is_ok()
    );
}

#[test]
fn propagation_cannot_consume_business_capacity_or_hide_raw_whitespace() {
    let prefix = r#"{"method":"policy.list","params":{"value":""#;
    let suffix = "\"}}\n";
    let fill = "x".repeat(BUSINESS_FRAME_BYTES - prefix.len() - suffix.len());
    let base = format!("{prefix}{fill}{suffix}");
    assert_eq!(base.len(), BUSINESS_FRAME_BYTES);
    assert!(request_fits_budget(base.as_bytes()).unwrap());
    for native in [
        r#", "traceContext":{"version":1}"#.to_owned(),
        format!(
            r#", "traceContext":{{"version":1,"baggage":"{}"}}"#,
            "x".repeat(16000)
        ),
    ] {
        let extended = format!("{}{native}}}\n", base[..base.len() - 2].to_owned());
        assert!(request_fits_budget(extended.as_bytes()).unwrap());
        let too_much = format!(" {extended}");
        assert!(!request_fits_budget(too_much.as_bytes()).unwrap());
    }
    let over_context = format!(
        r#"{{"method":"policy.list","traceContext":{{"version":1,"baggage":"{}"}}}}"#,
        "x".repeat(CONTEXT_FRAME_BYTES)
    );
    assert!(!request_fits_budget(over_context.as_bytes()).unwrap());
}
