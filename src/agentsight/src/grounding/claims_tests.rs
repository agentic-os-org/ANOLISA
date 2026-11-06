use super::*;

fn texts_of(claims: &[Claim], class: ClaimClass) -> Vec<String> {
    claims
        .iter()
        .filter(|c| c.class == class)
        .map(|c| c.text.clone())
        .collect()
}

fn number_values(text: &str) -> Vec<f64> {
    extract_claims(text)
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect()
}

// ---------------------------------------------------------------------------
// The over-flagging guard: trivial arithmetic must produce nothing to ground
// ---------------------------------------------------------------------------

#[test]
fn trivial_arithmetic_yields_no_verifiable_claim() {
    for text in ["1+1=2", "答案是 2", "2 + 3 = 5", "第 7 步"] {
        assert!(
            extract_claims(text).is_empty(),
            "{text:?} must not create a claim needing evidence, got {:?}",
            extract_claims(text)
        );
    }
}

#[test]
fn short_numbers_are_not_claims() {
    assert!(as_number("42").is_none());
    assert!(as_number("7").is_none());
    assert!(as_number("100").is_some(), "three digits is the threshold");
}

// ---------------------------------------------------------------------------
// Numeric normalisation — the dominant source of false "ungrounded"
// ---------------------------------------------------------------------------

#[test]
fn reformatted_numbers_parse_to_the_same_value() {
    let target = 242_391.0;
    assert_eq!(parse_number("242391"), Some(target));
    assert_eq!(parse_number("242,391"), Some(target));
    assert_eq!(parse_number("242_391"), Some(target));

    let approx = parse_number("24.2万").expect("chinese magnitude parses");
    assert!(
        numbers_agree(approx, target),
        "24.2万 should match 242391, got {approx}"
    );

    let rounded = parse_number("240k").expect("si magnitude parses");
    assert!(
        numbers_agree(rounded, target),
        "240k should match 242391, got {rounded}"
    );
}

#[test]
fn approximation_markers_are_stripped() {
    let v = parse_number("约240k").expect("prefix marker tolerated");
    assert!(numbers_agree(v, 242_391.0));
}

#[test]
fn genuinely_different_numbers_do_not_agree() {
    assert!(!numbers_agree(242_391.0, 180_000.0));
    assert!(!numbers_agree(100.0, 0.0));
}

