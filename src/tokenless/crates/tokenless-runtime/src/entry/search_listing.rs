//! Proves from shell arguments that search output rows start with prefixes the search wrote.
//!
//! Without line numbers or a file-listing mode, `grep` and `rg` print matching file
//! text verbatim, and that text can itself look like `path:12:...` rows, as in a
//! build log searched for `error`. Content cannot tell the two apart, so a listing
//! is returned only when every option, redirection and pipeline stage is understood.
//! Comments are skipped as the shell skips them, so `grep error build.log # -n`
//! still prints no line numbers.
//!
//! The option tables assume that nothing outside the command line changes row
//! shape. A configuration that does, such as `RIPGREP_CONFIG_PATH` adding
//! `--heading` or `--context`, produces rows the search compressor rejects, so it
//! loses savings rather than fidelity.

use super::{
    ShellSegment, ShellWord, command_basename, is_environment_assignment, native_output,
    shell_segments,
};

/// Row shape guaranteed by a search command's arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchListing {
    /// Every match row starts with the search's `line:` or `path:line:` prefix.
    Matches,
    /// Every row is a file name printed by the search.
    Paths,
}

/// Returns the listing that every output-producing pipeline of `command` guarantees.
///
/// Sequenced pipelines may only change directory, assign variables or produce
/// the same listing. Stages after the search may only select, order or limit
/// whole lines. Anything else, including unknown options, returns `None`.
pub(crate) fn search_listing(command: &str) -> Option<SearchListing> {
    let mut listing = None;
    for pipeline in pipelines(&shell_segments(command))? {
        if let Some(shape) = pipeline_listing(&pipeline)? {
            if listing.is_some_and(|known| known != shape) {
                return None;
            }
            listing = Some(shape);
        }
    }
    listing
}

/// Words of one pipeline stage, including its redirections.
type Stage<'a> = Vec<&'a ShellWord>;

/// Groups segments into pipelines of stages; `None` for subshells and substitutions.
fn pipelines(segments: &[ShellSegment]) -> Option<Vec<Vec<Stage<'_>>>> {
    let mut pipelines = Vec::new();
    let mut stages = Vec::new();
    let mut stage = Stage::new();
    let mut index = 0;
    while let Some(segment) = segments.get(index) {
        stage.extend(segment.words.iter());
        index += 1;
        let next = segments.get(index);
        // The tokenizer ends a segment at each `&` or `|`, so `&&` and `||`
        // arrive as an empty segment ending in the same character.
        let doubled = |separator| {
            next.is_some_and(|next: &ShellSegment| {
                next.words.is_empty() && next.separator == Some(separator)
            })
        };
        let skipped = match segment.separator {
            // `2>&1`, `>&2` and `&>FILE` continue the current command.
            Some('&') if segment.in_redirection => continue,
            Some('|') if doubled('|') => 1,
            Some('|') => {
                // `|&` also pipes standard error.
                index += usize::from(doubled('&'));
                stages.push(std::mem::take(&mut stage));
                continue;
            }
            Some('&') => usize::from(doubled('&')),
            Some(';' | '\n') | None => 0,
            Some(_) => return None,
        };
        index += skipped;
        if !stage.is_empty() || !stages.is_empty() {
            stages.push(std::mem::take(&mut stage));
            pipelines.push(std::mem::take(&mut stages));
        }
    }
    Some(pipelines)
}

