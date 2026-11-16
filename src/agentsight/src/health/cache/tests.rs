use super::*;

const SECOND: i64 = 1_000_000_000;

fn call(session: &str, index: i64, input: i64, read: Option<i64>) -> CacheCallRow {
    CacheCallRow {
        id: index,
        call_id: Some(format!("{session}-{index}")),
        trace_id: Some(format!("trace-{session}-{index}")),
        session_id: Some(session.to_string()),
        agent_name: Some("Profiling".to_string()),
        provider: Some("openai".to_string()),
        model: Some("qwen3.8-max".to_string()),
        start_ns: index * 10 * SECOND,
        end_ns: Some(index * 10 * SECOND + SECOND),
        input_tokens: Some(input),
        cache_read_tokens: read,
        cache_creation_tokens: Some(0),
        output_tokens: Some(10),
        ttft_ms: Some(100.0),
        system_fingerprint: Some(1),
        tools_fingerprint: Some(2),
        message_count: Some(index + 2),
        ..CacheCallRow::default()
    }
}

fn run(rows: Vec<CacheCallRow>) -> CacheDiagnosis {
    let window = CacheCallWindow {
        rows,
        truncated: false,
    };
    diagnose(window, 0, i64::MAX, &CacheDiagnosisOptions::default())
}

fn reasons(group: &CacheGroupDiagnosis) -> Vec<CacheReason> {
    group
        .findings
        .iter()
        .map(|finding| finding.reason)
        .collect()
}

#[test]
fn separates_conservative_and_complete_rates() {
    let diagnosis = run(vec![
        call("s", 0, 1000, Some(0)),
        call("s", 1, 2000, Some(1000)),
        call("s", 2, 1000, None),
    ]);
    let group = &diagnosis.groups[0];
    assert_eq!(group.coverage.usage_calls, 3);
    assert_eq!(group.coverage.cache_read_field_calls, 2);
    assert_eq!(group.coverage.explicit_zero_read_calls, 1);
    assert_eq!(group.conservative_hit_rate, Some(0.25));
    assert!((group.complete_hit_rate.unwrap() - 1.0 / 3.0).abs() < 1e-12);
    assert_eq!(group.status, CacheStatus::InsufficientSamples);
    assert!(reasons(group).contains(&CacheReason::CacheFieldsMissing));
}

#[test]
fn missing_counters_are_not_reported_as_misses() {
    let no_read = run(vec![call("s", 0, 1000, None), call("s", 1, 2000, None)]);
    assert_eq!(no_read.groups[0].status, CacheStatus::Undeterminable);
    assert_eq!(no_read.groups[0].conservative_hit_rate, None);

    let mut no_usage = call("s", 0, 0, None);
    no_usage.input_tokens = None;
    let diagnosis = run(vec![no_usage]);
    assert_eq!(diagnosis.groups[0].status, CacheStatus::NoUsage);
    assert!(diagnosis.groups[0].findings.is_empty());
}

#[test]
fn growth_after_full_reuse_is_expected() {
    // 28,872 of the previous 28,878 tokens were read; the low ratio is new input.
    let diagnosis = run(vec![
        call("s", 0, 28_878, Some(28_000)),
        call("s", 1, 58_566, Some(28_872)),
    ]);
    let finding = &diagnosis.groups[0].findings[0];
    assert_eq!(finding.reason, CacheReason::NewInputDominant);
    assert_eq!(finding.kind, FindingKind::Expected);
    assert_eq!(finding.missed_tokens, 0);
}

#[test]
fn fixed_prefix_reads_while_history_grows_are_suspected() {
    let mut rows = vec![call("s", 0, 14_000, Some(0))];
    rows.extend((1..30).map(|index| call("s", index, 14_000 + index * 10_000, Some(13_828))));
    let diagnosis = run(rows);
    let group = &diagnosis.groups[0];
    assert_eq!(group.status, CacheStatus::NeedsReview);
    let top = &group.findings[0];
    assert_eq!(top.reason, CacheReason::FixedPrefixPlateau);
    assert_eq!(top.kind, FindingKind::Suspected);
    assert!(top.missed_tokens > 0);
    assert_eq!(top.examples.len(), 3);
    // The largest miss comes first so the example is the most expensive call.
    assert_eq!(top.examples[0].call_id.as_deref(), Some("s-29"));
}

