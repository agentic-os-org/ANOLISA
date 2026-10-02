use super::super::{ColumnBinding, ColumnSemantic, ColumnSpec, MemoryUnitFactors, TableSchema};

pub(in crate::hooks::linux_memory) fn split_tokens(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

pub(in crate::hooks::linux_memory) fn memory_target_program(command: &str) -> &str {
    let mut seen_env = false;
    let mut skip_next_env_arg = false;
    let mut after_sudo = false;
    let mut skip_next_sudo_arg = false;
    for token in command.split_whitespace() {
        if skip_next_env_arg {
            skip_next_env_arg = false;
            continue;
        }
        if skip_next_sudo_arg {
            skip_next_sudo_arg = false;
            continue;
        }
        if is_env_assignment_token(token) {
            continue;
        }
        let basename = token
            .rsplit_once('/')
            .map(|(_, name)| name)
            .unwrap_or(token);
        if basename == "sudo" {
            after_sudo = true;
            continue;
        }
        if after_sudo && is_sudo_option_token(token, &mut skip_next_sudo_arg) {
            continue;
        }
        if !seen_env && basename == "env" {
            seen_env = true;
            continue;
        }
        if seen_env {
            match token {
                "-i" | "--ignore-environment" | "-0" | "--null" | "--" => continue,
                "-u" | "--unset" | "-C" | "--chdir" => {
                    skip_next_env_arg = true;
                    continue;
                }
                _ => {}
            }
            if token.starts_with("--unset=")
                || token.starts_with("--chdir=")
                || token.starts_with("--argv0=")
            {
                continue;
            }
            if token.starts_with('-') {
                return "";
            }
        }
        return basename;
    }
    ""
}

pub(in crate::hooks::linux_memory) fn is_batch_top_command(command: &str) -> bool {
    if memory_target_program(command) != "top" {
        return false;
    }

    let mut seen_program = false;
    let mut after_sudo = false;
    let mut skip_next_sudo_arg = false;
    let mut skip_next_iteration_arg = false;
    let mut seen_batch = false;
    let mut seen_one_shot_iteration = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if skip_next_sudo_arg {
                skip_next_sudo_arg = false;
                continue;
            }
            if is_env_assignment_token(token) {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "sudo" {
                after_sudo = true;
                continue;
            }
            if after_sudo && is_sudo_option_token(token, &mut skip_next_sudo_arg) {
                continue;
            }
            if basename == "top" {
                seen_program = true;
            }
            continue;
        }

        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if skip_next_iteration_arg {
            seen_one_shot_iteration |= is_one_shot_top_iteration(token);
            skip_next_iteration_arg = false;
            continue;
        }
        if token == "--batch" {
            seen_batch = true;
            continue;
        }
        if matches!(token, "-n" | "--iterations") {
            skip_next_iteration_arg = true;
            continue;
        }
        if let Some(iterations) = token.strip_prefix("--iterations=") {
            seen_one_shot_iteration |= is_one_shot_top_iteration(iterations);
            continue;
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if short_options.starts_with('-') {
                continue;
            }
            if short_options.contains('b') {
                seen_batch = true;
            }
            if short_options == "n" {
                skip_next_iteration_arg = true;
                continue;
            }
            if let Some(n_pos) = short_options.find('n') {
                let iterations = &short_options[n_pos + 1..];
                if iterations.is_empty() {
                    skip_next_iteration_arg = true;
                } else {
                    seen_one_shot_iteration |= is_one_shot_top_iteration(iterations);
                }
            }
        }
    }

    seen_batch && seen_one_shot_iteration
}

pub(in crate::hooks::linux_memory) fn is_one_shot_top_iteration(value: &str) -> bool {
    value.trim_start_matches('=').trim() == "1"
}

