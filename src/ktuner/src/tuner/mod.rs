use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::bench::BenchResult;
use crate::rules::Recommendation;

const ROLLBACK_PATH: &str = "/var/lib/ktuner/rollback.json";
const SYSCTL_PERSIST_PATH: &str = "/etc/sysctl.d/99-ktuner.conf";

#[derive(Serialize, Deserialize)]
struct RollbackEntry {
    previous: String,
    applied: String,
    path: String,
}

#[derive(Serialize, Deserialize)]
struct RollbackData {
    version: u32,
    entries: BTreeMap<String, RollbackEntry>,
}

/// One parameter that failed to apply, with the write/verify error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyFailure {
    pub param: String,
    pub error: String,
}

/// One parameter the kernel accepted but with a value different from the
/// request. The parameter IS applied — with the kernel's value — so the
/// delta is surfaced as a note rather than a failure (#4160).
///
/// Boundary case, deliberate: a write the kernel silently IGNORES (accepts,
/// value unchanged) records `applied = old` — strictly better than the old
/// invisibility, and sysctl.d only gains a no-op line.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ClampNote {
    pub param: String,
    pub requested: String,
    pub effective: String,
}

/// Outcome of applying a batch: how many params were applied, which failed
/// with why, and which the kernel accepted with an adjusted value. Mirrors
/// `RollbackOutcome` so `tune` can report partial failure the way `rollback`
/// already does — the previous return type (a bare count) could not represent
/// failures at all, so quiet mode dropped them entirely. `clamped` is disjoint
/// from `failed`: a rejected write is a failure, an accepted-but-adjusted
/// write is applied with a note.
pub struct ApplyOutcome {
    pub applied: usize,
    pub failed: Vec<ApplyFailure>,
    pub clamped: Vec<ClampNote>,
}

pub fn apply(recommendations: &[Recommendation]) -> Result<ApplyOutcome> {
    apply_inner(recommendations, false)
}

pub fn apply_quiet(recommendations: &[Recommendation]) -> Result<ApplyOutcome> {
    apply_inner(recommendations, true)
}

fn apply_inner(recommendations: &[Recommendation], quiet: bool) -> Result<ApplyOutcome> {
    let guard = lock_ledger_at(ROLLBACK_PATH)?;
    load_rollback()?; // Refuse an unreadable ledger before any live write.
    let total = recommendations.len();
    let mut applied_recs: Vec<Recommendation> = Vec::new();
    let mut failed: Vec<ApplyFailure> = Vec::new();
    let mut clamped: Vec<ClampNote> = Vec::new();
    for (i, rec) in recommendations.iter().enumerate() {
        match apply_recordable(rec) {
            Ok((applied, outcome)) => {
                if !quiet {
                    println!(
                        "    {} [{}/{}] {} → {}",
                        "✓".green(),
                        i + 1,
                        total,
                        rec.param,
                        outcome.effective
                    );
                    if outcome.clamped {
                        // The write landed but the kernel adjusted it; the
                        // effective value is what gets recorded, so the
                        // operator sees the delta here (#4160).
                        println!(
                            "      {} 内核实际生效 {}（期望 {}，已被内核调整并按实际值记录）",
                            "⚠".yellow(),
                            outcome.effective,
                            rec.recommended_value
                        );
                    }
                }
                if outcome.clamped {
                    clamped.push(ClampNote {
                        param: rec.param.clone(),
                        requested: rec.recommended_value.clone(),
                        effective: outcome.effective.clone(),
                    });
                }
                // The ledger and sysctl.d must describe live reality: record
                // the value the kernel actually took, not the request (#4160).
                applied_recs.push(applied);
            }
            Err(e) => {
                failed.push(ApplyFailure {
                    param: rec.param.clone(),
                    error: e.to_string(),
                });
                if !quiet {
                    println!(
                        "    {} [{}/{}] {} : {}",
                        "✗".red(),
                        i + 1,
                        total,
                        rec.param,
                        e
                    );
                }
            }
        }
    }

    if !applied_recs.is_empty() {
        save_rollback(&guard, &applied_recs)?;
        persist_from_rollback(&guard)?;
        if !quiet {
            println!();
            println!(
                "  {} 项配置已应用并持久化（重启后自动生效）",
                applied_recs.len()
            );
        }
    } else if !quiet {
        println!();
        println!("  没有配置被成功应用");
    }
    Ok(ApplyOutcome {
        applied: applied_recs.len(),
        failed,
        clamped,
    })
}

/// Apply a single recommendation with rollback recording and persistence, but
/// without apply()'s progress output — used by `ktuner fix` so a single fix is
/// just as reversible (and survives reboot) as `tune`. Returns the write
/// outcome so `fix` can report the value the kernel actually took.
pub fn apply_one(rec: &Recommendation) -> Result<WriteOutcome> {
    let guard = lock_ledger_at(ROLLBACK_PATH)?;
    load_rollback()?;
    let (applied, outcome) = apply_recordable(rec)?;
    save_rollback(&guard, std::slice::from_ref(&applied))?;
    persist_from_rollback(&guard)?;
    Ok(outcome)
}

// Recommendations are gathered before locking and may describe an older
// state. Capture a writable original only after the transaction owns the lock.
fn read_previous(param: &str) -> Result<String> {
    let path = param_to_path(param);
    let value = fs::read_to_string(&path)
        .with_context(|| format!("read original value from {path} before applying"))?;
    let trimmed = value.trim();
    Ok(trimmed
        .split_whitespace()
        .find_map(|token| token.strip_prefix('[').and_then(|t| t.strip_suffix(']')))
        .unwrap_or(trimmed)
        .to_string())
}

fn apply_recordable(rec: &Recommendation) -> Result<(Recommendation, WriteOutcome)> {
    let previous = read_previous(&rec.param)?;
    let outcome = write_and_verify(&rec.param, &rec.recommended_value)?;
    let mut applied = rec_with_effective(rec, &outcome);
    applied.current_value = previous;
    Ok((applied, outcome))
}

/// The result of a verified write: the value now live in the kernel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteOutcome {
    /// What the kernel actually took: the read-back value when it is
    /// observable (whether it matches the request or was clamped), the
    /// request itself for write-only tunables where no read-back exists.
    pub effective: String,
    /// True when the write was accepted but the live value differs from the
    /// request. Callers must still record `effective` — the change is real —
    /// and surface the divergence (#4160).
    pub clamped: bool,
}

/// Write `value` to the kernel path for `param` and verify it took effect by
/// reading it back. This is the single choke point for every live parameter
/// write (tune / fix / import all route through here), so the code-execution
/// deny-list is enforced here too as defense-in-depth — see is_forbidden_param.
pub fn write_and_verify(param: &str, value: &str) -> Result<WriteOutcome> {
    if is_forbidden_param(param) {
        anyhow::bail!("拒绝写入可执行代码的内核参数 {param}（core_pattern / modprobe 等）");
    }

    let path = param_to_path(param);

    if !Path::new(&path).exists() {
        anyhow::bail!("参数路径不存在");
    }

    fs::write(&path, value).with_context(|| {
        let is_root = unsafe { libc::geteuid() } == 0;
        if is_root {
            format!("写入 {path} 失败（容器内参数只读）")
        } else {
            format!("写入 {path} 失败（需要 sudo 权限）")
        }
    })?;

    // Verify by reading back. Some tunables are write-only (mode 0200, e.g.
    // vm.drop_caches / vm.compact_memory): the write is accepted but the read
    // fails — treat that as success with the request as the record, since the
    // kernel took the write and no read-back exists to diverge from.
    let readback = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => {
            return Ok(WriteOutcome {
                effective: value.to_string(),
                clamped: false,
            })
        }
    };

    // A mismatch is NOT a failure (#4160): fs::write already succeeded, so
    // the live value changed. Return what the kernel actually took so every
    // caller records it instead of leaving an untracked live change that the
    // rollback ledger cannot undo and sysctl.d does not persist.
    match classify_readback(value, readback.trim()) {
        ReadbackVerdict::Verified { effective } => Ok(WriteOutcome {
            effective,
            clamped: false,
        }),
        ReadbackVerdict::Clamped { effective } => Ok(WriteOutcome {
            effective,
            clamped: true,
        }),
    }
}

/// Classification of a read-back against the value that was written. Pure
/// (strings in, verdict out) so every verify decision — including the
/// kernel's clamping behaviour — is unit-testable without a writable
/// /proc/sys.
#[derive(Debug, PartialEq, Eq)]
pub enum ReadbackVerdict {
    /// The read-back confirms the write took effect as requested. `effective`
    /// is what to record: the request for bracket-list and leading-token
    /// files, the kernel's own rendering for an exact scalar match.
    Verified { effective: String },
    /// The write was ACCEPTED but the live value differs from the request:
    /// the kernel clamped or normalized it (e.g. an out-of-range
    /// net.core.rmem_max settles at a bound). The change did happen, so
    /// `effective` must reach the rollback ledger and sysctl.d; only the
    /// requested-vs-actual delta is surfaced as a note (#4160).
    Clamped { effective: String },
}

/// Whether a sysfs/sysctl read-back confirms `value`, and if not, what the
/// kernel actually took. sysfs "list" files (block scheduler,
/// transparent_hugepage/enabled|defrag, ...) echo every option and mark the
/// ACTIVE one in brackets, e.g. "always madvise [never]" — the selected value
/// is inside `[ ]`, not necessarily first, so a token that merely appears
/// unbracketed does NOT count. Otherwise we compare tokens (tolerating a
/// single written value against a multi-token read-back that leads with it —
/// a confirmed write, not a clamp).
fn classify_readback(value: &str, readback_trimmed: &str) -> ReadbackVerdict {
    if readback_trimmed.contains('[') {
        if readback_trimmed.contains(&format!("[{value}]")) {
            return ReadbackVerdict::Verified {
                effective: value.to_string(),
            };
        }
        // The active option differs from the request: the write landed on the
        // bracketed option, which is the value to record.
        let active = readback_trimmed
            .split_whitespace()
            .find_map(|t| t.strip_prefix('[').and_then(|t| t.strip_suffix(']')))
            .unwrap_or_default()
            .to_string();
        return ReadbackVerdict::Clamped { effective: active };
    }
    let rec_tokens: Vec<&str> = value.split_whitespace().collect();
    let read_tokens: Vec<&str> = readback_trimmed.split_whitespace().collect();
    if rec_tokens == read_tokens {
        return ReadbackVerdict::Verified {
            effective: readback_trimmed.to_string(),
        };
    }
    if rec_tokens.len() == 1 && read_tokens.len() > 1 && read_tokens.first() == rec_tokens.first() {
        // Leading-token match (e.g. write "bbr", read back "bbr cubic"):
        // a confirmed write of the REQUEST — the kernel only echoed extra
        // tokens it appends to its rendering. Record the request: the full
        // multi-token read-back is not a value the kernel can take back on
        // rollback or that sysctl.d can persist for a scalar param.
        return ReadbackVerdict::Verified {
            effective: value.to_string(),
        };
    }
    ReadbackVerdict::Clamped {
        effective: readback_trimmed.to_string(),
    }
}

/// The ledger/persistence view of `rec` after a write: `recommended_value`
/// becomes the value the kernel actually took, so the rollback ledger and
/// sysctl.d describe reality (#4160). Every other field is preserved verbatim.
fn rec_with_effective(rec: &Recommendation, outcome: &WriteOutcome) -> Recommendation {
    let mut applied = rec.clone();
    applied.recommended_value = outcome.effective.clone();
    applied
}

