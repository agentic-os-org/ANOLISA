//! OOM recovery module
//!
//! On AgentSight startup, scans `dmesg` for OOM kill events that occurred
//! after the last known AgentSight shutdown timestamp. For each killed process
//! that matches a known agent name, an `agent_crash` InterruptionEvent is
//! written to the interruption store with `oom: true` in its detail JSON.
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
/// interruption events for any killed process whose name matches a known agent.
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

        // Match against known agent process name prefixes
        let agent_name = match_agent_name(&ev.process_name);

        // Try to correlate with genai_events to find active session/conversation
        // via pending (in-flight) LLM calls at OOM time.
        let (session_id, conversation_id, active_conversations): (
            Option<String>,
            Option<String>,
            Vec<String>,
        ) = if let Some(gstore) = genai_store {
            match gstore.list_pending_for_pid(ev.pid) {
                Ok(pairs) => {
                    let primary = pairs.first();
                    let convs: Vec<String> = pairs
                        .iter()
                        .filter_map(|(_, _, _, cid)| cid.clone())
                        .collect();
                    (
                        primary.and_then(|(_, sid, _, _)| sid.clone()),
                        primary.and_then(|(_, _, _, cid)| cid.clone()),
                        convs,
                    )
                }
                Err(e) => {
                    log::debug!(
                        "OOM recovery: failed to query genai for pid={}: {}",
                        ev.pid,
                        e
                    );
                    (None, None, Vec::new())
                }
            }
        } else {
            (None, None, Vec::new())
        };

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

        let interruption = InterruptionEvent::new(
            InterruptionType::AgentCrash,
            session_id,
            None,
            conversation_id,
            None,
            Some(ev.pid),
            agent_name.map(|s| s.to_string()),
            ev.timestamp_ns,
            Some(detail),
        );

        match interruption_store.insert(&interruption) {
            Ok(_) => {
                log::info!(
                    "OOM recovery: wrote agent_crash for pid={} name={} at {}",
                    ev.pid,
                    ev.process_name,
                    ev.timestamp_ns,
                );
                written += 1;
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

    log::info!(
        "OOM recovery: scanned {} OOM events, wrote {} new interruption records",
        events.len(),
        written,
    );
}

/// `dmesg` with its output locale pinned.
///
/// `dmesg -T` renders the timestamp with `strftime("%c")`, which follows the
/// inherited locale (`LC_ALL` outranks `LC_TIME`, which outranks `LANG`). On
/// a host exporting e.g. `LANG=zh_CN.UTF-8` the weekday and month names come
/// out localized, [`parse_dmesg_timestamp`] cannot read them, and every OOM
/// event is stamped with the scan time instead of the kill time — which also
/// defeats the `(pid, timestamp)` dedup in [`recover_oom_events`]. Pin the
/// child to the C locale so the output keeps the format the parser documents.
fn dmesg_command() -> Command {
    let mut command = Command::new("dmesg");
    command.env("LC_ALL", "C");
    command
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
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);

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

        // Try to extract timestamp from dmesg -T format: [Fri Apr 17 10:00:00 2026]
        let timestamp_ns = parse_dmesg_timestamp(line).unwrap_or(now_ns);

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
    let ns = dt.and_utc().timestamp_nanos_opt()?;
    Some(ns)
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
        // Node processes could be either; record with unknown agent but still track
        Some("node(unknown-agent)")
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
/// Returns `true` if the PID appears in an OOM kill line in dmesg
/// (either format accepted by [`line_matches_oom_kill`]).
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

    content
        .lines()
        .any(|line| line_matches_oom_kill(line, &pid_str))
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

    // ─── dmesg output locale (startup recovery timestamps) ────────────────

    /// A `dmesg` stand-in that mimics util-linux: `-T` renders the timestamp
    /// with `strftime("%c")`, so the weekday and month names follow the
    /// inherited locale (`LC_ALL` outranks `LC_TIME`, which outranks `LANG`).
    const FAKE_DMESG: &str = r#"#!/bin/sh
case "${LC_ALL:-${LC_TIME:-${LANG:-}}}" in
    ""|C|POSIX|C.*)
        printf '[Fri Apr 17 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB\n'
        ;;
    *)
        printf '[五 4月 17 10:00:00 2026] Out of memory: Killed process 12345 (openclaw-gatewa) total-vm:1024kB\n'
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
}
