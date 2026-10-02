use super::*;

mod support;

pub(super) use support::*;

pub(super) fn parse_ps_process_rows(output: &str) -> Vec<ProcessMemoryRow> {
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    for (idx, line) in lines.iter().enumerate() {
        let tokens = split_tokens(line);
        let Some(header) = PsHeader::from_tokens(&tokens) else {
            continue;
        };
        return lines[idx + 1..]
            .iter()
            .filter_map(|row| parse_process_row(row, &header))
            .collect();
    }
    Vec::new()
}

pub(super) fn parse_top_process_rows(output: &str) -> Vec<ProcessMemoryRow> {
    let lines: Vec<&str> = output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    for (idx, line) in lines.iter().enumerate() {
        let tokens = split_tokens(line);
        let Some(header) = TopHeader::from_tokens(&tokens) else {
            continue;
        };
        return lines[idx + 1..]
            .iter()
            .filter_map(|row| parse_top_process_row(row, &header))
            .collect();
    }
    Vec::new()
}

#[derive(Debug, Clone)]
struct PsHeader {
    pid_idx: usize,
    mem_idx: usize,
    rss_idx: Option<usize>,
    command_idx: usize,
    command_is_tail: bool,
}

impl PsHeader {
    pub(super) fn from_tokens(tokens: &[&str]) -> Option<Self> {
        let schema = detect_table_schema(
            tokens,
            &[
                ColumnSpec::first(ColumnSemantic::Pid, &["PID"]),
                ColumnSpec::first(ColumnSemantic::MemPct, &["%MEM", "PMEM", "MEM%"]),
                ColumnSpec::first(ColumnSemantic::Rss, &["RSS", "RSZ", "RSSIZE"]),
                ColumnSpec::last(ColumnSemantic::Command, &["COMMAND", "ARGS", "CMD", "COMM"]),
            ],
        );
        let pid_idx = schema.index(ColumnSemantic::Pid)?;
        let mem_idx = schema.index(ColumnSemantic::MemPct)?;
        let rss_idx = schema.index(ColumnSemantic::Rss);
        let command_idx = schema.index(ColumnSemantic::Command)?;
        let command_is_tail = command_idx + 1 == tokens.len();
        Some(Self {
            pid_idx,
            mem_idx,
            rss_idx,
            command_idx,
            command_is_tail,
        })
    }
}

#[derive(Debug, Clone)]
struct TopHeader {
    pid_idx: usize,
    mem_idx: usize,
    res_idx: Option<usize>,
    command_idx: usize,
}

impl TopHeader {
    pub(super) fn from_tokens(tokens: &[&str]) -> Option<Self> {
        let schema = detect_table_schema(
            tokens,
            &[
                ColumnSpec::first(ColumnSemantic::Pid, &["PID"]),
                ColumnSpec::first(ColumnSemantic::MemPct, &["%MEM"]),
                ColumnSpec::first(ColumnSemantic::Res, &["RES"]),
                ColumnSpec::last(ColumnSemantic::Command, &["COMMAND", "ARGS", "CMD", "COMM"]),
            ],
        );
        Some(Self {
            pid_idx: schema.index(ColumnSemantic::Pid)?,
            mem_idx: schema.index(ColumnSemantic::MemPct)?,
            res_idx: schema.index(ColumnSemantic::Res),
            command_idx: schema.index(ColumnSemantic::Command)?,
        })
    }
}

fn parse_process_row(line: &str, header: &PsHeader) -> Option<ProcessMemoryRow> {
    let tokens = split_tokens(line);
    let pid = tokens.get(header.pid_idx)?.to_string();
    let mem_pct = parse_float(tokens.get(header.mem_idx)?)?;
    let rss_kib = header
        .rss_idx
        .and_then(|idx| tokens.get(idx))
        .and_then(|value| parse_u64(value));
    let command = command_from_tokens(&tokens, header.command_idx, header.command_is_tail)?;
    Some(ProcessMemoryRow {
        pid,
        command,
        mem_pct,
        rss_kib,
    })
}

fn parse_top_process_row(line: &str, header: &TopHeader) -> Option<ProcessMemoryRow> {
    let tokens = split_tokens(line);
    let pid = tokens.get(header.pid_idx)?.to_string();
    let mem_pct = parse_float(tokens.get(header.mem_idx)?)?;
    let rss_kib = header
        .res_idx
        .and_then(|idx| tokens.get(idx))
        .and_then(|value| parse_memory_to_mib(value))
        .map(|mib| (mib * 1024.0).round() as u64);
    let command = command_from_tokens(&tokens, header.command_idx, true)?;
    Some(ProcessMemoryRow {
        pid,
        command,
        mem_pct,
        rss_kib,
    })
}

