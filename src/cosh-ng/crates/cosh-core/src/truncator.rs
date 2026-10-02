pub struct OutputTruncator {
    pub max_bytes: usize,
    pub max_lines: usize,
}

impl Default for OutputTruncator {
    fn default() -> Self {
        Self {
            max_bytes: 25000,
            max_lines: 1000,
        }
    }
}

pub(crate) fn truncate_to_byte_limit(value: &str, max_bytes: usize) -> &str {
    let mut boundary = max_bytes.min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    &value[..boundary]
}

/// Builds the head kept when the line count exceeds the budget: at most
/// `max_lines` lines and at most `max_bytes` bytes.
///
/// Both bounds are applied while walking the input rather than afterwards, so
/// the buffer never allocates past `max_bytes`. Collecting the lines first cost
/// one 16-byte fat pointer each, which a newline-dense capture (issue #2841's
/// `yes` reproducer: ~16.7M two-byte lines in 32 MiB) turned into 256 MiB of
/// descriptors just to keep the first thousand. Appending only what still fits
/// covers the mirror case, where a single line dwarfs the whole budget and must
/// not be materialized in full just to be trimmed after.
fn bounded_head(output: &str, max_lines: usize, max_bytes: usize) -> String {
    let mut head = String::new();
    for (idx, line) in output.lines().take(max_lines).enumerate() {
        if idx > 0 {
            // No room left even for the separator, so the budget is spent.
            if head.len() >= max_bytes {
                break;
            }
            head.push('\n');
        }
        let remaining = max_bytes - head.len();
        if line.len() > remaining {
            head.push_str(truncate_to_byte_limit(line, remaining));
            break;
        }
        head.push_str(line);
    }
    head
}

