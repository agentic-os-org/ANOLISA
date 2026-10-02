use super::{is_bounded_positive_count, is_safe_readonly_path};

pub(super) fn is_readonly_head(tokens: &[String]) -> bool {
    is_readonly_head_tail(tokens, false)
}

pub(super) fn is_readonly_tail(tokens: &[String]) -> bool {
    is_readonly_head_tail(tokens, true)
}

fn is_readonly_head_tail(tokens: &[String], is_tail: bool) -> bool {
    let mut idx = 1;
    let mut saw_path = false;
    let mut paths_only = false;

    while idx < tokens.len() {
        let token = tokens[idx].as_str();
        if paths_only {
            if !is_safe_readonly_path(token) {
                return false;
            }
            saw_path = true;
            idx += 1;
            continue;
        }

        match token {
            "--" => {
                paths_only = true;
                idx += 1;
            }
            "-q" | "-v" => idx += 1,
            "-n" | "-c" => {
                let Some(value) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_bounded_positive_count(value, 10_000) {
                    return false;
                }
                idx += 2;
            }
            "-f" if is_tail => return false,
            _ if token.starts_with("-n") || token.starts_with("-c") => {
                if !is_bounded_positive_count(&token[2..], 10_000) {
                    return false;
                }
                idx += 1;
            }
            _ if token.starts_with('-') && token[1..].chars().all(|ch| ch.is_ascii_digit()) => {
                if !is_bounded_positive_count(&token[1..], 10_000) {
                    return false;
                }
                idx += 1;
            }
            _ if token.starts_with('-') => return false,
            _ => {
                if !is_safe_readonly_path(token) {
                    return false;
                }
                saw_path = true;
                idx += 1;
            }
        }
    }

    saw_path
}

pub(super) fn is_readonly_grep(tokens: &[String]) -> bool {
    let mut idx = 1;
    let mut pattern_seen = false;
    let mut saw_path = false;
    let mut operands_only = false;

    while idx < tokens.len() {
        let token = tokens[idx].as_str();
        if operands_only {
            if !pattern_seen {
                pattern_seen = true;
            } else if !is_safe_readonly_path(token) {
                return false;
            } else {
                saw_path = true;
            }
            idx += 1;
            continue;
        }

        match token {
            "--" => {
                operands_only = true;
                idx += 1;
            }
            "-e" => {
                if tokens.get(idx + 1).is_none() {
                    return false;
                }
                pattern_seen = true;
                idx += 2;
            }
            "-A" | "-B" | "-C" | "-m" => {
                let Some(value) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_bounded_positive_count(value, 10_000) {
                    return false;
                }
                idx += 2;
            }
            _ if is_safe_grep_short_flags(token) => idx += 1,
            _ if token.starts_with("-A")
                || token.starts_with("-B")
                || token.starts_with("-C")
                || token.starts_with("-m") =>
            {
                if !is_bounded_positive_count(&token[2..], 10_000) {
                    return false;
                }
                idx += 1;
            }
            _ if token.starts_with('-') => return false,
            _ if !pattern_seen => {
                pattern_seen = true;
                idx += 1;
            }
            _ => {
                if !is_safe_readonly_path(token) {
                    return false;
                }
                saw_path = true;
                idx += 1;
            }
        }
    }

    pattern_seen && saw_path
}

fn is_safe_grep_short_flags(token: &str) -> bool {
    token.starts_with('-')
        && token.len() > 1
        && token[1..].chars().all(|ch| {
            matches!(
                ch,
                'n' | 'i' | 'H' | 'h' | 'E' | 'F' | 'w' | 'x' | 's' | 'l' | 'L' | 'c'
            )
        })
}