#[test]
fn attributes_prefix_breaks_to_the_changed_part() {
    let base = || call("s", 0, 20_000, Some(19_000));
    let next = || call("s", 1, 21_000, Some(0));
    let cases: [(fn(&mut CacheCallRow), CacheReason); 5] = [
        (
            |row| row.model = Some("glm-5".into()),
            CacheReason::ModelSwitched,
        ),
        (
            |row| row.start_ns += 3600 * SECOND,
            CacheReason::CacheExpired,
        ),
        (
            |row| row.system_fingerprint = Some(9),
            CacheReason::SystemPromptChanged,
        ),
        (
            |row| row.tools_fingerprint = Some(9),
            CacheReason::ToolsChanged,
        ),
        (
            |row| row.message_count = Some(1),
            CacheReason::HistoryRewritten,
        ),
    ];
    for (change, expected) in cases {
        let mut row = next();
        change(&mut row);
        let diagnosis = run(vec![base(), row]);
        let found: Vec<CacheReason> = diagnosis
            .groups
            .iter()
            .flat_map(|group| group.findings.iter().map(|finding| finding.reason))
            .collect();
        assert!(found.contains(&expected), "{expected:?} not in {found:?}");
    }
    let unexplained = run(vec![base(), next()]);
    assert_eq!(
        reasons(&unexplained.groups[0]),
        vec![CacheReason::PrefixNotReused]
    );
}

#[test]
fn first_calls_are_cold_and_sessionless_calls_are_unattributed() {
    let mut sessionless = call("x", 1, 5000, Some(0));
    sessionless.session_id = None;
    let diagnosis = run(vec![call("s", 0, 5000, Some(0)), sessionless]);
    let found = reasons(&diagnosis.groups[0]);
    assert!(found.contains(&CacheReason::ColdStart));
    assert!(found.contains(&CacheReason::NoSessionContext));
}

#[test]
fn drops_repeated_captures_of_one_call() {
    let first = call("s", 0, 1000, Some(500));
    let mut retry = first.clone();
    retry.id = 99;
    let diagnosis = run(vec![first, retry]);
    let group = &diagnosis.groups[0];
    assert_eq!(group.coverage.calls, 1);
    assert_eq!(group.coverage.duplicate_calls, 1);
    assert_eq!(group.tokens.input, 1000);
}

#[test]
fn groups_case_insensitively_and_splits_failures() {
    let mut upper = call("s", 1, 1000, Some(0));
    upper.agent_name = Some("PROFILING".into());
    upper.failed = true;
    let diagnosis = run(vec![call("s", 0, 1000, Some(900)), upper]);
    assert_eq!(diagnosis.groups.len(), 1);
    assert_eq!(diagnosis.groups[0].coverage.failed_calls, 1);
    assert!(reasons(&diagnosis.groups[0]).contains(&CacheReason::FailedCalls));
}

/// Six provider counters from a before/after replay of one tool history; the
/// expected costs follow `(U*12 + R*1 + W*15) / 1e6` with `U = I - R - W`.
fn replay(arm: &str) -> Vec<CacheCallRow> {
    let counts = match arm {
        "before" => [(10_410, 0, 2255), (11_322, 2255, 0), (12_234, 2255, 0)],
        _ => [
            (10_410, 0, 10_402),
            (11_322, 10_402, 912),
            (12_234, 11_314, 912),
        ],
    };
    counts
        .iter()
        .enumerate()
        .map(|(index, (input, read, write))| {
            let mut row = call(arm, index as i64, *input, Some(*read));
            row.cache_creation_tokens = Some(*write);
            row
        })
        .collect()
}

fn priced(rows: Vec<CacheCallRow>) -> CacheDiagnosis {
    let options = CacheDiagnosisOptions {
        prices: Some(CachePrices {
            input: 12.0,
            cache_read: 1.0,
            cache_write: 15.0,
            output: 36.0,
        }),
        ..CacheDiagnosisOptions::default()
    };
    let window = CacheCallWindow {
        rows,
        truncated: false,
    };
    diagnose(window, 0, i64::MAX, &options)
}

