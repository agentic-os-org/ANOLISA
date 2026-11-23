//! R04 attribution: exact same-domain kills or trusted global kills, conservative
//! monotonic age, and structured journal source verification.

use super::cgroup::in_current_domain;
use super::oom::{merge_oom_signal, OomSignal};

#[derive(Debug, Clone, Default)]
pub(crate) struct AttributionContext {
    pub current_cgroup: Option<String>,
    pub now_monotonic_ns: Option<u64>,
    pub host_qualified: bool,
    pub boot_id: Option<String>,
    pub source_verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttributedOom {
    pub age_seconds: u64,
    pub epoch: u64,
    pub event_identity: String,
    pub pid: u64,
    pub process: Option<String>,
    pub scope: &'static str,
    pub task_cgroup: Option<String>,
    pub oom_cgroup: Option<String>,
}

fn event_identity(signal: &OomSignal) -> String {
    if signal.has_event_anchor {
        signal.monotonic_ns.map_or_else(
            || format!("epoch:{}", signal.identity_epoch),
            |time| format!("uptime-ns:{time}"),
        )
    } else {
        format!("epoch:{}", signal.epoch)
    }
}

fn signal_age(signal: &OomSignal, ctx: &AttributionContext, now_epoch: u64) -> Option<u64> {
    if let (Some(now), Some(event)) = (ctx.now_monotonic_ns, signal.monotonic_ns) {
        return now.checked_sub(event).map(|ns| ns / 1_000_000_000);
    }
    Some(now_epoch.saturating_sub(signal.event_epoch()))
}

pub(super) fn select_attributed(
    events: &[OomSignal],
    ctx: &AttributionContext,
    now_epoch: u64,
) -> Option<AttributedOom> {
    let mut best: Option<(u64, AttributedOom)> = None;
    for signal in events {
        if !signal.has_kill_record {
            continue;
        }
        let Some(pid) = signal.pid.filter(|pid| *pid > 0) else {
            continue;
        };
        let scope = match signal.constraint.as_deref() {
            Some("CONSTRAINT_NONE") if ctx.host_qualified => "host",
            Some("CONSTRAINT_MEMCG") => {
                let Some(current) = ctx.current_cgroup.as_deref() else {
                    continue;
                };
                let Some(victim) = signal.task_cgroup.as_deref() else {
                    continue;
                };
                if !in_current_domain(victim, current) {
                    continue;
                }
                "current-cgroup"
            }
            _ => continue,
        };
        let Some(age_seconds) = signal_age(signal, ctx, now_epoch) else {
            continue;
        };
        let attributed = AttributedOom {
            age_seconds,
            epoch: signal.event_epoch(),
            event_identity: event_identity(signal),
            pid,
            process: signal.process.clone(),
            scope,
            task_cgroup: signal.task_cgroup.clone(),
            oom_cgroup: signal.oom_cgroup.clone(),
        };
        let rank = signal.event_epoch();
        if best.as_ref().is_none_or(|(current, _)| rank > *current) {
            best = Some((rank, attributed));
        }
    }
    best.map(|(_, attributed)| attributed)
}

pub(crate) fn now_monotonic_ns() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut value = nix::libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { nix::libc::clock_gettime(nix::libc::CLOCK_MONOTONIC, &mut value) } < 0 {
            return None;
        }
        Some(value.tv_sec as u64 * 1_000_000_000 + value.tv_nsec as u64)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

pub(super) fn journal_message(entry: &serde_json::Value) -> Option<String> {
    match entry.get("MESSAGE")? {
        serde_json::Value::String(message) => Some(message.clone()),
        serde_json::Value::Array(bytes) => {
            let bytes = bytes
                .iter()
                .map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect::<Option<Vec<u8>>>()?;
            String::from_utf8(bytes).ok()
        }
        _ => None,
    }
}

pub(super) fn journal_usec(entry: &serde_json::Value, key: &str) -> Option<u64> {
    entry.get(key)?.as_str()?.parse().ok()
}

/// `/proc/sys/kernel/random/boot_id` carries dashed 36-char UUIDs while
/// journald `_BOOT_ID` stores the same id as 32 hex chars without dashes;
/// normalize both before comparing so real systemd sources verify.
fn boot_id_matches(expected: &str, actual: &str) -> bool {
    fn normalize(value: &str) -> String {
        value
            .chars()
            .filter(|ch| *ch != '-')
            .collect::<String>()
            .to_ascii_lowercase()
    }
    let expected = normalize(expected);
    let actual = normalize(actual);
    !expected.is_empty() && expected == actual
}

pub(super) fn journal_verified(entry: &serde_json::Value, ctx: &AttributionContext) -> bool {
    entry.get("_TRANSPORT").and_then(|v| v.as_str()) == Some("kernel")
        && entry.get("SYSLOG_FACILITY").and_then(|v| v.as_str()) == Some("0")
        && match (
            ctx.boot_id.as_deref(),
            entry.get("_BOOT_ID").and_then(|v| v.as_str()),
        ) {
            (Some(expected), Some(actual)) => boot_id_matches(expected, actual),
            _ => false,
        }
}

/// Returns None when the output is not structured journal JSON at all, so the
/// caller can fall back to the legacy text read. Entries failing transport,
/// facility or boot verification are excluded from recent eligibility; the
/// excluded count keeps the gap visible instead of claiming a clean source.
pub(super) fn parse_journal_json_signals(
    content: &str,
    ctx: &AttributionContext,
) -> Option<(Vec<OomSignal>, u64, bool)> {
    let mut events = Vec::new();
    let mut unclocked = 0u64;
    let mut panic_recent = false;
    let mut saw_json = false;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<serde_json::Value>(line) else {
            unclocked += 1;
            continue;
        };
        saw_json = true;
        let Some(message) = journal_message(&entry) else {
            continue;
        };
        let lower = message.to_ascii_lowercase();
        if lower.contains("kernel panic") || lower.contains("panic:") {
            panic_recent = true;
        }
        if !super::is_oom_line(&lower) {
            continue;
        }
        let verified = journal_verified(&entry, ctx);
        if !verified {
            unclocked += 1;
            continue;
        }
        let Some(epoch) = journal_usec(&entry, "__REALTIME_TIMESTAMP").map(|usec| usec / 1_000_000)
        else {
            unclocked += 1;
            continue;
        };
        let mut signal = OomSignal::from_line(epoch, &message);
        if let Some(monotonic) = journal_usec(&entry, "__MONOTONIC_TIMESTAMP") {
            signal.monotonic_ns = Some(monotonic.saturating_mul(1000));
        }
        merge_oom_signal(&mut events, signal, lower.contains("invoked oom-killer"));
    }
    saw_json.then_some((events, unclocked, panic_recent))
}

#[cfg(test)]
mod tests;
