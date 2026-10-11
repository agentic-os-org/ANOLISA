//! OOM recovery module
//!
//! On AgentSight startup, scans `dmesg` for OOM kill events that occurred
//! after the last known AgentSight shutdown timestamp. For each killed process
//! that matches a known agent name — or that has in-flight LLM calls in
//! genai_events, whatever its comm — an `agent_crash` InterruptionEvent is
//! written to the interruption store with `oom: true` in its detail JSON;
//! kills with neither signal are noise and are skipped.
//!
//! This handles the case where AgentSight itself was killed by OOM and
//! therefore could not record the crash in real-time.

use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::interruption::types::{InterruptionEvent, InterruptionType};
use crate::storage::sqlite::GenAISqliteStore;
use crate::storage::sqlite::InterruptionStore;

/// A parsed OOM kill event from dmesg
#[derive(Debug)]
struct OomKillEvent {
    /// Approximate timestamp in nanoseconds since Unix epoch
    pub timestamp_ns: i64,
    /// Killed process PID
    pub pid: i32,
    /// Killed process name (comm, may be truncated to 15 chars)
    pub process_name: String,
}

/// Run OOM recovery on startup.
///
/// Reads `dmesg -T`, parses OOM kill events, and writes `agent_crash`
/// interruption events for any killed process whose name matches a known
/// agent or that has a pending llm_call correlation; kills with neither
/// signal are skipped as noise.
///
/// Uses the latest existing OOM event timestamp in the DB as `since_ns` to
/// avoid re-writing events from previous runs. Each event is also checked
/// individually by (pid, occurred_at_ns) before insertion.
pub fn recover_oom_events(
    interruption_store: &Arc<InterruptionStore>,
    genai_store: Option<&Arc<GenAISqliteStore>>,
    _since_ns: i64,
) {
    // Use the latest OOM event timestamp already in DB as the dedup cutoff
    let since_ns = interruption_store.latest_oom_event_ns();
    log::info!("OOM recovery: scanning dmesg, since_ns={since_ns}");

    let events = match parse_dmesg_oom_events() {
        Ok(e) => e,
        Err(err) => {
            log::warn!("OOM recovery: failed to read dmesg: {err}");
            return;
        }
    };

    if events.is_empty() {
        log::info!("OOM recovery: no OOM kill events found in dmesg");
        return;
    }

    let mut written = 0usize;
    for ev in &events {
        // Skip events at or before the last recorded OOM timestamp
        if since_ns > 0 && ev.timestamp_ns <= since_ns {
            continue;
        }

        // Per-event dedup: skip if already recorded with same (pid, timestamp)
        if interruption_store.oom_event_exists(ev.pid, ev.timestamp_ns) {
            log::debug!(
                "OOM recovery: skip duplicate pid={} ts={}",
                ev.pid,
                ev.timestamp_ns
            );
            continue;
        }

        // Try to correlate with genai_events to find active session/conversation
        // via pending (in-flight) LLM calls at OOM time.
        let (attributions, active_conversations): (
            Vec<(Option<String>, Option<String>)>,
            Vec<String>,
        ) = if let Some(gstore) = genai_store {
            match gstore.list_pending_for_pid(ev.pid) {
                Ok(pairs) => {
                    let convs: Vec<String> = pairs
                        .iter()
                        .filter_map(|(_, _, _, cid)| cid.clone())
                        .collect();
                    (group_pending_attributions(&pairs), convs)
                }
                Err(e) => {
                    log::debug!(
                        "OOM recovery: failed to query genai for pid={}: {}",
                        ev.pid,
                        e
                    );
                    (Vec::new(), Vec::new())
                }
            }
        } else {
            (Vec::new(), Vec::new())
        };

        // A known-agent kill with no pending correlation still gets one
        // unattributed event, as before. With correlation, emit one event per
        // conversation: the old `pairs.first()` attribution marked every
        // pending call interrupted while leaving all but the first
        // conversation without a parent agent_crash event.
        let attributions = if attributions.is_empty() {
            vec![(None, None)]
        } else {
            attributions
        };

        let mut wrote_for_pid = false;
        for (session_id, conversation_id) in attributions {
            let Some(interruption) =
                oom_interruption_for(ev, session_id, conversation_id, &active_conversations)
            else {
                // Neither a known-agent comm nor any pending llm_call
                // correlation: pure noise (a build job, a browser tab), not
                // an agent crash.
                log::debug!(
                    "OOM recovery: skip non-agent pid={} name={}",
                    ev.pid,
                    ev.process_name
                );
                continue;
            };

            match interruption_store.insert(&interruption) {
                Ok(_) => {
                    log::info!(
                        "OOM recovery: wrote agent_crash for pid={} name={} at {}",
                        ev.pid,
                        ev.process_name,
                        ev.timestamp_ns,
                    );
                    written += 1;
                    wrote_for_pid = true;
                }
                Err(e) => {
                    log::warn!(
                        "OOM recovery: failed to insert event for pid={}: {}",
                        ev.pid,
                        e
                    );
                }
            }
        }

        // The in-flight calls are the evidence the crash belonged to an agent,
        // and every live path that records the same fact marks them in the same
        // operation. Leaving them `pending` kept the trace view showing them in
        // flight, made the grader answer `ConversationNotReady`, and — once the
        // 60 s stale sweep eventually flipped the status — left
        // `interruption_type` NULL, so the call never linked to the crash that
        // ended it.
        if wrote_for_pid && let Some(gstore) = genai_store {
            match gstore.mark_pending_interrupted_for_pid(ev.pid as i32, "oom_crash") {
                Ok(0) => {}
                Ok(count) => log::info!(
                    "OOM recovery: marked {count} pending call(s) interrupted for pid={}",
                    ev.pid
                ),
                Err(e) => log::warn!(
                    "OOM recovery: failed to mark pending calls for pid={}: {}",
                    ev.pid,
                    e
                ),
            }
        }
    }

    log::info!(
        "OOM recovery: scanned {} OOM events, wrote {} new interruption records",
        events.len(),
        written,
    );
}