/// Returns `Some(None)` for a pipeline that prints nothing and `None` when its
/// output is not understood.
fn pipeline_listing(stages: &[Stage<'_>]) -> Option<Option<SearchListing>> {
    let stages = stages
        .iter()
        .map(|stage| without_redirections(stage))
        .collect::<Option<Vec<_>>>()?;
    let (first, filters) = stages.split_first()?;
    if filters.is_empty() && is_silent(first) {
        return Some(None);
    }
    let (name, arguments) = native_output::search_invocation(first)?;
    let tool = SearchTool::named(name)?;
    let shape = tool.listing(&parse(tool, arguments)?)?;
    filters
        .iter()
        .all(|filter| keeps_lines(filter))
        .then_some(Some(shape))
}

/// Drops input and standard-error redirections; `None` when standard output
/// is redirected, because the search rows then go elsewhere.
///
/// A redirection glued to an argument, as in `src>out.txt` or `'2'>out.txt`,
/// also returns `None`: its default descriptor is standard output or input.
fn without_redirections(stage: &[&ShellWord]) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut stage = stage.iter();
    while let Some(word) = stage.next() {
        let Some(redirection) = word.redirection else {
            words.push(word.text.clone());
            continue;
        };
        let (descriptor, rest) = word.text.split_at(redirection.offset);
        if !redirection.plain_prefix || !descriptor.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let operator = ["<<<", "<<", "<>", "<", ">>", ">"]
            .into_iter()
            .find(|operator| rest.starts_with(operator))?;
        let writes_output = match operator {
            ">" | ">>" => matches!(descriptor, "" | "1"),
            "<>" => descriptor == "1",
            _ => false,
        };
        if writes_output {
            return None;
        }
        if rest == operator {
            stage.next();
        }
    }
    Some(words)
}

/// Recognizes commands that print nothing: variable assignments, `cd` other
/// than `cd -`, and `export` of named variables.
fn is_silent(words: &[String]) -> bool {
    match words.split_first() {
        Some((command, arguments)) if command == "cd" => {
            !arguments.iter().any(|argument| argument == "-")
        }
        // Without operands, or with `-p`, `export` lists the environment.
        Some((command, arguments)) if command == "export" => {
            !arguments.is_empty() && !arguments.iter().any(|argument| argument.starts_with('-'))
        }
        _ => words.iter().all(|word| is_environment_assignment(word)),
    }
}

/// Recognizes stages that select, order or limit whole lines from standard input.
fn keeps_lines(words: &[String]) -> bool {
    let Some((command, arguments)) = words.split_first() else {
        return false;
    };
    match command_basename(command) {
        "head" | "tail" => line_limit_arguments(arguments),
        "sort" => sort_arguments(arguments),
        "uniq" => arguments.iter().all(|argument| {
            matches!(
                argument.as_str(),
                "-d" | "-u" | "-i" | "--repeated" | "--unique" | "--ignore-case"
            )
        }),
        "cat" => arguments.is_empty(),
        name => SearchTool::named(name).is_some_and(|tool| {
            parse(tool, arguments).is_some_and(|invocation| tool.filters(&invocation))
        }),
    }
}

/// Accepts only line or byte counts, so no file operand replaces standard input.
///
/// A byte count can cut the first or last row. The search compressor then
/// rejects a row without a line number or names a truncated path in a header;
/// both views keep the received bytes.
fn line_limit_arguments(arguments: &[String]) -> bool {
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let argument = argument.as_str();
        let valid = if matches!(argument, "-n" | "-c" | "--lines" | "--bytes") {
            arguments.next().is_some()
        } else {
            argument.starts_with("--lines=")
                || argument.starts_with("--bytes=")
                || argument.strip_prefix('-').is_some_and(|count| {
                    (!count.is_empty() && count.bytes().all(|byte| byte.is_ascii_digit()))
                        || count
                            .strip_prefix(['n', 'c'])
                            .is_some_and(|value| !value.is_empty())
                })
        };
        if !valid {
            return false;
        }
    }
    true
}

