use super::*;

pub(super) struct BoundedLine {
    text: String,
    direction: FailureExcerptDirection,
}

pub(super) struct BoundedOutput {
    pub(super) lines: Vec<BoundedLine>,
}

impl BoundedOutput {
    pub(super) fn new(output: &str) -> Self {
        let mut head = Vec::new();
        let mut head_bytes = 0usize;
        let mut tail = std::collections::VecDeque::new();
        let mut tail_bytes = 0usize;

        for (index, line) in output.lines().enumerate() {
            if head.len() < CLASSIFIER_SIDE_LINES && head_bytes < CLASSIFIER_SIDE_BYTES {
                let available = CLASSIFIER_SIDE_BYTES - head_bytes;
                if available > 1 {
                    let text = prefix_bytes(line, available - 1).to_string();
                    head_bytes += text.len() + 1;
                    head.push((index, normalize_output(&text)));
                }
            }

            let text = suffix_bytes(line, CLASSIFIER_SIDE_BYTES - 1).to_string();
            let line_bytes = text.len() + 1;
            while tail.len() >= CLASSIFIER_SIDE_LINES
                || tail_bytes + line_bytes > CLASSIFIER_SIDE_BYTES
            {
                let Some((_, removed)): Option<(usize, String)> = tail.pop_front() else {
                    break;
                };
                tail_bytes -= removed.len() + 1;
            }
            tail_bytes += line_bytes;
            tail.push_back((index, normalize_output(&text)));
        }

        let tail_start = tail.front().map(|(index, _)| *index).unwrap_or(usize::MAX);
        let mut lines = Vec::with_capacity(head.len() + tail.len());
        lines.extend(
            head.into_iter()
                .filter(|(index, _)| *index < tail_start)
                .map(|(_, text)| BoundedLine {
                    text,
                    direction: FailureExcerptDirection::Head,
                }),
        );
        lines.extend(tail.into_iter().map(|(_, text)| BoundedLine {
            text,
            direction: FailureExcerptDirection::Tail,
        }));
        Self { lines }
    }

    pub(super) fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn prefix_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn suffix_bytes(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

pub(super) fn build_or_test_family(command: &str) -> Option<BuildOrTestFamily> {
    let program = first_program_token(command);
    let subcommand = subcommand_after_program(command, program);
    match program {
        "cargo"
            if matches!(
                subcommand,
                Some("test" | "build" | "check" | "clippy" | "bench")
            ) =>
        {
            Some(BuildOrTestFamily::Cargo)
        }
        "make" | "gmake" => Some(BuildOrTestFamily::Make),
        "ninja" => Some(BuildOrTestFamily::Ninja),
        "mvn" | "mvnw" => Some(BuildOrTestFamily::Maven),
        "gradle" | "gradlew" => Some(BuildOrTestFamily::Gradle),
        "npm" if matches!(subcommand, Some("test" | "run")) => Some(BuildOrTestFamily::Npm),
        "pytest" | "py.test" => Some(BuildOrTestFamily::Pytest),
        "go" if subcommand == Some("test") => Some(BuildOrTestFamily::GoTest),
        _ => None,
    }
}

fn subcommand_after_program<'a>(command: &'a str, program: &str) -> Option<&'a str> {
    let mut found = false;
    for token in command.split_whitespace() {
        let basename = token.rsplit('/').next().unwrap_or(token);
        if !found {
            if basename == program {
                found = true;
            }
            continue;
        }
        if token.starts_with('-') || token.starts_with('+') {
            continue;
        }
        return Some(token);
    }
    None
}

pub(super) fn terminal_summary_for_family(
    output: &BoundedOutput,
    family: BuildOrTestFamily,
) -> Option<FailureTerminalSignature> {
    output
        .lines
        .iter()
        .filter(|line| line.direction == FailureExcerptDirection::Tail)
        .find_map(|line| terminal_summary_line(family, &line.text))
}

