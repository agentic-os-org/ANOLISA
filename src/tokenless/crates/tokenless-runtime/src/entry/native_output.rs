//! Native output ownership through the bundled RTK's built-in transparent prefixes.
//!
//! Custom `hooks.transparent_prefixes` are deliberately not read or inferred.

use std::borrow::Cow;

use super::{
    command_basename, is_environment_assignment, normalized_exclude_executable, npx_runner_index,
};

struct ProcessWrapper {
    name: &'static str,
    value_opts: &'static [&'static str],
    flag_opts: &'static [&'static str],
    attached_opts: &'static [&'static str],
    positionals: usize,
    numeric_opts: bool,
}

// Mirror discover/registry.rs at the pinned RTK v0.49.0 revision. Unknown
// options must stop recognition rather than consume a possible executable.
const PROCESS_WRAPPERS: &[ProcessWrapper] = &[
    ProcessWrapper {
        name: "timeout",
        value_opts: &["-s", "-k", "--signal", "--kill-after"],
        flag_opts: &["--preserve-status", "--foreground", "-v", "--verbose"],
        attached_opts: &["-s", "-k"],
        positionals: 1,
        numeric_opts: false,
    },
    ProcessWrapper {
        name: "time",
        value_opts: &["-f", "-o", "--format", "--output"],
        flag_opts: &[
            "-p",
            "-a",
            "-v",
            "--append",
            "--verbose",
            "--portability",
            "--quiet",
        ],
        attached_opts: &["-f", "-o"],
        positionals: 0,
        numeric_opts: false,
    },
    ProcessWrapper {
        name: "nice",
        value_opts: &["-n", "--adjustment"],
        flag_opts: &[],
        attached_opts: &["-n"],
        positionals: 0,
        numeric_opts: true,
    },
    ProcessWrapper {
        name: "nohup",
        value_opts: &[],
        flag_opts: &[],
        attached_opts: &[],
        positionals: 0,
        numeric_opts: false,
    },
];

const MAX_ENV_SPLIT_DEPTH: usize = 4;

/// Borrowed or `-S`-expanded command view with the executable at `index`.
pub(super) struct UnwrappedCommand<'a> {
    words: Cow<'a, [String]>,
    index: usize,
}

impl<'a> UnwrappedCommand<'a> {
    pub(super) fn raw_executable(&self) -> &str {
        &self.words[self.index]
    }

    pub(super) fn executable(&self) -> &str {
        command_basename(&self.words[self.index])
    }

    pub(super) fn args(&self) -> &[String] {
        &self.words[(self.index + 1).min(self.words.len())..]
    }
}

enum EnvShortCluster<'a> {
    FlagOnly,
    ConsumesNextWord,
    SplitInline(&'a str),
    SplitNextWord,
}

enum EnvStep<'a> {
    Advance(usize),
    Expand { raw: &'a str, tail_start: usize },
}

fn is_npx_wrapper(word: &str) -> bool {
    let exe = normalized_exclude_executable(word);
    exe.eq_ignore_ascii_case("npx") || exe.eq_ignore_ascii_case("pnpx")
}

/// Identifies native search executables through the same transparent wrappers.
pub(super) fn is_search(words: &[String]) -> bool {
    unwrap_transparent_prefix(words).is_some_and(|cmd| matches!(cmd.executable(), "grep" | "rg"))
}

