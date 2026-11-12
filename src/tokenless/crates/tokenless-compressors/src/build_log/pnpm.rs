//! Recognize complete pnpm records without interpreting lifecycle script output.

use std::collections::HashSet;

use super::{LayoutEdit, LineRole, RoutineFamily, saves_both};

const MAX_LINE_BYTES: usize = 4096;
const MAX_NOTICE_BYTES: usize = 8192;
const MAX_DEPENDENCY_ROWS: usize = 256;
const MAX_DEPENDENCY_BYTES: usize = 64 * 1024;
const MAX_LAYOUT_EDITS: usize = 256;

pub(super) struct Plan {
    pub roles: Vec<LineRole>,
    pub run_starts: HashSet<usize>,
    pub layouts: Vec<LayoutEdit>,
}

impl Plan {
    fn opaque(line_count: usize) -> Self {
        Self {
            roles: vec![LineRole::Unknown; line_count],
            run_starts: HashSet::new(),
            layouts: Vec::new(),
        }
    }
}

struct Progress {
    counts: [u64; 4],
    done: bool,
}

struct Dependency<'a> {
    sign: &'a str,
    name: &'a str,
    version: &'a str,
    available: Option<&'a str>,
}

pub(super) fn detect(sample: &[String]) -> bool {
    let lines = sample.iter().map(String::as_str).collect::<Vec<_>>();
    let mut progress_count = 0;
    let mut has_total = false;
    let mut has_completion = false;
    let mut has_structure = false;
    for (index, line) in lines.iter().enumerate() {
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        progress_count += usize::from(progress(line).is_some());
        has_total |= package_total(line).is_some();
        has_completion |= completion(line);
        if category(line).is_some() || compact_category(line).is_some() {
            has_structure |=
                dependencies(&lines, index, compact_category(line).is_some()).is_some();
        } else if line.trim_start_matches(' ').starts_with('╭') {
            has_structure |= notification(&lines, index).is_some();
        } else if line.starts_with("Update available! ") {
            has_structure |= lines.get(index..index + 3).and_then(notice_body).is_some();
        }
    }
    (has_completion && (progress_count > 0 || has_structure)) || (progress_count >= 3 && has_total)
}

pub(super) fn plan(lines: &[&str]) -> Plan {
    let mut plan = Plan::opaque(lines.len());
    if lines
        .iter()
        .any(|line| line.chars().any(|ch| ch.is_control() && ch != '\n'))
    {
        return plan;
    }
    let records = lines
        .iter()
        .map(|line| line.strip_suffix('\n').unwrap_or(line))
        .collect::<Vec<_>>();
    let mut index = 0;
    let mut previous_counts: Option<[u64; 4]> = None;
    while index < lines.len() {
        let line = records[index];
        if !lines[index].ends_with('\n') || line.len() > MAX_LINE_BYTES {
            break;
        }
        if let Some(record) = progress(line) {
            if record.done {
                plan.roles[index] = LineRole::Summary;
                previous_counts = None;
            } else {
                if previous_counts.is_some_and(|previous| {
                    record
                        .counts
                        .iter()
                        .zip(previous)
                        .any(|(now, old)| *now < old)
                }) {
                    plan.run_starts.insert(index);
                }
                plan.roles[index] = LineRole::Routine(RoutineFamily::PnpmProgress);
                previous_counts = Some(record.counts);
            }
            index += 1;
            continue;
        }
        previous_counts = None;
        if line.is_empty() || retained_status(line) || completion(line) {
            plan.roles[index] = LineRole::Summary;
            index += 1;
            continue;
        }
        if let Some((sign, count)) = package_total(line) {
            plan.roles[index] = LineRole::Summary;
            if let Some(bar) = records.get(index + 1)
                && lines[index + 1].ends_with('\n')
                && bar.len() <= MAX_LINE_BYTES
                && count > 0
                && usize::try_from(count).ok() == Some(bar.len())
                && bar.bytes().all(|ch| ch == sign)
                && records.get(index + 2) != Some(bar)
            {
                plan.layouts.push(LayoutEdit {
                    start: index + 1,
                    end: index + 2,
                    replacement: String::new(),
                });
                plan.roles[index + 1] = LineRole::Phase;
                index += 1;
            }
            index += 1;
        } else {
            let block = if category(line).is_some() {
                dependencies(&records, index, false)
            } else {
                notification(&records, index)
            };
            let Some((end, replacement)) = block else {
                break;
            };
            if !lines[end - 1].ends_with('\n') {
                break;
            }
            plan.roles[index..end].fill(LineRole::Phase);
            if saves_both(&lines[index..end].concat(), &replacement) {
                plan.layouts.push(LayoutEdit {
                    start: index,
                    end,
                    replacement,
                });
            }
            index = end;
        }
        if plan.layouts.len() > MAX_LAYOUT_EDITS {
            return Plan::opaque(lines.len());
        }
    }
    plan
}