/// Drop `..`, `.` and empty path components so a parameter name can never
/// escape its intended root (defends against path traversal via `ktuner import`
/// of a malicious .conf — see is_safe_param). Legitimate single/nested segments
/// are preserved unchanged.
fn sanitize_rel(s: &str) -> String {
    s.split('/')
        .filter(|p| !p.is_empty() && *p != "." && *p != "..")
        .collect::<Vec<_>>()
        .join("/")
}

pub fn param_to_path(param: &str) -> String {
    if let Some(rest) = param.strip_prefix("block/") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            format!(
                "/sys/block/{}/queue/{}",
                sanitize_rel(parts[0]),
                sanitize_rel(parts[1])
            )
        } else {
            format!("/sys/block/{}", sanitize_rel(rest))
        }
    } else if let Some(rest) = param.strip_prefix("transparent_hugepage/") {
        format!("/sys/kernel/mm/transparent_hugepage/{}", sanitize_rel(rest))
    } else if let Some(path) = net_conf_path(param) {
        path
    } else {
        // sysctl: dots become slashes, so any ".." is turned into "//" and
        // cannot traverse; the result is always rooted at /proc/sys.
        format!("/proc/sys/{}", param.replace('.', "/"))
    }
}

/// Resolve `net.<proto>.conf.<interface>.<property>` (dotted or slashed
/// spelling) with the INTERFACE segment kept verbatim. Under
/// /proc/sys/net/{ipv4,ipv6}/conf/ every interface is a directory whose name
/// may itself contain dots — a VLAN subinterface is `eth0.100`, so the real
/// file is conf/eth0.100/forwarding (a literal-dot directory). The blanket
/// dot->slash translation instead produced conf/eth0/100/forwarding, which
/// never exists, so `ktuner why` answered "parameter not found" for BOTH
/// spellings even though the file was right there. Property names under
/// conf/ never contain dots or slashes, so the last separator splits
/// interface from property and everything before it stays verbatim; for
/// dot-free interfaces (all, default, eth0) the result is byte-identical to
/// the blanket translation. Returns None for every other sysctl.
fn net_conf_path(param: &str) -> Option<String> {
    for proto in ["ipv4", "ipv6"] {
        for sep in ['.', '/'] {
            let prefix = format!("net{sep}{proto}{sep}conf{sep}");
            if let Some(rest) = param.strip_prefix(&prefix) {
                return Some(match split_conf_tail(rest) {
                    Some((iface, prop)) => {
                        format!("/proc/sys/net/{proto}/conf/{iface}/{prop}")
                    }
                    None => format!("/proc/sys/net/{proto}/conf/{rest}"),
                });
            }
        }
    }
    None
}

/// Split a conf-family tail into (interface, property): the LAST separator
/// is the boundary, because properties are plain names while interfaces may
/// contain dots (VLAN `eth0.100`). None when the tail has no separator — an
/// interface named without a property, a directory rather than a tunable.
fn split_conf_tail(rest: &str) -> Option<(&str, &str)> {
    rest.rsplit_once('/').or_else(|| rest.rsplit_once('.'))
}

/// Whether a parameter name is structurally legitimate to apply. Used to reject
/// hostile entries from imported config files before they ever reach the
/// filesystem. Rejects traversal, absolute paths, NUL bytes, and degenerate
/// spellings with empty segments (`vm//swappiness`, `vm..swappiness`).
pub fn is_safe_param(param: &str) -> bool {
    if param.is_empty() || param.starts_with('/') || param.contains('\0') {
        return false;
    }
    // Dots are separators for sysctl names (param_to_path turns them into
    // slashes), so inspect the slash-resolved spelling: `..` segments are the
    // traversal risk on the '/'-separated block/ and transparent_hugepage/
    // branches, and empty segments are degenerate repeated or trailing
    // separators. The kernel collapses those on write, but the rollback
    // ledger keeps the spelling verbatim and persistence would emit a name
    // sysctl.d rejects (e.g. `vm..swappiness = 60`).
    if param
        .replace('.', "/")
        .split('/')
        .any(|seg| seg.is_empty() || seg == "..")
    {
        return false;
    }
    true
}

/// Kernel parameters that turn an attacker-controlled string into code the
/// kernel later runs as root (`kernel.core_pattern`'s `|program`, the
/// `modprobe` / `hotplug` / `poweroff_cmd` helper paths, `binfmt_misc`
/// handlers, `usermodehelper` gates), or that flip a one-way switch a
/// *reversible* tuner must never touch (`modules_disabled`,
/// `kexec_load_disabled`). `ktuner import` reads an UNTRUSTED .conf, so these
/// are rejected outright before any write — membership is unconditional, no
/// value is "safe". ktuner's own rules never recommend these, so guarding the
/// write choke point (write_and_verify) with this list is defense-in-depth
/// with zero legitimate-use regression.
pub fn is_forbidden_param(param: &str) -> bool {
    // Match on the RESOLVED filesystem path, not on the parameter's spelling, so
    // every equivalent spelling that lands on the same file is rejected: dotted
    // `kernel.core_pattern`, slashed `kernel/core_pattern`, doubled separators
    // `kernel//core_pattern`, or a `..`-laden name. A dotted-name-only deny-list
    // was fully bypassable because param_to_path's `.replace('.', "/")` is a
    // no-op on an already-slashed name, so `kernel/core_pattern` dodged the list
    // yet still resolved to /proc/sys/kernel/core_pattern.
    const FORBIDDEN_PATHS: &[&str] = &[
        "/proc/sys/kernel/core_pattern",
        "/proc/sys/kernel/modprobe",
        "/proc/sys/kernel/hotplug",
        "/proc/sys/kernel/poweroff_cmd",
        "/proc/sys/kernel/modules_disabled",
        "/proc/sys/kernel/kexec_load_disabled",
        "/proc/sys/kernel/usermodehelper", // + /bset, /inheritable ...
        "/proc/sys/fs/binfmt_misc",        // + /register ...
    ];

    let resolved = canonicalize_path(&param_to_path(param));
    FORBIDDEN_PATHS
        .iter()
        .any(|p| resolved == *p || resolved.starts_with(&format!("{p}/")))
}

/// Collapse empty/`.` segments and resolve `..` in a slash path so equivalent
/// spellings normalise to one comparable absolute form (e.g. `/a//b/../c` ->
/// `/a/c`). Used so is_forbidden_param can compare resolved paths.
fn canonicalize_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    format!("/{}", out.join("/"))
}

fn load_rollback() -> Result<RollbackData> {
    load_rollback_from(ROLLBACK_PATH)
}

fn load_rollback_from(path: &str) -> Result<RollbackData> {
    let json = match fs::read_to_string(path) {
        Ok(json) => json,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RollbackData {
                version: 1,
                entries: BTreeMap::new(),
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("read rollback ledger {path}; {}", ledger_remedy(path)))
        }
    };
    // Only an absent ledger is empty: hiding errors would discard originals
    // when a later merge replaces the existing rollback record.
    serde_json::from_str(&json)
        .with_context(|| format!("parse rollback ledger {path}; {}", ledger_remedy(path)))
}

// tune/fix reach the ledger only after parameters were written, and rollback
// reads the same file, so the error must name a way out. Moving the ledger
// aside is only safe while the copy is kept: a fresh ledger would record the
// already-tuned values as the originals.
fn ledger_remedy(path: &str) -> String {
    format!(
        "parameters may already be applied; inspect and repair {path} (or move it aside \
         and keep the copy, which holds the original values), then rerun the command"
    )
}

// Publish a fresh inode only after its contents and exact final mode are ready.
fn write_atomic(path: &str, content: &[u8], mode: u32) -> Result<()> {
    write_atomic_with(path, mode, |file| {
        file.write_all(content)
            .with_context(|| format!("write temporary contents for {path}"))
    })
}

// Keep the inode private throughout writing, even under umask 000. Exclusive
// creation also refuses stale files and symlinks instead of trusting their mode
// or reusing an inode for which another user may already hold a writable fd.
fn write_atomic_with(
    path: &str,
    mode: u32,
    write_content: impl FnOnce(&mut fs::File) -> Result<()>,
) -> Result<()> {
    let tmp = format!("{path}.tmp.{}", std::process::id());
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("create private temporary file {tmp}"))?;

    let result = (|| {
        write_content(&mut file)?;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .with_context(|| format!("set temporary file permissions for {tmp}"))?;
        fs::rename(&tmp, path).with_context(|| format!("replace {path}"))
    })();
    // Only remove a temporary file we created. Preserve the original error if
    // cleanup fails, including when the content writer fails after a partial write.
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn save_rollback(guard: &LedgerLock, recommendations: &[Recommendation]) -> Result<()> {
    merge_rollback_locked(
        guard,
        ROLLBACK_PATH,
        recommendations.iter().map(|r| {
            (
                r.param.clone(),
                r.current_value.clone(),
                r.recommended_value.clone(),
            )
        }),
    )
}

/// Merge `(param, previous, applied)` entries into a cumulative rollback record.
/// For a kernel path already recorded, keep the ORIGINAL `previous` (the true
/// pre-ktuner value) so rollback always restores pristine state even across
/// multiple tune/fix/import runs; only refresh `applied`. New params are added.
/// Pure (no I/O) so the keep-original-previous invariant is unit-testable.
///
/// Known limitation: alias matching only prevents NEW duplicates. A ledger
/// written before this fix may already hold two spellings of one kernel path
/// (e.g. `vm.swappiness` from tune and `vm/swappiness` from import); both are
/// kept, a new alias updates whichever is found first, and rollback restores
/// both to the same path in key order, so the slashed entry (`'/'` sorts after
/// `'.'`) writes last and may restore an intermediate value. Healing such
/// ledgers at merge time is left to a follow-up.
fn merge_entries<I>(mut data: RollbackData, entries: I) -> RollbackData
where
    I: IntoIterator<Item = (String, String, String)>,
{
    for (param, previous, applied) in entries {
        let path = param_to_path(&param);
        let identity = canonicalize_path(&path);
        // Equivalent spellings must share the first rollback record, or a
        // later alias would restore an intermediate value over the original.
        if let Some(entry) = data
            .entries
            .values_mut()
            .find(|entry| canonicalize_path(&entry.path) == identity)
        {
            entry.applied = applied;
            continue;
        }
        data.entries
            .entry(param)
            .and_modify(|e| e.applied = applied.clone())
            .or_insert_with(|| RollbackEntry {
                previous: previous.clone(),
                applied: applied.clone(),
                path,
            });
    }
    data
}

/// Guard holding an exclusive `flock` on the ledger's lockfile. The lock is
/// released when the descriptor closes on drop; the file itself stays on
/// disk (flock state belongs to the open descriptor, not the file).
struct LedgerLock {
    _file: fs::File,
}

