//! Cost-based grouping of mandatory context windows within one original hunk.
use std::cmp::Reverse;
use std::ops::{Add, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Metrics {
    pub(super) chars: usize,
    // The unrounded numerator of heuristic-v1; no tokenizer is needed.
    pub(super) weight: usize,
}

impl Metrics {
    pub(super) fn measure(text: &str) -> Self {
        text.chars().fold(Self::default(), |mut cost, c| {
            cost.chars += 1;
            cost.weight += if is_cjk(c) { 4 } else { 1 };
            cost
        })
    }

    pub(super) fn no_larger_than(self, baseline: Self) -> bool {
        self.chars <= baseline.chars && self.weight <= baseline.weight
    }
}

impl Add for Metrics {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self {
            chars: self.chars + other.chars,
            weight: self.weight + other.weight,
        }
    }
}

impl Sub for Metrics {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self {
            chars: self.chars - other.chars,
            weight: self.weight - other.weight,
        }
    }
}

pub(super) fn prefix_costs(lines: &[&str], units: &[(usize, usize)]) -> Vec<Metrics> {
    let mut prefix = Vec::with_capacity(units.len() + 1);
    let mut total = Metrics::default();
    prefix.push(total);
    for &(a, b) in units {
        for line in &lines[a..b] {
            total = total + Metrics::measure(line);
        }
        prefix.push(total);
    }
    prefix
}

// Kept identical to protocol's private classifier; parity is checked below.
fn is_cjk(character: char) -> bool {
    matches!(character,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{20000}'..='\u{2A6DF}'
        | '\u{2A700}'..='\u{2B73F}'
        | '\u{2B740}'..='\u{2B81F}'
        | '\u{2B820}'..='\u{2CEAF}'
        | '\u{2CEB0}'..='\u{2EBEF}'
        | '\u{30000}'..='\u{3134F}'
        | '\u{3100}'..='\u{312F}'
        | '\u{AC00}'..='\u{D7AF}'
        | '\u{3040}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
    )
}

fn range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}

fn range_len(start: usize, count: usize) -> usize {
    digits(start) + if count == 1 { 0 } else { 1 + digits(count) }
}

fn digits(n: usize) -> usize {
    if n == 0 { 1 } else { n.ilog10() as usize + 1 }
}

// Weight, reversed retained units, hunk count, and earliest starting window.
type StartKey = (usize, Reverse<usize>, usize, usize);
const NO_START: StartKey = (usize::MAX, Reverse(0), usize::MAX, usize::MAX);

// Each DP state is inserted once; contiguous ranges are queried many times.
struct RangeMin {
    size: usize,
    tree: Vec<StartKey>,
}

impl RangeMin {
    fn new(len: usize) -> Self {
        let size = len.next_power_of_two();
        Self {
            size,
            tree: vec![NO_START; size * 2],
        }
    }

    fn insert(&mut self, index: usize, key: StartKey) {
        let mut node = self.size + index;
        self.tree[node] = key;
        while node > 1 {
            node /= 2;
            self.tree[node] = self.tree[node * 2].min(self.tree[node * 2 + 1]);
        }
    }

    fn minimum(&self, begin: usize, end: usize) -> StartKey {
        let mut left = self.size + begin;
        let mut right = self.size + end;
        let mut best = NO_START;
        while left < right {
            if left % 2 == 1 {
                best = best.min(self.tree[left]);
                left += 1;
            }
            if right % 2 == 1 {
                right -= 1;
                best = best.min(self.tree[right]);
            }
            left /= 2;
            right /= 2;
        }
        best
    }
}

// Counts shrink as starting windows advance. Within each of 0, 1, 2..9,
// 10..99, ... the count contributes the same number of header characters.
fn count_class_min(count: usize) -> usize {
    match count {
        0 | 1 => count,
        2..=9 => 2,
        _ => 10usize.pow(count.ilog10()),
    }
}