pub(super) fn is_readonly_rg(tokens: &[String]) -> bool {
    let mut idx = 1;
    let mut pattern_seen = false;
    let mut saw_files_mode = false;
    let mut operands_only = false;

    while idx < tokens.len() {
        let token = tokens[idx].as_str();
        if operands_only {
            if !pattern_seen && !saw_files_mode {
                pattern_seen = true;
            } else if !is_safe_readonly_path(token) {
                return false;
            }
            idx += 1;
            continue;
        }

        match token {
            "--" => {
                operands_only = true;
                idx += 1;
            }
            "--files" => {
                saw_files_mode = true;
                idx += 1;
            }
            "--line-number" | "--ignore-case" | "--smart-case" | "--case-sensitive"
            | "--fixed-strings" | "--word-regexp" | "--count" | "--no-heading"
            | "--with-filename" | "--hidden" => idx += 1,
            "-n" | "-i" | "-S" | "-s" | "-w" | "-x" | "-l" | "-c" => idx += 1,
            "-g" | "--glob" | "-t" | "--type" | "-T" | "--type-not" => {
                if tokens.get(idx + 1).is_none() {
                    return false;
                }
                idx += 2;
            }
            "-A" | "-B" | "-C" | "-m" | "--max-count" => {
                let Some(value) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_bounded_positive_count(value, 10_000) {
                    return false;
                }
                idx += 2;
            }
            _ if token.starts_with("-A")
                || token.starts_with("-B")
                || token.starts_with("-C")
                || token.starts_with("-m") =>
            {
                if !is_bounded_positive_count(&token[2..], 10_000) {
                    return false;
                }
                idx += 1;
            }
            _ if token.starts_with("--pre") => return false,
            _ if token.starts_with('-') => return false,
            _ if !pattern_seen && !saw_files_mode => {
                pattern_seen = true;
                idx += 1;
            }
            _ => {
                if !is_safe_readonly_path(token) {
                    return false;
                }
                idx += 1;
            }
        }
    }

    pattern_seen || saw_files_mode
}

pub(super) fn is_readonly_find(tokens: &[String]) -> bool {
    if tokens.len() == 1 {
        return false;
    }

    let mut idx = 1;
    while idx < tokens.len() {
        let token = tokens[idx].as_str();
        match token {
            "-print" | "-ls" => idx += 1,
            "-maxdepth" | "-mindepth" => {
                let Some(value) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_bounded_positive_count(value, 20) {
                    return false;
                }
                idx += 2;
            }
            "-name" | "-iname" | "-path" => {
                if tokens.get(idx + 1).is_none() {
                    return false;
                }
                idx += 2;
            }
            "-type" => {
                let Some(value) = tokens.get(idx + 1) else {
                    return false;
                };
                if !value
                    .chars()
                    .all(|ch| matches!(ch, 'f' | 'd' | 'l' | 's' | 'p'))
                {
                    return false;
                }
                idx += 2;
            }
            _ if token.starts_with('-') => return false,
            _ => {
                if !is_safe_readonly_path(token) {
                    return false;
                }
                idx += 1;
            }
        }
    }

    true
}

pub(super) fn is_readonly_ps(tokens: &[String]) -> bool {
    let mut idx = 1;
    while idx < tokens.len() {
        let token = tokens[idx].as_str();
        match token {
            "aux" | "-A" | "-a" | "-e" | "-f" | "-r" | "-u" | "-x" => idx += 1,
            "-ef" | "-aux" => idx += 1,
            "-o" => {
                let Some(fields) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_safe_ps_fields(fields) {
                    return false;
                }
                idx += 2;
            }
            "-Ao" => {
                let Some(fields) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_safe_ps_fields(fields) {
                    return false;
                }
                idx += 2;
            }
            _ if token.starts_with("-Ao") => {
                let fields = token.trim_start_matches("-Ao");
                if fields.is_empty() || !is_safe_ps_fields(fields) {
                    return false;
                }
                idx += 1;
            }
            _ => return false,
        }
    }

    true
}

fn is_safe_ps_fields(fields: &str) -> bool {
    fields.split(',').all(|field| {
        matches!(
            field,
            "pid"
                | "ppid"
                | "pcpu"
                | "pmem"
                | "rss"
                | "vsz"
                | "stat"
                | "state"
                | "time"
                | "etime"
                | "user"
                | "uid"
                | "comm"
                | "command"
        )
    })
}