/// `dmesg` with its output environment pinned.
///
/// `dmesg -T` renders the timestamp with `strftime("%c")`, which follows the
/// inherited locale (`LC_ALL` outranks `LC_TIME`, which outranks `LANG`) and
/// the inherited time zone (`TZ`). On a host exporting e.g.
/// `LANG=zh_CN.UTF-8` the weekday and month names come out localized,
/// [`parse_dmesg_timestamp`] cannot read them, and every OOM event is stamped
/// with the scan time instead of the kill time — which also defeats the
/// `(pid, timestamp)` dedup in [`recover_oom_events`]. On a host east of UTC
/// the wall-clock fields are rendered in the local zone while the parser reads
/// them as UTC, so every kill is stamped hours away from when it happened.
/// Pin the child to the C locale and to UTC so the output keeps the format and
/// the instant the parser documents.
fn dmesg_command() -> Command {
    let mut command = Command::new("dmesg");
    command.env("LC_ALL", "C");
    command.env("TZ", "UTC");
    command
}

/// Distinct `(session_id, conversation_id)` pairs among a pid's pending calls.
///
/// Order is first-seen; an empty input yields an empty list, leaving the
/// caller to decide whether an unattributed event should be emitted. The
/// startup path writes one event per pair instead of only the first, so every
/// conversation whose calls will be marked interrupted keeps a parent event.
fn group_pending_attributions(
    pairs: &[(String, Option<String>, Option<String>, Option<String>)],
) -> Vec<(Option<String>, Option<String>)> {
    let mut groups: Vec<(Option<String>, Option<String>)> = Vec::new();
    for (_, session_id, _, conversation_id) in pairs {
        let key = (session_id.clone(), conversation_id.clone());
        if !groups.contains(&key) {
            groups.push(key);
        }
    }
    groups
}

/// Build the `agent_crash` interruption for one OOM kill, or `None` when the
/// kill is noise.
///
/// The match table is deliberately narrow (openclaw / cosh / node), but an
/// unmatched process that has in-flight LLM calls in genai_events is still
/// an agent-class kill — the correlation is the evidence — so it is recorded
/// with `agent_name: None` and the correlation's attribution, exactly as
/// before. Only a kill with neither a known-agent comm nor any pending
/// correlation (a build job, a browser tab, an unrelated worker) is skipped.
fn oom_interruption_for(
    ev: &OomKillEvent,
    session_id: Option<String>,
    conversation_id: Option<String>,
    active_conversations: &[String],
) -> Option<InterruptionEvent> {
    let agent_name = match_agent_name(&ev.process_name);
    if agent_name.is_none()
        && session_id.is_none()
        && conversation_id.is_none()
        && active_conversations.is_empty()
    {
        return None;
    }
    let mut detail = serde_json::json!({
        "pid": ev.pid,
        "process_name": ev.process_name,
        "agent_name": agent_name,
        "oom": true,
        "source": "dmesg",
    });
    if !active_conversations.is_empty() {
        detail["active_conversations"] = serde_json::json!(active_conversations);
    }
    Some(InterruptionEvent::new(
        InterruptionType::AgentCrash,
        session_id,
        None,
        conversation_id,
        None,
        Some(ev.pid),
        agent_name.map(|s| s.to_string()),
        ev.timestamp_ns,
        Some(detail),
    ))
}

/// Parse OOM kill events from `dmesg -T` output.
///
/// Looks for lines like:
///   [Fri Apr 17 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) ...
fn parse_dmesg_oom_events() -> Result<Vec<OomKillEvent>, Box<dyn std::error::Error>> {
    let output = dmesg_command()
        .arg("-T")
        .output()
        .map_err(|e| format!("failed to run dmesg: {e}"))?;

    if !output.status.success() {
        // Some systems require privileges; fall back to dmesg without -T
        let output2 = dmesg_command().output()?;
        return parse_dmesg_lines(&String::from_utf8_lossy(&output2.stdout));
    }

    parse_dmesg_lines(&String::from_utf8_lossy(&output.stdout))
}

fn parse_dmesg_lines(content: &str) -> Result<Vec<OomKillEvent>, Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    // Boot epoch, only needed for boot-relative stamps; read once per scan.
    let boot_ns = boot_time_ns();

    for line in content.lines() {
        // Summary format: "Killed process <pid> (<name>)"
        // Structured format (memcg OOM): "oom-kill:...,task=<name>,pid=<pid>,..."
        // Both must be recognised — memcg OOM on modern kernels (5.0+) emits
        // only the structured line, so matching the summary alone misses it (#3130).
        let parsed = if line.contains("Killed process") {
            parse_killed_process(line)
        } else if line.contains("oom-kill:") {
            parse_oom_kill_structured(line)
        } else {
            continue;
        };

        let (pid, process_name) = match parsed {
            Some(v) => v,
            None => continue,
        };

        // `dmesg -T` renders an absolute wall-clock stamp; the plain-dmesg
        // fallback (busybox/Alpine, older util-linux) renders seconds since
        // boot. Convert the latter via the boot epoch so the value is stable
        // across scans. A line whose time cannot be determined is skipped
        // rather than stamped with the scan time: a fabricated "now" changes
        // on every scan, so the (pid, timestamp) dedup never matches and every
        // restart re-inserts all historical OOM kills as fresh events.
        let timestamp_ns = match parse_dmesg_timestamp(line) {
            Some(ts) => ts,
            None => match (parse_boot_offset_ns(line), boot_ns) {
                (Some(offset), Some(boot)) => boot.saturating_add(offset),
                _ => continue,
            },
        };

        events.push(OomKillEvent {
            timestamp_ns,
            pid,
            process_name,
        });
    }

    Ok(events)
}