#[test]
fn magnitude_suffix_survives_the_digit_threshold() {
    let claim = as_number("24.2万").expect("abbreviated forms are still claims");
    assert_eq!(claim.class, ClaimClass::Number);
    assert!(numbers_agree(claim.value.unwrap_or_default(), 242_000.0));
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

#[test]
fn urls_lose_trailing_sentence_punctuation() {
    let claims = extract_claims("见 https://example.com/repo/issues/12。");
    assert_eq!(
        texts_of(&claims, ClaimClass::Url),
        vec!["https://example.com/repo/issues/12"]
    );
}

#[test]
fn multiple_urls_are_all_captured() {
    let claims = extract_claims("a https://a.test/x b http://b.test/y c");
    let urls = texts_of(&claims, ClaimClass::Url);
    assert_eq!(urls.len(), 2, "got {urls:?}");
}

#[test]
fn paths_versions_and_identifiers_are_separated() {
    let claims = extract_claims("patched /etc/agentsight/config.json to v1.2.3 via Foo::bar");
    assert!(
        texts_of(&claims, ClaimClass::Path).contains(&"/etc/agentsight/config.json".to_string()),
        "{claims:?}"
    );
    assert!(texts_of(&claims, ClaimClass::Version).contains(&"v1.2.3".to_string()));
    assert!(texts_of(&claims, ClaimClass::Identifier).contains(&"Foo::bar".to_string()));
}

#[test]
fn iso_dates_are_dates_and_bare_years_are_numbers() {
    let iso = extract_claims("released 2026-08-26 already");
    assert_eq!(texts_of(&iso, ClaimClass::Date), vec!["2026-08-26"]);

    let marked = extract_claims("in 2026年 things changed");
    assert_eq!(texts_of(&marked, ClaimClass::Date), vec!["2026"]);
}

#[test]
fn short_quotes_are_ignored_long_ones_kept() {
    let claims = extract_claims("set \"id\" and \"a longer quoted span\" here");
    assert_eq!(
        texts_of(&claims, ClaimClass::Quoted),
        vec!["a longer quoted span"]
    );
}

#[test]
fn quotes_pair_up_instead_of_overlapping() {
    let claims = extract_claims("\"first long span\" then \"second long span\"");
    assert_eq!(texts_of(&claims, ClaimClass::Quoted).len(), 2);
}

// ---------------------------------------------------------------------------
// Anchoring policy
// ---------------------------------------------------------------------------

#[test]
fn only_hard_classes_may_anchor_a_finding() {
    for class in [
        ClaimClass::Number,
        ClaimClass::Date,
        ClaimClass::Url,
        ClaimClass::Path,
        ClaimClass::Version,
    ] {
        let claim = Claim {
            text: "x".into(),
            class,
            value: None,
        };
        assert!(claim.can_anchor_finding(), "{class:?} should anchor");
    }

    for class in [ClaimClass::Quoted, ClaimClass::Identifier] {
        let claim = Claim {
            text: "x".into(),
            class,
            value: None,
        };
        assert!(
            !claim.can_anchor_finding(),
            "{class:?} is reworded too easily to accuse on"
        );
    }
}

#[test]
fn repeated_facts_are_deduplicated_in_order() {
    let claims = extract_claims("count 242391 then again 242391 and 180500");
    assert_eq!(
        texts_of(&claims, ClaimClass::Number),
        vec!["242391", "180500"]
    );
}

#[test]
fn values_embedded_in_key_equals_value_are_extracted() {
    let claims = extract_claims("stargazers_count=242391 forks_count=15672");
    let numbers = texts_of(&claims, ClaimClass::Number);
    assert!(
        numbers.contains(&"242391".to_string()) && numbers.contains(&"15672".to_string()),
        "tool output reports facts this way; got {numbers:?}"
    );
}

#[test]
fn url_query_strings_are_not_split_on_equals() {
    let claims = extract_claims("see https://a.test/search?q=242391&page=2");
    assert_eq!(
        texts_of(&claims, ClaimClass::Url),
        vec!["https://a.test/search?q=242391&page=2"]
    );
}

#[test]
fn a_non_url_http_prefixed_token_yields_its_number() {
    // `http_code=242391` is a key=value fact token (curl -w style), not a
    // URL: the structural split must expose the value, or a fabricated
    // figure in such a token escapes the grounding check entirely. The URL
    // protection stays: a scheme-prefixed URL is still not split.
    let numbers = texts_of(&extract_claims("http_code=242391"), ClaimClass::Number);
    assert!(
        numbers.contains(&"242391".to_string()),
        "a non-URL http-prefixed fact token must give up its value; got {numbers:?}"
    );

    let claims = extract_claims("see https://a.test/search?q=242391&page=2");
    assert_eq!(
        texts_of(&claims, ClaimClass::Url),
        vec!["https://a.test/search?q=242391&page=2"],
        "the URL guard must keep sparing real URLs"
    );
    assert!(texts_of(&claims, ClaimClass::Number).is_empty());
}

#[test]
fn non_ascii_prose_does_not_panic() {
    let claims = extract_claims("这是一段中文说明，包含数字 242391 和路径 /tmp/输出.log");
    assert!(!claims.is_empty());
}

#[test]
fn numbers_inside_serialized_json_are_visible() {
    // Exact shape of a real web_fetch result: the payload is a JSON string
    // inside a JSON string, so the escaped newline rides along with the value.
    let raw = "\\\"size\\\": 6356126,\\n  \\\"stargazers_count\\\": 244618,\\n";
    let numbers = texts_of(&extract_claims(raw), ClaimClass::Number);
    assert!(
        numbers.contains(&"244618".to_string()),
        "a figure the tool returned must be visible as evidence; got {numbers:?}"
    );
}

#[test]
fn comma_grouped_claim_matches_its_json_source() {
    // The agent restates it as 244,618; the tool returned 244618 inside JSON.
    let claim = extract_claims("stargazers_count 值为 244,618。")
        .into_iter()
        .find(|c| c.class == ClaimClass::Number)
        .expect("the restated figure is a claim");
    let evidence = extract_claims("\\\"stargazers_count\\\": 244618,\\n")
        .into_iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect::<Vec<_>>();
    assert!(
        evidence
            .iter()
            .any(|v| numbers_agree(claim.value.unwrap_or_default(), *v)),
        "claim {:?} should match one of {evidence:?}",
        claim.value
    );
}

#[test]
fn urls_survive_structural_splitting() {
    let claims = extract_claims("参见 https://api.github.com/repos/torvalds/linux");
    assert_eq!(
        texts_of(&claims, ClaimClass::Url),
        vec!["https://api.github.com/repos/torvalds/linux"]
    );
}

#[test]
fn thousands_separator_does_not_spawn_fragment_claims() {
    // Splitting `244,618` at its separator would invent a claim of "244" that no
    // evidence can support, and the report would accuse the agent of it.
    let numbers = texts_of(
        &extract_claims("stargazers_count 值为 244,618。"),
        ClaimClass::Number,
    );
    assert_eq!(
        numbers,
        vec!["244,618"],
        "a grouped number is one fact, not three"
    );
}

#[test]
fn compact_bracketed_comma_list_yields_individual_numbers() {
    assert_eq!(
        number_values("status codes [200,404,500]"),
        vec![200.0, 404.0, 500.0]
    );
    assert_eq!(
        number_values("measurements [200,404.5,1.2e3]"),
        vec![200.0, 404.5, 1200.0]
    );
    assert_eq!(number_values("CI/CD[200,404]"), vec![200.0, 404.0]);
}

#[test]
fn spaced_bracketed_comma_list_keeps_the_first_number() {
    assert_eq!(
        number_values("status codes [200, 404, 500]"),
        vec![200.0, 404.0, 500.0]
    );
}

#[test]
fn compact_bracketed_comma_list_tolerates_sentence_punctuation() {
    for text in [
        "status codes [200,404,500].",
        "status codes [200,404,500]!",
        "status codes [200,404,500]?",
        "状态码 [200,404,500]。",
    ] {
        assert_eq!(number_values(text), vec![200.0, 404.0, 500.0], "{text}");
    }
}

#[test]
fn bracketed_comma_list_allows_mixed_spacing() {
    for text in ["status codes [200,404, 500]", "status codes [200, 404,500]"] {
        assert_eq!(number_values(text), vec![200.0, 404.0, 500.0], "{text}");
    }
}

#[test]
fn bracketed_comma_list_survives_decoration_and_adjacent_prose() {
    for text in [
        "`[200,404,500]`",
        "**[200,404,500]**",
        "([200,404,500])",
        "[200,404,500];",
        "状态码[200,404,500]。",
    ] {
        assert_eq!(number_values(text), vec![200.0, 404.0, 500.0], "{text}");
    }
}

#[test]
fn bracketed_comma_normalization_preserves_path_claims() {
    for path in ["/tmp/a[b,c].txt", "/tmp/a[200,404].txt"] {
        assert_eq!(
            texts_of(&extract_claims(path), ClaimClass::Path),
            vec![path]
        );
    }
}

#[test]
fn outer_numeric_list_preserves_a_nested_path() {
    for (text, expected_numbers) in [
        ("[100, /tmp/a[200,404].txt]", vec![100.0]),
        ("[100,/tmp/a[200,404].txt,500]", vec![100.0, 500.0]),
        ("x=[100,/tmp/a[200,404].txt,500]", vec![100.0, 500.0]),
        ("x=`[100,/tmp/a[200,404].txt,500]`", vec![100.0, 500.0]),
        ("x=([100,/tmp/a[200,404].txt,500])", vec![100.0, 500.0]),
        ("x=**[100,/tmp/a[200,404].txt,500]**", vec![100.0, 500.0]),
    ] {
        let claims = extract_claims(text);
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec!["/tmp/a[200,404].txt"],
            "{text}"
        );
        assert_eq!(number_values(text), expected_numbers, "{text}");
    }
}

