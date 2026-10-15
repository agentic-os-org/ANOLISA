use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use crate::bench::BenchResult;
use crate::category;
use crate::rules::{Category, Recommendation};

const ROLLBACK_PATH: &str = "/var/lib/ktuner/rollback.json";
const SYSCTL_PERSIST_PATH: &str = "/etc/sysctl.d/99-ktuner.conf";

/// Write one advisory line of engine output, dropping a failed write.
///
/// Everything the engine prints is advisory: the verdict is the value the
/// command returns, and the CLI prints its own body from that. stdout can
/// refuse the write, though — a consumer that stopped reading (`ktuner
/// rollback | head -1`), or a stream redirected to a full device — and Rust's
/// `println!` panics on the write error. The engine prints inside its
/// transactions: between the live write and the ledger save in `apply_locked`,
/// between the restores in `restore_entries_with`. A panic there abandons a
/// half-done run — applied knobs stay live with no rollback record of the
/// originals, a restore stops between entries — and replaces the command's
/// documented exit status with the panic code. A consumer that left is a
/// pipeline condition, not a reason to drop the work already started, so the
/// error is swallowed here.
fn emit(args: std::fmt::Arguments<'_>) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_fmt(args);
    let _ = stdout.write_all(b"\n");
}

/// [`emit`] with `println!`-style formatting; the trailing newline keeps every
/// engine line byte-identical to what `println!` produced.
macro_rules! progress {
    () => {
        $crate::tuner::emit(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::tuner::emit(format_args!($($arg)*))
    };
}

#[derive(Serialize, Deserialize, Clone)]
struct RollbackEntry {
    previous: String,
    applied: String,
    path: String,
    /// The rule-layer category of the recommendation that recorded this
    /// entry, so a later `rollback --category` can select the entries a
    /// `tune --category` wrote without re-deriving the category from the
    /// parameter name: the category is a property of the rule, and a
    /// `net.*` knob can carry a security finding. `None` for an entry no
    /// rule recorded — a library import, or a ledger written before this
    /// field existed — and such an entry is selected by no category ever.
    ///
    /// Additive: a reader that does not know the field ignores it (serde
    /// drops unknown keys), and `skip_serializing_if` keeps a `None` out of
    /// the file, so re-serializing an older ledger does not add a key to
    /// entries that never had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<Category>,
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

/// One parameter a batch wrote, and the two values that describe the change:
/// what the ledger recorded as the original and what the kernel took.
pub struct WrittenEntry {
    pub param: String,
    /// The original the ledger recorded under the transaction lock — the value
    /// `ktuner rollback` restores, with [`AppliedFix::recorded_previous`]'s
    /// meaning. This is the ledger's own record when the parameter already had
    /// one (a re-write keeps the FIRST previous), so a caller reporting it
    /// agrees with the ledger and `rollback --list` instead of naming an
    /// intermediate value no rollback can restore. `None` for a write-only
    /// tunable: the write lands but is unrecordable.
    pub previous: Option<String>,
    /// The value the kernel actually took: the request unless the kernel
    /// clamped/normalized it, with [`WriteOutcome::effective`]'s meaning.
    /// Recording the request instead would make the ledger's `applied` differ
    /// from what is live.
    pub applied: String,
}

/// Outcome of applying a batch: which params were written with what, which
/// failed with why, and which the kernel accepted with an adjusted value.
/// Mirrors `RollbackOutcome` so `tune` can report partial failure the way
/// `rollback` already does — the previous return type (a bare count) could not
/// represent failures at all, so quiet mode dropped them entirely. `clamped` is
/// disjoint from `failed`: a rejected write is a failure, an
/// accepted-but-adjusted write is applied with a note.
pub struct ApplyOutcome {
    /// The parameters this batch wrote, in write order — one entry per write,
    /// so [`Self::applied`] is derived from this list and can never drift from
    /// the names it summarizes. A write that also zeroes its mutually
    /// exclusive twin is still one write: the twin is a ledger record and a
    /// kernel side effect, named by [`Self::cleared`], not a write of its own.
    pub written: Vec<WrittenEntry>,
    pub failed: Vec<ApplyFailure>,
    pub clamped: Vec<ClampNote>,
    /// Whether the boot replay is armed: false when a non-sysctl entry was
    /// persisted but `systemctl enable` did not succeed, so the block/THP
    /// values will not be replayed at the next boot. True when the unit is
    /// enabled, and when this batch had nothing outside /proc/sys to persist.
    /// The verdict belongs to the caller: a failed enable is not a write
    /// failure (`applied` and the exit code stay per-write), but a run that
    /// cannot report it promises a replay it never armed.
    pub boot_unit_enabled: bool,
    /// The mutually exclusive twins this batch's writes zeroed as a kernel
    /// side effect, in write order, dotted: exactly the twin records the
    /// batch put in the ledger, so a caller can name what changed besides the
    /// parameters it asked for. A twin that already read 0, or one a write
    /// left alone because the value did not change, is not listed.
    pub cleared: Vec<String>,
}

impl ApplyOutcome {
    /// How many parameters this batch wrote — one per [`Self::written`] entry,
    /// not one per ledger record: a `*_bytes` knob also records the ratio twin
    /// the kernel clears for it, and the batch's progress and exit-code
    /// semantics stay per-write.
    pub fn applied(&self) -> usize {
        self.written.len()
    }
}

pub fn apply(recommendations: &[Recommendation]) -> Result<ApplyOutcome> {
    apply_inner(recommendations, false)
}

pub fn apply_quiet(recommendations: &[Recommendation]) -> Result<ApplyOutcome> {
    apply_inner(recommendations, true)
}

fn apply_inner(recommendations: &[Recommendation], quiet: bool) -> Result<ApplyOutcome> {
    let guard = lock_ledger_at(ROLLBACK_PATH)?;
    apply_locked(recommendations, quiet, &guard)
}

// Keep the transaction guard alive through every write and persistence step.
fn apply_locked(
    recommendations: &[Recommendation],
    quiet: bool,
    guard: &LedgerLock,
) -> Result<ApplyOutcome> {
    // The ledger this transaction mutates, read through the guard's own path
    // so a fixture guard reaches its fixture and not the production constant.
    // An unreadable ledger is refused here, before any live write, and the
    // originals it already holds are the ones this batch must report: a
    // re-write keeps the FIRST previous, so the pre-write read of an
    // already-recorded knob is an intermediate value no rollback restores.
    let ledger = load_rollback_from(&guard.path)?;
    let total = recommendations.len();
    let mut applied_recs: Vec<Recommendation> = Vec::new();
    let mut written: Vec<WrittenEntry> = Vec::new();
    let mut failed: Vec<ApplyFailure> = Vec::new();
    let mut clamped: Vec<ClampNote> = Vec::new();
    let mut cleared: Vec<String> = Vec::new();
    // Nothing outside /proc/sys was applied, so nothing needs a boot replay.
    let mut boot_unit_enabled = true;
    for (i, rec) in recommendations.iter().enumerate() {
        // A mutually exclusive knob disables its twin as a side effect;
        // snapshot the twin before the write or its original is unrecorded
        // and no rollback can ever bring it back. (The `applied` counter
        // below already counts writes, not ledger records, so the twin
        // record leaves the batch's progress and exit code unchanged.)
        let sibling = cleared_sibling_entry(&rec.param);
        match apply_recordable(rec) {
            Ok((previous, outcome)) => {
                // The write cleared the twin only when the kernel's handler
                // for this pair clears it under this write: the snapshot above
                // says what to record, `cleared_by` says whether the write
                // really cleared anything. Taken before `previous` moves into
                // the ledger record below — the pre-write read is what the
                // decision compares against.
                let twin = sibling.filter(|twin| twin.cleared_by(previous.as_deref(), &outcome));
                if !quiet {
                    progress!(
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
                        progress!(
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
                if let Some(recorded) = previous.as_ref() {
                    // The ledger and sysctl.d must describe live reality:
                    // record the value the kernel actually took, not the
                    // request (#4160). A param whose original could not be
                    // read is applied but unrecordable — see apply_recordable.
                    let mut applied_rec = rec_with_effective(rec, &outcome);
                    applied_rec.current_value = recorded.clone();
                    applied_recs.push(applied_rec);
                }
                written.push(WrittenEntry {
                    param: rec.param.clone(),
                    // The ledger's keep-original merge leaves an earlier
                    // record, when there is one, in place of this pre-write
                    // read, so the ledger's own value is the original a later
                    // `rollback` restores and the one this entry must report;
                    // the read is the original of a first write.
                    previous: recorded_previous_value(&ledger, &rec.param).or(previous),
                    applied: outcome.effective.clone(),
                });
                // The write disabled the mutually exclusive twin: record what
                // was live before the write (the snapshot above), or no
                // rollback can ever bring that original back. The twin is a
                // live change of the pair even when the knob itself was
                // unrecordable, so it lands outside the branch.
                if let Some(twin) = twin {
                    cleared.push(twin.entry.0.clone());
                    applied_recs.push(sibling_rec(twin.entry, rec.category.clone()));
                }
            }
            Err(e) => {
                failed.push(ApplyFailure {
                    param: rec.param.clone(),
                    error: e.to_string(),
                });
                if !quiet {
                    progress!(
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
        boot_unit_enabled = persist_from_rollback(guard)?;
        if !quiet {
            progress!();
            // Count writes, not ledger records: a *_bytes knob also records
            // the ratio sibling the kernel clears for it, and the batch's
            // progress and exit-code semantics stay per-write.
            if boot_unit_enabled {
                progress!("  {} 项配置已应用并持久化（重启后自动生效）", written.len());
            } else {
                progress!(
                    "  {} 项配置已应用，但 ktuner-nonsysctl.service 未能启用（systemctl enable 失败），块设备与 THP 等非 sysctl 配置重启后不会自动生效",
                    written.len()
                );
            }
        }
    } else if written.is_empty() && !quiet {
        progress!();
        progress!("  没有配置被成功应用");
    }
    Ok(ApplyOutcome {
        written,
        failed,
        clamped,
        boot_unit_enabled,
        cleared,
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
    /// Whether the boot replay is armed, with [`ApplyOutcome`]'s meaning: a
    /// refused `systemctl enable` means a non-sysctl fix will not survive the
    /// next boot, and `fix` persists through the same choke point as `tune`.
    pub boot_unit_enabled: bool,
    /// The mutually exclusive twin this write zeroed as a kernel side effect,
    /// with [`ApplyOutcome::cleared`]'s meaning: `Some` exactly when the
    /// ledger recorded the twin. `None` for a knob without a twin, a twin that
    /// already read 0, and a write that left the value where it was.
    pub cleared: Option<String>,
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
    // The ledger's own record of this kernel path, when there is one: a later
    // write of an already-recorded knob keeps the FIRST `previous`
    // (merge_entries' keep-original invariant), so the pre-write read is then
    // an intermediate value that neither the ledger holds nor a rollback
    // restores. Reporting it made a second `fix` of the same parameter
    // contradict the ledger and `rollback --list` on the very knob it had just
    // fixed — the same contradiction #6280 fixed for the first write.
    let pristine = recorded_previous_value(&load_rollback()?, &rec.param);
    // Snapshot the twin before the write: a mutually exclusive knob disables
    // it as a kernel side effect, and only the ledger can bring the original
    // back on rollback.
    let sibling = cleared_sibling_entry(&rec.param);
    let (previous, outcome) = apply_recordable(rec)?;
    let mut batch = Vec::new();
    if let Some(recorded) = pristine.as_ref().or(previous.as_ref()) {
        let mut applied = rec_with_effective(rec, &outcome);
        applied.current_value = recorded.clone();
        batch.push(applied);
    }
    // The disabled twin is a live change of the pair even when the knob
    // itself was unrecordable (no readable pristine value), so it extends
    // the batch regardless — as long as the write really cleared it: a no-op
    // write leaves the dirty foreground twin alone, and the record would
    // otherwise claim a kernel state that never existed. It records the
    // knob's own category: the pair is one unit for every later selection.
    let twin = sibling.filter(|twin| twin.cleared_by(previous.as_deref(), &outcome));
    let cleared = twin.as_ref().map(|twin| twin.entry.0.clone());
    batch.extend(twin.map(|twin| sibling_rec(twin.entry, rec.category.clone())));
    let mut boot_unit_enabled = true;
    if !batch.is_empty() {
        save_rollback(&guard, &batch)?;
        boot_unit_enabled = persist_from_rollback(&guard)?;
    }
    Ok(AppliedFix {
        recorded_previous: pristine.or(previous),
        outcome,
        boot_unit_enabled,
        cleared,
    })
}

/// The ledger record a write of the kernel file `identity` belongs to, with
/// whether it was recognized by its recorded `path`.
///
/// Two rules, in this order: the entry whose recorded path IS that kernel file,
/// or — when that recorded path is gone — the entry whose own KEY resolves to
/// the same identity. A key names the same kernel file, so a record written
/// under an older translation of the knob (a dotted interface spelled
/// `.../conf/Br0/100/forwarding` before [`net_iface_path`] kept the interface
/// segment verbatim) is still the record a later write of that file belongs to;
/// [`merge_entries`] re-points it at the path the knob resolves to now. A
/// recorded path that still exists keeps its entry: only a path that is gone
/// can be an older translation of the same file, never a second knob.
///
/// One predicate for every reader of the ledger by kernel file — the merge,
/// the original `fix` reports, the classification a no-op `import` keeps — so
/// no two of them can disagree about which record a write belongs to, and none
/// can read past a record the merge adopts. The third element is the answer's
/// provenance: `true` when the recorded path matched, `false` when the entry
/// was recognized by its key because its recorded path is gone.
fn matching_record<'a>(
    entries: &'a BTreeMap<String, RollbackEntry>,
    identity: &str,
) -> Option<(&'a String, &'a RollbackEntry, bool)> {
    entries.iter().find_map(|(key, entry)| {
        let same_path = canonicalize_path(&entry.path) == identity;
        let stale = !same_path
            && !Path::new(&entry.path).exists()
            && canonicalize_path(&param_to_path(key)) == identity;
        (same_path || stale).then_some((key, entry, same_path))
    })
}

/// The ledger's own record of `param`'s kernel file, for the original a
/// rollback restores, or `None` when the ledger does not record it yet.
///
/// The record is the one [`matching_record`] answers with — the same rule
/// [`merge_entries`] adopts a write by — so the answer is the value that
/// record keeps: merge refreshes `applied`/`category` on the latest write and
/// never the original `previous`. A lookup keyed on the recorded path alone
/// missed the record a merge adopts through its key (a path an older
/// translation left behind) and reported the fresh re-read instead,
/// contradicting the ledger and `rollback` on the very knob just written.
///
/// The lookup runs on the healed view every other consumer reads: a legacy
/// ledger can hold two spellings of one kernel path, and the merge, a restore
/// and the listing all call [`heal_alias_duplicates`] before they act, so the
/// record whose `previous` a rollback writes is the survivor the heal keeps.
/// Reading the raw ledger answered with the discarded record instead.
fn recorded_previous_value(data: &RollbackData, param: &str) -> Option<String> {
    let identity = canonicalize_path(&param_to_path(param));
    // Healed on a copy, so the caller keeps the ledger it loaded: the original
    // to report is the one the survivor of a legacy alias pair holds.
    let mut healed = data.clone();
    heal_alias_duplicates(&mut healed);
    matching_record(&healed.entries, &identity)
        .map(|(_key, entry, _same_path)| entry.previous.clone())
}

/// The classification an imported write of `param` records, looked up in the
/// ledger by the same kernel-file rule [`matching_record`] keys the reported
/// original on, in the healed view that answer reads.
///
/// A write that MOVES the value records none: the live value is then the
/// imported file's, not a rule's, and the ledger's classification is the rule
/// layer's own ("latest write wins", the rule `applied` follows). A write that
/// leaves the value where it was changed nothing the record describes — the
/// live value is still the one the rules put there — so the classification
/// they stamped on it stays. Clearing it would report a rule-tuned knob as
/// unclassified on the strength of a write the kernel did not act on: no
/// `--category` would select the entry, `rollback --category` would leave the
/// rules' own write in place and echo the knob as one no category wrote. That
/// is the same "a no-op write records nothing the kernel did not do" rule the
/// twin recorder follows ([`ClearedSibling::cleared_by`]). A lookup keyed on
/// the recorded path alone found no record for one whose path an older
/// translation left behind, and cleared the classification anyway. Reading the
/// raw ledger read the DISCARDED alias of a legacy pair, and the merge then
/// wrote that category onto the survivor — a rule's classification replaced by
/// an unrelated alias's, on a write that changed nothing.
///
/// `previous` and `effective` are the canonical forms [`read_previous`] and
/// the read-back produce, so an exact match is the no-op test; a value the
/// kernel renders another way does not compare equal and keeps the
/// conservative reading that the value moved.
fn imported_category(
    data: &RollbackData,
    param: &str,
    previous: &str,
    effective: &str,
) -> Option<Category> {
    if previous != effective {
        return None;
    }
    let identity = canonicalize_path(&param_to_path(param));
    // Healed on a copy, like the reported original: the classification to keep
    // is the survivor's, the one the following merge keeps the record of.
    let mut healed = data.clone();
    heal_alias_duplicates(&mut healed);
    matching_record(&healed.entries, &identity)
        .and_then(|(_key, entry, _same_path)| entry.category.clone())
}

/// Render an option-list reading, with whitespace-separated fields collapsed.
/// Bracketed tokens denote the active option. Use [`active_value_for_path`]
/// when the source path is known, since proc sysctl strings may contain
/// literal brackets rather than an option selection.
pub fn active_value(value: &str) -> String {
    let trimmed = value.trim();
    let active = trimmed
        .split_whitespace()
        .find_map(|token| token.strip_prefix('[').and_then(|t| t.strip_suffix(']')))
        .unwrap_or(trimmed);
    active.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Render a known parameter path, preserving literal brackets in proc sysctls.
/// Other paths retain [`active_value`]'s option-list interpretation.
pub fn active_value_for_path(path: &str, value: &str) -> String {
    if path.starts_with("/proc/sys/") {
        value.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        active_value(value)
    }
}

// Recommendations are gathered before locking and may describe an older
// state. Capture a writable original only after the transaction owns the lock.
fn read_previous(param: &str) -> Result<String> {
    let path = param_to_path(param);
    let value = fs::read_to_string(&path)
        .with_context(|| format!("read original value from {path} before applying"))?;
    // The original must reach the ledger in the canonical single-space form
    // every other consumer publishes: the kernel renders multi-value sysctls
    // TAB-separated, and a raw `previous` made `fix`'s output and
    // `rollback --list` disagree with `why`'s `current` about the same knob.
    Ok(active_value_for_path(&path, &value))
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
///
/// The table is keyed on the dotted spelling, but the caller's spelling is not
/// fixed: `apply_import` resolves both spellings to one file
/// ([`param_to_path`]), which is the separator handling `validate_import_value`
/// documents ("other separator spellings that map to the same file cannot slip
/// past") and the reason 33c0d669a dotted-normalized the runtime-dangerous
/// guard. A slashed spelling that missed the table would clear the twin with
/// no record of its original — the loss the table exists to prevent.
///
/// Public because the CLI's `fix --dry-run` preview reports the twin this
/// write would clear (`would_clear`) from this one table: a second copy of
/// the pair relationship could drift from the ledger's snapshot, and the
/// preview would then name a different twin than the write records.
pub fn cleared_sibling(param: &str) -> Option<&'static str> {
    match param.replace('/', ".").as_str() {
        "vm.dirty_bytes" => Some("vm.dirty_ratio"),
        "vm.dirty_ratio" => Some("vm.dirty_bytes"),
        "vm.dirty_background_bytes" => Some("vm.dirty_background_ratio"),
        "vm.dirty_background_ratio" => Some("vm.dirty_background_bytes"),
        "vm.overcommit_kbytes" => Some("vm.overcommit_ratio"),
        "vm.overcommit_ratio" => Some("vm.overcommit_kbytes"),
        _ => None,
    }
}

/// A twin a mutually exclusive write is about to disable: the ledger triple
/// to record for it, plus the clearing condition of the pair's kernel handler.
struct ClearedSibling {
    /// `(twin, live value, "0")`, the record [`sibling_rec`] turns into a
    /// ledger entry.
    entry: (String, String, String),
    /// True for the pairs whose handler zeroes the twin on every accepted
    /// write; false for the dirty foreground pair, whose handler zeroes it
    /// only inside its changed-value branch. See [`ClearedSibling::cleared_by`].
    on_any_write: bool,
}

impl ClearedSibling {
    /// Whether the write that just landed actually cleared the twin.
    ///
    /// `previous` is the knob's own pre-write read and `outcome` the verified
    /// result, so together they say whether the value changed. The background
    /// and overcommit handlers clear the twin on every accepted write
    /// (`if (ret == 0 && write)`: v6.6 mm/page-writeback.c
    /// dirty_background_ratio_handler / dirty_background_bytes_handler,
    /// mm/util.c overcommit_ratio_handler / overcommit_kbytes_handler), while
    /// the dirty foreground handlers clear it only inside their changed-value
    /// branch (`if (ret == 0 && write && vm_dirty_bytes != old_bytes)` and its
    /// ratio twin, mm/page-writeback.c:518-544). A no-op write therefore
    /// leaves vm.dirty_ratio / vm.dirty_bytes alone, and recording the twin
    /// then claims a state the kernel never entered: the false record is what
    /// `is_cleared_twin` keys persistence's skip on, so it hides the twin's
    /// own line from the boot config as well.
    ///
    /// An unreadable original (`previous` is `None`) keeps the record: the
    /// change cannot be ruled out, and a missing record would lose the twin's
    /// original for good.
    fn cleared_by(&self, previous: Option<&str>, outcome: &WriteOutcome) -> bool {
        if self.on_any_write {
            return true;
        }
        match previous {
            Some(previous) => previous != outcome.effective,
            None => true,
        }
    }
}

/// Ledger triple for the twin a mutually exclusive write is about to disable —
/// `(twin, live value, "0")`, read BEFORE the write lands, after which the
/// kernel has already zeroed the twin and the original is gone forever —
/// together with the condition the pair's handler clears it under, so the
/// caller can drop the record when the write did not clear anything.
/// None when `param` disables nothing, the twin is unreadable, or it holds
/// no configured value. The recorded `applied` is "0" because that is the
/// value the kernel puts live, exactly like a clamped read-back (#4160): the
/// ledger must describe live reality for both knobs of the pair, or a
/// rollback restores `dirty_bytes = 0` and leaves the host with a zeroed
/// `dirty_ratio` — writeback throttling silently disabled.
fn cleared_sibling_entry(param: &str) -> Option<ClearedSibling> {
    let sibling = cleared_sibling(param)?;
    let live = read_previous(sibling).ok()?;
    sibling_cleared_record(sibling, &live).map(|entry| ClearedSibling {
        entry,
        on_any_write: clears_twin_on_any_write(param),
    })
}

/// Whether the kernel handler that owns `param` zeroes its mutually exclusive
/// twin on every accepted write, rather than only on a write that changes the
/// knob. The dirty pair is the changing one (v6.6 mm/page-writeback.c:518-544:
/// the twin is zeroed inside the `vm_dirty_bytes != old_bytes` /
/// `vm_dirty_ratio != old_ratio` branch), so a write that leaves the value
/// where it was leaves the twin alone; the background pair and the overcommit
/// pair clear theirs whenever the write is accepted (see
/// [`ClearedSibling::cleared_by`]). The table is keyed on the dotted spelling
/// but callers pass the spelling they were given, which
/// [`cleared_sibling`] already normalizes.
fn clears_twin_on_any_write(param: &str) -> bool {
    matches!(
        param.replace('/', ".").as_str(),
        "vm.dirty_background_bytes"
            | "vm.dirty_background_ratio"
            | "vm.overcommit_kbytes"
            | "vm.overcommit_ratio"
    )
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
/// every field beyond the ledger triple is default — except the category,
/// which the record inherits from the knob whose write cleared the twin:
/// the pair is one unit everywhere else (restore, exclusion), and a
/// `--category` rollback that selects one half takes the other along, so the
/// two records must classify alike.
fn sibling_rec(entry: (String, String, String), category: Category) -> Recommendation {
    Recommendation {
        param: entry.0,
        current_value: entry.1,
        recommended_value: entry.2,
        category,
        ..Default::default()
    }
}

/// Whether `param`'s ledger entry records a kernel side effect: a knob a
/// mutually exclusive twin's write disabled, so its `applied` is 0 while the
/// twin that disabled it is still recorded. Both consumers of the pair share
/// this predicate — persistence never re-applies such a line after its
/// clearer's (the clearer reproduces the zeroed twin at boot), and the
/// restore writes it AFTER its clearer (whose write would zero it again).
///
/// Both sides of the comparison are canonical. `cleared_sibling` answers in
/// the dotted spelling, but a KEY is the raw spelling the caller used:
/// `merge_entries` matches an existing record by the path the parameter
/// resolves to and keeps the FIRST key, so a twin disabled by a later run
/// stays under the slashed key an earlier import wrote. Comparing that raw key
/// against the canonical answer would miss the pair, persist its zero line and
/// restore it before its clearer — whose own restore then zeroes it again,
/// losing the pristine value the record exists to bring back.
fn is_cleared_twin(
    entries: &BTreeMap<String, RollbackEntry>,
    param: &str,
    entry: &RollbackEntry,
) -> bool {
    cleared_twin_of(entries, param, entry).is_some()
}

/// The ledger key whose write cleared `param`, or `None` when `param` holds no
/// cleared twin — the identity [`is_cleared_twin`] answers yes or no, for a
/// caller that has to name the clearer rather than only recognize the pair.
///
/// The key comes back in the spelling the ledger holds it under, not the
/// canonical one: that is the spelling every listing publishes as `param`, so
/// a caller can address the clearer's own row by the name it is given here.
///
/// A pair recorded with BOTH halves at 0 is the one shape the ledger cannot
/// attribute: every handler of a mutually exclusive pair clears its twin on the
/// write (`if (ret == 0 && write)` for the background and overcommit pairs,
/// inside the changed-value branch for the dirty foreground pair), so with both
/// records at 0 each one looks like the other's side effect. Left there, each
/// half is the other's cleared twin: `render_persistence` skips both lines (a
/// ledger holding only the pair renders no sysctl file at all, which then
/// *removes* the one on disk while the ledger still claims the entries), the
/// listing reports each half as cleared by the other, and the restore writes
/// each half after its "clearer". The ratio half is taken as the writer there —
/// its own zero is a value a caller wrote, and the ratio line alone reproduces
/// the live pair at boot, since its write zeroes the bytes half — so a zero
/// bytes record stops being a clearer, while a bytes record that holds a real
/// value keeps clearing the ratio half as before.
fn cleared_twin_of<'a>(
    entries: &'a BTreeMap<String, RollbackEntry>,
    param: &str,
    entry: &RollbackEntry,
) -> Option<&'a str> {
    if entry.applied != "0" {
        return None;
    }
    let canonical = param.replace('/', ".");
    entries
        .keys()
        .find(|k| {
            cleared_sibling(k).is_some_and(|twin| twin == canonical)
                && !(is_ratio_half(&canonical) && entries[*k].applied == "0")
        })
        .map(String::as_str)
}

/// Whether `param` is the ratio half of its mutually exclusive pair. All three
/// pairs are `<something>_ratio` against a byte-counted twin
/// (`vm.dirty_ratio`/`vm.dirty_bytes`, `vm.dirty_background_ratio`/
/// `vm.dirty_background_bytes`, `vm.overcommit_ratio`/`vm.overcommit_kbytes`).
fn is_ratio_half(param: &str) -> bool {
    param.ends_with("_ratio")
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
    // The structural guard belongs here for the same reason the deny-list does:
    // this is the choke point every live write routes through, while
    // `is_safe_param` was enforced only by `apply_import` — its one caller.
    // `apply` / `apply_one` / `apply_quiet` take a caller-built Recommendation,
    // and `param_to_path` translates `..` segments straight into the path it
    // hands to `fs::write`, so a name like
    // `net/ipv4/conf/../../../../../../tmp/x` wrote outside /proc/sys as the
    // caller (root under sudo) instead of being refused.
    if !is_safe_param(param) {
        anyhow::bail!("拒绝写入非法参数名 {param}（路径穿越或空路径段）");
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
        Ok(s) => classify_readback_for_path(path, value, s.trim()),
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
fn classify_readback_for_path(path: &str, value: &str, readback_trimmed: &str) -> ReadbackVerdict {
    // proc_dostring accepts literal brackets; only option-list readers assign
    // selection meaning to them. The source path is known at every caller.
    if path.starts_with("/proc/sys/") {
        classify_scalar_readback(value, readback_trimmed)
    } else {
        classify_readback(value, readback_trimmed)
    }
}

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
    classify_scalar_readback(value, readback_trimmed)
}

fn classify_scalar_readback(value: &str, readback_trimmed: &str) -> ReadbackVerdict {
    let rec_tokens: Vec<&str> = value.split_whitespace().collect();
    let read_tokens: Vec<&str> = readback_trimmed.split_whitespace().collect();
    if rec_tokens == read_tokens {
        return ReadbackVerdict::Verified {
            // The canonical single-space form, not the kernel's TAB-separated
            // rendering: `effective` becomes the ledger's `applied`, the
            // `fix` output and the sysctl.d line, and every other surface
            // publishes the collapsed form (see [`active_value`]).
            effective: read_tokens.join(" "),
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
        // Collapsed like the exact-match arm: a clamped multi-value read-back
        // (kernel.sem-style) is recorded in the canonical single-space form.
        effective: read_tokens.join(" "),
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
    let mut file = create_temporary(&tmp)?;

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

/// Create the private temporary file for `tmp`, reclaiming a leftover that can
/// only be stale.
///
/// The name embeds the pid, so a file left behind by a process killed between
/// the create and the rename carries the pid of a dead process: with pid reuse
/// the next writer of the same target picks the same name and, without a
/// reclaim, every later save fails — the live kernel write already landed, so
/// the ledger loses the only record of the original and persistence never
/// happens. A name is unique per live pid and same-process writers to one path
/// are serialized by the ledger lock, so a file at *our* pid's name cannot be
/// in flight: it is unreachable state.
///
/// Only a regular file is reclaimed. A symlink (or anything else) at the name
/// keeps the refusal that exclusive creation exists for: the temporary must
/// never be an inode another user chose.
fn create_temporary(tmp: &str) -> Result<fs::File> {
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)
    {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(tmp)
                .with_context(|| format!("inspect temporary file {tmp}"))?;
            if !metadata.file_type().is_file() {
                anyhow::bail!(
                    "create private temporary file {tmp}: the name exists and is not a regular file"
                );
            }
            fs::remove_file(tmp).with_context(|| format!("remove stale temporary file {tmp}"))?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(tmp)
                .with_context(|| format!("create private temporary file {tmp}"))
        }
        Err(error) => Err(error).with_context(|| format!("create private temporary file {tmp}")),
    }
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
                // Every write that comes from a rule carries the rule's own
                // category (a cleared twin inherits its clearer's), so the
                // ledger can answer `rollback --category` without guessing
                // from the parameter name.
                Some(r.category.clone()),
            )
        }),
    )
}

/// Merge `(param, previous, applied, category)` entries into a cumulative
/// rollback record. For a kernel path already recorded, keep the ORIGINAL
/// `previous` (the true pre-ktuner value) so rollback always restores
/// pristine state even across multiple tune/fix/import runs; refresh
/// `applied` and `category`, which describe the latest write. New params are
/// added. Pure (no I/O) so the keep-original-previous invariant is
/// unit-testable.
///
/// The category follows the same "latest write wins" rule as `applied`: an
/// entry refreshed by a rule write is classified by that rule, and one
/// refreshed by a library import — which carries no rule category — loses
/// its classification, because the record then describes a value the rules
/// did not write. An unclassified entry is selected by no category: the
/// classification is never guessed from the parameter name.
///
/// Ledgers written before the alias dedup may already hold two spellings of
/// one kernel path (e.g. `vm.swappiness` from tune and `vm/swappiness` from
/// import); [`heal_alias_duplicates`] collapses such pairs before the new
/// entries are merged, so a legacy duplicate cannot survive into the next
/// ledger generation — where it would restore an intermediate value over the
/// original and double-render the knob in sysctl.d.
fn merge_entries<I>(mut data: RollbackData, entries: I) -> RollbackData
where
    I: IntoIterator<Item = (String, String, String, Option<Category>)>,
{
    heal_alias_duplicates(&mut data);
    for (param, previous, applied, category) in entries {
        let path = param_to_path(&param);
        let identity = canonicalize_path(&path);
        // Equivalent spellings must share the first rollback record, or a
        // later alias would restore an intermediate value over the original.
        // The record this write belongs to is the one [`matching_record`]
        // answers with; matching by path alone inserted a second record for one
        // kernel file — the pristine `previous` then sat in a record no restore
        // can reach, while the restore wrote the new record's intermediate
        // value and the ledger could never clear.
        let matched = matching_record(&data.entries, &identity)
            .map(|(key, _entry, same_path)| (key.clone(), same_path));
        if let Some((key, same_path)) = matched {
            if let Some(entry) = data.entries.get_mut(&key) {
                if !same_path {
                    // The recorded path is gone: re-point it at what the
                    // entry's own parameter resolves to now. `previous` stays
                    // the original.
                    entry.path = path.clone();
                }
                entry.applied = applied;
                entry.category = category;
            }
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
                e.category = category.clone();
            })
            .or_insert_with(|| RollbackEntry {
                previous: previous.clone(),
                applied: applied.clone(),
                path,
                category,
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
/// collapses to `x.previous` (the pristine original), `y.applied` (the
/// newest value) and `y.category` (the latest classification), under `x`'s
/// key. A pair where the reverse relation also
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
            let latest_category = data.entries[&y].category.clone();
            let survivor = data.entries.get_mut(&x).unwrap();
            survivor.applied = newest;
            survivor.category = latest_category;
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
    // The ledger is 0600 inside a 0700 root-owned directory (README), and the
    // directory is created here — on every command that takes the lock, not
    // only on a save. Creating it with the umask default instead left
    // /var/lib/ktuner world-listable (0755) for good on any host whose first
    // lock-taking command never merged: `rollback` with no ledger, a refused
    // import, a save that failed after the live write. `merge_rollback_locked`
    // already re-hardens an existing directory; a mode that only that path
    // applies is not enough. DirBuilder (not `set_permissions`) so a directory
    // that already exists is left untouched: `rollback --list` is documented
    // read-only and must not chmod what it did not create.
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .context("创建 rollback 目录失败")?;
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
    I: IntoIterator<Item = (String, String, String, Option<Category>)>,
{
    let guard = lock_ledger_at(path)?;
    merge_rollback_locked(&guard, path, entries)
}

// Callers retain the same descriptor through live writes and persistence.
// Opening a second descriptor here would deadlock against their own flock.
fn merge_rollback_locked<I>(_guard: &LedgerLock, path: &str, entries: I) -> Result<()>
where
    I: IntoIterator<Item = (String, String, String, Option<Category>)>,
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
    apply_import_reporting_boot_unit(param, value, current).map(|_| ())
}

/// [`apply_import`] with the boot-replay verdict kept: `false` when the write
/// recorded a non-sysctl entry and `systemctl enable ktuner-nonsysctl.service`
/// did not succeed, with [`ApplyOutcome::boot_unit_enabled`]'s meaning — the
/// value is live now but will not be replayed after a reboot. `tune`, `fix`
/// and `rollback` report that fact to their callers; a library caller of the
/// import path was told success instead, with no way to learn that the
/// block-device or THP value it just imported is not coming back. `true` when
/// the unit is enabled, and when this write had nothing outside `/proc/sys` to
/// persist. The write, its ledger record and its persistence are exactly
/// [`apply_import`]'s.
pub fn apply_import_reporting_boot_unit(
    param: &str,
    value: &str,
    current: Option<&str>,
) -> Result<bool> {
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
    let data = load_rollback()?;
    let previous = match read_previous(param) {
        Ok(previous) => Some(previous),
        Err(error) if current.is_some() => return Err(error),
        Err(_) => None,
    };
    // A mutually exclusive knob disables its twin as a kernel side effect;
    // snapshot it before the write so the imported change records both knobs
    // of the pair and stays reversible — and drop the record when the write
    // did not clear the twin (a no-op write leaves the dirty foreground twin
    // alone), which would otherwise claim a kernel state that never existed.
    let sibling = cleared_sibling_entry(param);
    let outcome = write_and_verify(param, value)?;
    let sibling = sibling.filter(|twin| twin.cleared_by(previous.as_deref(), &outcome));
    let mut boot_unit_enabled = true;
    if let Some(previous) = previous {
        // No category for a write that moved the value: it came from an
        // imported file, not from a rule recommendation, and a later
        // `rollback --category` must not pretend otherwise (the ledger's
        // classification is the rule layer's own). A write that left the value
        // where it was keeps the classification the record already holds: see
        // [`imported_category`].
        let category = imported_category(&data, param, &previous, &outcome.effective);
        merge_rollback_locked(
            &guard,
            ROLLBACK_PATH,
            std::iter::once((param.to_string(), previous, outcome.effective, category))
                .chain(sibling.map(|twin| (twin.entry.0, twin.entry.1, twin.entry.2, None))),
        )?;
        // Persistence renders the record just written, so its verdict is this
        // write's verdict. A refused `systemctl enable` leaves the value live
        // and the record in place — `rollback` still undoes it — but the boot
        // replay is not armed, and the caller is the only one who can say so.
        boot_unit_enabled = persist_from_rollback(&guard)?;
    }
    Ok(boot_unit_enabled)
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
    // The unit's verdict is the script's exit status, which is the LAST
    // command's status: with two or more entries a failed write on any earlier
    // line was invisible and the unit reported success while that value never
    // landed. Accumulate the failures and exit with them, so every write is
    // covered without giving up on the entries that follow a failing one.
    nonsysctl_script.push_str("rc=0\n\n");

    let mut has_sysctl = false;
    let mut has_nonsysctl = false;

    for (param, entry) in entries {
        if param.starts_with("block/") || param.starts_with("transparent_hugepage/") {
            // The guard is a deliberate skip, not a failure: a device renamed
            // between the apply and the boot (sda -> sdb), a disk that is gone,
            // or a node that only shows up later leaves the path absent. With
            // no `set -e` and no accumulator, the status systemd reads as the
            // unit's verdict would be the exit code of the last rendered line,
            // and `&&` makes a failed guard there that status - a degraded
            // boot for a skip this renderer chose. An if-statement carries no
            // status for the skip, so only a write that really fails sets the
            // accumulator, and `exit $rc` is the unit's verdict.
            nonsysctl_script.push_str(&format!(
                "if [ -f '{}' ]; then echo '{}' > '{}' || rc=1; fi\n",
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

    if has_nonsysctl {
        nonsysctl_script.push_str("\nexit $rc\n");
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
///
/// The structural guard the write choke point enforces holds here for the same
/// reason: the `block/` and `transparent_hugepage/` branch renders the
/// recorded path into the generated script verbatim, so a name like
/// `block/../../../../tmp/x` — which resolves outside /sys — would be replayed
/// by the init system as root at every boot. A ledger written before the guard
/// existed can still hold one.
fn persistable_entries(
    entries: &BTreeMap<String, RollbackEntry>,
) -> BTreeMap<String, RollbackEntry> {
    entries
        .iter()
        .filter(|(param, entry)| {
            is_safe_param(param)
                && !is_forbidden_param(param)
                && !is_forbidden_resolved_path(&entry.path)
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
///
/// Returns whether the boot replay is armed: false when the non-sysctl unit
/// was written but `systemctl enable` did not succeed (no systemd, or a
/// refused enable), so the script will not run at the next boot.
fn persist_from_rollback(guard: &LedgerLock) -> Result<bool> {
    persist_from_rollback_at(
        guard,
        SYSCTL_PERSIST_PATH,
        NONSYSCTL_SCRIPT_PATH,
        NONSYSCTL_SERVICE_PATH,
        "systemctl",
    )
}

/// [`persist_from_rollback`] with the generated-file paths injectable (the
/// [`rollback_preview_at`] / [`finalize_rollback_at`] idiom): a fixture test
/// can land a file the ledger can no longer render, or a fixture guard whose
/// ledger renders one, without touching /etc. `systemctl` names the program
/// the unit is enabled with, so a test can stand in one that refuses.
fn persist_from_rollback_at(
    guard: &LedgerLock,
    sysctl_path: &str,
    script_path: &str,
    service_path: &str,
    systemctl: &str,
) -> Result<bool> {
    let data = load_rollback_from(&guard.path)?;
    persist_rollback_data_at(
        guard,
        &data,
        sysctl_path,
        script_path,
        service_path,
        systemctl,
    )
}

// Render a transaction's proposed ledger before retiring its originals on
// disk. The caller keeps the same ledger lock throughout both writes.
fn persist_rollback_data_at(
    _guard: &LedgerLock,
    data: &RollbackData,
    sysctl_path: &str,
    script_path: &str,
    service_path: &str,
    systemctl: &str,
) -> Result<bool> {
    #[cfg(test)]
    tests::record_batch_write("persist", &_guard.path);
    let entries = persistable_entries(&data.entries);
    let (sysctl_content, nonsysctl_script) = render_persistence(&entries);

    if let Some(sysctl_content) = sysctl_content {
        // sysctl.d convention: world-readable, same as the systemd service
        // file below; write_atomic lands the mode before the rename so no
        // 0600 intermediate is ever visible at the final path.
        // /etc/sysctl.d belongs to the package that replays it at boot
        // (systemd-sysctl), so a missing directory means nothing would read
        // the file: say so instead of guessing at permissions.
        let sysctl_dir = Path::new(sysctl_path).parent().unwrap_or(Path::new("/"));
        if !sysctl_dir.is_dir() {
            anyhow::bail!(
                "未持久化：{} 不存在（本机没有 systemd-sysctl？），已应用的 sysctl 值重启后不会保留",
                sysctl_dir.display()
            );
        }
        write_atomic(sysctl_path, sysctl_content.as_bytes(), 0o644)
            .context("持久化 sysctl 配置失败（需要 root 权限？）")?;
    } else if !remove_persisted(sysctl_path, true) {
        // A file this regenerate cannot render must not survive it. The
        // render comes back empty when every sysctl-class entry the ledger
        // holds is one the deny-list refuses; the file on disk, though, may
        // be the one an earlier generation wrote from those very entries —
        // and systemd-sysctl applies it as root at the next boot, which is
        // the write the deny-list exists to keep off the boot path.
        // `rollback` cannot clean it up either: the deny-listed entry counts
        // as a failed restore, and an incomplete restore keeps the cleanup
        // (files included) back for a retry, so no other command ever
        // removes it. Leaving the failure silent would do the same, hence
        // the error.
        anyhow::bail!("清理 {sysctl_path} 失败（未能删除的配置会在下次启动时重新生效）");
    }

    if let Some(nonsysctl_script) = nonsysctl_script {
        let dir = Path::new(script_path).parent().unwrap();
        fs::create_dir_all(dir).ok();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("设置 {} 权限 0755 失败", dir.display()))?;
        write_atomic(script_path, nonsysctl_script.as_bytes(), 0o755)
            .context("写入非 sysctl 持久化脚本失败")?;

        let service = format!(
            "[Unit]\n\
             Description=Apply ktuner non-sysctl kernel parameters\n\
             After=local-fs.target\n\n\
             [Service]\n\
             Type=oneshot\n\
             ExecStart={script_path}\n\
             RemainAfterExit=yes\n\n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        );

        // A host with no systemd unit directory cannot arm the replay at all,
        // which is the state the refused `systemctl enable` below reports as
        // `boot_unit_enabled: false` (README, "tune output"). It has to be
        // reported rather than raised: the live restores and the regenerated
        // sysctl file have already landed by this point, and an Err here
        // returns before `save_ledger_at`, leaving the ledger on disk still
        // listing the entries the run just restored — a later `rollback`
        // would then undo values that are already back to their originals.
        // The sysctl arm checks its own directory for the same reason; this one
        // hands back the enable's verdict instead of an error.
        let service_dir = Path::new(service_path)
            .parent()
            .unwrap_or_else(|| Path::new("/"));
        if !service_dir.is_dir() {
            return Ok(false);
        }

        write_atomic(service_path, service.as_bytes(), 0o644)
            .context("写入 systemd service 失败")?;

        systemctl_quiet(systemctl, &["daemon-reload"]);
        if !systemctl_quiet(systemctl, &["enable", "ktuner-nonsysctl.service"]) {
            return Ok(false);
        }
    } else {
        // The same invariant one artifact further: the script and its unit
        // render only while the ledger keeps a non-sysctl entry the deny-list
        // accepts, and an edited or pre-filter ledger can hold one whose
        // *path* the filter refuses — `persistable_entries` drops an entry
        // whose recorded path is deny-listed however the parameter is named.
        // The pair an earlier generation wrote from it would still replay that
        // write as root at the next boot, and nothing else removes it, so a
        // failed retire is an error here too.
        //
        // Both removals are attempted before the error is raised. `||` would
        // short-circuit, so a script that refuses to go (an immutable file, a
        // read-only /etc, a directory in its place) would stop the unit from
        // being retired with it — and the unit is what runs the script at the
        // next boot, on a path that may by then hold something systemd fails
        // to execute, which leaves the unit failed and the boot degraded.
        //
        // The retire undoes both artifacts the write arm created: the files
        // AND systemd's enablement of the unit. Deleting the unit file while
        // multi-user.target.wants/ktuner-nonsysctl.service still pointed at it
        // left systemd trying to start a unit that no longer exists at every
        // boot (`systemctl is-enabled` kept answering "enabled" for it), and
        // the terminal cleanup cannot repair that: `finalize_rollback_at`
        // only disables while the unit file is still there, which a retire is
        // exactly the case where it is not. The write arm's pair of calls is
        // mirrored here (disable, then daemon-reload) and their exit statuses
        // stay advisory, as in the cleanup a full rollback runs.
        systemctl_quiet(systemctl, &["disable", "ktuner-nonsysctl.service"]);
        systemctl_quiet(systemctl, &["daemon-reload"]);
        let script_removed = remove_persisted(script_path, true);
        let service_removed = remove_persisted(service_path, true);
        if !script_removed || !service_removed {
            anyhow::bail!(
                "清理 {script_path} 或 {service_path} 失败（未能删除的持久化会在下次启动时重新生效）"
            );
        }
    }

    Ok(true)
}

/// Run `program` (systemctl) with its output discarded. Returns whether it
/// ran and exited 0; a host without systemd fails to spawn it.
fn systemctl_quiet(program: &str, args: &[&str]) -> bool {
    use std::process::Stdio;
    std::process::Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// One ledger row as `rollback --list` publishes it: the recorded
/// (param, applied, previous) triple, the category the recording rule
/// classified it as, and the value live in the kernel now.
///
/// `live` is read from the entry's own path — the file a restore writes —
/// and rendered through [`active_value_for_path`], the reader every other surface
/// uses; `drifted` compares it against `applied` with the same equality the
/// write path uses (`live_reading` below). Both are `None` (JSON null) when
/// the path cannot be read: a device that is gone, a module that is not
/// loaded, a write-only tunable is reported as
/// unreadable, never guessed — an absent reading is not an error, and bytes
/// the kernel stored outside UTF-8 are a value, not a read failure.
///
/// `category` is the rule-layer category of the recommendation that recorded
/// the entry, carried from the ledger as it was recorded (never re-derived
/// from the name: a security finding can sit on a `net.*` knob). It is `None`
/// (JSON null) for a record written before the field existed and for one an
/// imported file supplied, matching how the ledger treats them: no category
/// ever selected them.
///
/// `cleared_by` names the entry whose write zeroed this one, when the kernel
/// zeroed it as the side effect of a mutually exclusive pair — the relation
/// `is_cleared_twin` recognizes, from the side that has to say which
/// parameter is responsible. A row whose `applied` is 0 because a twin
/// disabled it otherwise reads as though ktuner had chosen to zero the knob.
/// It is `None` (JSON null) for every other row: a zero the caller asked for,
/// and a pair whose clearer is not in the published set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackPreview {
    pub param: String,
    pub applied: String,
    pub previous: String,
    pub category: Option<Category>,
    pub live: Option<String>,
    pub drifted: Option<bool>,
    pub cleared_by: Option<String>,
}

/// The live value at `path` and whether it still matches `applied`, or
/// `(None, None)` when the path cannot be read — a device that is gone, a
/// module that is not loaded, a write-only tunable.
///
/// The value is rendered through [`active_value_for_path`], the reader `why`,
/// the ledger's original and the write read-back share, and `drifted` reuses
/// `classify_readback_for_path`, the equality the write path uses to decide
/// whether a write took. Sharing both is what keeps a kernel-rendered value
/// (a bracketed sysfs option, a TAB-separated multi-value sysctl, the
/// leading-token echo of a scalar) from being reported as drift, and keeps
/// the preview from inventing a second, stricter comparison of its own. The
/// read is silently lossy: bytes that are not text decode to the replacement
/// character, exactly as `why`'s reader and detect's comm / cmdline / cpuinfo
/// readers decode the raw bytes the kernel stores (a latin-1
/// `kernel.hostname` is a value, not an unreadable path), while a path that
/// really cannot be read is reported as unreadable in the JSON, never printed
/// and never an error.
fn live_reading(path: &str, applied: &str) -> (Option<String>, Option<bool>) {
    match fs::read(path) {
        Ok(bytes) => {
            let raw = String::from_utf8_lossy(&bytes);
            let drifted = matches!(
                classify_readback_for_path(path, applied, raw.trim()),
                ReadbackVerdict::Clamped { .. }
            );
            (Some(active_value_for_path(path, &raw)), Some(drifted))
        }
        Err(_) => (None, None),
    }
}

/// Map a healed ledger to the rows `--list` publishes, in BTreeMap (param)
/// order. Nothing is filtered or re-sorted: the published set must stay
/// exactly the entry set a rollback would walk.
fn preview_entries(data: &RollbackData) -> Vec<RollbackPreview> {
    data.entries
        .iter()
        .map(|(param, entry)| {
            let (live, drifted) = live_reading(&entry.path, &entry.applied);
            // Read off the published set, not the whole ledger: a clearer the
            // selection left out cannot be named, and a name the listing does
            // not carry would address no row.
            let cleared_by = cleared_twin_of(&data.entries, param, entry).map(str::to_string);
            RollbackPreview {
                param: param.clone(),
                applied: entry.applied.clone(),
                previous: entry.previous.clone(),
                category: entry.category.clone(),
                live,
                drifted,
                cleared_by,
            }
        })
        .collect()
}

pub fn rollback_preview() -> Result<Vec<RollbackPreview>> {
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
fn rollback_preview_at(path: &str) -> Result<Vec<RollbackPreview>> {
    Ok(read_pending_at(path)?
        .map(|data| preview_entries(&data))
        .unwrap_or_default())
}

/// The healed ledger at `path` for a read-only preview, or `None` when there
/// is no ledger, with the locking contract [`rollback_preview_at`] documents.
fn read_pending_at(path: &str) -> Result<Option<RollbackData>> {
    // No ledger = nothing pending, which is not an error (a fresh install, or
    // a completed rollback): --list reports an empty pending set without
    // creating the ledger directory or lock file.
    if !Path::new(path).exists() {
        return Ok(None);
    }
    let _guard = lock_ledger_shared_at(path)?;
    // Re-check under the shared lock: a concurrent finalize holds LOCK_EX
    // while it deletes the ledger, so the state observed here is the state
    // the read below sees.
    if !Path::new(path).exists() {
        return Ok(None);
    }
    #[cfg(test)]
    tests::finalize_race_probe(path);
    let json = fs::read_to_string(path).context("读取 rollback 文件失败")?;
    Ok(Some(parse_rollback_ledger_at(path, &json)?))
}

/// The pending entries a rollback of `params` would restore, for
/// `rollback --list <param>...`.
pub fn rollback_preview_params(params: &[String]) -> Result<Vec<RollbackPreview>> {
    rollback_preview_params_at(params, ROLLBACK_PATH)
}

/// [`rollback_preview_params`] with the ledger path injectable (the
/// [`rollback_preview_at`] idiom).
///
/// The names resolve through [`resolve_ledger_keys`] and [`rollback_units`],
/// the lookup `rollback <param>...` itself runs, so the preview lists exactly
/// the entries that command would restore: each named entry plus the mutually
/// exclusive twin the ledger records for it, in the same param order as the
/// unfiltered `--list`. A name the ledger does not record is the same command
/// error the restore reports; an absent ledger records nothing, so every name
/// misses there too, still without creating the ledger directory or lock file.
fn rollback_preview_params_at(params: &[String], path: &str) -> Result<Vec<RollbackPreview>> {
    let data = read_pending_at(path)?.unwrap_or(RollbackData {
        version: 1,
        entries: BTreeMap::new(),
    });
    let keys = resolve_ledger_keys(&data.entries, params)?;
    let entries = rollback_units(&data.entries, &keys)
        .into_iter()
        .flatten()
        .collect();
    Ok(preview_entries(&RollbackData {
        version: data.version,
        entries,
    }))
}

/// The pending entries a rollback of everything but `excluded` would restore,
/// for `rollback --list --exclude <param>...`.
pub fn rollback_preview_excluding(
    excluded: &[String],
) -> Result<(Vec<String>, Vec<RollbackPreview>)> {
    rollback_preview_excluding_at(excluded, ROLLBACK_PATH)
}

/// [`rollback_preview_excluding`] with the ledger path injectable (the
/// [`rollback_preview_at`] idiom).
///
/// The names resolve through the same [`excluded_units`] selection the restore
/// runs, so the preview lists exactly the entries that command would restore —
/// every recorded entry the names do not address, minus the units an excluded
/// key touches — in the unfiltered `--list`'s param order. A name the ledger
/// does not record is not an error here either: it comes back in the order
/// given, and an absent ledger records nothing, so every name misses and the
/// listing is empty, still without creating the ledger directory or lock file.
fn rollback_preview_excluding_at(
    excluded: &[String],
    path: &str,
) -> Result<(Vec<String>, Vec<RollbackPreview>)> {
    let data = read_pending_at(path)?.unwrap_or(RollbackData {
        version: 1,
        entries: BTreeMap::new(),
    });
    let (unmatched, units) = excluded_units(&data.entries, excluded);
    let entries = units.into_iter().flatten().collect();
    Ok((
        unmatched,
        preview_entries(&RollbackData {
            version: data.version,
            entries,
        }),
    ))
}

/// What a category-scoped rollback previews: the entries the matching
/// restore would retire, plus the two reports that restore answers with.
pub struct CategoryPreview {
    /// `--exclude` names that address no ledger entry, in the order given —
    /// the inert-name report `rollback --exclude` publishes.
    pub unmatched: Vec<String>,
    /// Recorded entries without a category that this run leaves in place, in
    /// param order: no named category can select one, so they are reported
    /// by name instead of silently left behind.
    pub unclassified: Vec<String>,
    /// The entries the matching restore would retire, in the unfiltered
    /// listing's param order.
    pub entries: Vec<RollbackPreview>,
}

/// The entries a category-scoped rollback would restore, for
/// `rollback --list --category <cat>`.
pub fn rollback_preview_categories(
    cats: &[String],
    excluded: &[String],
) -> Result<CategoryPreview> {
    rollback_preview_categories_at(cats, excluded, ROLLBACK_PATH)
}

/// [`rollback_preview_categories`] with the ledger path injectable (the
/// [`rollback_preview_at`] idiom).
///
/// The selection is the one [`rollback_categories`] runs, so the preview
/// lists exactly the entries that command would restore, in the unfiltered
/// `--list`'s param order. An absent ledger selects nothing — every
/// `--exclude` name is unrecorded there and the listing is empty — and, like
/// every preview, it creates neither the ledger directory nor its lock file.
/// An unknown category is refused here for the same reason the restore
/// refuses it (see [`refuse_unknown_categories`]): the preview must not
/// promise a set the selection would never be allowed to build.
fn rollback_preview_categories_at(
    cats: &[String],
    excluded: &[String],
    path: &str,
) -> Result<CategoryPreview> {
    refuse_unknown_categories(cats)?;
    let data = read_pending_at(path)?.unwrap_or(RollbackData {
        version: 1,
        entries: BTreeMap::new(),
    });
    let selection = category_selection(&data.entries, cats, excluded);
    let entries = selection.units.into_iter().flatten().collect();
    Ok(CategoryPreview {
        unmatched: selection.unmatched,
        unclassified: selection.unclassified,
        entries: preview_entries(&RollbackData {
            version: data.version,
            entries,
        }),
    })
}

/// Parse the ledger body read from `path` into the healed entry map every
/// reader walks. A corrupt ledger is an error, never an empty list — silently
/// treating a corrupt ledger as empty is how the original values get lost
/// (cf. #3578) — and the error names the recovery it needs: the file holds the
/// only record of the originals, so it must be repaired, or moved aside with
/// its copy kept, and the command rerun. [`load_rollback_from`] already
/// answers with that remedy; the readers of the same file answered a bare
/// parser error instead, on the one message an operator is most likely to act
/// on by deleting the file.
///
/// Legacy duplicate spellings are healed first, so the restore, the selection
/// and the preview describe what `rollback` will actually perform:
/// `restore_entries` heals before restoring, and an unhealed listing would
/// promise a second restore that never runs and report a `previous` the kernel
/// will never receive.
fn parse_rollback_ledger_at(path: &str, json: &str) -> Result<RollbackData> {
    let mut data: RollbackData = serde_json::from_str(json)
        .with_context(|| format!("parse rollback ledger {path}; {}", ledger_remedy(path)))?;
    heal_alias_duplicates(&mut data);
    Ok(data)
}

/// Restore just the ledger entry a parameter names, leaving every other entry
/// in place: the batch rollback's only escape used to be restoring everything,
/// so one tuning that turned out to have a side effect forced the caller to
/// drop all of it.
///
/// Returns the resolved ledger key (the spelling `rollback --list` publishes)
/// and the same counters a full rollback reports. A parameter the ledger does
/// not record is a command error, not an incomplete restore: there is no
/// original to write back and nothing a retry could do.
pub fn rollback_param(param: &str) -> Result<(String, RollbackOutcome)> {
    rollback_param_at(
        param,
        ROLLBACK_PATH,
        SYSCTL_PERSIST_PATH,
        NONSYSCTL_SERVICE_PATH,
        NONSYSCTL_SCRIPT_PATH,
    )
}

/// Restore the ledger entries the parameters name as one transaction: a host
/// tuned through several parameters is undone in a single run rather than one
/// command per parameter, which also rewrote the persisted file once per
/// parameter and left no single answer when one of the runs failed.
///
/// Every step — the lookup, the restores, the ledger rewrite and the
/// persistence regeneration — happens under one ledger lock, so the persisted
/// file is regenerated once, from the entries that remain. Returns the
/// resolved ledger keys (one per parameter, in the order given and
/// deduplicated, the spelling `rollback --list` publishes) and the aggregate
/// outcome. A parameter the ledger does not record is a command error that
/// refuses the whole command before anything is touched, the same one a single
/// parameter gets: skipping the miss would let one typo drop the rest of the
/// batch while the run still exited 0.
pub fn rollback_params(params: &[String]) -> Result<(Vec<String>, RollbackOutcome)> {
    rollback_params_at(
        params,
        ROLLBACK_PATH,
        SYSCTL_PERSIST_PATH,
        NONSYSCTL_SERVICE_PATH,
        NONSYSCTL_SCRIPT_PATH,
    )
}

/// [`rollback_param`] with the ledger and generated-file paths injectable (the
/// [`rollback_preview_at`] / [`finalize_rollback_at`] idiom): a fixture restores
/// and retires entries in a private directory without touching /var/lib, /etc
/// or /proc/sys.
///
/// One parameter is the batch of length one: the same engine answers both, so
/// the single-parameter contract cannot drift from the batch it is a case of.
fn rollback_param_at(
    param: &str,
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
) -> Result<(String, RollbackOutcome)> {
    let (keys, outcome) = rollback_params_at(
        &[param.to_string()],
        ledger,
        sysctl_path,
        service_path,
        script_path,
    )?;
    // One name resolves to exactly one ledger key: the lookup refuses the
    // command when it has no answer, so the fallback is only ever the spelling
    // the caller gave.
    Ok((
        keys.into_iter().next().unwrap_or_else(|| param.to_string()),
        outcome,
    ))
}

/// [`rollback_params`] with the ledger and generated-file paths injectable (the
/// same idiom as [`rollback_param_at`]).
///
/// One ledger lock covers the whole batch: the lookup, every restore, the
/// ledger rewrite (or final cleanup) and the persistence all happen between
/// the same `lock_ledger_at` and its drop, so a concurrent tune/fix can neither
/// merge into the ledger this run replaces nor read a half-written one, and the
/// persisted file is regenerated from the surviving entries exactly once
/// instead of once per parameter.
fn rollback_params_at(
    params: &[String],
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
) -> Result<(Vec<String>, RollbackOutcome)> {
    let guard = lock_ledger_at(ledger)?;
    if !Path::new(ledger).exists() {
        anyhow::bail!("没有找到 rollback 文件 ({ledger})，可能尚未执行过 tune");
    }
    let json = fs::read_to_string(ledger).context("读取 rollback 文件失败")?;
    // Healed, so the lookup sees the single record the restore will act on.
    let mut data = parse_rollback_ledger_at(ledger, &json)?;

    // Resolve every name before anything is written.
    let keys = resolve_ledger_keys(&data.entries, params)?;
    let units = rollback_units(&data.entries, &keys);

    let outcome = restore_units_at(
        &guard,
        &mut data,
        &units,
        ledger,
        sysctl_path,
        service_path,
        script_path,
    )?;
    Ok((keys, outcome))
}

/// Restore `units` as one transaction, retiring the entries that landed.
///
/// The step both rollback forms share: the caller selects its units (named
/// parameters, or every entry an exclusion leaves), takes the lock, reads the
/// ledger and heals it, and this runs the same restore -> retire -> persist
/// sequence against that selection. One unit at a time, and a unit that did
/// not fully restore keeps its whole record (a retry restores the values that
/// did not land, and until then the persisted file still replays those
/// entries — the same contract a partial full rollback has). When nothing
/// lands, neither the ledger nor the persisted file is touched; when the
/// ledger empties, the terminal cleanup runs.
fn restore_units_at(
    guard: &LedgerLock,
    data: &mut RollbackData,
    units: &[BTreeMap<String, RollbackEntry>],
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
) -> Result<RollbackOutcome> {
    restore_units_with(
        guard,
        data,
        units,
        ledger,
        sysctl_path,
        service_path,
        script_path,
        "systemctl",
    )
}

/// [`restore_units_at`] with the `systemctl` program injectable, the same
/// seam [`persist_rollback_data_at`] and [`finalize_rollback_at`] take, so a
/// test can stand in for a host whose `systemctl` refuses.
#[allow(clippy::too_many_arguments)]
fn restore_units_with(
    guard: &LedgerLock,
    data: &mut RollbackData,
    units: &[BTreeMap<String, RollbackEntry>],
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
    systemctl: &str,
) -> Result<RollbackOutcome> {
    let mut outcome = RollbackOutcome {
        restored_params: Vec::new(),
        failures: Vec::new(),
        missing: Vec::new(),
        leftovers: Vec::new(),
        boot_unit_enabled: true,
    };
    let mut retired = false;
    for unit in units {
        let subset = RollbackData {
            version: data.version,
            entries: unit.clone(),
        };
        let unit_outcome = restore_entries(&subset, true);
        // A unit that did not fully restore keeps its whole record: a retry
        // restores the values that did not land, and until then the persisted
        // file still replays those entries — the same contract a partial full
        // rollback has. Retiring a landed half would let the next tune/fix
        // regenerate persistence from the ledger and return the rolled-back
        // value to the boot path. Each unit decides on its own, so the batch
        // retires what did restore and keeps what did not.
        if rollback_should_finalize(unit_outcome.failed(), unit_outcome.skipped()) {
            for key in unit.keys() {
                data.entries.remove(key);
            }
            retired = true;
        }
        outcome.restored_params.extend(unit_outcome.restored_params);
        outcome.failures.extend(unit_outcome.failures);
        outcome.missing.extend(unit_outcome.missing);
        outcome.leftovers.extend(unit_outcome.leftovers);
    }

    if !retired {
        // Nothing landed, so the ledger and the persisted file still describe
        // the live kernel exactly as they did and neither is touched: no
        // half-batch state is written, the same contract a single parameter
        // keeps when its unit does not restore.
        return Ok(outcome);
    }

    if data.entries.is_empty() {
        // Nothing left to restore: the same terminal cleanup a full rollback
        // runs. A file or ledger that survives it counts as a failure, so the
        // caller exits 1 — the surviving file would re-apply the tuned values
        // at the next boot, and the kept ledger preserves the originals for a
        // retry.
        let cleanup_leftovers = finalize_rollback_at(
            ledger,
            sysctl_path,
            service_path,
            script_path,
            true,
            systemctl,
        );
        outcome.leftovers.extend(cleanup_leftovers);
        return Ok(outcome);
    }
    // A failed regenerate can leave the old boot config replaying the value
    // just restored. Keep its original in the ledger until every persisted
    // file is updated, so the same parameter can be retried after the failure.
    outcome.boot_unit_enabled = persist_rollback_data_at(
        guard,
        data,
        sysctl_path,
        script_path,
        service_path,
        systemctl,
    )?;
    save_ledger_at(ledger, data)?;
    Ok(outcome)
}

/// Restore every recorded entry except the ones `excluded` names.
///
/// The complement of [`rollback_params`]: instead of naming what to restore,
/// the caller names what to keep, so one tuning can be withdrawn without the
/// caller subtracting its name from a listing that a concurrent run may have
/// changed. The names resolve through the same lookup as every other rollback
/// form, and everything the names do not address is restored as one
/// transaction through the same tail [`rollback_params_at`] runs. A name the
/// ledger does not record is not an error: it comes back (in the order given)
/// so an exclusion that protected nothing is visible instead of silent.
pub fn rollback_excluding(excluded: &[String]) -> Result<(Vec<String>, RollbackOutcome)> {
    rollback_excluding_at(
        excluded,
        ROLLBACK_PATH,
        SYSCTL_PERSIST_PATH,
        NONSYSCTL_SERVICE_PATH,
        NONSYSCTL_SCRIPT_PATH,
    )
}

/// [`rollback_excluding`] with the ledger and generated-file paths injectable
/// (the same idiom as [`rollback_params_at`]).
fn rollback_excluding_at(
    excluded: &[String],
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
) -> Result<(Vec<String>, RollbackOutcome)> {
    let guard = lock_ledger_at(ledger)?;
    if !Path::new(ledger).exists() {
        anyhow::bail!("没有找到 rollback 文件 ({ledger})，可能尚未执行过 tune");
    }
    let json = fs::read_to_string(ledger).context("读取 rollback 文件失败")?;
    // Healed, so the selection acts on the single record the restore will.
    let mut data = parse_rollback_ledger_at(ledger, &json)?;

    let (unmatched, units) = excluded_units(&data.entries, excluded);
    let outcome = restore_units_at(
        &guard,
        &mut data,
        &units,
        ledger,
        sysctl_path,
        service_path,
        script_path,
    )?;
    Ok((unmatched, outcome))
}

/// What a category-scoped rollback did: the two reports its caller owes and
/// the attempt's counters.
pub struct CategoryRollback {
    /// `--exclude` names that address no ledger entry, in the order given.
    pub unmatched: Vec<String>,
    /// Recorded entries without a category that stay in the ledger, in param
    /// order.
    pub unclassified: Vec<String>,
    /// The restored/failed/skipped counters of the attempt, with the meaning
    /// every other rollback form gives them.
    pub outcome: RollbackOutcome,
}

/// Restore every recorded entry whose category one of `cats` names, minus
/// the entries the `excluded` names address.
///
/// The category-scoped counterpart of [`rollback_params`] and
/// [`rollback_excluding`]: the caller names the rule categories that wrote
/// the entries, which is the one question about a recorded change that a
/// listing cannot answer — the category belongs to the rule, and a `net.*`
/// knob can carry a security finding. The selection runs through
/// [`category_selection`] and the restore through the same one-transaction
/// tail every other form uses.
pub fn rollback_categories(cats: &[String], excluded: &[String]) -> Result<CategoryRollback> {
    rollback_categories_at(
        cats,
        excluded,
        ROLLBACK_PATH,
        SYSCTL_PERSIST_PATH,
        NONSYSCTL_SERVICE_PATH,
        NONSYSCTL_SCRIPT_PATH,
    )
}

/// [`rollback_categories`] with the ledger and generated-file paths
/// injectable (the same idiom as [`rollback_params_at`]).
fn rollback_categories_at(
    cats: &[String],
    excluded: &[String],
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
) -> Result<CategoryRollback> {
    refuse_unknown_categories(cats)?;
    let guard = lock_ledger_at(ledger)?;
    if !Path::new(ledger).exists() {
        anyhow::bail!("没有找到 rollback 文件 ({ledger})，可能尚未执行过 tune");
    }
    let json = fs::read_to_string(ledger).context("读取 rollback 文件失败")?;
    // Healed, so the selection acts on the single record the restore will.
    let mut data = parse_rollback_ledger_at(ledger, &json)?;

    let selection = category_selection(&data.entries, cats, excluded);
    let outcome = restore_units_at(
        &guard,
        &mut data,
        &selection.units,
        ledger,
        sysctl_path,
        service_path,
        script_path,
    )?;
    Ok(CategoryRollback {
        unmatched: selection.unmatched,
        unclassified: selection.unclassified,
        outcome,
    })
}

/// The restore units an exclusion keeps, and the excluded names that
/// addressed no ledger entry (in the order given).
///
/// Every recorded key the names do not address is selected, so the units are
/// the complement of the exclusion, in the ledger's own param order. A unit
/// an excluded key belongs to is dropped whole: the unit is the entry plus
/// the mutually exclusive twin the ledger recorded with it, and the kernel
/// zeroes either knob when the other is written, so restoring the free half
/// would both zero the excluded entry and leave the ledger describing a
/// kernel it does not match.
fn excluded_units(
    entries: &BTreeMap<String, RollbackEntry>,
    excluded: &[String],
) -> (Vec<String>, Vec<BTreeMap<String, RollbackEntry>>) {
    let (excluded_keys, unmatched) = match_ledger_keys(entries, excluded);
    let keys: Vec<String> = entries
        .keys()
        .filter(|key| !excluded_keys.contains(key))
        .cloned()
        .collect();
    let units = rollback_units(entries, &keys)
        .into_iter()
        .filter(|unit| !unit.keys().any(|key| excluded_keys.contains(key)))
        .collect();
    (unmatched, units)
}

/// The outcome of [`category_selection`]: the restore units the named
/// categories select, and the two name lists the caller reports.
struct CategorySelection {
    /// `--exclude` names that address no ledger entry, in the order given.
    unmatched: Vec<String>,
    /// Recorded entries without a category that this selection leaves in
    /// place, in param order.
    unclassified: Vec<String>,
    /// The restore units to restore, in the ledger's param order.
    units: Vec<BTreeMap<String, RollbackEntry>>,
}

/// Refuse a category value no rule category names, with the message `check`,
/// `tune` and `rollback --category` answer: the vocabulary is closed
/// ([`category::validate_category`]), and a name outside it is an argument
/// error, never a selection.
///
/// The CLI validates its own `--category` values before it calls in, which is
/// a guard at a *caller*; [`rollback_categories`] and
/// [`rollback_preview_categories`] are library entry points reachable without
/// that caller, so the rule has to hold here too. An unrecognized value must
/// not reach the selection: the predicate it would be compared against holds
/// no entry for a name that is not in the vocabulary, and treating the name as
/// "matches every entry" turned a typo into a restore of the whole categorized
/// ledger.
///
/// Both callers run this before they take the ledger lock, so a refused
/// command leaves nothing behind — no ledger directory, no lock file.
fn refuse_unknown_categories(cats: &[String]) -> Result<()> {
    for cat in cats {
        category::validate_category(cat)?;
    }
    Ok(())
}

/// The restore units a category-scoped rollback selects, and the reports its
/// caller owes.
///
/// Selection order mirrors `tune --category … --exclude …`: every recorded
/// entry whose category one of `cats` names is selected first, then the
/// entries the `excluded` names address are dropped, so the exclusions
/// narrow the categories' set rather than the other way around. The excluded
/// names resolve through [`match_ledger_keys`], the lookup every rollback
/// form shares, and a unit an excluded key belongs to is dropped whole
/// exactly as [`excluded_units`] drops it — the second filter is what keeps
/// the twin of an excluded entry out of the restore.
///
/// An entry with no category is selected by no value: the classification is
/// the rule layer's own and is never guessed from the parameter name, so
/// such records come back in `unclassified`, in param order, one name per
/// entry this selection leaves in place. A record the selection restores as
/// a unit member is not reported.
fn category_selection(
    entries: &BTreeMap<String, RollbackEntry>,
    cats: &[String],
    excluded: &[String],
) -> CategorySelection {
    let (excluded_keys, unmatched) = match_ledger_keys(entries, excluded);
    let keys: Vec<String> = entries
        .iter()
        .filter(|(key, entry)| {
            entry.category.as_ref().is_some_and(|category| {
                category::matches_any_category(&category_spelling(key), category, cats)
            })
        })
        .map(|(key, _)| key.to_string())
        .filter(|key| !excluded_keys.contains(key))
        .collect();
    let units: Vec<BTreeMap<String, RollbackEntry>> = rollback_units(entries, &keys)
        .into_iter()
        .filter(|unit| !unit.keys().any(|key| excluded_keys.contains(key)))
        .collect();
    let unclassified = entries
        .iter()
        .filter(|(key, entry)| {
            entry.category.is_none() && !units.iter().any(|unit| unit.contains_key(*key))
        })
        .map(|(key, _)| key.to_string())
        .collect();
    CategorySelection {
        unmatched,
        unclassified,
        units,
    }
}

/// The parameter spelling [`category::category_matches`] reads, derived from
/// a ledger key.
///
/// A key is the first spelling a writer recorded, and a library import is
/// free to leave a slashed or uppercased sysctl alias behind the record a
/// rule later refreshed; [`category::param_subcategory`] reads such a name
/// as "other" and the entry would silently leave its own category.
/// Normalize the separators and the case the way the CLI resolves aliases,
/// except for the two sysfs families, whose names are literal paths
/// (`block/<device>/…`, `transparent_hugepage/…`). Every predicate is a
/// prefix or substring test on the family, so the normalization cannot move
/// an entry between categories.
fn category_spelling(key: &str) -> String {
    if key.starts_with("block/") || key.starts_with("transparent_hugepage/") {
        key.to_string()
    } else {
        key.replace('/', ".").to_lowercase()
    }
}

/// The ledger key each name addresses and the names that address none.
///
/// One pass, so every consumer of a name list resolves entries through the
/// same lookup ([`ledger_key_for`]) with the same deduplication: two spellings
/// of one kernel path resolve to one key, so the second is the same record,
/// not a second restore of it. `unmatched` keeps the order the names were
/// given in.
fn match_ledger_keys(
    entries: &BTreeMap<String, RollbackEntry>,
    names: &[String],
) -> (Vec<String>, Vec<String>) {
    let mut keys: Vec<String> = Vec::new();
    let mut unmatched: Vec<String> = Vec::new();
    for name in names {
        match ledger_key_for(entries, name) {
            // Two spellings of one kernel path resolve to one key, so the
            // second is the same record, not a second restore of it.
            Some(key) => {
                if !keys.contains(&key) {
                    keys.push(key);
                }
            }
            None => unmatched.push(name.clone()),
        }
    }
    (keys, unmatched)
}

/// The ledger key each parameter names, in the order given and deduplicated.
///
/// A name the ledger does not record refuses the WHOLE command (the same
/// command error a single parameter gets, reported for the first miss in the
/// order given): a silently skipped miss would let one typo drop the rest of
/// the batch while the run still reported success.
fn resolve_ledger_keys(
    entries: &BTreeMap<String, RollbackEntry>,
    params: &[String],
) -> Result<Vec<String>> {
    let (keys, unmatched) = match_ledger_keys(entries, params);
    if let Some(param) = unmatched.first() {
        anyhow::bail!("parameter not recorded in the rollback ledger: {param}");
    }
    Ok(keys)
}

/// The restore units the resolved `keys` cover, in the order of `keys`.
///
/// The unit is the named entry plus the mutually exclusive twin the ledger
/// records for it: the kernel zeroes either knob when the other is written,
/// so restoring or retiring half the pair would leave this run's own side
/// effect outside the ledger. Naming both halves covers one unit, so the
/// pair is restored (and counted) once, not twice.
fn rollback_units(
    entries: &BTreeMap<String, RollbackEntry>,
    keys: &[String],
) -> Vec<BTreeMap<String, RollbackEntry>> {
    let mut units: Vec<BTreeMap<String, RollbackEntry>> = Vec::new();
    for key in keys {
        let mut unit = BTreeMap::new();
        unit.insert(key.clone(), entries[key].clone());
        if let Some(twin) = paired_ledger_key(entries, key) {
            unit.insert(twin.clone(), entries[&twin].clone());
        }
        if !units.iter().any(|seen| seen.keys().eq(unit.keys())) {
            units.push(unit);
        }
    }
    units
}

/// Replace the ledger with `data`, keeping the merge writer's contract: a
/// private 0600 file published atomically, so no reader can observe a
/// half-written ledger.
fn save_ledger_at(path: &str, data: &RollbackData) -> Result<()> {
    #[cfg(test)]
    tests::record_batch_write("ledger", path);
    let json = serde_json::to_string_pretty(data)?;
    write_atomic(path, json.as_bytes(), 0o600).context("保存 rollback 文件失败")
}

/// The ledger key a user-supplied parameter addresses.
///
/// Ledger keys are parameter spellings, so each side resolves through
/// [`param_to_path`] and the results are canonicalized — the same "one kernel
/// file" test [`merge_entries`] applies before it treats two spellings as one
/// record. Dotted and slashed sysctl aliases therefore reach one entry, while
/// the per-interface families keep their literal dots and case (`Br0.100`
/// stays a different identity from `br0.100`). Pure so the lookup contract is
/// unit-testable.
///
/// The canonicalization is for the recorded side; a user-supplied spelling
/// that [`is_safe_param`] rejects (`vm..swappiness`, `/vm.swappiness`, a `..`
/// segment) addresses no entry, even when the kernel would collapse it onto a
/// recorded path — `why`, `check --only`, `fix` and `tune --exclude` treat the
/// same spelling as an unknown name.
fn ledger_key_for(entries: &BTreeMap<String, RollbackEntry>, param: &str) -> Option<String> {
    if !is_safe_param(param) {
        return None;
    }
    let wanted = canonicalize_path(&param_to_path(param));
    entries
        .iter()
        .find(|(recorded, _)| canonicalize_path(&param_to_path(recorded)) == wanted)
        .map(|(recorded, _)| recorded.clone())
}

/// The ledger key recording the mutually exclusive twin of `param`, when the
/// ledger holds it.
///
/// [`cleared_sibling`] answers in the dotted spelling, but a key is the raw
/// spelling a caller used — [`merge_entries`] keeps the first one it saw — so a
/// twin disabled by a later import can sit under a slashed key. The comparison
/// normalizes the separator for the same reason `is_cleared_twin` does.
fn paired_ledger_key(entries: &BTreeMap<String, RollbackEntry>, param: &str) -> Option<String> {
    let twin = cleared_sibling(param)?;
    entries
        .keys()
        .find(|recorded| recorded.replace('/', ".") == twin)
        .cloned()
}

/// Outcome of a rollback attempt: which params were restored, and which ones
/// the attempt left behind.
///
/// The entries are carried, not just counted: a run that exits 1 has to be
/// able to name what it did not restore, the way `tune` names the parameters
/// it failed to write — and a run that exits 0 has to be able to name what it
/// did restore, because the selection is expanded inside the engine (a
/// mutually exclusive twin comes along with the entry that names it), so the
/// caller never sees the set it actually retired. `restored`, `failed` and
/// `skipped` are therefore the lengths of the lists below, and cannot drift
/// from the names a caller reports.
pub struct RollbackOutcome {
    /// The ledger keys that landed: the accepted writes, then the refused
    /// writes whose recorded original turned out to be live because their
    /// twin's restore landed. Both are ledger keys — the spelling
    /// `rollback --list` publishes — so a restored twin is named here by its
    /// own key, not by the parameter whose restore brought it back.
    pub restored_params: Vec<String>,
    /// The entries whose restore did not land, with the reason.
    pub failures: Vec<ApplyFailure>,
    /// The ledger keys whose recorded path does not exist, so there was
    /// nothing to write.
    pub missing: Vec<String>,
    /// The persisted files and the ledger a terminal cleanup could not
    /// remove. Each survivor re-applies the tuned values at the next boot (or
    /// keeps the originals for a retry), so it counts like a failed restore
    /// even though no parameter is at fault.
    pub leftovers: Vec<String>,
    /// Whether the non-sysctl boot replay is still armed after this rollback.
    /// A partial rollback regenerates the persisted files from the entries it
    /// keeps and re-runs `systemctl enable`; `false` means that enable was
    /// refused, so the block-device or THP values still recorded will not be
    /// replayed after a reboot. `true` when nothing outside `/proc/sys` is
    /// left to persist, and for every rollback that regenerates nothing.
    pub boot_unit_enabled: bool,
}

impl RollbackOutcome {
    /// How many ledger entries landed: the length of [`Self::restored_params`],
    /// so the count cannot drift from the names a caller reports.
    pub fn restored(&self) -> usize {
        self.restored_params.len()
    }

    /// How many units of the attempt did not land: the failed parameters plus
    /// whatever the cleanup left behind.
    pub fn failed(&self) -> usize {
        self.failures.len() + self.leftovers.len()
    }

    /// How many entries were skipped because their recorded path was absent.
    pub fn skipped(&self) -> usize {
        self.missing.len()
    }

    /// Whether all recorded values were restored, including an empty ledger.
    /// Failed writes and missing paths leave restoration incomplete.
    pub fn is_complete(&self) -> bool {
        rollback_should_finalize(self.failed(), self.skipped())
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
    if outcome.restored() == 0 {
        RollbackStatus::Nothing
    } else if outcome.failed() == 0 && outcome.skipped() == 0 {
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
    // A write the kernel REFUSES does not prove the knob is unrestored. The
    // kernel enforces a minimum on both dirty `*_bytes` knobs (v6.6
    // mm/page-writeback.c registers `dirty_bytes` with `dirty_bytes_min`
    // = 2 * PAGE_SIZE and `dirty_background_bytes` with `SYSCTL_ONE`, and
    // proc_doulongvec_minmax rejects anything below them with EINVAL), so the
    // pristine 0 a ratio-mode host records cannot be written back through its
    // own file at all. The zero comes back through the twin's restore instead:
    // `dirty_ratio_handler` clears `vm.dirty_bytes` on every changed write.
    // Verdicts therefore wait for the post-loop read below, exactly like the
    // divergence check for accepted writes.
    let mut refused: Vec<(&String, String)> = Vec::new();
    let mut failures: Vec<ApplyFailure> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
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
                progress!("  {} {} : 拒绝恢复（代码执行参数）", "✗".red(), param);
            }
            failures.push(ApplyFailure {
                param: param.clone(),
                error: "拒绝恢复（代码执行参数）".to_string(),
            });
            continue;
        }
        // The structural guard the write choke point enforces belongs here for
        // the same reason the deny-list does: `restore` writes the path the
        // ledger recorded, and a name like
        // `net/ipv4/conf/../../../../..<file>` resolves outside /proc/sys and
        // /sys entirely. The apply path refuses such a name before writing it
        // (`write_and_verify`); a ledger written before that guard existed, or
        // by a library caller, still holds one, and restoring it would replay
        // the same traversal as root — the write the undo path must refuse for
        // the write path's own decision to mean anything.
        if !is_safe_param(param) {
            if !quiet {
                progress!(
                    "  {} {} : 拒绝恢复非法参数名（路径穿越或空路径段）",
                    "✗".red(),
                    param
                );
            }
            failures.push(ApplyFailure {
                param: param.clone(),
                error: "拒绝恢复非法参数名（路径穿越或空路径段）".to_string(),
            });
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
                                progress!(
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
                                progress!(
                                    "  {} {} : 回读为 {}（期望 {}），未恢复",
                                    "✗".red(),
                                    param,
                                    effective,
                                    entry.previous
                                );
                            }
                            failures.push(ApplyFailure {
                                param: param.clone(),
                                error: format!(
                                    "回读为 {effective}（期望 {}），未恢复",
                                    entry.previous
                                ),
                            });
                        }
                    }
                }
                Err(e) => {
                    if !quiet {
                        progress!("  {} {} : {}", "✗".red(), param, e);
                    }
                    refused.push((param, e.to_string()));
                }
            }
        } else {
            if !quiet {
                progress!("  {} {} : 路径不存在，跳过", "⊘".yellow(), param);
            }
            missing.push(param.clone());
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
    // The entries that landed, in the order the batch landed them: the
    // accepted writes, then the refused writes the twin's restore brought
    // back. A write whose final read-back diverged is dropped from the list,
    // so the names and the count are one value.
    let mut restored_params: Vec<String> = verified.iter().map(|param| (*param).clone()).collect();
    for param in &verified {
        let entry = &data.entries[*param];
        if let ReadbackVerdict::Clamped { effective } =
            readback_verdict(&entry.path, &entry.previous)
        {
            if !quiet {
                progress!(
                    "  {} {} : 最终回读为 {effective}（期望 {}），已被后续写入覆盖，未恢复",
                    "✗".red(),
                    param,
                    entry.previous
                );
            }
            restored_params.retain(|landed| landed != *param);
            failures.push(ApplyFailure {
                param: (*param).clone(),
                error: format!(
                    "最终回读为 {effective}（期望 {}），已被后续写入覆盖，未恢复",
                    entry.previous
                ),
            });
        }
    }

    // Judge the refused writes by the same final read. A refused write whose
    // recorded original IS live at the end was achieved by another entry —
    // the twin's restore is the only writer of the un-writable zero above — so
    // it is a restore, not a failure. Counting it as a failure kept rollback
    // at "Partial" with exit 1 for good, retained the ledger and left the boot
    // replay in place on a host whose pair state was already fully restored.
    // An unreadable path proves nothing (write-only tunables have no
    // read-back), so only a readable, matching value counts.
    for (param, error) in refused {
        let entry = &data.entries[param];
        let live = fs::read_to_string(&entry.path).ok();
        let restored_by_twin = live.is_some_and(|live| {
            matches!(
                classify_readback_for_path(&entry.path, &entry.previous, live.trim()),
                ReadbackVerdict::Verified { .. }
            )
        });
        if restored_by_twin {
            if !quiet {
                progress!(
                    "  {} {} → {} (写入被拒绝，孪生的写入已恢复)",
                    "✓".green(),
                    param,
                    entry.previous
                );
            }
            restored_params.push(param.clone());
        } else {
            failures.push(ApplyFailure {
                param: param.clone(),
                error,
            });
        }
    }

    RollbackOutcome {
        restored_params,
        failures,
        missing,
        leftovers: Vec::new(),
        boot_unit_enabled: true,
    }
}

/// Delete one persisted config file, reporting what happened. Returns whether
/// the file is gone: one that survived still re-applies the tuned values on the
/// next boot, so the caller must not report a finished rollback.
fn remove_persisted(path: &str, quiet: bool) -> bool {
    match fs::remove_file(path) {
        Ok(()) => {
            if !quiet {
                progress!("  已清理 {path}");
            }
            true
        }
        // Only a path that is genuinely absent is already clean. A metadata
        // check first would also call a dangling symlink absent — the lookup
        // follows the link — while its directory entry stays behind and
        // systemd-sysctl re-applies the file the moment the target appears.
        // Unlinking does not follow the link, so the entry always goes.
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => true,
        Err(err) => {
            if !quiet {
                progress!("  {} {path} : {err}（未清理）", "✗".red());
            }
            false
        }
    }
}

/// Delete the persisted config `tune` wrote and, only when every file is gone,
/// the rollback ledger at `ledger`. Returns how many removals failed — the
/// persisted files plus the ledger itself, because a rollback that leaves any
/// of them behind is not finished.
///
/// The removal result used to be discarded while the 已清理 line printed
/// unconditionally and the ledger was deleted anyway: on an `/etc` that refuses
/// the delete (immutable file, read-only mount, a directory in the file's
/// place) `rollback` reported success, exited 0, and dropped the only record of
/// the originals — the next boot then re-applied every tuned value from the
/// surviving file with nothing left to roll it back with.
///
/// The ledger's own removal counts the same way, and it is the `ledger` file
/// this function removes last: it is the record a retry restores from and the
/// set `rollback --list` publishes, so a run that could not drop it and still
/// returned 0 would keep reporting a pending rollback of values that are
/// already restored. A ledger that is already gone is not a failure.
///
/// `systemctl` names the program the unit is disabled with — a parameter for
/// the same reason [`persist_from_rollback_at`] takes one: this is the `_at`
/// form, so a fixture lands a service file under a private path, and a
/// hardcoded program would then run the HOST's systemctl against the
/// PRODUCTION unit name — on a host that has ktuner installed, running the
/// test suite would disable the unit that replays the non-sysctl values at
/// boot. The fixture contract is that none of the generated files, nor a
/// program acting on them, is the production one.
fn finalize_rollback_at(
    ledger: &str,
    sysctl_path: &str,
    service_path: &str,
    script_path: &str,
    quiet: bool,
    systemctl: &str,
) -> Vec<String> {
    let mut leftovers: Vec<String> = Vec::new();
    if !remove_persisted(sysctl_path, quiet) {
        leftovers.push(sysctl_path.to_string());
    }
    if Path::new(service_path).exists() {
        systemctl_quiet(systemctl, &["disable", "ktuner-nonsysctl.service"]);
        if !remove_persisted(service_path, quiet) {
            leftovers.push(service_path.to_string());
        }
        systemctl_quiet(systemctl, &["daemon-reload"]);
    }
    if !remove_persisted(script_path, quiet) {
        leftovers.push(script_path.to_string());
    }
    if leftovers.is_empty() {
        match fs::remove_file(ledger) {
            Ok(()) => {}
            // Only a delete that left the ledger behind counts: ENOENT — the
            // ledger already gone — is the state this call exists to reach.
            Err(error) if Path::new(ledger).exists() => {
                if !quiet {
                    progress!("  {} {ledger} : {error}（未清理）", "✗".red());
                }
                leftovers.push(ledger.to_string());
            }
            Err(_) => {}
        }
    }
    leftovers
}

fn rollback_inner(quiet: bool) -> Result<RollbackOutcome> {
    let _guard = lock_ledger_at(ROLLBACK_PATH)?;
    if !Path::new(ROLLBACK_PATH).exists() {
        anyhow::bail!("没有找到 rollback 文件 ({ROLLBACK_PATH})，可能尚未执行过 tune");
    }

    // The existence check, restore and cleanup share the apply transaction lock.
    let json = fs::read_to_string(ROLLBACK_PATH).context("读取 rollback 文件失败")?;
    let data = parse_rollback_ledger_at(ROLLBACK_PATH, &json)?;
    let RollbackOutcome {
        restored_params,
        failures,
        missing,
        mut leftovers,
        ..
    } = restore_entries(&data, quiet);
    let mut failed = failures.len() + leftovers.len();
    let skipped = missing.len();

    // A persisted file that survived cleanup re-applies the tuned values on the
    // next boot, and a ledger that survived it keeps the record a retry
    // restores from, so the restoration is not complete either way. Counting
    // both like a failed or skipped restore is what keeps the ledger for a
    // retry and turns the exit code into 1 (#4535), instead of a silent success
    // that deletes the originals.
    if rollback_should_finalize(failed, skipped) {
        leftovers.extend(finalize_rollback_at(
            ROLLBACK_PATH,
            SYSCTL_PERSIST_PATH,
            NONSYSCTL_SERVICE_PATH,
            NONSYSCTL_SCRIPT_PATH,
            quiet,
            "systemctl",
        ));
        failed = failures.len() + leftovers.len();
    }

    if !quiet {
        if !leftovers.is_empty() {
            progress!(
                "  {} {} 项清理未能完成，已保留 {} 以便重试（未能删除的持久化配置会在下次启动时重新生效）",
                "⚠".yellow(),
                leftovers.len(),
                ROLLBACK_PATH
            );
        } else if failed > 0 || skipped > 0 {
            progress!(
                "  {} {} 项恢复失败、{} 项路径缺失，已保留 {} 以便重试（未删除持久化配置）",
                "⚠".yellow(),
                failed,
                skipped,
                ROLLBACK_PATH
            );
        }
        progress!();
        progress!("  共恢复 {} 项配置。", restored_params.len());
    }
    Ok(RollbackOutcome {
        restored_params,
        failures,
        missing,
        leftovers,
        boot_unit_enabled: true,
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
    progress!("  {}", "性能对比 (before → after)".bold());
    progress!(
        "  {:<24} {:>16} {:>16} {:>8}",
        "指标",
        "调前",
        "调后",
        "变化"
    );
    progress!("  {}", "─".repeat(66));

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
            progress!(
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
        progress!(
            "  {:<24} {}  {} {}",
            b.name,
            before_val,
            after_val,
            change_display
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
            progress!();
            progress!(
                "  {} {} 出现波动，可能是 benchmark 噪声，未自动回滚。",
                "△".yellow(),
                result.degraded[0]
            );
            progress!(
                "    建议重新运行确认，或手动回滚: {}",
                "sudo ktuner rollback".bold()
            );
        }
        return Ok(None);
    }

    progress!();
    progress!(
        "  {} 检测到 {} 项指标恶化超过 {}%，执行自动回滚...",
        "⚠".yellow(),
        result.degraded.len(),
        DEGRADATION_THRESHOLD as u32
    );
    for name in &result.degraded {
        progress!("    - {}", name.red());
    }
    progress!();

    let outcome = rollback()?;
    Ok(Some(outcome))
}

#[cfg(test)]
mod tests {
    #[test]
    fn path_reading_preserves_proc_scalar_brackets() {
        for param in ["kernel.hostname", "kernel/domainname"] {
            let path = param_to_path(param);
            for value in [
                "[production]",
                "node[production]",
                "release[blue",
                "release [blue]",
                "[]",
                "[none]",
            ] {
                assert_eq!(active_value_for_path(&path, &format!("{value}\n")), value);
                assert_eq!(
                    classify_readback_for_path(&path, value, value),
                    ReadbackVerdict::Verified {
                        effective: value.to_string(),
                    }
                );
            }
        }
    }

    #[test]
    fn proc_scalar_clamp_retains_the_complete_readback() {
        let path = param_to_path("kernel.hostname");
        for (requested, actual) in [
            ("production", "[production]"),
            ("[production]", "[staging]"),
            ("node[production]", "node[staging]"),
            ("release[blue", "release[green"),
        ] {
            assert_eq!(
                classify_readback_for_path(&path, requested, actual),
                ReadbackVerdict::Clamped {
                    effective: actual.to_string(),
                }
            );
        }
    }

    #[test]
    fn path_reading_keeps_sysfs_and_unknown_option_lists() {
        for path in [
            "/sys/block/sda/queue/scheduler",
            "/tmp/ktuner-option-fixture",
        ] {
            for readback in ["[none]", "mq-deadline [none] kyber"] {
                assert_eq!(active_value_for_path(path, readback), "none");
                assert_eq!(
                    classify_readback_for_path(path, "none", readback),
                    ReadbackVerdict::Verified {
                        effective: "none".into(),
                    }
                );
            }
        }
        let thp = param_to_path("transparent_hugepage/enabled");
        let readback = "always [madvise] never";
        assert_eq!(active_value_for_path(&thp, readback), "madvise");
        assert_eq!(
            classify_readback_for_path(&thp, "never", readback),
            ReadbackVerdict::Clamped {
                effective: "madvise".into(),
            }
        );
        assert_eq!(active_value("[none]"), "none");
    }

    #[test]
    fn proc_path_reading_keeps_numeric_and_leading_token_contracts() {
        let path = param_to_path("net.ipv4.tcp_rmem");
        let readback = "4096\t131072\t6291456\n";
        assert_eq!(
            active_value_for_path(&path, readback),
            "4096 131072 6291456"
        );
        assert_eq!(
            classify_readback_for_path(&path, "4096 131072 6291456", readback),
            ReadbackVerdict::Verified {
                effective: "4096 131072 6291456".into(),
            }
        );
        assert_eq!(
            classify_readback_for_path(&path, "4096 131072 10000000", readback),
            ReadbackVerdict::Clamped {
                effective: "4096 131072 6291456".into(),
            }
        );
        let congestion = param_to_path("net.ipv4.tcp_congestion_control");
        assert_eq!(
            classify_readback_for_path(&congestion, "bbr", "bbr cubic"),
            ReadbackVerdict::Verified {
                effective: "bbr".into(),
            }
        );
    }

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
                        category: None,
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
                        category: None,
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
                    category: None,
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "0".into(),
                    path: "/proc/sys/vm/dirty_ratio".into(),
                    category: None,
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
                category: None,
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
                    category: None,
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "5".into(),
                    path: "/proc/sys/vm/dirty_ratio".into(),
                    category: None,
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
                    None,
                ),
                sibling_cleared_record("vm.overcommit_kbytes", "8589934592")
                    .map(|(param, previous, applied)| (param, previous, applied, None))
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

    /// Which pairs clear their twin on any accepted write, and which only on a
    /// write that changes the knob. This is the decision `ClearedSibling::cleared_by`
    /// makes before a twin record is published, pinned per pair so a
    /// regression in either direction — recording a twin the kernel never
    /// cleared, or dropping one it did clear — fails here, without a writable
    /// /proc/sys: the integration test for the no-op case needs root and mount
    /// namespaces, this one does not.
    #[test]
    fn twin_clearing_follows_the_handler_of_each_pair() {
        let unchanged = WriteOutcome {
            effective: "0".to_string(),
            clamped: false,
        };
        let changed = WriteOutcome {
            effective: "1073741824".to_string(),
            clamped: false,
        };
        for param in ["vm.dirty_bytes", "vm.dirty_ratio", "vm/dirty_ratio"] {
            // The dirty foreground handlers zero the twin inside their
            // changed-value branch (v6.6 mm/page-writeback.c:518-544).
            assert!(!clears_twin_on_any_write(param), "{param}");
            let twin = ClearedSibling {
                entry: (
                    "vm.dirty_ratio".to_string(),
                    "20".to_string(),
                    "0".to_string(),
                ),
                on_any_write: clears_twin_on_any_write(param),
            };
            assert!(
                !twin.cleared_by(Some("0"), &unchanged),
                "{param}: a write that changes nothing clears nothing"
            );
            assert!(
                twin.cleared_by(Some("0"), &changed),
                "{param}: a changed write clears the twin"
            );
            assert!(
                twin.cleared_by(None, &unchanged),
                "{param}: an unreadable original cannot rule the clear out"
            );
        }
        for param in [
            "vm.dirty_background_bytes",
            "vm.dirty_background_ratio",
            "vm.overcommit_kbytes",
            "vm.overcommit_ratio",
            "vm/overcommit_ratio",
        ] {
            // These handlers clear on every accepted write (`if (ret == 0 &&
            // write)`), so a no-op write still records the twin.
            assert!(clears_twin_on_any_write(param), "{param}");
            let twin = ClearedSibling {
                entry: (
                    "vm.overcommit_kbytes".to_string(),
                    "8589934592".to_string(),
                    "0".to_string(),
                ),
                on_any_write: clears_twin_on_any_write(param),
            };
            assert!(
                twin.cleared_by(Some("0"), &unchanged),
                "{param}: these handlers clear the twin on any write"
            );
        }
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
                    category: None,
                },
            ),
            (
                "vm.overcommit_ratio".to_string(),
                RollbackEntry {
                    previous: "80".into(),
                    applied: "0".into(),
                    path: "/proc/sys/vm/overcommit_ratio".into(),
                    category: None,
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

    /// Both halves of a pair recorded at 0: every handler of a mutually
    /// exclusive pair clears its twin on the write, so the ledger cannot say
    /// which write produced the state and each record looks like the other's
    /// side effect. Without a tie-break both halves are skipped — the pair
    /// vanishes from the boot config, and a ledger holding only the pair
    /// renders no file at all, which removes the one on disk — and the listing
    /// reports each half as cleared by the other. The ratio line is the one
    /// that reproduces the pair: writing it zeroes the bytes half.
    #[test]
    fn render_persistence_keeps_one_line_of_an_all_zero_pair() {
        let entries = BTreeMap::from([
            (
                "vm.dirty_background_bytes".to_string(),
                RollbackEntry {
                    previous: "268435456".into(),
                    applied: "0".into(),
                    path: "/proc/sys/vm/dirty_background_bytes".into(),
                    category: Some(Category::Performance),
                },
            ),
            (
                "vm.dirty_background_ratio".to_string(),
                RollbackEntry {
                    previous: "10".into(),
                    applied: "0".into(),
                    path: "/proc/sys/vm/dirty_background_ratio".into(),
                    category: Some(Category::Performance),
                },
            ),
        ]);
        let bytes = &entries["vm.dirty_background_bytes"];
        assert_eq!(
            cleared_twin_of(&entries, "vm.dirty_background_bytes", bytes),
            Some("vm.dirty_background_ratio"),
            "the bytes half is the side effect, so its line is skipped"
        );
        let ratio = &entries["vm.dirty_background_ratio"];
        assert_eq!(
            cleared_twin_of(&entries, "vm.dirty_background_ratio", ratio),
            None,
            "the ratio half is the writer: the pair must not skip itself"
        );

        let (config, script) = render_persistence(&entries);
        let config = config.expect("one line of the pair is renderable");
        assert!(config.contains("vm.dirty_background_ratio = 0"), "{config}");
        assert!(
            !config.contains("dirty_background_bytes"),
            "the zeroed bytes line would zero the ratio again at boot: {config}"
        );
        assert!(script.is_none());
    }

    /// The twin lookup must answer for the separator spellings every other
    /// write path resolves to the same file: `apply_import` takes the slash
    /// spelling of a sysctl — `validate_import_value` resolves the parameter
    /// "the same way the write path does, so `kernel/sysrq` and other separator
    /// spellings that map to the same file cannot slip past", and 33c0d669a
    /// dotted-normalized the runtime-dangerous guard for exactly this reason.
    /// `cleared_sibling` matched the dotted spelling only, so a slashed
    /// clearer wrote the knob, the kernel zeroed its twin, and the ledger kept
    /// no entry for the twin's original — the loss
    /// [`mutually_exclusive_writes_record_the_twin_they_disable`] pins for the
    /// dotted spelling, with the same consequence: a rollback restores what the
    /// ledger holds, reports a full restore, and leaves the twin zeroed.
    #[test]
    fn cleared_sibling_accepts_the_separator_spellings_import_resolves() {
        for (param, twin) in [
            ("vm/dirty_bytes", "vm.dirty_ratio"),
            ("vm/dirty_ratio", "vm.dirty_bytes"),
            ("vm/dirty_background_bytes", "vm.dirty_background_ratio"),
            ("vm/dirty_background_ratio", "vm.dirty_background_bytes"),
            ("vm/overcommit_kbytes", "vm.overcommit_ratio"),
            ("vm/overcommit_ratio", "vm.overcommit_kbytes"),
        ] {
            assert_eq!(
                cleared_sibling(param),
                Some(twin),
                "the slash spelling {param} resolves to the same file as its dotted form"
            );
        }
        // A knob outside the pairs disables nothing, whichever separator the
        // caller used.
        for param in ["vm.swappiness", "vm/swappiness", "net/ipv4/tcp_syncookies"] {
            assert_eq!(cleared_sibling(param), None, "{param} disables no twin");
        }
    }

    /// The pair can be recorded under the spelling the caller imported the
    /// CLEARER with: a first run writes the slashed `vm/overcommit_kbytes`, a
    /// later one applies the dotted ratio, and `merge_entries` matches the twin
    /// record to the existing entry by resolved path while keeping the first
    /// key — so the zeroed twin sits under the slashed spelling. The predicate
    /// both consumers share must still recognize it: otherwise persistence
    /// writes the `= 0` line and the restore writes the twin BEFORE its
    /// clearer, whose own write zeroes the just-restored value again.
    #[test]
    fn cleared_twin_is_recognized_under_the_importers_spelling() {
        let entries = BTreeMap::from([
            (
                "vm/overcommit_kbytes".to_string(),
                RollbackEntry {
                    previous: "4194304".to_string(),
                    applied: "0".to_string(),
                    path: "/proc/sys/vm/overcommit_kbytes".to_string(),
                    category: None,
                },
            ),
            (
                "vm.overcommit_ratio".to_string(),
                RollbackEntry {
                    previous: "50".to_string(),
                    applied: "100".to_string(),
                    path: "/proc/sys/vm/overcommit_ratio".to_string(),
                    category: None,
                },
            ),
        ]);
        assert!(
            is_cleared_twin(
                &entries,
                "vm/overcommit_kbytes",
                &entries["vm/overcommit_kbytes"]
            ),
            "the zeroed twin recorded under the imported slash key must count"
        );
        assert!(
            !is_cleared_twin(
                &entries,
                "vm.overcommit_ratio",
                &entries["vm.overcommit_ratio"]
            ),
            "the clearer is not a side-effect record"
        );
        let (config, _) = render_persistence(&entries);
        let config = config.unwrap();
        assert!(
            !config.contains("vm/overcommit_kbytes"),
            "the zero line of a cleared twin must not be persisted:\n{config}"
        );
        assert!(config.contains("vm.overcommit_ratio = 100"), "{config}");
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
                    category: None,
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "0".into(),
                    path: ratio.to_str().unwrap().into(),
                    category: None,
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
        assert_eq!(outcome.restored(), 2);
        assert_eq!(fs::read_to_string(&bytes).unwrap(), "0");
        assert_eq!(
            fs::read_to_string(&ratio).unwrap(),
            "20",
            "the cleared ratio must come back with the rollback"
        );
        assert!(rollback_should_finalize(
            outcome.failed(),
            outcome.skipped()
        ));
    }

    use super::*;

    /// An outcome's failure list of length `n`. The classifier, the
    /// completeness gate and the JSON body read the lists' lengths and (for
    /// the body) their names, so a fixture only has to be well formed.
    fn test_failures(n: usize) -> Vec<ApplyFailure> {
        (0..n)
            .map(|i| ApplyFailure {
                param: format!("vm.ktuner_failed_{i}"),
                error: "write refused".to_string(),
            })
            .collect()
    }

    /// An outcome's missing list of length `n`, the path-absent entries.
    /// An outcome's restored list of length `n`: the ledger keys a batch
    /// landed, in the order it landed them.
    fn test_restored(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("vm.ktuner_restored_{i}")).collect()
    }

    fn test_missing(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("vm.ktuner_missing_{i}")).collect()
    }

    #[test]
    fn render_persistence_normalizes_slashed_sysctl_names() {
        let mut entries = std::collections::BTreeMap::new();
        entries.insert(
            "kernel/sysrq".to_string(),
            RollbackEntry {
                previous: "1".to_string(),
                applied: "176".to_string(),
                path: "/proc/sys/kernel/sysrq".to_string(),
                category: None,
            },
        );
        entries.insert(
            "block/sda/scheduler".to_string(),
            RollbackEntry {
                previous: "none".to_string(),
                applied: "mq-deadline".to_string(),
                path: "/sys/block/sda/queue/scheduler".to_string(),
                category: None,
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

    /// The generated script is the ExecStart of `ktuner-nonsysctl.service`, a
    /// `Type=oneshot` unit: a command that exits non-zero is the unit's own
    /// failure, which is exactly what puts a host into the `degraded` state
    /// systemctl reports. The `[ -f ]` guard is what makes "the node is not
    /// there" a deliberate skip — a device renamed between the apply and the
    /// boot (sda -> sdb), a disk that is gone, or a node that appears only
    /// later all leave `/sys/block/<dev>/…` absent — so a skip must not end up
    /// as the script's exit status: `&&` carries the failed test into it, and
    /// the unit then fails on every boot for a node the renderer chose to skip.
    #[test]
    fn nonsysctl_script_skips_an_absent_node_without_failing() {
        let dir = AtomicTestDir::new("nonsysctl-absent");
        let node = dir.0.join("queue").join("read_ahead_kb");
        let entries = BTreeMap::from([(
            "block/sda/read_ahead_kb".to_string(),
            RollbackEntry {
                previous: "128".to_string(),
                applied: "2048".to_string(),
                path: node.to_str().unwrap().to_string(),
                category: None,
            },
        )]);
        let script = render_persistence(&entries)
            .1
            .expect("a block entry renders the boot script");
        let path = dir.0.join("apply-nonsysctl.sh");
        fs::write(&path, &script).unwrap();

        // Run it the way the unit does; `sh` is enough for a POSIX test and
        // echo, and the node stays absent for the whole run.
        let status = std::process::Command::new("sh")
            .arg(&path)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "an absent node is the skip the guard exists for, not a boot failure: {script}"
        );
    }

    /// Control for the test above: a node that IS there still receives the
    /// recorded value, so the skip cannot degenerate into "never write".
    #[test]
    fn nonsysctl_script_applies_a_present_node() {
        let dir = AtomicTestDir::new("nonsysctl-present");
        let queue = dir.0.join("queue");
        fs::create_dir(&queue).unwrap();
        let node = queue.join("read_ahead_kb");
        fs::write(&node, "128\n").unwrap();
        let entries = BTreeMap::from([(
            "block/sda/read_ahead_kb".to_string(),
            RollbackEntry {
                previous: "128".to_string(),
                applied: "2048".to_string(),
                path: node.to_str().unwrap().to_string(),
                category: None,
            },
        )]);
        let script = render_persistence(&entries)
            .1
            .expect("a block entry renders the boot script");
        let path = dir.0.join("apply-nonsysctl.sh");
        fs::write(&path, &script).unwrap();

        let status = std::process::Command::new("sh")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success(), "a present node must apply: {script}");
        assert_eq!(
            fs::read_to_string(&node).unwrap().trim(),
            "2048",
            "the recorded value must reach the node"
        );
    }

    /// Control for the skip tests: a write that really fails must still fail
    /// the unit, even when it is not the last line. The unit's status is the
    /// script's exit status — the last command's, unless the script accumulates
    /// — so a failed write on an earlier line let the ones after it (here: a
    /// deliberate skip) report success for a value that never landed, and a
    /// host silently booted with part of its plan missing.
    #[test]
    fn nonsysctl_script_reports_a_failed_write_behind_a_skipped_one() {
        let dir = AtomicTestDir::new("nonsysctl-failed-write");
        let absent = dir.0.join("gone").join("read_ahead_kb");
        let entries = BTreeMap::from([
            (
                // Every uid gets EINVAL from this node for a value it cannot
                // parse, so the failing write is reproducible as root too —
                // unlike a permission failure, which root bypasses.
                "block/sda/read_ahead_kb".to_string(),
                RollbackEntry {
                    previous: "128".to_string(),
                    applied: "not-a-number".to_string(),
                    path: "/proc/self/oom_score_adj".to_string(),
                    category: None,
                },
            ),
            (
                // Sorts after the failing entry, so the failing write is the
                // FIRST line: without the accumulator its status is discarded
                // and this skip's success becomes the unit's verdict.
                "block/sdb/read_ahead_kb".to_string(),
                RollbackEntry {
                    previous: "128".to_string(),
                    applied: "2048".to_string(),
                    path: absent.to_str().unwrap().to_string(),
                    category: None,
                },
            ),
        ]);
        let script = render_persistence(&entries)
            .1
            .expect("a block entry renders the boot script");
        let path = dir.0.join("apply-nonsysctl.sh");
        fs::write(&path, &script).unwrap();

        let status = std::process::Command::new("sh")
            .arg(&path)
            .status()
            .unwrap();
        assert!(
            !status.success(),
            "a write that fails is the unit's failure, not the last line's skip: {script}"
        );
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
                    category: None,
                },
            ),
            (
                "kernel.core_pattern".to_string(),
                RollbackEntry {
                    previous: "core".to_string(),
                    applied: "|/tmp/evil".to_string(),
                    path: "/proc/sys/kernel/core_pattern".to_string(),
                    category: None,
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".to_string(),
                    applied: "10".to_string(),
                    // Innocent name, deny-listed recorded path.
                    path: "/proc/sys/kernel/modprobe".to_string(),
                    category: None,
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

    /// The boot path refuses the names the write path refuses, for the same
    /// reason it refuses the deny-listed ones: the `block/` branch renders the
    /// RECORDED path into the generated script, which the init system runs as
    /// root at every boot, so a name that walks out of /sys must be dropped
    /// before the file is rendered — not only before the entry is restored.
    #[test]
    fn persistence_drops_entries_the_write_path_refuses() {
        let victim = std::env::temp_dir().join(format!(
            "ktuner-persist-unsafe-victim-{}",
            std::process::id()
        ));
        let param = format!("block/../../../../..{}", victim.display());
        assert!(
            !is_safe_param(&param),
            "the write choke point refuses this name"
        );
        let entries = BTreeMap::from([
            (
                "vm.swappiness".to_string(),
                RollbackEntry {
                    previous: "60".to_string(),
                    applied: "1".to_string(),
                    path: "/proc/sys/vm/swappiness".to_string(),
                    category: None,
                },
            ),
            (
                param.clone(),
                RollbackEntry {
                    previous: "x".to_string(),
                    applied: "pwned".to_string(),
                    path: param_to_path(&param),
                    category: None,
                },
            ),
        ]);

        let kept = persistable_entries(&entries);
        assert!(kept.contains_key("vm.swappiness"), "ordinary params stay");
        assert!(
            !kept.contains_key(&param),
            "an unsafe name must not reach the boot path: {:?}",
            kept.keys().collect::<Vec<_>>()
        );
        let (_, script) = render_persistence(&kept);
        let script = script.unwrap_or_default();
        assert!(
            !script.contains("victim"),
            "the generated script must not carry the traversal: {script}"
        );
    }

    /// Dropping a deny-listed entry from the *render* is not enough: the file
    /// the previous generation wrote is still on disk, and systemd-sysctl
    /// applies it as root at the next boot — the exact write the deny-list
    /// exists to keep off the boot path. The regenerate reads the whole ledger
    /// every time, so a file it can no longer render must be retired, or a
    /// denied line survives every later run that has something else to persist.
    #[test]
    fn persist_retires_a_file_the_ledger_can_no_longer_render() {
        let dir = AtomicTestDir::new("persist-retire");
        let ledger = dir.0.join("rollback.json");
        let sysctl = dir.0.join("99-ktuner.conf");
        let script = dir.0.join("apply-nonsysctl.sh");
        let service = dir.0.join("ktuner-nonsysctl.service");
        let path = ledger.to_str().unwrap();

        // A file an earlier generation left for a ledger whose only
        // sysctl-class entry is a deny-listed one (the ledger an edited file
        // or one from before the persist-side filter can hold).
        fs::write(
            &sysctl,
            "# Generated by ktuner - do not edit manually\nkernel.core_pattern = |/tmp/evil\n",
        )
        .unwrap();
        merge_rollback_at(
            path,
            [(
                "kernel.core_pattern".to_string(),
                "core".to_string(),
                "|/tmp/evil".to_string(),
                None,
            )],
        )
        .unwrap();
        let guard = lock_ledger_at(path).unwrap();
        persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            script.to_str().unwrap(),
            service.to_str().unwrap(),
            "true",
        )
        .unwrap();
        assert!(
            !sysctl.exists(),
            "a file with nothing left to render must be retired, not left for the next boot"
        );

        // Control: with a persistable entry in the ledger the file is
        // regenerated (without the denied line) — the retire arm must not
        // fire while there is still something to persist. The merge takes
        // the same exclusive lock, so the transaction guard goes first.
        drop(guard);
        merge_rollback_at(
            path,
            [(
                "vm.swappiness".to_string(),
                "60".to_string(),
                "10".to_string(),
                None,
            )],
        )
        .unwrap();
        let guard = lock_ledger_at(path).unwrap();
        persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            script.to_str().unwrap(),
            service.to_str().unwrap(),
            "true",
        )
        .unwrap();
        let rendered = fs::read_to_string(&sysctl).unwrap();
        assert!(rendered.contains("vm.swappiness = 10"), "{rendered}");
        assert!(!rendered.contains("core_pattern"), "{rendered}");
    }

    /// The boot script and its unit are rendered from the same filtered
    /// entries as the sysctl file, and an entry whose recorded *path* the
    /// deny-list refuses is dropped however the parameter is named. The pair
    /// an earlier generation wrote from such an entry must be retired with it,
    /// or the denied write still runs as root at the next boot.
    #[test]
    fn persist_retires_the_script_and_unit_of_an_unrenderable_ledger() {
        let dir = AtomicTestDir::new("persist-retire-script");
        let ledger = dir.0.join("rollback.json");
        let sysctl = dir.0.join("99-ktuner.conf");
        let script = dir.0.join("apply-nonsysctl.sh");
        let service = dir.0.join("ktuner-nonsysctl.service");
        let path = ledger.to_str().unwrap();

        // A non-sysctl parameter whose recorded path is deny-listed — the
        // shape an edited or pre-filter ledger has. The filter drops the entry
        // by path, so neither the sysctl file nor the script renders.
        let ledger_json = serde_json::json!({
            "version": 1,
            "entries": {
                "block/sda/read_ahead_kb": {
                    "previous": "128",
                    "applied": "2048",
                    "path": "/proc/sys/kernel/core_pattern",
                }
            }
        });
        fs::write(&ledger, serde_json::to_string(&ledger_json).unwrap()).unwrap();
        fs::write(
            &script,
            "#!/bin/bash\necho '|/tmp/evil' > /proc/sys/kernel/core_pattern\n",
        )
        .unwrap();
        fs::write(
            &service,
            "[Unit]\nDescription=Apply ktuner non-sysctl kernel parameters\n",
        )
        .unwrap();

        let guard = lock_ledger_at(path).unwrap();
        persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            script.to_str().unwrap(),
            service.to_str().unwrap(),
            "true",
        )
        .unwrap();

        assert!(
            !script.exists() && !service.exists(),
            "the script and its unit must be retired with the render that owns them"
        );
    }

    /// Write a stand-in `systemctl` that appends its arguments to `calls`, and
    /// return its path once it can be executed.
    ///
    /// The production code spawns the program itself and folds a spawn error
    /// into "did not run", so a stand-in that fails to exec only surfaces as a
    /// missing `calls` file. A sibling test thread that forks while this file
    /// is still open for writing hands the child that write handle until its
    /// own exec, and an exec of the stand-in in that window fails with
    /// `ETXTBSY`. Warm the stand-in up with `--probe`, which it answers
    /// without recording anything, until it no longer fails that way — the
    /// same bounded retry `spawn_fresh_executable` in the detect tests uses.
    fn executable_systemctl_stand_in(
        dir: &std::path::Path,
        calls: &std::path::Path,
    ) -> std::path::PathBuf {
        let program = dir.join("systemctl");
        fs::write(
            &program,
            format!(
                "#!/bin/sh\n[ \"$1\" = --probe ] && exit 0\nprintf '%s\\n' \"$*\" >> '{}'\n",
                calls.display()
            ),
        )
        .expect("write the stand-in program");
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755))
            .expect("make the stand-in executable");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match std::process::Command::new(&program).arg("--probe").status() {
                Ok(status) => {
                    assert!(status.success(), "the stand-in answers --probe");
                    return program;
                }
                Err(err)
                    if err.raw_os_error() == Some(libc::ETXTBSY)
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(err) => panic!("run the stand-in: {err}"),
            }
        }
    }

    /// A retire is the mirror of the write arm, so it must undo both artifacts
    /// the write created. Deleting the unit file alone left systemd's
    /// enablement pointing at a file that no longer exists: the unit was
    /// started (and failed) at every boot, `systemctl is-enabled` kept
    /// answering "enabled" for it, and the terminal cleanup cannot repair it —
    /// `finalize_rollback_at` only disables while the unit file is still
    /// there, and after a retire it never is.
    #[test]
    fn persist_retire_disables_the_unit_it_removes() {
        let dir = AtomicTestDir::new("persist-retire-disable");
        let ledger = dir.0.join("rollback.json");
        let sysctl = dir.0.join("99-ktuner.conf");
        let script = dir.0.join("apply-nonsysctl.sh");
        let service = dir.0.join("ktuner-nonsysctl.service");
        let path = ledger.to_str().unwrap();

        // The unrenderable ledger the retire arm owns: the pair an earlier
        // generation wrote for a deny-listed recorded path.
        let ledger_json = serde_json::json!({
            "version": 1,
            "entries": {
                "block/sda/read_ahead_kb": {
                    "previous": "128",
                    "applied": "2048",
                    "path": "/proc/sys/kernel/core_pattern",
                }
            }
        });
        fs::write(&ledger, serde_json::to_string(&ledger_json).unwrap()).unwrap();
        fs::write(&script, "#!/bin/sh\n").unwrap();
        fs::write(&service, "[Unit]\n").unwrap();

        // The stand-in records its arguments: a run that reaches the host's
        // systemctl instead writes nothing here.
        let calls = dir.0.join("systemctl-calls");
        let program = executable_systemctl_stand_in(&dir.0, &calls);

        let guard = lock_ledger_at(path).unwrap();
        persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            script.to_str().unwrap(),
            service.to_str().unwrap(),
            program.to_str().unwrap(),
        )
        .unwrap();
        drop(guard);

        assert!(
            !script.exists() && !service.exists(),
            "the retire still removes both files"
        );
        assert_eq!(
            fs::read_to_string(&calls).expect("the named program must be the one that ran"),
            "disable ktuner-nonsysctl.service\ndaemon-reload\n",
            "retiring the unit must undo the enablement the write arm created"
        );
    }

    /// The pair is retired best-effort across BOTH files: the unit is the boot
    /// path that runs the script, and systemd fails a unit whose ExecStart
    /// cannot be executed, so a script that refuses to go must not stop the
    /// unit from being retired with it. `||` short-circuits, so the second
    /// removal only ran while the first succeeded, and the unit was left for
    /// the next boot — a failed oneshot that no later command clears, because
    /// an incomplete rollback never reaches `finalize_rollback_at`.
    #[test]
    fn persist_retires_the_unit_even_when_the_script_survives() {
        let dir = AtomicTestDir::new("persist-retire-both");
        let ledger = dir.0.join("rollback.json");
        let sysctl = dir.0.join("99-ktuner.conf");
        let script = dir.0.join("apply-nonsysctl.sh");
        let service = dir.0.join("ktuner-nonsysctl.service");
        let path = ledger.to_str().unwrap();

        // The same unrenderable ledger as above, so the pair is retired...
        let ledger_json = serde_json::json!({
            "version": 1,
            "entries": {
                "block/sda/read_ahead_kb": {
                    "previous": "128",
                    "applied": "2048",
                    "path": "/proc/sys/kernel/core_pattern",
                }
            }
        });
        fs::write(&ledger, serde_json::to_string(&ledger_json).unwrap()).unwrap();
        // ...with the script path occupied by something `remove_file` refuses.
        // A directory is the portable shape: no permission bits are involved,
        // so the refusal is the same for root and for an unprivileged runner.
        fs::create_dir(&script).unwrap();
        fs::write(
            &service,
            "[Unit]\nDescription=Apply ktuner non-sysctl kernel parameters\n",
        )
        .unwrap();

        let guard = lock_ledger_at(path).unwrap();
        let result = persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            script.to_str().unwrap(),
            service.to_str().unwrap(),
            "true",
        );

        assert!(
            result.is_err(),
            "a file that could not be removed is reported"
        );
        assert!(
            !service.exists(),
            "the unit must still be retired, not left to fail the next boot"
        );
    }

    /// Retirement is an unlink, not a metadata lookup: a dangling symlink at
    /// the generated path also fails a lookup, and leaving its directory entry
    /// behind means systemd-sysctl applies the file once the target appears.
    #[test]
    fn persist_retires_a_dangling_link_at_the_generated_path() {
        let dir = AtomicTestDir::new("persist-retire-link");
        let ledger = dir.0.join("rollback.json");
        let sysctl = dir.0.join("99-ktuner.conf");
        let path = ledger.to_str().unwrap();

        fs::write(&sysctl, "# leftovers from a previous generation\n").unwrap();
        let target = dir.0.join("missing-target.conf");
        fs::remove_file(&sysctl).unwrap();
        std::os::unix::fs::symlink(&target, &sysctl).unwrap();

        // Nothing to render, so the generated path is retired.
        let guard = lock_ledger_at(path).unwrap();
        persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            dir.0.join("script.sh").to_str().unwrap(),
            dir.0.join("unit.service").to_str().unwrap(),
            "true",
        )
        .unwrap();

        assert!(
            fs::symlink_metadata(&sysctl).is_err(),
            "the directory entry must be gone, not just resolved away"
        );
    }

    /// The boot script only runs if its unit is enabled, so a refused or
    /// impossible `systemctl enable` (no systemd on the host) must not be
    /// reported as a persisted change.
    #[test]
    fn persist_reports_a_boot_unit_that_was_not_enabled() {
        let dir = AtomicTestDir::new("persist-enable");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let ledger_json = serde_json::json!({
            "version": 1,
            "entries": {
                "block/sda/read_ahead_kb": {
                    "previous": "128",
                    "applied": "2048",
                    "path": "/sys/block/sda/queue/read_ahead_kb",
                }
            }
        });
        fs::write(&ledger, serde_json::to_string(&ledger_json).unwrap()).unwrap();
        let script = dir.0.join("apply-nonsysctl.sh");
        let service = dir.0.join("ktuner-nonsysctl.service");
        let guard = lock_ledger_at(path).unwrap();
        let persist = |systemctl: &str| {
            persist_from_rollback_at(
                &guard,
                dir.0.join("99-ktuner.conf").to_str().unwrap(),
                script.to_str().unwrap(),
                service.to_str().unwrap(),
                systemctl,
            )
            .unwrap()
        };

        assert!(persist("true"), "an enabled unit replays at boot");
        assert!(
            !persist("false"),
            "a refused enable leaves nothing to replay"
        );
        assert!(
            !persist(dir.0.join("no-systemctl").to_str().unwrap()),
            "a host without systemctl has no unit to run"
        );
        assert!(service.exists(), "the unit itself is still written");
    }

    /// A partial rollback regenerates the boot replay for the entries it keeps
    /// and re-runs `systemctl enable`; a refused enable must reach the outcome
    /// instead of being dropped, because the block/THP values still recorded
    /// will not be replayed after a reboot.
    #[test]
    fn partial_rollback_reports_a_boot_unit_that_was_not_enabled() {
        for (systemctl, armed) in [("true", true), ("false", false)] {
            let dir = AtomicTestDir::new(&format!("rollback-boot-unit-{systemctl}"));
            let (conf, service, script) = fixture_paths(&dir);
            let swappiness = dir.0.join("swappiness");
            let read_ahead = dir.0.join("read_ahead_kb");
            fs::write(&swappiness, "10").unwrap();
            fs::write(&read_ahead, "2048").unwrap();
            let ledger = fixture_ledger(
                &dir,
                &[
                    ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                    (
                        "block/sda/read_ahead_kb",
                        "128",
                        "2048",
                        read_ahead.to_str().unwrap(),
                    ),
                ],
            );
            let guard = lock_ledger_at(&ledger).unwrap();
            let mut data: RollbackData =
                serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
            let keys = resolve_ledger_keys(&data.entries, &["vm.swappiness".to_string()]).unwrap();
            let units = rollback_units(&data.entries, &keys);

            let outcome = restore_units_with(
                &guard, &mut data, &units, &ledger, &conf, &service, &script, systemctl,
            )
            .unwrap();

            assert_eq!(outcome.restored(), 1);
            assert!(outcome.is_complete());
            assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
            assert!(
                Path::new(&service).exists(),
                "the kept block entry regenerates its unit"
            );
            assert_eq!(
                outcome.boot_unit_enabled, armed,
                "systemctl {systemctl}: the boot replay verdict must reach the outcome"
            );
        }
    }

    /// /etc/sysctl.d is owned by the package that replays it at boot, so its
    /// absence is the host telling us nothing would read the file.
    #[test]
    fn persist_names_a_missing_sysctl_directory() {
        let dir = AtomicTestDir::new("persist-no-sysctl-d");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        merge_rollback_at(
            path,
            [(
                "vm.swappiness".to_string(),
                "60".to_string(),
                "10".to_string(),
                None,
            )],
        )
        .unwrap();
        let guard = lock_ledger_at(path).unwrap();
        let missing = dir.0.join("sysctl.d");
        let error = persist_from_rollback_at(
            &guard,
            missing.join("99-ktuner.conf").to_str().unwrap(),
            dir.0.join("apply-nonsysctl.sh").to_str().unwrap(),
            dir.0.join("ktuner-nonsysctl.service").to_str().unwrap(),
            "true",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("未持久化") && error.contains(missing.to_str().unwrap()),
            "{error}"
        );
    }

    /// A host with no systemd unit directory cannot arm the non-sysctl replay,
    /// and that is a verdict, not a failure: the live writes and the
    /// regenerated sysctl file have already landed by the time the unit is
    /// written, so an error here returns before `save_ledger_at` and leaves the
    /// ledger on disk still listing the entries the run just restored.
    #[test]
    fn persist_reports_an_unarmed_unit_without_a_systemd_directory() {
        let dir = AtomicTestDir::new("persist-no-systemd");
        let ledger = dir.0.join("rollback.json");
        let sysctl = dir.0.join("99-ktuner.conf");
        let script = dir.0.join("apply-nonsysctl.sh");
        // No systemd: the unit file's parent directory does not exist.
        let service = dir.0.join("systemd").join("ktuner-nonsysctl.service");
        let path = ledger.to_str().unwrap();
        merge_rollback_at(
            path,
            [(
                "block/sda/scheduler".to_string(),
                "mq-deadline".to_string(),
                "none".to_string(),
                None,
            )],
        )
        .unwrap();
        let guard = lock_ledger_at(path).unwrap();
        let enabled = persist_from_rollback_at(
            &guard,
            sysctl.to_str().unwrap(),
            script.to_str().unwrap(),
            service.to_str().unwrap(),
            "true",
        )
        .expect("an unarmed replay is a verdict, not an error");
        assert!(
            !enabled,
            "the unit cannot be written, so the replay is not armed"
        );
        assert!(
            script.exists(),
            "the script stays: it is what a later armed run replays"
        );
        assert!(!service.exists(), "no unit file was written");
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
                        category: None,
                    },
                ),
                (
                    "block/sda/scheduler".to_string(),
                    RollbackEntry {
                        previous: "mq-deadline".to_string(),
                        applied: "none".to_string(),
                        path: "/sys/block/sda/queue/scheduler".to_string(),
                        category: None,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let entries = triples(&parse_rollback_ledger_at(ROLLBACK_PATH, &json).unwrap());
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

    /// (param, applied, previous) view of a parsed ledger, for the parse
    /// tests whose subject is the healed entry set rather than the live read.
    fn triples(data: &RollbackData) -> Vec<(String, String, String)> {
        data.entries
            .iter()
            .map(|(param, entry)| (param.clone(), entry.applied.clone(), entry.previous.clone()))
            .collect()
    }

    #[test]
    fn test_parse_rollback_entries_empty_ledger() {
        // Fresh install / post-rollback state: empty, not an error.
        let data =
            parse_rollback_ledger_at(ROLLBACK_PATH, r#"{"version":1,"entries":{}}"#).unwrap();
        assert!(data.entries.is_empty());
    }

    #[test]
    fn preview_heals_a_legacy_duplicate_spelling_ledger() {
        // The same legacy ledger restore_entries heals: tune recorded
        // vm.swappiness 60->10, a later import recorded vm/swappiness 10->5.
        // The preview must describe the healed restore — one pending entry
        // returning the pristine 60 — not promise a second restore (10) that
        // rollback never performs.
        let entries = triples(
            &parse_rollback_ledger_at(
                ROLLBACK_PATH,
                r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"10","applied":"5","path":"/proc/sys/vm/swappiness"}
            }}"#,
            )
            .unwrap(),
        );
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
        let entries = triples(
            &parse_rollback_ledger_at(
                ROLLBACK_PATH,
                r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"60","applied":"10","path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"20","applied":"30","path":"/proc/sys/vm/swappiness"}
            }}"#,
            )
            .unwrap(),
        );
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
        // The #3578 "corrupt is not empty" contract, with the remedy the
        // operator needs to keep the originals: the error names the file and
        // the recovery, exactly as [`load_rollback_from`] answers.
        let err = match parse_rollback_ledger_at(ROLLBACK_PATH, "not json") {
            Ok(_) => panic!("a corrupt ledger must not parse"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(message.contains("inspect and repair"), "got: {message}");
        assert!(message.contains(ROLLBACK_PATH), "got: {message}");
    }

    #[test]
    fn test_parse_rollback_entries_rejects_wrong_shape() {
        // Wrong top-level type and wrong entries type: Err, no panic, no
        // silent default.
        assert!(parse_rollback_ledger_at(ROLLBACK_PATH, "[1,2,3]").is_err());
        assert!(parse_rollback_ledger_at(ROLLBACK_PATH, r#"{"entries":"x"}"#).is_err());
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
                            category: None,
                        },
                    )
                })
                .collect(),
        };
        let json = serde_json::to_string(&data).unwrap();
        let params: Vec<String> = parse_rollback_ledger_at(ROLLBACK_PATH, &json)
            .unwrap()
            .entries
            .keys()
            .cloned()
            .collect();
        let params: Vec<&str> = params.iter().map(String::as_str).collect();
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
        assert_eq!(outcome.applied(), 0);
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
                None,
            )],
        );
        let data = merge_entries(
            data,
            [(
                "net/ipv4/conf/eth0.100/forwarding".to_string(),
                "1".to_string(),
                "1".to_string(),
                None,
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
                None,
            )],
        );
        let data = merge_entries(
            data,
            [(
                "net/ipv4/neigh/eth0.100/gc_thresh3".to_string(),
                "4096".to_string(),
                "4096".to_string(),
                None,
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
    fn write_and_verify_rejects_an_unsafe_param() {
        // The choke point every live write routes through must enforce the
        // structural guard, not only the callers that remember it: `apply`,
        // `apply_one` and `apply_quiet` take a caller-built Recommendation, and
        // the conf/neigh branch of `param_to_path` keeps the interface segment
        // verbatim by design, so a `..` segment there survives into the path
        // `fs::write` gets. `apply_import` is the only caller that checks — this
        // one has to hold on its own.
        let dir = std::env::temp_dir().join(format!("ktuner-write-unsafe-{}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        fs::write(&victim, "original").unwrap();
        // /proc/sys/net/ipv4/conf/../../../../../<dir>/victim resolves outside
        // /proc/sys entirely (five components back from conf reach the root).
        let param = format!("net/ipv4/conf/../../../../..{}", victim.display());
        assert!(
            !is_safe_param(&param),
            "the name validator already rejects this spelling"
        );
        let error = write_and_verify(&param, "pwned")
            .expect_err("an unsafe name must not reach the filesystem");
        assert!(
            error.to_string().contains("拒绝写入非法参数名"),
            "the refusal must name the reason: {error}"
        );
        assert_eq!(
            fs::read_to_string(&victim).unwrap(),
            "original",
            "the write must not have escaped /proc/sys"
        );
        // The refusal is the name guard's, not a blanket one: a legitimate
        // spelling still reaches path resolution. The knob is deliberately
        // nonexistent so the check holds unprivileged and never touches a live
        // sysctl — a real `vm.swappiness` write needs root and would mutate
        // the host running the tests.
        let err = write_and_verify("vm.ktuner_no_such_param", "1").unwrap_err();
        assert!(
            !err.to_string().contains("拒绝写入非法参数名"),
            "an ordinary name must not hit the name guard: {err}"
        );

        fs::remove_dir_all(&dir).ok();
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
                restored_params: test_restored(3),
                failures: test_failures(0),
                missing: test_missing(0),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
            }),
            RollbackStatus::Full
        );
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored_params: test_restored(2),
                failures: test_failures(1),
                missing: test_missing(0),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
            }),
            RollbackStatus::Partial
        );
        // A skipped (path-absent) param means the restore was NOT total, so it
        // must be Partial — not Full — even with zero write failures. This is the
        // cosmetic contradiction the skipped field fixes.
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored_params: test_restored(2),
                failures: test_failures(0),
                missing: test_missing(1),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
            }),
            RollbackStatus::Partial
        );
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored_params: test_restored(0),
                failures: test_failures(2),
                missing: test_missing(0),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
            }),
            RollbackStatus::Nothing
        );
        // 0 restored must NEVER be reported as a full restore, even with 0
        // failures — this is the exact false-success the fix removes.
        assert_eq!(
            classify_rollback(&RollbackOutcome {
                restored_params: test_restored(0),
                failures: test_failures(0),
                missing: test_missing(0),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
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
                    restored_params: test_restored(restored),
                    failures: test_failures(failed),
                    missing: test_missing(skipped),
                    leftovers: Vec::new(),
                    boot_unit_enabled: true,
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

    /// A persisted file that survives the cleanup re-applies the tuned values on
    /// the next boot, so the rollback is not complete: the failure has to be
    /// counted — which keeps the ledger for a retry and turns the exit code into
    /// 1 — instead of printing 已清理 and deleting the ledger that still holds
    /// the originals.
    ///
    /// A directory standing in for the persisted file makes `remove_file` fail
    /// with `EISDIR` on every filesystem, so this needs neither root nor a
    /// read-only `/etc`.
    #[test]
    fn test_cleanup_failure_keeps_the_ledger_and_reports_incomplete() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_cleanup_failure_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        let ledger = dir.join("rollback.json");
        fs::write(&ledger, b"{}").expect("write ledger");
        let sysctl = dir.join("99-ktuner.conf");
        fs::create_dir_all(&sysctl).expect("create a directory where the file belongs");
        let service = dir.join("ktuner-nonsysctl.service");
        let script = dir.join("apply-nonsysctl.sh");
        fs::write(&script, b"#!/bin/sh\n").expect("write script");

        let cleanup_leftovers = finalize_rollback_at(
            ledger.to_str().unwrap(),
            sysctl.to_str().unwrap(),
            service.to_str().unwrap(),
            script.to_str().unwrap(),
            true,
            "true",
        );

        assert_eq!(
            cleanup_leftovers,
            vec![sysctl.to_str().unwrap().to_string()],
            "exactly the file that could not be removed is named"
        );
        assert!(
            ledger.exists(),
            "the ledger keeps the originals the surviving file will re-apply, \
             so a failed cleanup must not delete it"
        );
        assert!(!script.exists(), "the removable files are still cleaned up");

        // What those names mean for the caller: a cleanup failure is an
        // incomplete restoration, like a failed or skipped restore, so the
        // ledger survives and `rollback` exits 1 rather than 0.
        let outcome = RollbackOutcome {
            restored_params: test_restored(3),
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: cleanup_leftovers,
            boot_unit_enabled: true,
        };
        assert!(!outcome.is_complete());
        assert_eq!(classify_rollback(&outcome), RollbackStatus::Partial);

        fs::remove_dir_all(&dir).ok();
    }

    /// The success path is unchanged: every persisted file goes first, and only
    /// then the ledger.
    #[test]
    fn test_cleanup_deletes_every_file_and_then_the_ledger() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_cleanup_complete_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        let ledger = dir.join("rollback.json");
        fs::write(&ledger, b"{}").expect("write ledger");
        let sysctl = dir.join("99-ktuner.conf");
        fs::write(&sysctl, b"vm.swappiness = 10\n").expect("write sysctl file");
        let service = dir.join("ktuner-nonsysctl.service");
        fs::write(&service, b"[Unit]\n").expect("write unit file");
        let script = dir.join("apply-nonsysctl.sh");
        fs::write(&script, b"#!/bin/sh\n").expect("write script");

        let cleanup_leftovers = finalize_rollback_at(
            ledger.to_str().unwrap(),
            sysctl.to_str().unwrap(),
            service.to_str().unwrap(),
            script.to_str().unwrap(),
            true,
            "true",
        );

        assert!(cleanup_leftovers.is_empty());
        assert!(!sysctl.exists() && !service.exists() && !script.exists());
        assert!(
            !ledger.exists(),
            "a complete cleanup drops the ledger with the persisted config"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The `_at` form exists so a fixture touches neither the generated files
    /// nor a program acting on them: the cleanup must run the program its
    /// caller names. Before the program became a parameter, a fixture whose
    /// service file existed reached the HOST's systemctl under the PRODUCTION
    /// unit name — on a machine that has ktuner installed, one `cargo test`
    /// run disabled the unit that replays the non-sysctl values at the next
    /// boot.
    #[test]
    fn test_cleanup_runs_the_program_the_caller_names() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_cleanup_injection_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        let ledger = dir.join("rollback.json");
        fs::write(&ledger, b"{}").expect("write ledger");
        let sysctl = dir.join("99-ktuner.conf");
        fs::write(&sysctl, b"vm.swappiness = 10\n").expect("write sysctl file");
        let service = dir.join("ktuner-nonsysctl.service");
        fs::write(&service, b"[Unit]\n").expect("write unit file");
        let script = dir.join("apply-nonsysctl.sh");
        fs::write(&script, b"#!/bin/sh\n").expect("write script");
        // The stand-in records its arguments: a run that reaches the host's
        // systemctl instead writes nothing here.
        let calls = dir.join("systemctl-calls");
        let program = executable_systemctl_stand_in(&dir, &calls);

        let cleanup_leftovers = finalize_rollback_at(
            ledger.to_str().unwrap(),
            sysctl.to_str().unwrap(),
            service.to_str().unwrap(),
            script.to_str().unwrap(),
            true,
            program.to_str().unwrap(),
        );

        assert!(cleanup_leftovers.is_empty());
        assert!(
            !service.exists(),
            "the named program's work still completes"
        );
        assert_eq!(
            fs::read_to_string(&calls).expect("the named program must be the one that ran"),
            "disable ktuner-nonsysctl.service\ndaemon-reload\n"
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// The ledger is the third thing a cleanup deletes, and its removal used to
    /// be the one result thrown away: a ledger that survived the delete left
    /// `rollback --list` reporting a pending rollback of values the run had
    /// already restored, while `rollback` reported a finished cleanup and exited
    /// 0. It is the record a retry restores from, so a delete that fails leaves
    /// the cleanup incomplete the same way a surviving persisted file does.
    ///
    /// A directory standing in for the ledger makes `remove_file` fail with
    /// `EISDIR` on every filesystem, so this needs neither root nor a
    /// read-only `/var/lib`.
    #[test]
    fn test_cleanup_counts_a_ledger_that_could_not_be_removed() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_cleanup_ledger_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).expect("create temp dir");
        let ledger = dir.join("rollback.json");
        fs::create_dir_all(&ledger).expect("create a directory where the ledger belongs");
        let sysctl = dir.join("99-ktuner.conf");
        fs::write(&sysctl, b"vm.swappiness = 10\n").expect("write sysctl file");
        let service = dir.join("ktuner-nonsysctl.service");
        let script = dir.join("apply-nonsysctl.sh");
        fs::write(&script, b"#!/bin/sh\n").expect("write script");

        let cleanup_leftovers = finalize_rollback_at(
            ledger.to_str().unwrap(),
            sysctl.to_str().unwrap(),
            service.to_str().unwrap(),
            script.to_str().unwrap(),
            true,
            "true",
        );

        assert_eq!(
            cleanup_leftovers,
            vec![ledger.to_str().unwrap().to_string()],
            "the persisted files went, so the ledger is the removal that failed"
        );
        assert!(
            ledger.exists(),
            "the record a retry restores from is still there, so the cleanup is \
             not finished"
        );
        assert!(!sysctl.exists() && !script.exists());

        // What the name means for the caller: the rollback is incomplete, so
        // `rollback` exits 1 instead of reporting a finished cleanup.
        let outcome = RollbackOutcome {
            restored_params: test_restored(3),
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: cleanup_leftovers,
            boot_unit_enabled: true,
        };
        assert!(!outcome.is_complete());
        assert_eq!(classify_rollback(&outcome), RollbackStatus::Partial);

        fs::remove_dir_all(&dir).ok();
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
                [(first.to_string(), "10".to_string(), "20".to_string(), None)],
            );
            let data = merge_entries(
                data,
                [(second.to_string(), "20".to_string(), "30".to_string(), None)],
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
                None,
            )],
        );
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries["vm/swappiness"];
        assert_eq!(entry.previous, "10");
        assert_eq!(entry.applied, "30");
        assert_eq!(entry.path, "/proc/sys/vm/swappiness");
    }

    #[test]
    fn merge_entries_records_the_rule_category() {
        // Each entry remembers which rule layer wrote it. The category is a
        // property of the rule, not of the parameter name — the second
        // entry below is a security finding on a net.* knob — so the ledger
        // is the only place a later `rollback --category` can find it.
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        let data = merge_entries(
            data,
            [
                (
                    "net.core.somaxconn".to_string(),
                    "128".to_string(),
                    "4096".to_string(),
                    Some(Category::Performance),
                ),
                (
                    "net.core.bpf_jit_harden".to_string(),
                    "0".to_string(),
                    "1".to_string(),
                    Some(Category::Security),
                ),
            ],
        );
        assert_eq!(
            data.entries["net.core.somaxconn"].category,
            Some(Category::Performance)
        );
        assert_eq!(
            data.entries["net.core.bpf_jit_harden"].category,
            Some(Category::Security)
        );
        let json = serde_json::to_string(&data).unwrap();
        assert!(json.contains(r#""category":"performance""#), "{json}");
        assert!(json.contains(r#""category":"security""#), "{json}");
    }

    #[test]
    fn merge_entries_keeps_the_earliest_previous_and_the_latest_category() {
        // One rule for both extra fields: the pristine original is kept from
        // the earliest write, the classification from the latest — the same
        // way `applied` describes the latest write. A knob re-recorded under
        // another category is selected by that category afterwards.
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        let data = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "60".to_string(),
                "10".to_string(),
                Some(Category::Performance),
            )],
        );
        let data = merge_entries(
            data,
            [(
                "vm/swappiness".to_string(),
                "10".to_string(),
                "20".to_string(),
                Some(Category::Security),
            )],
        );
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries["vm.swappiness"];
        assert_eq!(
            entry.previous, "60",
            "the pristine original is the earliest"
        );
        assert_eq!(entry.applied, "20");
        assert_eq!(
            entry.category,
            Some(Category::Security),
            "the classification is the latest write's"
        );
    }

    #[test]
    fn a_write_without_a_rule_category_records_none() {
        // The library import path records no category: the value came from a
        // file, not from a rule recommendation, and the ledger must not
        // invent a classification for it. The field is then absent from the
        // file, so a reader that does not know it sees the old shape.
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::new(),
        };
        let data = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "60".to_string(),
                "40".to_string(),
                None,
            )],
        );
        assert_eq!(data.entries["vm.swappiness"].category, None);
        let json = serde_json::to_string(&data).unwrap();
        assert!(!json.contains("category"), "{json}");
        // Re-recording the knob through a rule classifies it, and the
        // pristine original survives the refresh.
        let data = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "40".to_string(),
                "10".to_string(),
                Some(Category::Performance),
            )],
        );
        let entry = &data.entries["vm.swappiness"];
        assert_eq!(entry.category, Some(Category::Performance));
        assert_eq!(entry.previous, "60");
        assert_eq!(entry.applied, "10");
    }

    #[test]
    fn a_no_op_import_keeps_the_rule_category() {
        // `apply_import`'s recording decision, without a writable /proc/sys: a
        // write that moves the value records no classification (what is live
        // is then the imported file's, not a rule's), while a write the kernel
        // accepts without changing anything keeps the category the recording
        // rule stamped on the entry — the record still describes the value the
        // rules wrote, and clearing it would report a rule-tuned knob as one
        // no `--category` selects.
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::from([(
                "vm.swappiness".to_string(),
                RollbackEntry {
                    previous: "60".to_string(),
                    applied: "1".to_string(),
                    path: "/proc/sys/vm/swappiness".to_string(),
                    category: Some(Category::Performance),
                },
            )]),
        };
        // The alias spelling of the same kernel file resolves to the same
        // record, exactly as the merge keys it.
        for spelling in ["vm.swappiness", "vm/swappiness"] {
            assert_eq!(
                imported_category(&data, spelling, "1", "1"),
                Some(Category::Performance),
                "{spelling}: a write that changes nothing keeps the rule's category"
            );
            assert_eq!(
                imported_category(&data, spelling, "1", "10"),
                None,
                "{spelling}: a write that moves the value records no rule category"
            );
        }
        // A knob the ledger does not record yet records no classification
        // either: nothing is invented for it.
        assert_eq!(imported_category(&data, "vm.dirty_ratio", "1", "1"), None);
        // An entry a library import wrote carries no classification to keep.
        let imported_only = RollbackData {
            version: 1,
            entries: BTreeMap::from([(
                "vm.swappiness".to_string(),
                RollbackEntry {
                    previous: "60".to_string(),
                    applied: "1".to_string(),
                    path: "/proc/sys/vm/swappiness".to_string(),
                    category: None,
                },
            )]),
        };
        assert_eq!(
            imported_category(&imported_only, "vm.swappiness", "1", "1"),
            None
        );
    }

    #[test]
    fn a_ledger_without_categories_re_saves_without_adding_the_key() {
        // A ledger written before the field existed: reading it must not
        // invent a category, and the restore that keeps one of its entries
        // must re-save that entry without adding the key — the record it had
        // is the record it keeps.
        let dir = AtomicTestDir::new("legacy-ledger-roundtrip");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = dir.0.join("rollback.json");
        fs::write(
            &ledger,
            format!(
                r#"{{"version":1,"entries":{{
                    "net.core.somaxconn":{{"previous":"128","applied":"4096","path":"{}"}},
                    "vm.swappiness":{{"previous":"60","applied":"10","path":"{}"}}
                }}}}"#,
                somaxconn.display(),
                swappiness.display(),
            ),
        )
        .unwrap();
        let path = ledger.to_str().unwrap().to_string();
        let data = load_rollback_from(&path).unwrap();
        assert_eq!(data.entries["vm.swappiness"].category, None);

        // Restoring one entry leaves the other, so the ledger is rewritten
        // from a record that never had a category.
        let (key, outcome) =
            rollback_param_at("vm.swappiness", &path, &conf, &service, &script).unwrap();
        assert_eq!(key, "vm.swappiness");
        assert!(outcome.is_complete());
        let raw = fs::read_to_string(&ledger).unwrap();
        assert!(!raw.contains("category"), "re-saving added the key: {raw}");
        let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(
            parsed["entries"]["net.core.somaxconn"],
            serde_json::json!({
                "previous": "128", "applied": "4096", "path": somaxconn.to_str().unwrap(),
            })
        );
    }

    /// The same kept classification, for a record whose recorded `path` is no
    /// longer what the knob resolves to: the merge adopts that record through
    /// its key (the stale-path repair), so a lookup that only compares the
    /// recorded path finds no record at all and a no-op import refreshes the
    /// entry with `category: null` — erasing the classification a rule stamped
    /// on a knob the import did not change, and dropping it from what
    /// `rollback --category` selects.
    #[test]
    fn a_no_op_import_keeps_a_stale_records_category() {
        let param = "net.ipv4.conf.Br0.100.forwarding";
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::from([(
                param.to_string(),
                RollbackEntry {
                    previous: "1".to_string(),
                    applied: "1".to_string(),
                    path: "/proc/sys/net/ipv4/conf/Br0/100/forwarding".to_string(),
                    category: Some(Category::Performance),
                },
            )]),
        };
        for spelling in [param, "net/ipv4/conf/Br0.100/forwarding"] {
            assert_eq!(
                imported_category(&data, spelling, "1", "1"),
                Some(Category::Performance),
                "{spelling}: a write that changes nothing keeps the rule's category"
            );
        }
        // And the merge that follows the import keeps it too: the adopted
        // record survives with the classification the rules wrote. This is the
        // category `apply_import` hands the merge for this write.
        let category = imported_category(&data, param, "1", "1");
        let merged = merge_entries(
            data,
            [(
                "net/ipv4/conf/Br0.100/forwarding".to_string(),
                "1".to_string(),
                "1".to_string(),
                category,
            )],
        );
        assert_eq!(
            merged.entries[param].category,
            Some(Category::Performance),
            "the classification survives the no-op import"
        );
    }

    /// The same heal, on the classification a no-op import keeps. The answer
    /// decides what the merge writes onto the record it keeps, so reading the
    /// raw ledger does not only report the discarded alias's category — it
    /// REPLACES the survivor's with it: a rule's classification swapped for an
    /// unrelated alias's by a write that changed nothing, after which no
    /// `--category` selects the entry the rule wrote.
    #[test]
    fn a_duplicated_alias_keeps_the_category_the_heal_keeps() {
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"10","applied":"30",
                    "path":"/proc/sys/vm/swappiness","category":"performance"},
                "vm/swappiness":{"previous":"20","applied":"40",
                    "path":"/proc/sys/vm/swappiness","category":"security"}
            }}"#,
        )
        .unwrap();
        // No chain relation, so the heal keeps the greatest key verbatim —
        // the record a restore writes last.
        let mut healed = data.clone();
        heal_alias_duplicates(&mut healed);
        assert_eq!(
            healed.entries["vm/swappiness"].category,
            Some(Category::Security)
        );
        assert_eq!(
            imported_category(&data, "vm.swappiness", "40", "40"),
            Some(Category::Security),
            "the kept classification must be the survivor's"
        );
        // And the merge that follows the import keeps that answer: the
        // survivor is what the ledger holds after the no-op write.
        let category = imported_category(&data, "vm.swappiness", "40", "40");
        let merged = merge_entries(
            data,
            [(
                "vm.swappiness".to_string(),
                "40".to_string(),
                "40".to_string(),
                category,
            )],
        );
        assert_eq!(merged.entries.len(), 1);
        assert_eq!(
            merged.entries["vm/swappiness"].category,
            Some(Category::Security),
            "a no-op import must not reclassify the record it keeps"
        );
    }

    #[test]
    fn heal_alias_duplicates_keeps_the_latest_classification() {
        // Two spellings of one kernel path chained by their values collapse
        // to the earlier key with the later write's `applied` — and its
        // category, for the same reason: the classification describes the
        // record that survived.
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{
                    "previous":"60","applied":"10",
                    "path":"/proc/sys/vm/swappiness","category":"performance"
                },
                "vm/swappiness":{
                    "previous":"10","applied":"20",
                    "path":"/proc/sys/vm/swappiness","category":"security"
                }
            }}"#,
        )
        .unwrap();
        let data = merge_entries(data, []);
        assert_eq!(data.entries.len(), 1);
        let entry = &data.entries["vm.swappiness"];
        assert_eq!(entry.previous, "60");
        assert_eq!(entry.applied, "20");
        assert_eq!(entry.category, Some(Category::Security));
    }

    #[test]
    fn a_cleared_sibling_inherits_the_category_of_its_clearer() {
        // The twin has no rule of its own — the kernel zeroed it as a side
        // effect of the write — so it is classified like the knob that
        // cleared it: the pair is one restore unit, and a category that
        // selects one half must select the other.
        let dir = AtomicTestDir::new("sibling-category");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let cleared = sibling_cleared_record("vm.dirty_ratio", "20").expect("configured twin");
        let knob = Recommendation {
            param: "vm.dirty_bytes".to_string(),
            current_value: "0".to_string(),
            recommended_value: "1073741824".to_string(),
            category: Category::Security,
            writable: true,
            ..Default::default()
        };
        let sibling = sibling_rec(cleared, knob.category.clone());
        let guard = lock_ledger_at(path).unwrap();
        save_rollback(&guard, &[knob, sibling]).unwrap();
        let data = load_rollback_from(path).unwrap();
        assert_eq!(
            data.entries["vm.dirty_bytes"].category,
            Some(Category::Security)
        );
        assert_eq!(
            data.entries["vm.dirty_ratio"].category,
            Some(Category::Security),
            "the cleared twin shares its clearer's classification"
        );
        assert_eq!(data.entries["vm.dirty_ratio"].applied, "0");
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
                None,
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

    /// The same stale path, but under the OTHER spelling of the knob: a merge
    /// arriving as `net/ipv4/conf/Br0.100/forwarding` compared the recorded
    /// path only, so the dotted entry above was not recognized as the record
    /// this write belongs to — a second record for one kernel file was
    /// inserted. The pristine `previous` then sat in a record no restore can
    /// reach (its path is gone), the restore wrote the new record's
    /// intermediate value, and the ledger never cleared. The key names the same
    /// knob, so the merge adopts it and re-points the path.
    #[test]
    fn test_merge_adopts_a_stale_path_under_the_other_spelling() {
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
                "net/ipv4/conf/Br0.100/forwarding".to_string(),
                "1".to_string(),
                "1".to_string(),
                None,
            )],
        );
        assert_eq!(
            data.entries.len(),
            1,
            "one kernel file stays one record: {:?}",
            data.entries.keys().collect::<Vec<_>>()
        );
        let entry = &data.entries["net.ipv4.conf.Br0.100.forwarding"];
        assert_eq!(entry.previous, "0", "the pristine value survives");
        assert_eq!(entry.applied, "1");
        assert_eq!(
            entry.path, "/proc/sys/net/ipv4/conf/Br0.100/forwarding",
            "the stale path is re-pointed at what the knob resolves to"
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
                    None,
                ),
                (
                    "vm.swappiness_extra".to_string(),
                    "40".to_string(),
                    "50".to_string(),
                    None,
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
                    None,
                )],
            );
            let data = merge_entries(
                data,
                [(
                    second.to_string(),
                    "middle".to_string(),
                    "after".to_string(),
                    None,
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
            data = merge_entries(
                data,
                [(param.to_string(), previous, applied.to_string(), None)],
            );
        }
        assert_eq!(fs::read_to_string(&path).unwrap(), "30");
        // Run the production restore loop against a temporary parameter file;
        // no /proc/sys writes or system-wide rollback cleanup are needed.
        for entry in data.entries.values_mut() {
            entry.path = path.to_str().unwrap().to_string();
        }
        let outcome = restore_entries(&data, true);
        assert_eq!(fs::read_to_string(&path).unwrap(), "10");
        assert_eq!(outcome.restored(), 1);
        assert_eq!(outcome.failed(), 0);
        assert_eq!(outcome.skipped(), 0);
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
        assert_eq!(outcome.restored(), 1, "one knob, one restore write");
        assert_eq!(outcome.failed(), 0);
        assert_eq!(outcome.skipped(), 0);
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
                None,
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
        assert_eq!(outcome.restored(), 1, "one knob, one restore write");
        assert_eq!(outcome.failed(), 0);
        assert_eq!(outcome.skipped(), 0);
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
                    category: None,
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
        assert_eq!(outcome.restored(), 1);
        assert_eq!(outcome.failed(), 2);
        assert_eq!(outcome.skipped(), 1);
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
                category: None,
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
            (outcome.restored(), outcome.failed(), outcome.skipped()),
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
                    category: None,
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
        assert_eq!(outcome.restored(), 1);
        assert_eq!(outcome.failed(), 1);
        assert_eq!(outcome.skipped(), 0);
        assert!(!rollback_should_finalize(
            outcome.failed(),
            outcome.skipped()
        ));
    }

    /// The undo path has to refuse the names the write path refuses. The write
    /// choke point enforces `is_safe_param` before any live write, which is
    /// what lets the apply paths take a caller-built parameter; `restore`
    /// writes the path the ledger RECORDED and checks only the deny-list next
    /// to it. A ledger written before that guard existed, or by a library
    /// caller, can hold `net/ipv4/conf/../../../../..<file>`: its recorded
    /// path resolves outside /proc/sys, and restoring it would replay as root
    /// the very traversal the apply path now refuses.
    #[test]
    fn restore_refuses_a_param_the_write_path_refuses() {
        let dir = AtomicTestDir::new("restore_unsafe_name");
        let victim = dir.0.join("victim");
        fs::write(&victim, "original").unwrap();
        // /proc/sys/net/ipv4/conf/../../../../.. reaches the filesystem root,
        // so this name addresses the file it names, not a kernel parameter.
        let param = format!("net/ipv4/conf/../../../../..{}", victim.display());
        assert!(
            !is_safe_param(&param),
            "the write choke point refuses this name"
        );
        let entry = RollbackEntry {
            previous: "pwned".to_string(),
            applied: "original".to_string(),
            path: param_to_path(&param),
            category: None,
        };
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::from([(param.clone(), entry)]),
        };

        let outcome = restore_entries(&data, true);

        assert_eq!(
            fs::read_to_string(&victim).unwrap(),
            "original",
            "a restore must not write outside the kernel's own trees"
        );
        assert_eq!(outcome.restored(), 0);
        assert_eq!(
            outcome.failed(),
            1,
            "the refusal is a failure, so the record stays for a retry"
        );
        assert_eq!(outcome.skipped(), 0);
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
                    category: None,
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
        assert_eq!(outcome.restored(), 1, "only the ratio write holds");
        assert_eq!(
            outcome.failed(),
            1,
            "the bytes knob was cleared by the ratio write and must count as failed"
        );
        assert_eq!(outcome.skipped(), 0);
        assert!(
            !rollback_should_finalize(outcome.failed(), outcome.skipped()),
            "a cleared sibling must keep the ledger for inspection"
        );
    }

    #[test]
    fn a_refused_write_the_twin_restored_counts_as_restored() {
        // The kernel rejects the pristine 0 of a ratio-mode host on both dirty
        // `*_bytes` knobs (v6.6 mm/page-writeback.c: `dirty_bytes` is
        // registered with `dirty_bytes_min` = 2 * PAGE_SIZE and
        // `dirty_background_bytes` with `SYSCTL_ONE`, and
        // proc_doulongvec_minmax returns EINVAL below them — `echo 0 >
        // /proc/sys/vm/dirty_bytes` fails on a stock host). The zero comes back
        // through the TWIN's restore instead: `dirty_ratio_handler` clears
        // `vm.dirty_bytes` on every changed write, and the ledger's cleared-twin
        // record restores that ratio after the refused bytes write. Judging the
        // refused write by its own error reported Partial forever (exit 1,
        // ledger kept, boot replay left in place) on a host whose pair state the
        // twin's write had already brought back.
        let dir = AtomicTestDir::new("restore_refused_write");
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        let bytes_path = bytes.to_str().unwrap().to_string();
        let ratio_path = ratio.to_str().unwrap().to_string();
        // Live state at rollback time: the bytes knob holds the tuned value,
        // the ratio the write cleared.
        fs::write(&bytes, "268435456").unwrap();
        fs::write(&ratio, "0").unwrap();
        let entries = BTreeMap::from([
            (
                "vm.dirty_bytes".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "268435456".into(),
                    path: bytes_path.clone(),
                    category: None,
                },
            ),
            (
                "vm.dirty_ratio".to_string(),
                RollbackEntry {
                    previous: "20".into(),
                    applied: "0".into(),
                    path: ratio_path.clone(),
                    category: None,
                },
            ),
        ]);
        // The kernel write, simulated on plain files: the bytes file refuses
        // the below-minimum zero, and a changed ratio write zeroes it.
        let mut kernel = |path: &str, value: &str| -> std::io::Result<()> {
            if path == bytes_path && value.parse::<u64>().unwrap() < 2 * 4096 {
                return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
            }
            fs::write(path, value)?;
            if path == ratio_path {
                fs::write(&bytes, "0")
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
            fs::read_to_string(&bytes).unwrap(),
            "0",
            "the twin's restore puts the recorded original back"
        );
        assert_eq!(fs::read_to_string(&ratio).unwrap(), "20");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0),
            "both halves of the pair are restored"
        );
        // The refused write landed through the twin, so it is one of the
        // restored entries and must be named like any other: a caller cannot
        // learn from the count which key the pair's second write brought back.
        // The order is the one the batch decided in: the accepted write first,
        // then the refused write the deferred read proved live.
        assert_eq!(
            outcome
                .restored_params
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["vm.dirty_ratio", "vm.dirty_bytes"],
            "the refused write the twin restored is named too"
        );
        assert!(
            rollback_should_finalize(outcome.failed(), outcome.skipped()),
            "a fully restored pair must finalize: the ledger is deleted and the boot replay removed"
        );
    }

    #[test]
    fn a_refused_write_nothing_restores_still_fails() {
        // Same refusal WITHOUT the twin's record: nothing can put the recorded
        // zero live, so the bytes knob keeps the tuned value and the rollback
        // must report a failure (keep the ledger for a retry) — the deferred
        // verdict must not turn any refusal into a success.
        let dir = AtomicTestDir::new("restore_refused_write_unrestored");
        let bytes = dir.0.join("dirty_bytes");
        let bytes_path = bytes.to_str().unwrap().to_string();
        fs::write(&bytes, "268435456").unwrap();
        let entries = BTreeMap::from([(
            "vm.dirty_bytes".to_string(),
            RollbackEntry {
                previous: "0".into(),
                applied: "268435456".into(),
                path: bytes_path.clone(),
                category: None,
            },
        )]);
        let mut kernel = |path: &str, value: &str| -> std::io::Result<()> {
            if path == bytes_path && value.parse::<u64>().unwrap() < 2 * 4096 {
                return Err(std::io::Error::from_raw_os_error(libc::EINVAL));
            }
            fs::write(path, value)
        };
        let outcome = restore_entries_with(
            &RollbackData {
                version: 1,
                entries,
            },
            true,
            &mut kernel,
        );
        assert_eq!(fs::read_to_string(&bytes).unwrap(), "268435456");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (0, 1, 0),
            "a refusal nothing else repaired stays a failure"
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
                    category: None,
                },
            ),
            (
                "vm.overcommit_ratio".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "80".into(),
                    path: ratio_path.clone(),
                    category: None,
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
        assert_eq!(outcome.restored(), 2);
        assert_eq!(outcome.failed(), 0);
        assert!(rollback_should_finalize(
            outcome.failed(),
            outcome.skipped()
        ));
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
                    category: None,
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
        assert_eq!(outcome.restored(), 2);
        assert_eq!(outcome.failed(), 0);
        assert_eq!(outcome.skipped(), 0);
        assert!(rollback_should_finalize(
            outcome.failed(),
            outcome.skipped()
        ));
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
                None,
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
                None,
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

    /// `fix` reports its original through `apply_one`, which must answer with
    /// the record the merge above keeps: the pristine `previous`, not a fresh
    /// read of the knob. Once a parameter is recorded, the pre-write read is an
    /// intermediate value the ledger does not hold and a rollback does not
    /// restore, so reporting it made a second `fix` of one knob contradict the
    /// ledger and `rollback --list` on the very knob it had just fixed — the
    /// contradiction #6280 fixed for the first write only.
    #[test]
    fn the_recorded_original_wins_over_a_reread() {
        let mut entries = BTreeMap::new();
        entries.insert(
            "vm.swappiness".to_string(),
            RollbackEntry {
                previous: "10".to_string(),
                applied: "30".to_string(),
                path: "/proc/sys/vm/swappiness".to_string(),
                category: None,
            },
        );
        let data = RollbackData {
            version: 1,
            entries,
        };
        assert_eq!(
            recorded_previous_value(&data, "vm.swappiness").as_deref(),
            Some("10"),
            "the ledger's pristine previous is what rollback restores"
        );
        // The alias spelling resolves to the same record: merge_entries keys
        // on the canonicalized path, so the reported original must come from
        // the record that write refreshes.
        assert_eq!(
            recorded_previous_value(&data, "vm/swappiness").as_deref(),
            Some("10")
        );
        // A knob the ledger does not record yet has no recorded original: the
        // caller's under-lock pre-write read is then the value to report.
        assert_eq!(recorded_previous_value(&data, "vm.dirty_ratio"), None);
    }

    /// A record whose `path` no longer is what the knob resolves to (a ledger
    /// written before the dotted-interface fix translated
    /// `net.ipv4.conf.Br0.100.forwarding` as `conf/Br0/100/forwarding`, a path
    /// no kernel has) is still the record a later write belongs to: the merge
    /// recognizes it by its key, re-points it and keeps its `previous`. The
    /// reported original has to come from that same record, or `fix` answers
    /// with the intermediate value it just read while the ledger and a rollback
    /// restore the pristine one — the contradiction a lookup keyed on the
    /// recorded path alone leaves in place.
    #[test]
    fn a_stale_record_reports_the_original_the_merge_keeps() {
        let param = "net.ipv4.conf.Br0.100.forwarding";
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::from([(
                param.to_string(),
                RollbackEntry {
                    previous: "0".to_string(),
                    applied: "1".to_string(),
                    path: "/proc/sys/net/ipv4/conf/Br0/100/forwarding".to_string(),
                    category: None,
                },
            )]),
        };
        // Both spellings of the knob resolve to the file the record is about;
        // neither resolves to the path it recorded.
        for spelling in [param, "net/ipv4/conf/Br0.100/forwarding"] {
            assert_eq!(
                recorded_previous_value(&data, spelling).as_deref(),
                Some("0"),
                "{spelling}: the record the merge adopts holds this original"
            );
        }
        // The control the answer keys on: the write of either spelling lands on
        // that one record — one entry, pristine `previous`, path re-pointed —
        // so the reported original and the merge cannot disagree about it.
        let merged = merge_entries(
            data,
            [(
                "net/ipv4/conf/Br0.100/forwarding".to_string(),
                "1".to_string(),
                "0".to_string(),
                None,
            )],
        );
        assert_eq!(
            merged.entries.len(),
            1,
            "one kernel file stays one record: {:?}",
            merged.entries.keys().collect::<Vec<_>>()
        );
        let entry = &merged.entries[param];
        assert_eq!(entry.previous, "0", "the pristine original survives");
        assert_eq!(entry.applied, "0");
        assert_eq!(
            entry.path, "/proc/sys/net/ipv4/conf/Br0.100/forwarding",
            "the stale path is re-pointed at what the knob resolves to"
        );
        assert_eq!(
            recorded_previous_value(&merged, "net/ipv4/conf/Br0.100/forwarding").as_deref(),
            Some("0")
        );
    }

    /// A legacy ledger can hold two spellings of one kernel path (the alias
    /// dedup predates the heal). Every consumer heals before it acts — the
    /// merge, a restore and the listing — and the heal keeps ONE of the pair
    /// when the values cannot order it, so the record whose `previous` a
    /// rollback writes is that survivor. A lookup that reads the raw ledger
    /// answers with the other record, and `fix` reports an original no
    /// rollback restores — contradicting the ledger on the very knob it just
    /// fixed.
    #[test]
    fn a_duplicated_alias_reports_the_original_the_heal_keeps() {
        let data: RollbackData = serde_json::from_str(
            r#"{"version":1,"entries":{
                "vm.swappiness":{"previous":"10","applied":"30",
                    "path":"/proc/sys/vm/swappiness"},
                "vm/swappiness":{"previous":"20","applied":"40",
                    "path":"/proc/sys/vm/swappiness"}
            }}"#,
        )
        .unwrap();
        // The heal every other consumer runs first. Neither `applied` equals
        // the other's `previous` — the knob was changed outside ktuner between
        // the two records — so the pair cannot be ordered and the greatest key
        // survives verbatim, which is the record today's key-order restore
        // writes last.
        let mut healed = data.clone();
        heal_alias_duplicates(&mut healed);
        assert_eq!(healed.entries.len(), 1, "one kernel file is one record");
        assert_eq!(healed.entries["vm/swappiness"].previous, "20");
        // The reported original is that survivor's: the value a rollback
        // writes.
        assert_eq!(
            recorded_previous_value(&data, "vm.swappiness").as_deref(),
            Some("20"),
            "the answer must be the record the merge and a restore act on"
        );
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

    /// The ledger directory is created by the lock, not by the save, and it is
    /// documented as 0700 root-owned (README: "the ledger is 0600 under a 0700
    /// root-owned dir"). A first `rollback` on a fresh host takes the lock,
    /// finds no ledger and fails; the directory it created stays behind, so
    /// the mode must be set on creation — a mode applied only by a successful
    /// merge left it at the umask's 0755.
    #[test]
    fn test_ledger_lock_creates_the_directory_private() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_ledger_dir_mode_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        let ledger = dir.join("varlib/rollback.json");
        let guard = lock_ledger_at(ledger.to_str().unwrap()).unwrap();
        let mode = fs::metadata(dir.join("varlib"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        drop(guard);
        fs::remove_dir_all(&dir).ok();
        assert_eq!(mode, 0o700, "the ledger directory must be created 0700");
    }

    /// The create-only mode above is deliberate: `rollback --list` is
    /// documented read-only ("nothing is written or deleted"), so a shared
    /// acquisition must not chmod a directory it did not create. A save
    /// (`merge_rollback_locked`) still re-hardens an existing one.
    #[test]
    fn test_ledger_lock_leaves_an_existing_directory_mode_alone() {
        let dir = std::env::temp_dir().join(format!(
            "ktuner_ledger_dir_keep_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        fs::remove_dir_all(&dir).ok();
        let ledger = dir.join("varlib/rollback.json");
        fs::create_dir_all(dir.join("varlib")).unwrap();
        fs::set_permissions(dir.join("varlib"), fs::Permissions::from_mode(0o755)).unwrap();
        let guard = lock_ledger_shared_at(ledger.to_str().unwrap()).unwrap();
        let mode = fs::metadata(dir.join("varlib"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        drop(guard);
        fs::remove_dir_all(&dir).ok();
        assert_eq!(mode, 0o755, "a shared lock must not chmod the directory");
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
        assert_eq!(outcome.applied(), 1);
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
        // The batch's own record of the write: the count is derived from the
        // entries, and each entry names the original the ledger holds. That
        // second half only holds if the recording site consults the ledger:
        // the fixture's live value moves with every write, so a re-write of
        // the same knob must still report the FIRST original — the one
        // `rollback` restores — and not the value it found live.
        assert_eq!(outcome.applied(), outcome.written.len());
        assert_eq!(outcome.written.len(), 1);
        assert_eq!(outcome.written[0].param, "net.core.somaxconn");
        assert_eq!(outcome.written[0].previous.as_deref(), Some("1024"));
        assert_eq!(outcome.written[0].applied, "65535");
        let again = [Recommendation {
            param: "net.core.somaxconn".to_string(),
            current_value: "stale gathered value".to_string(),
            recommended_value: "4096".to_string(),
            writable: true,
            ..Default::default()
        }];
        let outcome = apply_locked(&again, true, &guard).expect("second fixture-guarded apply");
        assert_eq!(
            outcome.written[0].previous.as_deref(),
            Some("1024"),
            "a re-write must report the original the ledger keeps, not the live value"
        );
        assert_eq!(outcome.written[0].applied, "4096");
        assert_eq!(
            load_rollback_from(guard_ledger.to_str().unwrap())
                .unwrap()
                .entries["net.core.somaxconn"]
                .previous,
            "1024",
            "the entry must agree with the ledger it summarizes"
        );
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

    /// Write a hand-built ledger whose entries point at caller-chosen paths,
    /// so the live reads hit temp fixture files (a gone device and an
    /// unreadable path included) instead of the host's /proc/sys and /sys.
    fn write_preview_ledger(path: &str, entries: &[(&str, &str, &str, &str)]) {
        let mut map = serde_json::Map::new();
        for (param, previous, applied, target) in entries {
            map.insert(
                (*param).to_string(),
                serde_json::json!({
                    "previous": previous, "applied": applied, "path": target,
                }),
            );
        }
        fs::write(
            path,
            serde_json::json!({ "version": 1, "entries": map }).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn test_rollback_preview_reports_pending_set() {
        // Control: a normal preview still publishes the recorded
        // (param, applied, previous) triples exactly as before — same values,
        // same BTreeMap order, nothing filtered — and adds the live reading
        // on top. The fixture paths are temp files, so the live read never
        // touches the host's /proc/sys or /sys.
        let dir = AtomicTestDir::new("preview-normal");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        fs::write(&somaxconn, "256\n").unwrap();
        fs::write(&swappiness, "10\n").unwrap();
        write_preview_ledger(
            path,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "256",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(
            entries,
            vec![
                RollbackPreview {
                    param: "net.core.somaxconn".to_string(),
                    applied: "256".to_string(),
                    previous: "128".to_string(),
                    category: None,
                    live: Some("256".to_string()),
                    drifted: Some(false),
                    cleared_by: None,
                },
                RollbackPreview {
                    param: "vm.swappiness".to_string(),
                    applied: "10".to_string(),
                    previous: "60".to_string(),
                    category: None,
                    live: Some("10".to_string()),
                    drifted: Some(false),
                    cleared_by: None,
                },
            ]
        );
    }

    #[test]
    fn test_rollback_preview_flags_a_drifted_value() {
        // The knob moved after ktuner wrote it: live must carry the value
        // actually in the kernel, and drifted must say the recorded applied
        // value is no longer the one live.
        let dir = AtomicTestDir::new("preview-drift");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let target = dir.0.join("swappiness");
        fs::write(&target, "60\n").unwrap();
        write_preview_ledger(
            path,
            &[("vm.swappiness", "60", "1", target.to_str().unwrap())],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].live.as_deref(), Some("60"));
        assert_eq!(entries[0].drifted, Some(true));
    }

    #[test]
    fn test_rollback_preview_reports_nulls_for_a_missing_path() {
        // The device is gone / the module is not loaded: the path cannot be
        // read, which is not an error but an unanswerable comparison — both
        // fields are null, and the row is still reported.
        let dir = AtomicTestDir::new("preview-missing");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let target = dir.0.join("gone");
        write_preview_ledger(
            path,
            &[(
                "block/sda/scheduler",
                "mq-deadline",
                "none",
                target.to_str().unwrap(),
            )],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].live, None);
        assert_eq!(entries[0].drifted, None);
    }

    #[test]
    fn test_rollback_preview_reports_nulls_for_an_unreadable_path() {
        // A path that exists but cannot be read (here: a directory) is
        // unreadable, not empty and not an error: null, null.
        let dir = AtomicTestDir::new("preview-unreadable");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let target = dir.0.join("a-directory");
        fs::create_dir(&target).unwrap();
        write_preview_ledger(
            path,
            &[("vm.swappiness", "60", "1", target.to_str().unwrap())],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].live, None);
        assert_eq!(entries[0].drifted, None);
    }

    #[test]
    fn test_rollback_preview_reports_a_raw_byte_value_lossily() {
        // The kernel stores a string sysctl as raw bytes: `sethostname(2)`
        // refuses only NUL and names past the length limit (v6.6
        // kernel/sys.c __sethostname), so `kernel.hostname` — and
        // `kernel.domainname`, which root writes the same way — can hold a
        // name outside UTF-8. Every other reader in the crate decodes such a
        // file lossily, and `why` answers the value of the very same knob; a
        // strict read here made the listing claim the path cannot be read at
        // all — the shape the README reserves for a device that is gone, a
        // module that is not loaded and a write-only tunable — and lost the
        // drift answer with it.
        let dir = AtomicTestDir::new("preview-raw-bytes");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let drifted_target = dir.0.join("hostname");
        fs::write(&drifted_target, b"node-\xff.lan\n").unwrap();
        let matching_target = dir.0.join("domainname");
        fs::write(&matching_target, b"lan-\xfe.example\n").unwrap();
        write_preview_ledger(
            path,
            &[
                (
                    "kernel.hostname",
                    "node-1.lan",
                    "node-1.lan",
                    drifted_target.to_str().unwrap(),
                ),
                (
                    "kernel.domainname",
                    "lan-\u{fffd}.example",
                    "lan-\u{fffd}.example",
                    matching_target.to_str().unwrap(),
                ),
            ],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 2);
        // A value that differs from the record is still compared, so the
        // operator sees the drift instead of an unreadable path.
        let hostname = entries
            .iter()
            .find(|entry| entry.param == "kernel.hostname")
            .expect("the hostname entry is listed");
        assert_eq!(hostname.live.as_deref(), Some("node-\u{fffd}.lan"));
        assert_eq!(hostname.drifted, Some(true));
        // And one that only differs in the bytes the kernel cannot render is
        // not drift: the comparison runs on the lossy value.
        let domainname = entries
            .iter()
            .find(|entry| entry.param == "kernel.domainname")
            .expect("the domainname entry is listed");
        assert_eq!(domainname.live.as_deref(), Some("lan-\u{fffd}.example"));
        assert_eq!(domainname.drifted, Some(false));
    }

    #[test]
    fn test_rollback_preview_normalizes_multi_value_reads() {
        // tcp_rmem-style: the kernel renders the value TAB-separated while
        // the ledger records (and every other surface publishes) the
        // single-space form. The live value must come back in that canonical
        // form and the comparison must not report the format difference as
        // drift.
        let dir = AtomicTestDir::new("preview-multi-value");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let target = dir.0.join("tcp_rmem");
        fs::write(&target, "4096\t87380\t6291456\n").unwrap();
        write_preview_ledger(
            path,
            &[(
                "net.ipv4.tcp_rmem",
                "4096 87380 6291456",
                "4096 87380 6291456",
                target.to_str().unwrap(),
            )],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].live.as_deref(), Some("4096 87380 6291456"));
        assert_eq!(entries[0].drifted, Some(false));
    }

    #[test]
    fn test_rollback_preview_reads_the_active_option() {
        // sysfs option lists bracket the ACTIVE choice: the live value is
        // that token, and it matches the ledger even though the file's own
        // rendering differs.
        let dir = AtomicTestDir::new("preview-option-list");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let target = dir.0.join("scheduler");
        fs::write(&target, "[none] mq-deadline kyber\n").unwrap();
        write_preview_ledger(
            path,
            &[(
                "block/sda/scheduler",
                "mq-deadline",
                "none",
                target.to_str().unwrap(),
            )],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].live.as_deref(), Some("none"));
        assert_eq!(entries[0].drifted, Some(false));
    }

    #[test]
    fn test_rollback_preview_tolerates_a_leading_token_echo() {
        // The write path confirms a scalar write when the read-back leads
        // with the written token (the kernel may append its own rendering).
        // Reusing that equality keeps the preview from calling the same file
        // drift under a stricter comparison of its own.
        let dir = AtomicTestDir::new("preview-leading-token");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let target = dir.0.join("congestion");
        fs::write(&target, "bbr cubic\n").unwrap();
        write_preview_ledger(
            path,
            &[(
                "net.ipv4.tcp_congestion_control",
                "cubic",
                "bbr",
                target.to_str().unwrap(),
            )],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].live.as_deref(), Some("bbr cubic"));
        assert_eq!(entries[0].drifted, Some(false));
    }

    #[test]
    fn test_rollback_preview_compares_a_cleared_twin_against_the_kernel_zero() {
        // The kernel zeroes a mutually exclusive twin on the clearer's write,
        // and the ledger records applied = "0" for that twin. While it still
        // reads 0 the pair state the write left is live; the moment it is
        // non-zero (someone set the ratio again) the preview reports drift on
        // that row like on any other.
        let dir = AtomicTestDir::new("preview-cleared-twin");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let cleared = dir.0.join("dirty_background_ratio");
        let reenabled = dir.0.join("dirty_ratio");
        fs::write(&cleared, "0\n").unwrap();
        fs::write(&reenabled, "20\n").unwrap();
        write_preview_ledger(
            path,
            &[
                (
                    "vm.dirty_background_ratio",
                    "10",
                    "0",
                    cleared.to_str().unwrap(),
                ),
                ("vm.dirty_ratio", "20", "0", reenabled.to_str().unwrap()),
            ],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].param, "vm.dirty_background_ratio");
        assert_eq!(entries[0].live.as_deref(), Some("0"));
        assert_eq!(entries[0].drifted, Some(false));
        assert_eq!(entries[1].param, "vm.dirty_ratio");
        assert_eq!(entries[1].live.as_deref(), Some("20"));
        assert_eq!(entries[1].drifted, Some(true));
    }

    #[test]
    fn test_rollback_preview_drifted_is_null_exactly_when_live_is() {
        // One invariant across every readability outcome: drifted is a
        // comparison, so it exists exactly when there is a live value to
        // compare. No row may claim "not drifted" from an unreadable path.
        let dir = AtomicTestDir::new("preview-invariant");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        let same = dir.0.join("same");
        let moved = dir.0.join("moved");
        let missing = dir.0.join("missing");
        let unreadable = dir.0.join("unreadable");
        fs::write(&same, "1").unwrap();
        fs::write(&moved, "2").unwrap();
        fs::create_dir(&unreadable).unwrap();
        write_preview_ledger(
            path,
            &[
                ("a.same", "0", "1", same.to_str().unwrap()),
                ("b.moved", "0", "1", moved.to_str().unwrap()),
                ("c.missing", "0", "1", missing.to_str().unwrap()),
                ("d.unreadable", "0", "1", unreadable.to_str().unwrap()),
            ],
        );
        let entries = rollback_preview_at(path).unwrap();
        assert_eq!(entries.len(), 4);
        let readings: Vec<(Option<&str>, Option<bool>)> = entries
            .iter()
            .map(|e| (e.live.as_deref(), e.drifted))
            .collect();
        assert_eq!(
            readings,
            vec![
                (Some("1"), Some(false)),
                (Some("2"), Some(true)),
                (None, None),
                (None, None),
            ]
        );
        for entry in &entries {
            assert_eq!(
                entry.drifted.is_none(),
                entry.live.is_none(),
                "{}: drifted must be null exactly when live is",
                entry.param
            );
        }
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

    /// Every row `--list` publishes names the entry whose write zeroed it when
    /// the kernel did that as a side effect — the relation
    /// [`is_cleared_twin`] recognizes, which the row would otherwise only show
    /// as an `applied` of 0 that nothing asked for.
    #[test]
    fn preview_rows_name_the_entry_that_cleared_them() {
        let dir = AtomicTestDir::new("preview-cleared-by");
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        let swappiness = dir.0.join("swappiness");
        fs::write(&bytes, "1073741824").unwrap();
        fs::write(&ratio, "0").unwrap();
        fs::write(&swappiness, "0").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
                ("vm.swappiness", "60", "0", swappiness.to_str().unwrap()),
            ],
        );

        // Writing dirty_bytes zeroed dirty_ratio, and the listing says which
        // entry did it. The half that was asked for names nobody, and a
        // recorded 0 outside any pair names nobody either — the key is the
        // cleared-twin relation, not a restatement of `applied == "0"`.
        let listed: Vec<(String, Option<String>)> = rollback_preview_at(&ledger)
            .unwrap()
            .iter()
            .map(|entry| (entry.param.clone(), entry.cleared_by.clone()))
            .collect();
        assert_eq!(
            listed,
            vec![
                ("vm.dirty_bytes".to_string(), None),
                (
                    "vm.dirty_ratio".to_string(),
                    Some("vm.dirty_bytes".to_string())
                ),
                ("vm.swappiness".to_string(), None),
            ]
        );

        // `--list <param>` publishes the pair the restore takes along, so the
        // clearer is in the set the answer names — the selection cannot leave
        // it out.
        let named = rollback_preview_params_at(&["vm.dirty_ratio".to_string()], &ledger).unwrap();
        assert_eq!(named.len(), 2, "the twin comes along: {named:?}");
        assert_eq!(named[0].param, "vm.dirty_bytes");
        assert_eq!(named[0].cleared_by, None);
        assert_eq!(named[1].param, "vm.dirty_ratio");
        assert_eq!(
            named[1].cleared_by.as_deref(),
            Some("vm.dirty_bytes"),
            "the named twin's clearer: {named:?}"
        );
    }

    /// `rollback --list <param>...` lists only the entries the names resolve
    /// to — any spelling of a recorded parameter, each once — in the same
    /// param order the unfiltered `--list` uses, with the live reading every
    /// entry carries there.
    #[test]
    fn preview_params_lists_only_the_named_entries() {
        let dir = AtomicTestDir::new("preview-params-named");
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        let keep = dir.0.join("keep_me");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "1024").unwrap();
        fs::write(&keep, "2").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
                ("vm.keep_me", "1", "2", keep.to_str().unwrap()),
            ],
        );
        let ledger_before = fs::read_to_string(&ledger).unwrap();

        let params = [
            "vm.swappiness".to_string(),
            "net/core/somaxconn".to_string(),
            "vm/swappiness".to_string(),
        ];
        let entries = rollback_preview_params_at(&params, &ledger).unwrap();

        assert_eq!(
            entries,
            vec![
                RollbackPreview {
                    param: "net.core.somaxconn".to_string(),
                    applied: "4096".to_string(),
                    previous: "128".to_string(),
                    category: None,
                    live: Some("1024".to_string()),
                    drifted: Some(true),
                    cleared_by: None,
                },
                RollbackPreview {
                    param: "vm.swappiness".to_string(),
                    applied: "10".to_string(),
                    previous: "60".to_string(),
                    category: None,
                    live: Some("10".to_string()),
                    drifted: Some(false),
                    cleared_by: None,
                },
            ]
        );
        assert_eq!(
            fs::read_to_string(&ledger).unwrap(),
            ledger_before,
            "the preview is read-only"
        );
    }

    /// Naming one half of a mutually exclusive pair lists the recorded twin
    /// too, because `rollback <param>` restores the pair as one unit: the
    /// preview is the entry set that restore walks, not a subset of it.
    #[test]
    fn preview_params_lists_the_twin_the_restore_takes_along() {
        let dir = AtomicTestDir::new("preview-params-twin");
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&bytes, "1073741824").unwrap();
        fs::write(&ratio, "0").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );

        let params = ["vm.dirty_bytes".to_string()];
        let listed: Vec<String> = rollback_preview_params_at(&params, &ledger)
            .unwrap()
            .into_iter()
            .map(|entry| entry.param)
            .collect();
        assert_eq!(listed, ["vm.dirty_bytes", "vm.dirty_ratio"]);

        // The units the restore walks for the same names hold exactly those
        // entries.
        let data =
            parse_rollback_ledger_at(ROLLBACK_PATH, &fs::read_to_string(&ledger).unwrap()).unwrap();
        let keys = resolve_ledger_keys(&data.entries, &params).unwrap();
        let restored: Vec<String> = rollback_units(&data.entries, &keys)
            .into_iter()
            .flat_map(|unit| unit.into_keys())
            .collect();
        assert_eq!(restored, listed);
    }

    /// A name the ledger does not record is the restore's own command error,
    /// so `--list` cannot promise a rollback that would refuse; with no
    /// ledger every name misses, and the preview still creates nothing.
    #[test]
    fn preview_params_refuses_an_unrecorded_name() {
        let dir = AtomicTestDir::new("preview-params-unrecorded");
        let swappiness = dir.0.join("swappiness");
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[("vm.swappiness", "60", "10", swappiness.to_str().unwrap())],
        );

        let params = ["vm.swappiness".to_string(), "vm.typo".to_string()];
        let error = rollback_preview_params_at(&params, &ledger)
            .expect_err("an unrecorded name must refuse the preview");
        assert_eq!(
            error.to_string(),
            "parameter not recorded in the rollback ledger: vm.typo"
        );

        let absent = dir.0.join("nested").join("rollback.json");
        let error = rollback_preview_params_at(&params[..1], absent.to_str().unwrap())
            .expect_err("no ledger records no parameter");
        assert!(error.to_string().contains("not recorded"), "{error:#}");
        assert!(
            !absent.parent().unwrap().exists(),
            "preview must not create the ledger directory"
        );
        assert!(
            !std::path::Path::new(&format!("{}.lock", absent.display())).exists(),
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
        merge_rollback_at(
            path,
            [("vm.swappiness".into(), "60".into(), "10".into(), None)],
        )
        .unwrap();
        ARM_FINALIZE_RACE_PROBE.store(true, Ordering::SeqCst);
        let entries = rollback_preview_at(path)
            .expect("preview must survive a concurrent finalize's delete window");
        ARM_FINALIZE_RACE_PROBE.store(false, Ordering::SeqCst);
        // The recorded triple is the contract under test here; the live read
        // is not asserted (merge_rollback_at points the entry at the host's
        // /proc/sys path).
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].param, "vm.swappiness");
        assert_eq!(entries[0].applied, "10");
        assert_eq!(entries[0].previous, "60");
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
                    [(
                        "vm.audit_a".to_string(),
                        "60".to_string(),
                        "10".to_string(),
                        None,
                    )],
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
                        None,
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

    /// A non-regular file at the temporary's name keeps the refusal that
    /// exclusive creation exists for: the temporary must never be an inode
    /// another user chose, and a symlink there would redirect the private
    /// contents (and later the rename) to a file of the attacker's choosing.
    #[test]
    fn test_write_atomic_rejects_a_non_regular_temporary() {
        use std::os::unix::fs::symlink;

        let dir = AtomicTestDir::new("existing-nonregular");
        let target = dir.0.join("target");
        let path = target.to_str().unwrap();
        let tmp = format!("{path}.tmp.{}", std::process::id());
        let victim = dir.0.join("victim");
        fs::write(&target, b"original target").unwrap();
        fs::write(&victim, b"original temporary").unwrap();
        symlink(&victim, &tmp).unwrap();

        assert!(write_atomic(path, b"replacement", 0o644).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original target");
        assert_eq!(fs::read(&victim).unwrap(), b"original temporary");
        assert!(fs::symlink_metadata(&tmp).unwrap().file_type().is_symlink());
    }

    /// A regular file at the temporary's name is a leftover from a process
    /// killed between the create and the rename; with pid reuse it blocks every
    /// later save of that target forever. The name carries the writer's pid and
    /// same-process writers to one path are serialized by the ledger lock, so a
    /// file at *our* pid's name cannot be in flight and is reclaimed instead.
    #[test]
    fn test_write_atomic_reclaims_a_stale_regular_temporary() {
        let dir = AtomicTestDir::new("existing-regular");
        let target = dir.0.join("target");
        let path = target.to_str().unwrap();
        let tmp = format!("{path}.tmp.{}", std::process::id());
        fs::write(&target, b"original target").unwrap();
        fs::write(&tmp, b"leftover from a killed run").unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o666)).unwrap();

        write_atomic(path, b"replacement", 0o644)
            .expect("a stale leftover must not block the save");
        assert_eq!(fs::read(&target).unwrap(), b"replacement");
        assert!(
            !Path::new(&tmp).exists(),
            "the reclaimed temporary is renamed onto the target, not left behind"
        );
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644,
            "the published file carries the requested mode, not the leftover's"
        );
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
    fn test_classify_readback_exact_effective_keeps_the_canonical_format() {
        // The kernel renders multi-value sysctls TAB-separated
        // (proc_dointvec joins net.ipv4.tcp_rmem / kernel.sem fields with
        // \t), while every reading consumer publishes the single-space form:
        // read_sysctl_string for a recommendation's `current`, and — since
        // the `why` fallback fix — the same collapse there. `effective` is
        // what the ledger records as `applied`, what `fix` prints, what
        // `rollback --list` shows and what sysctl.d persists, so it must
        // hold the same canonical format instead of the raw tab line.
        assert_eq!(
            classify_readback("4096 87380 16777216", "4096\t87380\t16777216"),
            ReadbackVerdict::Verified {
                effective: "4096 87380 16777216".to_string()
            }
        );
    }

    #[test]
    fn test_classify_readback_clamped_effective_keeps_the_canonical_format() {
        // A clamped multi-value read-back (kernel.sem-style quadruple where
        // the kernel settled a field differently) is recorded the same way:
        // the ledger's `applied` and the persistence line must carry the
        // canonical single-space rendering, not the kernel's TAB separators.
        assert_eq!(
            classify_readback("250 32000 100 128", "250\t32000\t100\t999"),
            ReadbackVerdict::Clamped {
                effective: "250 32000 100 999".to_string()
            }
        );
    }

    #[test]
    fn test_read_previous_records_the_canonical_multi_value_format() {
        // `read_previous` is where the ledger's `previous` comes from (and
        // `fix` prints it verbatim, `rollback --list` republishes it). The
        // kernel renders net.ipv4.tcp_rmem TAB-separated, so without the
        // collapse the same knob's value flips format between `why`'s
        // `current` ("4096 131072 6291456") and the ledger's `previous`
        // ("4096\t131072\t6291456") — the same instability the `why`
        // fallback fix removed for its own two branches. Skipped on test
        // hosts without the file (non-Linux).
        let path = "/proc/sys/net/ipv4/tcp_rmem";
        if !std::path::Path::new(path).exists() {
            return;
        }
        let original = read_previous("net.ipv4.tcp_rmem").expect("tcp_rmem is readable");
        assert_eq!(
            original,
            original.split_whitespace().collect::<Vec<_>>().join(" "),
            "the ledger original must hold the canonical single-space form"
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
        assert_eq!(outcome.applied(), 0);
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
        merge_rollback_at(
            path,
            [("vm.swappiness".into(), "60".into(), "10".into(), None)],
        )
        .unwrap();
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
        merge_rollback_at(
            path,
            [("vm.swappiness".into(), "60".into(), "10".into(), None)],
        )
        .unwrap();
        merge_rollback_at(
            path,
            [
                ("vm.swappiness".into(), "10".into(), "20".into(), None),
                (
                    "net.core.somaxconn".into(),
                    "128".into(),
                    "256".into(),
                    None,
                ),
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
            let error = merge_rollback_at(
                path,
                [("vm.swappiness".into(), "10".into(), "20".into(), None)],
            )
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

    /// The same remedy, from every reader of the file. The ledger holds the
    /// only record of the originals, so the message an operator gets for a
    /// damaged one is the recovery they must not skip: repair it, or move it
    /// aside with its copy kept, then rerun. [`load_rollback_from`] answers
    /// that already; the rollback forms and the previews reported the parser's
    /// own text under `解析 rollback 文件失败` instead, naming neither the file
    /// nor the stakes — the answer most likely to be met with `rm`.
    #[test]
    fn a_corrupt_ledger_names_the_remedy_from_every_reader() {
        let dir = AtomicTestDir::new("ledger-remedy-readers");
        let ledger = dir.0.join("rollback.json");
        let path = ledger.to_str().unwrap();
        fs::write(&ledger, b"{").unwrap();
        let (conf, service, script) = fixture_paths(&dir);
        let params = ["vm.swappiness".to_string()];
        let errors = [
            rollback_params_at(&params, path, &conf, &service, &script)
                .err()
                .unwrap_or_else(|| panic!("restore of a corrupt ledger must not parse")),
            rollback_excluding_at(&[], path, &conf, &service, &script)
                .err()
                .unwrap_or_else(|| panic!("an exclusion over a corrupt ledger must not parse")),
            rollback_categories_at(&["mem".to_string()], &[], path, &conf, &service, &script)
                .err()
                .unwrap_or_else(|| {
                    panic!("a category selection over a corrupt ledger must not parse")
                }),
            rollback_preview_at(path)
                .err()
                .unwrap_or_else(|| panic!("the preview must not read a corrupt ledger")),
            rollback_preview_params_at(&params, path)
                .err()
                .unwrap_or_else(|| panic!("the narrowed preview must not read a corrupt ledger")),
        ];
        for error in errors {
            let message = error.to_string();
            assert!(message.contains("inspect and repair"), "{message}");
            assert!(
                message.contains(path),
                "the error must name the file it is about: {message}"
            );
        }
    }

    /// Write a rollback ledger whose recorded paths point into `dir`, for the
    /// single-parameter tests: the restores they drive must land on plain files
    /// instead of /proc/sys and /sys.
    fn fixture_ledger(dir: &AtomicTestDir, entries: &[(&str, &str, &str, &str)]) -> String {
        let mut map = serde_json::Map::new();
        for (param, previous, applied, path) in entries {
            map.insert(
                (*param).to_string(),
                serde_json::json!({ "previous": previous, "applied": applied, "path": path }),
            );
        }
        let ledger = dir.0.join("rollback.json");
        fs::write(
            &ledger,
            serde_json::json!({ "version": 1, "entries": map }).to_string(),
        )
        .unwrap();
        ledger.to_str().unwrap().to_string()
    }

    /// The generated-file paths `rollback_param_at` must be told, in the same
    /// order as `finalize_rollback_at`: (sysctl.d, unit, script).
    fn fixture_paths(dir: &AtomicTestDir) -> (String, String, String) {
        (
            dir.0.join("99-ktuner.conf").to_str().unwrap().to_string(),
            dir.0
                .join("ktuner-nonsysctl.service")
                .to_str()
                .unwrap()
                .to_string(),
            dir.0
                .join("apply-nonsysctl.sh")
                .to_str()
                .unwrap()
                .to_string(),
        )
    }

    /// `rollback <param>` restores only the named ledger entry: the rest stay
    /// live and recorded, and the persisted file is regenerated from what
    /// remains instead of re-applying the rolled-back value at the next boot.
    #[test]
    fn rollback_param_retires_one_entry_and_keeps_the_rest() {
        let dir = AtomicTestDir::new("rollback-param-one");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        fs::write(&conf, "vm.swappiness = 10\nnet.core.somaxconn = 4096\n").unwrap();

        // A slashed alias of the recorded key must reach the same entry, the
        // way fix/why accept it.
        let (key, outcome) =
            rollback_param_at("vm/swappiness", &ledger, &conf, &service, &script).unwrap();

        assert_eq!(key, "vm.swappiness");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (1, 0, 0)
        );
        assert!(outcome.is_complete());
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(
            fs::read_to_string(&somaxconn).unwrap(),
            "4096",
            "the untouched entry must stay live"
        );
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert!(
            data["entries"].get("vm.swappiness").is_none(),
            "the restored entry must leave the ledger: {data}"
        );
        assert_eq!(data["entries"]["net.core.somaxconn"]["previous"], "128");
        let persisted = fs::read_to_string(&conf).unwrap();
        assert!(
            !persisted.contains("vm.swappiness"),
            "the rolled-back value must leave the persisted file: {persisted}"
        );
        assert!(
            persisted.contains("net.core.somaxconn = 4096"),
            "{persisted}"
        );
    }

    #[test]
    fn rollback_param_keeps_originals_when_persistence_fails() {
        let dir = AtomicTestDir::new("rollback-param-persist-failure");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        let before = fs::read_to_string(&ledger).unwrap();
        // A directory at the generated-file path refuses atomic replacement,
        // even for root, without requiring any host mount or kernel writes.
        fs::create_dir(&conf).unwrap();
        assert!(rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script).is_err());
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(fs::read_to_string(&ledger).unwrap(), before);
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "4096");

        fs::remove_dir(&conf).unwrap();
        let (_, outcome) =
            rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script).unwrap();
        assert!(outcome.is_complete());
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert!(data["entries"].get("vm.swappiness").is_none());
        assert_eq!(data["entries"]["net.core.somaxconn"]["previous"], "128");
        let persisted = fs::read_to_string(&conf).unwrap();
        assert!(!persisted.contains("vm.swappiness"));
        assert!(persisted.contains("net.core.somaxconn = 4096"));
    }

    #[test]
    fn rollback_param_keeps_originals_after_partial_persistence() {
        let dir = AtomicTestDir::new("rollback-param-persist-partial");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        let before = fs::read_to_string(&ledger).unwrap();
        // Sysctl regeneration lands first; retiring a stale script then fails.
        fs::create_dir(&script).unwrap();
        fs::write(&service, "stale unit").unwrap();
        assert!(rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script).is_err());
        assert!(!fs::read_to_string(&conf).unwrap().contains("vm.swappiness"));
        assert!(!Path::new(&service).exists());
        assert_eq!(fs::read_to_string(&ledger).unwrap(), before);
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");

        fs::remove_dir(&script).unwrap();
        let (_, outcome) =
            rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script).unwrap();
        assert!(outcome.is_complete());
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert!(data["entries"].get("vm.swappiness").is_none());
        assert_eq!(data["entries"]["net.core.somaxconn"]["previous"], "128");
    }

    /// The same operation on the last recorded entry runs the full rollback's
    /// terminal cleanup: the persisted files first, then the ledger.
    #[test]
    fn rollback_param_empties_the_ledger_and_finalizes() {
        let dir = AtomicTestDir::new("rollback-param-empty");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[("vm.swappiness", "60", "10", swappiness.to_str().unwrap())],
        );
        fs::write(&conf, "vm.swappiness = 10\n").unwrap();

        let (key, outcome) =
            rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script).unwrap();

        assert_eq!(key, "vm.swappiness");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (1, 0, 0)
        );
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert!(
            !Path::new(&ledger).exists(),
            "an emptied ledger is deleted like a full rollback"
        );
        assert!(
            !Path::new(&conf).exists(),
            "the persisted file goes with it"
        );
    }

    /// A mutually exclusive pair is restored and retired as one unit whichever
    /// half is named: the kernel's write to either knob zeroes the other, so a
    /// half restore could not leave the ledger describing the live kernel.
    #[test]
    fn rollback_param_restores_the_recorded_twin() {
        for (target, label) in [
            ("vm.dirty_bytes", "rollback-param-twin-clearer"),
            ("vm.dirty_ratio", "rollback-param-twin-cleared"),
        ] {
            let dir = AtomicTestDir::new(label);
            let (conf, service, script) = fixture_paths(&dir);
            let bytes = dir.0.join("dirty_bytes");
            let ratio = dir.0.join("dirty_ratio");
            let somaxconn = dir.0.join("somaxconn");
            fs::write(&bytes, "1073741824").unwrap();
            fs::write(&ratio, "0").unwrap();
            fs::write(&somaxconn, "4096").unwrap();
            let ledger = fixture_ledger(
                &dir,
                &[
                    ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                    ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
                    (
                        "net.core.somaxconn",
                        "128",
                        "4096",
                        somaxconn.to_str().unwrap(),
                    ),
                ],
            );
            fs::write(
                &conf,
                "vm.dirty_bytes = 1073741824\nnet.core.somaxconn = 4096\n",
            )
            .unwrap();

            let (key, outcome) =
                rollback_param_at(target, &ledger, &conf, &service, &script).unwrap();

            assert_eq!(key, target, "the resolved key is the recorded spelling");
            assert_eq!(
                (outcome.restored(), outcome.failed(), outcome.skipped()),
                (2, 0, 0),
                "{target}: the recorded twin is restored and counted"
            );
            // The selection is expanded inside the engine, so the entry the
            // caller named is not the whole set it retired: the twin's own
            // ledger key has to be named, in the order the batch landed the
            // two writes (a clearer before the twin its write clears).
            assert_eq!(
                outcome
                    .restored_params
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                vec!["vm.dirty_bytes", "vm.dirty_ratio"],
                "{target}: the restored twin is named, not only counted"
            );
            assert_eq!(fs::read_to_string(&bytes).unwrap(), "0", "{target}");
            assert_eq!(
                fs::read_to_string(&ratio).unwrap(),
                "20",
                "{target}: the kernel-cleared twin comes back"
            );
            assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "4096", "{target}");
            let data: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
            assert!(
                data["entries"].get("vm.dirty_bytes").is_none(),
                "{target}: {data}"
            );
            assert!(
                data["entries"].get("vm.dirty_ratio").is_none(),
                "{target}: both halves are retired together: {data}"
            );
            assert_eq!(
                data["entries"]["net.core.somaxconn"]["applied"], "4096",
                "{target}"
            );
            let persisted = fs::read_to_string(&conf).unwrap();
            assert!(
                !persisted.contains("vm.dirty_bytes"),
                "{target}: {persisted}"
            );
            assert!(
                !persisted.contains("vm.dirty_ratio"),
                "{target}: {persisted}"
            );
            assert!(
                persisted.contains("net.core.somaxconn = 4096"),
                "{target}: {persisted}"
            );
        }
    }

    /// The twin can be recorded under the spelling an import used: merge keeps
    /// the first key it saw, so the cleared twin may sit under a slashed key
    /// while its clearer is dotted. The pair lookup must still find it.
    #[test]
    fn rollback_param_retires_a_cleared_twin_under_the_importers_spelling() {
        let dir = AtomicTestDir::new("rollback-param-twin-slash");
        let (conf, service, script) = fixture_paths(&dir);
        let kbytes = dir.0.join("overcommit_kbytes");
        let ratio = dir.0.join("overcommit_ratio");
        fs::write(&kbytes, "0").unwrap();
        fs::write(&ratio, "100").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "vm/overcommit_kbytes",
                    "4194304",
                    "0",
                    kbytes.to_str().unwrap(),
                ),
                ("vm.overcommit_ratio", "50", "100", ratio.to_str().unwrap()),
            ],
        );
        fs::write(&conf, "vm.overcommit_ratio = 100\n").unwrap();

        let (key, outcome) =
            rollback_param_at("vm.overcommit_ratio", &ledger, &conf, &service, &script).unwrap();

        assert_eq!(key, "vm.overcommit_ratio");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0)
        );
        assert_eq!(fs::read_to_string(&ratio).unwrap(), "50");
        assert_eq!(
            fs::read_to_string(&kbytes).unwrap(),
            "4194304",
            "the twin under the imported slash key must be restored too"
        );
        assert!(
            !Path::new(&ledger).exists(),
            "both halves retired, so the emptied ledger finalizes"
        );
    }

    /// A target whose path is gone is a skipped restore: the record survives,
    /// the persisted file is untouched, and the caller sees an incomplete
    /// outcome (exit 1 in the CLI).
    #[test]
    fn rollback_param_keeps_the_record_when_the_target_path_is_gone() {
        let dir = AtomicTestDir::new("rollback-param-missing");
        let (conf, service, script) = fixture_paths(&dir);
        let absent = dir.0.join("absent");
        let ledger = fixture_ledger(
            &dir,
            &[("vm.swappiness", "60", "10", absent.to_str().unwrap())],
        );
        fs::write(&conf, "vm.swappiness = 10\n").unwrap();

        let (key, outcome) =
            rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script).unwrap();

        assert_eq!(key, "vm.swappiness");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (0, 0, 1)
        );
        assert!(
            !outcome.is_complete(),
            "a skipped path is not a completed rollback"
        );
        assert!(Path::new(&ledger).exists(), "the record must survive");
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert_eq!(data["entries"]["vm.swappiness"]["previous"], "60");
        assert_eq!(fs::read_to_string(&conf).unwrap(), "vm.swappiness = 10\n");
    }

    /// The outcome names what a batch left behind, not just how many entries
    /// did. One entry is addressed at a directory, which `fs::write` refuses
    /// with `EISDIR` on every filesystem; the next one's recorded path is
    /// absent. A caller that exits 1 has to be able to name both, without a
    /// second root-only `--list` run to find out which parameters are still
    /// tuned.
    #[test]
    fn rollback_params_name_the_entries_they_left() {
        let dir = AtomicTestDir::new("rollback-param-leftovers");
        let (conf, service, script) = fixture_paths(&dir);
        let blocked = dir.0.join("blocked");
        fs::create_dir(&blocked).unwrap();
        let absent = dir.0.join("absent");
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", blocked.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "4096",
                    "128",
                    absent.to_str().unwrap(),
                ),
            ],
        );
        fs::write(&conf, "vm.swappiness = 10\n").unwrap();

        let (keys, outcome) = rollback_params_at(
            &[
                "vm.swappiness".to_string(),
                "net.core.somaxconn".to_string(),
            ],
            &ledger,
            &conf,
            &service,
            &script,
        )
        .unwrap();

        assert_eq!(keys, ["vm.swappiness", "net.core.somaxconn"]);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (0, 1, 1)
        );
        assert_eq!(
            outcome
                .failures
                .iter()
                .map(|failure| failure.param.as_str())
                .collect::<Vec<_>>(),
            ["vm.swappiness"],
            "the entry the write refused is named"
        );
        assert!(
            !outcome.failures[0].error.is_empty(),
            "and it carries the reason the write did not land"
        );
        assert_eq!(
            outcome.missing,
            ["net.core.somaxconn"],
            "the entry whose recorded path is gone is named too"
        );
        assert!(
            !outcome.is_complete(),
            "neither entry restored, so the ledger must be kept for a retry"
        );
        assert!(Path::new(&ledger).exists(), "the record must survive");
    }

    /// A pair restores as one unit: one half landing while the other is skipped
    /// keeps BOTH records so a retry can finish the pair. Retiring the landed
    /// half would let the next tune/fix regenerate persistence from the ledger
    /// and put the rolled-back value back on the boot path.
    #[test]
    fn rollback_param_keeps_the_whole_pair_when_one_half_fails() {
        let dir = AtomicTestDir::new("rollback-param-pair-half");
        let (conf, service, script) = fixture_paths(&dir);
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("absent_ratio");
        fs::write(&bytes, "1073741824").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
            ],
        );
        let conf_before = "vm.dirty_bytes = 1073741824\n";
        fs::write(&conf, conf_before).unwrap();

        let (key, outcome) =
            rollback_param_at("vm.dirty_bytes", &ledger, &conf, &service, &script).unwrap();

        assert_eq!(key, "vm.dirty_bytes");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (1, 0, 1)
        );
        assert_eq!(
            fs::read_to_string(&bytes).unwrap(),
            "0",
            "the half that could land did"
        );
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert_eq!(data["entries"]["vm.dirty_bytes"]["previous"], "0", "{data}");
        assert_eq!(
            data["entries"]["vm.dirty_ratio"]["previous"], "20",
            "the half that did not land keeps its record for a retry: {data}"
        );
        assert_eq!(
            fs::read_to_string(&conf).unwrap(),
            conf_before,
            "an incomplete unit must leave persistence untouched"
        );
    }

    /// A parameter the ledger does not record is a command error: nothing to
    /// restore, nothing to retry, and never a success. The ledger and the
    /// persisted file stay untouched — no empty ledger is invented.
    #[test]
    fn rollback_param_errors_when_not_recorded() {
        let dir = AtomicTestDir::new("rollback-param-not-recorded");
        let (conf, service, script) = fixture_paths(&dir);
        let other = dir.0.join("somaxconn");
        fs::write(&other, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[("net.core.somaxconn", "128", "4096", other.to_str().unwrap())],
        );
        let before = fs::read_to_string(&ledger).unwrap();

        let error = rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script)
            .err()
            .expect("an unrecorded parameter must not answer with an outcome");
        assert!(error.to_string().contains("not recorded"), "{error:#}");
        assert!(error.to_string().contains("vm.swappiness"), "{error:#}");
        assert_eq!(
            fs::read_to_string(&ledger).unwrap(),
            before,
            "a lookup miss must not rewrite the ledger"
        );
        assert!(!Path::new(&conf).exists());

        // An empty ledger is the same miss; a missing ledger keeps the full
        // rollback's command error.
        let empty = dir.0.join("empty.json");
        fs::write(&empty, r#"{"version":1,"entries":{}}"#).unwrap();
        assert!(rollback_param_at(
            "vm.swappiness",
            empty.to_str().unwrap(),
            &conf,
            &service,
            &script
        )
        .is_err());
        let missing = dir.0.join("missing.json");
        let error = rollback_param_at(
            "vm.swappiness",
            missing.to_str().unwrap(),
            &conf,
            &service,
            &script,
        )
        .err()
        .expect("a missing ledger is a command error");
        assert!(
            error.to_string().contains("没有找到 rollback 文件"),
            "{error:#}"
        );
        assert!(!missing.exists(), "the miss must not create a ledger");
    }

    /// The ledger lookup accepts the same spellings fix/why accept, and keeps
    /// per-interface identity case-sensitive with its literal dots.
    #[test]
    fn rollback_param_lookup_accepts_the_fix_and_why_aliases() {
        let entries = BTreeMap::from([
            (
                "vm.swappiness".to_string(),
                RollbackEntry {
                    previous: "60".into(),
                    applied: "10".into(),
                    path: "/proc/sys/vm/swappiness".into(),
                    category: None,
                },
            ),
            (
                "vm/overcommit_ratio".to_string(),
                RollbackEntry {
                    previous: "50".into(),
                    applied: "100".into(),
                    path: "/proc/sys/vm/overcommit_ratio".into(),
                    category: None,
                },
            ),
            (
                "net.ipv4.conf.Br0.100.forwarding".to_string(),
                RollbackEntry {
                    previous: "0".into(),
                    applied: "1".into(),
                    path: "/proc/sys/net/ipv4/conf/Br0.100/forwarding".into(),
                    category: None,
                },
            ),
            (
                "block/sda/scheduler".to_string(),
                RollbackEntry {
                    previous: "mq-deadline".into(),
                    applied: "none".into(),
                    path: "/sys/block/sda/queue/scheduler".into(),
                    category: None,
                },
            ),
        ]);
        assert_eq!(
            ledger_key_for(&entries, "vm.swappiness").as_deref(),
            Some("vm.swappiness")
        );
        assert_eq!(
            ledger_key_for(&entries, "vm/swappiness").as_deref(),
            Some("vm.swappiness"),
            "the slashed alias reaches the dotted key"
        );
        assert_eq!(
            ledger_key_for(&entries, "vm.overcommit_ratio").as_deref(),
            Some("vm/overcommit_ratio"),
            "the dotted alias reaches the slashed key"
        );
        assert_eq!(
            ledger_key_for(&entries, "net/ipv4/conf/Br0.100/forwarding").as_deref(),
            Some("net.ipv4.conf.Br0.100.forwarding")
        );
        assert_eq!(
            ledger_key_for(&entries, "net.ipv4.conf.br0.100.forwarding"),
            None,
            "interface identity stays case-sensitive"
        );
        assert_eq!(
            ledger_key_for(&entries, "block/sda/scheduler").as_deref(),
            Some("block/sda/scheduler")
        );
        assert_eq!(ledger_key_for(&entries, "vm.nothing_here"), None);
        assert_eq!(
            paired_ledger_key(&entries, "vm.overcommit_ratio"),
            None,
            "no twin is recorded for the pair"
        );
    }

    /// The whole operation runs under the ledger's exclusive lock: while
    /// another holder keeps it, the parameter write must not happen, and the
    /// restore completes once the lock is released.
    #[test]
    fn rollback_param_waits_for_the_ledger_lock() {
        use std::sync::{Arc, Barrier};

        let dir = AtomicTestDir::new("rollback-param-lock");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[("vm.swappiness", "60", "10", swappiness.to_str().unwrap())],
        );
        let guard = lock_ledger_at(&ledger).unwrap();

        let ready = Arc::new(Barrier::new(2));
        let worker_ready = ready.clone();
        let worker = std::thread::spawn(move || {
            worker_ready.wait();
            rollback_param_at("vm.swappiness", &ledger, &conf, &service, &script)
        });
        ready.wait();
        // 200 ms is the window transactions.rs uses for the same assertion.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            fs::read_to_string(&swappiness).unwrap(),
            "10",
            "the write escaped the ledger lock"
        );

        drop(guard);
        let (key, outcome) = worker.join().unwrap().unwrap();
        assert_eq!(key, "vm.swappiness");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (1, 0, 0)
        );
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
    }

    /// Ledger rewrites and persistence regenerations, as (kind, ledger path)
    /// pairs. A multi-parameter rollback is ONE transaction — one lock, one
    /// ledger rewrite, one persistence regeneration — and that is what its
    /// tests assert: the writes are counted here rather than inferred from
    /// their end state, because two writes leave the same bytes a single one
    /// does. Filled from `#[cfg(test)]` probes in `save_ledger_at` and
    /// `persist_from_rollback_at`, the way `finalize_race_probe` stands in for
    /// its race window; nothing of this is compiled into a release build.
    static BATCH_WRITES: std::sync::Mutex<Vec<(&'static str, String)>> =
        std::sync::Mutex::new(Vec::new());

    pub(super) fn record_batch_write(kind: &'static str, ledger: &str) {
        if let Ok(mut writes) = BATCH_WRITES.lock() {
            writes.push((kind, ledger.to_string()));
        }
    }

    /// How many `kind` writes targeted `ledger`. Both are unique per fixture —
    /// `AtomicTestDir` embeds the process id and the test label — so a
    /// parallel test's writes can never be counted here.
    fn batch_writes(kind: &str, ledger: &str) -> usize {
        BATCH_WRITES
            .lock()
            .map(|writes| {
                writes
                    .iter()
                    .filter(|(recorded, path)| *recorded == kind && path == ledger)
                    .count()
            })
            .unwrap_or(0)
    }

    /// One batch restores every named entry, keeps the unnamed ones, and
    /// rewrites the ledger and the persisted file once from what remains: the
    /// host is undone in a single run instead of one command per parameter.
    #[test]
    fn rollback_params_restores_every_named_entry() {
        let dir = AtomicTestDir::new("rollback-params-three");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        let cache_pressure = dir.0.join("vfs_cache_pressure");
        let keep = dir.0.join("keep_me");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&cache_pressure, "50").unwrap();
        fs::write(&keep, "2").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "vm.vfs_cache_pressure",
                    "100",
                    "50",
                    cache_pressure.to_str().unwrap(),
                ),
                ("vm.keep_me", "1", "2", keep.to_str().unwrap()),
            ],
        );
        fs::write(
            &conf,
            "net.core.somaxconn = 4096\nvm.swappiness = 10\nvm.vfs_cache_pressure = 50\n",
        )
        .unwrap();

        // A slashed spelling of one name, in the order the caller wrote them.
        let params = [
            "vm.swappiness".to_string(),
            "net/core/somaxconn".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        let (keys, outcome) =
            rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(
            keys,
            [
                "vm.swappiness".to_string(),
                "net.core.somaxconn".to_string(),
                "vm.vfs_cache_pressure".to_string()
            ],
            "each positional reports the ledger key it resolved to, in the order given"
        );
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (3, 0, 0)
        );
        assert!(outcome.is_complete());
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
        assert_eq!(fs::read_to_string(&cache_pressure).unwrap(), "100");
        assert_eq!(
            fs::read_to_string(&keep).unwrap(),
            "2",
            "an entry nobody named stays live"
        );
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert_eq!(data["entries"].as_object().unwrap().len(), 1, "{data}");
        assert!(data["entries"].get("vm.keep_me").is_some(), "{data}");
        let persisted = fs::read_to_string(&conf).unwrap();
        for gone in [
            "vm.swappiness",
            "vm.vfs_cache_pressure",
            "net.core.somaxconn",
        ] {
            assert!(
                !persisted.contains(gone),
                "{gone} must leave the persisted file: {persisted}"
            );
        }
        assert!(persisted.contains("vm.keep_me = 2"), "{persisted}");
    }

    /// Naming both halves of a mutually exclusive pair restores the pair once:
    /// each name reports its own ledger key, but the unit — the named entry
    /// plus its recorded twin — is not restored or counted twice.
    #[test]
    fn rollback_params_dedupes_a_twin_pair() {
        let dir = AtomicTestDir::new("rollback-params-twins");
        let (conf, service, script) = fixture_paths(&dir);
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&bytes, "1073741824").unwrap();
        fs::write(&ratio, "0").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        fs::write(
            &conf,
            "vm.dirty_bytes = 1073741824\nnet.core.somaxconn = 4096\n",
        )
        .unwrap();

        let params = ["vm.dirty_bytes".to_string(), "vm.dirty_ratio".to_string()];
        let (keys, outcome) =
            rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(keys, params, "each named half reports its ledger key");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0),
            "both halves of the pair are restored once, not four times"
        );
        assert_eq!(fs::read_to_string(&bytes).unwrap(), "0");
        assert_eq!(fs::read_to_string(&ratio).unwrap(), "20");
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert_eq!(data["entries"].as_object().unwrap().len(), 1, "{data}");
        assert!(data["entries"].get("vm.dirty_bytes").is_none(), "{data}");
        assert!(data["entries"].get("vm.dirty_ratio").is_none(), "{data}");
        let persisted = fs::read_to_string(&conf).unwrap();
        assert!(!persisted.contains("vm.dirty"), "{persisted}");
    }

    /// Two spellings of one kernel path are one restore: the second adds a
    /// name, not a second pass over the same entry, so `--list`'s key appears
    /// in the result once while the pair of spellings is still accepted.
    #[test]
    fn rollback_params_dedupes_alias_spellings() {
        let dir = AtomicTestDir::new("rollback-params-alias");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let forwarding = dir.0.join("forwarding");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&forwarding, "1").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.ipv4.conf.Br0.100.forwarding",
                    "0",
                    "1",
                    forwarding.to_str().unwrap(),
                ),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        fs::write(
            &conf,
            "net.ipv4.conf.Br0.100.forwarding = 1\nvm.swappiness = 10\n",
        )
        .unwrap();

        // Per-interface identity keeps its literal dots and case, so both
        // spellings of the VLAN interface address one entry.
        let params = [
            "vm/swappiness".to_string(),
            "vm.swappiness".to_string(),
            "net/ipv4/conf/Br0.100/forwarding".to_string(),
            "net.ipv4.conf.Br0.100.forwarding".to_string(),
        ];
        let (keys, outcome) =
            rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(
            keys,
            [
                "vm.swappiness".to_string(),
                "net.ipv4.conf.Br0.100.forwarding".to_string()
            ],
            "one ledger key per entry, in the order the spellings first appear"
        );
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0),
            "an aliased spelling must not restore the entry twice"
        );
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(fs::read_to_string(&forwarding).unwrap(), "0");
    }

    /// A name the ledger does not record refuses the whole command before
    /// anything is touched — the same command error a single parameter gets.
    /// Skipping the miss instead would let one typo drop the rest of the batch
    /// while the run still exited 0.
    #[test]
    fn rollback_params_refuses_an_unrecorded_name_before_touching_anything() {
        let dir = AtomicTestDir::new("rollback-params-unrecorded");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        let conf_before = "vm.swappiness = 10\nnet.core.somaxconn = 4096\n";
        fs::write(&conf, conf_before).unwrap();
        let ledger_before = fs::read_to_string(&ledger).unwrap();

        let params = ["vm.swappiness".to_string(), "vm.typo".to_string()];
        let error = rollback_params_at(&params, &ledger, &conf, &service, &script)
            .err()
            .expect("an unrecorded name must refuse the command");
        assert!(error.to_string().contains("not recorded"), "{error:#}");
        assert!(error.to_string().contains("vm.typo"), "{error:#}");
        assert_eq!(
            fs::read_to_string(&swappiness).unwrap(),
            "10",
            "the recorded name in the same batch must not be restored either"
        );
        assert_eq!(fs::read_to_string(&ledger).unwrap(), ledger_before);
        assert_eq!(fs::read_to_string(&conf).unwrap(), conf_before);

        // A missing ledger keeps the full rollback's command error, and an
        // empty one is the same lookup miss.
        let empty = dir.0.join("empty.json");
        fs::write(&empty, r#"{"version":1,"entries":{}}"#).unwrap();
        assert!(rollback_params_at(
            &["vm.swappiness".to_string()],
            empty.to_str().unwrap(),
            &conf,
            &service,
            &script
        )
        .is_err());
        let missing = dir.0.join("missing.json");
        let error =
            rollback_params_at(&params, missing.to_str().unwrap(), &conf, &service, &script)
                .err()
                .expect("a missing ledger is a command error");
        assert!(
            error.to_string().contains("没有找到 rollback 文件"),
            "{error:#}"
        );
        assert!(!missing.exists(), "the refusal must not create a ledger");
    }

    /// A degenerate spelling addresses no ledger entry, even though the
    /// kernel would collapse it onto a recorded path: `why`, `check --only`,
    /// `fix` and `tune --exclude` all treat it as an unknown name, and
    /// [`is_safe_param`] defines it as invalid. Every rollback form resolves
    /// names through [`match_ledger_keys`], so a positional refuses the
    /// command and an `--exclude` value reports it unmatched.
    #[test]
    fn rollback_params_refuses_a_degenerate_spelling() {
        let dir = AtomicTestDir::new("rollback-params-degenerate");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[("vm.swappiness", "60", "10", swappiness.to_str().unwrap())],
        );
        let conf_before = "vm.swappiness = 10\n";
        fs::write(&conf, conf_before).unwrap();
        let ledger_before = fs::read_to_string(&ledger).unwrap();

        for spelling in [
            "vm..swappiness",
            "vm//swappiness",
            "vm.swappiness.",
            "/vm.swappiness",
            "kernel/../vm/swappiness",
        ] {
            let error =
                rollback_params_at(&[spelling.to_string()], &ledger, &conf, &service, &script)
                    .err()
                    .unwrap_or_else(|| panic!("{spelling} must refuse the command"));
            assert!(error.to_string().contains("not recorded"), "{error:#}");
            assert_eq!(fs::read_to_string(&swappiness).unwrap(), "10", "{spelling}");
            assert_eq!(fs::read_to_string(&ledger).unwrap(), ledger_before);
            assert_eq!(fs::read_to_string(&conf).unwrap(), conf_before);

            let data: RollbackData = serde_json::from_str(&ledger_before).unwrap();
            let (keys, unmatched) = match_ledger_keys(&data.entries, &[spelling.to_string()]);
            assert!(keys.is_empty(), "{spelling} matched {keys:?}");
            assert_eq!(unmatched, [spelling.to_string()]);
        }
    }

    /// A batch where one unit lands and another does not retires only the unit
    /// that restored: the landed entry leaves the ledger and the persisted
    /// file, the failed and skipped ones keep their records for a retry, and
    /// the caller sees an incomplete outcome (exit 1 in the CLI).
    #[test]
    fn rollback_params_keeps_the_failed_unit_and_retires_the_rest() {
        let dir = AtomicTestDir::new("rollback-params-mixed");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let unwritable = dir.0.join("unwritable");
        let absent = dir.0.join("absent");
        fs::write(&swappiness, "10").unwrap();
        fs::create_dir(&unwritable).unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    unwritable.to_str().unwrap(),
                ),
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "vm.vfs_cache_pressure",
                    "100",
                    "50",
                    absent.to_str().unwrap(),
                ),
            ],
        );
        fs::write(
            &conf,
            "net.core.somaxconn = 4096\nvm.swappiness = 10\nvm.vfs_cache_pressure = 50\n",
        )
        .unwrap();

        let params = [
            "vm.swappiness".to_string(),
            "net.core.somaxconn".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        let (keys, outcome) =
            rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(keys, params);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (1, 1, 1)
        );
        assert!(
            !outcome.is_complete(),
            "a batch that did not finish must not report a completed rollback"
        );
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert!(data["entries"].get("vm.swappiness").is_none(), "{data}");
        assert_eq!(
            data["entries"]["net.core.somaxconn"]["previous"], "128",
            "the failed unit keeps its record: {data}"
        );
        assert_eq!(
            data["entries"]["vm.vfs_cache_pressure"]["previous"], "100",
            "the skipped unit keeps its record: {data}"
        );
        let persisted = fs::read_to_string(&conf).unwrap();
        assert!(
            !persisted.contains("vm.swappiness"),
            "the restored entry must leave the persisted file: {persisted}"
        );
        assert!(
            persisted.contains("net.core.somaxconn = 4096")
                && persisted.contains("vm.vfs_cache_pressure = 50"),
            "the unrestored entries stay on the boot path: {persisted}"
        );
    }

    /// A batch where nothing lands touches neither the ledger nor the
    /// persisted file: no half-batch state is ever written, exactly as a
    /// single parameter leaves both alone when its unit does not restore.
    #[test]
    fn rollback_params_keeps_every_record_when_nothing_lands() {
        let dir = AtomicTestDir::new("rollback-params-nothing");
        let (conf, service, script) = fixture_paths(&dir);
        let absent = dir.0.join("absent");
        let also_absent = dir.0.join("also_absent");
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", absent.to_str().unwrap()),
                (
                    "vm.vfs_cache_pressure",
                    "100",
                    "50",
                    also_absent.to_str().unwrap(),
                ),
            ],
        );
        let conf_before = "vm.swappiness = 10\nvm.vfs_cache_pressure = 50\n";
        fs::write(&conf, conf_before).unwrap();
        let ledger_before = fs::read_to_string(&ledger).unwrap();

        let params = [
            "vm.swappiness".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        let (keys, outcome) =
            rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(keys, params);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (0, 0, 2)
        );
        assert_eq!(fs::read_to_string(&ledger).unwrap(), ledger_before);
        assert_eq!(fs::read_to_string(&conf).unwrap(), conf_before);
    }

    /// Naming the last entries empties the ledger, so the batch runs the same
    /// terminal cleanup a full rollback runs: the persisted file first, then
    /// the ledger.
    #[test]
    fn rollback_params_empties_the_ledger_and_finalizes() {
        let dir = AtomicTestDir::new("rollback-params-empty");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        fs::write(&conf, "vm.swappiness = 10\nnet.core.somaxconn = 4096\n").unwrap();

        let params = [
            "vm.swappiness".to_string(),
            "net.core.somaxconn".to_string(),
        ];
        let (keys, outcome) =
            rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(keys, params);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0)
        );
        assert!(outcome.is_complete());
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
        assert!(
            !Path::new(&ledger).exists(),
            "an emptied ledger is deleted like a full rollback"
        );
        assert!(
            !Path::new(&conf).exists(),
            "the persisted file goes with it"
        );
    }

    /// The whole batch sits inside one ledger lock: while another holder keeps
    /// it, no target of any parameter is written, and the batch completes only
    /// once the lock is released. A per-parameter loop (one call to
    /// `rollback_param` each) would take the lock once per parameter and could
    /// be interleaved with a concurrent tune/fix between them — the half-batch
    /// state this feature exists to prevent.
    #[test]
    fn rollback_params_holds_the_ledger_lock_for_the_whole_batch() {
        use std::sync::{Arc, Barrier};

        let dir = AtomicTestDir::new("rollback-params-lock");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        let cache_pressure = dir.0.join("vfs_cache_pressure");
        for (path, value) in [
            (&swappiness, "10"),
            (&somaxconn, "4096"),
            (&cache_pressure, "50"),
        ] {
            fs::write(path, value).unwrap();
        }
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "vm.vfs_cache_pressure",
                    "100",
                    "50",
                    cache_pressure.to_str().unwrap(),
                ),
            ],
        );
        let conf_before = "vm.swappiness = 10\n";
        fs::write(&conf, conf_before).unwrap();
        let guard = lock_ledger_at(&ledger).unwrap();

        let ready = Arc::new(Barrier::new(2));
        let worker_ready = ready.clone();
        let params = [
            "vm.swappiness".to_string(),
            "net.core.somaxconn".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        // The worker owns its copies: this thread keeps reading the fixture
        // and the persisted file while the batch blocks on the lock.
        let (worker_conf, worker_service, worker_script) =
            (conf.clone(), service.clone(), script.clone());
        let worker = std::thread::spawn(move || {
            worker_ready.wait();
            rollback_params_at(
                &params,
                &ledger,
                &worker_conf,
                &worker_service,
                &worker_script,
            )
        });
        ready.wait();
        // 200 ms is the window transactions.rs and the single-parameter lock
        // test use for the same assertion.
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            fs::read_to_string(&swappiness).unwrap(),
            "10",
            "a write escaped the ledger lock"
        );
        assert_eq!(
            fs::read_to_string(&somaxconn).unwrap(),
            "4096",
            "the second parameter of the batch must wait for the same lock"
        );
        assert_eq!(fs::read_to_string(&cache_pressure).unwrap(), "50");
        assert_eq!(
            fs::read_to_string(&conf).unwrap(),
            conf_before,
            "persistence must not be regenerated outside the lock"
        );

        drop(guard);
        let (keys, outcome) = worker.join().unwrap().unwrap();
        assert_eq!(keys.len(), 3);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (3, 0, 0)
        );
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
        assert_eq!(fs::read_to_string(&cache_pressure).unwrap(), "100");
    }

    /// The batch is one transaction: the ledger is rewritten once and the
    /// persisted file regenerated once, from the entries that remain — not
    /// once per parameter. Two writes would leave the same final bytes, so the
    /// count itself is the assertion.
    #[test]
    fn rollback_params_writes_the_ledger_and_persistence_once() {
        let dir = AtomicTestDir::new("rollback-params-once");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let unwritable = dir.0.join("unwritable");
        let absent = dir.0.join("absent");
        let surviver = dir.0.join("surviver");
        fs::write(&swappiness, "10").unwrap();
        fs::create_dir(&unwritable).unwrap();
        fs::write(&surviver, "2").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    unwritable.to_str().unwrap(),
                ),
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "vm.vfs_cache_pressure",
                    "100",
                    "50",
                    absent.to_str().unwrap(),
                ),
                ("vm.keep_me", "1", "2", surviver.to_str().unwrap()),
            ],
        );
        fs::write(&conf, "net.core.somaxconn = 4096\nvm.swappiness = 10\n").unwrap();

        let params = [
            "vm.swappiness".to_string(),
            "net.core.somaxconn".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        let (_, outcome) = rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (1, 1, 1)
        );
        assert_eq!(
            batch_writes("ledger", &ledger),
            1,
            "a mixed batch rewrites the ledger once, not once per parameter"
        );
        assert_eq!(
            batch_writes("persist", &ledger),
            1,
            "a mixed batch regenerates persistence once, from the surviving entries"
        );

        // A batch that restores everything (and leaves a record behind, so the
        // finalize path is not the one under test) is the same one write.
        let dir = AtomicTestDir::new("rollback-params-once-all");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        let surviver = dir.0.join("surviver");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&surviver, "2").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                ("vm.keep_me", "1", "2", surviver.to_str().unwrap()),
            ],
        );
        fs::write(&conf, "vm.swappiness = 10\nnet.core.somaxconn = 4096\n").unwrap();
        let params = [
            "vm.swappiness".to_string(),
            "net.core.somaxconn".to_string(),
        ];
        let (_, outcome) = rollback_params_at(&params, &ledger, &conf, &service, &script).unwrap();

        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0)
        );
        assert_eq!(batch_writes("ledger", &ledger), 1);
        assert_eq!(batch_writes("persist", &ledger), 1);

        // Nothing lands: nothing is written at all.
        let dir = AtomicTestDir::new("rollback-params-once-none");
        let (conf, service, script) = fixture_paths(&dir);
        let absent = dir.0.join("absent");
        let ledger = fixture_ledger(
            &dir,
            &[("vm.swappiness", "60", "10", absent.to_str().unwrap())],
        );
        let (_, outcome) = rollback_params_at(
            &["vm.swappiness".to_string()],
            &ledger,
            &conf,
            &service,
            &script,
        )
        .unwrap();
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (0, 0, 1)
        );
        assert_eq!(batch_writes("ledger", &ledger), 0);
        assert_eq!(batch_writes("persist", &ledger), 0);
    }

    /// `rollback --exclude` restores every recorded entry the names do not
    /// address: each one back to its original, retired from the ledger and
    /// gone from the regenerated persisted file, while the excluded entry
    /// stays live and recorded. A slashed spelling addresses the same record
    /// the CLI normalizes its arguments to.
    #[test]
    fn rollback_excluding_restores_every_entry_the_names_do_not_address() {
        let dir = AtomicTestDir::new("rollback-excluding-three");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        let cache_pressure = dir.0.join("vfs_cache_pressure");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&cache_pressure, "50").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "vm.vfs_cache_pressure",
                    "100",
                    "50",
                    cache_pressure.to_str().unwrap(),
                ),
            ],
        );
        fs::write(
            &conf,
            "net.core.somaxconn = 4096\nvm.swappiness = 10\nvm.vfs_cache_pressure = 50\n",
        )
        .unwrap();

        let (unmatched, outcome) = rollback_excluding_at(
            &["vm/swappiness".to_string()],
            &ledger,
            &conf,
            &service,
            &script,
        )
        .unwrap();

        assert!(unmatched.is_empty(), "{unmatched:?}");
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0)
        );
        assert!(outcome.is_complete());
        assert_eq!(
            fs::read_to_string(&swappiness).unwrap(),
            "10",
            "the excluded entry is never written"
        );
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
        assert_eq!(fs::read_to_string(&cache_pressure).unwrap(), "100");
        let data: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
        assert_eq!(data["entries"].as_object().unwrap().len(), 1, "{data}");
        assert!(data["entries"].get("vm.swappiness").is_some(), "{data}");
        let persisted = fs::read_to_string(&conf).unwrap();
        assert!(persisted.contains("vm.swappiness = 10"), "{persisted}");
        for gone in ["net.core.somaxconn", "vm.vfs_cache_pressure"] {
            assert!(!persisted.contains(gone), "{gone} must leave {persisted}");
        }
    }

    /// Excluding either half of a recorded mutually exclusive pair keeps the
    /// whole unit in place: the kernel zeroes either knob when the other is
    /// written (v6.6 mm/page-writeback.c dirty_ratio_handler and
    /// dirty_bytes_handler), so restoring the free half would zero the
    /// excluded entry and leave the ledger describing a kernel it does not
    /// match. Neither half is written or retired; the entries outside the
    /// pair are restored as usual.
    #[test]
    fn rollback_excluding_keeps_the_whole_unit_an_exclusion_touches() {
        for excluded in ["vm.dirty_bytes", "vm.dirty_ratio"] {
            let dir = AtomicTestDir::new("rollback-excluding-twin");
            let (conf, service, script) = fixture_paths(&dir);
            let bytes = dir.0.join("dirty_bytes");
            let ratio = dir.0.join("dirty_ratio");
            let somaxconn = dir.0.join("somaxconn");
            fs::write(&bytes, "1073741824").unwrap();
            fs::write(&ratio, "0").unwrap();
            fs::write(&somaxconn, "4096").unwrap();
            let ledger = fixture_ledger(
                &dir,
                &[
                    ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                    ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
                    (
                        "net.core.somaxconn",
                        "128",
                        "4096",
                        somaxconn.to_str().unwrap(),
                    ),
                ],
            );
            fs::write(
                &conf,
                "vm.dirty_bytes = 1073741824\nnet.core.somaxconn = 4096\n",
            )
            .unwrap();

            let (unmatched, outcome) =
                rollback_excluding_at(&[excluded.to_string()], &ledger, &conf, &service, &script)
                    .unwrap();

            assert!(unmatched.is_empty(), "{unmatched:?}");
            assert_eq!(
                (outcome.restored(), outcome.failed(), outcome.skipped()),
                (1, 0, 0),
                "only the entry outside the pair is restored ({excluded} excluded)"
            );
            assert_eq!(
                fs::read_to_string(&bytes).unwrap(),
                "1073741824",
                "{excluded} excluded: the pair must not be written"
            );
            assert_eq!(
                fs::read_to_string(&ratio).unwrap(),
                "0",
                "{excluded} excluded: the pair must not be written"
            );
            assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
            let data: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&ledger).unwrap()).unwrap();
            let entries = data["entries"].as_object().unwrap();
            assert_eq!(entries.len(), 2, "{data}");
            assert!(entries.get("vm.dirty_bytes").is_some(), "{data}");
            assert!(entries.get("vm.dirty_ratio").is_some(), "{data}");
            let persisted = fs::read_to_string(&conf).unwrap();
            assert!(
                persisted.contains("vm.dirty_bytes = 1073741824"),
                "{persisted}"
            );
            assert!(!persisted.contains("net.core.somaxconn"), "{persisted}");
        }
    }

    /// A name the ledger does not record is not an error for the exclusion:
    /// it comes back in the order given (a repeated name stays repeated) so
    /// an exclusion that protected nothing is visible, and the run restores
    /// everything left exactly as a full rollback would.
    #[test]
    fn rollback_excluding_reports_unmatched_names_in_order() {
        let dir = AtomicTestDir::new("rollback-excluding-unmatched");
        let (conf, service, script) = fixture_paths(&dir);
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
            ],
        );
        fs::write(&conf, "vm.swappiness = 10\n").unwrap();

        let (unmatched, outcome) = rollback_excluding_at(
            &[
                "vm.typo".to_string(),
                "net.core.typo2".to_string(),
                "vm.typo".to_string(),
            ],
            &ledger,
            &conf,
            &service,
            &script,
        )
        .unwrap();

        assert_eq!(unmatched, ["vm.typo", "net.core.typo2", "vm.typo"]);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0)
        );
        assert!(outcome.is_complete());
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
        assert!(
            !Path::new(&ledger).exists(),
            "an inert exclusion still restores everything, so the ledger empties and finalizes"
        );
        assert!(
            !Path::new(&conf).exists(),
            "the persisted file is cleaned up"
        );
    }

    /// When no unit is left to restore — every entry excluded, or the one
    /// remaining selection belonging to the excluded unit — the run attempts
    /// nothing: the counters stay zero, the attempt is complete, and neither
    /// the ledger nor the persisted file is touched.
    #[test]
    fn rollback_excluding_leaves_everything_alone_when_nothing_is_selected() {
        for excluded in [
            vec!["vm.dirty_bytes".to_string(), "vm.dirty_ratio".to_string()],
            vec!["vm.dirty_ratio".to_string()],
        ] {
            let dir = AtomicTestDir::new("rollback-excluding-empty");
            let (conf, service, script) = fixture_paths(&dir);
            let bytes = dir.0.join("dirty_bytes");
            let ratio = dir.0.join("dirty_ratio");
            fs::write(&bytes, "1073741824").unwrap();
            fs::write(&ratio, "0").unwrap();
            let ledger = fixture_ledger(
                &dir,
                &[
                    ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                    ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
                ],
            );
            fs::write(&conf, "vm.dirty_bytes = 1073741824\n").unwrap();
            let ledger_before = fs::read(&ledger).unwrap();
            let persisted_before = fs::read(&conf).unwrap();

            let (unmatched, outcome) =
                rollback_excluding_at(&excluded, &ledger, &conf, &service, &script).unwrap();

            assert!(unmatched.is_empty(), "{excluded:?} names recorded entries");
            assert_eq!(
                (outcome.restored(), outcome.failed(), outcome.skipped()),
                (0, 0, 0),
                "{excluded:?}: nothing is attempted"
            );
            assert!(outcome.is_complete(), "an empty attempt is complete");
            assert_eq!(fs::read(&ledger).unwrap(), ledger_before, "{excluded:?}");
            assert_eq!(fs::read(&conf).unwrap(), persisted_before, "{excluded:?}");
            assert_eq!(
                fs::read_to_string(&bytes).unwrap(),
                "1073741824",
                "{excluded:?}: the pair is never written"
            );
            assert_eq!(
                fs::read_to_string(&ratio).unwrap(),
                "0",
                "{excluded:?}: the pair is never written"
            );
        }
    }

    /// `rollback --list --exclude` previews the set the restore would walk:
    /// every recorded entry the names do not address, minus the unit an
    /// excluded key touches, in the unfiltered listing's param order and
    /// entry shape — with the live reading and drift comparison every entry
    /// carries there. It writes nothing, and it reports unmatched names
    /// instead of refusing.
    #[test]
    fn preview_excluding_lists_the_restore_set() {
        let dir = AtomicTestDir::new("preview-excluding");
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "1024").unwrap();
        fs::write(&bytes, "1073741824").unwrap();
        fs::write(&ratio, "0").unwrap();
        let ledger = fixture_ledger(
            &dir,
            &[
                ("vm.swappiness", "60", "10", swappiness.to_str().unwrap()),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                ),
                ("vm.dirty_bytes", "0", "1073741824", bytes.to_str().unwrap()),
                ("vm.dirty_ratio", "20", "0", ratio.to_str().unwrap()),
            ],
        );
        let ledger_before = fs::read_to_string(&ledger).unwrap();

        let listed = |excluded: &[&str]| {
            let excluded: Vec<String> = excluded.iter().map(|name| name.to_string()).collect();
            let (unmatched, entries) = rollback_preview_excluding_at(&excluded, &ledger).unwrap();
            (
                unmatched,
                entries
                    .into_iter()
                    .map(|entry| entry.param)
                    .collect::<Vec<_>>(),
            )
        };

        // One name excluded: its entry leaves the listing; the rest keep the
        // live reading and drift comparison the unfiltered listing carries.
        let (unmatched, params) = listed(&["vm/swappiness"]);
        assert!(unmatched.is_empty(), "{unmatched:?}");
        assert_eq!(
            params,
            ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio"]
        );
        let (_, entries) =
            rollback_preview_excluding_at(&["vm.swappiness".to_string()], &ledger).unwrap();
        assert_eq!(entries[0].live.as_deref(), Some("1024"));
        assert_eq!(entries[0].drifted, Some(true));

        // Excluding one half of the pair keeps the unit: both halves leave the
        // preview, because the restore walks neither of them.
        let (unmatched, params) = listed(&["vm.dirty_bytes"]);
        assert!(unmatched.is_empty());
        assert_eq!(params, ["net.core.somaxconn", "vm.swappiness"]);

        // A name the ledger does not record comes back and excludes nothing.
        let (unmatched, params) = listed(&["vm.typo"]);
        assert_eq!(unmatched, ["vm.typo"]);
        assert_eq!(
            params,
            [
                "net.core.somaxconn",
                "vm.dirty_bytes",
                "vm.dirty_ratio",
                "vm.swappiness"
            ]
        );

        assert_eq!(
            fs::read_to_string(&ledger).unwrap(),
            ledger_before,
            "the preview is read-only"
        );
    }

    /// The category a rule recorded travels with its entry into every
    /// `rollback --list` shape: the unfiltered listing, the by-name listing,
    /// the exclusion listing and the category listing publish the recorded
    /// value — never a name-derived guess, so the security finding on a
    /// `net.*` knob keeps its category — and a record written before the
    /// field existed publishes none.
    #[test]
    fn previews_carry_the_recorded_category_through_every_selection() {
        let dir = AtomicTestDir::new("preview-category");
        let somaxconn = dir.0.join("somaxconn");
        let bpf_jit_harden = dir.0.join("bpf_jit_harden");
        let swappiness = dir.0.join("swappiness");
        let scheduler = dir.0.join("scheduler");
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&bpf_jit_harden, "1").unwrap();
        fs::write(&swappiness, "10").unwrap();
        fs::write(&scheduler, "none").unwrap();
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "net.core.bpf_jit_harden",
                    "0",
                    "1",
                    bpf_jit_harden.to_str().unwrap(),
                    Some(Category::Security),
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    None,
                ),
                (
                    "block/sda/scheduler",
                    "mq-deadline",
                    "none",
                    scheduler.to_str().unwrap(),
                    Some(Category::Performance),
                ),
            ],
        );

        let recorded = |entries: Vec<RollbackPreview>| -> Vec<(String, Option<Category>)> {
            entries
                .into_iter()
                .map(|entry| (entry.param, entry.category))
                .collect()
        };

        // The unfiltered listing: every entry carries what it was recorded
        // with, and the pre-category record carries none.
        assert_eq!(
            recorded(rollback_preview_at(&ledger).unwrap()),
            vec![
                (
                    "block/sda/scheduler".to_string(),
                    Some(Category::Performance)
                ),
                (
                    "net.core.bpf_jit_harden".to_string(),
                    Some(Category::Security)
                ),
                (
                    "net.core.somaxconn".to_string(),
                    Some(Category::Performance)
                ),
                ("vm.swappiness".to_string(), None),
            ]
        );

        // The by-name listing keeps the recorded category.
        assert_eq!(
            recorded(
                rollback_preview_params_at(&["net/core/somaxconn".to_string()], &ledger).unwrap()
            ),
            vec![(
                "net.core.somaxconn".to_string(),
                Some(Category::Performance)
            )]
        );

        // The exclusion listing keeps it on every entry that stays.
        let (unmatched, entries) =
            rollback_preview_excluding_at(&["vm.swappiness".to_string()], &ledger).unwrap();
        assert!(unmatched.is_empty(), "{unmatched:?}");
        assert_eq!(
            recorded(entries),
            vec![
                (
                    "block/sda/scheduler".to_string(),
                    Some(Category::Performance)
                ),
                (
                    "net.core.bpf_jit_harden".to_string(),
                    Some(Category::Security)
                ),
                (
                    "net.core.somaxconn".to_string(),
                    Some(Category::Performance)
                ),
            ]
        );

        // The category listing selects by the recorded value and publishes it:
        // only recorded security entries come back, and the unclassified
        // record is reported instead of listed.
        let preview =
            rollback_preview_categories_at(&["security".to_string()], &[], &ledger).unwrap();
        assert_eq!(
            recorded(preview.entries),
            vec![(
                "net.core.bpf_jit_harden".to_string(),
                Some(Category::Security)
            )]
        );
        assert_eq!(preview.unclassified, ["vm.swappiness"]);
        assert!(preview.unmatched.is_empty(), "{:?}", preview.unmatched);
    }

    /// With no ledger there is nothing to restore and nothing to exclude: the
    /// preview is the empty listing and every name comes back as unrecorded,
    /// without creating the ledger directory or lock file; the restore is the
    /// same command error every other rollback form reports.
    #[test]
    fn preview_excluding_an_absent_ledger_reports_every_name_and_creates_nothing() {
        let dir = AtomicTestDir::new("preview-excluding-absent");
        let (conf, service, script) = fixture_paths(&dir);
        let ledger = dir.0.join("nested").join("rollback.json");
        let path = ledger.to_str().unwrap();

        let (unmatched, entries) = rollback_preview_excluding_at(
            &["vm.typo".to_string(), "vm.swappiness".to_string()],
            path,
        )
        .unwrap();
        assert_eq!(unmatched, ["vm.typo", "vm.swappiness"]);
        assert!(entries.is_empty());
        assert!(!ledger.exists(), "the preview must not create the ledger");
        assert!(!ledger.parent().unwrap().exists());
        assert!(!Path::new(&format!("{}.lock", ledger.display())).exists());

        let error = rollback_excluding_at(&["vm.typo".to_string()], path, &conf, &service, &script)
            .err()
            .expect("no ledger records nothing to restore");
        assert!(
            error.to_string().contains("没有找到 rollback 文件"),
            "{error:#}"
        );
    }

    /// A recommendation shaped the way the rules emit one: the parameter, a
    /// value to write, and the rule layer's own category.
    fn ruled_rec(param: &str, category: Category) -> Recommendation {
        Recommendation {
            param: param.to_string(),
            current_value: "0".to_string(),
            recommended_value: "1".to_string(),
            category,
            writable: true,
            ..Default::default()
        }
    }

    /// [`fixture_ledger`] with a recorded category per entry — the ledger the
    /// category-scoped selection reads, written the way the production
    /// merger writes one (a categorized entry carries the serde spelling of
    /// the rule category, an unclassified one carries no key at all).
    fn fixture_category_ledger(
        dir: &AtomicTestDir,
        entries: &[(&str, &str, &str, &str, Option<Category>)],
    ) -> String {
        let mut map = serde_json::Map::new();
        for (param, previous, applied, path, category) in entries {
            let mut entry = serde_json::json!({
                "previous": previous, "applied": applied, "path": path,
            });
            if let Some(category) = category {
                entry["category"] = serde_json::to_value(category).unwrap();
            }
            map.insert((*param).to_string(), entry);
        }
        let ledger = dir.0.join("rollback.json");
        fs::write(
            &ledger,
            serde_json::json!({ "version": 1, "entries": map }).to_string(),
        )
        .unwrap();
        ledger.to_str().unwrap().to_string()
    }

    /// The ledger keys a selection would restore, in unit order.
    fn selected_keys(selection: &CategorySelection) -> Vec<String> {
        selection
            .units
            .iter()
            .flat_map(|unit| unit.keys().cloned())
            .collect()
    }

    /// The contract of the whole feature: `rollback --category X` restores
    /// exactly the entries `tune --category X` wrote. One ledger is recorded
    /// from every finding of a mixed plan (the way the production merger
    /// records one), and each named category must select the slice
    /// `filter_by_categories` keeps from the live plan — for a single value
    /// and for the union of several, through the aliases too. The plan
    /// includes the two cases name-based guessing gets wrong: a security
    /// finding on a `net.*` knob (never selected by `--category net`) and a
    /// performance finding on a `kernel.*` knob (never selected by
    /// `--category security`).
    #[test]
    fn category_selection_agrees_with_the_tune_category_filter() {
        let recs = [
            ruled_rec("net.core.bpf_jit_harden", Category::Security),
            ruled_rec("net.core.somaxconn", Category::Performance),
            ruled_rec("vm.swappiness", Category::Performance),
            ruled_rec("kernel.dmesg_restrict", Category::Security),
            ruled_rec("kernel.sched_latency_ns", Category::Performance),
        ];
        let data = merge_entries(
            RollbackData {
                version: 1,
                entries: BTreeMap::new(),
            },
            recs.iter().map(|r| {
                (
                    r.param.clone(),
                    r.current_value.clone(),
                    r.recommended_value.clone(),
                    Some(r.category.clone()),
                )
            }),
        );
        for names in [
            vec!["net"],
            vec!["mem"],
            vec!["cpu"],
            vec!["io"],
            vec!["security"],
            vec!["network"],
            vec!["内存"],
            vec!["net", "mem"],
            vec!["内存", "安全"],
        ] {
            let cats: Vec<String> = names.iter().map(|cat| cat.to_string()).collect();
            let mut plan: Vec<String> = category::filter_by_categories(recs.to_vec(), &cats)
                .iter()
                .map(|r| r.param.clone())
                .collect();
            plan.sort();
            let selection = category_selection(&data.entries, &cats, &[]);
            assert_eq!(selected_keys(&selection), plan, "categories {names:?}");
            assert!(
                selection.unclassified.is_empty(),
                "{names:?}: every entry here was written by a rule"
            );
        }
    }

    /// Entries with no category — a ledger written before the field existed,
    /// or one a library import wrote — are selected by no value at all, and
    /// the classification is never guessed from the parameter name: a
    /// `net.*` record stays put under `--category net`, and a record whose
    /// name looks like a security knob stays put under `--category security`
    /// too. They are echoed by name so the caller sees why they were left.
    #[test]
    fn category_selection_reports_unclassified_entries_instead_of_guessing() {
        let dir = AtomicTestDir::new("category-unclassified");
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        let suid_dumpable = dir.0.join("suid_dumpable");
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    None,
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "fs.suid_dumpable",
                    "0",
                    "2",
                    suid_dumpable.to_str().unwrap(),
                    None,
                ),
            ],
        );
        let data = load_rollback_from(&ledger).unwrap();

        // The net.* record is unclassified: its name does not select it.
        let selection = category_selection(&data.entries, &["net".to_string()], &[]);
        assert!(
            selected_keys(&selection).is_empty(),
            "selected {:?}",
            selected_keys(&selection)
        );
        assert_eq!(
            selection.unclassified,
            ["fs.suid_dumpable", "net.core.somaxconn"]
        );

        // Nor does a name that looks like a security knob: the category is
        // the rule layer's, and no rule recorded one for it.
        let selection = category_selection(&data.entries, &["security".to_string()], &[]);
        assert!(
            selected_keys(&selection).is_empty(),
            "selected {:?}",
            selected_keys(&selection)
        );
        assert_eq!(
            selection.unclassified,
            ["fs.suid_dumpable", "net.core.somaxconn"]
        );

        // A classified entry of another category is not reported at all.
        let selection = category_selection(&data.entries, &["mem".to_string()], &[]);
        assert_eq!(selected_keys(&selection), ["vm.swappiness"]);
        assert_eq!(
            selection.unclassified,
            ["fs.suid_dumpable", "net.core.somaxconn"]
        );
    }

    /// `--category … --exclude …` narrows the way `tune` does: the categories
    /// select first, then the exclusions drop. An excluded name that dresses
    /// no entry is echoed like every other inert exclusion, and excluding one
    /// half of a recorded mutually exclusive pair keeps the whole unit — the
    /// selected half is dropped too, because restoring it would zero the
    /// excluded one in the kernel.
    #[test]
    fn category_selection_stacks_exclusions_on_the_selection() {
        let dir = AtomicTestDir::new("category-excluding");
        let bytes = dir.0.join("dirty_bytes");
        let ratio = dir.0.join("dirty_ratio");
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "vm.dirty_bytes",
                    "0",
                    "1073741824",
                    bytes.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.dirty_ratio",
                    "20",
                    "0",
                    ratio.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    Some(Category::Performance),
                ),
            ],
        );
        let data = load_rollback_from(&ledger).unwrap();

        let selection = category_selection(
            &data.entries,
            &["net".to_string(), "mem".to_string()],
            &["vm.swappiness".to_string(), "vm.typo".to_string()],
        );
        // The categories keep somaxconn and the pair; swappiness matched the
        // categories too and was dropped by the exclusion, while net.*
        // bpf_jit_harden was never selected. The unmatched name is echoed.
        assert_eq!(
            selected_keys(&selection),
            ["net.core.somaxconn", "vm.dirty_bytes", "vm.dirty_ratio"]
        );
        assert_eq!(selection.unmatched, ["vm.typo"]);
        assert!(selection.unclassified.is_empty());

        let selection = category_selection(
            &data.entries,
            &["mem".to_string()],
            &["vm.dirty_bytes".to_string()],
        );
        // Only swappiness is left: excluding one half of the pair drops the
        // whole unit, so the selected half is not restored either.
        assert_eq!(selected_keys(&selection), ["vm.swappiness"]);
        assert!(
            selection.unmatched.is_empty(),
            "the name addressed an entry, so it is not an inert exclusion"
        );
    }

    /// The restore half: only the selected entries are written and retired,
    /// the rest stay live and recorded, and the persisted file is regenerated
    /// from what remains. The unclassified record is reported and untouched.
    #[test]
    fn rollback_categories_restores_only_the_selected_entries() {
        let dir = AtomicTestDir::new("rollback-categories-restore");
        let (conf, service, script) = fixture_paths(&dir);
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        let dmesg = dir.0.join("dmesg_restrict");
        let unknown = dir.0.join("unknown_knob");
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&swappiness, "10").unwrap();
        fs::write(&dmesg, "1").unwrap();
        fs::write(&unknown, "7").unwrap();
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "kernel.dmesg_restrict",
                    "0",
                    "1",
                    dmesg.to_str().unwrap(),
                    Some(Category::Security),
                ),
                (
                    "vm.ktuner_unknown_knob",
                    "9",
                    "7",
                    unknown.to_str().unwrap(),
                    None,
                ),
            ],
        );
        fs::write(
            &conf,
            "net.core.somaxconn = 4096\nvm.swappiness = 10\nkernel.dmesg_restrict = 1\n",
        )
        .unwrap();

        let report =
            rollback_categories_at(&["net".to_string()], &[], &ledger, &conf, &service, &script)
                .unwrap();

        assert!(report.unmatched.is_empty(), "{:?}", report.unmatched);
        assert_eq!(
            (
                report.outcome.restored(),
                report.outcome.failed(),
                report.outcome.skipped()
            ),
            (1, 0, 0)
        );
        assert!(report.outcome.is_complete());
        assert_eq!(report.unclassified, ["vm.ktuner_unknown_knob"]);
        assert_eq!(
            fs::read_to_string(&somaxconn).unwrap(),
            "128",
            "the selected entry is restored"
        );
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "10");
        assert_eq!(fs::read_to_string(&dmesg).unwrap(), "1");
        assert_eq!(fs::read_to_string(&unknown).unwrap(), "7");
        let data = load_rollback_from(&ledger).unwrap();
        assert_eq!(
            data.entries.keys().collect::<Vec<_>>(),
            [
                "kernel.dmesg_restrict",
                "vm.ktuner_unknown_knob",
                "vm.swappiness"
            ],
            "the untouched records stay"
        );
        let persisted = fs::read_to_string(&conf).unwrap();
        assert!(!persisted.contains("somaxconn"), "{persisted}");
        assert!(persisted.contains("vm.swappiness = 10"), "{persisted}");
    }

    /// A selection that comes up empty is the empty ledger's answer: the
    /// counters stay zero, the attempt is complete, and neither the ledger
    /// nor the persisted file is touched — whichever way the set came up
    /// empty (no entry of that category, or every selected unit kept by an
    /// exclusion).
    #[test]
    fn rollback_categories_selecting_nothing_touches_nothing() {
        let dir = AtomicTestDir::new("rollback-categories-empty");
        let (conf, service, script) = fixture_paths(&dir);
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    None,
                ),
            ],
        );
        fs::write(&conf, "net.core.somaxconn = 4096\n").unwrap();
        let ledger_before = fs::read(&ledger).unwrap();
        let persisted_before = fs::read(&conf).unwrap();

        // No entry of the named category.
        let report =
            rollback_categories_at(&["io".to_string()], &[], &ledger, &conf, &service, &script)
                .unwrap();
        assert_eq!(
            (
                report.outcome.restored(),
                report.outcome.failed(),
                report.outcome.skipped()
            ),
            (0, 0, 0)
        );
        assert!(report.outcome.is_complete(), "an empty attempt is complete");
        assert_eq!(report.unclassified, ["vm.swappiness"]);
        assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
        assert_eq!(fs::read(&conf).unwrap(), persisted_before);
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "4096");
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "10");

        // Every selected entry excluded.
        let report = rollback_categories_at(
            &["net".to_string()],
            &["net.core.somaxconn".to_string()],
            &ledger,
            &conf,
            &service,
            &script,
        )
        .unwrap();
        assert_eq!(
            (
                report.outcome.restored(),
                report.outcome.failed(),
                report.outcome.skipped()
            ),
            (0, 0, 0)
        );
        assert!(report.outcome.is_complete());
        assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
        assert_eq!(fs::read(&conf).unwrap(), persisted_before);
    }

    /// A category value no rule category names is a command error, exactly the
    /// one `check`, `tune` and `rollback --category` answer: the CLI validates
    /// its own argument before the call, and the library entry is reachable
    /// without that caller. Without the refusal the predicate's fallback
    /// answered "everything" for such a value, so a typo restored every
    /// categorized record instead of none. The refusal comes before the lock,
    /// so a refused command creates neither the ledger directory nor its lock
    /// file.
    #[test]
    fn rollback_categories_refuses_an_unknown_category() {
        let dir = AtomicTestDir::new("rollback-categories-unknown");
        let (conf, service, script) = fixture_paths(&dir);
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    Some(Category::Performance),
                ),
            ],
        );
        let ledger_before = fs::read(&ledger).unwrap();

        for spelling in ["garbage", "netwok", "", "Network "] {
            let error = rollback_categories_at(
                &[spelling.to_string()],
                &[],
                &ledger,
                &conf,
                &service,
                &script,
            )
            .err()
            .unwrap_or_else(|| panic!("{spelling:?} must refuse the command"));
            assert!(error.to_string().contains("未知分类"), "{error:#}");
            // The preview walks the same selection, so it refuses it too.
            let preview = rollback_preview_categories_at(&[spelling.to_string()], &[], &ledger);
            assert!(preview.is_err(), "{spelling:?} must refuse the preview");
            // A refused command restored nothing and left no trace.
            assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "4096");
            assert_eq!(fs::read_to_string(&swappiness).unwrap(), "10");
            assert_eq!(fs::read(&ledger).unwrap(), ledger_before);
            assert!(
                !Path::new(&format!("{ledger}.lock")).exists(),
                "{spelling:?} must be refused before the ledger lock is taken"
            );
        }
    }

    /// The preview lists exactly the entries the restore then retires, in the
    /// unfiltered listing's shape, and answers the same two reports; it
    /// writes nothing.
    #[test]
    fn preview_categories_lists_the_restore_set() {
        let dir = AtomicTestDir::new("preview-categories-set");
        let (conf, service, script) = fixture_paths(&dir);
        let somaxconn = dir.0.join("somaxconn");
        let swappiness = dir.0.join("swappiness");
        fs::write(&somaxconn, "4096").unwrap();
        fs::write(&swappiness, "10").unwrap();
        let ledger = fixture_category_ledger(
            &dir,
            &[
                (
                    "net.core.somaxconn",
                    "128",
                    "4096",
                    somaxconn.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.swappiness",
                    "60",
                    "10",
                    swappiness.to_str().unwrap(),
                    Some(Category::Performance),
                ),
                (
                    "vm.ktuner_unknown_knob",
                    "9",
                    "7",
                    "/nonexistent/ktuner/knob",
                    None,
                ),
            ],
        );
        let ledger_before = fs::read(&ledger).unwrap();

        let preview =
            rollback_preview_categories_at(&["net".to_string()], &["vm.typo".to_string()], &ledger)
                .unwrap();
        let params: Vec<&str> = preview.entries.iter().map(|e| e.param.as_str()).collect();
        assert_eq!(params, ["net.core.somaxconn"]);
        assert_eq!(preview.unmatched, ["vm.typo"]);
        assert_eq!(preview.unclassified, ["vm.ktuner_unknown_knob"]);
        assert_eq!(
            fs::read(&ledger).unwrap(),
            ledger_before,
            "the preview is read-only"
        );

        // The listing is exactly the set the restore then retires.
        let report =
            rollback_categories_at(&["net".to_string()], &[], &ledger, &conf, &service, &script)
                .unwrap();
        assert_eq!(report.outcome.restored(), 1);
        let data = load_rollback_from(&ledger).unwrap();
        assert!(!data.entries.contains_key("net.core.somaxconn"));
        assert!(data.entries.contains_key("vm.swappiness"));
    }

    /// With no ledger there is nothing to select: the preview is the empty
    /// listing and every excluded name comes back as unrecorded, without
    /// creating the ledger directory or its lock file; the restore reports
    /// the same command error every other rollback form reports.
    #[test]
    fn preview_categories_an_absent_ledger_selects_nothing() {
        let dir = AtomicTestDir::new("preview-categories-absent");
        let (conf, service, script) = fixture_paths(&dir);
        let ledger = dir.0.join("nested").join("rollback.json");
        let path = ledger.to_str().unwrap();

        let preview =
            rollback_preview_categories_at(&["net".to_string()], &["vm.typo".to_string()], path)
                .unwrap();
        assert!(preview.entries.is_empty());
        assert_eq!(preview.unmatched, ["vm.typo"]);
        assert!(preview.unclassified.is_empty());
        assert!(!ledger.exists(), "the preview must not create the ledger");
        assert!(!ledger.parent().unwrap().exists());
        assert!(!Path::new(&format!("{}.lock", ledger.display())).exists());

        let error =
            rollback_categories_at(&["net".to_string()], &[], path, &conf, &service, &script)
                .err()
                .expect("no ledger records nothing to restore");
        assert!(
            error.to_string().contains("没有找到 rollback 文件"),
            "{error:#}"
        );
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

    /// A stdout that fails every write: `/dev/full` answers ENOSPC, the same
    /// way a pipe whose consumer left answers EPIPE.
    fn full_stdout() -> Option<std::fs::File> {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .ok()
    }

    /// Point fd 1 at a stdout that fails every write until the guard drops,
    /// including while a panic unwinds. The saved descriptor is restored, so
    /// the harness still gets its own output afterwards.
    struct BrokenStdout {
        saved: i32,
        _full: std::fs::File,
    }

    impl BrokenStdout {
        fn take(full: std::fs::File) -> Self {
            let _ = std::io::stdout().flush();
            let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
            assert!(saved >= 0, "dup(1) failed");
            let rc = unsafe { libc::dup2(full.as_raw_fd(), libc::STDOUT_FILENO) };
            assert!(rc >= 0, "dup2 onto fd 1 failed");
            Self { saved, _full: full }
        }
    }

    impl Drop for BrokenStdout {
        fn drop(&mut self) {
            unsafe {
                libc::dup2(self.saved, libc::STDOUT_FILENO);
                libc::close(self.saved);
            }
        }
    }

    /// The child half of [`a_failed_stdout_write_cannot_abort_a_transaction`],
    /// run with `--nocapture` so the harness does not buffer the output these
    /// calls produce: buffered bytes cannot fail a write, and the failure is
    /// the whole subject. The restore and the report run under the replaced
    /// fd 1, so their progress lines hit a stdout that refuses every byte.
    fn broken_stdout_body() {
        let full = full_stdout().expect("the parent checked /dev/full exists");
        let broken = BrokenStdout::take(full);
        // Precondition, not the assertion: a refused write is what makes the
        // calls below exercise the failure at all.
        assert!(
            std::io::stdout()
                .write_fmt(format_args!("probe\n"))
                .is_err(),
            "the redirected stdout must refuse a write"
        );

        // The restore loop prints each entry as it lands. A panic on the
        // first line used to abandon the rest: the entry already restored
        // stayed restored, the others kept their tuned values, and the
        // caller's counts never came back.
        let dir = AtomicTestDir::new("stdout-failure");
        let swappiness = dir.0.join("swappiness");
        let somaxconn = dir.0.join("somaxconn");
        fs::write(&swappiness, "10").unwrap();
        fs::write(&somaxconn, "4096").unwrap();
        let data = RollbackData {
            version: 1,
            entries: BTreeMap::from([
                (
                    "net.core.somaxconn".to_string(),
                    RollbackEntry {
                        previous: "128".into(),
                        applied: "4096".into(),
                        path: somaxconn.to_str().unwrap().to_string(),
                        category: None,
                    },
                ),
                (
                    "vm.swappiness".to_string(),
                    RollbackEntry {
                        previous: "60".into(),
                        applied: "10".into(),
                        path: swappiness.to_str().unwrap().to_string(),
                        category: None,
                    },
                ),
            ]),
        };
        let outcome = restore_entries(&data, false);
        assert_eq!(
            (outcome.restored(), outcome.failed(), outcome.skipped()),
            (2, 0, 0),
            "every recorded entry must be restored despite the failed writes"
        );
        assert_eq!(fs::read_to_string(&somaxconn).unwrap(), "128");
        assert_eq!(fs::read_to_string(&swappiness).unwrap(), "60");

        // The report surface: identical runs stay clean, which the caller can
        // only learn if the table's writes cannot panic.
        let before = vec![bench("seq_read", 100.0, "MB/s")];
        let after = vec![bench("seq_read", 100.0, "MB/s")];
        assert!(
            verify_and_report(&before, &after).degraded.is_empty(),
            "the comparison must still grade the metrics"
        );

        drop(broken);
    }

    /// The engine's output is advisory: its verdict is the values it returns
    /// (and the CLI's own body), so a stdout that cannot take a write must
    /// never abort the work it is part of. Rust's `println!` panics on a write
    /// error, and the engine prints inside its transactions — between the live
    /// write and the ledger save in `apply_locked`, between the restores in
    /// `restore_entries_with` — so a consumer that stopped reading
    /// (`ktuner rollback | head -1`) used to abandon a half-done transaction,
    /// with the entries already restored standing and the rest never written,
    /// and replaced the command's documented exit status with the panic code.
    /// The same failure skips the automatic rollback, whose verdict
    /// `verify_and_report` returns only after rendering its table.
    ///
    /// The work runs in a re-executed child: the harness buffers a test
    /// thread's own output, and a buffered write cannot fail, so the child
    /// disables that (`--nocapture`) and points fd 1 at `/dev/full`.
    #[test]
    fn a_failed_stdout_write_cannot_abort_a_transaction() {
        const CHILD: &str = "KTUNER_BROKEN_STDOUT_CHILD";
        if std::env::var_os(CHILD).is_some() {
            broken_stdout_body();
            return;
        }
        if full_stdout().is_none() {
            eprintln!("skipping: /dev/full is not available on this host");
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tuner::tests::a_failed_stdout_write_cannot_abort_a_transaction",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "a failed stdout write must not abort the engine (child exited {:?}):\n{stdout}\n{stderr}",
            output.status.code()
        );
        assert!(
            !stderr.contains("panicked"),
            "the engine panicked on a failed stdout write:\n{stderr}"
        );
    }
}
