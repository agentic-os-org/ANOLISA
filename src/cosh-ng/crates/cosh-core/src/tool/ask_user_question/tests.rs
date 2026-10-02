use super::*;

/// Every rejection case that the provider stream can hand to Core, pinned to a
/// stable error code so the tool-result text stays diagnosable.
#[test]
fn invalid_arguments_map_to_stable_error_codes() {
    let cases: &[(&str, &str, AskUserArgumentError, &str)] = &[
        (
            "no argument delta at all",
            "",
            AskUserArgumentError::EmptyArguments,
            "unparsed",
        ),
        (
            "whitespace-only arguments",
            "   \n",
            AskUserArgumentError::EmptyArguments,
            "unparsed",
        ),
        (
            "truncated json",
            r#"{"question":"Which branch"#,
            AskUserArgumentError::InvalidJson,
            "unparsed",
        ),
        (
            "not json at all",
            "question: which branch?",
            AskUserArgumentError::InvalidJson,
            "unparsed",
        ),
        (
            "array root",
            r#"[{"question":"Which branch?"}]"#,
            AskUserArgumentError::RootNotObject,
            "root_not_object",
        ),
        (
            "string root",
            r#""Which branch?""#,
            AskUserArgumentError::RootNotObject,
            "root_not_object",
        ),
        (
            "empty object",
            "{}",
            AskUserArgumentError::MissingQuestion,
            "missing",
        ),
        (
            "question omitted but options present",
            r#"{"options":[{"label":"Stash"}]}"#,
            AskUserArgumentError::MissingQuestion,
            "missing",
        ),
        (
            "explicit null question",
            r#"{"question":null}"#,
            AskUserArgumentError::QuestionWrongType,
            "null",
        ),
        (
            "number question",
            r#"{"question":42}"#,
            AskUserArgumentError::QuestionWrongType,
            "wrong_type_number",
        ),
        (
            "array question",
            r#"{"question":["one","two"]}"#,
            AskUserArgumentError::QuestionWrongType,
            "wrong_type_array",
        ),
        (
            "object question",
            r#"{"question":{"text":"Which branch?"}}"#,
            AskUserArgumentError::QuestionWrongType,
            "wrong_type_object",
        ),
        (
            "boolean question",
            r#"{"question":true}"#,
            AskUserArgumentError::QuestionWrongType,
            "wrong_type_boolean",
        ),
        (
            "empty question",
            r#"{"question":""}"#,
            AskUserArgumentError::EmptyQuestion,
            "empty_string",
        ),
        (
            "whitespace question",
            r#"{"question":"  \t \n "}"#,
            AskUserArgumentError::EmptyQuestion,
            "empty_string",
        ),
        (
            "claude-style nested questions",
            r#"{"questions":[{"question":"How should local changes be handled?","header":"Local changes","options":[{"label":"Stash"}],"multiSelect":false}]}"#,
            AskUserArgumentError::UnsupportedNestedQuestions,
            "nested_questions",
        ),
        (
            "nested questions alongside a valid question",
            r#"{"question":"Which branch?","questions":[{"question":"And then?"}]}"#,
            AskUserArgumentError::UnsupportedNestedQuestions,
            "nested_questions",
        ),
        (
            "options wrong type",
            r#"{"question":"Which branch?","options":"Stash"}"#,
            AskUserArgumentError::OptionsWrongType,
            "nonempty_string",
        ),
        (
            "explicit null options",
            r#"{"question":"Which branch?","options":null}"#,
            AskUserArgumentError::OptionsWrongType,
            "nonempty_string",
        ),
        (
            "explicit null allow_free_text",
            r#"{"question":"Which branch?","allow_free_text":null}"#,
            AskUserArgumentError::AllowFreeTextWrongType,
            "nonempty_string",
        ),
        (
            "explicit null multi_select",
            r#"{"question":"Which branch?","multi_select":null}"#,
            AskUserArgumentError::MultiSelectWrongType,
            "nonempty_string",
        ),
        (
            "explicit null option description",
            r#"{"question":"Which branch?","options":[{"label":"Stash","description":null}]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "explicit null option label",
            r#"{"question":"Which branch?","options":[{"label":null}]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "bare string option is not a declared format",
            r#"{"question":"Which branch?","options":["Stash"]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "option missing label",
            r#"{"question":"Which branch?","options":[{"description":"keep it"}]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "option label wrong type",
            r#"{"question":"Which branch?","options":[{"label":7}]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "option label blank",
            r#"{"question":"Which branch?","options":[{"label":"   "}]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "option description wrong type",
            r#"{"question":"Which branch?","options":[{"label":"Stash","description":3}]}"#,
            AskUserArgumentError::OptionInvalid,
            "nonempty_string",
        ),
        (
            "allow_free_text wrong type",
            r#"{"question":"Which branch?","allow_free_text":"yes"}"#,
            AskUserArgumentError::AllowFreeTextWrongType,
            "nonempty_string",
        ),
        (
            "multi_select wrong type",
            r#"{"question":"Which branch?","multi_select":1}"#,
            AskUserArgumentError::MultiSelectWrongType,
            "nonempty_string",
        ),
        (
            "no answer path without options",
            r#"{"question":"Which branch?","allow_free_text":false}"#,
            AskUserArgumentError::NoAnswerPath,
            "nonempty_string",
        ),
        (
            "no answer path with empty options",
            r#"{"question":"Which branch?","allow_free_text":false,"options":[]}"#,
            AskUserArgumentError::NoAnswerPath,
            "nonempty_string",
        ),
    ];

    for (label, raw, expected_error, expected_shape) in cases {
        let report = inspect_arguments(raw);
        assert_eq!(
            report.outcome.as_ref().err().copied(),
            Some(*expected_error),
            "case: {label}"
        );
        assert_eq!(report.question_shape, *expected_shape, "case: {label}");
        assert_eq!(report.argument_bytes, raw.len(), "case: {label}");

        let message = expected_error.tool_error_message();
        assert!(
            message.contains(expected_error.code()),
            "case: {label} message must carry its code"
        );
        assert!(
            !message.contains("Agent needs your input"),
            "case: {label} must not fall back to a generic prompt"
        );
    }
}