#[test]
fn path_list_item_does_not_hide_later_numeric_items() {
    for text in ["[/tmp/a.txt, 200,404]", "[/tmp/a.txt,200,404]"] {
        assert_eq!(
            texts_of(&extract_claims(text), ClaimClass::Path),
            vec!["/tmp/a.txt"],
            "{text}"
        );
        assert_eq!(number_values(text), vec![200.0, 404.0], "{text}");
    }
}

#[test]
fn path_first_mixed_lists_keep_following_items_separate() {
    for path in ["/tmp/a.txt", "/tmp/a[200].txt", "/tmp/a[200,404].txt"] {
        let text = format!("x=[{path},n=500]");
        let claims = extract_claims(&text);
        assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path], "{text}");
        assert_eq!(number_values(&text), vec![500.0], "{text}");

        let text = format!("x=[{path},v1.2.3]");
        let claims = extract_claims(&text);
        assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path], "{text}");
        assert_eq!(texts_of(&claims, ClaimClass::Version), vec!["v1.2.3"]);

        let text = format!("x=[{path},a/b/c]");
        let claims = extract_claims(&text);
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec![path.to_string(), "a/b/c".to_string()],
            "{text}"
        );
    }
}

#[test]
fn mixed_lists_tolerate_quoted_and_empty_items() {
    for path in ["/tmp/a.txt", "/tmp/a[200,404].txt"] {
        for text in [
            format!("x=[{path},\"500\"]"),
            format!("x=[{path},,500]"),
            format!("x=[\"{path}\",500]"),
        ] {
            let claims = extract_claims(&text);
            assert!(
                claims.iter().any(|claim| claim.text == path),
                "{text}: {claims:?}"
            );
            assert_eq!(number_values(&text), vec![500.0], "{text}: {claims:?}");
        }
    }
}