pub(in crate::hooks::linux_memory) fn is_free_sampling_command(command: &str) -> bool {
    if memory_target_program(command) != "free" {
        return false;
    }

    let mut seen_program = false;
    let mut after_sudo = false;
    let mut skip_next_sudo_arg = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if skip_next_sudo_arg {
                skip_next_sudo_arg = false;
                continue;
            }
            if is_env_assignment_token(token) {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "sudo" {
                after_sudo = true;
                continue;
            }
            if after_sudo && is_sudo_option_token(token, &mut skip_next_sudo_arg) {
                continue;
            }
            if basename == "free" {
                seen_program = true;
            }
            continue;
        }

        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        if matches!(token, "-s" | "--seconds" | "-c" | "--count")
            || token.starts_with("--seconds=")
            || token.starts_with("--count=")
        {
            return true;
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if !short_options.starts_with('-')
                && (short_options.contains('s') || short_options.contains('c'))
            {
                return true;
            }
        }
    }
    false
}

pub(in crate::hooks::linux_memory) fn is_env_assignment_token(token: &str) -> bool {
    let Some(eq_pos) = token.find('=') else {
        return false;
    };
    if eq_pos == 0 {
        return false;
    }
    let name = &token[..eq_pos];
    name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && !name.bytes().next().unwrap_or(0).is_ascii_digit()
}

pub(in crate::hooks::linux_memory) fn is_sudo_option_token(
    token: &str,
    skip_next_arg: &mut bool,
) -> bool {
    match token {
        "--" => return true,
        "-u" | "-g" | "-h" | "-p" | "-C" | "-T" | "--user" | "--group" | "--host" | "--prompt"
        | "--close-from" | "--command-timeout" => {
            *skip_next_arg = true;
            return true;
        }
        "--askpass" | "--background" | "--edit" | "--help" | "--login" | "--non-interactive"
        | "--preserve-env" | "--reset-timestamp" | "--remove-timestamp" | "--shell" | "--stdin"
        | "--validate" | "--version" | "-A" | "-b" | "-E" | "-e" | "-H" | "-K" | "-k" | "-l"
        | "-n" | "-P" | "-S" | "-s" | "-V" | "-v" => return true,
        _ => {}
    }
    if token.starts_with("--user=")
        || token.starts_with("--group=")
        || token.starts_with("--host=")
        || token.starts_with("--prompt=")
        || token.starts_with("--close-from=")
        || token.starts_with("--command-timeout=")
        || token.starts_with("--preserve-env=")
    {
        return true;
    }
    if token.len() > 2 && matches!(&token[..2], "-u" | "-g" | "-h" | "-p" | "-C" | "-T") {
        return true;
    }
    token
        .strip_prefix('-')
        .filter(|opts| !opts.starts_with('-') && !opts.is_empty())
        .is_some_and(|opts| opts.chars().all(|ch| "AbEeHKklnPSsVv".contains(ch)))
}

impl<'a> ColumnSpec<'a> {
    pub(in crate::hooks::linux_memory) fn first(
        semantic: ColumnSemantic,
        aliases: &'a [&'a str],
    ) -> Self {
        Self {
            semantic,
            aliases,
            prefer_last: false,
        }
    }

    pub(in crate::hooks::linux_memory) fn last(
        semantic: ColumnSemantic,
        aliases: &'a [&'a str],
    ) -> Self {
        Self {
            semantic,
            aliases,
            prefer_last: true,
        }
    }
}

impl TableSchema {
    pub(in crate::hooks::linux_memory) fn index(&self, semantic: ColumnSemantic) -> Option<usize> {
        self.columns
            .iter()
            .find(|column| column.semantic == semantic)
            .map(|column| column.index)
    }
}