/// Accepts ordering options without `-o`, `-m` or file operands.
fn sort_arguments(arguments: &[String]) -> bool {
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        let valid = match argument.as_str() {
            "-t" | "-k" | "--key" | "--field-separator" => arguments.next().is_some(),
            "--reverse"
            | "--unique"
            | "--numeric-sort"
            | "--general-numeric-sort"
            | "--human-numeric-sort"
            | "--version-sort"
            | "--ignore-case"
            | "--stable" => true,
            long if long.starts_with("--key=") || long.starts_with("--field-separator=") => true,
            short => match short.strip_prefix('-') {
                Some(cluster) if !cluster.is_empty() && !cluster.starts_with('-') => {
                    let mut valid = true;
                    for (position, flag) in cluster.char_indices() {
                        if matches!(flag, 't' | 'k') {
                            // The value is attached or follows as the next word.
                            if position + 1 == cluster.len() {
                                valid = arguments.next().is_some();
                            }
                            break;
                        }
                        if !"bdfgiMhnRrsuV".contains(flag) {
                            valid = false;
                            break;
                        }
                    }
                    valid
                }
                _ => false,
            },
        };
        if !valid {
            return false;
        }
    }
    true
}

/// Option grammar and output effects of one search executable.
///
/// Unknown options reject the command: a value-taking option misread as a
/// flag cluster (`rg -tjson`) could otherwise look like `-n`.
struct SearchTool {
    /// Short options without a value.
    flags: &'static str,
    /// Short options whose value is attached or in the next word.
    values: &'static str,
    long_flags: &'static [&'static str],
    long_values: &'static [&'static str],
    /// Long options that take a value only after `=`.
    long_optional_values: &'static [&'static str],
    /// GNU grep reads `-NUM` as a context length.
    numeric_context: bool,
    line_numbers: &'static [&'static str],
    no_line_numbers: &'static [&'static str],
    context: &'static [&'static str],
    paths: &'static [&'static str],
    /// Options that print neither numbered rows nor file names.
    other_output: &'static [&'static str],
    /// Options a line filter may use without rewriting the lines it keeps.
    filter_options: &'static [&'static str],
}

// GNU grep 3.x. Long options are matched exactly; getopt abbreviations are
// rejected rather than resolved.
const GREP: SearchTool = SearchTool {
    flags: "EFGPiyvwxcLloqsbHhnTZzrRaIUV",
    values: "efmABCdD",
    long_flags: &[
        "extended-regexp",
        "fixed-strings",
        "basic-regexp",
        "perl-regexp",
        "ignore-case",
        "no-ignore-case",
        "invert-match",
        "word-regexp",
        "line-regexp",
        "count",
        "files-without-match",
        "files-with-matches",
        "only-matching",
        "quiet",
        "silent",
        "no-messages",
        "byte-offset",
        "with-filename",
        "no-filename",
        "line-number",
        "initial-tab",
        "null",
        "null-data",
        "recursive",
        "dereference-recursive",
        "text",
        "line-buffered",
        "binary",
        "no-group-separator",
        "version",
        "help",
    ],
    long_values: &[
        "regexp",
        "file",
        "max-count",
        "after-context",
        "before-context",
        "context",
        "directories",
        "devices",
        "include",
        "exclude",
        "exclude-from",
        "exclude-dir",
        "label",
        "binary-files",
        "group-separator",
    ],
    long_optional_values: &["color", "colour"],
    numeric_context: true,
    line_numbers: &["-n", "--line-number"],
    no_line_numbers: &[],
    context: &[
        "-A",
        "-B",
        "-C",
        "-NUM",
        "--after-context",
        "--before-context",
        "--context",
    ],
    paths: &["-l", "-L", "--files-with-matches", "--files-without-match"],
    other_output: &[
        "-c",
        "--count",
        "-q",
        "--quiet",
        "--silent",
        "-T",
        "--initial-tab",
        "-Z",
        "--null",
        "-z",
        "--null-data",
        "-V",
        "--version",
        "--help",
    ],
    filter_options: &[
        "-v",
        "-i",
        "-y",
        "-E",
        "-F",
        "-G",
        "-P",
        "-w",
        "-x",
        "-s",
        "-a",
        "-I",
        "-U",
        "-e",
        "-f",
        "-m",
        "--invert-match",
        "--ignore-case",
        "--no-ignore-case",
        "--extended-regexp",
        "--fixed-strings",
        "--basic-regexp",
        "--perl-regexp",
        "--word-regexp",
        "--line-regexp",
        "--no-messages",
        "--text",
        "--binary",
        "--line-buffered",
        "--regexp",
        "--file",
        "--max-count",
        "--color",
        "--colour",
    ],
};