pub(super) fn is_readonly_sysctl(tokens: &[String]) -> bool {
    if tokens.len() < 3 {
        return false;
    }
    if tokens[1] != "-n" {
        return false;
    }

    tokens.iter().skip(2).all(|key| {
        matches!(
            key.as_str(),
            "hw.ncpu"
                | "hw.logicalcpu"
                | "hw.physicalcpu"
                | "hw.memsize"
                | "hw.model"
                | "machdep.cpu.brand_string"
                | "machdep.cpu.core_count"
                | "machdep.cpu.thread_count"
                | "kern.osproductversion"
                | "kern.version"
        )
    })
}

pub(super) fn is_bounded_top_snapshot(tokens: &[String]) -> bool {
    if tokens.len() < 2 {
        return false;
    }

    let mut idx = 1;
    let mut linux_batch = false;
    let mut macos_single_sample = false;
    let mut top_count: Option<u16> = None;

    while idx < tokens.len() {
        let token = tokens[idx].as_str();
        match token {
            "-b" => {
                linux_batch = true;
                idx += 1;
            }
            "-l" => {
                let Some(val) = tokens.get(idx + 1) else {
                    return false;
                };
                if val != "1" {
                    return false;
                }
                macos_single_sample = true;
                idx += 2;
            }
            "-n" => {
                let Some(val) = tokens.get(idx + 1) else {
                    return false;
                };
                let Ok(count) = val.parse::<u16>() else {
                    return false;
                };
                if count == 0 || count > 100 {
                    return false;
                }
                top_count = Some(count);
                idx += 2;
            }
            "-s" | "-d" => {
                let Some(val) = tokens.get(idx + 1) else {
                    return false;
                };
                let Ok(delay) = val.parse::<u32>() else {
                    return false;
                };
                if delay > 5 {
                    return false;
                }
                idx += 2;
            }
            "-o" | "-stats" => {
                let Some(val) = tokens.get(idx + 1) else {
                    return false;
                };
                if !is_safe_top_field(val) {
                    return false;
                }
                idx += 2;
            }
            _ => return false,
        }
    }

    macos_single_sample || (linux_batch && top_count == Some(1))
}

fn is_safe_top_field(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | ',' | '%'))
}

pub(super) fn is_readonly_env(tokens: &[String]) -> bool {
    tokens.len() == 1
}

pub(super) fn is_readonly_git_stash(tokens: &[String]) -> bool {
    if tokens.len() < 3 {
        return false;
    }
    matches!(tokens[2].as_str(), "list" | "show")
}

pub(super) fn is_readonly_git_branch(tokens: &[String]) -> bool {
    let args = &tokens[2..];
    if args.is_empty() {
        return true;
    }

    let mut idx = 0;
    let mut saw_list_mode = false;
    while idx < args.len() {
        match args[idx].as_str() {
            "-a" | "-r" | "-l" | "--all" | "--remotes" | "--list" | "--no-color" => {
                saw_list_mode = true;
                idx += 1;
            }
            "--sort" | "--format" => {
                if args.get(idx + 1).is_none() {
                    return false;
                }
                saw_list_mode = true;
                idx += 2;
            }
            arg if arg.starts_with("--sort=") || arg.starts_with("--format=") => {
                saw_list_mode = true;
                idx += 1;
            }
            arg if arg.starts_with('-') => return false,
            arg => {
                if !saw_list_mode || !is_safe_readonly_path(arg) {
                    return false;
                }
                idx += 1;
            }
        }
    }

    true
}

pub(super) fn is_readonly_git_config(tokens: &[String]) -> bool {
    if tokens.len() < 3 {
        return false;
    }
    let args = &tokens[2..];
    match args[0].as_str() {
        "--get" | "--get-all" => args.len() >= 2,
        "--list" | "-l" => args.len() == 1,
        _ => false,
    }
}

pub(super) fn is_readonly_git_tag(tokens: &[String]) -> bool {
    if tokens.len() < 2 {
        return false;
    }
    let args = &tokens[2..];
    args.is_empty()
        || args
            .iter()
            .all(|a| matches!(a.as_str(), "-l" | "--list" | "-n" | "--sort" | "--column"))
}

pub(super) fn is_readonly_git_reflog(tokens: &[String]) -> bool {
    if tokens.len() < 3 {
        return false;
    }
    tokens[2] == "show"
}
