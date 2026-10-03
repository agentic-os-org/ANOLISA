use anyhow::Result;
use clap::{Parser, Subcommand};
use ktuner_engine::{
    self as engine, category, detect, evaluate, rules, services, tuner, Recommendation,
};
use serde_json::json;

#[derive(Parser)]
#[command(name = "ktuner", version, about = "Deterministic kernel-tuning engine")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Diagnose system and output tuning recommendations
    Check {
        #[arg(long)]
        category: Option<String>,
        #[arg(long)]
        conservative: bool,
    },
    /// Apply tuning recommendations
    Tune {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        conservative: bool,
        #[arg(long)]
        category: Option<String>,
    },
    /// Fix a single parameter
    Fix { param: String },
    /// Explain why a parameter should be changed
    Why { param: String },
    /// Roll back all applied changes
    Rollback,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.command {
        Commands::Check {
            category: cat,
            conservative,
        } => cmd_check(cat, conservative),
        Commands::Tune {
            dry_run,
            conservative,
            category: cat,
        } => cmd_tune(dry_run, conservative, cat),
        Commands::Fix { param } => cmd_fix(&param),
        Commands::Why { param } => cmd_why(&param),
        Commands::Rollback => cmd_rollback(),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let out = json!({ "error": format!("{e:#}") });
            eprintln!("{}", serde_json::to_string_pretty(&out).unwrap());
            std::process::exit(2);
        }
    }
}

