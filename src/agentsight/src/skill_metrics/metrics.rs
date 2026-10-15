//! Skill metrics computation logic.
//!
//! Computes 9 skill-related metrics from extracted skill events.

use chrono::Datelike;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::storage::sqlite::genai::TraceEventDetail;

use super::extractor::{extract_skill_downloads, extract_skill_loads};
use super::types::*;

// --- Orchestrator ---

/// Compute skill metrics from a set of GenAI events.
///
/// Only computes metrics enabled in `options`. Events may arrive in any
/// order: `time_range_ns` and every `first_seen_*` field are derived from
/// the timestamps themselves, not from slice position, so callers do not
/// need to pre-sort.
pub fn compute_skill_metrics(
    events: &[TraceEventDetail],
    options: &MetricOptions,
) -> SkillMetricsReport {
    // Phase 1: Extract all skill events from raw data
    let extracted = ExtractedData::from_events(events);

    // Phase 2: Compute each metric based on extracted data
    let downloads = if options.downloads {
        Some(compute_downloads(&extracted))
    } else {
        None
    };

    let loads = if options.loads {
        Some(compute_loads(&extracted))
    } else {
        None
    };

    let usage_ratio = if options.usage_ratio {
        Some(compute_usage_ratio(&extracted))
    } else {
        None
    };

    let distribution = if options.distribution {
        Some(compute_distribution(&extracted))
    } else {
        None
    };

    let hotness = if options.hotness {
        Some(compute_hotness(&extracted, &options.hotness_granularity))
    } else {
        None
    };

    let time_range = if events.is_empty() {
        (0, 0)
    } else {
        // Min/max over the timestamps themselves: correct for any input
        // ordering, unlike first()/last() which assumed pre-sorted events.
        let timestamps = events.iter().map(|e| e.start_timestamp_ns);
        (
            timestamps.clone().min().unwrap_or(0),
            timestamps.max().unwrap_or(0),
        )
    };

    SkillMetricsReport {
        downloads,
        loads,
        usage_ratio,
        distribution,
        hotness,
        computed_at: chrono::Utc::now().to_rfc3339(),
        time_range_ns: time_range,
        event_count: events.len() as u64,
    }
}

// --- Extracted Data (intermediate) ---

/// Pre-extracted skill data from all events.
struct ExtractedData {
    download_records: Vec<SkillDownloadRecord>,
    load_records: Vec<SkillLoadRecord>,
    /// Set of all session_ids seen.
    all_sessions: HashSet<String>,
}

impl ExtractedData {
    fn from_events(events: &[TraceEventDetail]) -> Self {
        let mut download_records = Vec::new();
        let mut load_records = Vec::new();
        let mut all_sessions: HashSet<String> = HashSet::new();

        for event in events {
            // conversation_id groups every LLM call of one user query — the
            // task/session these metrics are documented over (call_builder
            // and the genai events writer agree). trace_id is the per-call
            // response id and only remains as a fallback for rows written
            // before the conversation column existed.
            let session_id = event
                .conversation_id
                .clone()
                .or_else(|| event.trace_id.clone())
                .unwrap_or_default();
            // Rows with neither id cannot be attributed to any session;
            // counting them as one phantom "" session skews every
            // per-session metric.
            if !session_id.is_empty() {
                all_sessions.insert(session_id);
            }

            download_records.extend(extract_skill_downloads(event));
            load_records.extend(extract_skill_loads(event));
        }

        Self {
            download_records,
            load_records,
            all_sessions,
        }
    }
}

// --- Metric 1: Skill Download Count ---

