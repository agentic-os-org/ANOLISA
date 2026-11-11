use super::super::builder::HealthReportBuilder;
use super::super::model::{
    HealthFactCategory, HealthFactSource, HealthFactValue, HealthUnavailableReason,
};
use std::fs;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MemoryFacts {
    pub(crate) total_mib: u64,
    pub(crate) available_mib: u64,
    pub(crate) available_ratio: f64,
    pub(crate) used_ratio: f64,
    pub(crate) swap_total_mib: u64,
    pub(crate) swap_used_mib: u64,
    pub(crate) swap_used_ratio: f64,
}

impl MemoryFacts {
    pub(crate) fn record(&self, builder: &mut HealthReportBuilder, elapsed_ms: u128) {
        let id = builder.current_check_id();
        let memory = id != Some("resource.swap");
        let swap = id != Some("resource.memory");
        if memory {
            builder
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.total_mib",
                    HealthFactValue::Unsigned(self.total_mib),
                    Some("MiB".to_string()),
                    HealthFactSource::ProcMeminfo,
                    elapsed_ms,
                )
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.available_mib",
                    HealthFactValue::Unsigned(self.available_mib),
                    Some("MiB".to_string()),
                    HealthFactSource::ProcMeminfo,
                    elapsed_ms,
                )
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.available_ratio",
                    HealthFactValue::Float(self.available_ratio),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                )
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.used_ratio",
                    HealthFactValue::Float(self.used_ratio),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                );
        }
        if swap {
            builder
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.swap_total_mib",
                    HealthFactValue::Unsigned(self.swap_total_mib),
                    Some("MiB".to_string()),
                    HealthFactSource::ProcMeminfo,
                    elapsed_ms,
                )
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.swap_used_mib",
                    HealthFactValue::Unsigned(self.swap_used_mib),
                    Some("MiB".to_string()),
                    HealthFactSource::Derived,
                    elapsed_ms,
                )
                .add_fact(
                    HealthFactCategory::Memory,
                    "memory.swap_used_ratio",
                    HealthFactValue::Float(self.swap_used_ratio),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                );
            if builder.current_check_id() == Some("resource.swap") {
                builder.add_fact(
                    HealthFactCategory::Memory,
                    "memory.swap_memory_pressure",
                    HealthFactValue::Bool(
                        super::super::rules::memory_pressure_level(
                            self.available_mib as f64,
                            self.available_ratio,
                        )
                        .is_some(),
                    ),
                    None,
                    HealthFactSource::Derived,
                    elapsed_ms,
                );
            }
        }
    }
}

pub(crate) fn parse_proc_meminfo(content: &str) -> Option<MemoryFacts> {
    let total_kib = meminfo_kib(content, "MemTotal")?;
    let available_kib = meminfo_kib(content, "MemAvailable")?;
    let swap_total_kib = meminfo_kib(content, "SwapTotal").unwrap_or(0);
    let swap_free_kib = meminfo_kib(content, "SwapFree").unwrap_or(0);
    if total_kib == 0 {
        return None;
    }
    let swap_used_kib = swap_total_kib.saturating_sub(swap_free_kib);
    let available_ratio = available_kib as f64 / total_kib as f64;
    let swap_used_ratio = if swap_total_kib == 0 {
        0.0
    } else {
        swap_used_kib as f64 / swap_total_kib as f64
    };
    Some(MemoryFacts {
        total_mib: total_kib / 1024,
        available_mib: available_kib / 1024,
        available_ratio,
        used_ratio: 1.0 - available_ratio,
        swap_total_mib: swap_total_kib / 1024,
        swap_used_mib: swap_used_kib / 1024,
        swap_used_ratio,
    })
}

fn meminfo_kib(content: &str, key: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let (line_key, rest) = line.split_once(':')?;
        if line_key != key {
            return None;
        }
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })
}

pub(in crate::diagnostics::health) fn read_memory() -> Result<MemoryFacts, HealthUnavailableReason>
{
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|content| parse_proc_meminfo(&content))
        .ok_or(HealthUnavailableReason::ParseError)
}