// ripgrep 14. Line numbers are off unless requested, because hooks capture
// output without a terminal.
const RG: SearchTool = SearchTool {
    flags: "abcFHhIiLlNnoPpqSsUuVvwxz0.",
    values: "ABCdEefgjMmrTt",
    long_flags: &[
        "auto-hybrid-regex",
        "binary",
        "block-buffered",
        "byte-offset",
        "case-sensitive",
        "column",
        "count",
        "count-matches",
        "crlf",
        "debug",
        "files",
        "files-with-matches",
        "files-without-match",
        "fixed-strings",
        "follow",
        "glob-case-insensitive",
        "heading",
        "help",
        "hidden",
        "ignore-case",
        "ignore-file-case-insensitive",
        "include-zero",
        "invert-match",
        "json",
        "line-buffered",
        "line-number",
        "line-regexp",
        "max-columns-preview",
        "mmap",
        "multiline",
        "multiline-dotall",
        "no-auto-hybrid-regex",
        "no-binary",
        "no-block-buffered",
        "no-byte-offset",
        "no-column",
        "no-config",
        "no-context-separator",
        "no-crlf",
        "no-encoding",
        "no-filename",
        "no-fixed-strings",
        "no-follow",
        "no-glob-case-insensitive",
        "no-heading",
        "no-hidden",
        "no-ignore",
        "no-ignore-dot",
        "no-ignore-exclude",
        "no-ignore-files",
        "no-ignore-global",
        "no-ignore-messages",
        "no-ignore-parent",
        "no-ignore-vcs",
        "no-invert-match",
        "no-line-buffered",
        "no-line-number",
        "no-max-columns-preview",
        "no-messages",
        "no-mmap",
        "no-multiline",
        "no-multiline-dotall",
        "no-one-file-system",
        "no-pcre2",
        "no-pcre2-unicode",
        "no-pre",
        "no-require-git",
        "no-search-zip",
        "no-stats",
        "no-text",
        "no-trim",
        "no-unicode",
        "null",
        "null-data",
        "one-file-system",
        "only-matching",
        "passthru",
        "pcre2",
        "pcre2-version",
        "pretty",
        "quiet",
        "search-zip",
        "smart-case",
        "sort-files",
        "stats",
        "text",
        "trace",
        "trim",
        "type-list",
        "unrestricted",
        "version",
        "vimgrep",
        "with-filename",
        "word-regexp",
    ],
    long_values: &[
        "after-context",
        "before-context",
        "color",
        "colors",
        "context",
        "context-separator",
        "dfa-size-limit",
        "encoding",
        "engine",
        "field-context-separator",
        "field-match-separator",
        "file",
        "generate",
        "glob",
        "hostname-bin",
        "hyperlink-format",
        "iglob",
        "ignore-file",
        "max-columns",
        "max-count",
        "max-depth",
        "max-filesize",
        "path-separator",
        "pre",
        "pre-glob",
        "regex-size-limit",
        "regexp",
        "replace",
        "sort",
        "sortr",
        "threads",
        "type",
        "type-add",
        "type-clear",
        "type-not",
    ],
    long_optional_values: &[],
    numeric_context: false,
    line_numbers: &["-n", "--line-number", "--vimgrep"],
    no_line_numbers: &["-N", "--no-line-number"],
    context: &[
        "-A",
        "-B",
        "-C",
        "--after-context",
        "--before-context",
        "--context",
        "--passthru",
    ],
    paths: &[
        "-l",
        "--files",
        "--files-with-matches",
        "--files-without-match",
    ],
    other_output: &[
        "-c",
        "--count",
        "--count-matches",
        "--include-zero",
        "-h",
        "--help",
        "-V",
        "--version",
        "--pcre2-version",
        "-p",
        "--pretty",
        "--heading",
        "-q",
        "--quiet",
        "-U",
        "--multiline",
        "--multiline-dotall",
        "-0",
        "--null",
        "--null-data",
        "--json",
        "--stats",
        "--type-list",
        "--generate",
        "--debug",
        "--trace",
        "--field-match-separator",
        "--field-context-separator",
    ],
    filter_options: &[
        "-a",
        "-e",
        "-f",
        "-F",
        "-i",
        "-m",
        "-N",
        "-P",
        "-S",
        "-s",
        "-v",
        "-w",
        "-x",
        "--case-sensitive",
        "--color",
        "--engine",
        "--file",
        "--fixed-strings",
        "--ignore-case",
        "--invert-match",
        "--line-buffered",
        "--line-regexp",
        "--max-count",
        "--no-config",
        "--no-line-number",
        "--pcre2",
        "--regexp",
        "--smart-case",
        "--text",
        "--word-regexp",
    ],
};