fn compute_downloads(data: &ExtractedData) -> SkillDownloadMetrics {
    let mut downloads: BTreeMap<String, SkillFirstSeen> = BTreeMap::new();

    // Track sessions per skill for total_sessions count. Records from
    // id-less rows carry an empty session id and are not real sessions.
    let mut skill_sessions: HashMap<String, HashSet<String>> = HashMap::new();

    for record in &data.download_records {
        if !record.session_id.is_empty() {
            skill_sessions
                .entry(record.skill_name.clone())
                .or_default()
                .insert(record.session_id.clone());
        }

        // Keep the earliest record as "first seen" — the input is not
        // guaranteed to be sorted, so first-insert would be whichever
        // record the caller happened to pass first.
        let entry = downloads
            .entry(record.skill_name.clone())
            .or_insert_with(|| SkillFirstSeen {
                first_seen_session_id: record.session_id.clone(),
                first_seen_timestamp_ns: record.timestamp_ns,
                total_sessions: 0,
            });
        if record.timestamp_ns < entry.first_seen_timestamp_ns {
            entry.first_seen_session_id = record.session_id.clone();
            entry.first_seen_timestamp_ns = record.timestamp_ns;
        }
    }

    // Update total_sessions counts
    for (skill, sessions) in &skill_sessions {
        if let Some(entry) = downloads.get_mut(skill) {
            entry.total_sessions = sessions.len() as u64;
        }
    }

    SkillDownloadMetrics { downloads }
}

// --- Metric 2: Skill Load Count ---

fn compute_loads(data: &ExtractedData) -> SkillLoadMetrics {
    let mut loads: BTreeMap<String, u64> = BTreeMap::new();

    for record in &data.load_records {
        *loads.entry(record.skill_name.clone()).or_default() += 1;
    }

    let total_loads = loads.values().sum();
    SkillLoadMetrics { loads, total_loads }
}

// --- Metric 3: Skill Usage Ratio ---

fn compute_usage_ratio(data: &ExtractedData) -> SkillUsageRatio {
    let total_sessions = data.all_sessions.len() as u64;
    if total_sessions == 0 {
        return SkillUsageRatio {
            ratio: 0.0,
            with_skill_count: 0,
            without_skill_count: 0,
            total_sessions: 0,
        };
    }

    // Loads from id-less rows still count toward load totals, but they
    // cannot be attributed to any real session.
    let sessions_with_skill: HashSet<&String> = data
        .load_records
        .iter()
        .map(|r| &r.session_id)
        .filter(|s| !s.is_empty())
        .collect();
    let with_skill_count = sessions_with_skill.len() as u64;
    let without_skill_count = total_sessions.saturating_sub(with_skill_count);
    let ratio = with_skill_count as f64 / total_sessions as f64;

    SkillUsageRatio {
        ratio,
        with_skill_count,
        without_skill_count,
        total_sessions,
    }
}

// --- Metric 4: Per-task Skill Count Distribution ---

fn compute_distribution(data: &ExtractedData) -> SkillCountDistribution {
    // Group loaded skills by session, counting distinct skills per session
    let mut skills_per_session: HashMap<&String, HashSet<&String>> = HashMap::new();
    for record in &data.load_records {
        skills_per_session
            .entry(&record.session_id)
            .or_default()
            .insert(&record.skill_name);
    }

    // Build count vector (including sessions with 0 skills)
    let mut counts: Vec<u32> = Vec::new();
    for session in &data.all_sessions {
        let count = skills_per_session
            .get(session)
            .map(|s| s.len() as u32)
            .unwrap_or(0);
        counts.push(count);
    }

    if counts.is_empty() {
        return SkillCountDistribution {
            min: 0,
            max: 0,
            mean: 0.0,
            median: 0.0,
            p90: 0.0,
            histogram: [0; 6],
        };
    }

    counts.sort_unstable();

    let min = *counts.first().unwrap();
    let max = *counts.last().unwrap();
    let mean = counts.iter().map(|&c| c as f64).sum::<f64>() / counts.len() as f64;
    let median = percentile(&counts, 50.0);
    let p90 = percentile(&counts, 90.0);

    // Histogram: [0, 1, 2, 3, 4, 5+]
    let mut histogram = [0u64; 6];
    for &c in &counts {
        let bucket = if c >= 5 { 5 } else { c as usize };
        histogram[bucket] += 1;
    }

    SkillCountDistribution {
        min,
        max,
        mean,
        median,
        p90,
        histogram,
    }
}

// --- Metric 5: Skill Hotness Ranking ---