fn progress(line: &str) -> Option<Progress> {
    let rest = line.strip_prefix("Progress: resolved ")?;
    let (rest, done) = rest
        .strip_suffix(", done")
        .map_or((rest, false), |rest| (rest, true));
    let (resolved, rest) = rest.split_once(", reused ")?;
    let (reused, rest) = rest.split_once(", downloaded ")?;
    let (downloaded, added) = rest.split_once(", added ")?;
    Some(Progress {
        counts: [
            count(resolved)?,
            count(reused)?,
            count(downloaded)?,
            count(added)?,
        ],
        done,
    })
}

fn count(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|ch| ch.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn package_total(line: &str) -> Option<(u8, u64)> {
    let rest = line.strip_prefix("Packages: ")?;
    let sign = *rest.as_bytes().first()?;
    if !matches!(sign, b'+' | b'-') {
        return None;
    }
    Some((sign, count(&rest[1..])?))
}

fn completion(line: &str) -> bool {
    let Some((duration, version)) = line
        .strip_prefix("Done in ")
        .and_then(|rest| rest.split_once(" using pnpm v"))
    else {
        return false;
    };
    let number = duration
        .strip_suffix("ms")
        .or_else(|| duration.strip_suffix('s'));
    number.is_some_and(|number| {
        let mut parts = number.split('.');
        parts.next().is_some_and(|part| count(part).is_some())
            && parts.next().is_none_or(|part| count(part).is_some())
            && parts.next().is_none()
    }) && valid_version(version)
}

fn retained_status(line: &str) -> bool {
    matches!(
        line,
        "Already up to date" | "Lockfile is up to date, resolution step is skipped"
    )
}

fn category(line: &str) -> Option<&str> {
    matches!(
        line,
        "dependencies:" | "devDependencies:" | "optionalDependencies:"
    )
    .then(|| line.strip_suffix(':'))
    .flatten()
}

fn compact_category(line: &str) -> Option<&str> {
    let heading = line.strip_suffix(" (version; available):")?;
    matches!(
        heading,
        "dependencies" | "devDependencies" | "optionalDependencies"
    )
    .then_some(heading)
}

fn dependency(line: &str, compact: bool) -> Option<Dependency<'_>> {
    let (sign, rest) = line.split_once(' ')?;
    if !matches!(sign, "+" | "-") {
        return None;
    }
    let (name, rest) = rest.split_once(' ')?;
    if !valid_package(name) {
        return None;
    }
    let (version, available) = if compact {
        rest.split_once("; ")
            .map_or((rest, None), |(version, available)| {
                (version, Some(available))
            })
    } else if let Some((version, annotation)) = rest.split_once(" (") {
        (version, Some(annotation.strip_suffix(" is available)")?))
    } else {
        (rest, None)
    };
    if !valid_version(version) || available.is_some_and(|value| !valid_version(value)) {
        return None;
    }
    Some(Dependency {
        sign,
        name,
        version,
        available,
    })
}

