// Scanner, writer, and exactness policy for retained numeric literals.

/// Literals that `serde_json::Value` cannot reproduce, plus the ones it can.
const LITERALS: [&str; 23] = [
    "0",
    "42",
    "-0",
    "-0.0",
    "0.0",
    "1.2500",
    "1e3",
    "1E+3",
    "1000.00",
    "9007199254740993",
    "9223372036854775807",
    "9223372036854775808",
    "-9223372036854775808",
    "-9223372036854775809",
    "18446744073709551615",
    "18446744073709551616",
    "18446744073709551617",
    "1.234567890123456789",
    "0.1000000000000000055511151231257827",
    "123456789012345678901234567890",
    "5e-324",
    "1e-400",
    "1.7976931348623157e308",
];

#[test]
fn retained_literals_round_trip_verbatim() {
    for literal in LITERALS {
        let source = format!(r#"{{"nested":[{{"value":{literal}}}]}}"#);
        let document = parse(&source).unwrap_or_else(|_| panic!("{source} must parse"));
        assert_eq!(write_to_string(&document), source, "{literal} changed");
    }
}

#[test]
fn scanner_acceptance_matches_serde_json() {
    let mut documents = [
        "",
        " ",
        "null",
        "true",
        "false",
        "tru",
        "0",
        "-0",
        "01",
        "1.",
        ".1",
        "+1",
        "1e",
        "1e+",
        "-",
        "1 2",
        "[1,]",
        "[,1]",
        "[1",
        "{\"a\":1,}",
        "{\"a\"}",
        "{\"a\":}",
        "{\"a\":1",
        "{'a':1}",
        "\"unterminated",
        "\"\\ud800\"",
        "\"\\ud83d\\ude00\"",
        "\"raw\u{1f}\"",
        "\"\\/\"",
        "\"\\u001f\"",
        "[1,2,3]",
        "[]",
        "{}",
        "[[]]",
        "{\"a\":{\"b\":[1,2,{\"c\":null}]}}",
        "{\"a\":1,\"a\":2}",
        "NaN",
        "Infinity",
        "1e400",
        "-1e400",
        "1e309",
        "2e308",
        "1e308",
        "1e-400",
        "1e-323",
        "5e-324",
        "1e999999999999999999",
        "0e999999999999999999",
        "0.0e400",
        "18446744073709551617",
        "123456789012345678901234567890",
        "1.234567890123456789",
        "\"中文\"",
    ]
    .iter()
    .map(|source| source.to_string())
    .collect::<Vec<_>>();
    // `serde_json` stops at the 128th nested container; the scanner must agree,
    // otherwise adversarial nesting reaches it before any limit applies.
    for depth in [1usize, 2, 126, 127, 128, 129, 500] {
        documents.push(format!("{}1{}", "[".repeat(depth), "]".repeat(depth)));
        documents.push(format!(
            "{}1{}",
            r#"{"a":"#.repeat(depth),
            "}".repeat(depth)
        ));
    }
    let mut divergences = 0;
    for source in &documents {
        let scanned = parse(source).is_ok();
        let reference = serde_json::from_str::<Value>(source).is_ok();
        if scanned != reference {
            divergences += 1;
            eprintln!("divergence: scanned={scanned} serde_json={reference} for {source:?}");
        }
    }
    assert_eq!(divergences, 0);
}

#[test]
fn writer_matches_serde_json_when_numbers_are_exact() {
    for source in [
        r#"{"a":1,"b":[1,2,3],"c":"x","d":null,"e":true,"f":{},"g":[]}"#,
        r#"["é","😀","\u0000\u001f","quote\"back\\slash","slash/forward"]"#,
        r#"{"nested":{"deep":[{"x":-0.0},{"y":1000.0},{"z":18446744073709551615}]}}"#,
        r#"{"tab":"\t","nl":"\n","cr":"\r","bs":"\b","ff":"\f","del":"\u007f"}"#,
        r#"{"$serde_json::private::RawValue":"123","$serde_json::private::Number":"456"}"#,
        "[[[[[1]]]]]",
        r#"{"a":"\ud83d\ude00","b":"\u007f","c":"\u2028"}"#,
    ] {
        let document = parse(source).unwrap_or_else(|_| panic!("{source} must parse"));
        let reference: Value = serde_json::from_str(source).unwrap();
        assert_eq!(write_to_string(&document), serde_json::to_string(&reference).unwrap());
    }
}

#[test]
fn string_escapes_match_serde_json() {
    for text in [
        "",
        "plain",
        "quote\"back\\slash",
        "slash/forward",
        "\u{0}\u{1f}\u{7f}",
        "tab\tnl\r\n\u{8}\u{c}",
        "é😀",
        "\u{2028}\u{2029}",
        "x".repeat(4096).as_str(),
    ] {
        let source = serde_json::to_string(text).unwrap();
        let document = parse(&source).expect("serde_json output parses");
        assert_eq!(document, ExactValue::String(text.to_owned()));
        assert_eq!(write_to_string(&document), source);
    }
}

#[test]
fn duplicate_keys_keep_the_last_value() {
    let document = parse(r#"{"a":1,"a":2}"#).unwrap();
    assert_eq!(write_to_string(&document), r#"{"a":2}"#);
    let document = parse(r#"{"a":{"b":1,"b":2},"a":{"c":3}}"#).unwrap();
    assert_eq!(write_to_string(&document), r#"{"a":{"c":3}}"#);
}

#[test]
fn decimal_value_unites_spellings_and_separates_values() {
    assert_eq!(decimal_value("1.2500"), decimal_value("1.25"));
    assert_eq!(decimal_value("1e3"), decimal_value("1000.00"));
    assert_eq!(decimal_value("0"), decimal_value("0.000"));
    assert_eq!(decimal_value("1E+3"), decimal_value("1000"));
    assert_ne!(decimal_value("-0"), decimal_value("0"));
    assert_ne!(decimal_value("-0.0"), decimal_value("0.0"));
    assert_ne!(decimal_value("1.234567890123456789"), decimal_value("1.2345678901234567"));
    assert_ne!(decimal_value("18446744073709551617"), decimal_value("18446744073709551615"));
    assert_ne!(decimal_value("1e-400"), decimal_value("0"));
    for literal in LITERALS {
        assert!(decimal_value(literal).is_some(), "{literal} has no decimal value");
    }
}

#[test]
fn matches_exact_rejects_precision_type_and_sign_changes() {
    for (expected, decoded, retained) in [
        ("42", "42", true),
        ("1.25", "1.25", true),
        ("1.2500", "1.25", true),
        ("1e3", "1000.0", true),
        ("-0", "-0.0", true),
        ("18446744073709551615", "18446744073709551615", true),
        ("1.234567890123456789", "1.2345678901234567", false),
        ("18446744073709551617", "18446744073709551615", false),
        ("18446744073709551617", "1.8446744073709552e19", false),
        ("-9223372036854775809", "-9.223372036854776e18", false),
        ("1e-400", "0", false),
        ("1e-400", "0.0", false),
        ("-0", "0", false),
        ("-0", "0.0", false),
        ("1e3", "1000", false),
        ("42", "42.0", false),
        ("1.0", "1", false),
    ] {
        let expected_source = format!(r#"{{"v":{expected}}}"#);
        let document = parse(&expected_source).unwrap();
        let decoded_source = format!(r#"{{"v":{decoded}}}"#);
        let candidate: Value = serde_json::from_str(&decoded_source).unwrap();
        assert_eq!(
            matches_exact(&candidate, &document),
            retained,
            "{expected} decoded as {decoded}"
        );
    }
}

#[test]
fn matches_exact_checks_structure_strings_and_types() {
    let document = parse(r#"{"a":[1,"2",null,true,{"b":3}]}"#).unwrap();
    assert!(matches_exact(
        &serde_json::from_str(r#"{"a":[1,"2",null,true,{"b":3}]}"#).unwrap(),
        &document
    ));
    for candidate in [
        r#"{"a":[1,"2",null,true]}"#,
        r#"{"a":[1,"2",null,true,{"b":3},{"c":4}]}"#,
        r#"{"a":[1,2,null,true,{"b":3}]}"#,
        r#"{"a":[1,"2",null,true,{"b":"3"}]}"#,
        r#"{"a":[1,"2",null,true,{"b":3.0}]}"#,
        r#"{"b":[1,"2",null,true,{"b":3}]}"#,
        r#"[1,"2",null,true,{"b":3}]"#,
        r#"{"a":[1,"2",null,true,{"b":4}]}"#,
    ] {
        assert!(
            !matches_exact(&serde_json::from_str(candidate).unwrap(), &document),
            "{candidate} must not pass as the same document"
        );
    }
}

#[test]
fn dedup_keys_normalize_spelling_without_touching_output() {
    let verbose = parse(r#"{"v":1.2500,"w":[1e3,-0]}"#).unwrap();
    let short = parse(r#"{"v":1.25,"w":[1000.0,-0.0]}"#).unwrap();
    let mut verbose_key = String::new();
    let mut short_key = String::new();
    write_dedup_key(&verbose, &mut verbose_key);
    write_dedup_key(&short, &mut short_key);
    assert_eq!(verbose_key, short_key);
    assert_eq!(
        write_to_string(&verbose),
        r#"{"v":1.2500,"w":[1e3,-0]}"#.to_owned()
    );
    assert_ne!(write_to_string(&verbose), write_to_string(&short));
}

#[test]
fn approximate_values_are_for_statistics_only() {
    assert_eq!(approximate("1.2500"), Some(1.25));
    assert_eq!(approximate("1e-400"), Some(0.0));
    assert_eq!(approximate("18446744073709551617"), Some(1.8446744073709552e19));
    // No finite approximation means no place in the statistics, and the caller
    // retains the record instead of judging it by numbers it never saw.
    assert_eq!(approximate("1e400"), None);
    assert_eq!(approximate("-1e400"), None);
}

#[test]
fn to_serde_value_approximates_every_retained_literal() {
    for literal in LITERALS {
        let source = format!(r#"{{"value":{literal}}}"#);
        let document = parse(&source).unwrap();
        let approximate = to_serde_value(&document).expect("retained literals stay readable");
        assert_eq!(approximate, serde_json::from_str::<Value>(&source).unwrap());
    }
}