pub(in crate::hooks::linux_memory) fn detect_table_schema(
    tokens: &[&str],
    specs: &[ColumnSpec<'_>],
) -> TableSchema {
    let columns = specs
        .iter()
        .filter_map(|spec| {
            let matcher = |token: &&str| {
                spec.aliases
                    .iter()
                    .any(|alias| token.eq_ignore_ascii_case(alias))
            };
            let index = if spec.prefer_last {
                tokens.iter().rposition(matcher)
            } else {
                tokens.iter().position(matcher)
            }?;
            Some(ColumnBinding {
                semantic: spec.semantic,
                index,
                raw_name: tokens[index].to_string(),
            })
        })
        .collect();
    TableSchema { columns }
}

pub(in crate::hooks::linux_memory) fn free_unit_factors(command: &str) -> MemoryUnitFactors {
    let mut factors = MemoryUnitFactors {
        no_suffix: 1.0 / 1024.0,
        bare_suffix_mib: 1.0,
        bare_suffix_step: 1024.0,
    };
    let mut seen_program = false;
    let mut after_sudo = false;
    let mut skip_next_sudo_arg = false;
    for token in command.split_whitespace() {
        if !seen_program {
            if skip_next_sudo_arg {
                skip_next_sudo_arg = false;
                continue;
            }
            if is_env_assignment_token(token) {
                continue;
            }
            let basename = token
                .rsplit_once('/')
                .map(|(_, name)| name)
                .unwrap_or(token);
            if basename == "sudo" {
                after_sudo = true;
                continue;
            }
            if after_sudo && is_sudo_option_token(token, &mut skip_next_sudo_arg) {
                continue;
            }
            if basename == "free" {
                seen_program = true;
            }
            continue;
        }

        if matches!(token, "|" | ";" | "&&" | "||") {
            break;
        }
        match token {
            "--bytes" => factors.no_suffix = 1.0 / (1024.0 * 1024.0),
            "--kibi" => factors.no_suffix = 1.0 / 1024.0,
            "--mebi" => factors.no_suffix = 1.0,
            "--gibi" => factors.no_suffix = 1024.0,
            "--tebi" => factors.no_suffix = 1024.0 * 1024.0,
            "--pebi" => factors.no_suffix = 1024.0 * 1024.0 * 1024.0,
            "--kilo" => factors.no_suffix = 1000.0 / (1024.0 * 1024.0),
            "--mega" => factors.no_suffix = 1_000_000.0 / (1024.0 * 1024.0),
            "--giga" => factors.no_suffix = 1_000_000_000.0 / (1024.0 * 1024.0),
            "--tera" => factors.no_suffix = 1_000_000_000_000.0 / (1024.0 * 1024.0),
            "--peta" => factors.no_suffix = 1_000_000_000_000_000.0 / (1024.0 * 1024.0),
            "--si" => {
                factors.bare_suffix_mib = 1_000_000.0 / (1024.0 * 1024.0);
                factors.bare_suffix_step = 1000.0;
            }
            _ => {}
        }
        if let Some(short_options) = token.strip_prefix('-') {
            if short_options.starts_with('-') {
                continue;
            }
            if short_options.contains('b') {
                factors.no_suffix = 1.0 / (1024.0 * 1024.0);
            }
            if short_options.contains('k') {
                factors.no_suffix = 1.0 / 1024.0;
            }
            if short_options.contains('m') {
                factors.no_suffix = 1.0;
            }
            if short_options.contains('g') {
                factors.no_suffix = 1024.0;
            }
            if short_options.contains('t') {
                factors.no_suffix = 1024.0 * 1024.0;
            }
        }
    }

    factors
}

pub(in crate::hooks::linux_memory) fn command_from_tokens(
    tokens: &[&str],
    idx: usize,
    consume_tail: bool,
) -> Option<String> {
    if idx >= tokens.len() {
        return None;
    }
    if consume_tail {
        Some(tokens[idx..].join(" "))
    } else {
        Some(tokens[idx].to_string())
    }
}

pub(in crate::hooks::linux_memory) fn parse_float(value: &str) -> Option<f64> {
    value.trim_end_matches('%').parse::<f64>().ok()
}

pub(in crate::hooks::linux_memory) fn parse_u64(value: &str) -> Option<u64> {
    value.parse::<u64>().ok()
}

pub(in crate::hooks::linux_memory) fn parse_memory_to_mib(value: &str) -> Option<f64> {
    parse_memory_value_to_mib(
        value,
        MemoryUnitFactors {
            no_suffix: 1.0 / 1024.0,
            bare_suffix_mib: 1.0,
            bare_suffix_step: 1024.0,
        },
    )
}

pub(in crate::hooks::linux_memory) fn parse_free_memory_value_to_mib(
    value: &str,
    unit_factors: MemoryUnitFactors,
) -> Option<f64> {
    parse_memory_value_to_mib(value, unit_factors)
}

pub(in crate::hooks::linux_memory) fn free_output_uses_coarse_no_suffix_units(
    unit_factors: MemoryUnitFactors,
    values: &[&str],
) -> bool {
    unit_factors.no_suffix >= 1024.0 && values.iter().all(|value| is_bare_integer_value(value))
}

pub(in crate::hooks::linux_memory) fn is_bare_integer_value(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(in crate::hooks::linux_memory) fn parse_memory_value_to_mib(
    value: &str,
    unit_factors: MemoryUnitFactors,
) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let suffix_start = value
        .char_indices()
        .find_map(|(idx, ch)| {
            if ch.is_ascii_digit() || ch == '.' {
                None
            } else {
                Some(idx)
            }
        })
        .unwrap_or(value.len());
    let number = &value[..suffix_start];
    let suffix = value[suffix_start..].trim().to_ascii_lowercase();
    let factor = match suffix.as_str() {
        "" => unit_factors.no_suffix,
        "b" => 1.0 / (1024.0 * 1024.0),
        "k" | "kb" => unit_factors.bare_suffix_mib / unit_factors.bare_suffix_step,
        "ki" | "kib" => 1.0 / 1024.0,
        "m" | "mb" => unit_factors.bare_suffix_mib,
        "mi" | "mib" => 1.0,
        "g" | "gb" => unit_factors.bare_suffix_mib * unit_factors.bare_suffix_step,
        "gi" | "gib" => 1024.0,
        "t" | "tb" => {
            unit_factors.bare_suffix_mib
                * unit_factors.bare_suffix_step
                * unit_factors.bare_suffix_step
        }
        "ti" | "tib" => 1024.0 * 1024.0,
        "p" | "pb" => {
            unit_factors.bare_suffix_mib
                * unit_factors.bare_suffix_step
                * unit_factors.bare_suffix_step
                * unit_factors.bare_suffix_step
        }
        "pi" | "pib" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    parse_float(number).map(|v| v * factor)
}

pub(in crate::hooks::linux_memory) fn is_top_mem_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    !line.contains("swap")
        && line.contains(" mem")
        && line.contains(" total")
        && line.contains(" free")
}

pub(in crate::hooks::linux_memory) fn is_top_swap_line(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains(" swap") && line.contains(" total")
}

pub(in crate::hooks::linux_memory) fn top_unit_factor(line: &str) -> Option<f64> {
    let line = line.to_ascii_lowercase();
    if line.starts_with("tib") {
        Some(1024.0 * 1024.0)
    } else if line.starts_with("gib") {
        Some(1024.0)
    } else if line.starts_with("mib") {
        Some(1.0)
    } else if line.starts_with("kib") {
        Some(1.0 / 1024.0)
    } else {
        None
    }
}

pub(in crate::hooks::linux_memory) fn top_metric_value(line: &str, label: &str) -> Option<f64> {
    let normalized = line.replace([',', ':'], " ");
    let tokens = split_tokens(&normalized);
    tokens
        .windows(2)
        .find(|window| window[1].trim_end_matches('.').eq_ignore_ascii_case(label))
        .and_then(|window| parse_float(window[0]))
}

pub(in crate::hooks::linux_memory) fn top_avail_mem_value(line: &str) -> Option<f64> {
    let normalized = line.replace([',', ':'], " ");
    let tokens = split_tokens(&normalized);
    tokens.windows(3).find_map(|window| {
        if window[1].eq_ignore_ascii_case("avail") && window[2].eq_ignore_ascii_case("Mem") {
            parse_float(window[0])
        } else {
            None
        }
    })
}

pub(in crate::hooks::linux_memory) fn display_command(command: &str) -> String {
    let mut tokens = command.split_whitespace().filter(|token| *token != "\\_");
    tokens.next().unwrap_or(command).trim().to_string()
}

pub(in crate::hooks::linux_memory) fn kib_to_mib(kib: u64) -> u64 {
    (kib + 512) / 1024
}

pub(in crate::hooks::linux_memory) fn round_mib(mib: f64) -> u64 {
    mib.round() as u64
}
