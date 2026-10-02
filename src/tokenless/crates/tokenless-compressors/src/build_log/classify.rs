//! Dialect evidence and semantic line roles for build/test logs.

use std::collections::HashSet;

use super::template::generic_progress_template;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BuildLogFormat {
    Cargo,
    Pytest,
    Npm,
    Jest,
    Go,
    Make,
    Generic,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Evidence {
    pub(crate) strong: usize,
    pub(crate) weak: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineRole {
    Diagnostic,
    Summary,
    Phase,
    Routine(RoutineFamily),
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RoutineFamily {
    CargoCompile,
    CargoDownload,
    CargoTestPass,
    PytestPass,
    PytestProgress,
    NpmFetch,
    NpmSpinner,
    JestPass,
    GoDownload,
    GoPass,
    MakeCompile,
    Generic,
}

impl RoutineFamily {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::CargoCompile => "compilation progress",
            Self::CargoDownload => "dependency download",
            Self::CargoTestPass => "passing test",
            Self::PytestPass => "passing test",
            Self::PytestProgress => "test progress",
            Self::NpmFetch => "package fetch",
            Self::NpmSpinner => "spinner progress",
            Self::JestPass => "passing test suite",
            Self::GoDownload => "dependency download",
            Self::GoPass => "passing package",
            Self::MakeCompile => "compiler invocation",
            Self::Generic => "repeated progress",
        }
    }
}

pub(crate) fn format_evidence(format: BuildLogFormat, line: &str) -> Evidence {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (strong, weak) = match format {
        BuildLogFormat::Cargo => (
            trimmed.starts_with("error[E")
                || trimmed.starts_with("test result:")
                || (trimmed.starts_with("Finished ") && trimmed.contains(" profile [")),
            starts_any(
                trimmed,
                &[
                    "$ cargo ",
                    "Compiling ",
                    "Checking ",
                    "Downloading ",
                    "Downloaded ",
                    "Updating crates.io index",
                ],
            ),
        ),
        BuildLogFormat::Pytest => (
            trimmed.contains("test session starts")
                || trimmed.trim_matches('=').trim() == "FAILURES"
                || trimmed.contains("short test summary info")
                || is_pytest_summary(trimmed, &lower),
            trimmed.contains("::")
                && [" PASSED", " FAILED", " SKIPPED", " XFAIL", " XPASS"]
                    .iter()
                    .any(|marker| trimmed.contains(marker)),
        ),
        BuildLogFormat::Npm => (
            trimmed.starts_with("npm ERR!") || trimmed.starts_with("npm error"),
            trimmed.starts_with("$ npm ")
                || trimmed.starts_with("npm http fetch ")
                || trimmed.starts_with("npm warn "),
        ),
        BuildLogFormat::Jest => (
            starts_any(trimmed, &["Test Suites:", "Snapshots:"])
                || (trimmed.starts_with("Tests:")
                    && (lower.contains("passed") || lower.contains("failed"))),
            starts_any(trimmed, &["PASS ", "FAIL ", "RUNS "])
                || (trimmed.starts_with('✓') || trimmed.starts_with('✕')),
        ),
        BuildLogFormat::Go => (
            starts_any(trimmed, &["=== RUN", "--- PASS:", "--- FAIL:"])
                || trimmed.starts_with("FAIL\t")
                || is_go_diagnostic(trimmed),
            trimmed.starts_with("$ go ")
                || trimmed.starts_with("go: downloading ")
                || trimmed.starts_with("ok  \t"),
        ),
        BuildLogFormat::Make => (
            trimmed.starts_with("make: ***")
                || (trimmed.starts_with("make")
                    && (trimmed.contains("Entering directory")
                        || trimmed.contains("Leaving directory")))
                || is_c_diagnostic(trimmed),
            is_compiler_command(trimmed),
        ),
        BuildLogFormat::Generic => (false, false),
    };
    Evidence {
        strong: usize::from(strong),
        weak: usize::from(weak),
    }
}

pub(crate) fn classify(
    line: &str,
    format: BuildLogFormat,
    generic_templates: &HashSet<String>,
) -> LineRole {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    if is_diagnostic(trimmed, &lower, format) {
        return LineRole::Diagnostic;
    }
    if is_summary(trimmed, &lower, format) {
        return LineRole::Summary;
    }
    if is_phase(trimmed, format) {
        return LineRole::Phase;
    }
    if let Some(family) = routine_family(trimmed, format, generic_templates) {
        return LineRole::Routine(family);
    }
    LineRole::Unknown
}

fn is_diagnostic(trimmed: &str, lower: &str, format: BuildLogFormat) -> bool {
    let generic = [
        "error",
        "warning",
        "warn",
        "failed",
        "failure",
        "fatal",
        "panic",
        "exception",
        "assertion",
        "assert",
    ]
    .iter()
    .any(|signal| contains_signal_word(lower, signal))
        || [
            "segmentation fault",
            "core dumped",
            "out of memory",
            "permission denied",
            "command not found",
            "timed out",
        ]
        .iter()
        .any(|signal| lower.contains(signal));
    generic
        || trimmed.starts_with('✕')
        || trimmed.starts_with('✗')
        || matches!(format, BuildLogFormat::Pytest)
            && (starts_any(trimmed, &["FAILED ", "ERROR ", "XPASS ", "XFAIL "])
                || [" FAILED", " ERROR", " XPASS", " XFAIL"]
                    .iter()
                    .any(|marker| trimmed.contains(marker)))
        || is_go_diagnostic(trimmed)
        || is_c_diagnostic(trimmed)
        || matches!(format, BuildLogFormat::Npm) && trimmed.starts_with("npm ERR!")
        || matches!(format, BuildLogFormat::Go) && trimmed.starts_with("--- FAIL:")
        || matches!(format, BuildLogFormat::Jest) && trimmed.starts_with("FAIL ")
}

fn contains_signal_word(value: &str, signal: &str) -> bool {
    value.match_indices(signal).any(|(start, matched)| {
        let end = start + matched.len();
        let before = value[..start].chars().next_back();
        let after = value[end..].chars().next();
        before.is_none_or(|character| !is_identifier_character(character))
            && after.is_none_or(|character| !is_identifier_character(character))
    })
}

fn is_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

fn is_summary(trimmed: &str, lower: &str, format: BuildLogFormat) -> bool {
    lower.starts_with("exit code:")
        || lower.starts_with("exit status:")
        || trimmed.starts_with("test result:")
        || matches!(format, BuildLogFormat::Cargo) && trimmed.starts_with("Finished ")
        || matches!(format, BuildLogFormat::Pytest) && is_pytest_summary(trimmed, lower)
        || matches!(format, BuildLogFormat::Npm)
            && (lower.starts_with("added ")
                || lower.starts_with("removed ")
                || lower.starts_with("changed ")
                || lower.starts_with("found ")
                || lower.contains("packages are looking for funding"))
        || matches!(format, BuildLogFormat::Jest)
            && starts_any(trimmed, &["Test Suites:", "Tests:", "Snapshots:", "Time:"])
        || matches!(format, BuildLogFormat::Go)
            && (trimmed.starts_with("PASS") || trimmed.starts_with("FAIL\t"))
}

fn is_phase(trimmed: &str, format: BuildLogFormat) -> bool {
    trimmed.starts_with("$ ")
        || trimmed.contains("test session starts")
        || trimmed.contains("short test summary info")
        || trimmed.trim_matches('=').trim() == "FAILURES"
        || trimmed.starts_with("collected ")
        || matches!(format, BuildLogFormat::Cargo) && trimmed.starts_with("running ")
        || matches!(format, BuildLogFormat::Make)
            && (trimmed.contains("Entering directory") || trimmed.contains("Leaving directory"))
        || matches!(format, BuildLogFormat::Npm) && trimmed.starts_with("> ")
}

fn routine_family(
    trimmed: &str,
    format: BuildLogFormat,
    generic_templates: &HashSet<String>,
) -> Option<RoutineFamily> {
    match format {
        BuildLogFormat::Cargo => {
            if starts_any(trimmed, &["Compiling ", "Checking ", "Building ", "Fresh "]) {
                Some(RoutineFamily::CargoCompile)
            } else if starts_any(trimmed, &["Downloading ", "Downloaded "]) {
                Some(RoutineFamily::CargoDownload)
            } else if trimmed.starts_with("test ") && trimmed.ends_with(" ... ok") {
                Some(RoutineFamily::CargoTestPass)
            } else {
                None
            }
        }
        BuildLogFormat::Pytest => {
            if trimmed.contains("::")
                && [" PASSED", " SKIPPED"]
                    .iter()
                    .any(|marker| trimmed.contains(marker))
            {
                Some(RoutineFamily::PytestPass)
            } else if is_pytest_progress(trimmed) {
                Some(RoutineFamily::PytestProgress)
            } else {
                None
            }
        }
        BuildLogFormat::Npm => {
            if trimmed.starts_with("npm http fetch ") && is_successful_http_fetch(trimmed) {
                Some(RoutineFamily::NpmFetch)
            } else if is_spinner(trimmed) {
                Some(RoutineFamily::NpmSpinner)
            } else {
                None
            }
        }
        BuildLogFormat::Jest => (trimmed.starts_with("PASS ") || trimmed.starts_with('✓'))
            .then_some(RoutineFamily::JestPass),
        BuildLogFormat::Go => {
            if trimmed.starts_with("go: downloading ") {
                Some(RoutineFamily::GoDownload)
            } else if trimmed.starts_with("ok  \t") || trimmed.starts_with("--- PASS:") {
                Some(RoutineFamily::GoPass)
            } else {
                None
            }
        }
        BuildLogFormat::Make => is_compiler_command(trimmed).then_some(RoutineFamily::MakeCompile),
        BuildLogFormat::Generic => generic_progress_template(trimmed)
            .is_some_and(|template| generic_templates.contains(&template))
            .then_some(RoutineFamily::Generic),
    }
}

fn starts_any(value: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| value.starts_with(prefix))
}

