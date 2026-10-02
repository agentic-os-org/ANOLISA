use super::*;

fn texts_of(claims: &[Claim], class: ClaimClass) -> Vec<String> {
    claims
        .iter()
        .filter(|c| c.class == class)
        .map(|c| c.text.clone())
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
