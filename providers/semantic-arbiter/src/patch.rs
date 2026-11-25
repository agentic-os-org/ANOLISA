//! Strict mirror of the OpenClaw `apply_patch` envelope grammar; parses, never applies.
//!
//! Input the OpenClaw parser would reject is rejected here too, so a covered
//! patch names exactly the files the native tool would attempt, in hunk order.

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";

/// One file hunk with its paths exactly as written.
#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    Write(String),
    Remove(String),
    Move { from: String, to: String },
}

/// Returns every hunk, or `None` when the native parser would reject the patch.
pub(crate) fn parse(input: &str) -> Option<Vec<Step>> {
    let trimmed = trim(input);
    if trimmed.is_empty() {
        return None;
    }
    let lines: Vec<&str> = trimmed
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let lines = envelope(&lines)?;
    let mut rest = &lines[1..lines.len() - 1];
    let mut steps = vec![];
    while !rest.is_empty() {
        let consumed = hunk(rest, &mut steps)?;
        rest = &rest[consumed..];
    }
    // The native tool reports an empty patch as an error, not a no-op.
    (!steps.is_empty()).then_some(steps)
}

/// ECMAScript `String.prototype.trim`, whose whitespace set differs from Rust's.
fn trim(text: &str) -> &str {
    text.trim_matches(|c| {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
                ..='\u{200a}'
                    | '\u{2028}'
                    | '\u{2029}'
                    | '\u{202f}'
                    | '\u{205f}'
                    | '\u{3000}'
                    | '\u{feff}'
        )
    })
}

fn strict(lines: &[&str]) -> bool {
    lines.first().map(|line| trim(line)) == Some(BEGIN)
        && lines.last().map(|line| trim(line)) == Some(END)
}

/// Accepts the bare envelope or one wrapped in a heredoc the model echoed verbatim.
fn envelope<'a>(lines: &'a [&'a str]) -> Option<&'a [&'a str]> {
    if strict(lines) {
        return Some(lines);
    }
    let (first, last) = (lines[0], lines[lines.len() - 1]);
    if lines.len() < 4
        || !["<<EOF", "<<'EOF'", "<<\"EOF\""].contains(&first)
        || !last.ends_with("EOF")
    {
        return None;
    }
    let inner = &lines[1..lines.len() - 1];
    strict(inner).then_some(inner)
}

fn hunk(lines: &[&str], steps: &mut Vec<Step>) -> Option<usize> {
    let header = trim(lines[0]);
    if let Some(path) = header.strip_prefix(ADD) {
        steps.push(Step::Write(path.to_owned()));
        let body = lines[1..]
            .iter()
            .take_while(|line| line.starts_with('+'))
            .count();
        return Some(1 + body);
    }
    if let Some(path) = header.strip_prefix(DELETE) {
        steps.push(Step::Remove(path.to_owned()));
        return Some(1);
    }
    let path = header.strip_prefix(UPDATE)?;
    let to = lines.get(1).and_then(|line| trim(line).strip_prefix(MOVE));
    let mut consumed = if to.is_some() { 2 } else { 1 };
    let mut chunks = 0;
    while let Some(line) = lines.get(consumed) {
        if trim(line).is_empty() {
            consumed += 1;
            continue;
        }
        if line.starts_with("***") {
            break;
        }
        consumed += chunk(&lines[consumed..], chunks == 0)?;
        chunks += 1;
    }
    if chunks == 0 {
        return None;
    }
    steps.push(match to {
        Some(to) => Step::Move {
            from: path.to_owned(),
            to: to.to_owned(),
        },
        None => Step::Write(path.to_owned()),
    });
    Some(consumed)
}

/// Lines one update chunk consumes; only the first chunk may omit its `@@` header.
fn chunk(lines: &[&str], first: bool) -> Option<usize> {
    let start = match lines[0] {
        "@@" => 1,
        line if line.starts_with("@@ ") => 1,
        _ if first => 0,
        _ => return None,
    };
    if start >= lines.len() {
        return None;
    }
    let mut parsed = 0;
    for line in &lines[start..] {
        if *line == END_OF_FILE {
            if parsed == 0 {
                return None;
            }
            parsed += 1;
            break;
        }
        match line.chars().next() {
            None | Some(' ' | '+' | '-') => parsed += 1,
            _ if parsed == 0 => return None,
            _ => break,
        }
    }
    Some(start + parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &str) -> Step {
        Step::Write(path.into())
    }

    #[test]
    fn hunks_are_reported_in_order_with_paths_as_written() {
        let patch = "*** Begin Patch\n*** Add File: notes/new.txt\n+hello\n\
            *** Update File: src/a.rs\n@@ fn a\n-old\n+new\n context\n\
            @@\n-x\n+y\n*** End of File\n*** Delete File: old.txt\n\
            *** Update File: from.txt\n*** Move to: to.txt\n-a\n+b\n*** End Patch";
        assert_eq!(
            parse(patch),
            Some(vec![
                write("notes/new.txt"),
                write("src/a.rs"),
                Step::Remove("old.txt".into()),
                Step::Move {
                    from: "from.txt".into(),
                    to: "to.txt".into()
                },
            ])
        );
    }

    #[test]
    fn crlf_padding_indented_headers_and_heredoc_wrapping_are_accepted() {
        let body = "*** Begin Patch\r\n  *** Add File: a.txt\r\n+x\r\n\
            *** Update File: b.txt\r\n\r\n-a\r\n+b\r\n*** End Patch";
        assert_eq!(
            parse(&format!("\u{feff}\n {body} \n")),
            Some(vec![write("a.txt"), write("b.txt")])
        );
        assert_eq!(
            parse(&format!("<<'EOF'\n{body}\nEOF")),
            Some(vec![write("a.txt"), write("b.txt")])
        );
    }

    #[test]
    fn input_the_native_parser_rejects_is_rejected() {
        for patch in [
            "",
            " \n ",
            "*** Begin Patch\n*** End Patch",
            "*** Begin Patch\n*** Add File: a\n+x",
            "*** Add File: a\n+x\n*** End Patch",
            "<<EOF\n*** Add File: a\n+x\nEOF",
            "*** Begin Patch\n*** Add File:\n*** End Patch",
            "*** Begin Patch\n*** Copy File: a\n*** End Patch",
            "*** Begin Patch\n*** Add File: a\nx\n*** End Patch",
            "*** Begin Patch\n*** Add File: a\n+x\n\n*** Delete File: b\n*** End Patch",
            "*** Begin Patch\n*** Update File: a\n*** End Patch",
            "*** Begin Patch\n*** Update File: a\n@@\n*** End Patch",
            "*** Begin Patch\n*** Update File: a\n@@\n*** End of File\n*** End Patch",
            "*** Begin Patch\n*** Update File: a\nnot a diff line\n*** End Patch",
            "*** Begin Patch\n*** Update File: a\n-x\nnot a header\n*** End Patch",
        ] {
            assert_eq!(parse(patch), None, "{patch:?}");
        }
    }
}