impl OutputTruncator {
    pub fn truncate(&self, output: &str) -> (String, bool) {
        let line_count = output.lines().count();
        let byte_count = output.len();

        if byte_count <= self.max_bytes && line_count <= self.max_lines {
            return (output.to_string(), false);
        }

        let truncated = if line_count > self.max_lines {
            bounded_head(output, self.max_lines, self.max_bytes)
        } else {
            truncate_to_byte_limit(output, self.max_bytes).to_string()
        };

        let result = format!(
            "{truncated}\n\n[output truncated: {byte_count} bytes / {line_count} lines → {} bytes]",
            truncated.len()
        );
        (result, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_truncation_when_within_limits() {
        let t = OutputTruncator::default();
        let (result, truncated) = t.truncate("hello world");
        assert_eq!(result, "hello world");
        assert!(!truncated);
    }

    #[test]
    fn truncates_by_line_count() {
        let t = OutputTruncator {
            max_bytes: 100_000,
            max_lines: 3,
        };
        let input = "line1\nline2\nline3\nline4\nline5\n";
        let (result, truncated) = t.truncate(input);
        assert!(truncated);
        assert_eq!(
            result,
            "line1\nline2\nline3\n\n[output truncated: 30 bytes / 5 lines → 17 bytes]"
        );
    }

    #[test]
    fn truncates_by_byte_count() {
        let t = OutputTruncator {
            max_bytes: 10,
            max_lines: 100_000,
        };
        let input = "a]".repeat(20);
        let (result, truncated) = t.truncate(&input);
        assert!(truncated);
        assert_eq!(
            result,
            "a]a]a]a]a]\n\n[output truncated: 40 bytes / 1 lines → 10 bytes]"
        );
    }

    fn assert_truncation(input: &str, max_bytes: usize, expected: &str) {
        let t = OutputTruncator {
            max_bytes,
            max_lines: 100_000,
        };
        let (result, truncated) = t.truncate(input);

        assert!(truncated);
        assert_eq!(
            result,
            format!(
                "{expected}\n\n[output truncated: {} bytes / 1 lines → {} bytes]",
                input.len(),
                expected.len()
            )
        );
    }

    #[test]
    fn truncates_multibyte_output_at_exact_utf8_boundaries() {
        assert_truncation("中文a", 6, "中文");
        assert_truncation("😀a", 4, "😀");
    }

    #[test]
    fn rounds_multibyte_output_limits_down_to_utf8_boundaries() {
        assert_truncation("中文", 5, "中");
        assert_truncation("a😀", 3, "a");
    }

    #[test]
    fn line_branch_respects_max_bytes_cap() {
        let t = OutputTruncator {
            max_bytes: 20,
            max_lines: 2,
        };
        // 3 lines of 30 chars each: line_count > max_lines triggers the line
        // branch, but the first two lines joined (61 bytes) exceed max_bytes.
        let input = format!("{}\n{}\n{}", "a".repeat(30), "b".repeat(30), "c".repeat(30));
        let (result, truncated) = t.truncate(&input);
        assert!(truncated);
        let content = result.split("\n\n[output truncated:").next().unwrap();
        assert_eq!(content.len(), 20);
        assert!(result.contains("→ 20 bytes]"));
    }

    #[test]
    fn line_branch_falls_back_to_byte_limit_at_utf8_boundary() {
        let t = OutputTruncator {
            max_bytes: 4,
            max_lines: 2,
        };
        // 3 lines; kept lines joined exceed max_bytes, so we fall back to a
        // UTF-8 safe byte boundary rather than splitting a multibyte character.
        let (result, truncated) = t.truncate("中文\n中文中文\n中文中文中文");
        assert!(truncated);
        let content = result.split("\n\n[output truncated:").next().unwrap();
        assert_eq!(content, "中");
        assert_eq!(content.len(), 3);
    }

    #[test]
    fn newline_dense_input_stays_within_the_line_budget() {
        // The shell tool's `yes` reproducer in miniature: millions of 2-byte
        // lines. The line branch must never materialize one slice per line of
        // the whole stream, only the budgeted head.
        let t = OutputTruncator::default();
        let input = "y\n".repeat(2_000_000);
        let (result, truncated) = t.truncate(&input);
        assert!(truncated);
        assert!(result.contains("4000000 bytes / 2000000 lines"));
        let content = result.split("\n\n[output truncated:").next().unwrap();
        // 1000 "y" lines joined by \n stay well under the 25 KB budget.
        assert_eq!(content.len(), 1999);
    }

    #[test]
    fn long_lines_do_not_allocate_a_full_second_copy() {
        // Complements the case above: few but very long lines. Stopping at the
        // byte cap keeps the built string near max_bytes instead of rejoining
        // every budgeted line first.
        let t = OutputTruncator {
            max_bytes: 1000,
            max_lines: 4,
        };
        let input = format!("{}\n", "x".repeat(10_000)).repeat(5);
        let (result, truncated) = t.truncate(&input);
        assert!(truncated);
        let content = result.split("\n\n[output truncated:").next().unwrap();
        assert_eq!(content.len(), 1000);
    }

    #[test]
    fn an_oversized_first_line_is_appended_only_up_to_the_budget() {
        // The line branch is entered because of the short lines that follow,
        // but the first line alone dwarfs the byte budget. Appending it whole
        // and trimming afterwards would allocate the entire line.
        let t = OutputTruncator {
            max_bytes: 40,
            max_lines: 3,
        };
        let input = format!("{}\n{}", "a".repeat(200_000), "b\n".repeat(10));
        let (result, truncated) = t.truncate(&input);
        assert!(truncated);
        let content = result.split("\n\n[output truncated:").next().unwrap();
        assert_eq!(content, "a".repeat(40));
    }

    #[test]
    fn bounded_head_never_allocates_past_the_byte_budget() {
        // Capacity is the observable proxy for peak allocation, and the only
        // assertion that separates this from trimming after the fact: the
        // returned text is identical either way, but appending the whole line
        // first would grow the buffer to the line's size.
        let input = format!("{}\n{}", "a".repeat(200_000), "b\n".repeat(10));
        let head = bounded_head(&input, 3, 40);
        assert_eq!(head, "a".repeat(40));
        assert!(
            head.capacity() < 200_000,
            "buffer grew to {} bytes for a 40-byte budget",
            head.capacity()
        );
    }

    #[test]
    fn bounded_head_stays_small_for_newline_dense_input() {
        // The other pathological shape: millions of tiny lines. Neither the
        // line count nor the byte count may drive the allocation.
        let input = "y\n".repeat(2_000_000);
        let head = bounded_head(&input, 1000, 25_000);
        assert_eq!(head.len(), 1999);
        assert!(
            head.capacity() < 25_000,
            "buffer grew to {} bytes",
            head.capacity()
        );
    }

    #[test]
    fn a_line_that_exactly_fills_the_budget_stops_cleanly() {
        // Guards the separator accounting: with no room left for the newline,
        // the loop must stop rather than push one byte past the budget.
        let t = OutputTruncator {
            max_bytes: 5,
            max_lines: 3,
        };
        let (result, truncated) = t.truncate("12345\nabc\ndef\nghi\n");
        assert!(truncated);
        let content = result.split("\n\n[output truncated:").next().unwrap();
        assert_eq!(content, "12345");
    }

    #[test]
    fn empty_input() {
        let t = OutputTruncator::default();
        let (result, truncated) = t.truncate("");
        assert_eq!(result, "");
        assert!(!truncated);
    }
}
