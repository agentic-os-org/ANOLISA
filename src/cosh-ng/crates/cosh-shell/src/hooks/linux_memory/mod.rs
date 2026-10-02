use crate::hooks::model::{HookInput, HookMatcher, HookTrigger};
use crate::hooks::BuiltinHook;
use crate::types::{
    BuiltinFindingFacts, FindingSeverity, HighMemoryProcessFacts, HookFinding, MemoryPressureFacts,
    MetricsConfidence, ProcessMemoryFact,
};

mod parser;
mod presentation;

use parser::*;
use presentation::{high_memory_finding, memory_pressure_finding};

#[derive(Debug, Clone, PartialEq)]
struct ProcessMemoryRow {
    pid: String,
    command: String,
    mem_pct: f64,
    rss_kib: Option<u64>,
}

#[derive(Debug, Clone, PartialEq)]
struct MemoryMetrics {
    total_mib: f64,
    available_mib: f64,
    swap_total_mib: Option<f64>,
    swap_used_mib: Option<f64>,
    confidence: MetricsConfidence,
}

#[derive(Debug, Clone, Copy)]
struct MemoryUnitFactors {
    no_suffix: f64,
    bare_suffix_mib: f64,
    bare_suffix_step: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnSemantic {
    Pid,
    MemPct,
    Rss,
    Res,
    Command,
    Total,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ColumnBinding {
    semantic: ColumnSemantic,
    index: usize,
    raw_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableSchema {
    columns: Vec<ColumnBinding>,
}

#[derive(Debug, Clone, Copy)]
struct ColumnSpec<'a> {
    semantic: ColumnSemantic,
    aliases: &'a [&'a str],
    prefer_last: bool,
}

pub struct HighMemoryProcessHook {
    matcher: HookMatcher,
}

impl Default for HighMemoryProcessHook {
    fn default() -> Self {
        Self::new()
    }
}

impl HighMemoryProcessHook {
    pub fn new() -> Self {
        Self {
            matcher: HookMatcher {
                id: "high-memory-process".into(),
                commands: vec!["ps".into(), "top".into(), "env".into()],
                command_patterns: Vec::new(),
                command_regex: None,
                exit_codes: Some(vec![0]),
                min_output_bytes: Some(1),
                trigger: HookTrigger::OnSuccess,
            },
        }
    }
}

impl BuiltinHook for HighMemoryProcessHook {
    fn id(&self) -> &str {
        "high-memory-process"
    }

    fn matcher(&self) -> &HookMatcher {
        &self.matcher
    }

    fn evaluate(&self, input: &HookInput) -> Option<HookFinding> {
        let rows = process_rows(input)?;
        high_memory_finding(&rows)
    }

    fn builtin_facts(&self, input: &HookInput) -> Option<BuiltinFindingFacts> {
        high_memory_facts(&process_rows(input)?)
    }
}

pub struct MemoryPressureHook {
    matcher: HookMatcher,
}

impl Default for MemoryPressureHook {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryPressureHook {
    pub fn new() -> Self {
        Self {
            matcher: HookMatcher {
                id: "memory-pressure".into(),
                commands: vec!["free".into(), "top".into(), "env".into()],
                command_patterns: Vec::new(),
                command_regex: None,
                exit_codes: Some(vec![0]),
                min_output_bytes: Some(1),
                trigger: HookTrigger::OnSuccess,
            },
        }
    }
}

impl BuiltinHook for MemoryPressureHook {
    fn id(&self) -> &str {
        "memory-pressure"
    }

    fn matcher(&self) -> &HookMatcher {
        &self.matcher
    }

    fn evaluate(&self, input: &HookInput) -> Option<HookFinding> {
        let metrics = memory_metrics(input)?;
        memory_pressure_finding(metrics.as_ref())
    }

    fn builtin_facts(&self, input: &HookInput) -> Option<BuiltinFindingFacts> {
        memory_pressure_facts(memory_metrics(input)?.as_ref())
    }
}

fn process_rows(input: &HookInput) -> Option<Vec<ProcessMemoryRow>> {
    match memory_target_program(&input.command) {
        "top" if is_batch_top_command(&input.command) => {
            Some(parse_top_process_rows(&input.output_preview))
        }
        "ps" => Some(parse_ps_process_rows(&input.output_preview)),
        _ => None,
    }
}

fn memory_metrics(input: &HookInput) -> Option<Option<MemoryMetrics>> {
    match memory_target_program(&input.command) {
        "top" if is_batch_top_command(&input.command) => {
            Some(parse_top_memory_metrics(&input.output_preview))
        }
        "free" if !is_free_sampling_command(&input.command) => Some(parse_free_memory_metrics(
            &input.command,
            &input.output_preview,
        )),
        _ => None,
    }
}

fn memory_pressure_facts(metrics: Option<&MemoryMetrics>) -> Option<BuiltinFindingFacts> {
    let metrics = metrics?;
    if metrics.total_mib <= 0.0 {
        return None;
    }
    Some(BuiltinFindingFacts::MemoryPressure(MemoryPressureFacts {
        confidence: metrics.confidence,
        available_ratio: metrics.available_mib / metrics.total_mib,
        swap_ratio: match (metrics.swap_total_mib, metrics.swap_used_mib) {
            (Some(total), Some(used)) if total > 0.0 => Some(used / total),
            _ => None,
        },
    }))
}

fn high_memory_facts(rows: &[ProcessMemoryRow]) -> Option<BuiltinFindingFacts> {
    let mut rows = rows
        .iter()
        .filter(|row| row.mem_pct >= 20.0)
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| right.mem_pct.total_cmp(&left.mem_pct));
    let processes = rows
        .into_iter()
        .take(3)
        .filter_map(|row| {
            let command_basename = display_command(&row.command);
            let command_basename = command_basename.rsplit('/').next()?;
            is_safe_process_basename(command_basename).then(|| ProcessMemoryFact {
                pid: row.pid.clone(),
                command_basename: command_basename.to_string(),
                mem_pct: row.mem_pct,
                rss_kib: row.rss_kib,
            })
        })
        .collect::<Vec<_>>();
    (!processes.is_empty()).then_some(BuiltinFindingFacts::HighMemoryProcesses(
        HighMemoryProcessFacts {
            confidence: MetricsConfidence::High,
            processes,
        },
    ))
}

fn is_safe_process_basename(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
}

#[cfg(test)]
mod tests;
