//! Cost-aware context cropping for Git diffs; metadata and changed lines stay verbatim.
use regex::Regex;

mod cost;

const BASELINE_NOTICE: &str =
    "[Diff context reduced to 2 lines; all changes retained. Retrieve original for full context.]";
const OPTIMIZED_NOTICE: &str =
    "[Diff context reduced; all changes retained. Retrieve original for full context.]";

pub(super) fn render(input: &str) -> Option<String> {
    render_impl(input, true)
}

fn same_file_paths(header: &str, old: &str, new: &str) -> bool {
    // Callers have checked the --- / +++ prefixes. Git appends a tab to
    // unquoted paths containing spaces; spaces themselves belong to the path.
    let old = old[4..].trim_end_matches(['\r', '\n']);
    let new = new[4..].trim_end_matches(['\r', '\n']);
    let old = old.strip_suffix('\t').unwrap_or(old);
    let new = new.strip_suffix('\t').unwrap_or(new);
    // Using the complete metadata paths avoids ambiguous spaces or " b/"
    // inside a filename. Preserve all original headers when rendering.
    if header.trim_end_matches(['\r', '\n']) != format!("diff --git {old} {new}") {
        return false;
    }
    let (Some(old), Some(new)) = (decode_git_path(old), decode_git_path(new)) else {
        return false;
    };
    match (old.strip_prefix(b"a/"), new.strip_prefix(b"b/")) {
        (Some(old), Some(new)) => !old.is_empty() && old == new,
        _ => false,
    }
}

fn decode_git_path(path: &str) -> Option<Vec<u8>> {
    let quoted = path.starts_with('"');
    let path = if quoted {
        path.strip_prefix('"')?.strip_suffix('"')?
    } else {
        path
    };
    let mut bytes = path.bytes();
    let mut decoded = Vec::with_capacity(path.len());
    while let Some(byte) = bytes.next() {
        let byte = match byte {
            b'\\' if quoted => match bytes.next()? {
                b'"' => b'"',
                b'\\' => b'\\',
                b'a' => 7,
                b'b' => 8,
                b't' => b'\t',
                b'n' => b'\n',
                b'v' => 11,
                b'f' => 12,
                b'r' => b'\r',
                first @ b'0'..=b'3' => {
                    let second = bytes.next()?;
                    let third = bytes.next()?;
                    if !(b'0'..=b'7').contains(&second) || !(b'0'..=b'7').contains(&third) {
                        return None;
                    }
                    (first - b'0') * 64 + (second - b'0') * 8 + (third - b'0')
                }
                _ => return None,
            },
            b'"' | b'\\' => return None,
            byte if byte.is_ascii_control() => return None,
            byte => byte,
        };
        if byte == 0 {
            return None;
        }
        decoded.push(byte);
    }
    Some(decoded)
}