fn compute_hotness(data: &ExtractedData, granularity: &HotnessGranularity) -> SkillHotnessRanking {
    // Group loads by time bucket (day or week)
    let mut bucket_counts: HashMap<String, HashMap<String, u64>> = HashMap::new();
    let mut total_counts: HashMap<String, u64> = HashMap::new();

    for record in &data.load_records {
        let bucket = match granularity {
            HotnessGranularity::Day => ns_to_date(record.timestamp_ns),
            HotnessGranularity::Week => ns_to_iso_week(record.timestamp_ns),
        };
        *bucket_counts
            .entry(bucket)
            .or_default()
            .entry(record.skill_name.clone())
            .or_default() += 1;
        *total_counts.entry(record.skill_name.clone()).or_default() += 1;
    }

    // Sort buckets chronologically
    let mut buckets: Vec<String> = bucket_counts.keys().cloned().collect();
    buckets.sort();

    // Compute per-bucket rankings
    let mut weekly_rankings: HashMap<String, Vec<WeeklyRank>> = HashMap::new();

    for bucket in &buckets {
        let counts = &bucket_counts[bucket];
        let mut sorted: Vec<(&String, &u64)> = counts.iter().collect();
        // Ties are ordered by name so the rank a skill gets no longer
        // depends on HashMap iteration order (which is randomized per
        // process): an 8-way tie produced arbitrary weekly ranks before.
        sorted.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));

        for (rank, &(skill, &count)) in sorted.iter().enumerate() {
            weekly_rankings
                .entry((*skill).clone())
                .or_default()
                .push(WeeklyRank {
                    iso_week: bucket.clone(),
                    load_count: count,
                    rank: (rank + 1) as u32,
                });
        }
    }

    // Build final rankings sorted by total loads
    let mut rankings: Vec<SkillRankEntry> = total_counts
        .iter()
        .map(|(skill, &total)| {
            let weekly = weekly_rankings.remove(skill).unwrap_or_default();
            let rank_delta = if weekly.len() >= 2 {
                let last = weekly[weekly.len() - 1].rank as i32;
                let prev = weekly[weekly.len() - 2].rank as i32;
                Some(prev - last) // positive = improved
            } else {
                None
            };
            SkillRankEntry {
                skill_name: skill.clone(),
                total_loads: total,
                total_rank: 0,
                weekly_ranks: weekly,
                rank_delta,
            }
        })
        .collect();

    // Same tiebreak as the per-bucket ranking: total_rank must be a function
    // of the event set, not of map iteration order.
    rankings.sort_by(|a, b| {
        b.total_loads
            .cmp(&a.total_loads)
            .then_with(|| a.skill_name.cmp(&b.skill_name))
    });
    for (i, entry) in rankings.iter_mut().enumerate() {
        entry.total_rank = (i + 1) as u32;
    }

    SkillHotnessRanking { rankings }
}

// --- Helper Functions ---

/// Convert nanosecond timestamp to ISO week string (e.g., "2026-W19").
fn ns_to_iso_week(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let nanos = (ns % 1_000_000_000) as u32;
    let dt = chrono::DateTime::from_timestamp(secs, nanos)
        .unwrap_or_default()
        .naive_utc();
    let iso_week = dt.iso_week();
    format!("{}-W{:02}", iso_week.year(), iso_week.week())
}

/// Convert nanosecond timestamp to date string (e.g., "2026-05-08").
fn ns_to_date(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let nanos = (ns % 1_000_000_000) as u32;
    let dt = chrono::DateTime::from_timestamp(secs, nanos)
        .unwrap_or_default()
        .naive_utc();
    format!("{}-{:02}-{:02}", dt.year(), dt.month(), dt.day())
}

