//! Recognizes shell commands whose only effect is printing local files.
//!
//! Such output is reported as `file_read`: data such as JSON, CSV, build logs
//! and diffs still compresses, but a printed HTML page stays verbatim because
//! it is source the agent may edit.

/// Programs that only print their operands; `sed` qualifies separately when it
/// runs in `-n` mode with a print-only script such as `sed -n '1,80p' page.html`.
const PRINT_PROGRAMS: [&str; 7] = ["cat", "head", "tail", "nl", "less", "more", "bat"];

/// Returns whether `command` is a plain invocation that only prints local files.
///
/// Only a single command qualifies, optionally after `cd ... &&` prefixes; a
/// pipe, redirection, heredoc, command list, substitution or unparsable quoting
/// keeps the output classified as command output.
pub(crate) fn prints_local_files(command: &str) -> bool {
    // A newline separates commands in the shell but is only whitespace to the
    // word splitter; surrounding blank lines separate nothing.
    let command = command.trim();
    if command.contains(['\n', '\r']) {
        return false;
    }
    let Some(words) = split_words(command) else {
        return false;
    };
    let mut groups: Vec<Vec<&str>> = vec![Vec::new()];
    for word in &words {
        if word == "&&" {
            groups.push(Vec::new());
        } else if word.contains(['|', ';', '&', '<', '>', '`']) || word.contains("$(") {
            return false;
        } else if let Some(group) = groups.last_mut() {
            group.push(word);
        }
    }
    let Some((last, prefixes)) = groups.split_last() else {
        return false;
    };
    if prefixes.iter().any(|group| group.first() != Some(&"cd")) {
        return false;
    }
    let Some((program, rest)) = last.split_first() else {
        return false;
    };
    let (options, operands): (Vec<&str>, Vec<&str>) =
        rest.iter().partition(|word| word.starts_with('-'));
    if *program == "sed" {
        return options.contains(&"-n")
            && !options
                .iter()
                .any(|option| option.starts_with("-i") || option.starts_with("--in-place"))
            && operands.len() >= 2
            && is_print_script(operands[0]);
    }
    PRINT_PROGRAMS.contains(program) && !operands.is_empty()
}

/// Matches a `sed` script made only of addresses and the `p` command.
fn is_print_script(script: &str) -> bool {
    script.strip_suffix('p').is_some_and(|addresses| {
        addresses
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, ',' | '$' | ' '))
    })
}

/// Splits `command` into words with POSIX shell quoting, like Python's
/// `shlex.split`; `None` on an unterminated quote or trailing backslash.
fn split_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        // Inside double quotes only the quote and the backslash
                        // itself can be escaped; any other pair stays literal.
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\') => word.push(c),
                            c => {
                                word.push('\\');
                                word.push(c);
                            }
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                word.push(chars.next()?);
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prints_of_local_files_qualify() {
        for command in [
            "cat page.html",
            "cat a.html b.html",
            "head -n 50 page.html",
            "tail -n +5 page.html",
            "nl -ba page.html",
            "bat --style=plain page.html",
            "cd src && cat page.html",
            "cd a && cd b && cat page.html",
            "sed -n '1,80p' page.html",
            "sed -n -e 5p page.html",
            "cat 'my page.html'",
            "cat \"q\\\"x.html\"",
            "cat my\\ page.html",
            "\ncat page.html\n",
        ] {
            assert!(prints_local_files(command), "{command:?}");
        }
    }

    #[test]
    fn anything_beyond_a_plain_print_keeps_command_output() {
        for command in [
            "",
            "cat",
            "cat -",
            "/bin/cat page.html",
            "LC_ALL=C cat page.html",
            "cat page.html | grep div",
            "cat page.html > copy.html",
            "cat < page.html",
            "cat <<EOF\n<p>hi</p>\nEOF",
            "cat page.html; ls",
            "cat page.html\nls",
            "cat page.html && ls",
            "ls && cat page.html",
            "cat $(ls *.html)",
            "cat `ls *.html`",
            "curl https://example.com/page.html",
            "sed -i 's/a/b/' page.html",
            "sed -n 's/a/b/p' page.html",
            "sed -n '1,5p'",
            "cat 'page.html",
            "cat page.html\\",
            "cat \"page.html",
        ] {
            assert!(!prints_local_files(command), "{command:?}");
        }
    }

    #[test]
    fn word_splitting_follows_posix_quoting() {
        assert_eq!(
            split_words("a\"b\"c d"),
            Some(vec!["abc".into(), "d".into()])
        );
        assert_eq!(split_words("'' x"), Some(vec!["".into(), "x".into()]));
        assert_eq!(split_words("\"a\\$b\""), Some(vec!["a\\$b".into()]));
        assert_eq!(split_words("'a\\nb'"), Some(vec!["a\\nb".into()]));
        assert_eq!(split_words("a\\ b"), Some(vec!["a b".into()]));
        assert_eq!(split_words("a\\"), None);
        assert_eq!(split_words("\"a\\"), None);
    }
}