/// Options in command-line order, spelled `-x` or `--name`, with any value.
struct Invocation {
    options: Vec<(String, Option<String>)>,
    operands: usize,
}

impl SearchTool {
    fn named(name: &str) -> Option<&'static Self> {
        match name {
            "grep" => Some(&GREP),
            "rg" => Some(&RG),
            _ => None,
        }
    }

    /// Returns the listing a search's options guarantee, if any.
    fn listing(&self, invocation: &Invocation) -> Option<SearchListing> {
        let mut line_numbers = false;
        let mut paths = false;
        for (name, value) in &invocation.options {
            let name = name.as_str();
            if self.context.contains(&name)
                || self.other_output.contains(&name)
                || colors_always(name, value.as_deref())
            {
                return None;
            }
            paths |= self.paths.contains(&name);
            if self.line_numbers.contains(&name) {
                line_numbers = true;
            } else if self.no_line_numbers.contains(&name) {
                line_numbers = false;
            }
        }
        if paths {
            Some(SearchListing::Paths)
        } else {
            line_numbers.then_some(SearchListing::Matches)
        }
    }

    /// Whether a search of standard input keeps or drops lines without rewriting them.
    fn filters(&self, invocation: &Invocation) -> bool {
        let patterns_from_options = invocation
            .options
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "-e" | "-f" | "--regexp" | "--file"));
        invocation.operands == usize::from(!patterns_from_options)
            && invocation.options.iter().all(|(name, value)| {
                self.filter_options.contains(&name.as_str())
                    && !colors_always(name, value.as_deref())
            })
    }
}

fn colors_always(name: &str, value: Option<&str>) -> bool {
    matches!(name, "--color" | "--colour") && matches!(value, Some("always" | "ansi"))
}

/// Parses options anywhere before `--`, as GNU getopt and ripgrep do.
fn parse(tool: &SearchTool, arguments: &[String]) -> Option<Invocation> {
    let mut options = Vec::new();
    let mut operands = 0;
    let mut options_done = false;
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if options_done || argument == "-" || !argument.starts_with('-') {
            operands += 1;
        } else if argument == "--" {
            options_done = true;
        } else if let Some(long) = argument.strip_prefix("--") {
            let (name, attached) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (long, None),
            };
            let value = if tool.long_values.contains(&name) {
                Some(match attached {
                    Some(value) => value,
                    None => arguments.next()?.clone(),
                })
            } else if tool.long_optional_values.contains(&name)
                || (tool.long_flags.contains(&name) && attached.is_none())
            {
                attached
            } else {
                return None;
            };
            options.push((format!("--{name}"), value));
        } else {
            let cluster = &argument[1..];
            for (position, flag) in cluster.char_indices() {
                if tool.numeric_context && flag.is_ascii_digit() {
                    options.push(("-NUM".to_owned(), None));
                } else if tool.flags.contains(flag) {
                    options.push((format!("-{flag}"), None));
                } else if tool.values.contains(flag) {
                    let attached = &cluster[position + flag.len_utf8()..];
                    let value = if attached.is_empty() {
                        arguments.next()?.clone()
                    } else {
                        attached.to_owned()
                    };
                    options.push((format!("-{flag}"), Some(value)));
                    break;
                } else {
                    return None;
                }
            }
        }
    }
    Some(Invocation { options, operands })
}