/// Compute percentile from sorted slice.
fn percentile(sorted: &[u32], pct: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = (pct / 100.0) * (sorted.len() - 1) as f64;
    let lower = idx.floor() as usize;
    let upper = idx.ceil() as usize;
    if lower == upper {
        sorted[lower] as f64
    } else {
        let frac = idx - lower as f64;
        sorted[lower] as f64 * (1.0 - frac) + sorted[upper] as f64 * frac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ns_to_iso_week() {
        // 2026-05-07 is in ISO week 19
        let ns: i64 = 1_778_000_000_000_000_000;
        let week = ns_to_iso_week(ns);
        assert!(week.starts_with("2026-W"));
    }

    #[test]
    fn test_percentile_basic() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&data, 0.0), 1.0);
        assert_eq!(percentile(&data, 100.0), 10.0);
        assert!((percentile(&data, 50.0) - 5.5).abs() < 0.01);
    }

    /// The response carries `loads` and `downloads` as JSON objects, so their key
    /// order is the map's iteration order. A `HashMap`'s is arbitrary and differs
    /// between two identical requests, which makes the dashboard's tie-break
    /// order — and any diff of two responses — unstable. The CLI tables were
    /// ordered for exactly this reason; the HTTP path was not.
    #[test]
    fn the_load_map_serializes_in_a_defined_order() {
        let events: Vec<TraceEventDetail> = ["zulu", "alpha", "mike", "kilo", "yankee"]
            .iter()
            .enumerate()
            .map(|(i, skill)| {
                load_event(
                    i as i64 + 1,
                    1_778_000_000_000_000_000 + i as i64,
                    "s1",
                    skill,
                )
            })
            .collect();

        let report = compute_skill_metrics(&events, &MetricOptions::all());
        let json = serde_json::to_string(&report.loads.expect("loads")).unwrap();

        assert!(
            json.starts_with(r#"{"loads":{"alpha":1,"kilo":1,"mike":1,"yankee":1,"zulu":1}"#),
            "the load map must serialize in a defined order, got {json}"
        );
    }

    #[test]
    fn test_compute_empty_events() {
        let report = compute_skill_metrics(&[], &MetricOptions::all());
        assert_eq!(report.event_count, 0);
        assert_eq!(report.loads.unwrap().total_loads, 0);
        assert_eq!(report.usage_ratio.unwrap().total_sessions, 0);
    }

    /// Minimal event carrying one skill advertisement, at a chosen
    /// timestamp and session, for order-independence tests.
    fn skill_event(id: i64, start_ns: i64, session: &str) -> TraceEventDetail {
        use crate::genai::semantic::{InputMessage, MessagePart};
        TraceEventDetail {
            id,
            call_id: Some(format!("c{id}")),
            start_timestamp_ns: start_ns,
            end_timestamp_ns: Some(start_ns + 1000),
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: None,
            system_instructions: Some(
                serde_json::to_string(&vec![InputMessage {
                    role: "system".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "<available_skills><skill><name>test-skill</name><description>A test</description></skill></available_skills>"
                            .to_string(),
                    }],
                    name: None,
                }])
                .unwrap(),
            ),
            agent_name: Some("TestAgent".into()),
            process_name: None,
            pid: Some(100),
            user_query: None,
            event_json: None,
            trace_id: Some(format!("resp-{session}")),
            conversation_id: Some(session.into()),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        }
    }

    /// Minimal event carrying one tool call that loads a skill's SKILL.md —
    /// the load-carrying counterpart of `skill_event`, built the way
    /// `extract_skill_loads` expects (an assistant ToolCall whose arguments
    /// reference the skill's file).
    fn load_event(id: i64, start_ns: i64, session: &str, skill: &str) -> TraceEventDetail {
        use crate::genai::semantic::{MessagePart, OutputMessage};
        TraceEventDetail {
            id,
            call_id: Some(format!("l{id}")),
            start_timestamp_ns: start_ns,
            end_timestamp_ns: Some(start_ns + 1000),
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            input_messages: None,
            output_messages: Some(
                serde_json::to_string(&vec![OutputMessage {
                    role: "assistant".to_string(),
                    parts: vec![MessagePart::ToolCall {
                        id: Some(format!("tc{id}")),
                        name: "read_file".to_string(),
                        arguments: Some(serde_json::json!({
                            "file_path": format!("/skills/{skill}/SKILL.md")
                        })),
                    }],
                    name: None,
                    finish_reason: None,
                }])
                .unwrap(),
            ),
            system_instructions: None,
            agent_name: Some("TestAgent".into()),
            process_name: None,
            pid: Some(100),
            user_query: None,
            event_json: None,
            trace_id: Some(format!("resp-{session}")),
            conversation_id: Some(session.into()),
            cache_read_tokens: None,
            status: Some("complete".into()),
            interruption_type: None,
        }
    }

    /// One task (one conversation) makes several LLM calls with distinct
    /// response ids (trace_id). Only the call that reads SKILL.md uses a
    /// skill, so the task-level usage ratio is 1.0 — not 1/calls.
    #[test]
    fn one_task_with_several_calls_counts_as_one_session() {
        use crate::genai::semantic::{MessagePart, OutputMessage};

        let mut with_skill = load_event(1, 1_000_000_000, "conv-task-1", "pdf");
        with_skill.trace_id = Some("chatcmpl-call-1".into());
        with_skill.conversation_id = Some("conv-task-1".into());

        let mut without_skill = load_event(2, 1_000_000_100, "conv-task-1", "pdf");
        without_skill.trace_id = Some("chatcmpl-call-2".into());
        without_skill.conversation_id = Some("conv-task-1".into());
        // The later call's output carries no SKILL.md read.
        without_skill.output_messages = Some(
            serde_json::to_string(&vec![OutputMessage {
                role: "assistant".to_string(),
                parts: vec![MessagePart::Text {
                    content: "done".to_string(),
                }],
                name: None,
                finish_reason: Some("stop".to_string()),
            }])
            .unwrap(),
        );

        let report = compute_skill_metrics(&[with_skill, without_skill], &MetricOptions::all());
        let usage = report.usage_ratio.unwrap();
        assert_eq!(usage.total_sessions, 1);
        assert_eq!(usage.with_skill_count, 1);
        assert_eq!(usage.without_skill_count, 0);
        assert_eq!(usage.ratio, 1.0);
    }

    #[test]
    fn tied_skills_get_name_ordered_ranks_per_bucket() {
        // Eight skills, one load each in the same week: every rank used to
        // depend on HashMap iteration order (randomized per process); after
        // the name tiebreak the weekly ranks are exactly the name-sorted
        // positions.
        let skills = [
            "zeta", "alpha", "delta", "echo", "bravo", "golf", "charlie", "foxtrot",
        ];
        let events: Vec<TraceEventDetail> = skills
            .iter()
            .enumerate()
            .map(|(i, s)| load_event(i as i64, 1_000_000_000, "s1", s))
            .collect();
        let report = compute_skill_metrics(&events, &MetricOptions::all());
        let mut names: Vec<&str> = skills.to_vec();
        names.sort();
        for entry in &report.hotness.as_ref().unwrap().rankings {
            let expected = names.iter().position(|n| *n == entry.skill_name).unwrap() + 1;
            let weekly = &entry.weekly_ranks[0];
            assert_eq!(
                weekly.rank as usize, expected,
                "skill {} weekly rank must be its name-sorted position",
                entry.skill_name
            );
        }
    }

    #[test]
    fn tied_totals_get_name_ordered_total_rank() {
        let skills = ["zeta", "alpha", "delta", "echo"];
        let events: Vec<TraceEventDetail> = skills
            .iter()
            .enumerate()
            .map(|(i, s)| load_event(i as i64, 1_000_000_000, "s1", s))
            .collect();
        let report = compute_skill_metrics(&events, &MetricOptions::all());
        // rankings[] is (total_loads DESC, skill_name ASC) and total_rank is
        // 1..=4 in that order.
        let got: Vec<(&str, u32)> = report
            .hotness
            .as_ref()
            .unwrap()
            .rankings
            .iter()
            .map(|e| (e.skill_name.as_str(), e.total_rank))
            .collect();
        assert_eq!(
            got,
            vec![("alpha", 1), ("delta", 2), ("echo", 3), ("zeta", 4)]
        );
    }

    #[test]
    fn rank_delta_is_stable_under_ties() {
        // Same tied multiset in two weeks: with arbitrary tie ranks the
        // delta flipped between runs; with the name tiebreak every skill's
        // rank is identical across weeks, so every delta is Some(0).
        let skills = ["zeta", "alpha", "delta"];
        let mut events = Vec::new();
        let week1 = 1_700_000_000_000_000_000i64;
        let week2 = 1_710_000_000_000_000_000i64;
        for (i, s) in skills.iter().enumerate() {
            events.push(load_event(i as i64, week1, "s1", s));
            events.push(load_event(100 + i as i64, week2, "s2", s));
        }
        let report = compute_skill_metrics(&events, &MetricOptions::all());
        for entry in &report.hotness.as_ref().unwrap().rankings {
            assert_eq!(
                entry.rank_delta,
                Some(0),
                "skill {} delta must be 0 under identical ties",
                entry.skill_name
            );
        }
    }

    #[test]
    fn distinct_counts_keep_count_order() {
        // The tiebreak must not disturb the primary key: alpha loads 3
        // times, zeta once — alpha ranks first regardless of names.
        let events = vec![
            load_event(1, 1_000_000_000, "s1", "zeta"),
            load_event(2, 1_000_000_001, "s1", "alpha"),
            load_event(3, 1_000_000_002, "s1", "alpha"),
            load_event(4, 1_000_000_003, "s1", "alpha"),
        ];
        let report = compute_skill_metrics(&events, &MetricOptions::all());
        assert_eq!(
            report.hotness.as_ref().unwrap().rankings[0].skill_name,
            "alpha"
        );
        assert_eq!(report.hotness.as_ref().unwrap().rankings[0].total_rank, 1);
        assert_eq!(
            report.hotness.as_ref().unwrap().rankings[1].skill_name,
            "zeta"
        );
        assert_eq!(report.hotness.as_ref().unwrap().rankings[1].total_rank, 2);
    }

    #[test]
    fn compute_is_repeatable_for_same_input() {
        // The hotness contract: same event set -> same serialized ranking.
        let skills = ["b", "a", "d", "c"];
        let events: Vec<TraceEventDetail> = skills
            .iter()
            .enumerate()
            .map(|(i, s)| load_event(i as i64, 1_000_000_000, "s1", s))
            .collect();
        let r1 = compute_skill_metrics(&events, &MetricOptions::all());
        let r2 = compute_skill_metrics(&events, &MetricOptions::all());
        // Compare the hotness ranking only: computed_at is a wall-clock
        // timestamp and loads.downloads is a HashMap whose serialization
        // order is intentionally unspecified — the contract under test is
        // that the RANKING is a function of the event set.
        assert_eq!(
            serde_json::to_string(r1.hotness.as_ref().unwrap()).unwrap(),
            serde_json::to_string(r2.hotness.as_ref().unwrap()).unwrap()
        );
    }

    #[test]
    fn time_range_is_independent_of_input_order() {
        // Descending input: first()/last() would report (30_000, 10_000).
        let events = [
            skill_event(1, 30_000, "session-late"),
            skill_event(2, 10_000, "session-early"),
        ];
        let report = compute_skill_metrics(&events, &MetricOptions::all());
        assert_eq!(report.time_range_ns, (10_000, 30_000));
    }

    #[test]
    fn downloads_first_seen_uses_earliest_timestamp() {
        // The later download arrives first in the slice; "first seen" must
        // still be the earlier one.
        let events = [
            skill_event(1, 30_000, "session-late"),
            skill_event(2, 10_000, "session-early"),
        ];
        let report = compute_skill_metrics(&events, &MetricOptions::all());
        let downloads = report.downloads.unwrap();
        let seen = downloads.downloads.get("test-skill").unwrap();
        assert_eq!(seen.first_seen_timestamp_ns, 10_000);
        assert_eq!(seen.first_seen_session_id, "session-early");
    }

    #[test]
    fn id_less_events_do_not_create_a_phantom_session() {
        // Rows with neither trace_id nor conversation_id used to collapse
        // into one phantom "" session: total_sessions was inflated,
        // usage_ratio dragged down and the histogram gained a bogus [0]
        // bar. Their loads still count; they are just not attributable to
        // any session.
        let mut events: Vec<TraceEventDetail> = (0..5_i64)
            .map(|i| {
                let mut e = load_event(i, 1_000_000_000 + i, "unused", "test-skill");
                e.trace_id = None;
                e.conversation_id = None;
                e
            })
            .collect();
        events.push(load_event(9, 1_000_000_100, "s1", "test-skill"));

        let report = compute_skill_metrics(&events, &MetricOptions::all());

        // The id-less loads still count toward load totals.
        assert_eq!(report.loads.unwrap().total_loads, 6);
        // But only the attributed session exists.
        let usage = report.usage_ratio.unwrap();
        assert_eq!(usage.total_sessions, 1);
        assert_eq!(usage.with_skill_count, 1);
        assert_eq!(usage.without_skill_count, 0);
        assert_eq!(usage.ratio, 1.0);
        let dist = report.distribution.unwrap();
        assert_eq!(dist.histogram[0], 0);
        assert_eq!(dist.histogram[1], 1);
    }
}
