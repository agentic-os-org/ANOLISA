//! Recognizes shell commands whose only effect is printing local files.
//!
//! Such output is reported as `file_read`: data such as JSON, CSV, build logs
//! and diffs still compresses, but a printed HTML page stays verbatim because
//! it is source the agent may edit.

/// Programs that only print their operands; `sed` qualifies separately when it
/// runs in quiet mode with print-only scripts such as
/// `sed -n '1,80p' page.html`.
const PRINT_PROGRAMS: [&str; 7] = ["cat", "head", "tail", "nl", "less", "more", "bat"];

/// Long `sed` options that never change what is printed, so a quiet
/// print-only read may carry them.
const SED_HARMLESS_LONGS: [&str; 7] = [
    "separate",
    "unbuffered",
    "null-data",
    "regexp-extended",
    "follow-symlinks",
    "posix",
    "debug",
];

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
    if *program == "sed" {
        return sed_prints_local_files(rest);
    }
    let operands: Vec<&str> = rest
        .iter()
        .filter(|word| !word.starts_with('-'))
        .copied()
        .collect();
    PRINT_PROGRAMS.contains(program) && !operands.is_empty()
}

/// Parses the words after `sed` the way its option parser does and returns
/// whether they describe a quiet, print-only read of at least one real file.
///
/// Position matters: `-n`, `--quiet` and `--silent` set quiet mode wherever
/// they appear; `-e` and `--expression` take their script from the rest of a
/// short cluster, from after `=`, or from the next word; `-f` and `--file`
/// name a script file whose content cannot be verified; `-i` and `--in-place`
/// mutate the operand; `--` ends the options; and with no `-e`/`-f` at all
/// the first operand is the script and the rest are files. Every script must
/// be print-only and at least one file must remain, so stdin (`-`), a
/// dangling option argument and an unknown option all keep the command
/// classified as command output.
fn sed_prints_local_files(rest: &[&str]) -> bool {
    let mut quiet = false;
    let mut scripts: Vec<&str> = Vec::new();
    let mut operands: Vec<&str> = Vec::new();
    let mut script_flag = false;
    let mut operands_only = false;
    let mut index = 0;
    while index < rest.len() {
        let word = rest[index];
        index += 1;
        if operands_only || !word.starts_with('-') || word == "-" {
            if word == "-" {
                return false; // stdin, not a local file, even after `--`
            }
            operands.push(word);
            continue;
        }
        if word == "--" {
            operands_only = true;
            continue;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, value) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            match name {
                "quiet" | "silent" => quiet = true,
                "expression" => {
                    let script = match value {
                        Some(script) => script,
                        None => match rest.get(index) {
                            Some(script) => {
                                index += 1;
                                script
                            }
                            None => return false, // dangling option argument
                        },
                    };
                    scripts.push(script);
                    script_flag = true;
                }
                "file" | "in-place" => return false,
                name if SED_HARMLESS_LONGS.contains(&name) => {}
                _ => return false, // unknown long option
            }
            continue;
        }
        // Short cluster: letters apply in order until -e/-f, which take the
        // rest of the cluster (or the next word) as their argument.
        let cluster = &word[1..];
        let bytes = cluster.as_bytes();
        let mut position = 0;
        while position < bytes.len() {
            match bytes[position] {
                b'n' => quiet = true,
                b's' | b'u' | b'z' | b'r' | b'E' => {}
                b'i' => return false, // in-place, with or without a suffix
                b'e' | b'f' => {
                    let argument = if position + 1 < bytes.len() {
                        &cluster[position + 1..]
                    } else {
                        match rest.get(index) {
                            Some(argument) => {
                                index += 1;
                                argument
                            }
                            None => return false, // dangling option argument
                        }
                    };
                    if bytes[position] == b'f' {
                        return false; // script file: cannot verify its content
                    }
                    scripts.push(argument);
                    script_flag = true;
                    break; // the rest of the cluster was the argument
                }
                _ => return false, // unknown or output-shaping short option
            }
            position += 1;
        }
    }
    // Without -e/-f the first operand is the script; otherwise every
    // operand is a file.
    let mut files = operands;
    if !script_flag && !files.is_empty() {
        scripts.push(files.remove(0));
    }
    quiet && !files.is_empty() && scripts.iter().all(|script| is_print_script(script))
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
    fn sed_quiet_spellings_and_clusters_qualify() {
        for command in [
            "sed --quiet '1,80p' page.html",
            "sed --silent '1,80p' page.html",
            "sed -ne '5p' page.html",
            "sed -n -e 5p -e 6p page.html",
            "sed -n --expression='1,80p' page.html",
            "sed --quiet --expression '1,80p' page.html",
            "sed -n -- '1,80p' page.html",
            "sed -n '1,80p' -- page.html",
            "sed -sn '1,80p' page.html",
        ] {
            assert!(prints_local_files(command), "{command:?}");
        }
    }

    #[test]
    fn sed_forms_beyond_a_plain_print_stay_command_output() {
        for command in [
            // Only the first script operand was verified, so a substituting
            // second script rode along as file_read.
            "sed -n -e 5p -e 's/a/b/p' page.html",
            "sed -ne 's/a/b/p' page.html",
            // Script files cannot be verified, in-place edits mutate the
            // operand, stdin is not a local file.
            "sed -f script.sed page.html",
            // GNU sed still reads standard input for `-`, even after `--`
            // ends the options; neither form reads a local file.
            "sed -n -- 1p -",
            "sed -n --expression=1p -- -",
            "sed -n -f script.sed page.html",
            "sed --file=script.sed page.html",
            "sed --file script.sed page.html",
            "sed -in 's/a/b/' page.html",
            "sed -n '1,80p' -",
            // -e consumes the rest of its cluster, so the script is `n`
            // here and `5p` is a file operand.
            "sed -en '5p' page.html",
            "sed --expression= page.html",
            "sed -n -e",
            "sed -n --unknown-flag '1,80p' page.html",
            // -l takes an argument, so the cluster is not quiet-only.
            "sed -nl '1,80p' page.html",
            // With -e given, every operand is a file and there is no -n.
            "sed 5p -e 6p page.html",
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
