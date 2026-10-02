use super::*;

#[test]
fn text_without_the_marker_carries_no_question() {
    assert_eq!(
        parse_in_band_question("Here is the plan, no question needed."),
        InBandQuestion::Absent
    );
    assert_eq!(parse_in_band_question(""), InBandQuestion::Absent);
}

#[test]
fn marked_text_with_a_valid_payload_shares_the_tool_contract() {
    let InBandQuestion::Valid(params) = parse_in_band_question(
        "Deciding.\nCOSH_QUESTION:{\"question\":\"Which branch?\",\"options\":[{\"label\":\"main\"}]}",
    ) else {
        panic!("valid payload must produce a question");
    };
    assert_eq!(params.question, "Which branch?");
    assert_eq!(params.options.len(), 1);
    assert!(params.allow_free_text, "omitted optional keeps its default");
}

/// The marker suppresses the visible assistant text, so every rejected payload
/// must be reported as a failure rather than collapsing into `Absent`.
#[test]
fn marked_text_with_an_unusable_payload_is_invalid_not_absent() {
    let cases: &[(&str, AskUserArgumentError)] = &[
        ("COSH_QUESTION:", AskUserArgumentError::EmptyArguments),
        ("COSH_QUESTION:   ", AskUserArgumentError::EmptyArguments),
        (
            "COSH_QUESTION:{\"question\":\"Which bra",
            AskUserArgumentError::InvalidJson,
        ),
        (
            "COSH_QUESTION:{\"prompt\":\"Which branch?\"}",
            AskUserArgumentError::MissingQuestion,
        ),
        (
            "COSH_QUESTION:{\"question\":null}",
            AskUserArgumentError::QuestionWrongType,
        ),
        (
            "COSH_QUESTION:{\"question\":\"Which branch?\",\"allow_free_text\":false}",
            AskUserArgumentError::NoAnswerPath,
        ),
        (
            "COSH_QUESTION:[{\"question\":\"Which branch?\"}]",
            AskUserArgumentError::RootNotObject,
        ),
    ];

    for (text, expected) in cases {
        assert_eq!(
            parse_in_band_question(text),
            InBandQuestion::Invalid(*expected),
            "payload {text:?} must be reported as invalid"
        );
    }
}

/// The failure text is what the user sees, so it must name the cause without
/// echoing a payload that may contain session content.
#[test]
fn in_band_error_names_the_code_without_echoing_the_payload() {
    let message = in_band_question_error(AskUserArgumentError::MissingQuestion);
    assert!(message.contains("code=missing_question"), "{message}");
    assert!(message.contains("COSH_QUESTION"), "{message}");
    assert!(message.contains("No question was shown"), "{message}");
}