fn dependencies(lines: &[&str], start: usize, compact: bool) -> Option<(usize, String)> {
    let heading = if compact {
        compact_category(lines[start])?
    } else {
        category(lines[start])?
    };
    let mut rows = Vec::new();
    let mut bytes = lines[start].len() + 1;
    let mut end = start + 1;
    loop {
        let line = *lines.get(end)?;
        if line.is_empty()
            || category(line).is_some()
            || (compact && compact_category(line).is_some())
            || completion(line)
        {
            break;
        }
        bytes += line.len() + 1;
        if line.len() > MAX_LINE_BYTES
            || rows.len() >= MAX_DEPENDENCY_ROWS
            || bytes > MAX_DEPENDENCY_BYTES
        {
            return None;
        }
        rows.push(dependency(line, compact)?);
        end += 1;
    }
    if rows.is_empty() {
        return None;
    }
    let mut output = format!("{heading} (version; available):\n");
    for row in rows {
        output.push_str(&format!("{} {} {}", row.sign, row.name, row.version));
        if let Some(available) = row.available {
            output.push_str("; ");
            output.push_str(available);
        }
        output.push('\n');
    }
    Some((end, output))
}

fn notification(lines: &[&str], start: usize) -> Option<(usize, String)> {
    let block = lines.get(start..start + 7)?;
    if block.iter().any(|line| line.len() > MAX_LINE_BYTES)
        || block.iter().map(|line| line.len() + 1).sum::<usize>() > MAX_NOTICE_BYTES
    {
        return None;
    }
    let top = block[0].trim_start_matches(' ');
    let indent = &block[0][..block[0].len() - top.len()];
    let rule = top.strip_prefix('╭')?.strip_suffix('╮')?;
    if rule.is_empty()
        || !rule.chars().all(|ch| ch == '─')
        || block[6] != format!("{indent}╰{rule}╯")
    {
        return None;
    }
    let body = block[1..6]
        .iter()
        .map(|line| {
            line.strip_prefix(indent)?
                .strip_prefix('│')?
                .strip_suffix('│')
                .map(|body| body.trim_matches(' '))
        })
        .collect::<Option<Vec<_>>>()?;
    if !body[0].is_empty() || !body[4].is_empty() {
        return None;
    }
    Some((start + 7, notice_body(&body[1..4])?))
}

fn notice_body(body: &[&str]) -> Option<String> {
    if body.len() != 3
        || body.iter().any(|line| line.len() > MAX_LINE_BYTES)
        || body.iter().map(|line| line.len() + 1).sum::<usize>() > MAX_NOTICE_BYTES
    {
        return None;
    }
    let (current, available) = body[0]
        .strip_prefix("Update available! ")?
        .strip_suffix('.')?
        .split_once(" → ")?;
    if !valid_version(current)
        || !valid_version(available)
        || body[1].strip_prefix("Changelog: https://pnpm.io/v/") != Some(available)
        || body[2] != "To update, run: pnpm add -g pnpm"
    {
        return None;
    }
    Some(format!("{}\n{}\n{}\n", body[0], body[1], body[2]))
}

fn valid_package(name: &str) -> bool {
    if name.len() > 214 {
        return false;
    }
    let valid_part = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['.', '_'])
            && part.bytes().all(|ch| {
                ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, b'-' | b'_' | b'.')
            })
    };
    if let Some(scoped) = name.strip_prefix('@') {
        scoped
            .split_once('/')
            .is_some_and(|(scope, package)| valid_part(scope) && valid_part(package))
    } else {
        valid_part(name)
    }
}

fn valid_version(version: &str) -> bool {
    let (release, build) = version
        .split_once('+')
        .map_or((version, None), |(release, build)| (release, Some(build)));
    let (base, prerelease) = release
        .split_once('-')
        .map_or((release, None), |(base, pre)| (base, Some(pre)));
    let mut numbers = base.split('.');
    for _ in 0..3 {
        if !numbers.next().is_some_and(|number| {
            count(number).is_some() && (number.len() == 1 || !number.starts_with('0'))
        }) {
            return false;
        }
    }
    if numbers.next().is_some() {
        return false;
    }
    let valid_identifiers = |value: &str, allow_zeroes: bool| {
        value.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')
                && (allow_zeroes
                    || !part.bytes().all(|ch| ch.is_ascii_digit())
                    || part.len() == 1
                    || !part.starts_with('0'))
        })
    };
    build.is_none_or(|value| valid_identifiers(value, true))
        && prerelease.is_none_or(|value| valid_identifiers(value, false))
}