#[test]
fn json_parse_status_distinguishes_empty_from_malformed() {
    assert_eq!(inspect_arguments("").json_parse_status, JSON_PARSE_EMPTY);
    assert_eq!(
        inspect_arguments("{\"question\":").json_parse_status,
        JSON_PARSE_INVALID
    );
    assert_eq!(
        inspect_arguments(r#"{"question":"ok"}"#).json_parse_status,
        JSON_PARSE_OK
    );
}

#[test]
fn error_codes_are_unique() {
    let all = [
        AskUserArgumentError::EmptyArguments,
        AskUserArgumentError::InvalidJson,
        AskUserArgumentError::RootNotObject,
        AskUserArgumentError::MissingQuestion,
        AskUserArgumentError::QuestionWrongType,
        AskUserArgumentError::EmptyQuestion,
        AskUserArgumentError::OptionsWrongType,
        AskUserArgumentError::OptionInvalid,
        AskUserArgumentError::AllowFreeTextWrongType,
        AskUserArgumentError::MultiSelectWrongType,
        AskUserArgumentError::NoAnswerPath,
        AskUserArgumentError::UnsupportedNestedQuestions,
    ];
    let mut codes: Vec<&str> = all.iter().map(|e| e.code()).collect();
    codes.sort_unstable();
    let total = codes.len();
    codes.dedup();
    assert_eq!(codes.len(), total, "error codes must be unique");
}

#[test]
fn valid_arguments_apply_documented_defaults() {
    let report = inspect_arguments(
        r#"{"question":"  How should local changes be handled?  ","options":[{"label":" Stash ","description":"git stash"},{"label":"Discard"}]}"#,
    );
    let params = report.outcome.expect("valid arguments");
    assert_eq!(params.question, "How should local changes be handled?");
    assert!(params.allow_free_text, "allow_free_text defaults to true");
    assert!(!params.multi_select, "multi_select defaults to false");
    assert_eq!(
        params.options,
        vec![
            AskUserQuestionOption {
                label: "Stash".to_string(),
                description: Some("git stash".to_string()),
            },
            AskUserQuestionOption {
                label: "Discard".to_string(),
                description: None,
            },
        ]
    );
    assert_eq!(report.question_shape, "nonempty_string");
}

/// Defaults belong to absent keys only: a present `null` violates the advertised
/// string/array/boolean schema and must not be laundered into a valid question.
#[test]
fn omitted_optionals_use_defaults_while_explicit_nulls_are_rejected() {
    let params = inspect_arguments(r#"{"question":"Which branch?"}"#)
        .outcome
        .expect("absent optionals fall back to defaults");
    assert!(params.options.is_empty());
    assert!(params.allow_free_text);
    assert!(!params.multi_select);

    for raw in [
        r#"{"question":null}"#,
        r#"{"question":"Which branch?","options":null}"#,
        r#"{"question":"Which branch?","allow_free_text":null}"#,
        r#"{"question":"Which branch?","multi_select":null}"#,
        r#"{"question":"Which branch?","options":[{"label":"Stash","description":null}]}"#,
    ] {
        assert!(
            inspect_arguments(raw).outcome.is_err(),
            "explicit null must be rejected: {raw}"
        );
    }
}

#[test]
fn options_only_question_may_disable_free_text() {
    let params = inspect_arguments(
        r#"{"question":"Which branch?","allow_free_text":false,"multi_select":true,"options":[{"label":"main"}]}"#,
    )
    .outcome
    .expect("options provide an answer path");
    assert!(!params.allow_free_text);
    assert!(params.multi_select);
    assert_eq!(params.options.len(), 1);
}

#[test]
fn declaration_matches_validated_contract() {
    let declaration = declaration();
    assert_eq!(declaration.name, TOOL_NAME);
    let schema = declaration.parameters;
    assert_eq!(schema["required"], serde_json::json!(["question"]));
    assert_eq!(schema["properties"]["question"]["type"], "string");
    assert_eq!(
        schema["properties"]["options"]["items"]["required"],
        serde_json::json!(["label"])
    );
    // The validator ignores unknown keys; the schema must not claim otherwise.
    assert!(schema.get("additionalProperties").is_none());
}