fn terminal_summary_line(
    family: BuildOrTestFamily,
    line: &str,
) -> Option<FailureTerminalSignature> {
    let trimmed = line.trim();
    match family {
        BuildOrTestFamily::Cargo if line.contains("test result: failed") => {
            Some(FailureTerminalSignature::CargoTest)
        }
        BuildOrTestFamily::Cargo if line.starts_with("error: test failed, to rerun pass") => {
            Some(FailureTerminalSignature::CargoTestRerun)
        }
        BuildOrTestFamily::Cargo
            if line.contains("could not compile") || line.contains("compilation failed") =>
        {
            Some(FailureTerminalSignature::CargoBuild)
        }
        BuildOrTestFamily::Make
            if trimmed.starts_with("make:") && line.contains("***") && line.contains("error") =>
        {
            Some(FailureTerminalSignature::Make)
        }
        BuildOrTestFamily::Ninja if trimmed == "ninja: build stopped: subcommand failed." => {
            Some(FailureTerminalSignature::Ninja)
        }
        BuildOrTestFamily::Maven
            if matches!(trimmed, "[info] build failure" | "[error] build failure") =>
        {
            Some(FailureTerminalSignature::Maven)
        }
        BuildOrTestFamily::Gradle
            if trimmed == "build failed" || trimmed.starts_with("build failed in ") =>
        {
            Some(FailureTerminalSignature::Gradle)
        }
        BuildOrTestFamily::Npm
            if trimmed.starts_with("npm err!") || trimmed.starts_with("npm error") =>
        {
            Some(FailureTerminalSignature::Npm)
        }
        BuildOrTestFamily::Pytest
            if trimmed.starts_with('=')
                && trimmed.ends_with('=')
                && line.contains(" failed")
                && line.contains(" in ") =>
        {
            Some(FailureTerminalSignature::Pytest)
        }
        BuildOrTestFamily::GoTest if trimmed == "fail" || line.starts_with("fail\t") => {
            Some(FailureTerminalSignature::GoTest)
        }
        _ => None,
    }
}

pub(super) fn output_permission_signature(
    output: &BoundedOutput,
) -> Option<(FailureTerminalSignature, FailureExcerptDirection)> {
    output
        .lines
        .iter()
        .filter(|line| line.direction == FailureExcerptDirection::Tail)
        .find(|line| {
            line.text.contains("permission denied")
                || line.text.contains("operation not permitted")
                || line.text.contains("eacces")
        })
        .map(|line| (FailureTerminalSignature::PermissionDenied, line.direction))
}

pub(super) fn runtime_exception_signature(
    output: &BoundedOutput,
) -> Option<(FailureTerminalSignature, FailureExcerptDirection)> {
    if output.lines.iter().any(|line| {
        line.text
            .trim_start()
            .starts_with("traceback (most recent call last):")
    }) {
        if let Some(line) = output.lines.iter().rev().find(|line| {
            line.direction == FailureExcerptDirection::Tail && python_exception_line(&line.text)
        }) {
            return Some((FailureTerminalSignature::PythonTraceback, line.direction));
        }
    }
    output
        .lines
        .iter()
        .find(|line| line.text.contains("thread '") && line.text.contains("panicked at"))
        .map(|line| (FailureTerminalSignature::RustPanic, line.direction))
}

fn python_exception_line(line: &str) -> bool {
    let Some((name, _)) = line.trim().split_once(':') else {
        return false;
    };
    let name = name.rsplit('.').next().unwrap_or(name);
    (name.ends_with("error") || name.ends_with("exception"))
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

pub(super) fn fatal_signal_signature(
    exit_code: i32,
    output: &BoundedOutput,
) -> Option<(FailureTerminalSignature, FailureExcerptDirection)> {
    output
        .lines
        .iter()
        .filter(|line| line.direction == FailureExcerptDirection::Tail)
        .find_map(|line| {
            if exit_code == 139 && line.text.contains("segmentation fault") {
                Some((FailureTerminalSignature::SegmentationFault, line.direction))
            } else if line.text.contains("core dumped") {
                Some((FailureTerminalSignature::CoreDumped, line.direction))
            } else {
                None
            }
        })
}

pub(super) fn push_terminal_signature_reason(
    reasons: &mut Vec<FailureReason>,
    signature: Option<(FailureTerminalSignature, FailureExcerptDirection)>,
) {
    if let Some((signature, direction)) = signature {
        reasons.push(FailureReason::TerminalSignature(signature));
        reasons.push(FailureReason::ExcerptDirection(direction));
    }
}