#[test]
fn input_cost_counts_each_token_once() {
    let before = priced(replay("before"));
    let after = priced(replay("after"));
    let before_cost = before.groups[0].cost.as_ref().unwrap();
    let after_cost = after.groups[0].cost.as_ref().unwrap();
    assert!((before_cost.input_cost - 0.364747).abs() < 1e-9);
    assert!((after_cost.input_cost - 0.205394).abs() < 1e-9);
    let reduction = 1.0 - after_cost.input_cost / before_cost.input_cost;
    assert!((reduction - 0.4369).abs() < 1e-4);
    assert!((before_cost.output_cost - 3.0 * 10.0 * 36.0 / 1e6).abs() < 1e-12);
    assert_eq!(before_cost.incompatible_calls, 0);
}

#[test]
fn incompatible_counters_are_excluded_from_cost() {
    let mut row = call("s", 0, 1000, Some(900));
    row.cache_creation_tokens = Some(500);
    let diagnosis = priced(vec![row]);
    let cost = diagnosis.groups[0].cost.as_ref().unwrap();
    assert_eq!(cost.incompatible_calls, 1);
    assert_eq!(cost.input_cost, 0.0);
    assert_eq!(cost.input_cost_per_mtok, None);
}

fn sampled(session: &str, read_share: f64) -> Vec<CacheCallRow> {
    (0..25)
        .map(|index| {
            let input = 20_000;
            call(
                session,
                index,
                input,
                Some((input as f64 * read_share) as i64),
            )
        })
        .collect()
}

#[test]
fn baseline_comparison_reports_direction_and_flags_large_drops() {
    let mut improved = run(sampled("after", 0.9));
    compare(&mut improved, &run(sampled("before", 0.2)));
    let comparison = improved.groups[0].baseline.as_ref().unwrap();
    assert_eq!(comparison.trend, CacheTrend::Improved);
    assert!(comparison.sufficient_samples);
    assert!((comparison.hit_rate_change_pp.unwrap() - 70.0).abs() < 1e-9);
    assert_eq!(improved.groups[0].status, CacheStatus::Healthy);

    let mut regressed = run(sampled("now", 0.82));
    assert_eq!(regressed.groups[0].status, CacheStatus::Healthy);
    compare(&mut regressed, &run(sampled("then", 0.99)));
    assert_eq!(regressed.groups[0].status, CacheStatus::NeedsReview);
    let comparison = regressed.groups[0].baseline.as_ref().unwrap();
    assert_eq!(comparison.trend, CacheTrend::Regressed);

    let mut incomparable = run(sampled("now", 0.7));
    compare(&mut incomparable, &run(vec![call("then", 0, 1000, None)]));
    let comparison = incomparable.groups[0].baseline.as_ref().unwrap();
    assert_eq!(comparison.trend, CacheTrend::Incomparable);
    assert!(!comparison.sufficient_samples);
}

#[test]
fn latency_is_split_by_hit_ratio() {
    let mut warm = call("s", 1, 10_000, Some(9_000));
    warm.ttft_ms = Some(200.0);
    let mut cold = call("s", 0, 10_000, Some(0));
    cold.ttft_ms = Some(900.0);
    let diagnosis = run(vec![cold, warm]);
    let latency = &diagnosis.groups[0].latency;
    assert_eq!(latency.warm.calls, 1);
    assert_eq!(latency.cold.calls, 1);
    assert_eq!(latency.warm.ttft_ms.as_ref().unwrap().p50, 200.0);
    assert_eq!(latency.cold.ttft_ms.as_ref().unwrap().p50, 900.0);
    assert_eq!(latency.cold.median_input_tokens, Some(10_000));
}

#[test]
fn large_suspected_misses_need_review_above_the_rate_threshold() {
    // Every call reads the same 18,000-token prefix while its previous input
    // was over 30,000 tokens: the rate clears 50%, yet over a third of the
    // input was reusable and missed.
    let mut rows = vec![call("s", 0, 30_000, Some(0))];
    rows.extend((1..25).map(|index| call("s", index, 30_000 + index * 100, Some(18_000))));
    let diagnosis = run(rows);
    let group = &diagnosis.groups[0];
    assert!(group.conservative_hit_rate.unwrap() > 0.5);
    let suspected: i64 = group
        .findings
        .iter()
        .filter(|finding| finding.kind == FindingKind::Suspected)
        .map(|finding| finding.missed_tokens)
        .sum();
    assert!(suspected as f64 >= SUSPECTED_MISS_SHARE * group.tokens.input as f64);
    assert_eq!(group.status, CacheStatus::NeedsReview);
}
