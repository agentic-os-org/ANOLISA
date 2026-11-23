//! Bounded host scalar sampling for R07 file-nr and R09 conntrack capacity.
//! Missing interfaces stay unknown; raw integers are recorded without scaling.

use std::fs;
use std::io::Read;
use std::path::Path;

use super::super::builder::HealthReportBuilder;
use super::super::model::{HealthFactCategory, HealthFactSource, HealthFactValue};

const SCALAR_MAX_BYTES: u64 = 2048;

fn read_scalar(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    let mut content = String::new();
    file.take(SCALAR_MAX_BYTES + 1)
        .read_to_string(&mut content)
        .ok()?;
    (content.len() as u64 <= SCALAR_MAX_BYTES).then_some(content)
}

pub(crate) fn parse_file_nr(content: &str) -> Option<(u64, u64, u64)> {
    let mut fields = content.split_whitespace();
    let allocated = fields.next()?.parse().ok()?;
    let unused = fields.next()?.parse().ok()?;
    let max = fields.next()?.parse().ok()?;
    fields.next().is_none().then_some((allocated, unused, max))
}

fn netns_identity() -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata("/proc/self/ns/net").ok()?;
    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

pub(crate) fn collect_host_scalars(builder: &mut HealthReportBuilder, elapsed_ms: u128) {
    let mut add = |key: &str, value: HealthFactValue| {
        builder.add_fact(
            HealthFactCategory::Host,
            key,
            value,
            None,
            HealthFactSource::Derived,
            elapsed_ms,
        );
    };
    if let Some((allocated, unused, max)) =
        read_scalar(Path::new("/proc/sys/fs/file-nr")).and_then(|content| parse_file_nr(&content))
    {
        add(
            "host.file_nr_allocated",
            HealthFactValue::Unsigned(allocated),
        );
        add("host.file_nr_unused", HealthFactValue::Unsigned(unused));
        add("host.file_nr_max", HealthFactValue::Unsigned(max));
        if let Some(level) =
            super::super::rules::handles::judge_handles(allocated, unused, max).level
        {
            add(
                "host.file_nr_level",
                HealthFactValue::String(level.to_string()),
            );
        }
    }
    let count = read_scalar(Path::new("/proc/sys/net/netfilter/nf_conntrack_count"))
        .and_then(|content| content.trim().parse::<u64>().ok());
    let max = read_scalar(Path::new("/proc/sys/net/netfilter/nf_conntrack_max"))
        .and_then(|content| content.trim().parse::<u64>().ok());
    if let (Some(count), Some(max)) = (count, max) {
        add("host.conntrack_count", HealthFactValue::Unsigned(count));
        add("host.conntrack_max", HealthFactValue::Unsigned(max));
        if let Some(netns) = netns_identity() {
            add("host.conntrack_netns_id", HealthFactValue::String(netns));
        }
        if let Some(level) = super::super::rules::conntrack::judge_conntrack(count, max).entry {
            let label = match level {
                super::super::rules::volume::MemberLevel::Critical => "critical",
                super::super::rules::volume::MemberLevel::Warning => "warning",
            };
            add(
                "host.conntrack_level",
                HealthFactValue::String(label.to_string()),
            );
        }
    }
}

#[cfg(test)]
mod tests;