#[test]
fn mixed_lists_preserve_commas_inside_path_items() {
    for (text, path, expected_numbers) in [
        (
            "x=[100,/tmp/a,b.txt,500]",
            "/tmp/a,b.txt",
            vec![100.0, 500.0],
        ),
        (
            "x=[100,/tmp/a,b[200,404].txt,500]",
            "/tmp/a,b[200,404].txt",
            vec![100.0, 500.0],
        ),
        ("x=[/tmp/a,b.txt,500]", "/tmp/a,b.txt", vec![500.0]),
        ("x=[/tmp/a,b.txt, 500]", "/tmp/a,b.txt", vec![500.0]),
        (
            "x=[100,/tmp/a，b.txt,500]",
            "/tmp/a，b.txt",
            vec![100.0, 500.0],
        ),
    ] {
        let claims = extract_claims(text);
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec![path],
            "{text}: {claims:?}"
        );
        assert_eq!(number_values(text), expected_numbers, "{text}: {claims:?}");
    }
}

#[test]
fn mixed_lists_preserve_multiple_path_commas_and_consecutive_paths() {
    let text = "x=[100,/tmp/a,b,c.txt,500]";
    let claims = extract_claims(text);
    assert_eq!(
        texts_of(&claims, ClaimClass::Path),
        vec!["/tmp/a,b,c.txt"],
        "{claims:?}"
    );
    assert_eq!(number_values(text), vec![100.0, 500.0], "{claims:?}");

    let text = "x=[/tmp/a,b.txt,/tmp/c.txt,500]";
    let claims = extract_claims(text);
    assert_eq!(
        texts_of(&claims, ClaimClass::Path),
        vec!["/tmp/a,b.txt", "/tmp/c.txt"],
        "{claims:?}"
    );
    assert_eq!(number_values(text), vec![500.0], "{claims:?}");
}

#[test]
fn bracketed_url_commas_remain_url_content() {
    let text = "[https://example.test/search?q=200,404]";
    let claims = extract_claims(text);
    assert_eq!(
        texts_of(&claims, ClaimClass::Url),
        vec!["https://example.test/search?q=200,404"]
    );
    assert!(number_values(text).is_empty(), "{claims:?}");
}

#[test]
fn compact_list_url_commas_do_not_become_numeric_claims() {
    let text = "[100,https://example.test/search?q=200,404, 500]";
    let claims = extract_claims(text);
    assert_eq!(
        texts_of(&claims, ClaimClass::Url),
        vec!["https://example.test/search?q=200,404"]
    );
    assert_eq!(number_values(text), vec![100.0, 500.0], "{claims:?}");

    let followed_by_quoted = "[100,https://example.test,\"404\",500]";
    assert_eq!(number_values(followed_by_quoted), vec![100.0, 404.0, 500.0]);
}

#[test]
fn markdown_link_labels_keep_grouped_numbers_whole() {
    for text in [
        "[244,781 stars](https://example.test)",
        r#"{"text":"[244,781 stars](https://example.test)"}"#,
        "[244,781](https://example.test)",
        r#"{"text":"[244,781](https://example.test)"}"#,
    ] {
        assert_eq!(number_values(text), vec![244_781.0], "{text}");
    }
}

#[test]
fn quoted_grouped_numbers_in_inline_arrays_stay_whole() {
    assert_eq!(number_values(r#"result ["244,781"]"#), vec![244_781.0]);
    assert_eq!(number_values(r#"result ["244，781"]"#), vec![244_781.0]);
    for text in [
        "result ['x,244,781,y']",
        r#"{"text":"result ['x,244,781,y']"}"#,
    ] {
        assert_eq!(number_values(text), vec![244_781.0], "{text}");
    }
}

#[test]
fn compact_list_does_not_truncate_short_nested_paths() {
    let unquoted = extract_claims("[100,/a[200,404].txt,500]");
    assert_eq!(
        texts_of(&unquoted, ClaimClass::Path),
        vec!["/a[200,404].txt"]
    );
    assert_eq!(
        number_values("[100,/a[200,404].txt,500]"),
        vec![100.0, 500.0]
    );

    let quoted_text = r#"[200,"/a[200,404].txt",500]"#;
    let quoted = extract_claims(quoted_text);
    assert_eq!(
        texts_of(&quoted, ClaimClass::Quoted),
        vec!["/a[200,404].txt"]
    );
    assert!(
        quoted
            .iter()
            .all(|claim| claim.text != "/a[200" && claim.text != ".txt"),
        "{quoted:?}"
    );
}

#[test]
fn path_ownership_covers_every_bracketed_segment() {
    let path = "/a[200,404]x[500,600].txt";
    let claims = extract_claims(path);
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path]);
    assert!(number_values(path).is_empty(), "{claims:?}");

    let list = r#"[100,"/a[200,404]x[500,600].txt",700]"#;
    let claims = extract_claims(list);
    assert_eq!(
        texts_of(&claims, ClaimClass::Quoted),
        vec!["/a[200,404]x[500,600].txt"]
    );
    assert_eq!(number_values(list), vec![100.0, 700.0], "{claims:?}");
}

#[test]
fn attached_path_suffixes_after_numeric_brackets_stay_whole() {
    for (text, path) in [
        ("path=/tmp/a;[200,404].txt", "/tmp/a;[200,404].txt"),
        ("path=/tmp/a;[200,404]/b", "/tmp/a;[200,404]/b"),
    ] {
        let claims = extract_claims(text);
        assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path], "{text}");
        assert!(number_values(text).is_empty(), "{text}: {claims:?}");
    }
}