/// Extract (pid, process_name) from a dmesg line containing "Killed process".
///
/// Handles: "Killed process 12345 (openclaw-gatewa)"
fn parse_killed_process(line: &str) -> Option<(i32, String)> {
    // Find "Killed process " then parse pid and (name)
    let after = line.split("Killed process ").nth(1)?;
    // after = "12345 (openclaw-gatewa) ..."
    let mut parts = after.splitn(2, ' ');
    let pid_str = parts.next()?;
    let rest = parts.next().unwrap_or("");

    let pid: i32 = pid_str.trim().parse().ok()?;

    // Extract name between first '(' and ')'
    let name = rest
        .split('(')
        .nth(1)
        .and_then(|s| s.split(')').next())
        .unwrap_or("")
        .to_string();

    if name.is_empty() {
        return None;
    }

    Some((pid, name))
}

/// Extract (pid, process_name) from a structured `oom-kill:` dmesg line.
///
/// Format: `oom-kill:constraint=...,task=<name>,pid=<pid>,uid=<uid>`
/// This is the only line emitted by memcg OOM kills on kernels 5.0+.
fn parse_oom_kill_structured(line: &str) -> Option<(i32, String)> {
    let after = line.split("oom-kill:").nth(1)?;
    let mut pid = None;
    let mut task = None;
    for field in after.split(',') {
        let field = field.trim();
        if let Some(v) = field.strip_prefix("pid=") {
            pid = v.parse::<i32>().ok();
        } else if let Some(v) = field.strip_prefix("task=") {
            task = Some(v.to_string());
        }
    }
    match (pid, task) {
        (Some(p), Some(t)) if !t.is_empty() => Some((p, t)),
        _ => None,
    }
}

/// Parse timestamp from dmesg -T format: "[Fri Apr 17 15:58:28 2026]"
/// Returns nanoseconds since Unix epoch, or None if parsing fails.
fn parse_dmesg_timestamp(line: &str) -> Option<i64> {
    // Format: [Fri Apr 17 15:58:28 2026]
    let start = line.find('[')?;
    let end = line.find(']')?;
    if end <= start {
        return None;
    }
    let ts_str = line[start + 1..end].trim();
    // ts_str = "Fri Apr 17 15:58:28 2026"
    // Fields: weekday(0) month(1) day(2) time(3) year(4)
    let parts: Vec<&str> = ts_str.split_whitespace().collect();
    if parts.len() < 5 {
        return None;
    }
    // Reconstruct as "17 Apr 2026 15:58:28" for a stable parse
    let normalised = format!("{} {} {} {}", parts[2], parts[1], parts[4], parts[3]);
    let dt = chrono::NaiveDateTime::parse_from_str(&normalised, "%d %b %Y %T").ok()?;
    // The child's environment pins `TZ=UTC` (see `dmesg_command`), so the
    // wall-clock fields are UTC and the plain conversion below is exact; the
    // non-`-T` fallback renders boot-relative seconds, which `line_timestamp_ns`
    // resolves against the boot epoch instead of this function.
    dt.and_utc().timestamp_nanos_opt()
}

/// Whether a comm names a metrics collector rather than something that runs
/// the agent.
///
/// Prometheus-style exporters are named after the thing they watch —
/// `node_exporter`, `node-exporter`, `prometheus-node-exporter` — so they look
/// like a runtime to a prefix match but are collectors, not workloads. ktuner
/// draws the same line for its own process detection.
fn is_monitoring_helper_name(name: &str) -> bool {
    name.starts_with("prometheus")
        || name
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token == "exporter")
}

/// Match a process comm name to a known agent name.
/// Returns Some(agent_name) if matched, None otherwise.
fn match_agent_name(comm: &str) -> Option<&'static str> {
    let comm_lower = comm.to_lowercase();
    if comm_lower.starts_with("openclaw-gatewa") || comm_lower.starts_with("openclaw") {
        Some("OpenClaw")
    } else if comm_lower == "co" || comm_lower == "cosh" || comm_lower.starts_with("copilot") {
        Some("Cosh")
    } else if comm_lower.starts_with("node") {
        // Node processes could be either; record with unknown agent but still
        // track. A collector is neither: an OOM-killed `node_exporter` without
        // a pending llm_call is noise, like any other non-agent kill, and used
        // to be recorded as a critical agent_crash.
        if is_monitoring_helper_name(&comm_lower) {
            None
        } else {
            Some("node(unknown-agent)")
        }
    } else {
        // Non-agent processes — skip
        None
    }
}

/// Check if a specific PID was OOM-killed recently by scanning dmesg.
///
/// This is used by the HealthChecker for real-time OOM attribution:
/// when an agent process disappears, we check dmesg to determine if it
/// was killed by the OOM killer (vs normal exit, SIGKILL, segfault, etc.).
///
/// Returns `true` if the PID appears in a recent OOM kill line in dmesg
/// (either format accepted by [`line_matches_oom_kill`] and a timestamp
/// within [`OOM_RECENCY_WINDOW_SECS`]).
pub fn was_pid_oom_killed(pid: i32) -> bool {
    let output = match dmesg_command().arg("-T").output() {
        Ok(o) if o.status.success() => o,
        Ok(_) => {
            // Fallback without -T
            match dmesg_command().output() {
                Ok(o) => o,
                Err(_) => return false,
            }
        }
        Err(_) => return false,
    };

    let content = String::from_utf8_lossy(&output.stdout);
    let pid_str = pid.to_string();
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    let cutoff_ns = now_ns.saturating_sub(OOM_RECENCY_WINDOW_SECS * 1_000_000_000);
    // Only needed for the plain-dmesg boot-relative fallback; read once.
    let boot_ns = boot_time_ns();

    content
        .lines()
        .any(|line| line_matches_recent_oom_kill(line, &pid_str, cutoff_ns, boot_ns))
}