/// Take an exclusive inter-process lock on `<ledger-path>.lock`, in the
/// ledger's own directory, mirroring the repo's `libc::flock` guard idiom
/// (blaze's pid handoff). The lock serializes every ledger transition:
/// without it two concurrent `ktuner fix`/`tune` runs (cron + config
/// management) freely interleaved load -> merge -> rename, so both loaded
/// the same snapshot, each renamed its own merge result, and the loser's
/// entry — a live kernel change with its only record of the pristine
/// `previous` — was silently dropped: rollback then restored the wrong
/// value or none, and the regenerated sysctl.d omitted the line.
fn lock_ledger_at(path: &str) -> Result<LedgerLock> {
    let dir = Path::new(path)
        .parent()
        .context("rollback path has no parent")?;
    fs::create_dir_all(dir).context("创建 rollback 目录失败")?;
    let lock_path = format!("{path}.lock");
    // 0600 like the ledger itself; contents never matter, only the flock on
    // the descriptor, so an existing file from an earlier run is fine.
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .with_context(|| format!("打开 rollback 锁文件 {lock_path} 失败"))?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(anyhow::anyhow!(
            "锁定 rollback 锁文件 {lock_path} 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(LedgerLock { _file: file })
}

#[cfg(test)]
fn merge_rollback_at<I>(path: &str, entries: I) -> Result<()>
where
    I: IntoIterator<Item = (String, String, String)>,
{
    let guard = lock_ledger_at(path)?;
    merge_rollback_locked(&guard, path, entries)
}

// Callers retain the same descriptor through live writes and persistence.
// Opening a second descriptor here would deadlock against their own flock.
fn merge_rollback_locked<I>(_guard: &LedgerLock, path: &str, entries: I) -> Result<()>
where
    I: IntoIterator<Item = (String, String, String)>,
{
    let data = merge_entries(load_rollback_from(path)?, entries);
    let dir = Path::new(path)
        .parent()
        .context("rollback path has no parent")?;
    fs::create_dir_all(dir).context("创建 rollback 目录失败")?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("设置 {} 权限 0700 失败", dir.display()))?;

    let json = serde_json::to_string_pretty(&data)?;
    write_atomic(path, json.as_bytes(), 0o600).context("保存 rollback 文件失败")?;
    Ok(())
}

/// Value check for imported (untrusted) params: format guard + sysrq
/// allowlist. Pure (no filesystem access) so tests can exercise allowed
/// sysrq values without writing /proc/sys or polluting the rollback ledger.
fn validate_import_value(param: &str, value: &str) -> Result<()> {
    // Format safety guard: reject empty, multi-line, or excessively long values.
    if value.is_empty() || value.contains('\n') || value.len() > 256 {
        anyhow::bail!("invalid value for {param}: empty, contains newline, or exceeds 256 bytes");
    }
    // Restricted parameter value allowlist. Resolve the parameter the same
    // way the write path does, so `kernel/sysrq` (and other separator
    // spellings that map to the same file) cannot slip past the dot-form
    // comparison.
    if canonicalize_path(&param_to_path(param)) == "/proc/sys/kernel/sysrq" {
        let v: u32 = value.trim().parse().unwrap_or(u32::MAX);
        if v != 0 && v != 176 {
            anyhow::bail!("kernel.sysrq import restricted to 0 or 176, got {value}");
        }
    }
    Ok(())
}

/// Apply one parameter from an imported (untrusted) .conf: enforce the
/// code-execution deny-list + write + read-back verify (all via
/// write_and_verify), then record it in the rollback ledger so `ktuner
/// rollback` can undo it. This gives `import` the same safety net as
/// `fix`/`tune` — previously import did a raw, unguarded, unverified fs::write
/// with no way back. The original is read under the transaction lock;
/// `current` is retained for compatibility but is never trusted as an original.
/// Unreadable write-only parameters can still be applied when `current` is
/// absent, without inventing a rollback value or persistence entry.
pub fn apply_import(param: &str, value: &str, current: Option<&str>) -> Result<()> {
    // Structural name guard before anything else: the rollback ledger records
    // the key verbatim and persistence re-emits it, so a degenerate spelling
    // must never be applied — rejecting here keeps it out of the ledger.
    if !is_safe_param(param) {
        anyhow::bail!("invalid parameter name {param}: traversal, empty segment, or absolute path");
    }
    validate_import_value(param, value)?;
    let guard = lock_ledger_at(ROLLBACK_PATH)?;
    load_rollback()?;
    let previous = match read_previous(param) {
        Ok(previous) => Some(previous),
        Err(error) if current.is_some() => return Err(error),
        Err(_) => None,
    };
    let outcome = write_and_verify(param, value)?;
    if let Some(previous) = previous {
        merge_rollback_locked(
            &guard,
            ROLLBACK_PATH,
            std::iter::once((param.to_string(), previous, outcome.effective)),
        )?;
        persist_from_rollback(&guard)?;
    }
    Ok(())
}

const NONSYSCTL_SCRIPT_PATH: &str = "/etc/ktuner/apply-nonsysctl.sh";
const NONSYSCTL_SERVICE_PATH: &str = "/etc/systemd/system/ktuner-nonsysctl.service";

/// Render the persisted file bodies from the rollback ledger, which is the
/// single source of truth for everything ktuner has applied. Returns
/// `(sysctl_conf, nonsysctl_script)`; `None` when a file would be empty.
fn render_persistence(
    entries: &std::collections::BTreeMap<String, RollbackEntry>,
) -> (Option<String>, Option<String>) {
    let mut sysctl_content = String::from("# Generated by ktuner - do not edit manually\n");
    sysctl_content.push_str("# Run `sudo ktuner rollback` to revert\n\n");

    let mut nonsysctl_script = String::from("#!/bin/bash\n");
    nonsysctl_script.push_str("# Generated by ktuner - do not edit manually\n");
    nonsysctl_script.push_str("# Run `sudo ktuner rollback` to revert\n\n");

    let mut has_sysctl = false;
    let mut has_nonsysctl = false;

    for (param, entry) in entries {
        if param.starts_with("block/") || param.starts_with("transparent_hugepage/") {
            nonsysctl_script.push_str(&format!(
                "[ -f '{}' ] && echo '{}' > '{}'\n",
                entry.path, entry.applied, entry.path
            ));
            has_nonsysctl = true;
        } else if param.contains('.') || param.contains('/') {
            // Slashed sysctl spellings ("kernel/sysrq") resolve to the same
            // /proc/sys file as the dotted form, but sysctl.d requires the
            // dotted spelling — normalize or the value silently vanishes at
            // reboot while the ledger still lists it.
            sysctl_content.push_str(&format!(
                "{} = {}\n",
                param.replace('/', "."),
                entry.applied
            ));
            has_sysctl = true;
        }
    }

    let sysctl = has_sysctl.then_some(sysctl_content);
    let nonsysctl = has_nonsysctl.then_some(nonsysctl_script);
    (sysctl, nonsysctl)
}

/// Regenerate the persisted config files from the cumulative rollback record,
/// which is the single source of truth for everything ktuner has applied. This
/// keeps persistence cumulative across runs (previously each run overwrote the
/// files with only its own batch, silently dropping earlier params) and never
/// persists a param that failed to apply (those are not in the record).
fn persist_from_rollback(_guard: &LedgerLock) -> Result<()> {
    let data = load_rollback()?;
    let (sysctl_content, nonsysctl_script) = render_persistence(&data.entries);

    if let Some(sysctl_content) = sysctl_content {
        // sysctl.d convention: world-readable, same as the systemd service
        // file below; write_atomic lands the mode before the rename so no
        // 0600 intermediate is ever visible at the final path.
        write_atomic(SYSCTL_PERSIST_PATH, sysctl_content.as_bytes(), 0o644)
            .context("持久化 sysctl 配置失败（需要 root 权限？）")?;
    }

    if let Some(nonsysctl_script) = nonsysctl_script {
        let dir = Path::new(NONSYSCTL_SCRIPT_PATH).parent().unwrap();
        fs::create_dir_all(dir).ok();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("设置 {} 权限 0755 失败", dir.display()))?;
        write_atomic(NONSYSCTL_SCRIPT_PATH, nonsysctl_script.as_bytes(), 0o755)
            .context("写入非 sysctl 持久化脚本失败")?;

        let service = format!(
            "[Unit]\n\
             Description=Apply ktuner non-sysctl kernel parameters\n\
             After=local-fs.target\n\n\
             [Service]\n\
             Type=oneshot\n\
             ExecStart={NONSYSCTL_SCRIPT_PATH}\n\
             RemainAfterExit=yes\n\n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        );

        write_atomic(NONSYSCTL_SERVICE_PATH, service.as_bytes(), 0o644)
            .context("写入 systemd service 失败")?;

        systemctl_quiet(&["daemon-reload"]);
        systemctl_quiet(&["enable", "ktuner-nonsysctl.service"]);
    }

    Ok(())
}

fn systemctl_quiet(args: &[&str]) {
    use std::process::Stdio;
    std::process::Command::new("systemctl")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok();
}

pub fn rollback_preview() -> Result<Vec<(String, String, String)>> {
    // No ledger = nothing pending, which is not an error (a fresh install, or
    // a completed rollback): --list reports an empty pending set.
    if !Path::new(ROLLBACK_PATH).exists() {
        return Ok(Vec::new());
    }
    let json = fs::read_to_string(ROLLBACK_PATH).context("读取 rollback 文件失败")?;
    parse_rollback_entries(&json)
}

/// Parse rollback-ledger JSON into (param, applied, previous) triples in
/// BTreeMap order. A corrupt ledger is an error, never an empty list —
/// silently treating a corrupt ledger as empty is how the original values
/// get lost (cf. #3578).
fn parse_rollback_entries(json: &str) -> Result<Vec<(String, String, String)>> {
    let data: RollbackData = serde_json::from_str(json).context("解析 rollback 文件失败")?;
    Ok(data
        .entries
        .iter()
        .map(|(param, entry)| (param.clone(), entry.applied.clone(), entry.previous.clone()))
        .collect())
}

/// Outcome of a rollback attempt: how many params were restored vs. failed to
/// restore vs. skipped (path absent). `failed`/`skipped` decide whether the
/// rollback ledger is safe to delete and whether the restore was actually total.
pub struct RollbackOutcome {
    pub restored: usize,
    pub failed: usize,
    pub skipped: usize,
}

impl RollbackOutcome {
    /// Whether all recorded values were restored, including an empty ledger.
    /// Failed writes and missing paths leave restoration incomplete.
    pub fn is_complete(&self) -> bool {
        rollback_should_finalize(self.failed, self.skipped)
    }
}

/// How to summarise a rollback to the user. Kept as a pure classifier so the
/// "系统恢复原状" (fully restored) claim is only made when it is actually true —
/// the caller previously printed it unconditionally, even when 0 params were
/// restored.
#[derive(Debug, PartialEq, Eq)]
pub enum RollbackStatus {
    /// Every recorded param was restored to its original value.
    Full,
    /// Some params were restored but at least one failed.
    Partial,
    /// Nothing was restored (0 succeeded), regardless of failures.
    Nothing,
}

pub fn classify_rollback(outcome: &RollbackOutcome) -> RollbackStatus {
    if outcome.restored == 0 {
        RollbackStatus::Nothing
    } else if outcome.failed == 0 && outcome.skipped == 0 {
        RollbackStatus::Full
    } else {
        RollbackStatus::Partial
    }
}

/// Tear down persisted config and delete the rollback ledger ONLY when EVERY
/// recorded param was actually restored. A param that failed to write OR whose
/// path was absent (skipped) is unrestored and its original value is still
/// needed, so the ledger must be kept and `ktuner rollback` can be retried.
/// Gating on `failed==0` alone lost the originals of skipped params (e.g. an
/// offline block device), and deleting the ledger when everything was skipped
/// (restored==0) was a regression over the prior `restored>0` guard.
fn rollback_should_finalize(failed: usize, skipped: usize) -> bool {
    failed == 0 && skipped == 0
}

pub fn rollback() -> Result<RollbackOutcome> {
    rollback_inner(false)
}

pub fn rollback_quiet() -> Result<RollbackOutcome> {
    rollback_inner(true)
}

fn restore_entries(data: &RollbackData, quiet: bool) -> RollbackOutcome {
    let mut restored = 0;
    let mut failed = 0;
    let mut skipped = 0;
    for (param, entry) in &data.entries {
        if is_forbidden_param(param) {
            if !quiet {
                println!("  {} {} : 拒绝恢复（代码执行参数）", "✗".red(), param);
            }
            failed += 1;
            continue;
        }
        if Path::new(&entry.path).exists() {
            match fs::write(&entry.path, &entry.previous) {
                Ok(()) => {
                    if !quiet {
                        println!("  {} {} → {} (已恢复)", "✓".green(), param, entry.previous);
                    }
                    restored += 1;
                }
                Err(e) => {
                    if !quiet {
                        println!("  {} {} : {}", "✗".red(), param, e);
                    }
                    failed += 1;
                }
            }
        } else {
            if !quiet {
                println!("  {} {} : 路径不存在，跳过", "⊘".yellow(), param);
            }
            skipped += 1;
        }
    }

    RollbackOutcome {
        restored,
        failed,
        skipped,
    }
}

fn rollback_inner(quiet: bool) -> Result<RollbackOutcome> {
    let _guard = lock_ledger_at(ROLLBACK_PATH)?;
    if !Path::new(ROLLBACK_PATH).exists() {
        anyhow::bail!("没有找到 rollback 文件 ({ROLLBACK_PATH})，可能尚未执行过 tune");
    }

    // The existence check, restore and cleanup share the apply transaction lock.
    let json = fs::read_to_string(ROLLBACK_PATH).context("读取 rollback 文件失败")?;
    let data: RollbackData = serde_json::from_str(&json).context("解析 rollback 文件失败")?;
    let RollbackOutcome {
        restored,
        failed,
        skipped,
    } = restore_entries(&data, quiet);

    if rollback_should_finalize(failed, skipped) {
        if Path::new(SYSCTL_PERSIST_PATH).exists() {
            fs::remove_file(SYSCTL_PERSIST_PATH).ok();
            if !quiet {
                println!("  已清理 {SYSCTL_PERSIST_PATH}");
            }
        }

        if Path::new(NONSYSCTL_SERVICE_PATH).exists() {
            systemctl_quiet(&["disable", "ktuner-nonsysctl.service"]);
            fs::remove_file(NONSYSCTL_SERVICE_PATH).ok();
            systemctl_quiet(&["daemon-reload"]);
            if !quiet {
                println!("  已清理 {NONSYSCTL_SERVICE_PATH}");
            }
        }

        if Path::new(NONSYSCTL_SCRIPT_PATH).exists() {
            fs::remove_file(NONSYSCTL_SCRIPT_PATH).ok();
            if !quiet {
                println!("  已清理 {NONSYSCTL_SCRIPT_PATH}");
            }
        }

        fs::remove_file(ROLLBACK_PATH).ok();
    } else if !quiet {
        println!(
            "  {} {} 项恢复失败、{} 项路径缺失，已保留 {} 以便重试（未删除持久化配置）",
            "⚠".yellow(),
            failed,
            skipped,
            ROLLBACK_PATH
        );
    }

    if !quiet {
        println!();
        println!("  共恢复 {restored} 项配置。");
    }
    Ok(RollbackOutcome {
        restored,
        failed,
        skipped,
    })
}

const DEGRADATION_THRESHOLD: f64 = 10.0;
const ROLLBACK_MIN_DEGRADED: usize = 2;

pub struct VerifyResult {
    pub degraded: Vec<String>,
}

/// Compare the before/after bench runs metric by metric and report which
/// ones degraded. Pairing is by `BenchResult.name`, never by position: bench
/// suites emit results in completion order, so a re-run may reorder the
/// vectors, and index-pairing would compare a throughput against a latency
/// and flag both — with `ROLLBACK_MIN_DEGRADED == 2` that spuriously rolls
/// back a fully improved system. A before-metric with no after-partner, or a
/// change that cannot be graded (`b.value <= 0.0`, non-finite `a.value`),
/// counts as degraded: unverifiable is never silently clean.
pub fn verify_and_report(before: &[BenchResult], after: &[BenchResult]) -> VerifyResult {
    println!("  {}", "性能对比 (before → after)".bold());
    println!(
        "  {:<24} {:>16} {:>16} {:>8}",
        "指标", "调前", "调后", "变化"
    );
    println!("  {}", "─".repeat(66));

    let mut degraded = Vec::new();

    let after_by_name: HashMap<&str, &BenchResult> =
        after.iter().map(|a| (a.name.as_str(), a)).collect();

    for b in before {
        let Some(a) = after_by_name.get(b.name.as_str()) else {
            // The after-run lost this metric (bench error, partial run). zip
            // truncation used to drop it from the comparison entirely; a
            // metric that cannot be verified must be surfaced instead.
            degraded.push(b.name.clone());
            let before_val = format!("{:>8.2} {:<10}", b.value, b.unit);
            let after_val = format!("{:>8} {:<10}", "—", "");
            println!(
                "  {:<24} {}  {} {}",
                b.name,
                before_val,
                after_val,
                "无法验证 ⚠".red()
            );
            continue;
        };

        let change = (a.value - b.value) / b.value * 100.0;

        // NaN (and the infinities a `b.value == 0.0` division produces)
        // compare false against BOTH thresholds, so without this guard a
        // NaN after-value passes as clean and renders as "↓NaN%".
        let unverifiable = !change.is_finite() || b.value <= 0.0;

        let is_latency = b.unit.contains("ns") || b.unit.contains("μs");

        let is_degraded = if unverifiable {
            true
        } else if is_latency {
            change > DEGRADATION_THRESHOLD
        } else {
            change < -DEGRADATION_THRESHOLD
        };

        if is_degraded {
            degraded.push(b.name.clone());
        }

        let change_display = if unverifiable || change.abs() < 1.0 {
            "—".dimmed().to_string()
        } else if is_latency {
            if change < 0.0 {
                format!("↓{:.1}%", change.abs()).green().to_string()
            } else if is_degraded {
                format!("↑{change:.1}% ⚠").red().to_string()
            } else {
                format!("↑{change:.1}%").yellow().to_string()
            }
        } else if change > 0.0 {
            format!("↑{change:.1}%").green().to_string()
        } else if is_degraded {
            format!("↓{:.1}% ⚠", change.abs()).red().to_string()
        } else {
            format!("↓{:.1}%", change.abs()).yellow().to_string()
        };

        let before_val = format!("{:>8.2} {:<10}", b.value, b.unit);
        let after_val = format!("{:>8.2} {:<10}", a.value, a.unit);
        println!(
            "  {:<24} {}  {} {}",
            b.name, before_val, after_val, change_display
        );
    }

    VerifyResult { degraded }
}

/// Returns `None` when degradation is below the auto-rollback threshold (no
/// rollback attempted), or `Some(outcome)` with the ACTUAL restore counts when a
/// rollback was performed. Callers must inspect the outcome before claiming the
/// system was restored — previously this returned a bare `true` even when
/// `rollback()` restored nothing, so the CLI told the user "已回滚，系统恢复原状"
/// while the tuned (degraded) values were still live.
pub fn auto_rollback_on_degradation(result: &VerifyResult) -> Result<Option<RollbackOutcome>> {
    if result.degraded.len() < ROLLBACK_MIN_DEGRADED {
        if result.degraded.len() == 1 {
            println!();
            println!(
                "  {} {} 出现波动，可能是 benchmark 噪声，未自动回滚。",
                "△".yellow(),
                result.degraded[0]
            );
            println!(
                "    建议重新运行确认，或手动回滚: {}",
                "sudo ktuner rollback".bold()
            );
        }
        return Ok(None);
    }

    println!();
    println!(
        "  {} 检测到 {} 项指标恶化超过 {}%，执行自动回滚...",
        "⚠".yellow(),
        result.degraded.len(),
        DEGRADATION_THRESHOLD as u32
    );
    for name in &result.degraded {
        println!("    - {}", name.red());
    }
    println!();

    let outcome = rollback()?;
    Ok(Some(outcome))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_persistence_normalizes_slashed_sysctl_names() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "kernel/sysrq".to_string(),
            RollbackEntry {
                previous: "1".to_string(),
                applied: "176".to_string(),
                path: "/proc/sys/kernel/sysrq".to_string(),
            },
        );
        entries.insert(
            "block/sda/scheduler".to_string(),
            RollbackEntry {
                previous: "none".to_string(),
                applied: "mq-deadline".to_string(),
                path: "/sys/block/sda/queue/scheduler".to_string(),
            },
        );
        let (sysctl, nonsysctl) = render_persistence(&entries);
        // The ledger is the single source of truth: a slashed sysctl spelling
        // must land in the sysctl.d file in its dotted form, not be dropped.
        let sysctl = sysctl.expect("slashed sysctl param must be persisted");
        assert!(sysctl.contains("kernel.sysrq = 176"));
        let nonsysctl = nonsysctl.expect("block param must be persisted");
        assert!(nonsysctl.contains("/sys/block/sda/queue/scheduler"));
    }

    #[test]
    fn render_persistence_empty_without_entries() {
        let entries = std::collections::BTreeMap::new();
        let (sysctl, nonsysctl) = render_persistence(&entries);
        assert!(sysctl.is_none());
        assert!(nonsysctl.is_none());
    }

    #[test]
    fn test_parse_rollback_entries_round_trip() {
        let data = RollbackData {
            version: 1,
            entries: [
                (
                    "vm.swappiness".to_string(),
                    RollbackEntry {
                        previous: "60".to_string(),
                        applied: "1".to_string(),
                        path: "/proc/sys/vm/swappiness".to_string(),
                    },
                ),
                (
                    "block/sda/scheduler".to_string(),
                    RollbackEntry {
                        previous: "mq-deadline".to_string(),
                        applied: "none".to_string(),
                        path: "/sys/block/sda/queue/scheduler".to_string(),
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let entries = parse_rollback_entries(&json).unwrap();
        assert_eq!(
            entries,
            vec![
                (
                    "block/sda/scheduler".to_string(),
                    "none".to_string(),
                    "mq-deadline".to_string()
                ),
                (
                    "vm.swappiness".to_string(),
                    "1".to_string(),
                    "60".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_parse_rollback_entries_empty_ledger() {
        // Fresh install / post-rollback state: empty, not an error.
        let entries = parse_rollback_entries(r#"{"version":1,"entries":{}}"#).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn test_parse_rollback_entries_rejects_corrupt_json() {
        // The #3578 "corrupt is not empty" contract.
        let err = parse_rollback_entries("not json").unwrap_err();
        assert!(err.to_string().contains("解析"), "got: {err}");
    }

    #[test]
    fn test_parse_rollback_entries_rejects_wrong_shape() {
        // Wrong top-level type and wrong entries type: Err, no panic, no
        // silent default.
        assert!(parse_rollback_entries("[1,2,3]").is_err());
        assert!(parse_rollback_entries(r#"{"entries":"x"}"#).is_err());
    }

    #[test]
    fn test_parse_rollback_entries_sorted_by_param() {
        // BTreeMap serialization emits sorted keys, so the parse output is
        // sorted by param — the ordering --list's output promises.
        let data = RollbackData {
            version: 1,
            entries: ["c", "a", "b"]
                .iter()
                .map(|p| {
                    (
                        p.to_string(),
                        RollbackEntry {
                            previous: "0".to_string(),
                            applied: "1".to_string(),
                            path: format!("/proc/sys/{p}"),
                        },
                    )
                })
                .collect(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let entries = parse_rollback_entries(&json).unwrap();
        let params: Vec<&str> = entries.iter().map(|(p, _, _)| p.as_str()).collect();
        assert_eq!(params, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_param_to_path_sysctl() {
        assert_eq!(param_to_path("vm.swappiness"), "/proc/sys/vm/swappiness");
        assert_eq!(
            param_to_path("net.core.somaxconn"),
            "/proc/sys/net/core/somaxconn"
        );
        assert_eq!(
            param_to_path("net.ipv4.tcp_fastopen"),
            "/proc/sys/net/ipv4/tcp_fastopen"
        );
        assert_eq!(
            param_to_path("kernel.randomize_va_space"),
            "/proc/sys/kernel/randomize_va_space"
        );
        assert_eq!(
            param_to_path("net.core.rmem_max"),
            "/proc/sys/net/core/rmem_max"
        );
    }

    #[test]
    fn apply_reports_missing_params_as_failures() {
        // Nonexistent paths fail inside write_and_verify before any write, so
        // this is safe for non-root CI: every param must come back as a
        // recorded failure with its reason, not vanish — the old quiet-mode
        // contract dropped the error text entirely, so `ktuner tune` printed
        // {"applied": 0} and exited 0 even when every write failed.
        let recs: Vec<Recommendation> = ["vm.ktuner_no_such_a", "vm.ktuner_no_such_b"]
            .iter()
            .map(|p| Recommendation {
                param: p.to_string(),
                current_value: "0".to_string(),
                recommended_value: "1".to_string(),
                writable: true,
                ..Default::default()
            })
            .collect();
        let outcome = apply_quiet(&recs).expect("apply_quiet must not fail on per-param errors");
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.failed.len(), 2, "both failures must be reported");
        assert_eq!(outcome.failed[0].param, "vm.ktuner_no_such_a");
        assert!(
            !outcome.failed[0].error.is_empty(),
            "error text must survive quiet mode"
        );
    }

    #[test]
    fn test_param_to_path_block_device() {
        assert_eq!(
            param_to_path("block/sda/scheduler"),
            "/sys/block/sda/queue/scheduler"
        );
        assert_eq!(
            param_to_path("block/nvme0n1/nr_requests"),
            "/sys/block/nvme0n1/queue/nr_requests"
        );
        assert_eq!(
            param_to_path("block/sda/read_ahead_kb"),
            "/sys/block/sda/queue/read_ahead_kb"
        );
        assert_eq!(
            param_to_path("block/nvme0n1/rq_affinity"),
            "/sys/block/nvme0n1/queue/rq_affinity"
        );
    }

    #[test]
    fn test_param_to_path_thp() {
        assert_eq!(
            param_to_path("transparent_hugepage/enabled"),
            "/sys/kernel/mm/transparent_hugepage/enabled"
        );
    }

    #[test]
    fn test_param_to_path_conf_vlan_interface() {
        // VLAN subinterfaces are literal-dot directories under conf/
        // (conf/eth0.100/forwarding). The blanket dot->slash translation
        // resolved both spellings to conf/eth0/100/forwarding, which never
        // exists — `ktuner why` then failed with "parameter not found" even
        // though the file was present.
        assert_eq!(
            param_to_path("net.ipv4.conf.eth0.100.forwarding"),
            "/proc/sys/net/ipv4/conf/eth0.100/forwarding"
        );
        assert_eq!(
            param_to_path("net/ipv4/conf/eth0.100/forwarding"),
            "/proc/sys/net/ipv4/conf/eth0.100/forwarding"
        );
        assert_eq!(
            param_to_path("net.ipv6.conf.eth0.100.accept_ra"),
            "/proc/sys/net/ipv6/conf/eth0.100/accept_ra"
        );
        // Dot-free interfaces keep the blanket translation's exact result.
        assert_eq!(
            param_to_path("net.ipv4.conf.all.send_redirects"),
            "/proc/sys/net/ipv4/conf/all/send_redirects"
        );
        assert_eq!(
            param_to_path("net.ipv4.conf.default.rp_filter"),
            "/proc/sys/net/ipv4/conf/default/rp_filter"
        );
        // Interface-only tails are directories, same as before.
        assert_eq!(
            param_to_path("net.ipv4.conf.eth0"),
            "/proc/sys/net/ipv4/conf/eth0"
        );
        // Degenerate double dots still resolve to a nonexistent literal-dot
        // directory: fail-closed, never a wrong write.
        assert_eq!(
            param_to_path("net.ipv4.conf.eth0..100.forwarding"),
            "/proc/sys/net/ipv4/conf/eth0..100/forwarding"
        );
    }

    #[test]
    fn test_conf_vlan_spellings_share_a_ledger_entry() {
        // Equivalent dotted/slashed spellings must resolve to the same real
        // file so merge_entries keeps ONE rollback entry pointing at it
        // (the alias-dedup contract), instead of two aliases for a path
        // that never existed.
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        let data = merge_entries(
            data,
            [(
                "net.ipv4.conf.eth0.100.forwarding".to_string(),
                "0".to_string(),
                "1".to_string(),
            )],
        );
        let data = merge_entries(
            data,
            [(
                "net/ipv4/conf/eth0.100/forwarding".to_string(),
                "1".to_string(),
                "1".to_string(),
            )],
        );
        assert_eq!(data.entries.len(), 1, "aliases must share one entry");
        let entry = data.entries.values().next().unwrap();
        assert_eq!(entry.path, "/proc/sys/net/ipv4/conf/eth0.100/forwarding");
        assert_eq!(entry.previous, "0", "pristine value survives the alias");
    }

    #[test]
    fn test_is_forbidden_param_conf_family_unaffected() {
        // The deny-list compares resolved paths; conf-family resolution lands
        // strictly under /proc/sys/net/, so no forbidden path becomes
        // reachable, VLAN tunables are not denied, and the forbidden
        // spellings keep their verdicts.
        assert!(!is_forbidden_param("net.ipv4.conf.eth0.100.forwarding"));
        assert!(!is_forbidden_param("net.ipv4.conf.all.send_redirects"));
        for p in [
            "kernel.core_pattern",
            "kernel/core_pattern",
            "kernel.modprobe",
            "kernel//modprobe",
        ] {
            assert!(is_forbidden_param(p), "{p} must stay forbidden");
        }
    }

    #[test]
    fn test_is_safe_param_vlan_conf_spellings() {
        // Both VLAN spellings remain legitimate names (the existing contract
        // next door already pins the dotted and slashed pair), while
        // degenerate double-dot interfaces stay rejected — the dot-preserving
        // resolution must not loosen the structural name guard.
        assert!(is_safe_param("net.ipv4.conf.eth0.100.forwarding"));
        assert!(is_safe_param("net/ipv4/conf/eth0.100/forwarding"));
        assert!(!is_safe_param("net.ipv4.conf.eth0..100.forwarding"));
        assert!(!is_safe_param("net.ipv4.conf..forwarding"));
        assert!(!is_safe_param("net.ipv4.conf.eth0.100."));
    }

    #[test]
    fn test_param_to_path_rejects_traversal() {
        // Traversal components must be stripped so the result can never escape
        // its root, even from a hostile imported .conf.
        assert_eq!(
            param_to_path("transparent_hugepage/../../../../etc/cron.d/evil"),
            "/sys/kernel/mm/transparent_hugepage/etc/cron.d/evil"
        );
        assert_eq!(
            param_to_path("block/sda/../../../../etc/passwd"),
            "/sys/block/sda/queue/etc/passwd"
        );
        // None of these may contain a ".." component after sanitization.
        for p in ["transparent_hugepage/../x", "block/x/../../y"] {
            assert!(!param_to_path(p).split('/').any(|s| s == ".."));
        }
    }

    #[test]
    fn test_is_safe_param() {
        assert!(is_safe_param("vm.swappiness"));
        assert!(is_safe_param("block/sda/scheduler"));
        assert!(is_safe_param("transparent_hugepage/enabled"));
        assert!(!is_safe_param("transparent_hugepage/../../../etc/cron.d/x"));
        assert!(!is_safe_param("block/x/../../../etc/passwd"));
        assert!(!is_safe_param("/etc/passwd"));
        assert!(!is_safe_param(""));
        assert!(!is_safe_param(".."));
    }

    #[test]
    fn test_is_safe_param_rejects_degenerate_separators() {
        // The kernel collapses repeated separators on write, so
        // `vm//swappiness` and `vm..swappiness` (dots are sysctl separators)
        // both reach the same file — but the ledger keeps the spelling
        // verbatim and persistence would emit a name sysctl.d rejects.
        assert!(!is_safe_param("vm//swappiness"));
        assert!(!is_safe_param("vm..swappiness"));
        assert!(!is_safe_param("vm.swappiness."));
        assert!(!is_safe_param("block//sda/scheduler"));
        // VLAN interfaces (`eth0.100` under procfs) keep both spellings
        // legitimate: the dot form and the canonical slashed form.
        assert!(is_safe_param("net.ipv4.conf.eth0/100.forwarding"));
        assert!(is_safe_param("net.ipv4.conf.eth0.100.forwarding"));
    }

    #[test]
    fn test_is_forbidden_param_blocks_code_exec() {
        // Code-execution / one-way primitives must be rejected — writing these
        // from an untrusted imported .conf is a root RCE or an irreversible
        // brick.
        for p in [
            "kernel.core_pattern",
            "kernel.modprobe",
            "kernel.hotplug",
            "kernel.poweroff_cmd",
            "kernel.modules_disabled",
            "kernel.kexec_load_disabled",
            "kernel.usermodehelper.bset",
            "kernel.usermodehelper.inheritable",
            "fs.binfmt_misc.register",
            "fs.binfmt_misc",
        ] {
            assert!(is_forbidden_param(p), "{p} must be forbidden");
        }
    }

    #[test]
    fn test_is_forbidden_param_allows_normal_tunables() {
        // Ordinary tunables must stay writable or every `tune` would break.
        for p in [
            "vm.swappiness",
            "net.core.somaxconn",
            "kernel.sched_migration_cost_ns",
            "kernel.randomize_va_space",
            "kernel.numa_balancing",
        ] {
            assert!(!is_forbidden_param(p), "{p} must be allowed");
        }
        // Prefix guard must respect the dot boundary and not over-match params
        // that merely share a stem.
        assert!(!is_forbidden_param("kernel.core_uses_pid"));
        assert!(!is_forbidden_param("fs.binfmt_misc_unrelated"));
    }

    #[test]
    fn test_classify_rollback() {
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored: 3,
                failed: 0,
                skipped: 0
            }),
            RollbackStatus::Full
        );
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored: 2,
                failed: 1,
                skipped: 0
            }),
            RollbackStatus::Partial
        );
        // A skipped (path-absent) param means the restore was NOT total, so it
        // must be Partial — not Full — even with zero write failures. This is the
        // cosmetic contradiction the skipped field fixes.
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored: 2,
                failed: 0,
                skipped: 1
            }),
            RollbackStatus::Partial
        );
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored: 0,
                failed: 2,
                skipped: 0
            }),
            RollbackStatus::Nothing
        );
        // 0 restored must NEVER be reported as a full restore, even with 0
        // failures — this is the exact false-success the fix removes.
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored: 0,
                failed: 0,
                skipped: 0
            }),
            RollbackStatus::Nothing
        );
    }

    #[test]
    fn rollback_completeness_includes_failed_and_skipped_entries() {
        for (restored, failed, skipped, complete) in [
            (0, 0, 0, true),
            (3, 0, 0, true),
            (0, 1, 0, false),
            (0, 0, 1, false),
            (2, 1, 0, false),
            (2, 0, 1, false),
            (2, 1, 1, false),
        ] {
            assert_eq!(
                RollbackOutcome {
                    restored,
                    failed,
                    skipped
                }
                .is_complete(),
                complete
            );
        }
    }

    #[test]
    fn test_rollback_finalize_only_when_all_restored() {
        // Finalize (delete ledger) only when EVERY param was restored: zero
        // failures AND zero skipped. A failed write or an absent path must keep
        // the ledger so originals aren't lost.
        assert!(rollback_should_finalize(0, 0));
        assert!(!rollback_should_finalize(1, 0)); // a write failed
        assert!(!rollback_should_finalize(0, 1)); // a path was absent — the missed case
        assert!(!rollback_should_finalize(2, 3));
    }

    #[test]
    fn test_merge_aliases_keep_one_pristine_rollback_entry() {
        for (first, second) in [
            ("vm.swappiness", "vm/swappiness"),
            ("vm/swappiness", "vm.swappiness"),
            ("net.ipv4.tcp_fastopen", "net/ipv4/tcp_fastopen"),
        ] {
            for param in [first, second] {
                assert!(is_safe_param(param));
                assert!(!is_forbidden_param(param));
            }
            let data = RollbackData {
                version: 1,
                entries: BTreeMap::new(),
            };
            let data = merge_entries(
                data,
                [(first.to_string(), "10".to_string(), "20".to_string())],
            );
            let data = merge_entries(
                data,
                [(second.to_string(), "20".to_string(), "30".to_string())],
            );
            assert_eq!(data.entries.len(), 1, "aliases {first} / {second}");
            let entry = &data.entries[first];
            assert_eq!(entry.previous, "10", "pristine value for {first}");
            assert_eq!(entry.applied, "30", "latest value for {second}");
            assert_eq!(canonicalize_path(&entry.path), param_to_path(first));
        }
    }

    #[test]
    fn test_merge_alias_preserves_single_existing_ledger_entry() {
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{"vm/swappiness":{
                "previous":"10","applied":"20","path":"/proc/sys/vm/swappiness"
            }}}"#,
        )
        .unwrap();
        let data = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "20".to_string(),
                "30".to_string(),
            )],
        );
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries["vm/swappiness"];
        assert_eq!(entry.previous, "10");
        assert_eq!(entry.applied, "30");
        assert_eq!(entry.path, "/proc/sys/vm/swappiness");
    }

    #[test]
    fn test_merge_keeps_distinct_kernel_paths_separate() {
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        let data = merge_entries(
            data,
            [
                (
                    "vm.swappiness".to_string(),
                    "10".to_string(),
                    "20".to_string(),
                ),
                (
                    "vm.swappiness_extra".to_string(),
                    "40".to_string(),
                    "50".to_string(),
                ),
            ],
        );
        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.entries["vm.swappiness"].previous, "10");
        assert_eq!(data.entries["vm.swappiness_extra"].previous, "40");
    }

    #[test]
    fn test_merge_sysfs_aliases_share_a_rollback_entry() {
        for (first, second) in [
            ("block/sda/scheduler", "block/sda//scheduler"),
            (
                "transparent_hugepage/enabled",
                "transparent_hugepage//enabled",
            ),
        ] {
            let data = RollbackData {
                version: 1,
                entries: BTreeMap::new(),
            };
            let data = merge_entries(
                data,
                [(
                    first.to_string(),
                    "before".to_string(),
                    "middle".to_string(),
                )],
            );
            let data = merge_entries(
                data,
                [(
                    second.to_string(),
                    "middle".to_string(),
                    "after".to_string(),
                )],
            );
            assert_eq!(data.entries.len(), 1, "aliases {first} / {second}");
            assert_eq!(data.entries[first].previous, "before");
            assert_eq!(data.entries[first].applied, "after");
        }
    }

    #[test]
    fn test_rollback_aliases_restore_pristine_value_once() {
        let dir = AtomicTestDir::new("rollback_alias");
        let path = dir.0.join("swappiness");
        fs::write(&path, "10").unwrap();
        let mut data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        for (param, applied) in [("vm.swappiness", "20"), ("vm/swappiness", "30")] {
            let previous = fs::read_to_string(&path).unwrap();
            fs::write(&path, applied).unwrap();
            data = merge_entries(data, [(param.to_string(), previous, applied.to_string())]);
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "30");
        // Run the production restore loop against a temporary parameter file;
        // no /proc/sys writes or system-wide rollback cleanup are needed.
        for entry in data.entries.values_mut() {
            entry.path = path.to_str().unwrap().to_string();
        }
        let outcome = restore_entries(&data, true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "10");
        assert_eq!(outcome.restored, 1);
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.skipped, 0);
    }

    #[test]
    fn test_restore_entries_preserves_guards_and_outcome_counts() {
        let dir = AtomicTestDir::new("rollback_outcome");
        let allowed = dir.0.join("allowed");
        let forbidden = dir.0.join("forbidden");
        fs::write(&allowed, "20").unwrap();
        fs::write(&forbidden, "unchanged").unwrap();
        let mut entries = BTreeMap::new();
        for (param, path) in [
            ("vm.swappiness", allowed.clone()),
            ("kernel.core_pattern", forbidden.clone()),
            ("vm.dirty_ratio", dir.0.clone()),
            ("vm.dirty_background_ratio", dir.0.join("missing")),
        ] {
            entries.insert(
                param.to_string(),
                RollbackEntry {
                    previous: "10".to_string(),
                    applied: "20".to_string(),
                    path: path.to_str().unwrap().to_string(),
                },
            );
        }
        let outcome = restore_entries(
            &RollbackData {
                version: 1,
                entries,
            },
            true,
        );
        assert_eq!(fs::read_to_string(allowed).unwrap(), "10");
        assert_eq!(fs::read_to_string(forbidden).unwrap(), "unchanged");
        assert_eq!(outcome.restored, 1);
        assert_eq!(outcome.failed, 2);
        assert_eq!(outcome.skipped, 1);
    }

    #[test]
    fn test_merge_keeps_original_previous_refreshes_applied() {
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        // Run 1: swappiness's pristine value is 10, ktuner applies 20.
        let data = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "10".to_string(),
                "20".to_string(),
            )],
        );
        // Run 2: a second tune sees the CURRENT value (20) as "previous" and
        // applies 30. The merge must NOT let this clobber the pristine 10.
        let data = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "20".to_string(),
                "30".to_string(),
            )],
        );
        let e = data.entries.get("vm.swappiness").expect("entry present");
        // The invariant the merge doc promises: rollback restores pristine state
        // across repeated runs. If run 2 overwrote `previous` it would be "20",
        // and rollback would only undo to the post-run-1 value — a silent
        // wrong-restore. Discriminating: making and_modify also set `previous`
        // fails this assert.
        assert_eq!(e.previous, "10", "pristine previous must survive re-tuning");
        assert_eq!(e.applied, "30", "applied must refresh to the latest");
        assert_eq!(e.path, "/proc/sys/vm/swappiness");
    }

    #[test]
    fn test_ledger_lock_excludes_a_second_descriptor() {
        // The guard must actually hold an exclusive flock: a second open file
        // description on the same lockfile cannot acquire (even in-process —
        // flock contends per descriptor), and can once the guard drops.
        let dir = std::env::temp_dir().join(format!(
            "ktuner_ledger_lock_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("rollback.json");
        let guard = lock_ledger_at(ledger.to_str().unwrap()).unwrap();
        // The lockfile lives beside the ledger, private like the ledger.
        let lock_path = dir.join("rollback.json.lock");
        assert_eq!(
            fs::metadata(&lock_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let second = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        let rc = unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, -1, "non-blocking acquire while held must fail");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EWOULDBLOCK)
        );
        drop(guard);
        let rc = unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "acquire after drop must succeed");
        unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_UN) };
        drop(second);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_concurrent_merges_retain_every_entry() {
        // Barrier-synchronized lost-update repro at the merge level: two fix
        // runs load the same (empty) ledger, each merges its own entry, and
        // without the ledger lock whichever write_atomic rename lands last
        // silently drops the other's entry — its pristine `previous` is then
        // recorded nowhere. With the lock, every round retains both.
        use std::sync::{Arc, Barrier};
        let rounds = 50;
        for round in 0..rounds {
            let dir = std::env::temp_dir().join(format!(
                "ktuner_ledger_race_{}_{:?}_{round}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&dir).unwrap();
            let ledger_a = dir.join("rollback.json");
            let ledger_b = ledger_a.clone();
            let barrier = Arc::new(Barrier::new(2));
            let barrier_a = barrier.clone();
            let barrier_b = barrier;
            let join_a = std::thread::spawn(move || {
                barrier_a.wait();
                merge_rollback_at(
                    ledger_a.to_str().unwrap(),
                    [("vm.audit_a".to_string(), "60".to_string(), "10".to_string())],
                )
            });
            let join_b = std::thread::spawn(move || {
                barrier_b.wait();
                merge_rollback_at(
                    ledger_b.to_str().unwrap(),
                    [(
                        "net.core.audit_b".to_string(),
                        "128".to_string(),
                        "4096".to_string(),
                    )],
                )
            });
            join_a.join().unwrap().expect("merge a");
            join_b.join().unwrap().expect("merge b");
            let data = load_rollback_from(dir.join("rollback.json").to_str().unwrap()).unwrap();
            let missing: Vec<&str> = ["vm.audit_a", "net.core.audit_b"]
                .iter()
                .filter(|p| !data.entries.contains_key(**p))
                .copied()
                .collect();
            assert!(
                missing.is_empty(),
                "round {round}: lost ledger update, missing {missing:?} in {:?}",
                data.entries.keys().collect::<Vec<_>>()
            );
            fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn test_is_forbidden_param_resists_spelling_bypass() {
        // Every spelling that resolves to a forbidden /proc/sys file must be
        // caught, not just the canonical dotted name — the original deny-list
        // was bypassed by writing kernel/core_pattern (slashes) in a .conf.
        for p in [
            "kernel/core_pattern",
            "kernel//core_pattern",
            "kernel/modprobe",
            "fs/binfmt_misc/register",
            "kernel/usermodehelper/bset",
        ] {
            assert!(
                is_forbidden_param(p),
                "{p} must be forbidden (slash spelling)"
            );
        }
    }

    struct AtomicTestDir(std::path::PathBuf);

    impl AtomicTestDir {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("ktuner_atomic_{label}_{}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for AtomicTestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).expect("remove atomic test directory");
        }
    }

    #[test]
    fn test_write_atomic_private_until_publish() {
        // Changing umask in the parallel test process would affect unrelated
        // tests. Re-execute only this test in a child with umask 000 instead.
        const CHILD: &str = "KTUNER_ATOMIC_UMASK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new("sh")
                .args(["-c", "umask 000; exec \"$@\"", "ktuner-umask-test"])
                .arg(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "tuner::tests::test_write_atomic_private_until_publish",
                    "--nocapture",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "child failed: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let dir = AtomicTestDir::new("private");
        let probe = dir.0.join("umask-probe");
        fs::write(&probe, b"probe").unwrap();
        assert_eq!(
            fs::metadata(&probe).unwrap().permissions().mode() & 0o777,
            0o666
        );
        for mode in [0o600, 0o644, 0o755] {
            let target = dir.0.join(format!("target-{mode:o}"));
            let path = target.to_str().unwrap();
            let tmp = format!("{path}.tmp.{}", std::process::id());
            write_atomic_with(path, mode, |file| {
                assert!(!target.exists(), "must not publish before writing");
                assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
                assert_eq!(fs::metadata(&tmp)?.permissions().mode() & 0o777, 0o600);
                file.write_all(b"payload")?;
                assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
                Ok(())
            })
            .unwrap();
            assert_eq!(
                fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                mode
            );
            assert_eq!(fs::read(&target).unwrap(), b"payload");
            assert!(!Path::new(&tmp).exists());
        }
    }

    #[test]
    fn test_write_atomic_rejects_existing_temporary() {
        use std::os::unix::fs::symlink;

        let dir = AtomicTestDir::new("existing");
        for symlinked in [false, true] {
            let target = dir.0.join(format!("target-{symlinked}"));
            let path = target.to_str().unwrap();
            let tmp = format!("{path}.tmp.{}", std::process::id());
            let victim = dir.0.join(format!("victim-{symlinked}"));
            fs::write(&target, b"original target").unwrap();
            if symlinked {
                fs::write(&victim, b"original temporary").unwrap();
                symlink(&victim, &tmp).unwrap();
            } else {
                fs::write(&tmp, b"original temporary").unwrap();
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o666)).unwrap();
            }
            assert!(write_atomic(path, b"replacement", 0o644).is_err());
            assert_eq!(fs::read(&target).unwrap(), b"original target");
            assert_eq!(fs::read(&tmp).unwrap(), b"original temporary");
            assert_eq!(
                fs::symlink_metadata(&tmp).unwrap().file_type().is_symlink(),
                symlinked
            );
        }
    }

    #[test]
    fn test_write_atomic_cleans_partial_write() {
        let dir = AtomicTestDir::new("partial");
        let target = dir.0.join("target");
        let path = target.to_str().unwrap();
        fs::write(&target, b"original").unwrap();
        let error = write_atomic_with(path, 0o644, |file| {
            file.write_all(b"partial")?;
            anyhow::bail!("injected write failure")
        })
        .unwrap_err();
        assert_eq!(error.to_string(), "injected write failure");
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert!(!Path::new(&format!("{path}.tmp.{}", std::process::id())).exists());
    }

    #[test]
    fn test_write_atomic_sets_exact_mode() {
        // Every mode the persist paths use must land on disk exactly, both
        // for a fresh target and for a pre-planted 0666 file — the
        // write-then-chmod bug only reached the final mode after an
        // attacker-observable window on the target path.
        for mode in [0o600u32, 0o644, 0o755] {
            for preexisting in [false, true] {
                let target = std::env::temp_dir()
                    .join(format!(
                        "ktuner_write_atomic_mode_{mode}_{preexisting}_{}",
                        std::process::id()
                    ))
                    .to_str()
                    .unwrap()
                    .to_string();
                if preexisting {
                    fs::write(&target, b"stale").unwrap();
                    fs::set_permissions(&target, fs::Permissions::from_mode(0o666)).unwrap();
                }

                write_atomic(&target, b"payload", mode).expect("write_atomic must succeed");

                let got = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
                assert_eq!(
                    got, mode,
                    "on-disk mode for {target} (preexisting={preexisting})"
                );
                assert_eq!(fs::read_to_string(&target).unwrap(), "payload");
                fs::remove_file(&target).ok();
            }
        }
    }

    #[test]
    fn test_write_atomic_orphans_stale_fd() {
        // The rename must swap the inode wholesale: a stale fd opened on the
        // pre-planted 0666 file keeps pointing at the orphaned inode, so
        // writes through it cannot pollute the replacement. A write-then-chmod
        // regression (truncating the same inode in place) would alias the fd
        // to the live target and leak the appended bytes into it.
        use std::io::Write;

        let target = std::env::temp_dir()
            .join(format!(
                "ktuner_write_atomic_stale_fd_{}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string();
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o666)).unwrap();

        let mut stale_fd = fs::OpenOptions::new()
            .append(true)
            .open(&target)
            .expect("open stale fd must succeed");

        write_atomic(&target, b"new", 0o600).expect("write_atomic must succeed");

        stale_fd.write_all(b"evil").expect("append via stale fd");
        stale_fd.flush().unwrap();
        drop(stale_fd);

        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "new",
            "stale-fd append must not pollute the replaced target"
        );
        let got = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(got, 0o600);
        fs::remove_file(&target).ok();
    }

    #[test]
    fn test_write_atomic_cleans_tmp_on_failure() {
        // rename() onto an existing directory fails with EISDIR — the easiest
        // failure to stage without root. The failed write must remove the tmp
        // sibling instead of littering the target directory.
        let target = std::env::temp_dir()
            .join(format!(
                "ktuner_write_atomic_cleanup_dir_{}",
                std::process::id()
            ))
            .to_str()
            .unwrap()
            .to_string();
        fs::create_dir_all(&target).unwrap();

        let result = write_atomic(&target, b"payload", 0o600);
        assert!(result.is_err(), "rename onto a directory must fail");

        // Same process as write_atomic, so the pid suffix matches.
        let tmp = format!("{target}.tmp.{}", std::process::id());
        assert!(
            !Path::new(&tmp).exists(),
            "failed write must not leave the tmp sibling behind"
        );
        fs::remove_dir_all(&target).ok();
    }

    #[test]
    fn test_classify_readback_scalar_exact() {
        // Plain scalar sysctls confirm with the kernel's rendering.
        assert_eq!(
            classify_readback("1", "1"),
            ReadbackVerdict::Verified {
                effective: "1".to_string()
            }
        );
        // Multi-token exact match (kernel.sem-style quadruples).
        assert_eq!(
            classify_readback("250 32000 100 128", "250 32000 100 128"),
            ReadbackVerdict::Verified {
                effective: "250 32000 100 128".to_string()
            }
        );
        // Whitespace differences are token-level, not byte-level.
        assert_eq!(
            classify_readback("10  20", "10 20"),
            ReadbackVerdict::Verified {
                effective: "10 20".to_string()
            }
        );
    }

    #[test]
    fn test_classify_readback_scalar_clamped() {
        // The kernel rejected the requested magnitude and settled at a bound:
        // the write DID land, so this is applied-with-note, not an error (#4160).
        assert_eq!(
            classify_readback("999999999", "4194304"),
            ReadbackVerdict::Clamped {
                effective: "4194304".to_string()
            }
        );
        // A single-value write that read back different.
        assert_eq!(
            classify_readback("1", "0"),
            ReadbackVerdict::Clamped {
                effective: "0".to_string()
            }
        );
        // Multi-token mismatch records the kernel's full read-back.
        assert_eq!(
            classify_readback("250 32000 100 128", "250 32000 100 999"),
            ReadbackVerdict::Clamped {
                effective: "250 32000 100 999".to_string()
            }
        );
    }

    #[test]
    fn test_classify_readback_bracket_list() {
        // Bracketed sysfs list files: the active option is inside [ ], not
        // necessarily first; an unbracketed token does NOT count.
        assert_eq!(
            classify_readback("never", "always madvise [never]"),
            ReadbackVerdict::Verified {
                effective: "never".to_string()
            }
        );
        assert_eq!(
            classify_readback("mq-deadline", "[mq-deadline] none"),
            ReadbackVerdict::Verified {
                effective: "mq-deadline".to_string()
            }
        );
        // The write landed on a DIFFERENT active option: clamped to it, and
        // the bracketed option is what must be recorded.
        assert_eq!(
            classify_readback("never", "[always] madvise never"),
            ReadbackVerdict::Clamped {
                effective: "always".to_string()
            }
        );
    }

    #[test]
    fn test_classify_readback_leading_token_is_verified() {
        // A single written value leading a multi-token read-back is a
        // confirmed write of the REQUEST (e.g. congestion-control listings):
        // the effective value is the request itself, because the kernel's
        // multi-token rendering is not a value a writable scalar param can
        // take back on rollback or that sysctl.d can persist.
        assert_eq!(
            classify_readback("bbr", "bbr cubic"),
            ReadbackVerdict::Verified {
                effective: "bbr".to_string()
            }
        );
    }

    #[test]
    fn test_rec_with_effective_swaps_only_recommended_value() {
        let rec = Recommendation {
            param: "net.core.rmem_max".to_string(),
            current_value: "212992".to_string(),
            recommended_value: "999999999".to_string(),
            reason: "万兆场景".to_string(),
            confidence: crate::rules::Confidence::High,
            category: crate::rules::Category::Performance,
            writable: true,
        };
        let outcome = WriteOutcome {
            effective: "4194304".to_string(),
            clamped: true,
        };
        let applied = rec_with_effective(&rec, &outcome);
        // The ledger view carries the kernel's value; everything else is
        // preserved verbatim so rollback restores the true original.
        assert_eq!(applied.recommended_value, "4194304");
        assert_eq!(applied.param, rec.param);
        assert_eq!(applied.current_value, "212992");
        assert_eq!(applied.reason, rec.reason);
        assert_eq!(applied.confidence, rec.confidence);
        assert_eq!(applied.category, rec.category);
        assert_eq!(applied.writable, rec.writable);
        // When nothing was clamped the rec passes through unchanged.
        let ok_outcome = WriteOutcome {
            effective: "999999999".to_string(),
            clamped: false,
        };
        assert_eq!(
            rec_with_effective(&rec, &ok_outcome).recommended_value,
            "999999999"
        );
    }

    #[test]
    fn test_write_and_verify_rejects_nonexistent_path() {
        // Nonexistent-path pattern: fails at the path check before any write,
        // so this is side-effect-free even as root in a container.
        let err = write_and_verify("vm.ktuner_no_such_param_for_clamp_test", "1").unwrap_err();
        assert!(err.to_string().contains("参数路径不存在"));
    }

    #[test]
    fn test_write_and_verify_rejects_forbidden_param_first() {
        // Defense-in-depth ordering: the deny-list fires before any path or
        // write attempt, even for a param whose path does not exist.
        let err = write_and_verify("kernel.core_pattern", "x").unwrap_err();
        assert!(err.to_string().contains("拒绝写入"));
    }

    #[test]
    fn test_apply_quiet_reports_no_clamps_when_nothing_lands() {
        // Two nonexistent params: nothing is written, so nothing can be
        // clamped, and the outcome must report zero applied with an empty
        // clamped list (no ledger/persist side effects even as root).
        let recs: Vec<Recommendation> = ["vm.ktuner_no_such_a", "vm.ktuner_no_such_b"]
            .iter()
            .map(|p| Recommendation {
                param: p.to_string(),
                current_value: "0".to_string(),
                recommended_value: "1".to_string(),
                writable: true,
                ..Default::default()
            })
            .collect();
        let outcome = apply_quiet(&recs).expect("apply must not fail on per-param errors");
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.failed.len(), 2);
        assert!(outcome.clamped.is_empty());
    }

    #[test]
    fn test_canonicalize_path() {
        assert_eq!(
            canonicalize_path("/proc/sys/kernel/core_pattern"),
            "/proc/sys/kernel/core_pattern"
        );
        assert_eq!(
            canonicalize_path("/proc/sys/kernel//core_pattern"),
            "/proc/sys/kernel/core_pattern"
        );
        assert_eq!(
            canonicalize_path("/proc/sys/kernel/./core_pattern"),
            "/proc/sys/kernel/core_pattern"
        );
        assert_eq!(
            canonicalize_path("/proc/sys/kernel/foo/../core_pattern"),
            "/proc/sys/kernel/core_pattern"
        );
        assert_eq!(canonicalize_path("/a/b/../c"), "/a/c");
        assert_eq!(canonicalize_path("/a/b/../../c"), "/c");
        assert_eq!(canonicalize_path("/"), "/");
    }
    #[test]
    fn test_validate_import_value_rejects_dangerous_values() {
        // kernel.sysrq restricted to 0 or 176
        let err = validate_import_value("kernel.sysrq", "1").unwrap_err();
        assert!(
            err.to_string().contains("restricted"),
            "expected 'restricted' in error, got: {err}"
        );
        let err = validate_import_value("kernel.sysrq", "511").unwrap_err();
        assert!(err.to_string().contains("restricted"));

        // Slash-separated spelling resolves to the same file and must hit the
        // same restriction.
        let err = validate_import_value("kernel/sysrq", "1").unwrap_err();
        assert!(
            err.to_string().contains("restricted"),
            "slash spelling bypassed the sysrq restriction: {err}"
        );

        // Empty value rejected
        let err = validate_import_value("any.param", "").unwrap_err();
        assert!(err.to_string().contains("invalid value"));

        // Newline in value rejected
        let err = validate_import_value("any.param", "val\nue").unwrap_err();
        assert!(err.to_string().contains("invalid value"));

        // Over-long value rejected
        let err = validate_import_value("any.param", &"x".repeat(257)).unwrap_err();
        assert!(err.to_string().contains("invalid value"));
    }

    #[test]
    fn test_validate_import_value_accepts_safe_values() {
        // Pure checks only: no write to /proc/sys and no rollback ledger
        // entry, so these stay side-effect-free even when run as root
        // (previously the accept path really wrote kernel.sysrq).
        assert!(validate_import_value("kernel.sysrq", "0").is_ok());
        assert!(validate_import_value("kernel.sysrq", "176").is_ok());
        // Slash spelling of an allowed value passes the same check.
        assert!(validate_import_value("kernel/sysrq", "176").is_ok());
        // Params outside the restricted set are untouched by the allowlist.
        assert!(validate_import_value("vm.swappiness", "10").is_ok());
    }

    #[test]
    fn test_apply_import_runs_validation_before_write() {
        // Wiring: apply_import must run validate_import_value before
        // write_and_verify. The param never exists, so "invalid value" can
        // only come from validation — without it the error would be the
        // path-not-found write failure.
        let err = apply_import("vm.ktuner_test_nonexistent", "", None).unwrap_err();
        assert!(
            err.to_string().contains("invalid value"),
            "apply_import skipped validation: {err}"
        );
    }

    #[test]
    fn test_apply_import_rejects_degenerate_names() {
        // The name guard fires before validate_import_value and before any
        // filesystem access, so this needs no root and touches no /proc/sys
        // entry. The value itself is valid — only the name can be the
        // rejection reason.
        for param in ["vm//swappiness", "vm..swappiness"] {
            let err = apply_import(param, "10", None).unwrap_err();
            assert!(
                err.to_string().contains("invalid parameter name"),
                "degenerate name {param:?} must be rejected up front: {err}"
            );
        }
    }

    #[test]
    fn test_apply_import_accepts_normal_values() {
        // Use a nonexistent parameter: when the tests run as root with a
        // writable /proc/sys, applying a real sysctl (e.g. vm.swappiness)
        // would actually mutate the host. A path that never exists exercises
        // the same write path with zero side effects; its failure is the
        // expected path-not-found, never a validation rejection.
        let r_normal = apply_import("vm.ktuner_test_nonexistent", "10", None);
        if let Err(e) = &r_normal {
            assert!(
                !e.to_string().contains("invalid value"),
                "normal value wrongly rejected: {e}"
            );
        }
    }

    #[test]
    fn test_absent_rollback_ledger_can_be_created() {
        let dir = AtomicTestDir::new("ledger-absent");
        let ledger = dir.0.join("nested/rollback.json");
        let path = ledger.to_str().unwrap();
        let data = load_rollback_from(path).unwrap();
        assert_eq!(data.version, 1);
        assert!(data.entries.is_empty());
        merge_rollback_at(path, [("vm.swappiness".into(), "60".into(), "10".into())]).unwrap();
        let data = load_rollback_from(path).unwrap();
        assert_eq!(data.entries["vm.swappiness"].previous, "60");
        assert_eq!(data.entries["vm.swappiness"].applied, "10");
        assert_eq!(
            fs::metadata(&ledger).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn test_valid_rollback_ledger_keeps_originals_across_merges() {
        let dir = AtomicTestDir::new("ledger-valid");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        merge_rollback_at(path, [("vm.swappiness".into(), "60".into(), "10".into())]).unwrap();
        merge_rollback_at(
            path,
            [
                ("vm.swappiness".into(), "10".into(), "20".into()),
                ("net.core.somaxconn".into(), "128".into(), "256".into()),
            ],
        )
        .unwrap();
        let data = load_rollback_from(path).unwrap();
        assert_eq!(data.entries.len(), 2);
        assert_eq!(data.entries["vm.swappiness"].previous, "60");
        assert_eq!(data.entries["vm.swappiness"].applied, "20");
        assert_eq!(data.entries["net.core.somaxconn"].previous, "128");
    }

    #[test]
    fn test_invalid_rollback_ledger_is_never_replaced() {
        let dir = AtomicTestDir::new("ledger-invalid");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        for contents in [
            b"".as_slice(),
            br#"{"version":1,"entries":{"vm.swappiness":{"previous":"60""#.as_slice(),
            br#"{"version":1,"entries":[]}"#.as_slice(),
            b"\xff\xfe".as_slice(),
        ] {
            fs::write(&ledger, contents).unwrap();
            fs::set_permissions(&ledger, fs::Permissions::from_mode(0o640)).unwrap();
            fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
            let error =
                merge_rollback_at(path, [("vm.swappiness".into(), "10".into(), "20".into())])
                    .expect_err("invalid ledger must prevent a replacement");
            assert!(error.to_string().contains(path), "{error:#}");
            assert_eq!(fs::read(&ledger).unwrap(), contents);
            assert_eq!(
                fs::metadata(&ledger).unwrap().permissions().mode() & 0o777,
                0o640
            );
            assert_eq!(
                fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert!(!Path::new(&format!("{path}.tmp.{}", std::process::id())).exists());
        }
    }

    #[test]
    fn test_rollback_read_errors_do_not_become_empty_ledgers() {
        let dir = AtomicTestDir::new("ledger-read-error");
        let path = dir.0.to_str().unwrap();
        let error = load_rollback_from(path)
            .err()
            .expect("a directory is an I/O error, not an absent ledger");
        assert!(error.to_string().contains(path), "{error:#}");
        assert!(dir.0.is_dir());
    }

    #[test]
    fn test_rollback_ledger_errors_name_a_remedy() {
        let dir = AtomicTestDir::new("ledger-remedy");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        fs::write(&ledger, b"{").unwrap();
        for error in [
            load_rollback_from(path)
                .err()
                .expect("truncated ledger must not parse"),
            load_rollback_from(dir.0.to_str().unwrap())
                .err()
                .expect("directory must not read"),
        ] {
            let message = error.to_string();
            assert!(message.contains("inspect and repair"), "{message}");
            assert!(message.contains("rerun the command"), "{message}");
        }
    }

    fn bench(name: &str, value: f64, unit: &str) -> BenchResult {
        BenchResult {
            name: name.to_string(),
            value,
            unit: unit.to_string(),
        }
    }

    #[test]
    fn verify_pairs_by_name_so_reordering_cannot_flip_the_verdict() {
        // Same host, both metrics genuinely improved (latency 500→80 ns,
        // throughput 100→600 MB/s), but the after-run finished in swapped
        // order. Index-pairing compared seq_read against 80 "MB/s" and
        // io_latency against 600 "ns", flagged BOTH degraded — enough to
        // trigger auto_rollback_on_degradation on a fully improved system.
        let before = vec![
            bench("seq_read", 100.0, "MB/s"),
            bench("io_latency", 500.0, "ns"),
        ];
        let after = vec![
            bench("io_latency", 80.0, "ns"),
            bench("seq_read", 600.0, "MB/s"),
        ];
        let result = verify_and_report(&before, &after);
        assert!(
            result.degraded.is_empty(),
            "every metric improved, yet flagged degraded: {:?}",
            result.degraded
        );
    }

    #[test]
    fn verify_surfaces_a_before_metric_missing_from_the_after_run() {
        // zip truncation used to shrink the comparison to the metrics both
        // runs share, so a bench that silently lost fsync reported a clean
        // tune. A metric that cannot be verified must never be silently
        // clean.
        let before = vec![
            bench("seq_read", 100.0, "MB/s"),
            bench("fsync", 500.0, "ns"),
        ];
        let after = vec![bench("seq_read", 200.0, "MB/s")];
        let result = verify_and_report(&before, &after);
        assert_eq!(result.degraded, vec!["fsync".to_string()]);
    }

    #[test]
    fn verify_marks_non_finite_changes_degraded_instead_of_clean() {
        // NaN compares false against both thresholds, so it used to pass as
        // clean and rendered as "↓NaN%". A degenerate before-value is the
        // same problem from the other side (0.0 divides into ±inf).
        let result = verify_and_report(
            &[
                bench("seq_read", 100.0, "MB/s"),
                bench("io_latency", 500.0, "ns"),
            ],
            &[
                bench("seq_read", f64::NAN, "MB/s"),
                bench("io_latency", 500.0, "ns"),
            ],
        );
        assert_eq!(result.degraded, vec!["seq_read".to_string()]);

        let result = verify_and_report(
            &[bench("seq_read", 0.0, "MB/s")],
            &[bench("seq_read", 200.0, "MB/s")],
        );
        assert_eq!(result.degraded, vec!["seq_read".to_string()]);
    }

    #[test]
    fn verify_same_order_pairing_keeps_today_grading() {
        // Unchanged, aligned runs must grade exactly as before: improved
        // throughput is clean, a >10% latency regression is degraded.
        let before = vec![
            bench("seq_read", 100.0, "MB/s"),
            bench("io_latency", 500.0, "ns"),
        ];
        let after = vec![
            bench("seq_read", 200.0, "MB/s"),
            bench("io_latency", 600.0, "ns"),
        ];
        let result = verify_and_report(&before, &after);
        assert_eq!(result.degraded, vec!["io_latency".to_string()]);
    }
}