#[test]
fn empty_arguments_are_an_empty_object_and_malformed_ones_are_errors() {
    assert_eq!(
        parse_tool_arguments("  \n"),
        Ok(serde_json::Value::Object(serde_json::Map::new()))
    );
    assert_eq!(
        parse_tool_arguments(r#"{"command":"ls"#),
        Err(ArgumentError::InvalidJson)
    );
    assert_eq!(
        parse_tool_arguments(r#"{"command":"ls"}"#),
        Ok(serde_json::json!({"command": "ls"}))
    );
}

/// Every tool declares an object root. A non-object payload parses cleanly, so
/// without this check it would pass admission and reach the tool — as `null`
/// (every field looks absent) or as arguments an MCP server never declared.
#[test]
fn parsed_arguments_that_are_not_an_object_are_rejected() {
    let cases: &[(&str, &str)] = &[
        ("null", "null"),
        ("[]", "array"),
        (r#"[{"command":"ls"}]"#, "array"),
        ("7", "number"),
        ("true", "boolean"),
        (r#""command""#, "string"),
    ];

    for (raw, shape) in cases {
        assert_eq!(
            parse_tool_arguments(raw),
            Err(ArgumentError::RootNotObject { shape }),
            "payload {raw:?} must be refused"
        );
    }
}

#[test]
fn rejection_message_carries_a_code_and_no_payload() {
    let malformed = invalid_arguments_message(
        "shell",
        &ArgumentError::InvalidJson,
        1,
        MAX_INVALID_ARGUMENT_ATTEMPTS,
    );
    assert!(malformed.contains("code=invalid_json"), "{malformed}");
    assert!(malformed.contains("shell"), "{malformed}");

    let wrong_root = invalid_arguments_message(
        "shell",
        &ArgumentError::RootNotObject { shape: "array" },
        1,
        MAX_INVALID_ARGUMENT_ATTEMPTS,
    );
    assert!(
        wrong_root.contains("code=arguments_not_object"),
        "{wrong_root}"
    );
    assert!(wrong_root.contains("JSON array"), "{wrong_root}");
}

#[test]
fn rejection_message_shows_the_attempt_and_stops_inviting_retries_at_the_limit() {
    let retryable = invalid_arguments_message(
        "write_file",
        &ArgumentError::InvalidJson,
        1,
        MAX_INVALID_ARGUMENT_ATTEMPTS,
    );
    assert!(retryable.contains("attempt 1/3"), "{retryable}");
    assert!(retryable.contains("re-issue the call"), "{retryable}");

    let exhausted = invalid_arguments_message(
        "write_file",
        &ArgumentError::InvalidJson,
        MAX_INVALID_ARGUMENT_ATTEMPTS,
        MAX_INVALID_ARGUMENT_ATTEMPTS,
    );
    assert!(exhausted.contains("attempt 3/3"), "{exhausted}");
    assert!(
        !exhausted.contains("re-issue the call"),
        "the budget is spent, so the model must not be told to try again: {exhausted}"
    );
    assert!(exhausted.contains("run was stopped"), "{exhausted}");
}

#[test]
fn streak_counts_the_same_tool_and_code_and_resets_on_anything_else() {
    let mut streak = InvalidArgumentStreak::default();

    assert_eq!(streak.record("write_file", "invalid_json", "turn-1"), 1);
    assert_eq!(streak.record("write_file", "invalid_json", "turn-2"), 2);

    // A different error code from the same tool is a different failure.
    assert_eq!(
        streak.record("write_file", "arguments_not_object", "turn-3"),
        1
    );
    // As is the same code from a different tool.
    assert_eq!(streak.record("shell", "arguments_not_object", "turn-4"), 1);
    assert_eq!(streak.record("shell", "arguments_not_object", "turn-5"), 2);

    // A parseable call in between means the model recovered.
    streak.clear();
    assert_eq!(streak.record("shell", "arguments_not_object", "turn-6"), 1);
}

#[test]
fn streak_reaches_the_limit_only_on_three_consecutive_identical_failures() {
    let mut streak = InvalidArgumentStreak::default();

    assert!(streak.record("write_file", "invalid_json", "turn-1") < MAX_INVALID_ARGUMENT_ATTEMPTS);
    assert!(streak.record("write_file", "invalid_json", "turn-2") < MAX_INVALID_ARGUMENT_ATTEMPTS);
    assert_eq!(
        streak.record("write_file", "invalid_json", "turn-3"),
        MAX_INVALID_ARGUMENT_ATTEMPTS
    );
}

#[test]
fn streak_counts_matching_calls_only_once_per_provider_turn() {
    let mut streak = InvalidArgumentStreak::default();

    assert_eq!(streak.record("write_file", "invalid_json", "turn-1"), 1);
    assert_eq!(
        streak.record("write_file", "invalid_json", "turn-1"),
        1,
        "parallel calls in one assistant message are not retries"
    );
    assert_eq!(streak.record("write_file", "invalid_json", "turn-2"), 2);
}

#[test]
fn display_tool_name_strips_control_characters_and_bounds_length() {
    // A provider name is model output: the escape would reach a terminal intact.
    assert_eq!(
        display_tool_name("write\u{1b}[2Jfile"),
        "write[2Jfile",
        "the ESC byte must be gone, leaving the body as inert text"
    );
    assert_eq!(display_tool_name("write\r\nfile"), "writefile");
    assert_eq!(display_tool_name("a\u{2028}b"), "ab");
    assert_eq!(display_tool_name("  spaced  "), "spaced");

    // Nothing renderable left, and nothing at all, both need a stand-in.
    assert_eq!(display_tool_name("\u{1b}\r\n"), "tool");
    assert_eq!(display_tool_name(""), "tool");

    let long = display_tool_name(&"n".repeat(200));
    assert_eq!(long.chars().count(), 65, "{long}");
    assert!(long.ends_with('…'), "truncation must be visible: {long}");
}

#[test]
fn rejection_messages_use_the_safe_display_name() {
    let message = invalid_arguments_message(
        "write\u{1b}[2Jfile",
        &ArgumentError::InvalidJson,
        1,
        MAX_INVALID_ARGUMENT_ATTEMPTS,
    );
    assert!(!message.contains('\u{1b}'), "{message}");

    let exhausted =
        invalid_arguments_exhausted_error("write\u{1b}[2Jfile", &ArgumentError::InvalidJson);
    assert!(!exhausted.contains('\u{1b}'), "{exhausted}");

    let skipped = skipped_after_fatal_message("write\u{1b}[2Jfile");
    assert!(!skipped.contains('\u{1b}'), "{skipped}");
    assert!(skipped.contains("was not executed"), "{skipped}");
}

#[test]
fn exhausted_error_names_the_tool_and_code_without_the_payload() {
    let error = invalid_arguments_exhausted_error("write_file", &ArgumentError::InvalidJson);
    assert!(error.contains("write_file"), "{error}");
    assert!(error.contains("code=invalid_json"), "{error}");
    assert!(error.contains("never executed"), "{error}");
}