/// Window within which a dmesg OOM kill may be attributed to a process that
/// just disappeared.
///
/// Must exceed the slowest detector's lag (serve-mode HealthChecker: 30s cycle
/// plus the drain paths in trace mode), while still excluding kills from
/// earlier in the boot and recycled pid numbers — the dmesg ring buffer keeps
/// them for the whole boot.
const OOM_RECENCY_WINDOW_SECS: i64 = 300;

/// Returns `true` when a line attributes an OOM kill to `pid_str` *and* the
/// kill's timestamp is at or after `cutoff_ns`.
///
/// The pid match alone is not enough: dmesg keeps earlier kills for the whole
/// boot and pid numbers get recycled, so an un-bounded match reports a fresh
/// crash for a process that merely reused the number, and callers stamp the
/// wrong root cause (`oom: true` / `oom_crash`).
///
/// A line whose time cannot be resolved (neither a `dmesg -T` wall clock nor
/// a boot-relative stamp with a known boot epoch) cannot be shown to be
/// recent; it does not match.
fn line_matches_recent_oom_kill(
    line: &str,
    pid_str: &str,
    cutoff_ns: i64,
    boot_ns: Option<i64>,
) -> bool {
    if !line_matches_oom_kill(line, pid_str) {
        return false;
    }
    match line_timestamp_ns(line, boot_ns) {
        Some(ts) => ts >= cutoff_ns,
        None => false,
    }
}

/// Resolve a dmesg line's `[...]` prefix to epoch nanoseconds.
///
/// `dmesg -T` renders an absolute local wall clock; plain `dmesg` (the
/// unprivileged fallback) renders seconds since boot, which need `boot_ns`
/// (from `/proc/stat`'s `btime`) to become an absolute instant.
fn line_timestamp_ns(line: &str, boot_ns: Option<i64>) -> Option<i64> {
    if let Some(ns) = parse_dmesg_timestamp(line) {
        return Some(ns);
    }
    let boot = boot_ns?;
    parse_boot_offset_ns(line).map(|offset| boot.saturating_add(offset))
}

/// Parse the boot-relative prefix plain `dmesg` emits when `-T` is
/// unavailable: "[  123.456789]" -> nanoseconds since boot.
///
/// Returns `None` for bracket contents that are not a number (e.g. the
/// `dmesg -T` weekday form, `[ts]` placeholders), so the caller can fall
/// through to "cannot date this kill".
fn parse_boot_offset_ns(line: &str) -> Option<i64> {
    let start = line.find('[')?;
    let end = line.find(']')?;
    if end <= start {
        return None;
    }
    let ts_str = line[start + 1..end].trim();
    let (secs_str, frac_str) = match ts_str.split_once('.') {
        Some((secs, frac)) => (secs, frac),
        None => (ts_str, ""),
    };
    let secs: i64 = secs_str.parse().ok()?;
    let mut frac_ns: i64 = 0;
    if !frac_str.is_empty() {
        // Kernel printk emits microsecond precision; accept up to nanoseconds
        // and reject anything non-numeric rather than guessing.
        let digits: String = frac_str.chars().take(9).collect();
        let value: i64 = digits.parse().ok()?;
        frac_ns = value.checked_mul(10i64.checked_pow(9 - digits.len() as u32)?)?;
    }
    secs.checked_mul(1_000_000_000)?.checked_add(frac_ns)
}

/// Wall-clock time of system boot in epoch nanoseconds, read from
/// `/proc/stat`'s `btime` line.
///
/// Unlike `now - uptime`, which moves with the scan, `btime` is fixed, so
/// the same boot-relative dmesg line always maps to the same instant. `None`
/// when the file is unreadable or carries no numeric `btime`, in which case
/// boot-relative kills cannot be dated.
fn boot_time_ns() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().find(|l| l.starts_with("btime "))?;
    let secs: i64 = line.trim_start_matches("btime ").trim().parse().ok()?;
    secs.checked_mul(1_000_000_000)
}