#[test]
fn root_serialized_json_uses_typed_numeric_replay() {
    let text = r#""{\"counts\":[200,404]}""#;
    assert_eq!(number_values(text), vec![200.0, 404.0]);
}

#[test]
fn quoted_closing_brackets_do_not_close_the_outer_list() {
    assert_eq!(number_values(r#"result ["]",200,404]"#), vec![200.0, 404.0]);
}

#[test]
fn compact_mixed_lists_keep_numbers_adjacent_to_versions() {
    assert_eq!(number_values("[200,v1.2.3,500]"), vec![200.0, 500.0]);
}

#[test]
fn key_prefixed_urls_do_not_degroup_query_commas() {
    let text = "[100,url=https://example.test/?q=200,404, 500]";
    assert_eq!(number_values(text), vec![100.0, 500.0]);
}

#[test]
fn key_prefixed_short_paths_own_their_brackets() {
    let text = "path=/a[200,404]";
    let claims = extract_claims(text);
    assert_eq!(
        texts_of(&claims, ClaimClass::Path),
        vec![text.trim_start_matches("path=")]
    );
    assert!(number_values(text).is_empty(), "{claims:?}");
}

#[test]
fn attached_paths_do_not_swallow_following_fields() {
    let claims = extract_claims("path=/tmp,count=500");
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec!["/tmp"]);
    assert_eq!(number_values("path=/tmp,count=500"), vec![500.0]);
}

#[test]
fn attached_bracketed_paths_keep_their_commas_when_fields_follow() {
    let text = "path=/a[200,404],count=500";
    let claims = extract_claims(text);
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec!["/a[200,404]"]);
    assert_eq!(number_values(text), vec![500.0]);
}

#[test]
fn attached_dates_do_not_swallow_following_fields() {
    for separator in [',', ';', '|'] {
        let text = format!("date=2024-01-01{separator}count=500");
        let claims = extract_claims(&text);
        assert_eq!(
            texts_of(&claims, ClaimClass::Date),
            vec!["2024-01-01"],
            "{text}"
        );
        assert_eq!(number_values(&text), vec![500.0], "{text}");
    }
}

#[test]
fn attached_values_trim_sentence_decoration() {
    for text in ["path=/tmp.", "path=/tmp,"] {
        let claims = extract_claims(text);
        assert_eq!(texts_of(&claims, ClaimClass::Path), vec!["/tmp"]);
    }
    let claims = extract_claims("date=2024-01-01.");
    assert_eq!(texts_of(&claims, ClaimClass::Date), vec!["2024-01-01"]);
}

#[test]
fn attached_relative_paths_keep_leading_dots() {
    for (text, expected) in [
        ("path=./report.txt", "./report.txt"),
        ("path=../report.txt", "../report.txt"),
    ] {
        let claims = extract_claims(text);
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec![expected],
            "{text}"
        );
    }
}

#[test]
fn attached_field_boundaries_do_not_depend_on_claim_values() {
    let text = "path=/tmp,count=42";
    let claims = extract_claims(text);
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec!["/tmp"]);
    assert!(number_values(text).is_empty(), "{claims:?}");

    let text = "date=2024-01-01,url=https://x";
    let claims = extract_claims(text);
    assert_eq!(texts_of(&claims, ClaimClass::Date), vec!["2024-01-01"]);
    assert_eq!(texts_of(&claims, ClaimClass::Url), vec!["https://x"]);
}

#[test]
fn standalone_relative_paths_keep_field_like_punctuation() {
    for path in [
        "a,b/c=d.txt",
        "a;b/c=d.txt",
        "a|b/c=d.txt",
        "a，b/c=d.txt",
        "a,b/c/d",
    ] {
        let claims = extract_claims(path);
        assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path], "{path}");
    }
}