fn cmd_check(cat: Option<String>, conservative: bool) -> Result<i32> {
    if let Some(ref c) = cat {
        category::validate_category(c)?;
    }
    let info = detect::gather_system_info()?;
    let eval = evaluate(&info)?;
    let workload = engine::classify(&info);
    let runtime_env = detect::detect_runtime_env();
    let detected_services = services::detect_services(&info);

    let mut recs = eval.recommendations.clone();
    if let Some(ref c) = cat {
        recs = category::filter_by_category(recs, c);
    }
    if conservative {
        recs.retain(|r| r.confidence == rules::Confidence::High);
    }

    let score = eval.score();
    let counts = category::RecCounts::from_recs(&recs);
    // What `score` would report once this view has been applied. Not
    // `score + shown weight`: `score` is floored at 30, so adding the shown
    // weight counts that floor as a gain and promises points the untouched
    // findings still keep off the board (on a 66-finding host,
    // `--conservative` promised 72 while applying it lands on the floor, 30).
    let predicted_score = eval.score_after_applying(&recs);

    let recs_json: Vec<serde_json::Value> = recs
        .iter()
        .map(|r| {
            json!({
                "param": r.param,
                "current": r.current_value,
                "recommended": r.recommended_value,
                "reason": r.reason,
                "confidence": format!("{:?}", r.confidence).to_lowercase(),
                "category": format!("{:?}", r.category).to_lowercase(),
                "subcategory": category::param_subcategory(&r.param),
                "writable": r.writable,
            })
        })
        .collect();

    let output = json!({
        "score": score,
        "predicted_score": predicted_score,
        "total_checked": eval.total_checked,
        "recommendations": recs_json,
        "counts": {
            "performance": counts.perf,
            "security": counts.sec,
            "high_confidence": counts.high,
            "writable": counts.writable,
        },
        "system": {
            "kernel": info.kernel_version,
            "cpu_cores": info.cpu_cores,
            "memory_gb": info.memory_total_gb,
            "numa_nodes": info.numa_nodes,
        },
        "environment": format!("{runtime_env}"),
        "workload": format!("{workload}"),
        "services": detected_services,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);

    let code = if recs.is_empty() { 0 } else { 1 };
    Ok(code)
}

/// Why `ktuner tune` exited without applying anything. Pure so the
/// status/exit-code decision is unit-testable without touching the system.
///
/// `in_scope` is the recommendation list after the category/conservative
/// filters but BEFORE the writable/runtime-dangerous filter; `applicable` is
/// the number that survives it. Returns `None` when tune should proceed (at
/// least one applicable recommendation).
fn tune_short_circuit(
    in_scope: &[Recommendation],
    applicable: usize,
) -> Option<(serde_json::Value, i32)> {
    if applicable > 0 {
        return None;
    }
    if in_scope.is_empty() {
        // Genuinely nothing to recommend in scope — unchanged output and code.
        return Some((json!({ "status": "optimal", "applied": 0 }), 0));
    }
    // Recommendations exist but every one was filtered out before any write.
    // Reporting "optimal" here is false: `check` exits 1 on the same host.
    // A rec that is both unwritable and runtime-dangerous counts only as
    // unwritable, so the two counts always add up to in_scope.len().
    let unwritable = in_scope.iter().filter(|r| !r.writable).count();
    let runtime_dangerous = in_scope
        .iter()
        .filter(|r| r.writable && category::is_runtime_dangerous(&r.param))
        .count();
    Some((
        json!({
            "status": "blocked",
            "applied": 0,
            "recommendations": in_scope.len(),
            "blocked_unwritable": unwritable,
            "blocked_runtime_dangerous": runtime_dangerous,
        }),
        // Mirror check's exit-1 "has recommendations" convention: the system
        // is not optimal, tune simply cannot act on it in this environment.
        1,
    ))
}

/// JSON body of the `tune --dry-run` preview. Pure so the status contract
/// stays unit-testable without a live host.
///
/// The preview carries the same status vocabulary as the short-circuit path:
/// reaching here means at least one recommendation is applicable, so the
/// status is `"planned"` — never `"optimal"`, which the short-circuit path
/// reserves for a host with nothing to recommend. `blocked` counts the
/// recommendations this environment filtered out (unwritable or
/// runtime-dangerous), so a script can tell a partial plan from a complete
/// one without diffing `would_apply`.
fn dry_run_output(applicable: &[Recommendation], requested: usize) -> serde_json::Value {
    let recs_json: Vec<serde_json::Value> = applicable.iter().map(rec_json).collect();
    json!({
        "dry_run": true,
        "status": "planned",
        "blocked": requested - applicable.len(),
        "would_apply": recs_json,
    })
}

fn cmd_tune(dry_run: bool, conservative: bool, cat: Option<String>) -> Result<i32> {
    if !dry_run {
        let is_root = unsafe { libc::geteuid() } == 0;
        if !is_root {
            anyhow::bail!("tune requires root (sudo ktuner tune)");
        }
    }

    if let Some(ref c) = cat {
        category::validate_category(c)?;
    }
    let (_info, eval) = gather()?;
    let score_before = eval.score();
    let mut recs = eval.recommendations;
    if let Some(ref c) = cat {
        recs = category::filter_by_category(recs, c);
    }
    if conservative {
        recs.retain(|r| r.confidence == rules::Confidence::High);
    }
    // Keep the pre-filter set so the early exit can distinguish "nothing to
    // recommend" from "recommended, but nothing applicable here" (e.g. root
    // in a container with read-only /proc/sys, where every rec is refreshed
    // as unwritable) — reporting optimal in the latter case contradicts
    // check's exit 1 on the same host.
    let applicable: Vec<Recommendation> = recs
        .iter()
        .filter(|r| r.writable && !category::is_runtime_dangerous(&r.param))
        .cloned()
        .collect();
    let requested = recs.len();
    if let Some((output, code)) = tune_short_circuit(&recs, applicable.len()) {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(code);
    }
    let recs = applicable;

    if dry_run {
        let output = dry_run_output(&recs, requested);
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(0);
    }

    let outcome = tuner::apply_quiet(&recs)?;
    let (_, eval_after) = gather()?;
    let score_after = eval_after.score();

    let failed: Vec<serde_json::Value> = outcome
        .failed
        .iter()
        .map(|f| json!({ "param": f.param, "error": f.error }))
        .collect();
    // Disjoint from `failed`: parameters the kernel accepted but adjusted.
    // They ARE applied (with the kernel's value) and are recorded in the
    // rollback ledger / sysctl.d; the note makes the delta visible (#4160).
    let clamped = serde_json::to_value(&outcome.clamped)?;
    let output = json!({
        "applied": outcome.applied,
        "failed": failed,
        "clamped": clamped,
        "score_before": score_before,
        "score_after": score_after,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    // Mirror `check`'s exit convention (1 = attention needed): a tune that
    // failed some or all writes must not report success — the old code exited
    // 0 even when every write failed (e.g. read-only /proc/sys in a container).
    // A clamped write is NOT a failure: the change took effect.
    Ok(if outcome.failed.is_empty() { 0 } else { 1 })
}

/// Normalize a user-supplied parameter name for lookup: sysfs names
/// (`block/...`, `transparent_hugepage/...`) are filesystem identities and
/// must stay verbatim, while sysctl names accept slash/dot and case
/// variants.
fn normalize_param(param: &str) -> String {
    if param.starts_with("block/") || param.starts_with("transparent_hugepage/") {
        param.to_string()
    } else {
        param.replace('/', ".").to_lowercase()
    }
}

/// Find the recommendation for a user-supplied parameter name, accepting the
/// same aliases in every consumer (why / fix): the verbatim form first, then
/// the normalized form. Returns `None` when no recommendation matches.
fn find_recommendation<'a>(
    eval: &'a rules::EvalResult,
    param: &str,
) -> Option<&'a rules::Recommendation> {
    let normalized = normalize_param(param);
    eval.recommendations
        .iter()
        .find(|r| r.param == param || r.param == normalized)
}

fn cmd_fix(param: &str) -> Result<i32> {
    let is_root = unsafe { libc::geteuid() } == 0;
    if !is_root {
        anyhow::bail!("fix requires root (sudo ktuner fix {param})");
    }
    let (_, eval) = gather()?;
    // Same alias policy as why_with: sysfs names are filesystem identities,
    // while sysctl names accept slash/dot and case variants. Without this,
    // `ktuner why vm/swappiness` succeeds but `ktuner fix vm/swappiness`
    // reports "parameter not found".
    let rec = find_recommendation(&eval, param)
        .ok_or_else(|| anyhow::anyhow!("parameter not found or already optimal: {param}"))?;
    if !rec.writable {
        anyhow::bail!("parameter {param} is read-only in this environment");
    }
    if category::is_runtime_dangerous(&rec.param) {
        anyhow::bail!(
            "parameter {param} is dangerous to write at runtime, persist to /etc/sysctl.d instead"
        );
    }
    let fix_outcome = tuner::apply_one(rec)?;
    let (_, eval_after) = gather()?;
    let mut output = json!({
        "fixed": param,
        "previous": rec.current_value,
        // What the kernel actually took — equals the recommendation unless
        // the kernel clamped/normalized the write (#4160).
        "applied": fix_outcome.effective,
        "score_after": eval_after.score(),
        "remaining": eval_after.recommendations.len(),
    });
    if fix_outcome.clamped {
        output["requested"] = json!(rec.recommended_value);
        output["note"] = json!("内核实际生效值与推荐值不同（已按实际生效值记录并持久化，可回滚）");
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(0)
}

fn cmd_why(param: &str) -> Result<i32> {
    let (_, eval) = gather()?;
    let (output, code) = why_with(param, &eval, |path| {
        let path = std::path::Path::new(path);
        if !path.exists() {
            return Ok(None);
        }
        // A failed read is an error, never a value: write-only sysctls
        // (mode 0200, e.g. vm.compact_memory) and transient EIO must not
        // collapse into an empty "current".
        std::fs::read_to_string(path).map(Some)
    })?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(code)
}

fn why_with(
    param: &str,
    eval: &rules::EvalResult,
    read_current: impl FnOnce(&str) -> Result<Option<String>, std::io::Error>,
) -> Result<(serde_json::Value, i32)> {
    // sysfs names are filesystem identities, while sysctl names accept aliases.
    let normalized = normalize_param(param);
    if let Some(rec) = find_recommendation(eval, param) {
        let output = json!({
            "param": rec.param,
            "current": rec.current_value,
            "recommended": rec.recommended_value,
            "reason": rec.reason,
            "confidence": format!("{:?}", rec.confidence).to_lowercase(),
            "category": format!("{:?}", rec.category).to_lowercase(),
            "subcategory": category::param_subcategory(&rec.param),
            "writable": rec.writable,
        });
        return Ok((output, 1));
    }
    let path = tuner::param_to_path(&normalized);
    match read_current(&path) {
        Ok(Some(val)) => {
            let output = json!({ "param": normalized, "current": val.trim(), "status": "optimal" });
            Ok((output, 0))
        }
        Ok(None) => anyhow::bail!("parameter not found: {param}"),
        // README exit-code contract: 2 = error, details in stderr JSON.
        // Claiming "optimal" from a value that was never read would make
        // the structured output untrustworthy for agents.
        Err(err) => anyhow::bail!("cannot read current value of {param} at {path}: {err}"),
    }
}

fn cmd_rollback() -> Result<i32> {
    let is_root = unsafe { libc::geteuid() } == 0;
    if !is_root {
        anyhow::bail!("rollback requires root (sudo ktuner rollback)");
    }
    let outcome = tuner::rollback_quiet()?;
    let status = tuner::classify_rollback(&outcome);
    let output = json!({
        "restored": outcome.restored,
        "failed": outcome.failed,
        "skipped": outcome.skipped,
        "status": format!("{status:?}"),
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(if outcome.is_complete() { 0 } else { 1 })
}

fn gather() -> Result<(detect::SystemInfo, rules::EvalResult)> {
    let info = detect::gather_system_info()?;
    let eval = evaluate(&info)?;
    Ok((info, eval))
}

fn rec_json(r: &Recommendation) -> serde_json::Value {
    json!({
        "param": r.param,
        "current": r.current_value,
        "recommended": r.recommended_value,
        "reason": r.reason,
        "confidence": format!("{:?}", r.confidence).to_lowercase(),
        "category": format!("{:?}", r.category).to_lowercase(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn rec(param: &str, writable: bool) -> Recommendation {
        Recommendation {
            param: param.to_string(),
            writable,
            ..Default::default()
        }
    }

    #[test]
    fn tune_short_circuit_optimal_when_nothing_recommended() {
        // True optimal: no recommendations in scope at all — the output and
        // exit code must stay byte-identical to today's.
        let (output, code) = tune_short_circuit(&[], 0).expect("must short-circuit");
        assert_eq!(code, 0);
        assert_eq!(output, json!({ "status": "optimal", "applied": 0 }));
    }

    #[test]
    fn tune_short_circuit_blocked_when_all_unwritable() {
        // The read-only-/proc/sys container case: three recommendations exist,
        // every one refreshed as unwritable, so tune cannot act — and must
        // not claim the system is optimal while `check` exits 1.
        let recs = vec![
            rec("vm.swappiness", false),
            rec("fs.file-max", false),
            rec("net.core.somaxconn", false),
        ];
        let (output, code) = tune_short_circuit(&recs, 0).expect("must short-circuit when blocked");
        assert_eq!(code, 1);
        assert_eq!(
            output,
            json!({
                "status": "blocked",
                "applied": 0,
                "recommendations": 3,
                "blocked_unwritable": 3,
                "blocked_runtime_dangerous": 0,
            })
        );
    }

    #[test]
    fn tune_short_circuit_blocked_when_only_rec_is_dangerous() {
        // A host whose only recommendation is the runtime-dangerous
        // vm.nr_hugepages: writable, but excluded from runtime writes.
        let recs = vec![rec("vm.nr_hugepages", true)];
        let (output, code) = tune_short_circuit(&recs, 0).expect("must short-circuit when blocked");
        assert_eq!(code, 1);
        assert_eq!(output["status"], json!("blocked"));
        assert_eq!(output["blocked_runtime_dangerous"], json!(1));
        assert_eq!(output["blocked_unwritable"], json!(0));
    }

    #[test]
    fn tune_short_circuit_blocked_counts_partition_exactly() {
        // 2 unwritable + 1 writable-but-dangerous: the counts must partition
        // in_scope, and a rec that is BOTH unwritable and dangerous counts
        // once, as unwritable.
        let recs = vec![
            rec("vm.swappiness", false),
            rec("fs.file-max", false),
            rec("vm.nr_hugepages", true),
            rec("kernel.shmmax", false), // dangerous AND unwritable
        ];
        let (output, _) = tune_short_circuit(&recs, 0).expect("blocked");
        assert_eq!(output["recommendations"], json!(4));
        assert_eq!(output["blocked_unwritable"], json!(3));
        assert_eq!(output["blocked_runtime_dangerous"], json!(1));
        assert_eq!(
            output["blocked_unwritable"].as_u64().unwrap()
                + output["blocked_runtime_dangerous"].as_u64().unwrap(),
            output["recommendations"].as_u64().unwrap()
        );
    }

    #[test]
    fn tune_short_circuit_proceeds_when_anything_applicable() {
        // The mixed case must NOT short-circuit: tune still applies what it
        // can and reports per-parameter outcomes for the rest.
        let recs = vec![
            rec("vm.swappiness", false),
            rec("fs.file-max", false),
            rec("net.core.somaxconn", true),
        ];
        assert!(tune_short_circuit(&recs, 1).is_none());
    }

    #[test]
    fn dry_run_output_reports_planned_and_blocked_count() {
        // Two applicable recommendations out of three gathered: the preview
        // must carry the short-circuit status vocabulary ("planned", never
        // "optimal") and the count this environment filtered out, so a script
        // can tell a partial plan from a complete one without diffing
        // would_apply.
        let recs = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let output = dry_run_output(&recs, 3);
        assert_eq!(output["dry_run"], json!(true));
        assert_eq!(output["status"], json!("planned"));
        assert_eq!(output["blocked"], json!(1));
        assert_eq!(output["would_apply"].as_array().map(Vec::len), Some(2));
        // A fully applicable plan reports zero blocked.
        let full = dry_run_output(&recs, 2);
        assert_eq!(full["status"], json!("planned"));
        assert_eq!(full["blocked"], json!(0));
    }

    #[test]
    fn tune_short_circuit_empty_after_category_filter_is_optimal() {
        // "blocked" only fires when recommendations exist IN SCOPE, so
        // `--category net` on a net-clean host keeps today's optimal output.
        let (output, code) = tune_short_circuit(&[], 0).expect("short-circuits");
        assert_eq!(code, 0);
        assert_eq!(output["status"], json!("optimal"));
    }

    struct CurrentFile(PathBuf);

    impl CurrentFile {
        fn new(value: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "ktuner-why-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .unwrap();
            file.write_all(value.as_bytes()).unwrap();
            Self(path)
        }

        fn read_for(&self, actual_path: &str, expected_path: &str) -> Option<String> {
            (actual_path == expected_path).then(|| std::fs::read_to_string(&self.0).unwrap())
        }
    }

    impl Drop for CurrentFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn evaluation(recommendations: Vec<Recommendation>) -> rules::EvalResult {
        rules::EvalResult {
            recommendations,
            total_checked: 1,
        }
    }

    #[test]
    fn fix_and_why_share_alias_lookup() {
        // The lookup helper must accept the sysctl alias forms in every
        // consumer: without this, `ktuner why vm/swappiness` found the
        // recommendation while `ktuner fix vm/swappiness` reported
        // "parameter not found".
        let rec = Recommendation {
            param: "vm.swappiness".to_string(),
            current_value: "60".to_string(),
            recommended_value: "1".to_string(),
            writable: true,
            ..Default::default()
        };
        let eval = evaluation(vec![rec]);
        for alias in [
            "vm.swappiness",
            "vm/swappiness",
            "VM.Swappiness",
            "vm/swappiness",
        ] {
            assert!(
                find_recommendation(&eval, alias).is_some(),
                "alias {alias} must resolve to the vm.swappiness recommendation"
            );
        }
        // Sysfs identities are not rewritten: no dot/case folding.
        let sysfs = Recommendation {
            param: "block/sda/scheduler".to_string(),
            current_value: "noop".to_string(),
            recommended_value: "none".to_string(),
            writable: true,
            ..Default::default()
        };
        let eval = evaluation(vec![sysfs]);
        assert!(find_recommendation(&eval, "block/sda/scheduler").is_some());
        assert!(
            find_recommendation(&eval, "block.sda.scheduler").is_none(),
            "sysfs names must not be dot-folded into a match"
        );
        // Unknown params resolve to nothing.
        assert!(find_recommendation(&eval, "no/such/param").is_none());
    }

    #[test]
    fn why_reads_sysfs_fallback_without_rewriting_identity() {
        let eval = evaluation(Vec::new());
        for (param, path, value) in [
            (
                "transparent_hugepage/enabled",
                "/sys/kernel/mm/transparent_hugepage/enabled",
                "always [madvise] never\n",
            ),
            (
                "transparent_hugepage/defrag",
                "/sys/kernel/mm/transparent_hugepage/defrag",
                "always defer defer+madvise [madvise] never\n",
            ),
            (
                "block/Disk.0/scheduler",
                "/sys/block/Disk.0/queue/scheduler",
                "[none] mq-deadline\n",
            ),
        ] {
            let current = CurrentFile::new(value);
            let (output, code) =
                why_with(param, &eval, |actual| Ok(current.read_for(actual, path)))
                    .expect("existing sysfs parameter must be readable without a recommendation");
            assert_eq!(code, 0);
            assert_eq!(
                output,
                json!({ "param": param, "current": value.trim(), "status": "optimal" })
            );
        }
    }

    #[test]
    fn why_keeps_sysctl_aliases_in_current_value_fallback() {
        let eval = evaluation(Vec::new());
        let current = CurrentFile::new("Linux\n");
        for param in [
            "kernel.ostype",
            "kernel/ostype",
            "KERNEL.OSTYPE",
            "KERNEL/OSTYPE",
        ] {
            let (output, code) = why_with(param, &eval, |path| {
                Ok(current.read_for(path, "/proc/sys/kernel/ostype"))
            })
            .expect("sysctl spellings must resolve to the same parameter");
            assert_eq!(code, 0);
            assert_eq!(
                output,
                json!({ "param": "kernel.ostype", "current": "Linux", "status": "optimal" })
            );
        }
    }

    #[test]
    fn why_keeps_recommendation_matching_and_output() {
        for (query, param, subcategory) in [
            (
                "transparent_hugepage/enabled",
                "transparent_hugepage/enabled",
                "io",
            ),
            ("block/Disk.0/scheduler", "block/Disk.0/scheduler", "io"),
            ("vm.swappiness", "vm.swappiness", "memory"),
            ("VM/SWAPPINESS", "vm.swappiness", "memory"),
        ] {
            let eval = evaluation(vec![Recommendation {
                param: param.to_string(),
                current_value: "before".to_string(),
                recommended_value: "after".to_string(),
                reason: "existing reason".to_string(),
                writable: true,
                ..Default::default()
            }]);
            let (output, code) = why_with(query, &eval, |_| {
                panic!("a recommendation must not fall through to a filesystem read")
            })
            .unwrap();
            assert_eq!(code, 1);
            assert_eq!(
                output,
                json!({
                    "param": param,
                    "current": "before",
                    "recommended": "after",
                    "reason": "existing reason",
                    "confidence": "high",
                    "category": "performance",
                    "subcategory": subcategory,
                    "writable": true,
                })
            );
        }
    }

    #[test]
    fn why_does_not_invent_sysfs_aliases_or_missing_parameters() {
        let eval = evaluation(Vec::new());
        let current = CurrentFile::new("[none] mq-deadline\n");
        let thp_current = CurrentFile::new("always [madvise] never\n");
        for param in [
            "block/disk.0/scheduler",
            "BLOCK/Disk.0/scheduler",
            "block.Disk.0.scheduler",
            "transparent_hugepage.ENABLED",
            "transparent_hugepage/ENABLED",
            "TRANSPARENT_HUGEPAGE/enabled",
            "no_such_ktuner_parameter",
        ] {
            let error = why_with(param, &eval, |path| {
                Ok(current
                    .read_for(path, "/sys/block/Disk.0/queue/scheduler")
                    .or_else(|| {
                        thp_current.read_for(path, "/sys/kernel/mm/transparent_hugepage/enabled")
                    }))
            })
            .expect_err("only the exact existing sysfs spelling may be accepted");
            assert_eq!(error.to_string(), format!("parameter not found: {param}"));
        }
    }

    #[test]
    fn why_reports_read_failures_as_errors_not_optimal() {
        let eval = evaluation(Vec::new());
        // Write-only sysctls (vm.compact_memory, vm.drop_caches,
        // net.ipv4.route.flush — mode 0200, present on every kernel) and
        // transient EIO/EACCES must surface as exit-2 errors per the README
        // contract, never as {"current":"","status":"optimal"} with code 0.
        let error = why_with("vm.compact_memory", &eval, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            ))
        })
        .expect_err("a failed read must not masquerade as a value");
        let msg = error.to_string();
        assert!(
            msg.contains("cannot read current value") && msg.contains("vm.compact_memory"),
            "error must name the parameter and the failed read: {msg}"
        );
    }
}
