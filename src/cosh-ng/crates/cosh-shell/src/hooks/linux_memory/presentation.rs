use super::*;

const HIGH_MEMORY_CLI_HINT: &str =
    "ps -eo pid,ppid,comm,%mem,%cpu,rss,vsz,args --sort=-%mem | head -20";
const MEMORY_PRESSURE_CLI_HINT: &str =
    "free -m && ps -eo pid,ppid,comm,%mem,%cpu,rss,vsz,args --sort=-%mem | head -20";

pub(super) fn high_memory_finding(rows: &[ProcessMemoryRow]) -> Option<HookFinding> {
    let mut candidates: Vec<&ProcessMemoryRow> =
        rows.iter().filter(|row| row.mem_pct >= 20.0).collect();
    candidates.sort_by(|a, b| b.mem_pct.total_cmp(&a.mem_pct));
    let top = candidates.first()?;

    let severity = if top.mem_pct >= 50.0 {
        FindingSeverity::Critical
    } else if top.mem_pct >= 30.0 {
        FindingSeverity::Warning
    } else {
        FindingSeverity::Info
    };

    let summary = candidates
        .iter()
        .take(3)
        .map(|row| {
            let rss = row
                .rss_kib
                .map(|rss| format!(", RSS {} MiB", kib_to_mib(rss)))
                .unwrap_or_default();
            format!(
                "{} (PID {}) MEM {:.1}%{}",
                display_command(&row.command),
                row.pid,
                row.mem_pct,
                rss
            )
        })
        .collect::<Vec<_>>()
        .join("; ");

    Some(HookFinding {
        hook_id: "high-memory-process".into(),
        severity,
        title: format!(
            "{} (PID {}) uses {:.1}% MEM",
            display_command(&top.command),
            top.pid,
            top.mem_pct
        ),
        description: format!(
            "Process-level memory candidates from command output. Top matches: {summary}"
        ),
        suggestion: "Use available memory diagnosis skills or focused read-only commands to inspect pressure and high %MEM processes."
            .into(),
        skill: None,
        cli_hint: Some(HIGH_MEMORY_CLI_HINT.into()),
        context_refs: Vec::new(),
    })
}

pub(super) fn memory_pressure_finding(metrics: Option<&MemoryMetrics>) -> Option<HookFinding> {
    let metrics = metrics?;
    if metrics.total_mib <= 0.0 {
        return None;
    }
    let available_ratio = metrics.available_mib / metrics.total_mib;
    let swap_ratio = match (metrics.swap_total_mib, metrics.swap_used_mib) {
        (Some(total), Some(used)) if total > 0.0 => Some(used / total),
        _ => None,
    };

    let memory_severity = if available_ratio <= 0.05 {
        Some(FindingSeverity::Critical)
    } else if available_ratio <= 0.10 {
        Some(FindingSeverity::Warning)
    } else {
        None
    };
    let severity = memory_severity?;
    let swap_note = swap_ratio
        .map(|ratio| format!(", swap used {:.1}%", ratio * 100.0))
        .unwrap_or_default();
    let confidence_note = if metrics.confidence == MetricsConfidence::Low {
        " Confidence is lower; verify with a higher-resolution one-shot command before giving a root-cause conclusion."
    } else {
        ""
    };

    let title = format!(
        "Available memory is low: {} MiB / {} MiB",
        round_mib(metrics.available_mib),
        round_mib(metrics.total_mib)
    );

    Some(HookFinding {
        hook_id: "memory-pressure".into(),
        severity,
        title,
        description: format!(
            "Command output shows available memory at {:.1}% of total{}.{confidence_note}",
            available_ratio * 100.0,
            swap_note
        ),
        suggestion: "Use available memory diagnosis skills or focused read-only commands to inspect memory pressure, swap usage, and high %MEM processes."
            .into(),
        skill: None,
        cli_hint: Some(MEMORY_PRESSURE_CLI_HINT.into()),
        context_refs: Vec::new(),
    })
}