pub(super) fn optimize(
    lines: &[&str],
    units: &[(usize, usize)],
    keep: &[bool],
    coordinates: [usize; 4],
    suffix: &str,
    ending: &str,
    costs: &[Metrics],
) -> (String, Metrics) {
    let [old_start, old_count, new_start, new_count] = coordinates;
    let mut old = vec![old_start + usize::from(old_count == 0)];
    let mut new = vec![new_start + usize::from(new_count == 0)];
    // Each prefix vector starts with one value and only grows.
    for &(a, _) in units {
        old.push(old.last().unwrap() + usize::from(lines[a].starts_with([' ', '-'])));
        new.push(new.last().unwrap() + usize::from(lines[a].starts_with([' ', '+'])));
    }
    let mut windows = Vec::new();
    let mut i = 0;
    while i < keep.len() {
        if !keep[i] {
            i += 1;
            continue;
        }
        let a = i;
        while i < keep.len() && keep[i] {
            i += 1;
        }
        windows.push((a, i));
    }
    // (weight, retained units, emitted hunks). Changed units are invariant,
    // so maximizing retained units on ties maximizes retained context.
    let mut best = vec![(0usize, 0usize, 0usize); windows.len() + 1];
    let mut previous = vec![0usize; windows.len() + 1];
    let fixed_header = Metrics {
        chars: 9,
        weight: 9,
    } + Metrics::measure(suffix)
        + Metrics::measure(ending);
    let mut starts = RangeMin::new(windows.len());
    // At most twice the number of decimal count classes per end: O(w D log w)
    // grouping work, where D is bounded by the number of digits in usize.
    for end in 1..=windows.len() {
        let b = windows[end - 1].1;
        let a = windows[end - 1].0;
        // Remove end-dependent terms from the comparison key. Constant
        // offsets keep both subtractions unsigned; they do not affect order.
        starts.insert(
            end - 1,
            (
                best[end - 1].0
                    + (costs[units.len()].weight - costs[a].weight)
                    + digits(old[a])
                    + digits(new[a]),
                Reverse(best[end - 1].1 + (units.len() - a)),
                best[end - 1].2,
                end - 1,
            ),
        );
        let mut choice = None;
        let mut first = 0;
        while first < end {
            let a = windows[first].0;
            let old_limit = old[b] - count_class_min(old[b] - old[a]);
            let new_limit = new[b] - count_class_min(new[b] - new[a]);
            let last = windows[..end]
                .partition_point(|&(a, _)| old[a] <= old_limit)
                .min(windows[..end].partition_point(|&(a, _)| new[a] <= new_limit));
            // Header count widths are constant on [first, last). Zero-count
            // start adjustment is also constant: every old/new start then
            // equals its end position. One range minimum replaces scanning
            // all starts in this class, preserving every tie-breaker.
            let begin = starts.minimum(first, last).3;
            let a = windows[begin].0;
            let oc = old[b] - old[a];
            let nc = new[b] - new[a];
            let os = old[a] - usize::from(oc == 0);
            let ns = new[a] - usize::from(nc == 0);
            let header_weight = fixed_header.weight + range_len(os, oc) + range_len(ns, nc);
            let candidate = (
                best[begin].0 + header_weight + costs[b].weight - costs[a].weight,
                best[begin].1 + b - a,
                best[begin].2 + 1,
            );
            let key = (candidate.0, Reverse(candidate.1), candidate.2);
            if choice.is_none_or(|(old_key, _)| key < old_key) {
                choice = Some((key, begin));
                best[end] = candidate;
                previous[end] = begin;
            }
            first = last;
        }
    }
    let mut groups = Vec::new();
    let mut end = windows.len();
    while end > 0 {
        let begin = previous[end];
        groups.push((windows[begin].0, windows[end - 1].1));
        end = begin;
    }
    let mut output = String::new();
    let mut output_cost = Metrics::default();
    for &(a, b) in groups.iter().rev() {
        let oc = old[b] - old[a];
        let nc = new[b] - new[a];
        let os = old[a] - usize::from(oc == 0);
        let ns = new[a] - usize::from(nc == 0);
        output.push_str(&format!(
            "@@ -{} +{} @@{suffix}{ending}",
            range(os, oc),
            range(ns, nc)
        ));
        output.extend(lines[units[a].0..units[b - 1].1].iter().copied());
        let ranges = range_len(os, oc) + range_len(ns, nc);
        output_cost = output_cost
            + fixed_header
            + Metrics {
                chars: ranges,
                weight: ranges,
            }
            + (costs[b] - costs[a]);
    }
    debug_assert_eq!(output_cost.weight, best[windows.len()].0);
    (output, output_cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weight(text: &str) -> usize {
        Metrics::measure(text).weight
    }

    fn optimize(
        lines: &[&str],
        units: &[(usize, usize)],
        keep: &[bool],
        coordinates: [usize; 4],
        suffix: &str,
        ending: &str,
    ) -> String {
        let costs = prefix_costs(lines, units);
        let (output, measured) =
            super::optimize(lines, units, keep, coordinates, suffix, ending, &costs);
        assert_eq!(measured, Metrics::measure(&output));
        output
    }

    #[test]
    fn weight_matches_protocol_for_every_unicode_scalar() {
        // Four identical scalars remove protocol's ASCII rounding ambiguity.
        for value in 0..=0x10ffff {
            if let Some(c) = char::from_u32(value) {
                let text: String = [c; 4].iter().collect();
                assert_eq!(
                    weight(&text),
                    4 * tokenless_protocol::estimate_tokens(&text)
                );
            }
        }
    }

    #[test]
    fn compact_ranges_preserve_zero_and_decimal_boundaries() {
        for start in [0, 1, 9, 10, 99, 100, 999, 1000] {
            for count in [0, 1, 2, 9, 10, 99, 100] {
                assert_eq!(range_len(start, count), range(start, count).len());
            }
        }
        assert_eq!(range(0, 0), "0,0");
        assert_eq!(range(12, 1), "12");
    }

    fn verify_partitions(
        gaps: &[String],
        changes: &[&str],
        start: usize,
        suffix: &str,
        ending: &str,
    ) -> String {
        // Independent exhaustive oracle: construct every partition as text,
        // without using optimizer coordinates, prefix sums, or range lengths.
        let mut owned = Vec::new();
        let mut keep = Vec::new();
        let mut positions = Vec::new();
        for block in 0..=gaps.len() {
            let begin = owned.len();
            for sign in changes[block % changes.len()].chars() {
                owned.push(format!("{sign}change{block}{ending}"));
                keep.push(true);
            }
            positions.push((begin, owned.len()));
            if let Some(gap) = gaps.get(block) {
                owned.push(format!(" {gap}{ending}"));
                keep.push(false);
            }
        }
        let lines: Vec<_> = owned.iter().map(String::as_str).collect();
        let units: Vec<_> = (0..lines.len()).map(|i| (i, i + 1)).collect();
        let old_count = lines.iter().filter(|l| !l.starts_with('+')).count();
        let new_count = lines.iter().filter(|l| !l.starts_with('-')).count();
        let actual = optimize(
            &lines,
            &units,
            &keep,
            [
                start - usize::from(old_count == 0),
                old_count,
                start - usize::from(new_count == 0),
                new_count,
            ],
            suffix,
            ending,
        );
        let mut expected = None;
        for mask in 0..1usize << gaps.len() {
            let mut text = String::new();
            let mut first = 0;
            let mut retained = 0;
            let mut groups = 0;
            for last in 0..=gaps.len() {
                if last < gaps.len() && mask & (1 << last) == 0 {
                    continue;
                }
                let a = positions[first].0;
                let b = positions[last].1;
                let old_start = start + lines[..a].iter().filter(|l| !l.starts_with('+')).count();
                let new_start = start + lines[..a].iter().filter(|l| !l.starts_with('-')).count();
                let old_count = lines[a..b].iter().filter(|l| !l.starts_with('+')).count();
                let new_count = lines[a..b].iter().filter(|l| !l.starts_with('-')).count();
                let show = |pos, n| {
                    if n == 1 {
                        format!("{pos}")
                    } else {
                        format!("{pos},{n}")
                    }
                };
                text.push_str(&format!(
                    "@@ -{} +{} @@{suffix}{ending}",
                    show(old_start - usize::from(old_count == 0), old_count),
                    show(new_start - usize::from(new_count == 0), new_count)
                ));
                text.extend(lines[a..b].iter().copied());
                retained += b - a;
                groups += 1;
                first = last + 1;
            }
            let key = (weight(&text), Reverse(retained), groups);
            if expected.as_ref().is_none_or(|(old_key, _)| key < *old_key) {
                expected = Some((key, text));
            }
        }
        let (key, text) = expected.unwrap();
        let actual_key = (
            weight(&actual),
            Reverse(actual.lines().filter(|l| !l.starts_with("@@")).count()),
            actual.lines().filter(|l| l.starts_with("@@")).count(),
        );
        assert_eq!(actual_key, key, "{text:?} vs {actual:?}");
        actual
    }

    #[test]
    fn grouping_matches_exhaustive_partition_oracle() {
        for start in [1, 8, 98, 998] {
            for ending in ["\n", "\r\n"] {
                for suffix in ["", " function", " 中文函数"] {
                    for seed in 0..16usize {
                        let gaps: Vec<_> = (0..4)
                            .map(|i| {
                                if seed & (1 << i) == 0 {
                                    "x".into()
                                } else {
                                    "中".repeat(50)
                                }
                            })
                            .collect();
                        for changes in [&["-+"][..], &["+"], &["-"], &["+", "-", "-+", "++"]] {
                            verify_partitions(&gaps, changes, start, suffix, ending);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn short_gaps_merge_and_expensive_gaps_split() {
        assert_eq!(
            verify_partitions(&["x".into()], &["-+"], 1, "", "\n")
                .matches("@@ -")
                .count(),
            1
        );
        assert_eq!(
            verify_partitions(&["x".repeat(200)], &["-+"], 1, "", "\n")
                .matches("@@ -")
                .count(),
            2
        );
        // At start=1 a 6-character payload makes merged and split weight equal.
        let out = verify_partitions(&["x".repeat(6)], &["-+"], 1, "", "\n");
        assert_eq!(out.matches("@@ -").count(), 1);
    }

    #[test]
    fn unequal_counts_cross_decimal_classes() {
        let gaps = ["x".into(), "中".repeat(50), "x".repeat(200)];
        for count in [9, 10, 99, 100, 999, 1000] {
            let inserted = "+".repeat(count);
            let deleted = "-".repeat(count + 1);
            for start in [8, 98, 998] {
                verify_partitions(
                    &gaps,
                    &[&inserted, &deleted, "-+"],
                    start,
                    " function",
                    "\r\n",
                );
            }
        }
    }

    #[test]
    fn many_windows_retain_every_change() {
        let count = 50_001;
        let gap = format!(" {}\n", "x".repeat(80));
        let mut lines = Vec::with_capacity(count * 3);
        let mut keep = Vec::with_capacity(count * 3);
        for _ in 0..count {
            lines.extend(["-old\n", "+new\n", gap.as_str()]);
            keep.extend([true, true, false]);
        }
        let units: Vec<_> = (0..lines.len()).map(|i| (i, i + 1)).collect();
        let output = optimize(
            &lines,
            &units,
            &keep,
            [1, count * 2, 1, count * 2],
            "",
            "\n",
        );
        assert_eq!(output.matches("@@ -").count(), count);
        assert_eq!(output.matches("\n-old\n+new\n").count(), count);
        assert!(!output.contains(gap.as_str()));
    }

    #[test]
    fn zero_length_sides_are_repositioned() {
        for (line, coordinates, expected) in [
            ("+added\n", [0, 0, 1, 1], "@@ -0,0 +1 @@\n+added\n"),
            ("-removed\n", [1, 1, 0, 0], "@@ -1 +0,0 @@\n-removed\n"),
        ] {
            assert_eq!(
                optimize(&[line], &[(0, 1)], &[true], coordinates, "", "\n"),
                expected
            );
        }
    }
}
