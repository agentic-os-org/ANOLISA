use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
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

pub fn apply(recommendations: &[Recommendation]) -> Result<usize> {
    apply_inner(recommendations, false)
}

pub fn apply_quiet(recommendations: &[Recommendation]) -> Result<usize> {
    apply_inner(recommendations, true)
}

fn apply_inner(recommendations: &[Recommendation], quiet: bool) -> Result<usize> {
    let total = recommendations.len();
    let mut applied_recs: Vec<Recommendation> = Vec::new();
    for (i, rec) in recommendations.iter().enumerate() {
        match apply_single(rec) {
            Ok(()) => {
                if !quiet {
                    println!(
                        "    {} [{}/{}] {} → {}",
                        "✓".green(),
                        i + 1,
                        total,
                        rec.param,
                        rec.recommended_value
                    );
                }
                applied_recs.push(rec.clone());
            }
            Err(e) => {
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
        save_rollback(&applied_recs)?;
        persist_from_rollback()?;
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
    Ok(applied_recs.len())
}

/// Apply a single recommendation with rollback recording and persistence, but
/// without apply()'s progress output — used by `ktuner fix` so a single fix is
/// just as reversible (and survives reboot) as `tune`.
pub fn apply_one(rec: &Recommendation) -> Result<()> {
    apply_single(rec)?;
    save_rollback(std::slice::from_ref(rec))?;
    persist_from_rollback()?;
    Ok(())
}

fn apply_single(rec: &Recommendation) -> Result<()> {
    write_and_verify(&rec.param, &rec.recommended_value)
}

/// Write `value` to the kernel path for `param` and verify it took effect by
/// reading it back. This is the single choke point for every live parameter
/// write (tune / fix / import all route through here), so the code-execution
/// deny-list is enforced here too as defense-in-depth — see is_forbidden_param.
pub fn write_and_verify(param: &str, value: &str) -> Result<()> {
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
    // fails — treat that as success, not a spurious verify failure, since the
    // kernel took the write.
    let readback = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return Ok(()),
    };

    let readback_trimmed = readback.trim();
    if !readback_matches(value, readback_trimmed) {
        anyhow::bail!("验证失败: 期望 '{value}', 实际 '{readback_trimmed}'");
    }

    Ok(())
}

/// Whether a sysfs/sysctl read-back indicates `value` took effect. sysfs "list"
/// files (block scheduler, transparent_hugepage/enabled|defrag, ...) echo every
/// option and mark the ACTIVE one in brackets, e.g. "always madvise [never]" —
/// the selected value is inside `[ ]`, not necessarily first. So whenever the
/// read-back contains a bracketed token we look for `[value]`; otherwise we
/// compare tokens (tolerating a single written value against a multi-token
/// read-back that leads with it). Previously only params literally named
/// "scheduler" got the bracket-aware path, so THP writes were mis-reported as
/// verify failures.
fn readback_matches(value: &str, readback_trimmed: &str) -> bool {
    if readback_trimmed.contains('[') {
        return readback_trimmed.contains(&format!("[{value}]"));
    }
    let rec_tokens: Vec<&str> = value.split_whitespace().collect();
    let read_tokens: Vec<&str> = readback_trimmed.split_whitespace().collect();
    if rec_tokens.len() == 1 && read_tokens.len() > 1 {
        read_tokens.first() == rec_tokens.first()
    } else {
        rec_tokens == read_tokens
    }
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
    } else {
        // sysctl: dots become slashes, so any ".." is turned into "//" and
        // cannot traverse; the result is always rooted at /proc/sys.
        format!("/proc/sys/{}", param.replace('.', "/"))
    }
}

/// Whether a parameter name is structurally legitimate to apply. Used to reject
/// hostile entries from imported config files before they ever reach the
/// filesystem. Rejects traversal, absolute paths and NUL bytes.
pub fn is_safe_param(param: &str) -> bool {
    if param.is_empty() || param.starts_with('/') || param.contains('\0') {
        return false;
    }
    // `..` is only a traversal between path separators. The sysctl branch turns
    // dots into slashes (so any ".." there collapses to "//" and cannot
    // escape), leaving the block// and transparent_hugepage/ branches — both
    // '/'-separated — as the real risk.
    if param.split('/').any(|seg| seg == "..") {
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

fn load_rollback() -> RollbackData {
    if let Ok(json) = fs::read_to_string(ROLLBACK_PATH) {
        if let Ok(data) = serde_json::from_str::<RollbackData>(&json) {
            return data;
        }
    }
    RollbackData {
        version: 1,
        entries: BTreeMap::new(),
    }
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

fn save_rollback(recommendations: &[Recommendation]) -> Result<()> {
    merge_rollback(recommendations.iter().map(|r| {
        (
            r.param.clone(),
            r.current_value.clone(),
            r.recommended_value.clone(),
        )
    }))
}

/// Merge `(param, previous, applied)` entries into a cumulative rollback record.
/// For a param already recorded, keep the ORIGINAL `previous` (the true
/// pre-ktuner value) so rollback always restores pristine state even across
/// multiple tune/fix/import runs; only refresh `applied`. New params are added.
/// Pure (no I/O) so the keep-original-previous invariant is unit-testable.
fn merge_entries<I>(mut data: RollbackData, entries: I) -> RollbackData
where
    I: IntoIterator<Item = (String, String, String)>,
{
    for (param, previous, applied) in entries {
        let path = param_to_path(&param);
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

/// Merge the given entries into the on-disk rollback record and persist it.
fn merge_rollback<I>(entries: I) -> Result<()>
where
    I: IntoIterator<Item = (String, String, String)>,
{
    let dir = Path::new(ROLLBACK_PATH).parent().unwrap();
    fs::create_dir_all(dir).context("创建 rollback 目录失败")?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("设置 {} 权限 0700 失败", dir.display()))?;

    let data = merge_entries(load_rollback(), entries);

    let json = serde_json::to_string_pretty(&data)?;
    write_atomic(ROLLBACK_PATH, json.as_bytes(), 0o600).context("保存 rollback 文件失败")?;
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
/// with no way back. `current` is the pre-write value: rollback is only
/// recorded when it is known, so we never record a bogus "" original to restore.
pub fn apply_import(param: &str, value: &str, current: Option<&str>) -> Result<()> {
    validate_import_value(param, value)?;
    write_and_verify(param, value)?;
    if let Some(prev) = current {
        merge_rollback(std::iter::once((
            param.to_string(),
            prev.to_string(),
            value.to_string(),
        )))?;
    }
    Ok(())
}

const NONSYSCTL_SCRIPT_PATH: &str = "/etc/ktuner/apply-nonsysctl.sh";
const NONSYSCTL_SERVICE_PATH: &str = "/etc/systemd/system/ktuner-nonsysctl.service";

/// Regenerate the persisted config files from the cumulative rollback record,
/// which is the single source of truth for everything ktuner has applied. This
/// keeps persistence cumulative across runs (previously each run overwrote the
/// files with only its own batch, silently dropping earlier params) and never
/// persists a param that failed to apply (those are not in the record).
fn persist_from_rollback() -> Result<()> {
    let data = load_rollback();

    let mut sysctl_content = String::from("# Generated by ktuner - do not edit manually\n");
    sysctl_content.push_str("# Run `sudo ktuner rollback` to revert\n\n");

    let mut nonsysctl_script = String::from("#!/bin/bash\n");
    nonsysctl_script.push_str("# Generated by ktuner - do not edit manually\n");
    nonsysctl_script.push_str("# Run `sudo ktuner rollback` to revert\n\n");

    let mut has_sysctl = false;
    let mut has_nonsysctl = false;

    for (param, entry) in &data.entries {
        if param.starts_with("block/") || param.starts_with("transparent_hugepage/") {
            nonsysctl_script.push_str(&format!(
                "[ -f '{}' ] && echo '{}' > '{}'\n",
                entry.path, entry.applied, entry.path
            ));
            has_nonsysctl = true;
        } else if param.contains('.') {
            sysctl_content.push_str(&format!("{} = {}\n", param, entry.applied));
            has_sysctl = true;
        }
    }

    if has_sysctl {
        // sysctl.d convention: world-readable, same as the systemd service
        // file below; write_atomic lands the mode before the rename so no
        // 0600 intermediate is ever visible at the final path.
        write_atomic(SYSCTL_PERSIST_PATH, sysctl_content.as_bytes(), 0o644)
            .context("持久化 sysctl 配置失败（需要 root 权限？）")?;
    }

    if has_nonsysctl {
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
    if !Path::new(ROLLBACK_PATH).exists() {
        return Ok(Vec::new());
    }

    let json = fs::read_to_string(ROLLBACK_PATH).context("读取 rollback 文件失败")?;
    let data: RollbackData = serde_json::from_str(&json).context("解析 rollback 文件失败")?;

    let mut result = Vec::new();
    for (param, entry) in &data.entries {
        result.push((param.clone(), entry.applied.clone(), entry.previous.clone()));
    }
    Ok(result)
}

/// Outcome of a rollback attempt: how many params were restored vs. failed to
/// restore vs. skipped (path absent). `failed`/`skipped` decide whether the
/// rollback ledger is safe to delete and whether the restore was actually total.
pub struct RollbackOutcome {
    pub restored: usize,
    pub failed: usize,
    pub skipped: usize,
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

fn rollback_inner(quiet: bool) -> Result<RollbackOutcome> {
    if !Path::new(ROLLBACK_PATH).exists() {
        anyhow::bail!("没有找到 rollback 文件 ({ROLLBACK_PATH})，可能尚未执行过 tune");
    }

    let json = fs::read_to_string(ROLLBACK_PATH).context("读取 rollback 文件失败")?;
    let data: RollbackData = serde_json::from_str(&json).context("解析 rollback 文件失败")?;

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

pub fn verify_and_report(before: &[BenchResult], after: &[BenchResult]) -> VerifyResult {
    println!("  {}", "性能对比 (before → after)".bold());
    println!(
        "  {:<24} {:>16} {:>16} {:>8}",
        "指标", "调前", "调后", "变化"
    );
    println!("  {}", "─".repeat(66));

    let mut degraded = Vec::new();

    for (b, a) in before.iter().zip(after.iter()) {
        let change = if b.value > 0.0 {
            (a.value - b.value) / b.value * 100.0
        } else {
            0.0
        };

        let is_latency = b.unit.contains("ns") || b.unit.contains("μs");

        let is_degraded = if is_latency {
            change > DEGRADATION_THRESHOLD
        } else {
            change < -DEGRADATION_THRESHOLD
        };

        if is_degraded {
            degraded.push(b.name.clone());
        }

        let change_display = if change.abs() < 1.0 {
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
    fn test_readback_matches() {
        // Bracketed sysfs list files: the active option is inside [ ], not first.
        assert!(readback_matches("never", "always madvise [never]"));
        assert!(readback_matches("mq-deadline", "[mq-deadline] none"));
        assert!(!readback_matches("never", "[always] madvise never"));
        // Plain scalar sysctls.
        assert!(readback_matches("1", "1"));
        assert!(!readback_matches("1", "0"));
        // Single written value leading a multi-token read-back matches on first.
        assert!(readback_matches("bbr", "bbr cubic"));
        // Multi-token exact match, and its negation.
        assert!(readback_matches("250 32000 100 128", "250 32000 100 128"));
        assert!(!readback_matches("250 32000 100 128", "250 32000 100 999"));
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
}
