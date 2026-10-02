//! Phase 6.4: cgroup v2 memory quota.
//!
//! When enabled, the server process moves itself into a child cgroup at
//! startup and writes `memory.max` so a runaway index/snapshot can't
//! consume the host's memory. Linux-only; on other platforms this module
//! compiles to a no-op.
//!
//! We deliberately keep the scope tiny: only `memory.max`, no
//! `memory.high`, no io/pids controllers. Those are P7 territory.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CgroupConfig {
    /// When true, attempt to enter a child cgroup at startup and apply
    /// `memory_max`. Failure logs a warning and continues — never blocks
    /// service startup.
    #[serde(default)]
    pub enabled: bool,

    /// Maximum memory bytes for the server process. Accepts plain
    /// integers (`536870912`) or unit-suffixed strings (`512M`, `2G`,
    /// `1024K`). Default 512 MiB.
    #[serde(default = "default_memory_max")]
    pub memory_max: String,
}

fn default_memory_max() -> String {
    "512M".to_string()
}

/// Parse memory size strings like "512M" / "2G" / "1024K" / "1073741824".
/// Returns the value in bytes.
pub fn parse_memory_max(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty memory_max".into());
    }
    let (num_part, mult) = match s.chars().last() {
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024_u64),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024_u64 * 1024),
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024_u64 * 1024 * 1024),
        Some(c) if c.is_ascii_digit() => (s, 1_u64),
        _ => return Err(format!("unrecognized memory_max suffix in '{s}'")),
    };
    let n: u64 = num_part
        .trim()
        .parse()
        .map_err(|e| format!("parse '{num_part}': {e}"))?;
    n.checked_mul(mult)
        .ok_or_else(|| format!("memory_max overflow: '{s}' exceeds u64::MAX bytes"))
}

/// Outcome of an attempt to enter a memory-limited cgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CgroupOutcome {
    /// Successfully created and joined `<base>/<name>` with `memory.max=<n>`.
    Joined {
        path: std::path::PathBuf,
        memory_max: u64,
    },
    /// `enabled = false` — nothing attempted.
    Skipped,
    /// Enabled, but applying failed. Caller should keep going.
    Failed(String),
}

/// Best-effort entry into a memory-limited cgroup. See module docs.
pub fn apply(config: &CgroupConfig) -> CgroupOutcome {
    if !config.enabled {
        return CgroupOutcome::Skipped;
    }
    match imp::apply_linux(config) {
        Ok(o) => o,
        Err(e) => CgroupOutcome::Failed(e),
    }
}

mod imp {
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;

    use super::{CgroupConfig, CgroupOutcome, parse_memory_max};

    const ROOT: &str = "/sys/fs/cgroup";

    pub fn apply_linux(config: &CgroupConfig) -> Result<CgroupOutcome, String> {
        let max = parse_memory_max(&config.memory_max)?;

        // 1. Read current cgroup path from /proc/self/cgroup. v2 unified
        //    line looks like "0::/user.slice/user-1000.slice/session-3.scope"
        //    or "0::/" for the root.
        let raw = std::fs::read_to_string("/proc/self/cgroup")
            .map_err(|e| format!("read /proc/self/cgroup: {e}"))?;
        let unified = raw
            .lines()
            .find_map(|l| l.strip_prefix("0::"))
            .ok_or_else(|| "no v2 cgroup line in /proc/self/cgroup".to_string())?
            .trim();

        // 2. Pick a child path under it.
        let pid = std::process::id();
        let parent: PathBuf = if unified == "/" {
            PathBuf::from(ROOT)
        } else {
            PathBuf::from(ROOT).join(unified.strip_prefix('/').unwrap_or(unified))
        };
        let child = parent.join(format!("anolisa-memory.{pid}"));

        // 3. Make sure the parent delegates the memory controller into
        //    children — systemd-managed leaves often don't.
        //
        //    Cgroupfs permissions, not path heuristics, decide whether
        //    this write is allowed. Under a delegated parent (systemd
        //    `Delegate=memory` on a .service or .scope, or a `systemd
        //    --user` slice we own) we have write permission and the
        //    write either succeeds or is a no-op. Outside a delegated
        //    parent we lack write permission, the call returns
        //    EACCES/EPERM/EROFS, and we cannot affect sibling units —
        //    the subsequent memory.max write will then ENOENT, which
        //    surfaces as CgroupOutcome::Failed (the correct degraded
        //    result for non-delegated environments).
        let st_path = parent.join("cgroup.subtree_control");
        if let Err(e) = write_one(&st_path, "+memory") {
            tracing::info!(
                "could not enable +memory on parent subtree_control {}: {} \
                 (in a delegated scope this is expected; in a shared parent \
                 this may affect sibling units)",
                st_path.display(),
                e
            );
        }

        std::fs::create_dir_all(&child).map_err(|e| format!("mkdir {}: {e}", child.display()))?;

        // 4. Set memory.max BEFORE moving in, so the move atomically
        //    applies the limit.
        write_one(&child.join("memory.max"), &max.to_string())?;

        // 5. Move ourselves in.
        write_one(&child.join("cgroup.procs"), &pid.to_string())?;

        Ok(CgroupOutcome::Joined {
            path: child,
            memory_max: max,
        })
    }

    fn write_one(path: &std::path::Path, body: &str) -> Result<(), String> {
        let mut f = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| format!("open {}: {e}", path.display()))?;
        f.write_all(body.as_bytes())
            .map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decimal_only() {
        assert_eq!(parse_memory_max("1073741824"), Ok(1_073_741_824));
    }

    #[test]
    fn parse_with_suffixes() {
        assert_eq!(parse_memory_max("1024K"), Ok(1024 * 1024));
        assert_eq!(parse_memory_max("512M"), Ok(512 * 1024 * 1024));
        assert_eq!(parse_memory_max("2G"), Ok(2 * 1024 * 1024 * 1024));
        assert_eq!(parse_memory_max("4g"), Ok(4 * 1024 * 1024 * 1024));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_memory_max("").is_err());
        assert!(parse_memory_max("abc").is_err());
        assert!(parse_memory_max("12X").is_err());
    }

    #[test]
    fn parse_rejects_overflow() {
        // 18446744073709551615 = u64::MAX; * 1G silently wrapped pre-fix.
        let err = parse_memory_max("18446744073709551615G").unwrap_err();
        assert!(
            err.contains("overflow"),
            "expected overflow error, got: {err}"
        );
    }

    #[test]
    fn apply_skips_when_disabled() {
        let cfg = CgroupConfig {
            enabled: false,
            memory_max: "1G".into(),
        };
        assert_eq!(apply(&cfg), CgroupOutcome::Skipped);
    }
}