#[cfg(test)]
mod tests {
    use super::{SearchListing, search_listing};

    #[test]
    fn line_numbers_without_context_prove_numbered_rows() {
        for command in [
            "grep -n foo src/a.py",
            "grep -rn foo src",
            "grep -nr foo src",
            "grep -rnI --include='*.py' foo .",
            "grep --line-number --recursive foo src",
            "grep -rn -e foo -e bar src",
            "grep -rnm5 foo src",
            "grep -rn -m 5 foo src",
            "grep -rno foo src",
            "grep foo src -rn",
            "grep -rn -- -foo src",
            "grep -rn foo src 2>/dev/null",
            "grep -rn foo src 2> /dev/null",
            "grep -rn foo src 2>&1 | head -20",
            "grep -rn foo src 2>'err log'",
            "grep -rn foo src |& head -20",
            // Quoted operators are patterns, not redirections.
            "rg -n '=>' src",
            "rg -n '<' src",
            "grep -rn '>' src",
            "grep -rn '2>&1' src",
            "rg -n \\> src",
            // `#` starts a comment only at the start of an unquoted word.
            "grep -rn '#' src",
            "rg -n '#![a-z]' src",
            "grep -rn \\#include src",
            "grep -rn foo#bar src",
            "grep -rn '>'#x src",
            "grep -rn ''#x src",
            "rg -n $'>'#x src",
            // A quoted or escaped `#` after an operator is the target's text.
            "grep -rn foo src 2>'#err'",
            "grep -rn foo src 2>\"#err\"",
            "grep -rn foo src 2>\\#err",
            "grep -rn foo src 2>>'#err'",
            "grep -rn foo src 2>''#err",
            "grep -rn foo src <<<'#x'",
            "grep -rn foo src 2>&1#x",
            "grep -rn foo src 2>${v%%>#pat}",
            "grep -rn foo src # find callers",
            "grep -rn foo src;# find callers",
            "# find callers\ngrep -rn foo src",
            "grep -rn foo src | head -n 20 | tail -5",
            "grep -rn foo src | sort -t: -k1,1 | uniq",
            "grep -rn foo src | grep -v test",
            "grep -rn foo src | rg -v -e test",
            "grep -rn foo src | cat",
            "cd /repo && grep -rn foo src",
            "export LC_ALL=C && grep -rn foo src",
            "LC_ALL=C grep -rn foo src",
            "grep -rn foo src || grep -rn foo lib",
            "rg -n foo",
            "rg -n -tpy foo src",
            "rg --line-number --glob '*.rs' foo",
            "rg --vimgrep foo",
            "rg -N -n foo",
            "rg -n --no-heading -g '!target' foo",
            "env LANG=C timeout 30 rg -n match src | sort",
        ] {
            assert_eq!(
                search_listing(command),
                Some(SearchListing::Matches),
                "{command}"
            );
        }
    }

    #[test]
    fn file_listing_modes_prove_path_rows() {
        for command in [
            "grep -rl foo src",
            "grep -rL foo src",
            "grep -rln foo src",
            "grep --files-with-matches -r foo src",
            "rg -l foo",
            "rg --files",
            "rg --files -g '*.rs' | sort",
            "cd /repo && rg -l foo | head -50",
            "rg -l foo # list files",
        ] {
            assert_eq!(
                search_listing(command),
                Some(SearchListing::Paths),
                "{command}"
            );
        }
    }

