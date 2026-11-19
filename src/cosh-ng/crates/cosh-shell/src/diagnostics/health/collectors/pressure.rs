//! Bounded PSI sampling shared by host (/proc/pressure) and current-cgroup
//! collectors. Missing lines stay unknown; valid zeros are silent observations.

use std::fs;
use std::io::Read;
use std::path::Path;

use super::super::builder::HealthReportBuilder;
use super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};

pub(crate) const PRESSURE_MAX_BYTES: u64 = 3 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct PressureLine {
    pub avg10: f64,
    pub avg60: f64,
    pub total: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct PressureSample {
    pub some: Option<PressureLine>,
    pub full: Option<PressureLine>,
}

pub(crate) fn parse_pressure_line(line: &str) -> Option<PressureLine> {
    let mut avg10 = None;
    let mut avg60 = None;
    let mut total = None;
    for field in line.split_whitespace().skip(1) {
        let (key, value) = field.split_once('=')?;
        match key {
            "avg10" => avg10 = parse_percent(value),
            "avg60" => avg60 = parse_percent(value),
            "total" => total = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    Some(PressureLine {
        avg10: avg10?,
        avg60: avg60?,
        total: total?,
    })
}

fn parse_percent(value: &str) -> Option<f64> {
    let parsed = value.parse::<f64>().ok()?;
    (parsed.is_finite() && (0.0..=100.0).contains(&parsed)).then_some(parsed)
}

pub(crate) fn parse_pressure(content: &str) -> PressureSample {
    let mut sample = PressureSample::default();
    for line in content.lines() {
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("some") => sample.some = parse_pressure_line(line),
            Some("full") => sample.full = parse_pressure_line(line),
            _ => {}
        }
    }
    sample
}

pub(crate) fn read_pressure(path: &Path) -> Option<PressureSample> {
    let file = fs::File::open(path).ok()?;
    let mut content = String::new();
    file.take(PRESSURE_MAX_BYTES + 1)
        .read_to_string(&mut content)
        .ok()?;
    if content.len() as u64 > PRESSURE_MAX_BYTES {
        return None;
    }
    Some(parse_pressure(&content))
}

pub(crate) fn record_pressure(
    builder: &mut HealthReportBuilder,
    prefix: &str,
    category: HealthFactCategory,
    sample: &PressureSample,
    elapsed_ms: u128,
) {
    for (kind, line) in [("some", sample.some), ("full", sample.full)] {
        let Some(line) = line else {
            continue;
        };
        for (suffix, value) in [
            ("avg10", HealthFactValue::Float(line.avg10)),
            ("avg60", HealthFactValue::Float(line.avg60)),
            ("total", HealthFactValue::Unsigned(line.total)),
        ] {
            let unit = (suffix == "total").then(|| "us".to_string());
            builder.add_fact(
                category,
                format!("{prefix}_{kind}_{suffix}"),
                value,
                unit,
                HealthFactSource::Derived,
                elapsed_ms,
            );
        }
    }
}

#[cfg(test)]
mod tests;