fn render_impl(input: &str, optimize: bool) -> Option<String> {
    if !input.starts_with("diff --git ") {
        return None;
    }
    let lines: Vec<_> = input.split_inclusive('\n').collect();
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| l.starts_with("diff --git ").then_some(i))
        .collect();
    // The regular expression is a compile-time constant with valid syntax.
    let header = Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$").unwrap();
    // Build both views from the same parse. Shared metadata and untouched
    // hunks cancel in the final comparison; measure only differing parts.
    let mut baseline = String::new();
    let mut output = String::new();
    let mut baseline_cost = cost::Metrics::measure(BASELINE_NOTICE);
    let mut output_cost = cost::Metrics::measure(OPTIMIZED_NOTICE);
    let mut baseline_changed = false;
    let mut changed = false;
    for (section, &start) in starts.iter().enumerate() {
        let end = starts.get(section + 1).copied().unwrap_or(lines.len());
        let first = (start + 1..end)
            .find(|&i| lines[i].starts_with("@@ "))
            .unwrap_or(end);
        let meta = &lines[start + 1..first];
        // Encoded binary patches are not parsed; preserve the received output.
        if meta.iter().any(|l| l.trim_end() == "GIT binary patch") {
            return None;
        }
        if meta.last().is_some_and(|l| l.starts_with("Binary files ")) && first == end {
            if meta[..meta.len() - 1]
                .iter()
                .any(|l| !l.starts_with("index "))
            {
                return None;
            }
            baseline.extend(lines[start..end].iter().copied());
            output.extend(lines[start..end].iter().copied());
            continue;
        }
        if meta.iter().any(|line| {
            ![
                "index ",
                "--- ",
                "+++ ",
                "old mode ",
                "new mode ",
                "new file mode ",
                "deleted file mode ",
                "similarity index ",
                "dissimilarity index ",
                "rename from ",
                "rename to ",
                "copy from ",
                "copy to ",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        }) {
            return None;
        }
        let old: Vec<_> = meta.iter().filter(|l| l.starts_with("--- ")).collect();
        let new: Vec<_> = meta.iter().filter(|l| l.starts_with("+++ ")).collect();
        if first == end {
            let has = |p: &str| meta.iter().any(|l| l.starts_with(p));
            if !old.is_empty()
                || !new.is_empty()
                || !(has("new file mode ")
                    || has("deleted file mode ")
                    || (has("old mode ") && has("new mode "))
                    || (has("rename from ") && has("rename to "))
                    || (has("copy from ") && has("copy to ")))
            {
                return None;
            }
            baseline.extend(lines[start..end].iter().copied());
            output.extend(lines[start..end].iter().copied());
            continue;
        }
        if old.len() != 1 || new.len() != 1 {
            return None;
        }
        let special = meta.iter().any(|l| {
            ["new file mode ", "deleted file mode ", "rename ", "copy "]
                .iter()
                .any(|p| l.starts_with(p))
        });
        let eligible = !special && same_file_paths(lines[start], old[0], new[0]);
        baseline.extend(lines[start..first].iter().copied());
        output.extend(lines[start..first].iter().copied());
        let mut cursor = first;
        while cursor < end {
            let begin = cursor;
            let h = lines[cursor];
            let c = header.captures(h.trim_end_matches(['\r', '\n']))?;
            let old_start = c[1].parse::<usize>().ok()?;
            let new_start = c[3].parse::<usize>().ok()?;
            let old_count = c
                .get(2)
                .map_or(Some(1), |m| m.as_str().parse::<usize>().ok())?;
            let new_count = c
                .get(4)
                .map_or(Some(1), |m| m.as_str().parse::<usize>().ok())?;
            // Store each physical content line with its optional EOF marker.
            let mut units: Vec<(usize, usize)> = Vec::new();
            cursor += 1;
            while cursor < end && !lines[cursor].starts_with("@@ ") {
                match lines[cursor].as_bytes().first() {
                    Some(b' ' | b'+' | b'-') => units.push((cursor, cursor + 1)),
                    Some(b'\\')
                        if lines[cursor].trim_end_matches(['\r', '\n'])
                            == "\\ No newline at end of file" =>
                    {
                        let last = units.last_mut()?;
                        if last.1 != last.0 + 1 {
                            return None;
                        }
                        last.1 = cursor + 1;
                    }
                    _ => return None,
                }
                cursor += 1;
            }
            let old_actual = units
                .iter()
                .filter(|(i, _)| lines[*i].starts_with([' ', '-']))
                .count();
            let new_actual = units
                .iter()
                .filter(|(i, _)| lines[*i].starts_with([' ', '+']))
                .count();
            if old_actual != old_count
                || new_actual != new_count
                || (old_count > 0 && old_start == 0)
                || (new_count > 0 && new_start == 0)
            {
                return None;
            }
            if !units.iter().any(|(i, _)| lines[*i].starts_with(['+', '-'])) {
                return None;
            }
            let keep: Vec<_> = (0..units.len())
                .map(|i| {
                    units[i.saturating_sub(2)..(i + 3).min(units.len())]
                        .iter()
                        .any(|(j, _)| lines[*j].starts_with(['+', '-']))
                })
                .collect();
            if !eligible || keep.iter().all(|v| *v) {
                baseline.extend(lines[begin..cursor].iter().copied());
                output.extend(lines[begin..cursor].iter().copied());
                continue;
            }
            let original: String = lines[begin..cursor].iter().copied().collect();
            let costs = cost::prefix_costs(&lines, &units);
            let original_cost = cost::Metrics::measure(h) + costs[units.len()];
            let mut legacy = String::new();
            let mut legacy_cost = cost::Metrics::default();
            // Coordinates of the next old/new line, including zero-length sides.
            let mut old_pos = old_start.checked_add(usize::from(old_count == 0))?;
            let mut new_pos = new_start.checked_add(usize::from(new_count == 0))?;
            let mut i = 0;
            while i < units.len() {
                let a = i;
                let old_begin = old_pos;
                let new_begin = new_pos;
                let retained = keep[i];
                while i < units.len() && keep[i] == retained {
                    let line = lines[units[i].0];
                    old_pos = old_pos.checked_add(usize::from(line.starts_with([' ', '-'])))?;
                    new_pos = new_pos.checked_add(usize::from(line.starts_with([' ', '+'])))?;
                    i += 1;
                }
                if retained {
                    let oc = old_pos - old_begin;
                    let nc = new_pos - new_begin;
                    let os = old_begin - usize::from(oc == 0);
                    let ns = new_begin - usize::from(nc == 0);
                    let ending = if h.ends_with("\r\n") { "\r\n" } else { "\n" };
                    let head = format!("@@ -{os},{oc} +{ns},{nc} @@{}{ending}", &c[5]);
                    legacy_cost =
                        legacy_cost + cost::Metrics::measure(&head) + (costs[i] - costs[a]);
                    legacy.push_str(&head);
                    legacy.extend(lines[units[a].0..units[i - 1].1].iter().copied());
                }
            }
            baseline.push_str(&legacy);
            baseline_cost = baseline_cost + legacy_cost;
            baseline_changed = true;
            if optimize {
                let (optimized, optimized_cost) = cost::optimize(
                    &lines,
                    &units,
                    &keep,
                    [old_start, old_count, new_start, new_count],
                    &c[5],
                    if h.ends_with("\r\n") { "\r\n" } else { "\n" },
                    &costs,
                );
                // An equal-cost raw hunk keeps the most context and its exact bytes.
                let mut selected = original.as_str();
                let mut selected_cost = original_cost;
                for (candidate, candidate_cost) in [
                    (optimized.as_str(), optimized_cost),
                    (legacy.as_str(), legacy_cost),
                ] {
                    if candidate_cost.no_larger_than(original_cost)
                        && candidate_cost.weight < selected_cost.weight
                    {
                        selected = candidate;
                        selected_cost = candidate_cost;
                    }
                }
                changed |= selected != original;
                output.push_str(selected);
                output_cost = output_cost + selected_cost;
            }
        }
    }
    if !optimize {
        return baseline_changed.then(|| format!("{BASELINE_NOTICE}\n{baseline}\n[End diff]"));
    }
    if !changed {
        return None;
    }
    if output_cost.no_larger_than(baseline_cost) {
        Some(format!("{OPTIMIZED_NOTICE}\n{output}\n[End diff]"))
    } else {
        Some(format!("{BASELINE_NOTICE}\n{baseline}\n[End diff]"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const HEAD: &str = "diff --git a/f b/f\nindex 123..456 100644\n--- a/f\n+++ b/f\n";
    #[test]
    fn recalculates_and_splits_hunks() {
        let input = format!(
            "{HEAD}@@ -1,11 +1,11 @@ function\n a\n b\n-old\n+new\n c\n d\n e\n f\n g\n-before\n+after\n h\n i\n"
        );
        let out = render_impl(&input, false).unwrap();
        assert!(out.contains(
            "@@ -1,5 +1,5 @@ function\n a\n b\n-old\n+new\n c\n d\n@@ -7,5 +7,5 @@ function\n"
        ));
        assert!(!out.contains(" e\n"));
    }
    #[test]
    fn keeps_eof_markers_crlf_and_header_like_changes() {
        let input = format!(
            "{HEAD}@@ -1,4 +1,4 @@\n a\n b\n c\n--- old\n\\ No newline at end of file\n+++ new\n\\ No newline at end of file"
        );
        let out = render_impl(&input, false).unwrap();
        assert!(out.contains(
            "--- old\n\\ No newline at end of file\n+++ new\n\\ No newline at end of file"
        ));
        assert!(out.contains("@@ -2,3 +2,3 @@"));
        let crlf = input.replace('\n', "\r\n");
        assert!(
            render_impl(&crlf, false)
                .unwrap()
                .contains("@@ -2,3 +2,3 @@\r\n b\r\n")
        );
    }
    #[test]
    fn rejects_incomplete_input_and_detached_markers() {
        for body in [
            "@@ -1,4 +1,4 @@\n a\n b\n c\n-old\n",
            "@@ -1 +1 @@\n\\ No newline at end of file\n-old\n+new\n",
            "@@ -1 +1 @@\n-old\n+new\ntruncated\n",
        ] {
            assert!(render(&format!("{HEAD}{body}")).is_none());
        }
    }
    #[test]
    fn special_and_metadata_only_sections_are_unchanged() {
        let input = format!("{HEAD}@@ -1,4 +1,4 @@\n a\n b\n c\n-old\n+new\n");
        for section in [
            "diff --git a/bin b/bin\nindex 123..456 100644\nBinary files a/bin and b/bin differ\n",
            "diff --git a/x b/x\nold mode 100644\nnew mode 100755\n",
            "diff --git a/a b/b\nsimilarity index 100%\nrename from a\nrename to b\n",
        ] {
            assert!(
                render(&format!("{input}{section}"))
                    .unwrap()
                    .strip_suffix("\n[End diff]")
                    .unwrap()
                    .ends_with(section)
            );
        }
        let special = input.replace("a/f", "i/f").replace("b/f", "w/f");
        assert!(render(&special).is_none());
    }

    #[test]
    fn decodes_git_paths_without_losing_bytes() {
        for (encoded, expected) in [
            ("a/user settings.py ", b"a/user settings.py ".as_slice()),
            ("a/中文", "a/中文".as_bytes()),
            (r#""a/\344\270\255\346\226\207""#, "a/中文".as_bytes()),
            (r#""a/\a\b\t\n\v\f\r\"\\""#, b"a/\x07\x08\t\n\x0b\x0c\r\"\\"),
            (r#""a/\377\200""#, b"a/\xff\x80"),
        ] {
            assert_eq!(decode_git_path(encoded).as_deref(), Some(expected));
        }
        for malformed in [
            r#""a/unclosed"#,
            r#""a/\q""#,
            r#""a/\12""#,
            r#""a/\400""#,
            r#""a/\089""#,
            r#""a/\000""#,
            r#""a/inner"quote""#,
            r#"a/unquoted\slash"#,
            "a/raw\tcontrol",
        ] {
            assert_eq!(decode_git_path(malformed), None, "{malformed:?}");
        }
    }

    #[test]
    fn crops_special_paths_and_preserves_headers() {
        let body = "@@ -1,4 +1,4 @@\n a\n b\n c\n-old\n+new\n";
        for (old, new, separator) in [
            ("a/user settings.py", "b/user settings.py", "\t"),
            ("a/trailing ", "b/trailing ", "\t"),
            ("a/dir b/name", "b/dir b/name", "\t"),
            ("a/中文", "b/中文", ""),
            (r#""a/f""#, r#""b/f""#, ""),
            (r#""a/\377""#, r#""b/\377""#, ""),
        ] {
            let head =
                format!("diff --git {old} {new}\n--- {old}{separator}\n+++ {new}{separator}\n");
            let out = render(&format!("{head}{body}")).unwrap();
            assert!(out.contains(&head));
            assert!(out.contains("@@ -2,3 +2,3 @@\n b\n c\n-old\n+new\n"));
        }
    }

    #[test]
    fn invalid_paths_pass_through_without_blocking_other_files() {
        let body = "@@ -1,4 +1,4 @@\n a\n b\n c\n-old\n+new\n";
        for head in [
            "diff --git a/x b/x\n--- a/x\n+++ b/y\n",
            "diff --git a/x b/y\n--- a/x\n+++ b/y\n",
            "diff --git a/x b/x extra\n--- a/x\n+++ b/x\n",
            "diff --git a/x b/x\n--- a/x\t\t\n+++ b/x\n",
            "diff --git \"a/\\q\" \"b/\\q\"\n--- \"a/\\q\"\n+++ \"b/\\q\"\n",
            "diff --git \"a/\\376\" \"b/\\377\"\n--- \"a/\\376\"\n+++ \"b/\\377\"\n",
        ] {
            let section = format!("{head}{body}");
            assert_eq!(render(&section), None);
            let out = render(&format!("{HEAD}{body}{section}")).unwrap();
            assert!(
                out.strip_suffix("\n[End diff]")
                    .unwrap()
                    .ends_with(&section)
            );
        }
    }

    #[test]
    fn local_expansion_keeps_raw_hunk_while_other_hunk_shrinks() {
        let first = "@@ -1,11 +1,11 @@ function\n a\n b\n-old\n+new\n c\n d\n e\n f\n g\n-before\n+after\n h\n i\n";
        let second = format!(
            "@@ -20,5 +20,5 @@ function\n {}\n b\n c\n d\n-old\n+new\n",
            "context".repeat(60)
        );
        let input = format!("{HEAD}{first}{second}");
        let out = render(&input).unwrap();
        assert!(out.contains(first));
        assert!(!out.contains(&"context".repeat(60)));
        let baseline = render_impl(&input, false).unwrap();
        assert!(cost::Metrics::measure(&out).no_larger_than(cost::Metrics::measure(&baseline)));
        assert!(out.contains("@@ -22,3 +22,3 @@ function"));
    }

    #[test]
    fn no_effective_hunk_change_does_not_add_wrapper() {
        let input = format!(
            "{HEAD}@@ -1,11 +1,11 @@ function\n a\n b\n-old\n+new\n c\n d\n e\n f\n g\n-before\n+after\n h\n i\n"
        );
        assert_eq!(render(&input), None);
    }

    #[test]
    fn shared_parse_keeps_the_whole_diff_character_guard() {
        let input = format!(
            "{HEAD}@@ -1,12 +1,12 @@ {}\n {}\n a\n b\n-old\n+new\n c\n d\n {}\n e\n f\n-before\n+after\n g\n h\n",
            "中文".repeat(50),
            "x".repeat(300),
            "x".repeat(150),
        );
        // Merging saves a CJK-heavy header but retains more ASCII characters.
        // The complete fixed-context view must still win the character guard.
        let baseline = render_impl(&input, false).unwrap();
        assert_eq!(render(&input), Some(baseline));
    }
}