    #[test]
    fn rows_that_may_be_file_text_prove_nothing() {
        for command in [
            // Without line numbers a search prints the matching file text.
            "grep error build.log",
            "grep -r foo src",
            "rg foo src",
            "rg -n -N foo",
            "rg -tjson foo",
            // Context rows, counts and other formats are not numbered listings.
            "grep -rn -C2 foo src",
            "grep -rn -A 3 foo src",
            "grep -rn -3 foo src",
            "rg -n --context 2 foo",
            "rg -n --passthru foo",
            "grep -rnc foo src",
            "grep -rnz foo src",
            "grep --color=always -rn foo src",
            "rg -n --json foo",
            "rg -n --heading foo",
            "rg -n -U foo",
            "grep -rnq foo src && grep -rn bar src",
            // Unknown or abbreviated options may change the output.
            "grep -rn --frobnicate foo src",
            "grep -rn --line-num foo src",
            "rg -n --frobnicate foo",
            // Standard output goes elsewhere or other commands write to it.
            "grep -rn foo src > out.txt",
            "grep -rn foo src >> out.txt",
            "grep -rn foo src 1>out.txt",
            "grep -rn foo src &> out.txt",
            "grep -rn foo src &>out.txt",
            "grep -rn foo src >&2",
            "grep -rn foo src 1>&2",
            "grep -rn foo src>out.txt",
            "grep -rn foo src1>out.txt",
            "grep -rn foo src>>out.txt",
            "grep -rn foo 'src'>out.txt",
            "grep -rn foo src 1<>out.txt",
            // A quoted or escaped digit is an argument, not a descriptor.
            "grep -rn foo '2'>out.txt",
            "grep -rn foo \"2\">>out.txt",
            "grep -rn foo '0'>out.txt",
            "grep -rn foo \\2>out.txt",
            // Options named in a comment do not apply.
            "grep error build.log # -n",
            "grep error build.log # --line-number",
            "grep error build.log # TODO: switch to rg -n",
            "grep error build.log # -n | head",
            "grep error build.log 2>/dev/null # -n",
            "env LANG=C timeout 30 grep error build.log # -n",
            "grep error build.log # -l",
            "rg error build.log # --vimgrep",
            // A redirection operator ends a word, so `#` after it starts a comment.
            "grep error build.log 2># -n",
            "grep error build.log <# -n",
            "grep error build.log 2>># -n",
            "grep error build.log 0<# -n",
            "grep error build.log 2<># -n",
            "grep error build.log >># -n",
            "grep error build.log &># -n",
            "grep error build.log foo>#bar -n",
            // Expansions are not parsed, so their `>#` keeps the rest of the line.
            "grep -n error build.log 2>${v%%>#pat} -C2",
            "grep -rn foo src && cargo build",
            "grep -rn foo src; cat notes.txt",
            // A quoted operator before `&&` does not join the next command.
            "rg -n '=>' && grep error build.log",
            "rg -n -trust 'fn() ->' && grep error build.log",
            "rg -n -e '->' && grep error build.log",
            "grep -rn src \"=>\" && grep error build.log",
            "rg -n '<' && grep error build.log",
            "rg -n '2>' && grep error build.log",
            "grep -rn '2>' src && grep error build.log",
            "rg -n '=>' ; grep error build.log",
            // A quoted operator does not consume the option after it.
            "rg -n '<' -C2 src",
            "grep -rn '2>' -A3 src",
            "cat build.log | grep -n error",
            "grep -rn foo src | xargs cat",
            "grep -rn foo src | head notes.txt",
            "grep -rn foo src | cut -d: -f1",
            "grep -rn foo src | grep -n bar",
            "grep -rn foo src | grep -o bar",
            "grep -rn foo src | grep bar notes.txt",
            "grep -rn foo src | sort -o sorted.txt",
            "grep -rn foo src | uniq -c",
            "grep -rn foo src | head -20 && echo done",
            "grep -rl foo src && grep -rn bar src",
            "cd - && grep -rn foo src",
            // `export` prints the environment without operands or with options.
            "export && grep -rn foo src",
            "export -p && grep -rn foo src",
            "set -e; grep -rn foo src",
            "source ./env.sh && grep -rn foo src",
            "grep -rn $(echo foo) src",
            "(grep -rn foo src)",
            "find . -name '*.rs'",
            "",
        ] {
            assert_eq!(search_listing(command), None, "{command}");
        }
    }
}