#[test]
fn standalone_relative_paths_do_not_drop_equals_prefixed_segments() {
    for path in ["a=b/c=d.txt", "a=b/c=d/e"] {
        let claims = extract_claims(path);
        assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path], "{path}");
    }
}

#[test]
fn standalone_bracketed_paths_keep_semicolons_and_pipes() {
    for separator in [';', '|'] {
        let path = format!("/tmp/a[100,200]{separator}x[300,400].txt");
        let claims = extract_claims(&path);
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec![path.clone()],
            "{path}"
        );
        assert!(number_values(&path).is_empty(), "{path}: {claims:?}");
    }
}

#[test]
fn standalone_paths_keep_equals_before_numeric_brackets() {
    let path = "/tmp/a=b[200,404].txt";
    let claims = extract_claims(path);
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path]);
    assert!(number_values(path).is_empty(), "{claims:?}");
}

#[test]
fn undecorated_urls_keep_trailing_asterisks() {
    let url = "https://example.test/x*";
    let claims = extract_claims(url);
    assert_eq!(texts_of(&claims, ClaimClass::Url), vec![url]);
}

#[test]
fn list_residue_before_a_path_is_not_path_content() {
    for text in ["[100,200]/tmp/a", "x=[100,200]/tmp/a"] {
        let claims = extract_claims(text);
        assert_eq!(number_values(text), vec![100.0, 200.0], "{text}");
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec!["/tmp/a"],
            "{text}: {claims:?}"
        );
    }
}

#[test]
fn empty_list_decoration_does_not_glue_a_later_key_to_a_path() {
    for text in [
        "key=[]path=/tmp/a",
        "key=`[]`path=/tmp/a",
        "key=([])path=/tmp/a",
        "key=\"[]\"path=/tmp/a",
        "key=**[]**path=/tmp/a",
        "key=[[]]path=/tmp/a.txt",
        r"key=\[\]path=/tmp/a.txt",
    ] {
        let claims = extract_claims(text);
        assert_eq!(
            texts_of(&claims, ClaimClass::Path),
            vec![if text.contains(".txt") {
                "/tmp/a.txt"
            } else {
                "/tmp/a"
            }],
            "{text}: {claims:?}"
        );
    }
}

#[test]
fn empty_list_decoration_preserves_relative_path_equals() {
    let claims = extract_claims("key=[]a=b/c=d.txt");
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec!["a=b/c=d.txt"]);
}

#[test]
fn explicit_path_keys_drop_the_prefix_before_bare_relative_paths() {
    let claims = extract_claims("path=a/b=c.txt");
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec!["a/b=c.txt"]);
}