/// Invokes `predicate` at each command position reached while stripping
/// transparent wrappers (including wrapper-qualified forms such as
/// `uv run pytest` and the final unwrapped command), stopping on the first
/// match. Leading `VAR=val` assignments are skipped before each check.
pub(super) fn any_unwrapped_command(
    words: &[String],
    mut predicate: impl FnMut(&str, &[String]) -> bool,
) -> bool {
    let mut current = Cow::Borrowed(words);
    let mut index = 0;
    let mut split_depth = 0;
    while let Some(word) = current.get(index) {
        if is_environment_assignment(word) {
            index += 1;
            continue;
        }
        let rest = &current[(index + 1).min(current.len())..];
        if predicate(word, rest) {
            return true;
        }
        if matches!(
            command_basename(word),
            "noglob" | "command" | "builtin" | "exec" | "nocorrect"
        ) {
            let accepts_end_of_options =
                matches!(command_basename(word), "command" | "builtin" | "exec");
            index += 1;
            if accepts_end_of_options && current.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            }
        } else if command_basename(word) == "env" {
            index += 1;
            while index < current.len() {
                let step = {
                    let word = current[index].as_str();
                    if is_environment_assignment(word) {
                        EnvStep::Advance(1)
                    } else if word == "--" {
                        index += 1;
                        break;
                    } else if word == "--split-string" {
                        let Some(raw) = current.get(index + 1) else {
                            return false;
                        };
                        EnvStep::Expand {
                            raw: raw.as_str(),
                            tail_start: index + 2,
                        }
                    } else if let Some(raw) = word.strip_prefix("--split-string=") {
                        EnvStep::Expand {
                            raw,
                            tail_start: index + 1,
                        }
                    } else if word.starts_with("--") {
                        EnvStep::Advance(1 + usize::from(matches!(word, "--unset" | "--chdir")))
                    } else if let Some(cluster) = word.strip_prefix('-').filter(|r| !r.is_empty()) {
                        match parse_env_short_cluster(cluster) {
                            EnvShortCluster::FlagOnly => EnvStep::Advance(1),
                            EnvShortCluster::ConsumesNextWord => EnvStep::Advance(2),
                            EnvShortCluster::SplitInline(raw) => EnvStep::Expand {
                                raw,
                                tail_start: index + 1,
                            },
                            EnvShortCluster::SplitNextWord => {
                                let Some(raw) = current.get(index + 1) else {
                                    return false;
                                };
                                EnvStep::Expand {
                                    raw: raw.as_str(),
                                    tail_start: index + 2,
                                }
                            }
                        }
                    } else {
                        break;
                    }
                };
                match step {
                    EnvStep::Advance(delta) => index += delta,
                    EnvStep::Expand { raw, tail_start } => {
                        if split_depth >= MAX_ENV_SPLIT_DEPTH {
                            return false;
                        }
                        let Some(mut expanded) = split_env_s_words(raw) else {
                            return false;
                        };
                        expanded.extend_from_slice(&current[tail_start..]);
                        current = Cow::Owned(expanded);
                        index = 0;
                        split_depth += 1;
                    }
                }
            }
        } else if command_basename(word) == "uv"
            && current.get(index + 1).is_some_and(|arg| arg == "run")
        {
            index += 2;
        } else if is_npx_wrapper(word) {
            let Some(offset) = npx_runner_index(rest) else {
                return false;
            };
            index += 1 + offset;
        } else if let Some(wrapper) = PROCESS_WRAPPERS
            .iter()
            .find(|wrapper| wrapper.name == command_basename(word))
        {
            let Some(inner) = wrapper_inner_index(wrapper, &current[index..]) else {
                return false;
            };
            index += inner;
        } else {
            return false;
        }
    }
    false
}

/// Strips transparent wrappers and returns the resolved executable and trailing arguments.
pub(super) fn unwrap_transparent_prefix(words: &[String]) -> Option<UnwrappedCommand<'_>> {
    let mut current = Cow::Borrowed(words);
    let mut index = 0;
    let mut split_depth = 0;
    while let Some(word) = current.get(index) {
        if is_environment_assignment(word)
            || matches!(command_basename(word), "noglob" | "nocorrect")
        {
            index += 1;
        } else if matches!(command_basename(word), "command" | "builtin" | "exec") {
            index += 1;
            if current.get(index).is_some_and(|arg| arg == "--") {
                index += 1;
            }
        } else if command_basename(word) == "env" {
            index += 1;
            while index < current.len() {
                let step = {
                    let word = current[index].as_str();
                    if is_environment_assignment(word) {
                        EnvStep::Advance(1)
                    } else if word == "--" {
                        index += 1;
                        break;
                    } else if word == "--split-string" {
                        EnvStep::Expand {
                            raw: current.get(index + 1)?.as_str(),
                            tail_start: index + 2,
                        }
                    } else if let Some(raw) = word.strip_prefix("--split-string=") {
                        EnvStep::Expand {
                            raw,
                            tail_start: index + 1,
                        }
                    } else if word.starts_with("--") {
                        EnvStep::Advance(1 + usize::from(matches!(word, "--unset" | "--chdir")))
                    } else if let Some(cluster) = word.strip_prefix('-').filter(|r| !r.is_empty()) {
                        match parse_env_short_cluster(cluster) {
                            EnvShortCluster::FlagOnly => EnvStep::Advance(1),
                            EnvShortCluster::ConsumesNextWord => EnvStep::Advance(2),
                            EnvShortCluster::SplitInline(raw) => EnvStep::Expand {
                                raw,
                                tail_start: index + 1,
                            },
                            EnvShortCluster::SplitNextWord => EnvStep::Expand {
                                raw: current.get(index + 1)?.as_str(),
                                tail_start: index + 2,
                            },
                        }
                    } else {
                        break;
                    }
                };
                match step {
                    EnvStep::Advance(delta) => index += delta,
                    EnvStep::Expand { raw, tail_start } => {
                        if split_depth >= MAX_ENV_SPLIT_DEPTH {
                            return None;
                        }
                        let mut expanded = split_env_s_words(raw)?;
                        expanded.extend_from_slice(&current[tail_start..]);
                        current = Cow::Owned(expanded);
                        index = 0;
                        split_depth += 1;
                    }
                }
            }
        } else if command_basename(word) == "uv"
            && current.get(index + 1).is_some_and(|arg| arg == "run")
        {
            index += 2;
        } else if let Some(wrapper) = PROCESS_WRAPPERS
            .iter()
            .find(|wrapper| wrapper.name == command_basename(word))
        {
            let inner = wrapper_inner_index(wrapper, &current[index..])?;
            index += inner;
        } else {
            break;
        }
    }
    current.get(index)?;
    Some(UnwrappedCommand {
        words: current,
        index,
    })
}