/// Returns `true` if a single dmesg line attributes an OOM kill to `pid_str`.
///
/// Two kernel formats are recognised:
/// - Summary line (all kernels): `Out of memory: Killed process <pid> (<name>) ...`
/// - Structured line (kernel 5.0+, commit ef8444ea01d7):
///   `oom-kill:constraint=...,task=<name>,pid=<pid>,uid=<uid>`
///   On some systems (e.g. memcg OOM on 6.6.x) this is the only line
///   reliably present, so matching the summary line alone misses the kill.
///
/// Invariant: the pid comparison is whole-token (`==`), never a prefix
/// match, so pid 6693 must not match a line reporting pid 669334.
fn line_matches_oom_kill(line: &str, pid_str: &str) -> bool {
    // Summary format: token after "Killed process " is the full pid.
    if let Some(after) = line.split("Killed process ").nth(1) {
        if after.split_whitespace().next() == Some(pid_str) {
            return true;
        }
    }

    // Structured format: comma-separated key=value fields after "oom-kill:".
    if let Some(after) = line.split("oom-kill:").nth(1) {
        for field in after.split(',') {
            if let Some(value) = field.trim().strip_prefix("pid=") {
                if value == pid_str {
                    return true;
                }
            }
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real dmesg line observed on kernel 6.6.102 (memcg OOM, e2e evidence).
    const STRUCTURED_LINE: &str = "[Sat Jul 25 10:00:00 2026] oom-kill:constraint=CONSTRAINT_MEMCG,nodemask=(null),cpuset=/,mems_allowed=0,oom_memcg=/user.slice,task_memcg=/user.slice/session-1.scope,task=python3,pid=669334,uid=0";

    const SUMMARY_LINE: &str = "[Fri Apr 17 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB";

    #[test]
    fn structured_line_matches_target_pid() {
        assert!(line_matches_oom_kill(STRUCTURED_LINE, "669334"));
    }

    #[test]
    fn structured_line_rejects_prefix_pid() {
        // pid 6693 is a strict prefix of the line's pid 669334 — must not match.
        assert!(!line_matches_oom_kill(STRUCTURED_LINE, "6693"));
        // Nor the other direction: querying a longer pid than the line's.
        assert!(!line_matches_oom_kill(
            "[ts] oom-kill:constraint=CONSTRAINT_NONE,task=node,pid=6693,uid=1000",
            "669334"
        ));
    }

    #[test]
    fn summary_line_matches_target_pid() {
        assert!(line_matches_oom_kill(SUMMARY_LINE, "12345"));
        assert!(!line_matches_oom_kill(SUMMARY_LINE, "1234"));
    }

    #[test]
    fn unrelated_line_does_not_match() {
        assert!(!line_matches_oom_kill(
            "[ts] audit: type=1400 pid=669334 comm=python3",
            "669334"
        ));
        assert!(!line_matches_oom_kill("", "669334"));
    }

    // ─── was_pid_oom_killed: recency window ────────────────────────────────

    /// A `dmesg -T` stand-in that emits one kill from years ago (still in the
    /// ring buffer) and one dated now. The un-bounded matcher reports both as
    /// OOM kills, so a pid recycled from an old victim was marked `oom_crash`
    /// with the wrong root cause.
    const FAKE_DMESG_RECENCY: &str = r#"#!/bin/sh
printf '[Fri Apr 17 10:00:00 2020] Out of memory: Killed process 4000001 (openclaw-gatewa) total-vm:1024kB\n'
printf '[%s] Out of memory: Killed process 4000002 (openclaw-gatewa) total-vm:1024kB\n' "$(date '+%a %b %e %T %Y')"
"#;

    #[cfg(unix)]
    #[test]
    fn was_pid_oom_killed_ignores_stale_dmesg_kills() {
        const CHILD: &str = "AGENTSIGHT_OOM_RECENCY_CHILD";
        let fake_dir = std::env::temp_dir().join(format!(
            "agentsight-fake-dmesg-recency-{}",
            std::process::id()
        ));

        if std::env::var_os(CHILD).is_none() {
            std::fs::create_dir_all(&fake_dir).expect("create fake dmesg directory");
            let script = fake_dir.join("dmesg");
            std::fs::write(&script, FAKE_DMESG_RECENCY).expect("write fake dmesg");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                    .expect("make fake dmesg executable");
            }
            let path_var = format!(
                "{}:{}",
                fake_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let output = Command::new(std::env::current_exe().expect("test binary path"))
                .args([
                    "--exact",
                    "interruption::oom_recovery::tests::was_pid_oom_killed_ignores_stale_dmesg_kills",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("PATH", path_var)
                .output()
                .expect("re-exec the test binary");
            let _ = std::fs::remove_dir_all(&fake_dir);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "child test did not pass: {:?}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        // The 2020 kill is long outside any recency window: a process that
        // merely recycled pid 4000001 must not be attributed to the OOM killer.
        assert!(
            !was_pid_oom_killed(4_000_001),
            "a kill from years earlier in the ring buffer must not count as recent"
        );
        // A kill dated now must still be detected.
        assert!(
            was_pid_oom_killed(4_000_002),
            "a freshly dated dmesg kill must still be attributed"
        );
    }

    /// A `dmesg -T` stand-in that emits one recent kill of a known agent.
    const FAKE_DMESG_MARK: &str = r#"#!/bin/sh
printf '[%s] Out of memory: Killed process 4000003 (openclaw-gatewa) total-vm:1024kB\n' "$(date '+%a %b %e %T %Y')"
"#;

    /// `recover_oom_events` reads the killed pid's in-flight calls to prove it
    /// was an agent, writes the `agent_crash` rows — and left those calls
    /// `pending`. Every live path that records the same fact for the same pid
    /// marks them in the same operation, so after a restart the crash event and
    /// the call status disagreed, and the cause link (`interruption_type`) was
    /// never written at all.
    #[cfg(unix)]
    #[test]
    fn recovered_oom_kills_mark_the_correlated_calls_interrupted() {
        use crate::config::PeriodicStoragePolicy;
        use crate::storage::sqlite::GenAISqliteStore;
        use crate::storage::sqlite::genai::PendingCallInfo;

        const CHILD: &str = "AGENTSIGHT_OOM_MARK_CHILD";
        let fake_dir =
            std::env::temp_dir().join(format!("agentsight-fake-dmesg-mark-{}", std::process::id()));

        if std::env::var_os(CHILD).is_none() {
            std::fs::create_dir_all(&fake_dir).expect("create fake dmesg directory");
            let script = fake_dir.join("dmesg");
            std::fs::write(&script, FAKE_DMESG_MARK).expect("write fake dmesg");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                    .expect("make fake dmesg executable");
            }
            let path_var = format!(
                "{}:{}",
                fake_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let output = Command::new(std::env::current_exe().expect("test binary path"))
                .args([
                    "--exact",
                    "interruption::oom_recovery::tests::recovered_oom_kills_mark_the_correlated_calls_interrupted",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("TZ", "UTC")
                .env("PATH", path_var)
                .output()
                .expect("re-exec the test binary");
            let _ = std::fs::remove_dir_all(&fake_dir);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "child test did not pass: {:?}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let killed_pid = 4_000_003u32;
        let dir = std::env::temp_dir().join(format!("oom-mark-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create fixture dir");
        let istore = Arc::new(
            InterruptionStore::new_with_path(&dir.join("interruption_events.db"))
                .expect("interruption store"),
        );
        let genai_path = dir.join("genai_events.db");
        let genai = Arc::new(
            GenAISqliteStore::new_with_path(&genai_path, PeriodicStoragePolicy::default())
                .expect("genai store"),
        );
        genai
            .insert_pending(&PendingCallInfo {
                call_id: "oom-mark".to_string(),
                trace_id: None,
                conversation_id: Some("conv-oom-mark".to_string()),
                session_id: None,
                start_timestamp_ns: 1_000_000_000,
                pid: killed_pid as i32,
                process_name: "openclaw-gatewa".to_string(),
                agent_name: Some("OpenClaw".to_string()),
                http_method: Some("POST".to_string()),
                http_path: Some("/v1/chat/completions".to_string()),
                input_messages: None,
                system_instructions: None,
                user_query: None,
                is_sse: false,
                model: Some("gpt-4".to_string()),
                provider: Some("openai".to_string()),
                call_kind: "main".to_string(),
                pending_origin: crate::storage::sqlite::genai::PendingOrigin::RequestCapture,
                pending_match_key: None,
            })
            .expect("insert_pending");

        recover_oom_events(&istore, Some(&genai), 0);

        assert!(
            genai
                .list_pending_for_pids(&[killed_pid as i32])
                .expect("list pending")
                .is_empty(),
            "a recovered OOM kill must mark its correlated calls interrupted"
        );
        // The cause link matters as much as the status: the stale sweep flips
        // the status alone, leaving the call unattributable to its crash.
        let conn = rusqlite::Connection::open(&genai_path).expect("read the fixture database");
        let (status, interruption_type): (String, Option<String>) = conn
            .query_row(
                "SELECT status, interruption_type FROM genai_events WHERE call_id = 'oom-mark'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the call row");
        assert_eq!(status, "interrupted");
        assert_eq!(interruption_type.as_deref(), Some("oom_crash"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn oom_recency_gate_rejects_stale_lines_and_accepts_fresh_ones() {
        // The wall-clock lines are a day apart and the cutoff sits between
        // them, so no real UTC offset (chrono::Local is host-dependent) can
        // flip either comparison.
        let stale = "[Wed Apr 16 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB";
        let fresh = "[Fri Apr 18 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB";
        let cutoff_ns = 1_776_420_000_000_000_000; // 2026-04-17T10:00:00Z
        assert!(!line_matches_recent_oom_kill(
            stale, "12345", cutoff_ns, None
        ));
        assert!(line_matches_recent_oom_kill(
            fresh, "12345", cutoff_ns, None
        ));
        // Pid mismatch stays rejected regardless of recency.
        assert!(!line_matches_recent_oom_kill(fresh, "999", cutoff_ns, None));

        // Boot-relative stamps (plain-dmesg fallback) resolve via `btime`.
        let boot_line =
            "[  123.456789] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB";
        let boot_ns = 1_776_420_000_000_000_000i64;
        let kill_ns = boot_ns + 123_456_789_000;
        assert!(line_matches_recent_oom_kill(
            boot_line,
            "12345",
            kill_ns - 1,
            Some(boot_ns)
        ));
        assert!(!line_matches_recent_oom_kill(
            boot_line,
            "12345",
            kill_ns + 1,
            Some(boot_ns)
        ));
        // Without a boot epoch the kill cannot be dated and is not "recent".
        assert!(!line_matches_recent_oom_kill(boot_line, "12345", 0, None));
        // An undatable placeholder bracket is likewise not recent.
        assert!(!line_matches_recent_oom_kill(
            "[ts] Out of memory: Killed process 12345 (openclaw-gatewa)",
            "12345",
            0,
            None
        ));
    }

    // ─── dmesg output locale (startup recovery timestamps) ────────────────

    /// A `dmesg` stand-in that mimics util-linux: `-T` renders the timestamp
    /// with `strftime("%c")`, so the weekday and month names follow the
    /// inherited locale (`LC_ALL` outranks `LC_TIME`, which outranks `LANG`)
    /// and the wall-clock fields follow the inherited time zone (`TZ`).
    ///
    /// The stand-in collapses the zone to two cases — UTC keeps the kernel
    /// time, anything else renders it as UTC+8 — which is enough to tell the
    /// two apart without shipping a tz database.
    const FAKE_DMESG: &str = r#"#!/bin/sh
case "${TZ:-}" in
    ""|UTC|UTC0|Etc/UTC|Etc/GMT)
        hour='10:00:00'
        ;;
    *)
        hour='18:00:00'
        ;;
esac
case "${LC_ALL:-${LC_TIME:-${LANG:-}}}" in
    ""|C|POSIX|C.*)
        printf "[Fri Apr 17 $hour 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB\n"
        ;;
    *)
        printf "[五 4月 17 $hour 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB\n"
        ;;
esac
"#;

    #[cfg(unix)]
    #[test]
    fn oom_recovery_reads_dmesg_timestamps_under_a_foreign_locale() {
        // The parser only understands the C-locale `%b` form. Parse the fake
        // dmesg output in a re-executed child whose locale is foreign and
        // whose PATH finds the fake, so the assertion covers the command the
        // recovery path actually spawns without mutating other tests' env.
        const CHILD: &str = "AGENTSIGHT_OOM_LOCALE_CHILD";
        let fake_dir =
            std::env::temp_dir().join(format!("agentsight-fake-dmesg-{}", std::process::id()));

        if std::env::var_os(CHILD).is_none() {
            std::fs::create_dir_all(&fake_dir).expect("create fake dmesg directory");
            let script = fake_dir.join("dmesg");
            std::fs::write(&script, FAKE_DMESG).expect("write fake dmesg");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                    .expect("make fake dmesg executable");
            }
            let path_var = format!(
                "{}:{}",
                fake_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let output = Command::new(std::env::current_exe().expect("test binary path"))
                .args([
                    "--exact",
                    "interruption::oom_recovery::tests::oom_recovery_reads_dmesg_timestamps_under_a_foreign_locale",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("LC_ALL", "zh_CN.UTF-8")
                .env("LANG", "zh_CN.UTF-8")
                // Pin the zone so the expected epoch below is the UTC reading
                // on every host; the child environment is the only thing that
                // decides this, so the assertion holds regardless of the
                // developer's TZ.
                .env("TZ", "UTC")
                .env("PATH", path_var)
                .output()
                .expect("re-exec the test binary");
            let _ = std::fs::remove_dir_all(&fake_dir);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "child test did not pass: {:?}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let events = parse_dmesg_oom_events().expect("read fake dmesg");
        let event = events
            .iter()
            .find(|event| event.pid == 12345)
            .expect("killed process event");
        // "[Fri Apr 17 10:00:00 2026]" -> 2026-04-17T10:00:00Z
        assert_eq!(
            event.timestamp_ns, 1_776_420_000_000_000_000,
            "the C-locale timestamp must survive a foreign LC_TIME"
        );
    }

    #[cfg(unix)]
    #[test]
    fn oom_recovery_reads_dmesg_timestamps_under_a_foreign_timezone() {
        // `dmesg -T` renders the kernel time in the *local* zone, but the
        // recovery path reads the fields as UTC. A host east of UTC therefore
        // records every OOM kill hours in the future. Re-exec so the fake
        // dmesg sees a foreign TZ without mutating other tests' env.
        const CHILD: &str = "AGENTSIGHT_OOM_TIMEZONE_CHILD";
        let fake_dir =
            std::env::temp_dir().join(format!("agentsight-fake-dmesg-tz-{}", std::process::id()));

        if std::env::var_os(CHILD).is_none() {
            std::fs::create_dir_all(&fake_dir).expect("create fake dmesg directory");
            let script = fake_dir.join("dmesg");
            std::fs::write(&script, FAKE_DMESG).expect("write fake dmesg");
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                    .expect("make fake dmesg executable");
            }
            let path_var = format!(
                "{}:{}",
                fake_dir.display(),
                std::env::var("PATH").unwrap_or_default()
            );
            let output = Command::new(std::env::current_exe().expect("test binary path"))
                .args([
                    "--exact",
                    "interruption::oom_recovery::tests::oom_recovery_reads_dmesg_timestamps_under_a_foreign_timezone",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .env("LC_ALL", "C")
                .env("TZ", "Asia/Shanghai")
                .env("PATH", path_var)
                .output()
                .expect("re-exec the test binary");
            let _ = std::fs::remove_dir_all(&fake_dir);
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains("1 passed"),
                "child test did not pass: {:?}\n{stdout}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let events = parse_dmesg_oom_events().expect("read fake dmesg");
        let event = events
            .iter()
            .find(|event| event.pid == 12345)
            .expect("killed process event");
        // The fake renders 18:00 in UTC+8, which is 10:00Z. Reading the wall
        // clock as UTC would stamp the kill eight hours into the future.
        assert_eq!(
            event.timestamp_ns, 1_776_420_000_000_000_000,
            "the kill time must be read as the real instant, not as UTC wall clock"
        );
    }

    /// `dmesg -T` renders the stamp as local wall-clock time, so the parser
    /// must resolve it in the host's local zone. Without that, every recovered
    /// event is shifted by the UTC offset (e.g. +8h on Asia/Shanghai) and
    /// falls outside time-range queries, retention windows and correlation.
    ///
    /// Re-executed in a child so TZ can be set without mutating the parallel
    /// test process. `TZ=UTC-8` is POSIX for UTC+08:00 and, unlike an IANA
    /// name, needs no tzdata in the container.
    #[cfg(unix)]
    // ─── parse_dmesg_lines: startup recovery path (#3130) ─────────────────
    #[test]
    fn parse_dmesg_lines_recognises_structured_oom_kill() {
        // The startup recovery path must recognise the same structured format
        // that the real-time path (was_pid_oom_killed) already does.
        let dmesg = format!(
            "{}\n{}\n",
            "[Fri Apr 17 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB",
            STRUCTURED_LINE,
        );
        let events = parse_dmesg_lines(&dmesg).expect("parse");
        assert_eq!(events.len(), 2, "both formats must be recognised");
        // Summary line
        assert_eq!(events[0].pid, 12345);
        assert_eq!(events[0].process_name, "openclaw-gatewa");
        // Structured line
        assert_eq!(events[1].pid, 669334);
        assert_eq!(events[1].process_name, "python3");
    }

    #[test]
    fn boot_relative_dmesg_lines_get_a_stable_timestamp() {
        // When `dmesg -T` is unavailable (busybox/Alpine, older util-linux)
        // the fallback prints boot-relative stamps ("[  123.456789]"), which
        // parse_dmesg_timestamp cannot read. Stamping those with the scan
        // time makes the value change on every run, so the (pid, timestamp)
        // dedup can never match and every restart re-inserts all historical
        // OOM kills as fresh events. The derived time must be stable.
        let line =
            "[  123.456789] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB";
        let first = parse_dmesg_lines(line).expect("parse");
        std::thread::sleep(std::time::Duration::from_millis(5));
        let second = parse_dmesg_lines(line).expect("parse");
        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(
            first[0].timestamp_ns, second[0].timestamp_ns,
            "a boot-relative line must not be restamped with the scan time"
        );
    }

    #[test]
    fn parse_dmesg_lines_structured_only() {
        // memcg-only OOM: no summary line, only the structured one.
        let events = parse_dmesg_lines(STRUCTURED_LINE).expect("parse");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].pid, 669334);
        assert_eq!(events[0].process_name, "python3");
    }

    #[test]
    fn parse_dmesg_lines_skips_malformed_structured() {
        // Missing pid= or task= must not produce a bogus event.
        assert!(
            parse_oom_kill_structured("[ts] oom-kill:constraint=CONSTRAINT_MEMCG,task=node,uid=0")
                .is_none()
        );
        assert!(
            parse_oom_kill_structured("[ts] oom-kill:constraint=CONSTRAINT_MEMCG,pid=1234,uid=0")
                .is_none()
        );
        assert!(parse_oom_kill_structured("").is_none());
    }

    // ─── oom_interruption_for: only agent kills become agent_crash ──────────

    fn oom_event(process_name: &str) -> OomKillEvent {
        OomKillEvent {
            timestamp_ns: 1_700_000_000_000_000_000,
            pid: 4242,
            process_name: process_name.to_string(),
        }
    }

    #[test]
    fn non_agent_oom_kill_is_not_an_agent_crash() {
        // No known-agent comm AND no pending llm_call correlation: pure noise
        // (a build job, a browser tab); writing it with agent_name: None and
        // no attribution produced false critical agent_crash records.
        assert!(oom_interruption_for(&oom_event("python3"), None, None, &[]).is_none());
        assert!(oom_interruption_for(&oom_event("chrome"), None, None, &[]).is_none());
        assert!(oom_interruption_for(&oom_event("mysqld"), None, None, &[]).is_none());
    }

    #[test]
    fn monitoring_exporter_oom_kill_is_not_an_agent_crash() {
        // A metrics collector is named after what it watches, not after a Node
        // agent runtime: `node_exporter` is Prometheus's node collector. With
        // no pending llm_call correlation the kill is noise, like any other
        // non-agent kill — but `starts_with("node")` recorded it as a critical
        // agent_crash.
        assert!(oom_interruption_for(&oom_event("node_exporter"), None, None, &[]).is_none());
        assert!(oom_interruption_for(&oom_event("node-exporter"), None, None, &[]).is_none());

        // A bare `node` runtime — what agent processes run as — still matches,
        // and so do node-ish names that are not collectors.
        assert!(oom_interruption_for(&oom_event("node"), None, None, &[]).is_some());
        assert!(oom_interruption_for(&oom_event("node-red"), None, None, &[]).is_some());

        // The correlation safety net is unchanged: an exporter kill that has a
        // pending llm_call for its pid is still recorded.
        assert!(
            oom_interruption_for(
                &oom_event("node_exporter"),
                Some("sess-1".to_string()),
                Some("conv-1".to_string()),
                &["conv-1".to_string()],
            )
            .is_some()
        );
    }

    #[test]
    fn unmatched_oom_kill_with_pending_correlation_is_kept() {
        // A claude/qwen/codex-class comm is not in the narrow match table,
        // but a pending llm_call row for the pid is the evidence this was an
        // agent-class kill: the event must keep its session attribution and
        // agent_name: None, exactly as before the noise skip.
        let interruption = oom_interruption_for(
            &oom_event("claude"),
            Some("sess-9".to_string()),
            Some("conv-9".to_string()),
            &["conv-9".to_string()],
        )
        .expect("correlation is evidence of an agent kill");
        assert_eq!(interruption.agent_name, None);
        assert_eq!(interruption.session_id.as_deref(), Some("sess-9"));
        assert_eq!(interruption.conversation_id.as_deref(), Some("conv-9"));
        assert!(matches!(
            interruption.interruption_type,
            InterruptionType::AgentCrash
        ));
        let detail: serde_json::Value =
            serde_json::from_str(&interruption.detail.expect("detail")).expect("valid json");
        assert_eq!(detail["agent_name"], serde_json::Value::Null);
        assert_eq!(detail["active_conversations"][0], "conv-9");
    }

    #[test]
    fn agent_oom_kill_keeps_its_detail_shape() {
        let interruption = oom_interruption_for(
            &oom_event("openclaw-gatewa"),
            Some("sess-1".to_string()),
            Some("conv-1".to_string()),
            &["conv-1".to_string()],
        )
        .expect("a kill of a known agent runtime must be recorded");
        assert!(matches!(
            interruption.interruption_type,
            InterruptionType::AgentCrash
        ));
        assert_eq!(interruption.agent_name.as_deref(), Some("OpenClaw"));
        assert_eq!(interruption.pid, Some(4242));
        assert_eq!(interruption.session_id.as_deref(), Some("sess-1"));
        let detail: serde_json::Value =
            serde_json::from_str(&interruption.detail.expect("detail")).expect("valid json");
        assert_eq!(detail["agent_name"], "OpenClaw");
        assert_eq!(detail["oom"], true);
        assert_eq!(detail["source"], "dmesg");
        assert_eq!(detail["active_conversations"][0], "conv-1");
    }

    /// One OOM kill of a multi-session agent must produce one event per
    /// conversation: taking only `pairs.first()` attributed the crash to the
    /// first conversation while every pending call (all conversations) was
    /// marked interrupted, leaving the rest without a parent event.
    #[test]
    fn oom_recovery_attributions_cover_every_pending_conversation() {
        let pairs = vec![
            (
                "c1".to_string(),
                Some("s1".to_string()),
                None,
                Some("conv-1".to_string()),
            ),
            (
                "c2".to_string(),
                Some("s2".to_string()),
                None,
                Some("conv-2".to_string()),
            ),
            // A duplicate conversation collapses into a single attribution.
            (
                "c3".to_string(),
                Some("s1".to_string()),
                None,
                Some("conv-1".to_string()),
            ),
        ];
        assert_eq!(
            group_pending_attributions(&pairs),
            vec![
                (Some("s1".to_string()), Some("conv-1".to_string())),
                (Some("s2".to_string()), Some("conv-2".to_string())),
            ]
        );
        assert!(group_pending_attributions(&[]).is_empty());
    }
}
