//! Native-find ownership through the bundled RTK's built-in transparent prefixes.
//!
//! Custom `hooks.transparent_prefixes` are deliberately not read or inferred.

use super::{command_basename, is_environment_assignment};

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

/// Recognizes the inner executable without changing build/test wrapper policy.
pub(super) fn invokes_find(words: &[String]) -> bool {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        if is_environment_assignment(word)
            || matches!(
                command_basename(word),
                "noglob" | "command" | "builtin" | "exec" | "nocorrect"
            )
        {
            index += 1;
        } else if command_basename(word) == "env" {
            index += 1;
            while let Some(word) = words.get(index) {
                if is_environment_assignment(word) {
                    index += 1;
                } else if word.starts_with('-') && word.as_str() != "-" {
                    index += 1 + usize::from(matches!(word.as_str(), "-u" | "--unset"));
                } else {
                    break;
                }
            }
        } else if word == "uv" && words.get(index + 1).is_some_and(|arg| arg == "run") {
            index += 2;
        } else if let Some(wrapper) = PROCESS_WRAPPERS
            .iter()
            .find(|wrapper| wrapper.name == command_basename(word))
        {
            let Some(inner) = wrapper_inner_index(wrapper, &words[index..]) else {
                return false;
            };
            index += inner;
        } else {
            break;
        }
    }
    words
        .get(index)
        .is_some_and(|word| command_basename(word) == "find")
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