fn parse_env_short_cluster(cluster: &str) -> EnvShortCluster<'_> {
    for (offset, ch) in cluster.char_indices() {
        match ch {
            // Value-less GNU/BSD `env` short flags that may precede `S`, `u`, `C`, or `P`.
            'i' | '0' | 'v' => continue,
            'S' => {
                let rest = &cluster[offset + ch.len_utf8()..];
                return if rest.is_empty() {
                    EnvShortCluster::SplitNextWord
                } else {
                    EnvShortCluster::SplitInline(rest)
                };
            }
            'u' | 'C' | 'P' => {
                let rest = &cluster[offset + ch.len_utf8()..];
                return if rest.is_empty() {
                    EnvShortCluster::ConsumesNextWord
                } else {
                    EnvShortCluster::FlagOnly
                };
            }
            _ => return EnvShortCluster::FlagOnly,
        }
    }
    EnvShortCluster::FlagOnly
}

/// Tokenizes GNU `env -S` / `--split-string` operands (unquoted space/tab and `\_` separators,
/// quotes, backslash escapes including `\c` stop, and `#` comments).
///
/// `${VARNAME}` variable expansion and bare-`$` rejection are deliberately not mirrored because
/// static command recognition inspects wrapper prefixes without host env state.
fn split_env_s_words(raw: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '#' if !in_word => break,
            '\'' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => word.push(ch),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => word.push(decode_env_s_escape(chars.next()?)),
                        ch => word.push(ch),
                    }
                }
            }
            '\\' => match chars.next()? {
                // Outside quotes, GNU `env -S` treats `\_` as an argument separator and `\c`
                // as end-of-input; inside double quotes both pass through `decode_env_s_escape`.
                '_' => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                'c' => break,
                ch => {
                    in_word = true;
                    word.push(decode_env_s_escape(ch));
                }
            },
            ch => {
                in_word = true;
                word.push(ch);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Some(words)
}

fn decode_env_s_escape(ch: char) -> char {
    match ch {
        '_' => ' ',
        't' => '\t',
        'n' => '\n',
        'r' => '\r',
        'f' => '\x0c',
        'v' => '\x0b',
        other => other,
    }
}

fn wrapper_inner_index(wrapper: &ProcessWrapper, words: &[String]) -> Option<usize> {
    let mut index = 1;
    let mut options_done = false;
    let mut positionals = wrapper.positionals;
    loop {
        let arg = words.get(index)?.as_str();
        if !options_done && arg == "--" {
            options_done = true;
            index += 1;
            continue;
        }
        if !options_done && wrapper.numeric_opts && is_numeric_option(arg) {
            index += 1;
            continue;
        }
        if !options_done && arg.starts_with('-') && arg != "-" {
            if wrapper.flag_opts.contains(&arg) || takes_attached_value(wrapper, arg) {
                index += 1;
                continue;
            }
            if wrapper.value_opts.contains(&arg) {
                words.get(index + 1)?;
                index += 2;
                continue;
            }
            return None;
        }
        if positionals > 0 {
            positionals -= 1;
            index += 1;
            continue;
        }
        // RTK refuses a wrapper whose options already refer to RTK itself.
        return (!words[..index].iter().any(|word| word == "rtk")).then_some(index);
    }
}

fn is_numeric_option(arg: &str) -> bool {
    let Some(digits) = arg.strip_prefix('-').or_else(|| arg.strip_prefix('+')) else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|c| c.is_ascii_digit())
}

fn takes_attached_value(wrapper: &ProcessWrapper, arg: &str) -> bool {
    if let Some((name, _)) = arg.split_once('=') {
        return wrapper.value_opts.contains(&name);
    }
    wrapper
        .attached_opts
        .iter()
        .any(|opt| arg.len() > opt.len() && arg.starts_with(opt))
}