pub(super) fn parse_free_memory_metrics(command: &str, output: &str) -> Option<MemoryMetrics> {
    let mut header_tokens: Option<Vec<&str>> = None;
    let mut mem_tokens: Option<Vec<&str>> = None;
    let mut swap_tokens: Option<Vec<&str>> = None;
    let mut header_row_count = 0;
    let mut mem_row_count = 0;
    for line in output.lines() {
        let tokens = split_tokens(line);
        if tokens.is_empty() {
            continue;
        }
        let first_token = tokens[0].trim_end_matches(':');
        if first_token.eq_ignore_ascii_case("mem") {
            mem_row_count += 1;
            mem_tokens = Some(tokens);
        } else if first_token.eq_ignore_ascii_case("swap") {
            swap_tokens = Some(tokens);
        } else if tokens
            .iter()
            .any(|token| token.eq_ignore_ascii_case("total"))
            && tokens
                .iter()
                .any(|token| token.eq_ignore_ascii_case("available"))
        {
            header_row_count += 1;
            header_tokens = Some(tokens);
        }
    }
    if header_row_count != 1 || mem_row_count != 1 {
        return None;
    }
    let header = header_tokens?;
    let mem = mem_tokens?;
    let schema = detect_table_schema(
        &header,
        &[
            ColumnSpec::first(ColumnSemantic::Total, &["total"]),
            ColumnSpec::first(ColumnSemantic::Available, &["available"]),
        ],
    );
    let total_idx = schema.index(ColumnSemantic::Total)? + 1;
    let available_idx = schema.index(ColumnSemantic::Available)? + 1;
    let unit_factors = free_unit_factors(command);
    let total_raw = mem.get(total_idx)?;
    let available_raw = mem.get(available_idx)?;
    let total_mib = parse_free_memory_value_to_mib(total_raw, unit_factors)?;
    let available_mib = parse_free_memory_value_to_mib(available_raw, unit_factors)?;
    let (swap_total_mib, swap_used_mib) = swap_tokens
        .as_ref()
        .and_then(|swap| {
            Some((
                parse_free_memory_value_to_mib(swap.get(1)?, unit_factors)?,
                parse_free_memory_value_to_mib(swap.get(2)?, unit_factors)?,
            ))
        })
        .map(|(total, used)| (Some(total), Some(used)))
        .unwrap_or((None, None));
    Some(MemoryMetrics {
        total_mib,
        available_mib,
        swap_total_mib,
        swap_used_mib,
        confidence: if free_output_uses_coarse_no_suffix_units(
            unit_factors,
            &[total_raw, available_raw],
        ) {
            MetricsConfidence::Low
        } else {
            MetricsConfidence::High
        },
    })
}

pub(super) fn parse_top_memory_metrics(output: &str) -> Option<MemoryMetrics> {
    let mut total_mib = None;
    let mut free_mib = None;
    let mut swap_total_mib = None;
    let mut swap_used_mib = None;
    let mut available_mib = None;

    for line in output.lines() {
        let trimmed = line.trim();
        if is_top_mem_line(trimmed) {
            let factor = top_unit_factor(trimmed)?;
            total_mib = top_metric_value(trimmed, "total").map(|v| v * factor);
            free_mib = top_metric_value(trimmed, "free").map(|v| v * factor);
        } else if is_top_swap_line(trimmed) {
            let factor = top_unit_factor(trimmed)?;
            swap_total_mib = top_metric_value(trimmed, "total").map(|v| v * factor);
            swap_used_mib = top_metric_value(trimmed, "used").map(|v| v * factor);
            available_mib = top_avail_mem_value(trimmed).map(|v| v * factor);
        }
    }

    let total_mib = total_mib?;
    if let Some(available_mib) = available_mib {
        return Some(MemoryMetrics {
            total_mib,
            available_mib,
            swap_total_mib,
            swap_used_mib,
            confidence: MetricsConfidence::High,
        });
    }
    Some(MemoryMetrics {
        total_mib,
        available_mib: free_mib?,
        swap_total_mib,
        swap_used_mib,
        confidence: MetricsConfidence::Low,
    })
}