#[test]
fn root_json_string_preserves_identifier_claims() {
    let claims = extract_claims(r#""Foo::bar""#);
    assert_eq!(
        texts_of(&claims, ClaimClass::Identifier),
        vec!["Foo::bar"],
        "{claims:?}"
    );
}

#[test]
fn root_deep_containers_complete_on_a_small_stack() {
    let text = format!("{}200,404{}", "[".repeat(64), "]".repeat(64));
    std::thread::Builder::new()
        .stack_size(32 * 1024)
        .spawn(move || {
            let claims = extract_claims(&text);
            let numbers: Vec<_> = claims
                .iter()
                .filter(|claim| claim.class == ClaimClass::Number)
                .filter_map(|claim| claim.value)
                .collect();
            assert_eq!(numbers, vec![200.0, 404.0], "{claims:?}");
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn serialized_containers_match_value_replay() {
    for text in [
        r#"{"404":[200,{"500":"119,120","path":"/tmp/a.txt"}],"active":true}"#,
        r#"{"404":200,"404":600,"next":"[700,800]"}"#,
        r#"[[200],{"path":"/tmp/a,b.txt","number":1.2e3},null]"#,
    ] {
        let value = serde_json::from_str(text).unwrap();
        let expected = json_typed_claims(value);
        let actual = json_typed_claims(serde_json::Value::String(text.to_string()));
        assert_eq!(actual, expected, "{text}");
    }
}

#[test]
fn first_url_determines_attached_url_end() {
    let text = "u=(http://a),n=500,v=https://b";
    let claims = extract_claims(text);
    let urls = texts_of(&claims, ClaimClass::Url);
    assert!(urls.contains(&"http://a".to_string()), "{claims:?}");
    assert!(urls.contains(&"https://b".to_string()), "{claims:?}");
    assert_eq!(number_values(text), vec![500.0], "{claims:?}");
}

#[test]
fn balanced_url_parentheses_are_url_content() {
    let url = "https://a/b(c)";
    let claims = extract_claims(url);
    assert_eq!(texts_of(&claims, ClaimClass::Url), vec![url]);
}

#[test]
fn standalone_paths_keep_terminal_brackets() {
    let path = "/tmp/a[200]";
    let claims = extract_claims(path);
    assert_eq!(texts_of(&claims, ClaimClass::Path), vec![path]);
}

#[test]
fn brackets_prefer_list_structure_over_ambiguous_digit_grouping() {
    for text in ["total 244,781", "star 数**：244,781"] {
        assert_eq!(
            number_values(text),
            vec![244_781.0],
            "unbracketed grouping must survive decoration: {text}"
        );
    }

    // `[244,781]` can be either one grouped number or two list items. Brackets
    // tip the balance toward list structure so compact lists cannot collapse
    // into a fabricated larger claim.
    assert_eq!(number_values("totals [244,781]"), vec![244.0, 781.0]);
}

#[test]
fn json_glued_value_is_still_recovered() {
    let numbers = texts_of(
        &extract_claims("\\\"stargazers_count\\\": 244618,\\n"),
        ClaimClass::Number,
    );
    assert!(
        numbers.contains(&"244618".to_string()),
        "a value glued to JSON structure must still surface; got {numbers:?}"
    );
}

#[test]
fn sqlite_pipe_delimited_output_yields_its_numbers() {
    // Verbatim shape from a live capture: `SELECT status, COUNT(*) … GROUP BY`
    // returns pipe-delimited columns. The number was invisible as evidence, so
    // the agent correctly reporting 119 was accused of inventing it.
    let claims = extract_claims("complete|119\ninterrupted|3");
    let numbers: Vec<f64> = claims
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    assert!(
        numbers.contains(&119.0),
        "sqlite's default column separator must not hide a count: {claims:?}"
    );

    // The guard against splitting a thousands separator still holds.
    let intact = extract_claims("244,618");
    let values: Vec<f64> = intact
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    assert!(
        values.contains(&244_618.0) && !values.contains(&244.0),
        "a grouped number must stay whole: {intact:?}"
    );
}

#[test]
fn a_markdown_link_is_not_one_glued_url() {
    // Verbatim shape from a live capture. Scanning to end-of-word produced
    // `…/linux](https://…/linux`, which no evidence could match, so a round that
    // had fetched the page successfully was accused of fabricating the link.
    let claims = extract_claims(
        "参见 [https://api.github.com/repos/torvalds/linux](https://api.github.com/repos/torvalds/linux) 的结果",
    );
    let urls: Vec<&str> = claims
        .iter()
        .filter(|c| c.class == ClaimClass::Url)
        .map(|c| c.text.as_str())
        .collect();
    assert!(
        urls.contains(&"https://api.github.com/repos/torvalds/linux"),
        "the real URL must be extracted: {urls:?}"
    );
    assert!(
        !urls.iter().any(|u| u.contains("](")),
        "markdown glue must not survive into a claim: {urls:?}"
    );
}

#[test]
fn a_slash_inside_prose_is_not_a_path_claim() {
    // `CI/CD`, `3/4` and `MB/s` are prose, not file paths, but a bare
    // `contains('/')` classified them as Path — a hard class that anchors a
    // finding, so an answer that merely mentions one could be reported as
    // fabricated.
    for text in [
        "构建 CI/CD 流水线",
        "耗时 3/4 秒",
        "带宽 12 MB/s",
        "输入/输出",
        "and/or",
    ] {
        let claims = extract_claims(text);
        assert!(
            !claims.iter().any(|c| c.class == ClaimClass::Path),
            "{text} must not produce a path claim: {claims:?}"
        );
    }
}

#[test]
fn real_paths_are_still_path_claims() {
    for (text, expected) in [
        (
            "patched /etc/agentsight/config.json",
            "/etc/agentsight/config.json",
        ),
        // Edge trimming drops the leading `.` of `./x`, so the claim text is
        // the trimmed token; a single slash plus a file name stays a path.
        ("cat scripts/run.sh", "scripts/run.sh"),
        ("see src/grounding/claims.rs", "src/grounding/claims.rs"),
        ("edit config/agentsight.toml", "config/agentsight.toml"),
    ] {
        let claims = extract_claims(text);
        assert!(
            texts_of(&claims, ClaimClass::Path).contains(&expected.to_string()),
            "{text} must still report {expected}: {claims:?}"
        );
    }
}

#[test]
fn a_number_in_chinese_prose_is_still_a_claim() {
    // Verbatim from a live capture of a fabricated answer. Chinese prose puts no
    // spaces around punctuation, so `8081。其配置文件名为` is one whitespace-
    // delimited word: edge-trimming cannot reach the number and no ASCII
    // separator appears, so the invented port went unchecked while only the
    // path in the same sentence was caught.
    let claims = extract_claims(
        "trace 子命令默认监听的端口通常是 8081。其配置文件名为 agentsight.conf，默认位于 /etc/agentsight/ 目录下。",
    );
    let numbers: Vec<f64> = claims
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    assert!(
        numbers.contains(&8081.0),
        "a fabricated port stated in Chinese must be checkable: {claims:?}"
    );
    assert!(
        claims.iter().any(|c| c.class == ClaimClass::Path),
        "the path must still be found: {claims:?}"
    );
}

#[test]
fn a_fullwidth_grouped_number_stays_whole() {
    // An agent writing Chinese produced `244，618`. Because that did not parse as
    // a number it failed the stands-alone guard, so the CJK separator split
    // invented a claim of 244 that no evidence supports — the same fragment
    // already reported once for the ASCII form.
    assert_eq!(parse_number("244，618"), Some(244_618.0));

    let claims = extract_claims("总共有 244，618 条记录。");
    let values: Vec<f64> = claims
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    assert!(
        values.contains(&244_618.0),
        "the whole number must be the claim: {claims:?}"
    );
    assert!(
        !values.contains(&244.0),
        "a digit group must not become its own claim: {claims:?}"
    );
}

#[test]
fn a_grouped_number_survives_a_decorated_token() {
    // Verbatim from a live capture: `star 数**：244,781`. The stands-alone guard
    // spares only a token that is entirely a number, so this one failed on its
    // prefix and then broke at the thousands separator, inventing a "244" that
    // became a fabrication finding against a round that had answered correctly.
    let claims = extract_claims("**GitHub 仓库 torvalds/linux 的 star 数**：244,781");
    let values: Vec<f64> = claims
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    assert!(
        values.contains(&244_781.0),
        "the grouped number must survive: {claims:?}"
    );
    assert!(
        !values.contains(&244.0),
        "a digit group must not become its own claim: {claims:?}"
    );

    // A comma between JSON fields is real structure and must still separate, so
    // a value glued to the next key is still exposed.
    let json = extract_claims("{\\\"total\\\":244618,\\\"errors\\\":312}");
    let json_values: Vec<f64> = json
        .iter()
        .filter(|c| c.class == ClaimClass::Number)
        .filter_map(|c| c.value)
        .collect();
    assert!(
        json_values.contains(&244_618.0) && json_values.contains(&312.0),
        "structural commas must still split: {json:?}"
    );
}

#[test]
fn json_arrays_preserve_individual_numeric_facts() {
    for text in [
        r#"[119,120]"#,
        r#"[119, 120]"#,
        r#"{"counts":[119,120]}"#,
        r#"{"nested":[[119],{"count":120}]}"#,
    ] {
        let values: Vec<_> = extract_claims(text)
            .iter()
            .filter_map(|c| c.value)
            .collect();
        assert_eq!(values, vec![119.0, 120.0], "{text}");
    }
    let values: Vec<_> = extract_claims("[119.5,-240,1.2e3,true,null]")
        .iter()
        .filter_map(|c| c.value)
        .collect();
    assert_eq!(values, vec![119.5, -240.0, 1200.0]);
    assert!(extract_claims("[1,2]").is_empty());
}

#[test]
fn json_strings_keep_human_numeric_grouping() {
    for text in ["Recorded count: 119,120", r#"{"count":"119,120"}"#] {
        let values: Vec<_> = extract_claims(text)
            .iter()
            .filter_map(|c| c.value)
            .collect();
        assert_eq!(values, vec![119120.0], "{text}");
    }
    let claims = extract_claims(
        r#"{"count": 119, "path": "/tmp/report.txt", "version": "1.2.3", "url": "https://example.test/api"}"#,
    );
    for class in [
        ClaimClass::Number,
        ClaimClass::Path,
        ClaimClass::Version,
        ClaimClass::Url,
    ] {
        assert!(claims.iter().any(|c| c.class == class), "{class:?}");
    }
}

#[test]
fn json_object_keys_keep_their_numeric_facts() {
    // A payload keyed by a number, as a status-code histogram is, carries that
    // number exactly as an array element does. The typed replay that replaces
    // the text pass's numbers must read keys as well as values, or a figure the
    // payload did hold disappears from the claims and from the evidence pool.
    for (text, expected) in [
        (r#"{"404":119,"500":120}"#, vec![404.0, 500.0, 119.0, 120.0]),
        (r#"{"counts":{"404":119}}"#, vec![404.0, 119.0]),
    ] {
        let values: Vec<_> = extract_claims(text)
            .iter()
            .filter_map(|c| c.value)
            .collect();
        assert_eq!(values, expected, "{text}");
    }
}
