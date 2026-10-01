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
    let total_weight: usize = recs
        .iter()
        .map(|r| match r.confidence {
            rules::Confidence::High => 3,
            rules::Confidence::Medium => 2,
        })
        .sum();
    let predicted_score = (score + total_weight).min(100);

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
    recs.retain(|r| r.writable && !category::is_runtime_dangerous(&r.param));

    if recs.is_empty() {
        let output = json!({ "status": "optimal", "applied": 0 });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(0);
    }

    if dry_run {
        let recs_json: Vec<serde_json::Value> = recs.iter().map(rec_json).collect();
        let output = json!({ "dry_run": true, "would_apply": recs_json });
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(0);
    }

    let applied = tuner::apply_quiet(&recs)?;
    let (_, eval_after) = gather()?;
    let score_after = eval_after.score();

    let output = json!({
        "applied": applied,
        "score_before": score_before,
        "score_after": score_after,
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(0)
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
    tuner::apply_one(rec)?;
    let (_, eval_after) = gather()?;
    let output = json!({
        "fixed": param,
        "previous": rec.current_value,
        "applied": rec.recommended_value,
        "score_after": eval_after.score(),
        "remaining": eval_after.recommendations.len(),
    });
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(0)
}

fn cmd_why(param: &str) -> Result<i32> {
    let (_, eval) = gather()?;
    let (output, code) = why_with(param, &eval, |path| {
        std::path::Path::new(path)
            .exists()
            .then(|| std::fs::read_to_string(path).unwrap_or_default())
    })?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(code)
}

fn why_with(
    param: &str,
    eval: &rules::EvalResult,
    read_current: impl FnOnce(&str) -> Option<String>,
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
    if let Some(val) = read_current(&path) {
        let output = json!({ "param": normalized, "current": val.trim(), "status": "optimal" });
        return Ok((output, 0));
    }
    anyhow::bail!("parameter not found: {param}")
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
    Ok(0)
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
            let (output, code) = why_with(param, &eval, |actual| current.read_for(actual, path))
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
                current.read_for(path, "/proc/sys/kernel/ostype")
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
                current
                    .read_for(path, "/sys/block/Disk.0/queue/scheduler")
                    .or_else(|| {
                        thp_current.read_for(path, "/sys/kernel/mm/transparent_hugepage/enabled")
                    })
            })
            .expect_err("only the exact existing sysfs spelling may be accepted");
            assert_eq!(error.to_string(), format!("parameter not found: {param}"));
        }
    }
}
