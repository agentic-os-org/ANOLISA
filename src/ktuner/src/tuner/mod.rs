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
use crate::category;
use crate::rules::Recommendation;

const ROLLBACK_PATH: &str = "/var/lib/ktuner/rollback.json";
const SYSCTL_PERSIST_PATH: &str = "/etc/sysctl.d/99-ktuner.conf";

#[derive(Serialize, Deserialize, Clone)]
struct RollbackEntry {
    previous: String,
    applied: String,
    path: String,
}

#[derive(Serialize, Deserialize, Clone)]
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
    apply_locked(recommendations, quiet, &guard)
}

// Keep the transaction guard alive through every write and persistence step.
fn apply_locked(
    recommendations: &[Recommendation],
    quiet: bool,
    guard: &LedgerLock,
) -> Result<ApplyOutcome> {
    let total = recommendations.len();
    let mut applied_recs: Vec<Recommendation> = Vec::new();
    let mut applied = 0usize;
    let mut failed: Vec<ApplyFailure> = Vec::new();
    let mut clamped: Vec<ClampNote> = Vec::new();
    for (i, rec) in recommendations.iter().enumerate() {
        // A mutually exclusive knob disables its twin as a side effect;
        // snapshot the twin before the write or its original is unrecorded
        // and no rollback can ever bring it back. (The `applied` counter
        // below already counts writes, not ledger records, so the twin
        // record leaves the batch's progress and exit code unchanged.)
        let sibling = cleared_sibling_entry(&rec.param);
        match apply_recordable(rec) {
            Ok((previous, outcome)) => {
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
                // The write landed whether or not a pristine value exists.
                applied += 1;
                if let Some(previous) = previous {
                    // The ledger and sysctl.d must describe live reality:
                    // record the value the kernel actually took, not the
                    // request (#4160). A param whose original could not be
                    // read is applied but unrecordable — see apply_recordable.
                    let mut applied_rec = rec_with_effective(rec, &outcome);
                    applied_rec.current_value = previous;
                    applied_recs.push(applied_rec);
                }
                // The write disabled the mutually exclusive twin: record what
                // was live before the write (the snapshot above), or no
                // rollback can ever bring that original back. The twin is a
                // live change of the pair even when the knob itself was
                // unrecordable, so it lands outside the branch.
                if let Some(entry) = sibling {
                    applied_recs.push(sibling_rec(entry));
                }
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
        save_rollback(guard, &applied_recs)?;
        persist_from_rollback(guard)?;
        if !quiet {
            println!();
            // Count writes, not ledger records: a *_bytes knob also records
            // the ratio sibling the kernel clears for it, and the batch's
            // progress and exit-code semantics stay per-write.
            println!("  {} 项配置已应用并持久化（重启后自动生效）", applied);
        }
    } else if applied == 0 && !quiet {
        println!();
        println!("  没有配置被成功应用");
    }
    Ok(ApplyOutcome {
        applied,
        failed,
        clamped,
    })
}

/// One applied single-parameter fix: what the rollback ledger recorded, plus
/// the write itself.
pub struct AppliedFix {
    /// The pre-write original captured under the transaction lock — the value
    /// the ledger records as `previous` and `ktuner rollback` restores.
    /// Recommendations are gathered before locking, so `rec.current_value`
    /// can be stale (the knob moved between gather and apply); callers
    /// reporting the original must report this, or their output contradicts
    /// the ledger and `rollback --list`. `None` for a write-only tunable (no
    /// original could be read): nothing is recorded and nothing is
    /// restorable.
    pub recorded_previous: Option<String>,
    /// The write outcome: the value the kernel actually took and whether it
    /// clamped the request.
    pub outcome: WriteOutcome,
}

/// Apply a single recommendation with rollback recording and persistence, but
/// without apply()'s progress output — used by `ktuner fix` so a single fix is
/// just as reversible (and survives reboot) as `tune`. Returns the recorded
/// original (read under the lock, so it is what a rollback restores) and the
/// write outcome so `fix` can report both without re-reading the ledger. A
/// param whose original cannot be read (write-only tunable) applies without a
/// rollback record, mirroring apply_import, and reports no original.
pub fn apply_one(rec: &Recommendation) -> Result<AppliedFix> {
    let guard = lock_ledger_at(ROLLBACK_PATH)?;
    load_rollback()?;
    // Snapshot the twin before the write: a mutually exclusive knob disables
    // it as a kernel side effect, and only the ledger can bring the original
    // back on rollback.
    let sibling = cleared_sibling_entry(&rec.param);
    let (previous, outcome) = apply_recordable(rec)?;
    let mut batch = Vec::new();
    if let Some(recorded) = &previous {
        let mut applied = rec_with_effective(rec, &outcome);
        applied.current_value = recorded.clone();
        batch.push(applied);
    }
    // The disabled twin is a live change of the pair even when the knob
    // itself was unrecordable (no readable pristine value), so it extends
    // the batch regardless.
    batch.extend(sibling.map(sibling_rec));
    if !batch.is_empty() {
        save_rollback(&guard, &batch)?;
        persist_from_rollback(&guard)?;
    }
    Ok(AppliedFix {
        recorded_previous: previous,
        outcome,
    })
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

// Write-only tunables (mode 0200, e.g. vm.drop_caches / vm.compact_memory)
// deny the read but accept the write — write_and_verify documents that arm
// and records the request as the effective value. A failed read therefore
// means "no pristine value to restore", not "do not apply": failing here put
// the read before the write and made every such param dead on arrival, while
// apply_import kept an escape. Mirror apply_import instead: apply, report the
// effective value, and keep the param out of the rollback ledger and
// persistence — a previous that was never read must never be invented, or
// rollback would write it back over the kernel.
fn apply_recordable(rec: &Recommendation) -> Result<(Option<String>, WriteOutcome)> {
    let previous = read_previous(&rec.param).ok();
    let outcome = write_and_verify(&rec.param, &rec.recommended_value)?;
    Ok((previous, outcome))
}

/// The twin knob the kernel disables as a side effect of writing `param`.
/// The kernel keeps two mutually exclusive sysctl pairs — `vm.dirty_bytes` /
/// `vm.dirty_ratio` (and the `dirty_background_` twins) and
/// `vm.overcommit_kbytes` / `vm.overcommit_ratio` — where writing either knob
/// zeroes the other: mm/page-writeback.c zeroes the dirty twin on every
/// write (dirty_background_ratio_handler / dirty_background_bytes_handler)
/// or on every changing one (dirty_ratio_handler / dirty_bytes_handler), and
/// mm/util.c zeroes the overcommit twin on every write
/// (overcommit_ratio_handler / overcommit_kbytes_handler). vm.rst states the
/// rule for the overcommit pair: "Setting one disables the other (which then
/// appears as 0 when read)". Applying one of these knobs is therefore a live
/// change to TWO knobs while the batch records one.
///
/// The mapping is bidirectional on purpose: the ratio write disables the
/// bytes twin too, so the same `applied = "0"` record has to be captured
/// whichever side the write landed on. `vm.overcommit_ratio` is the
/// reachable case — a strict-overcommit host with a fixed
/// `vm.overcommit_kbytes` limit reads the ratio as 0, the ratio rule then
/// treats that 0 as "too low" and the write silently disables the fixed
/// limit, whose original only the ledger can bring back.
fn cleared_sibling(param: &str) -> Option<&'static str> {
    match param {
        "vm.dirty_bytes" => Some("vm.dirty_ratio"),
        "vm.dirty_ratio" => Some("vm.dirty_bytes"),
        "vm.dirty_background_bytes" => Some("vm.dirty_background_ratio"),
        "vm.dirty_background_ratio" => Some("vm.dirty_background_bytes"),
        "vm.overcommit_kbytes" => Some("vm.overcommit_ratio"),
        "vm.overcommit_ratio" => Some("vm.overcommit_kbytes"),
        _ => None,
    }
}

/// Ledger tuple for the twin a mutually exclusive write is about to disable:
/// `(twin, live value, "0")`, read BEFORE the write lands — after it the
/// kernel has already zeroed the twin and the original is gone forever.
/// None when `param` disables nothing, the twin is unreadable, or it holds
/// no configured value. The recorded `applied` is "0" because that is the
/// value the kernel puts live, exactly like a clamped read-back (#4160): the
/// ledger must describe live reality for both knobs of the pair, or a
/// rollback restores `dirty_bytes = 0` and leaves the host with a zeroed
/// `dirty_ratio` — writeback throttling silently disabled.
fn cleared_sibling_entry(param: &str) -> Option<(String, String, String)> {
    let sibling = cleared_sibling(param)?;
    let live = read_previous(sibling).ok()?;
    sibling_cleared_record(sibling, &live)
}

/// Pure decision core of [`cleared_sibling_entry`]: whether a write that
/// sees `live` on the mutually exclusive twin must record it. A twin that is
/// already 0 (or unreadably empty) loses nothing, so nothing is recorded.
fn sibling_cleared_record(sibling: &str, live: &str) -> Option<(String, String, String)> {
    (!live.is_empty() && live != "0")
        .then(|| (sibling.to_string(), live.to_string(), "0".to_string()))
}

/// A synthetic ledger record from a [`cleared_sibling_entry`] tuple. It
/// documents a side effect the kernel performs, not a write ktuner made, so
/// every field beyond the ledger triple is default.
fn sibling_rec(entry: (String, String, String)) -> Recommendation {
    Recommendation {
        param: entry.0,
        current_value: entry.1,
        recommended_value: entry.2,
        ..Default::default()
    }
}

/// Whether `param`'s ledger entry records a kernel side effect: a knob a
/// mutually exclusive twin's write disabled, so its `applied` is 0 while the
/// twin that disabled it is still recorded. Both consumers of the pair share
/// this predicate — persistence never re-applies such a line after its
/// clearer's (the clearer reproduces the zeroed twin at boot), and the
/// restore writes it AFTER its clearer (whose write would zero it again).
fn is_cleared_twin(
    entries: &BTreeMap<String, RollbackEntry>,
    param: &str,
    entry: &RollbackEntry,
) -> bool {
    entry.applied == "0" && entries.keys().any(|k| cleared_sibling(k) == Some(param))
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
    // The runtime-dangerous policy tune and fix enforce (tune filters the
    // knob out of the plan and names it in would_skip; fix refuses with the
    // advice to persist instead) holds here too: a library import routes
    // through the same choke point and must not apply vm.nr_hugepages at
    // runtime — allocating hugepages on a live host is what the policy
    // exists to prevent. Dotted-normalize the spelling first: the guard is
    // exact-match while import accepts slashed aliases of the same knob.
    if category::is_runtime_dangerous(&param.replace('/', ".")) {
        anyhow::bail!(
            "拒绝在运行时写入 {param}（运行时危险参数，请写入 /etc/sysctl.d 在重启时生效）"
        );
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

    // Verify by reading back. A mismatch is NOT a failure (#4160): fs::write
    // already succeeded, so the live value changed. Return what the kernel
    // actually took so every caller records it instead of leaving an untracked
    // live change that the rollback ledger cannot undo and sysctl.d does not
    // persist.
    Ok(match readback_verdict(&path, value) {
        ReadbackVerdict::Verified { effective } => WriteOutcome {
            effective,
            clamped: false,
        },
        ReadbackVerdict::Clamped { effective } => WriteOutcome {
            effective,
            clamped: true,
        },
    })
}

/// Read `path` back and classify it against the value just written. Some
/// tunables are write-only (mode 0200, e.g. vm.drop_caches /
/// vm.compact_memory): the write is accepted but the read fails — that is
/// `Verified` with the request as the record, since the kernel took the write
/// and no read-back exists to diverge from.
fn readback_verdict(path: &str, value: &str) -> ReadbackVerdict {
    match fs::read_to_string(path) {
        Ok(s) => classify_readback(value, s.trim()),
        Err(_) => ReadbackVerdict::Verified {
            effective: value.to_string(),
        },
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
    } else if let Some(path) = net_iface_path(param) {
        path
    } else {
        // sysctl: dots become slashes, so any ".." is turned into "//" and
        // cannot traverse; the result is always rooted at /proc/sys.
        format!("/proc/sys/{}", param.replace('.', "/"))
    }
}

/// Resolve `net.<proto>.{conf,neigh}.<interface>.<property>` (dotted or
/// slashed spelling) with the INTERFACE segment kept verbatim. Under
/// /proc/sys/net/{ipv4,ipv6}/{conf,neigh}/ every interface is a directory
/// whose name may itself contain dots — a VLAN subinterface is `eth0.100`, so
/// the real file is conf/eth0.100/forwarding (a literal-dot directory). The
/// blanket dot->slash translation instead produced conf/eth0/100/forwarding,
/// which never exists, so `ktuner why` answered "parameter not found" for
/// BOTH spellings even though the file was right there. Property names under
/// these families never contain dots or slashes, so the last separator splits
/// interface from property and everything before it stays verbatim; for
/// dot-free interfaces (all, default, eth0) the result is byte-identical to
/// the blanket translation. Returns None for every other sysctl.
fn net_iface_path(param: &str) -> Option<String> {
    for proto in ["ipv4", "ipv6"] {
        for family in ["conf", "neigh"] {
            for sep in ['.', '/'] {
                let prefix = format!("net{sep}{proto}{sep}{family}{sep}");
                if let Some(rest) = param.strip_prefix(&prefix) {
                    return Some(match split_iface_tail(rest) {
                        Some((iface, prop)) => {
                            format!("/proc/sys/net/{proto}/{family}/{iface}/{prop}")
                        }
                        None => format!("/proc/sys/net/{proto}/{family}/{rest}"),
                    });
                }
            }
        }
    }
    None
}

/// Split a per-interface family tail into (interface, property): the LAST
/// separator is the boundary, because properties are plain names while
/// interfaces may contain dots (VLAN `eth0.100`). None when the tail has no
/// separator — an interface named without a property, a directory rather than
/// a tunable.
fn split_iface_tail(rest: &str) -> Option<(&str, &str)> {
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
///
/// Matching is on the RESOLVED filesystem path, not on a parameter's spelling,
/// so every equivalent spelling that lands on the same file is rejected:
/// dotted `kernel.core_pattern`, slashed `kernel/core_pattern`, doubled
/// separators `kernel//core_pattern`, or a `..`-laden name. A
/// dotted-name-only deny-list was fully bypassable because param_to_path's
/// `.replace('.', "/")` is a no-op on an already-slashed name, so
/// `kernel/core_pattern` dodged the list yet still resolved to
/// /proc/sys/kernel/core_pattern.
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

/// Whether a *filesystem path* (already resolved, as written) is deny-listed.
///
/// Both enforcement points go through here so they cannot drift: the write
/// choke point resolves a parameter into its path, while `restore` writes the
/// path the ledger recorded — and the recorded path, not the parameter next
/// to it, is what the kernel receives.
fn is_forbidden_resolved_path(path: &str) -> bool {
    let resolved = canonicalize_path(path);
    FORBIDDEN_PATHS
        .iter()
        .any(|p| resolved == *p || resolved.starts_with(&format!("{p}/")))
}

/// Whether a parameter name resolves to a deny-listed path.
pub fn is_forbidden_param(param: &str) -> bool {
    is_forbidden_resolved_path(&param_to_path(param))
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
        &guard.path,
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
/// Ledgers written before the alias dedup may already hold two spellings of
/// one kernel path (e.g. `vm.swappiness` from tune and `vm/swappiness` from
/// import); [`heal_alias_duplicates`] collapses such pairs before the new
/// entries are merged, so a legacy duplicate cannot survive into the next
/// ledger generation — where it would restore an intermediate value over the
/// original and double-render the knob in sysctl.d.
fn merge_entries<I>(mut data: RollbackData, entries: I) -> RollbackData
where
    I: IntoIterator<Item = (String, String, String)>,
{
    heal_alias_duplicates(&mut data);
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
        // The key may already exist under a path an older `param_to_path`
        // resolved differently (a dotted interface written before 5448, e.g.
        // `.../conf/Br0/100/forwarding`). `heal_alias_duplicates` compares
        // canonicalized paths and cannot merge that pair, so the stale path
        // would survive: the file does not exist, every restore skips the
        // entry, and the ledger never clears. Re-point it at what this
        // parameter resolves to now; `previous` stays the original.
        let current_path = path.clone();
        data.entries
            .entry(param)
            .and_modify(|e| {
                e.applied = applied.clone();
                e.path = current_path.clone();
            })
            .or_insert_with(|| RollbackEntry {
                previous: previous.clone(),
                applied: applied.clone(),
                path,
            });
    }
    data
}

/// Collapse ledger entries that record the same kernel path under two
/// spellings.
///
/// Ledgers written before the alias dedup (#3563) may already hold both
/// `vm.swappiness` (from tune) and `vm/swappiness` (from import). Left in
/// place, a rollback restores both in BTreeMap key order, so the later
/// spelling overwrites the pristine original with an intermediate value, and
/// `render_persistence` emits two lines for one knob.
///
/// Chain rule: when `x.applied == y.previous`, `y` was recorded while the
/// value `x` had applied was live, so `x` is the earlier record and the pair
/// collapses to `x.previous` (the pristine original) with `y.applied` (the
/// newest value), under `x`'s key. A pair where the reverse relation also
/// holds (`y.applied == x.previous`) proves nothing: both readings of the
/// pair are internally consistent (60 -> 10 -> 60 and 10 -> 60 -> 10), so the
/// values alone cannot order the records; the pair falls through to the
/// greatest-key rule instead of letting the key spelling decide which
/// `previous` is the "pristine" one — collapsing it anyway recorded an
/// intermediate value as the original and changed what a restore writes.
/// Records with no chain relation — the knob
/// was changed manually between the two ktuner runs — cannot be ordered, so
/// the entry with the greatest key survives verbatim: that is exactly the
/// record whose `previous` the key-order double write leaves in the kernel
/// today, so the healed restore changes no outcome, it only removes the
/// duplicate write. Pure (no I/O) so both rules are unit-testable.
fn heal_alias_duplicates(data: &mut RollbackData) {
    // Group the entry keys by the kernel path they resolve to.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, entry) in &data.entries {
        groups
            .entry(canonicalize_path(&entry.path))
            .or_default()
            .push(key.clone());
    }
    for (_path, mut keys) in groups {
        if keys.len() < 2 {
            continue;
        }
        keys.sort();
        // Collapse chain links first: x.applied == y.previous proves y was
        // recorded after x, so x's previous is the older original — unless
        // the reverse also holds, in which case both orderings of the pair
        // are consistent and the greatest-key rule below decides it.
        loop {
            let mut chain: Option<(String, String)> = None;
            'pairs: for x in &keys {
                for y in &keys {
                    if x != y
                        && data.entries[x].applied == data.entries[y].previous
                        && data.entries[y].applied != data.entries[x].previous
                    {
                        chain = Some((x.clone(), y.clone()));
                        break 'pairs;
                    }
                }
            }
            let Some((x, y)) = chain else { break };
            let newest = data.entries[&y].applied.clone();
            data.entries.get_mut(&x).unwrap().applied = newest;
            data.entries.remove(&y);
            keys.retain(|k| *k != y);
        }
        // Whatever remains has no order evidence: keep the greatest key, the
        // record today's key-order restore writes last.
        let survivor = keys.last().expect("group is non-empty").clone();
        for key in keys {
            if key != survivor {
                data.entries.remove(&key);
            }
        }
    }
}

/// Guard holding an exclusive `flock` on the ledger's lockfile. The lock is
/// released when the descriptor closes on drop; the file itself stays on
/// disk (flock state belongs to the open descriptor, not the file).
struct LedgerLock {
    /// The ledger path this guard was taken for. Writers derive the file
    /// they mutate from the lock they hold, so a guard taken over a private
    /// fixture ledger (the unit tests) steers every write back to that
    /// fixture instead of the production `ROLLBACK_PATH` constant.
    path: String,
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
    lock_ledger_with(path, libc::LOCK_EX)
}

/// Shared-acquisition twin of `lock_ledger_at` for read-only ledger access
/// (`rollback --list`): the preview's exists -> read pair must be atomic
/// against every LOCK_EX holder — a rollback finalize deletes the ledger
/// under that lock — but concurrent readers cost nothing, so LOCK_SH
/// excludes the writers without serializing parallel listings.
fn lock_ledger_shared_at(path: &str) -> Result<LedgerLock> {
    lock_ledger_with(path, libc::LOCK_SH)
}

fn lock_ledger_with(path: &str, operation: i32) -> Result<LedgerLock> {
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
    if unsafe { libc::flock(file.as_raw_fd(), operation) } != 0 {
        return Err(anyhow::anyhow!(
            "锁定 rollback 锁文件 {lock_path} 失败: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(LedgerLock {
        path: path.to_string(),
        _file: file,
    })
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
    // Refuse runtime-dangerous knobs before touching the filesystem: the
    // ledger lock create_dir_all's /var/lib/ktuner, which fails in an
    // unprivileged environment before write_and_verify's refusal can surface.
    // Dotted-normalize the spelling exactly like the write choke point does.
    if category::is_runtime_dangerous(&param.replace('/', ".")) {
        anyhow::bail!(
            "拒绝在运行时写入 {param}（运行时危险参数，请写入 /etc/sysctl.d 在重启时生效）"
        );
    }
    let guard = lock_ledger_at(ROLLBACK_PATH)?;
    load_rollback()?;
    let previous = match read_previous(param) {
        Ok(previous) => Some(previous),
        Err(error) if current.is_some() => return Err(error),
        Err(_) => None,
    };
    // A mutually exclusive knob disables its twin as a kernel side effect;
    // snapshot it before the write so the imported change records both knobs
    // of the pair and stays reversible.
    let sibling = cleared_sibling_entry(param);
    let outcome = write_and_verify(param, value)?;
    if let Some(previous) = previous {
        merge_rollback_locked(
            &guard,
            ROLLBACK_PATH,
            std::iter::once((param.to_string(), previous, outcome.effective)).chain(sibling),
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
            // The kernel re-disables the twin when the clearer line is
            // applied at boot, and a later "twin = 0" line would zero the
            // clearer's value right back — so the side-effect record of a
            // disabled twin is never persisted while its clearer is in the
            // ledger: the clearer line alone reproduces the live pair state.
            if is_cleared_twin(entries, param, entry) {
                continue;
            }
            // A slash as the first separator makes sysctl.d preserve literal
            // dots. Derive dotted-interface keys from the recorded proc path,
            // or systemd would interpret Br0.100 as two directories.
            let key = entry
                .path
                .strip_prefix("/proc/sys/")
                .filter(|path| net_iface_path(param).is_some() && path.contains('.'))
                .map(str::to_string)
                .unwrap_or_else(|| param.replace('/', "."));
            sysctl_content.push_str(&format!("{key} = {}\n", entry.applied));
            has_sysctl = true;
        }
    }

    let sysctl = has_sysctl.then_some(sysctl_content);
    let nonsysctl = has_nonsysctl.then_some(nonsysctl_script);
    (sysctl, nonsysctl)
}

/// The ledger entries persistence may render.
///
/// The rendered sysctl.d file (and the generated script) is applied by
/// systemd-sysctl at boot with root privileges, so an entry the deny-list
/// refuses must not reach it just because it sits in the ledger: rollback
/// refuses to restore it, and persistence would otherwise hand the same write
/// to the boot path instead — the root code-execution write the list exists to
/// prevent, deferred to the next reboot. Both fields are checked, since the
/// recorded path is what the script writes.
fn persistable_entries(
    entries: &BTreeMap<String, RollbackEntry>,
) -> BTreeMap<String, RollbackEntry> {
    entries
        .iter()
        .filter(|(param, entry)| {
            !is_forbidden_param(param) && !is_forbidden_resolved_path(&entry.path)
        })
        .map(|(param, entry)| (param.clone(), entry.clone()))
        .collect()
}

/// Regenerate the persisted config files from the cumulative rollback record,
/// which is the single source of truth for everything ktuner has applied. This
/// keeps persistence cumulative across runs (previously each run overwrote the
/// files with only its own batch, silently dropping earlier params) and never
/// persists a param that failed to apply (those are not in the record). The
/// record is read from the ledger the transaction's lock guards, so a fixture
/// lock renders its own ledger and never the production one.
fn persist_from_rollback(guard: &LedgerLock) -> Result<()> {
    let data = load_rollback_from(&guard.path)?;
    let entries = persistable_entries(&data.entries);
    let (sysctl_content, nonsysctl_script) = render_persistence(&entries);

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
    rollback_preview_at(ROLLBACK_PATH)
}

/// Read the pending rollback set for `--list` from the ledger at `path`.
///
/// An absent ledger short-circuits before the lock: lock acquisition creates
/// the ledger directory and its `<path>.lock` file, and `--list` is
/// documented as read-only ("nothing is written or deleted"), so a fresh
/// install must not gain either from a preview.
///
/// Otherwise the exists -> read pair runs under the ledger's lock, matching
/// rollback_inner's transaction shape: a concurrent rollback finalize holds
/// LOCK_EX while it deletes the ledger, so an unlocked preview could pass
/// exists() and then lose the file to that delete before read_to_string —
/// "empty pending" turned into a hard 读取 rollback 文件失败 error and a
/// `--list` exit 2. A shared lock is enough: preview never writes, and every
/// writer/finalizer takes LOCK_EX on the same `<ledger>.lock`, so LOCK_SH
/// keeps them out of the window without serializing parallel listings.
fn rollback_preview_at(path: &str) -> Result<Vec<(String, String, String)>> {
    // No ledger = nothing pending, which is not an error (a fresh install, or
    // a completed rollback): --list reports an empty pending set without
    // creating the ledger directory or lock file.
    if !Path::new(path).exists() {
        return Ok(Vec::new());
    }
    let _guard = lock_ledger_shared_at(path)?;
    // Re-check under the shared lock: a concurrent finalize holds LOCK_EX
    // while it deletes the ledger, so the state observed here is the state
    // the read below sees.
    if !Path::new(path).exists() {
        return Ok(Vec::new());
    }
    #[cfg(test)]
    tests::finalize_race_probe(path);
    let json = fs::read_to_string(path).context("读取 rollback 文件失败")?;
    parse_rollback_entries(&json)
}

/// Parse rollback-ledger JSON into (param, applied, previous) triples in
/// BTreeMap order. A corrupt ledger is an error, never an empty list —
/// silently treating a corrupt ledger as empty is how the original values
/// get lost (cf. #3578).
///
/// Legacy duplicate spellings are healed first, so the preview describes the
/// restore `rollback` will actually perform: `restore_entries` heals before
/// restoring, and an unhealed preview would promise a second restore that
/// never runs and report a `previous` the kernel will never receive.
fn parse_rollback_entries(json: &str) -> Result<Vec<(String, String, String)>> {
    let mut data: RollbackData = serde_json::from_str(json).context("解析 rollback 文件失败")?;
    heal_alias_duplicates(&mut data);
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
    restore_entries_with(data, quiet, &mut |path, value| fs::write(path, value))
}

/// `restore_entries` with the parameter write injectable. The kernel write is
/// a parameter because a single write can move a DIFFERENT knob than the one
/// addressed (the dirty pair's mutual clear), and that side effect has to be
/// reproducible on plain files for the re-check below to be testable without
/// a writable /proc/sys.
fn restore_entries_with(
    data: &RollbackData,
    quiet: bool,
    write_value: &mut dyn FnMut(&str, &str) -> std::io::Result<()>,
) -> RollbackOutcome {
    // Heal first: a legacy ledger may hold two spellings of one kernel path,
    // and restoring both in key order would overwrite the pristine original
    // with the intermediate value the second spelling recorded.
    let mut data = data.clone();
    heal_alias_duplicates(&mut data);
    let mut verified: Vec<&String> = Vec::new();
    let mut failed = 0;
    let mut skipped = 0;
    // A disabled twin is restored AFTER its clearer: the clearer's write
    // zeroes the twin whenever it lands, so a twin written first would be
    // wiped again by the clearer's own restore. That is what key order does
    // for the dirty pairs (`vm.dirty_bytes` before `vm.dirty_ratio`), but
    // not for the overcommit pair, where the disabled `vm.overcommit_kbytes`
    // sorts before the `vm.overcommit_ratio` that disables it.
    let ordered: Vec<(&String, &RollbackEntry)> = data
        .entries
        .iter()
        .filter(|(param, entry)| !is_cleared_twin(&data.entries, param, entry))
        .chain(
            data.entries
                .iter()
                .filter(|(param, entry)| is_cleared_twin(&data.entries, param, entry)),
        )
        .collect();
    for (param, entry) in ordered {
        // The deny-list is enforced on BOTH fields: the parameter name (what a
        // ktuner-written ledger records) and the recorded path, which is the
        // file the kernel actually receives. Every ledger ktuner writes has the
        // two in agreement, but a ledger that was edited or written by another
        // version can point an innocent parameter at `kernel.core_pattern` —
        // and restore runs as root.
        if is_forbidden_param(param) || is_forbidden_resolved_path(&entry.path) {
            if !quiet {
                println!("  {} {} : 拒绝恢复（代码执行参数）", "✗".red(), param);
            }
            failed += 1;
            continue;
        }
        if Path::new(&entry.path).exists() {
            match write_value(&entry.path, &entry.previous) {
                Ok(()) => {
                    // Confirm the write with the same read-back the tune path
                    // uses (#5717): fs::write returning Ok only means the
                    // kernel accepted the value, not that it took it. A
                    // clamped read-back (the kernel kept another value) must
                    // not count as restored, or rollback reports Full and
                    // deletes the ledger while the system is still tuned. It
                    // is a failure, which keeps the ledger for a retry.
                    match readback_verdict(&entry.path, &entry.previous) {
                        ReadbackVerdict::Verified { .. } => {
                            if !quiet {
                                println!(
                                    "  {} {} → {} (已恢复)",
                                    "✓".green(),
                                    param,
                                    entry.previous
                                );
                            }
                            verified.push(param);
                        }
                        ReadbackVerdict::Clamped { effective } => {
                            if !quiet {
                                println!(
                                    "  {} {} : 回读为 {}（期望 {}），未恢复",
                                    "✗".red(),
                                    param,
                                    effective,
                                    entry.previous
                                );
                            }
                            failed += 1;
                        }
                    }
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

    // Re-check every verified restore AFTER all writes ran. A per-write
    // read-back only proves its own file held `previous` at that moment; a
    // later write can move an earlier-restored knob behind its back — the
    // kernel zeroes the twin of a mutually exclusive pair on the clearer's
    // write (mm/page-writeback.c dirty_ratio_handler / dirty_bytes_handler,
    // mm/util.c overcommit_ratio_handler / overcommit_kbytes_handler). The
    // order above restores a clearer before its twin, but a ledger that
    // recorded only one side, or a knob in a chain, can still leave a
    // diverging value. A diverging re-read is a failure so rollback never
    // reports Full (and deletes the ledger) while a recorded original is not
    // the live value. Write-only tunables (read fails) stay verified: their
    // read-back never held anything to diverge from.
    let mut restored = verified.len();
    for param in &verified {
        let entry = &data.entries[*param];
        if let ReadbackVerdict::Clamped { effective } =
            readback_verdict(&entry.path, &entry.previous)
        {
            if !quiet {
                println!(
                    "  {} {} : 最终回读为 {effective}（期望 {}），已被后续写入覆盖，未恢复",
                    "✗".red(),
                    param,
                    entry.previous
                );
            }
            restored -= 1;
            failed += 1;
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
    #[test]
    fn persistence_preserves_literal_interface_dots() {
        for proto in ["ipv4", "ipv6"] {
            for param in [
                format!("net/{proto}/conf/Br0.100/forwarding"),
                format!("net.{proto}.conf.Br0.100.forwarding"),
            ] {
                let path = format!("/proc/sys/net/{proto}/conf/Br0.100/forwarding");
                let entries = BTreeMap::from([(
                    param,
                    RollbackEntry {
                        previous: "0".into(),
                        applied: "1".into(),
                        path,
                    },
                )]);
                let (config, script) = render_persistence(&entries);
                assert!(config
                    .unwrap()
                    .contains(&format!("net/{proto}/conf/Br0.100/forwarding = 1")));
                assert!(script.is_none());
            }
        }
    }

    #[test]
    fn persistence_preserves_neighbour_interface_dots() {
        // sysctl.d needs a slash-first key to keep a literal-dot interface
        // together (systemd would split net.ipv4.neigh.Br0.100 into two path
        // components), so the neigh key must be derived from the recorded
        // proc path exactly as the conf one is.
        for proto in ["ipv4", "ipv6"] {
            for param in [
                format!("net/{proto}/neigh/Br0.100/gc_thresh3"),
                format!("net.{proto}.neigh.Br0.100.gc_thresh3"),
            ] {
                let path = format!("/proc/sys/net/{proto}/neigh/Br0.100/gc_thresh3");
                let entries = BTreeMap::from([(
                    param,
                    RollbackEntry {
                        previous: "4096".into(),
                        applied: "8192".into(),
                        path,
                    },
                )]);
                let (config, script) = render_persistence(&entries);
                assert!(config
                    .unwrap()
                    .contains(&format!("net/{proto}/neigh/Br0.100/gc_thresh3 = 8192")));
                assert!(script.is_none());
            }
        }
    }

    #[test]
    fn render_persistence_omits_a_cleared_ratio_while_its_clearer_is_recorded() {
        // The ledger after a dirty_bytes tune on a ratio-mode host: the kernel
        // cleared vm.dirty_ratio as a side effect, so the record carries
        // applied = 0. Persisting that line would make systemd apply
        // "vm.dirty_ratio = 0" AFTER the bytes line at boot and zero the bytes
        // value right back — the tuning would silently vanish across reboots.
        // The bytes line alone reproduces the live pair state (kernel clears
        // the ratio itself), so the cleared-ratio record must be skipped.
        let entries = BTreeMap::from([
            (
                "vm.dirty_bytes".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "1073741824".into(),
                    path: "/proc/sys/vm/dirty_bytes".into(),
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "0".into(),
                    path: "/proc/sys/vm/dirty_ratio".into(),
                },
            ),
        ]);
        let (config, _) = render_persistence(&entries);
        let config = config.unwrap();
        assert!(config.contains("vm.dirty_bytes = 1073741824"));
        assert!(
            !config.contains("dirty_ratio"),
            "the cleared-ratio record must not be persisted: {config}"
        );
        // A ratio the host really uses (no clearer recorded, or a nonzero
        // applied from a genuine ratio write) still persists its line.
        let ratio_only = BTreeMap::from([(
            "vm.dirty_ratio".to_string(),
            RollbackEntry {
                previous: "20".into(),
                applied: "5".into(),
                path: "/proc/sys/vm/dirty_ratio".into(),
            },
        )]);
        let (config, _) = render_persistence(&ratio_only);
        assert!(config.unwrap().contains("vm.dirty_ratio = 5"));
        // Both knobs recorded with the ratio genuinely tuned (applied != 0):
        // the pair is live ratio-mode, so both lines persist as before.
        let both_live = BTreeMap::from([
            (
                "vm.dirty_bytes".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "1073741824".into(),
                    path: "/proc/sys/vm/dirty_bytes".into(),
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "5".into(),
                    path: "/proc/sys/vm/dirty_ratio".into(),
                },
            ),
        ]);
        let (config, _) = render_persistence(&both_live);
        let config = config.unwrap();
        assert!(config.contains("vm.dirty_bytes = 1073741824"));
        assert!(config.contains("vm.dirty_ratio = 5"));
    }

    #[test]
    fn mutually_exclusive_writes_record_the_twin_they_disable() {
        // Writing either knob of a mutually exclusive sysctl pair disables
        // the other, which then reads 0 — vm.rst states it for the
        // overcommit pair ("Setting one disables the other (which then
        // appears as 0 when read)") and the handlers do it for the dirty
        // pair too (mm/page-writeback.c dirty_ratio_handler /
        // dirty_background_ratio_handler versus their bytes twins; mm/util.c
        // overcommit_ratio_handler / overcommit_kbytes_handler). Every write
        // therefore moves TWO knobs, and the disabled twin's original has to
        // reach the ledger or no rollback can bring it back.
        for (param, twin, live) in [
            ("vm.overcommit_ratio", "vm.overcommit_kbytes", "8589934592"),
            ("vm.overcommit_kbytes", "vm.overcommit_ratio", "80"),
            ("vm.dirty_bytes", "vm.dirty_ratio", "20"),
            ("vm.dirty_ratio", "vm.dirty_bytes", "1073741824"),
            (
                "vm.dirty_background_bytes",
                "vm.dirty_background_ratio",
                "10",
            ),
            (
                "vm.dirty_background_ratio",
                "vm.dirty_background_bytes",
                "268435456",
            ),
        ] {
            assert_eq!(
                cleared_sibling(param),
                Some(twin),
                "writing {param} disables {twin}"
            );
            assert_eq!(
                sibling_cleared_record(twin, live),
                Some((twin.to_string(), live.to_string(), "0".to_string())),
                "the configured twin is recorded with its pre-write value"
            );
        }
        // A strict-overcommit host with a fixed overcommit_kbytes limit: the
        // ratio reads 0 (disabled, not low), the rule recommends 80, and the
        // write disables the limit. Both records land in the ledger, with the
        // disabled limit's original preserved as its `previous`.
        let data = merge_entries(
            RollbackData {
                version: 1,
                entries: BTreeMap::new(),
            },
            [
                (
                    "vm.overcommit_ratio".to_string(),
                    "0".to_string(),
                    "80".to_string(),
                ),
                sibling_cleared_record("vm.overcommit_kbytes", "8589934592")
                    .expect("the fixed limit is recorded before the write"),
            ],
        );
        assert_eq!(data.entries["vm.overcommit_ratio"].applied, "80");
        assert_eq!(
            data.entries["vm.overcommit_kbytes"].previous, "8589934592",
            "the disabled twin's original is what a rollback writes back"
        );
        assert_eq!(data.entries["vm.overcommit_kbytes"].applied, "0");
    }

    #[test]
    fn render_persistence_omits_the_disabled_overcommit_twin() {
        // The inverse of the dirty-ratio case: the ledger holds the fixed
        // overcommit_kbytes limit after a ratio write disabled it, so the
        // twin's record carries applied = 0. Persisting "vm.overcommit_ratio
        // = 0" after the kbytes line would make systemd zero the just-applied
        // fixed limit at boot — the tuning would silently vanish across
        // reboots. The kbytes line alone reproduces the live pair state.
        let entries = BTreeMap::from([
            (
                "vm.overcommit_kbytes".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "8589934592".into(),
                    path: "/proc/sys/vm/overcommit_kbytes".into(),
                },
            ),
            (
                "vm.overcommit_ratio".to_string(),
                RollbackEntry {
                    previous: "80".into(),
                    applied: "0".into(),
                    path: "/proc/sys/vm/overcommit_ratio".into(),
                },
            ),
        ]);
        let (config, _) = render_persistence(&entries);
        let config = config.unwrap();
        assert!(config.contains("vm.overcommit_kbytes = 8589934592"));
        assert!(
            !config.contains("overcommit_ratio"),
            "the disabled twin's record must not be re-applied at boot: {config}"
        );
    }

    #[test]
    fn restore_entries_restores_the_ratio_after_the_bytes() {
        // The dirty pair is mutually exclusive: writing either knob clears
        // the other. The restore must therefore write the bytes knob FIRST
        // (BTreeMap order guarantees "vm.dirty_bytes" < "vm.dirty_ratio") and
        // the ratio last, or the ratio write would clear the restored bytes
        // value on a live kernel. Guard test for the pair's ledger shape.
        let dir = AtomicTestDir::new("dirty_pair_restore");
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        fs::write(&bytes, "1073741824").unwrap();
        fs::write(&ratio, "0").unwrap();
        let entries = BTreeMap::from([
            (
                "vm.dirty_bytes".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "1073741824".into(),
                    path: bytes.to_str().unwrap().into(),
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "0".into(),
                    path: ratio.to_str().unwrap().into(),
                },
            ),
        ]);
        let outcome = restore_entries(
            &RollbackData {
                version: 1,
                entries,
            },
            true,
        );
        assert_eq!(outcome.restored, 2);
        assert_eq!(fs::read_to_string(&bytes).unwrap(), "0");
        assert_eq!(
            fs::read_to_string(&ratio).unwrap(),
            "20",
            "the cleared ratio must come back with the rollback"
        );
        assert!(rollback_should_finalize(outcome.failed, outcome.skipped));
    }

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
    fn persistence_drops_entries_the_deny_list_refuses() {
        // The rendered sysctl.d file is applied by systemd-sysctl at boot with
        // root privileges, so a deny-listed entry must not be re-emitted just
        // because it sits in the ledger: rollback refuses to restore it, and
        // persistence would otherwise hand the same root write to the boot
        // path instead.
        let entries = BTreeMap::from([
            (
                "vm.swappiness".to_string(),
                RollbackEntry {
                    previous: "60".to_string(),
                    applied: "1".to_string(),
                    path: "/proc/sys/vm/swappiness".to_string(),
                },
            ),
            (
                "kernel.core_pattern".to_string(),
                RollbackEntry {
                    previous: "core".to_string(),
                    applied: "|/tmp/evil".to_string(),
                    path: "/proc/sys/kernel/core_pattern".to_string(),
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".to_string(),
                    applied: "10".to_string(),
                    // Innocent name, deny-listed recorded path.
                    path: "/proc/sys/kernel/modprobe".to_string(),
                },
            ),
        ]);

        let kept = persistable_entries(&entries);
        assert!(kept.contains_key("vm.swappiness"), "ordinary params stay");
        assert!(
            !kept.contains_key("kernel.core_pattern") && !kept.contains_key("vm.dirty_ratio"),
            "a deny-listed param and a deny-listed path must both be dropped: {:?}",
            kept.keys().collect::<Vec<_>>()
        );
        let (config, script) = render_persistence(&kept);
        let config = config.unwrap_or_default();
        assert!(
            !config.contains("core_pattern") && !config.contains("modprobe"),
            "nothing deny-listed may reach the rendered file: {config}"
        );
        assert!(script.is_none());
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
    fn preview_heals_a_legacy_duplicate_spelling_ledger() {
        // The same legacy ledger restore_entries heals: tune recorded
        // vm.swappiness 60->10, a later import recorded vm/swappiness 10->5.
        // The preview must describe the healed restore — one pending entry
        // returning the pristine 60 — not promise a second restore (10) that
        // rollback never performs.
        let entries = parse_rollback_entries(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"10","applied":"5","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![(
                "vm.swappiness".to_string(),
                "5".to_string(),
                "60".to_string()
            )],
            "preview must match the healed restore set: one knob, pristine previous, newest applied"
        );
    }

    #[test]
    fn preview_heals_unrelated_duplicates_to_the_last_write() {
        // No chain relation: the survivor is the greatest key, exactly the
        // record restore_entries writes today, so preview and restore agree
        // on both the row count and the previous value that lands.
        let entries = parse_rollback_entries(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"20","applied":"30","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        assert_eq!(
            entries,
            vec![(
                "vm/swappiness".to_string(),
                "30".to_string(),
                "20".to_string()
            )],
            "preview must keep the last-write record, matching restore_entries"
        );
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
        let dir =
            std::env::temp_dir().join(format!("ktuner_missing_params_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("rollback.json");
        let guard = lock_ledger_at(ledger.to_str().unwrap()).unwrap();
        let outcome = apply_locked(&recs, true, &guard)
            .expect("apply_quiet must not fail on per-param errors");
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.failed.len(), 2, "both failures must be reported");
        assert_eq!(outcome.failed[0].param, "vm.ktuner_no_such_a");
        assert!(
            !outcome.failed[0].error.is_empty(),
            "error text must survive quiet mode"
        );
        assert!(
            !ledger.exists(),
            "failed parameters must not publish a ledger"
        );
        drop(guard);
        fs::remove_dir_all(dir).unwrap();
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
    fn test_param_to_path_neigh_vlan_interface() {
        // The neighbour family repeats conf's literal-dot layout
        // (neigh/eth0.100/gc_thresh3), so the same blanket dot->slash
        // translation resolved both spellings to neigh/eth0/100/gc_thresh3 —
        // a path that never exists, making `ktuner why` answer
        // "parameter not found" although the file was present.
        assert_eq!(
            param_to_path("net.ipv4.neigh.eth0.100.gc_thresh3"),
            "/proc/sys/net/ipv4/neigh/eth0.100/gc_thresh3"
        );
        assert_eq!(
            param_to_path("net/ipv4/neigh/eth0.100/gc_thresh3"),
            "/proc/sys/net/ipv4/neigh/eth0.100/gc_thresh3"
        );
        assert_eq!(
            param_to_path("net.ipv6.neigh.Br0.100.proxy_qlen"),
            "/proc/sys/net/ipv6/neigh/Br0.100/proxy_qlen"
        );
        // Dot-free interfaces keep the blanket translation's exact result.
        assert_eq!(
            param_to_path("net.ipv4.neigh.default.gc_thresh1"),
            "/proc/sys/net/ipv4/neigh/default/gc_thresh1"
        );
        assert_eq!(
            param_to_path("net.ipv4.neigh.eth0.proxy_delay"),
            "/proc/sys/net/ipv4/neigh/eth0/proxy_delay"
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
    fn test_neigh_vlan_spellings_share_a_ledger_entry() {
        // Same alias-dedup contract one namespace over: both neighbour
        // spellings must resolve to the real literal-dot file so the ledger
        // keeps one entry with the pristine value.
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        let data = merge_entries(
            data,
            [(
                "net.ipv4.neigh.eth0.100.gc_thresh3".to_string(),
                "1024".to_string(),
                "4096".to_string(),
            )],
        );
        let data = merge_entries(
            data,
            [(
                "net/ipv4/neigh/eth0.100/gc_thresh3".to_string(),
                "4096".to_string(),
                "4096".to_string(),
            )],
        );
        assert_eq!(data.entries.len(), 1, "aliases must share one entry");
        let entry = data.entries.values().next().unwrap();
        assert_eq!(entry.path, "/proc/sys/net/ipv4/neigh/eth0.100/gc_thresh3");
        assert_eq!(entry.previous, "1024", "pristine value survives the alias");
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
    fn forbidden_paths_match_the_recorded_write_target() {
        // The same list, applied to a *path*: this is the form `restore`
        // checks, because the ledger's recorded path is what the kernel
        // receives. Equivalent spellings of a deny-listed file must all match.
        for path in [
            "/proc/sys/kernel/core_pattern",
            "/proc/sys//kernel/core_pattern",
            "/proc/sys/kernel/../kernel/core_pattern",
            "/proc/sys/kernel/modprobe",
            "/proc/sys/fs/binfmt_misc/register",
        ] {
            assert!(is_forbidden_resolved_path(path), "{path} must be forbidden");
        }
        for path in ["/proc/sys/vm/swappiness", "/proc/sys/kernel/core_uses_pid"] {
            assert!(!is_forbidden_resolved_path(path), "{path} must be allowed");
        }
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

    /// A ledger entry whose recorded `path` predates the dotted-interface
    /// fix (`5448`) must be re-pointed when the same parameter is merged
    /// again, not just refreshed in `applied`.
    ///
    /// `heal_alias_duplicates` cannot do it: it compares canonicalized paths,
    /// and `/proc/sys/net/ipv4/conf/Br0/100/forwarding` differs from the path
    /// this parameter now resolves to. Left stale, the entry's file does not
    /// exist, so every restore skips it — the original value never comes back
    /// and the ledger entry never clears — while the write this merge records
    /// has no usable rollback record at all.
    #[test]
    fn test_merge_refreshes_a_stale_path_for_the_same_param() {
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "net.ipv4.conf.Br0.100.forwarding":{
                    "previous":"0","applied":"1",
                    "path":"/proc/sys/net/ipv4/conf/Br0/100/forwarding"
                }}}"#,
        )
        .unwrap();
        let data = merge_entries(
            data,
            [(
                "net.ipv4.conf.Br0.100.forwarding".to_string(),
                "0".to_string(),
                "1".to_string(),
            )],
        );
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries["net.ipv4.conf.Br0.100.forwarding"];
        assert_eq!(entry.previous, "0");
        assert_eq!(
            entry.path, "/proc/sys/net/ipv4/conf/Br0.100/forwarding",
            "the recorded path must be the one this parameter resolves to now"
        );
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
    fn rollback_heals_a_legacy_duplicate_spelling_ledger() {
        // Ledger shape from before the alias dedup (#3563): tune recorded
        // vm.swappiness 60->10, a later import recorded vm/swappiness 10->5
        // under its own spelling. Restoring both in BTreeMap key order
        // writes 60 and then 10, so the kernel ends up on the INTERMEDIATE
        // value and the pristine 60 never comes back.
        let dir = AtomicTestDir::new("rollback_heal_dup");
        let path = dir.0.join("swappiness");
        fs::write(&path, "5").unwrap(); // live value after both applies
        let mut data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"10","applied":"5","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        for entry in data.entries.values_mut() {
            entry.path = path.to_str().unwrap().to_string();
        }
        let outcome = restore_entries(&data, true);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "60",
            "rollback must restore the pristine pre-ktuner value, not the intermediate"
        );
        assert_eq!(outcome.restored, 1, "one knob, one restore write");
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.skipped, 0);
    }

    #[test]
    fn merge_heals_a_legacy_duplicate_spelling_ledger() {
        // Same legacy ledger, then any later tune/fix/import merge: the pair
        // must collapse instead of surviving into the next ledger
        // generation (where it would double-render in sysctl.d).
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"10","applied":"5","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        let data = merge_entries(
            data,
            [(
                "net.core.somaxconn".to_string(),
                "128".to_string(),
                "4096".to_string(),
            )],
        );
        assert_eq!(data.entries.len(), 2, "one entry per knob");
        let entry = &data.entries["vm.swappiness"];
        assert_eq!(entry.previous, "60", "pristine value survives the heal");
        assert_eq!(entry.applied, "5", "newest applied survives the heal");
    }

    #[test]
    fn merge_heals_unrelated_duplicates_to_the_last_write() {
        // No chain relation between the two records (the knob was set to 20
        // manually between them), so the pristine value cannot be
        // identified. The survivor is the greatest key: exactly the record
        // whose previous the key-order double write leaves in the kernel
        // today, so the healed restore changes no outcome, it only removes
        // the duplicate write.
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"20","applied":"30","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        let data = merge_entries(data, []);
        assert_eq!(data.entries.len(), 1, "one entry per knob");
        let entry = &data.entries["vm/swappiness"];
        assert_eq!(entry.previous, "20", "last-write previous survives");
        assert_eq!(entry.applied, "30", "last-write applied survives");
    }

    #[test]
    fn merge_keeps_a_bidirectional_duplicate_pair_unordered() {
        // Both readings of this pair are internally consistent: tune recorded
        // vm.swappiness 50 -> 100 and import recorded vm/swappiness 100 -> 50
        // (or the other way around). x.applied == y.previous holds in BOTH
        // directions, so the values cannot order the records — the chain
        // collapse used to let the sorted-key iteration pick a direction
        // anyway, recording the intermediate 50 as the "pristine" original.
        // The pair must fall through to the greatest-key rule instead, exactly
        // like the unrelated pair above.
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"50","applied":"100","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"100","applied":"50","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        let data = merge_entries(data, []);
        assert_eq!(data.entries.len(), 1, "one entry per knob");
        let entry = &data.entries["vm/swappiness"];
        assert_eq!(
            entry.previous, "100",
            "an unordered pair keeps the greatest key's record verbatim"
        );
        assert_eq!(entry.applied, "50", "last-write applied survives");
    }

    #[test]
    fn rollback_restores_the_key_order_value_for_a_bidirectional_pair() {
        // The user-visible contract of the greatest-key fallback: the healed
        // restore writes exactly what today's un-healed key-order double
        // write leaves in the kernel (both spellings restored in BTreeMap
        // order, so the greatest key's previous — 100 — wins). Collapsing the
        // bidirectional pair by iteration order instead restored the
        // intermediate 50 while reporting Full and finalizing the ledger.
        let dir = AtomicTestDir::new("rollback_bidirectional");
        let path = dir.0.join("swappiness");
        fs::write(&path, "50").unwrap(); // live value after both applies
        let mut data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"50","applied":"100","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"100","applied":"50","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        for entry in data.entries.values_mut() {
            entry.path = path.to_str().unwrap().to_string();
        }
        let outcome = restore_entries(&data, true);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "100",
            "the healed restore must change no outcome: the key-order double write leaves 100"
        );
        assert_eq!(outcome.restored, 1, "one knob, one restore write");
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.skipped, 0);
    }

    #[test]
    fn merge_still_collapses_a_one_directional_chain_with_repeated_values() {
        // Regression guard for the ambiguity check: a genuine chain whose
        // second run re-applied the first run's value (60 -> 10, then 10 -> 10)
        // is one-directional (10 == 10 forward, 10 != 60 backward), so it must
        // keep collapsing to the pristine 60 — the reverse-match rejection
        // must only bite when both directions hold.
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"10","applied":"10","path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        let data = merge_entries(data, []);
        assert_eq!(data.entries.len(), 1, "one entry per knob");
        let entry = &data.entries["vm.swappiness"];
        assert_eq!(entry.previous, "60", "pristine value survives the heal");
        assert_eq!(entry.applied, "10", "newest applied survives the heal");
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
    fn restore_refuses_a_deny_listed_path_behind_an_innocent_param() {
        // The ledger's recorded path is the write target, so the deny-list must
        // cover it and not only the parameter spelling next to it: a ledger
        // that was edited (or written by an older version) can point an
        // innocent `vm.swappiness` at kernel.core_pattern, and `rollback` runs
        // as root. The recorder stands in for the kernel write, so the test
        // never touches procfs.
        let mut entries = BTreeMap::new();
        entries.insert(
            "vm.swappiness".to_string(),
            RollbackEntry {
                previous: "10".to_string(),
                applied: "20".to_string(),
                path: "/proc/sys/kernel/core_pattern".to_string(),
            },
        );
        let data = RollbackData {
            version: 1,
            entries,
        };

        let mut attempted: Vec<String> = Vec::new();
        let outcome = restore_entries_with(&data, true, &mut |path, _value| {
            attempted.push(path.to_string());
            Ok(())
        });
        assert!(
            attempted.is_empty(),
            "a deny-listed target must never receive a write: {attempted:?}"
        );
        assert_eq!(
            (outcome.restored, outcome.failed, outcome.skipped),
            (0, 1, 0),
            "the entry must be counted as refused, not skipped or restored"
        );
    }

    #[test]
    fn test_restore_entries_counts_readback_mismatch_as_failed() {
        // fs::write returning Ok does not prove the kernel took the value: a
        // write can be clamped, and the vm.dirty_ratio <-> vm.dirty_bytes
        // pair silently clears its sibling. A restore whose write is not
        // confirmed must NOT count as restored — counting it lets rollback
        // report Full and delete the ledger while the live value is still the
        // tuned one, so `previous` is never re-applied. A symlink to /dev/null
        // has the same shape: the write is accepted and the read-back does not
        // hold `previous`.
        let dir = AtomicTestDir::new("rollback_readback_mismatch");
        let discarded = dir.0.join("discarded");
        std::os::unix::fs::symlink("/dev/null", &discarded).unwrap();
        let restored = dir.0.join("restored");
        fs::write(&restored, "30").unwrap();
        let mut entries = BTreeMap::new();
        for (param, path) in [("vm.dirty_bytes", &discarded), ("vm.swappiness", &restored)] {
            entries.insert(
                param.to_string(),
                RollbackEntry {
                    previous: "10".to_string(),
                    applied: "30".to_string(),
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
        // The confirmed restore is restored; the unconfirmed one is a failure
        // so the ledger is kept for a retry.
        assert_eq!(fs::read_to_string(&restored).unwrap(), "10");
        assert_eq!(outcome.restored, 1);
        assert_eq!(outcome.failed, 1);
        assert_eq!(outcome.skipped, 0);
        assert!(!rollback_should_finalize(outcome.failed, outcome.skipped));
    }

    #[test]
    fn restore_rechecks_every_param_after_all_writes_ran() {
        // One restore write can move a DIFFERENT knob than the one addressed:
        // the kernel zeroes the sibling of the dirty pair on every changing
        // write (v6.6 mm/page-writeback.c dirty_ratio_handler /
        // dirty_bytes_handler set the other to 0). BTreeMap order restores
        // vm.dirty_bytes before vm.dirty_ratio, so the ratio write clears the
        // bytes value that was already restored and verified — the per-write
        // read-back cannot see it because each one runs before the next
        // write. Scenario: ratio mode (20) tuned to 30, the operator then set
        // vm.dirty_bytes manually and ktuner applied a new bytes value, so
        // the ledger holds both knobs with non-zero originals. Only a
        // re-check after ALL writes ran catches the cleared sibling and keeps
        // the ledger instead of reporting Full and deleting it.
        let dir = AtomicTestDir::new("restore_recheck_after_writes");
        let ratio = dir.0.join("dirty_ratio");
        let bytes = dir.0.join("dirty_bytes");
        let ratio_path = ratio.to_str().unwrap().to_string();
        let bytes_path = bytes.to_str().unwrap().to_string();
        // Live state at rollback time: both knobs were tuned (30, 512MB).
        fs::write(&ratio, "30").unwrap();
        fs::write(&bytes, "536870912").unwrap();
        let mut entries = BTreeMap::new();
        for (param, previous, applied, path) in [
            (
                "vm.dirty_bytes",
                "268435456",
                "536870912",
                bytes_path.clone(),
            ),
            ("vm.dirty_ratio", "20", "30", ratio_path.clone()),
        ] {
            entries.insert(
                param.to_string(),
                RollbackEntry {
                    previous: previous.to_string(),
                    applied: applied.to_string(),
                    path,
                },
            );
        }
        // The kernel write, simulated on plain files: every changing write to
        // one knob of the pair zeroes its sibling.
        let mut kernel = |path: &str, value: &str| -> std::io::Result<()> {
            fs::write(path, value)?;
            if path == ratio_path {
                fs::write(&bytes, "0")
            } else if path == bytes_path {
                fs::write(&ratio, "0")
            } else {
                Ok(())
            }
        };
        let outcome = restore_entries_with(
            &RollbackData {
                version: 1,
                entries,
            },
            true,
            &mut kernel,
        );
        assert_eq!(
            fs::read_to_string(&ratio).unwrap(),
            "20",
            "the ratio side of the pair is restored"
        );
        assert_eq!(outcome.restored, 1, "only the ratio write holds");
        assert_eq!(
            outcome.failed, 1,
            "the bytes knob was cleared by the ratio write and must count as failed"
        );
        assert_eq!(outcome.skipped, 0);
        assert!(
            !rollback_should_finalize(outcome.failed, outcome.skipped),
            "a cleared sibling must keep the ledger for inspection"
        );
    }

    #[test]
    fn restore_lands_the_disabled_twin_after_its_clearer() {
        // The overcommit pair is mutually exclusive and the kernel clears it
        // UNCONDITIONALLY: mm/util.c overcommit_ratio_handler zeroes
        // sysctl_overcommit_kbytes on every successful write, value change or
        // not (`if (ret == 0 && write) sysctl_overcommit_kbytes = 0;`), and
        // the reverse handler does the same. A strict-overcommit host with a
        // fixed kbytes limit reads the ratio as 0, so ktuner's ratio
        // recommendation fires and the write disables the limit. The ledger
        // then holds both knobs, and the restore has to write the disabled
        // kbytes AFTER the ratio: key order puts "vm.overcommit_kbytes"
        // first, so writing it there would be undone by the ratio's own
        // restore a moment later.
        let dir = AtomicTestDir::new("overcommit_twin_restore");
        let kbytes = dir.0.join("overcommit_kbytes");
        let ratio = dir.0.join("overcommit_ratio");
        let kbytes_path = kbytes.to_str().unwrap().to_string();
        let ratio_path = ratio.to_str().unwrap().to_string();
        // Live state when the rollback runs: ratio 80, fixed limit disabled.
        fs::write(&ratio, "80").unwrap();
        fs::write(&kbytes, "0").unwrap();
        let entries = BTreeMap::from([
            (
                "vm.overcommit_kbytes".to_string(),
                RollbackEntry {
                    previous: "8589934592".into(),
                    applied: "0".into(),
                    path: kbytes_path.clone(),
                },
            ),
            (
                "vm.overcommit_ratio".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "80".into(),
                    path: ratio_path.clone(),
                },
            ),
        ]);
        // The kernel write, simulated on plain files: every write to one knob
        // of the pair disables the other, changed value or not.
        let mut kernel = |path: &str, value: &str| -> std::io::Result<()> {
            fs::write(path, value)?;
            if path == ratio_path {
                fs::write(&kbytes, "0")
            } else if path == kbytes_path {
                fs::write(&ratio, "0")
            } else {
                Ok(())
            }
        };
        let outcome = restore_entries_with(
            &RollbackData {
                version: 1,
                entries,
            },
            true,
            &mut kernel,
        );
        assert_eq!(
            fs::read_to_string(&kbytes).unwrap(),
            "8589934592",
            "the disabled fixed limit comes back with the rollback"
        );
        assert_eq!(
            fs::read_to_string(&ratio).unwrap(),
            "0",
            "the ratio the kernel disabled is left at its recorded original"
        );
        assert_eq!(outcome.restored, 2);
        assert_eq!(outcome.failed, 0);
        assert!(rollback_should_finalize(outcome.failed, outcome.skipped));
    }

    #[test]
    fn restore_recheck_passes_when_no_write_clobbers_a_sibling() {
        // The re-check must not turn a healthy restore into a failure: two
        // independent params, plain writes, both read back their previous.
        let dir = AtomicTestDir::new("restore_recheck_healthy");
        let mut entries = BTreeMap::new();
        for (param, name) in [
            ("vm.swappiness", "swappiness"),
            ("net.core.somaxconn", "somaxconn"),
        ] {
            let path = dir.0.join(name);
            fs::write(&path, "10").unwrap();
            entries.insert(
                param.to_string(),
                RollbackEntry {
                    previous: "10".to_string(),
                    applied: "30".to_string(),
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
        assert_eq!(outcome.restored, 2);
        assert_eq!(outcome.failed, 0);
        assert_eq!(outcome.skipped, 0);
        assert!(rollback_should_finalize(outcome.failed, outcome.skipped));
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

    /// Acquire the exclusive lock on `fd`, retrying while it is briefly
    /// unavailable.
    ///
    /// The CI runners' overlay filesystem can lag a released flock behind
    /// the closing `drop` (#6527), so a single-shot `LOCK_EX | LOCK_NB`
    /// probe of the release flakes under load. The property under test is
    /// exclusion while held and release on drop, not release visibility
    /// within one scheduler tick, so retry for a short bounded window
    /// before reporting the acquire result.
    fn acquire_exclusive_with_retry(fd: std::os::unix::io::RawFd) -> i32 {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            let rc = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 || std::time::Instant::now() > deadline {
                return rc;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
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
        let rc = acquire_exclusive_with_retry(second.as_raw_fd());
        assert_eq!(rc, 0, "acquire after drop must succeed");
        unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_UN) };
        drop(second);
        fs::remove_dir_all(&dir).ok();
    }

    /// A held-over second lock is what the runners' lagging flock release
    /// looks like to the acquiring side after the guard drops (#6527), so
    /// the acquire must retry past it and win once the holdover descriptor
    /// closes.
    #[test]
    fn test_ledger_lock_acquire_tolerates_a_brief_holdover() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_ledger_holdover_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("rollback.json");
        let guard = lock_ledger_at(ledger.to_str().unwrap()).unwrap();
        drop(guard);

        // The holdover: a descriptor that re-acquires the just-released
        // lock and keeps it for a moment, exactly the window the lagging
        // release leaves visible to the next acquire.
        let (acquired, held) = std::sync::mpsc::channel();
        let holdover = std::thread::spawn(move || {
            let file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .open(format!("{}.lock", ledger.to_str().unwrap()))
                .expect("open holdover lock");
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            acquired.send(rc).expect("signal holdover acquire");
            std::thread::sleep(std::time::Duration::from_millis(150));
        });
        assert_eq!(held.recv().unwrap(), 0, "holdover must acquire");

        let second = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("rollback.json.lock"))
            .unwrap();
        let rc = acquire_exclusive_with_retry(second.as_raw_fd());
        assert_eq!(rc, 0, "acquire must outlast the holdover");

        unsafe { libc::flock(second.as_raw_fd(), libc::LOCK_UN) };
        drop(second);
        holdover.join().unwrap();
        fs::remove_dir_all(&dir).ok();
    }

    fn fixture_guard_body() {
        // The guard points at a private ledger OUTSIDE the mounted fixtures;
        // the production constant is bind-mounted to an empty fixture here.
        let guard_dir =
            std::env::temp_dir().join(format!("ktuner-fixture-guard-{}", std::process::id()));
        let guard_ledger = guard_dir.join("rollback.json");
        let guard = lock_ledger_at(guard_ledger.to_str().unwrap()).unwrap();
        let recs = [Recommendation {
            param: "net.core.somaxconn".to_string(),
            current_value: "stale gathered value".to_string(),
            recommended_value: "65535".to_string(),
            writable: true,
            ..Default::default()
        }];
        let outcome = apply_locked(&recs, true, &guard).expect("fixture-guarded apply");
        assert_eq!(outcome.applied, 1);
        assert!(outcome.clamped.is_empty());
        // The entry followed the LOCK, not the production constant: a
        // fixture-guarded transaction publishes its ledger beside its own
        // lockfile, which is the whole point of f56776d7c's fixture locks —
        // otherwise a successful apply in a test would silently write
        // /var/lib/ktuner/rollback.json.
        let data = load_rollback_from(guard_ledger.to_str().unwrap()).unwrap();
        let entry = &data.entries["net.core.somaxconn"];
        assert_eq!(entry.previous, "1024");
        assert_eq!(entry.applied, "65535");
        assert!(
            !Path::new(ROLLBACK_PATH).exists(),
            "a fixture guard must not write the production ledger"
        );
        // Persistence rendered from the guard's own ledger into /etc (also a
        // fixture under the mount namespace).
        assert!(fs::read_to_string(SYSCTL_PERSIST_PATH)
            .unwrap()
            .contains("65535"));
        let _ = fs::remove_dir_all(&guard_dir);
    }

    #[test]
    #[ignore = "requires root and private mount namespaces; only fixture files are written"]
    fn fixture_guard_keeps_the_ledger_private() {
        if std::env::var_os("KTUNER_FIXTURE_CHILD").is_some() {
            fixture_guard_body();
            return;
        }
        assert_eq!(unsafe { libc::geteuid() }, 0);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ktuner-fixture-guard-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(dir.join("varlib")).unwrap();
        fs::create_dir_all(dir.join("etc/sysctl.d")).unwrap();
        fs::write(dir.join("somaxconn"), "1024").unwrap();
        let before = fs::read_to_string("/proc/sys/net/core/somaxconn").unwrap();
        let out = std::process::Command::new("unshare")
            .env("KTUNER_FIXTURE_CHILD", "1")
            .args(["--mount", "--propagation", "private", "sh", "-ec",
                "mount --bind \"$1/varlib\" /var/lib; mount --bind \"$1/etc\" /etc; mount --bind \"$1/somaxconn\" /proc/sys/net/core/somaxconn; exec \"$2\" --exact tuner::tests::fixture_guard_keeps_the_ledger_private --ignored --nocapture",
                "fixture-guard-test"])
            .arg(&dir)
            .arg(std::env::current_exe().unwrap())
            .output()
            .unwrap();
        assert_eq!(
            fs::read_to_string("/proc/sys/net/core/somaxconn").unwrap(),
            before
        );
        let _ = fs::remove_dir_all(&dir);
        assert!(
            out.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Arm switch for `finalize_race_probe`, set only by the preview race
    /// test so every other `rollback_preview_at` caller stays unaffected.
    pub(super) static ARM_FINALIZE_RACE_PROBE: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);

    /// Test-only injection point inside `rollback_preview_at`'s exists ->
    /// read_to_string window (the TOCTOU gap). Deterministic stand-in for a
    /// concurrent `ktuner rollback` finalize racing the preview: the real
    /// finalize may delete the ledger only while holding the exclusive
    /// ledger lock (#5447's discipline), so this probe grabs LOCK_EX
    /// non-blocking and deletes only if the grab succeeds. Against an
    /// unlocked preview the grab succeeds and the delete lands inside the
    /// window — the historical race; against the shared-locked preview the
    /// grab fails and the finalize defers, exactly as a real blocking LOCK_EX
    /// waiter would behind the preview's LOCK_SH.
    pub(super) fn finalize_race_probe(path: &str) {
        use std::sync::atomic::Ordering;
        if !ARM_FINALIZE_RACE_PROBE.load(Ordering::SeqCst) {
            return;
        }
        let lock_path = format!("{path}.lock");
        let file = match fs::OpenOptions::new().write(true).open(&lock_path) {
            Ok(file) => file,
            Err(_) => return,
        };
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            // Exclusive holder present (the preview's shared lock): a real
            // finalize blocks here instead of deleting mid-window.
            return;
        }
        let _ = fs::remove_file(path);
        drop(file);
    }

    #[test]
    fn test_rollback_preview_reports_pending_set() {
        // Control: a normal preview is unchanged by the locking — the pending
        // triples come back in (param, applied, previous) shape, BTreeMap order.
        let dir = AtomicTestDir::new("preview-normal");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        merge_rollback_at(
            path,
            [
                ("vm.swappiness".into(), "60".into(), "10".into()),
                ("net.core.somaxconn".into(), "128".into(), "256".into()),
            ],
        )
        .unwrap();
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(
            entries,
            vec![
                (
                    "net.core.somaxconn".to_string(),
                    "256".to_string(),
                    "128".to_string()
                ),
                (
                    "vm.swappiness".to_string(),
                    "10".to_string(),
                    "60".to_string()
                ),
            ]
        );
    }

    #[test]
    fn test_rollback_preview_empty_when_ledger_absent() {
        // Control: the exit-0 empty case stays empty — no ledger is not an
        // error, `--list` still reports an empty pending set.
        let dir = AtomicTestDir::new("preview-absent");
        let ledger = dir.0.join("rollback.json");
        assert!(rollback_preview_at(ledger.to_str().unwrap())
            .unwrap()
            .is_empty());
    }

    /// `rollback --list` is a documented read-only preview ("Read-only: no
    /// writes" in cmd_rollback, "nothing is written or deleted" in the
    /// README), but the shared-lock acquisition ran before the
    /// ledger-exists check and created the ledger directory (default umask
    /// mode, not the writer's 0700) plus `<ledger>.lock`. A preview with no
    /// ledger must leave the filesystem untouched.
    #[test]
    fn test_rollback_preview_absent_ledger_creates_nothing() {
        let dir = AtomicTestDir::new("preview-absent-no-write");
        let ledger = dir.0.join("nested").join("rollback.json");
        let entries = rollback_preview_at(ledger.to_str().unwrap()).unwrap();
        assert!(entries.is_empty());
        assert!(!ledger.exists(), "preview must not create the ledger");
        assert!(
            !ledger.parent().unwrap().exists(),
            "preview must not create the ledger directory"
        );
        assert!(
            !std::path::Path::new(&format!("{}.lock", ledger.display())).exists(),
            "preview must not create the lock file"
        );
    }

    #[test]
    fn test_rollback_preview_survives_concurrent_finalize_delete() {
        // A rollback finalize deletes the ledger under the exclusive lock; the
        // preview's exists -> read pair must not straddle that delete. Unlocked
        // (pre-fix), the probe's delete lands inside the window and the
        // preview died with 读取 rollback 文件失败 (--list exit 2) instead of
        // reporting the pending set.
        use std::sync::atomic::Ordering;
        let dir = AtomicTestDir::new("preview-race");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        merge_rollback_at(path, [("vm.swappiness".into(), "60".into(), "10".into())]).unwrap();
        ARM_FINALIZE_RACE_PROBE.store(true, Ordering::SeqCst);
        let entries = rollback_preview_at(path)
            .expect("preview must survive a concurrent finalize's delete window");
        ARM_FINALIZE_RACE_PROBE.store(false, Ordering::SeqCst);
        assert_eq!(
            entries,
            vec![(
                "vm.swappiness".to_string(),
                "10".to_string(),
                "60".to_string()
            )]
        );
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
    fn test_write_and_verify_rejects_runtime_dangerous_params() {
        // tune filters vm.nr_hugepages out of the plan (would_skip names it
        // runtime_dangerous) and fix refuses it with the advice to persist
        // instead; the write choke point must hold the same line for every
        // other caller — a library import routes through here, and applying a
        // runtime-dangerous knob from an imported .conf allocates hugepages
        // on a live host, exactly what the policy exists to prevent. The
        // guard fires before any path or write attempt, so both spellings of
        // the knob are refused without side effects even on a host with a
        // writable /proc/sys.
        for spelling in ["vm.nr_hugepages", "vm/nr_hugepages"] {
            let err = write_and_verify(spelling, "10").unwrap_err();
            assert!(
                err.to_string().contains("运行时危险"),
                "the runtime-dangerous guard must refuse {spelling}: {err}"
            );
        }
        // A knob outside the policy keeps flowing through the choke point:
        // the refusal is the guard's, not a blanket write failure. On this
        // read-only container the write itself fails, so assert the error is
        // NOT the guard's message.
        let err = write_and_verify("vm.ktuner_no_such", "1").unwrap_err();
        assert!(!err.to_string().contains("运行时危险"));
    }

    #[test]
    fn test_apply_import_refuses_runtime_dangerous_params() {
        // The public import entrance is the caller the CLI guard never saw:
        // an imported .conf carrying vm.nr_hugepages (either spelling) must
        // be refused by the policy — before the original is read, before any
        // write, before anything lands in the ledger — not applied live with
        // the "same safety net as fix/tune" that entrance promises.
        for spelling in ["vm.nr_hugepages", "vm/nr_hugepages"] {
            let err = apply_import(spelling, "10", None).unwrap_err().to_string();
            assert!(
                err.contains("运行时危险"),
                "import must refuse the runtime-dangerous {spelling}: {err}"
            );
        }
    }

    #[test]
    fn test_apply_quiet_reports_no_clamps_when_nothing_lands() {
        // Two nonexistent params: nothing is written, so nothing can be
        // clamped, and the outcome must report zero applied with an empty
        // clamped list. Only the private fixture lock may be created.
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
        let dir = std::env::temp_dir().join(format!("ktuner_no_clamps_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let ledger = dir.join("rollback.json");
        let guard = lock_ledger_at(ledger.to_str().unwrap()).unwrap();
        let outcome =
            apply_locked(&recs, true, &guard).expect("apply must not fail on per-param errors");
        assert_eq!(outcome.applied, 0);
        assert_eq!(outcome.failed.len(), 2);
        assert!(outcome.clamped.is_empty());
        assert!(
            !ledger.exists(),
            "failed parameters must not publish a ledger"
        );
        drop(guard);
        fs::remove_dir_all(dir).unwrap();
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
