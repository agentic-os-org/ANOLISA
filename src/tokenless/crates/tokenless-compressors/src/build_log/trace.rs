//! Trace-region boundary detection for build-log diagnostics.
//!
//! Whole trace regions are protected so progress reduction never cuts an
//! exception chain or backtrace.

use std::ops::Range;

/// Detect trace regions over ANSI-stripped line views. Regions never
/// overlap and are returned in order.
pub(crate) fn trace_regions(lines: &[String]) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let end = python_trace(lines, i)
            .or_else(|| rust_trace(lines, i))
            .or_else(|| go_trace(lines, i))
            .or_else(|| exception_trace(lines, i));
        match end {
            Some(end) if end > i => {
                regions.push(i..end);
                i = end;
            }
            _ => i += 1,
        }
    }
    regions
}

fn is_indented(line: &str) -> bool {
    line.starts_with(' ') || line.starts_with('\t')
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Python: `Traceback (most recent call last):` + indented frames + the
/// final exception line; chained tracebacks continue through the blank-line
/// separated `During handling of…` / `The above exception…` sentences (a
/// chained trace must survive its blank lines intact — a known failure mode
/// in prior art).
fn python_trace(lines: &[String], start: usize) -> Option<usize> {
    if !lines[start]
        .trim_start()
        .starts_with("Traceback (most recent call last):")
    {
        return None;
    }
    let mut i = start + 1;
    loop {
        while i < lines.len() && is_indented(&lines[i]) && !is_blank(&lines[i]) {
            i += 1;
        }
        if i < lines.len() && !is_blank(&lines[i]) && !is_indented(&lines[i]) {
            i += 1; // the final `SomeError: message` line
        }
        let mut j = i;
        while j < lines.len() && is_blank(&lines[j]) {
            j += 1;
        }
        if j < lines.len()
            && (lines[j].starts_with("During handling of the above exception")
                || lines[j].starts_with("The above exception was the direct cause"))
        {
            let mut k = j + 1;
            while k < lines.len() && is_blank(&lines[k]) {
                k += 1;
            }
            if k < lines.len()
                && lines[k]
                    .trim_start()
                    .starts_with("Traceback (most recent call last):")
            {
                i = k + 1;
                continue;
            }
            i = j + 1;
        }
        break;
    }
    Some(i)
}

/// Rust: `thread '…' panicked at …`, the panic message, and the optional
/// `stack backtrace:` block with numbered frames and `at path:line` lines.
fn rust_trace(lines: &[String], start: usize) -> Option<usize> {
    let head = lines[start].trim_start();
    if !(head.starts_with("thread '") && head.contains("panicked at")) {
        return None;
    }
    let mut i = start + 1;
    // The 2021+ format puts the panic message on its own unindented line
    // right after the header; absorbing one following line over-keeps at
    // worst, which is the safe direction.
    if i < lines.len() && !is_blank(&lines[i]) && !is_rust_backtrace_line(&lines[i]) {
        i += 1;
    }
    while i < lines.len() && is_rust_backtrace_line(&lines[i]) {
        i += 1;
    }
    Some(i)
}

fn is_rust_backtrace_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("note: run with") || trimmed == "stack backtrace:" {
        return true;
    }
    if !is_indented(line) {
        return false;
    }
    if trimmed.starts_with("at ") {
        return true;
    }
    // `  12: core::panicking::panic_fmt`
    match trimmed.split_once(':') {
        Some((frame_no, _)) => !frame_no.is_empty() && frame_no.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Go: `panic: …` / `goroutine N [state]:` headers, `\t/path/file.go:NN`
/// location lines with their function lines, `created by …` lines; multiple
/// goroutine dumps separated by single blank lines stay one region.
fn go_trace(lines: &[String], start: usize) -> Option<usize> {
    let head = &lines[start];
    if !(head.starts_with("panic: ") || is_goroutine_header(head) || head.starts_with("[signal ")) {
        return None;
    }
    let mut i = start + 1;
    while i < lines.len() {
        let line = &lines[i];
        let continues = line.starts_with('\t')
            || is_goroutine_header(line)
            || line.starts_with("created by ")
            || line.starts_with("[signal ");
        // A blank line bridges consecutive goroutine dumps; a non-blank line
        // is a function line when its location follows on the next line.
        let bridges = match lines.get(i + 1) {
            Some(next) if is_blank(line) => is_goroutine_header(next),
            Some(next) => next.starts_with('\t') && next.contains(".go:"),
            None => false,
        };
        if !(continues || bridges) {
            break;
        }
        i += 1;
    }
    Some(i)
}

fn is_goroutine_header(line: &str) -> bool {
    line.starts_with("goroutine ") && line.contains(" [") && line.ends_with("]:")
}

/// Java, JS/Node, and .NET share an Error/Exception header followed by
/// indented `at …` frames. Their cause and inner-exception separators remain
/// part of the same diagnostic region.
fn exception_trace(lines: &[String], start: usize) -> Option<usize> {
    let head = &lines[start];
    if is_indented(head) || !(head.contains("Error") || head.contains("Exception")) {
        return None;
    }
    let mut i = start + 1;
    let mut frames = 0usize;
    while i < lines.len() {
        while i < lines.len() && is_at_frame(&lines[i]) {
            frames += 1;
            i += 1;
        }
        if i == lines.len() {
            break;
        }
        let trimmed = lines[i].trim_start();
        if trimmed.starts_with("Caused by:")
            || trimmed.starts_with("---> ") && trimmed.contains("Exception")
            || trimmed == "--- End of inner exception stack trace ---"
            || (trimmed.starts_with("... ") && trimmed.ends_with(" more"))
        {
            i += 1;
            continue;
        }
        break;
    }
    (frames > 0).then_some(i)
}

fn is_at_frame(line: &str) -> bool {
    is_indented(line) && line.trim_start().starts_with("at ")
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../tests/trace_tests.rs");
}