fn is_successful_http_fetch(line: &str) -> bool {
    line.split_ascii_whitespace().any(|part| {
        part.len() == 3 && part.starts_with('2') && part.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn is_spinner(line: &str) -> bool {
    !line.is_empty()
        && line.chars().count() <= 3
        && line
            .chars()
            .all(|character| ('\u{2800}'..='\u{28ff}').contains(&character))
}

fn is_pytest_summary(line: &str, lower: &str) -> bool {
    lower.contains(" in ")
        && [
            " passed", " failed", " skipped", " xfailed", " xpassed", " error",
        ]
        .iter()
        .any(|outcome| lower.contains(outcome))
        && (line.starts_with('=') || line.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

fn is_pytest_progress(line: &str) -> bool {
    let Some(open) = line.rfind('[') else {
        return false;
    };
    let Some(percent) = line[open + 1..].strip_suffix(']') else {
        return false;
    };
    let Some(number) = percent.trim().strip_suffix('%') else {
        return false;
    };
    if !number.parse::<u8>().is_ok_and(|value| value <= 100) {
        return false;
    }

    let progress = line[..open].trim_end();
    let outcomes = progress
        .rsplit_once(char::is_whitespace)
        .map_or(progress, |(_, outcomes)| outcomes);
    !outcomes.is_empty()
        && outcomes
            .chars()
            .all(|character| matches!(character, '.' | 's' | 'S'))
}

fn is_compiler_command(line: &str) -> bool {
    starts_any(line, &["cc ", "gcc ", "clang ", "c++ ", "g++ "])
        && (line.contains(" -c ") || line.contains(" -o "))
}

fn is_go_diagnostic(line: &str) -> bool {
    let Some(position) = line.find(".go:") else {
        return false;
    };
    let rest = &line[position + 4..];
    let Some((line_number, rest)) = rest.split_once(':') else {
        return false;
    };
    !line_number.is_empty()
        && line_number.bytes().all(|byte| byte.is_ascii_digit())
        && rest.split_once(':').is_some_and(|(column, _)| {
            !column.is_empty() && column.bytes().all(|byte| byte.is_ascii_digit())
        })
}

fn is_c_diagnostic(line: &str) -> bool {
    [".c:", ".cc:", ".cpp:", ".cxx:", ".h:", ".hpp:"]
        .iter()
        .any(|extension| {
            let Some(position) = line.find(extension) else {
                return false;
            };
            let rest = &line[position + extension.len()..];
            rest.split_once(':').is_some_and(|(line_number, _)| {
                !line_number.is_empty() && line_number.bytes().all(|byte| byte.is_ascii_digit())
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/classify_tests.rs");
}
