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
        /// Apply everything but this parameter; may be repeated
        #[arg(long)]
        exclude: Vec<String>,
    },
    /// Fix a single parameter
    Fix {
        param: String,
        /// Show what this fix would write, without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Explain why a parameter should be changed
    Why { param: String },
    /// Roll back all applied changes, or one or more recorded parameters
    Rollback {
        /// Restore only these recorded parameters, leaving the other entries
        /// in the ledger in place
        params: Vec<String>,
        /// Show what a rollback would restore, without changing anything;
        /// with parameters, only the entries restoring them would touch
        #[arg(long)]
        list: bool,
    },
}

fn main() {
    // README's JSON output contract: "Errors go to stderr as JSON." A usage
    // error fires before any command runs, so parsing must not exit through
    // clap's own plain-text renderer — an agent reading stderr JSON (the
    // documented shape) has to be able to read this one too. `--help` and
    // `--version` are not errors: clap keeps rendering them on stdout with
    // exit 0.
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) if e.use_stderr() => {
            let out = json!({ "error": e.to_string().trim_end() });
            print_error_json(&out);
            std::process::exit(e.exit_code());
        }
        Err(e) => {
            // --help / --version text, which a pipeline may truncate: the same
            // closed-stdout handling the result bodies get. A write error that
            // is NOT a closed pipe is a real error, so it takes the README's
            // error shape (stderr JSON, exit 2) instead of exiting 0 on text
            // that never reached the consumer.
            if let Err(error) = write_stdout(&e.to_string()) {
                let out = error_body(&error);
                print_error_json(&out);
                std::process::exit(2);
            }
            std::process::exit(e.exit_code());
        }
    };
    let result = match cli.command {
        Commands::Check {
            category: cat,
            conservative,
        } => cmd_check(cat, conservative),
        Commands::Tune {
            dry_run,
            conservative,
            category: cat,
            exclude,
        } => cmd_tune(dry_run, conservative, cat, exclude),
        Commands::Fix { param, dry_run } => cmd_fix(&param, dry_run),
        Commands::Why { param } => cmd_why(&param),
        Commands::Rollback { params, list } => cmd_rollback(&params, list),
    };
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            let out = error_body(&e);
            print_error_json(&out);
            std::process::exit(2);
        }
    }
}

/// The stderr body a command error reports: `main` renders every command
/// failure through here, so a caller — including the tests — can byte-compare
/// what two failure modes would print.
fn error_body(error: &anyhow::Error) -> serde_json::Value {
    json!({ "error": format!("{error:#}") })
}

/// Print a command's JSON body, treating a closed stdout as a graceful stop
/// instead of a panic.
///
/// `ktuner check | head -1` used to abort with "failed printing to stdout:
/// Broken pipe" and exit 101 — the status the README reserves for the
/// command's own verdict — because Rust's `println!` panics on a write error.
/// A consumer that stopped reading is a pipeline condition, not a crash: the
/// work is done by the time the report is rendered, so the command finishes
/// and answers with the exit code its own verdict earned. The sibling anolisa
/// CLI was filed with this exact symptom and fixed the same way. A write
/// error that is NOT a broken pipe (a full disk behind a redirect) stays a
/// real error and surfaces as the README's stderr JSON body, exit 2.
fn print_json(value: &serde_json::Value) -> Result<()> {
    write_stdout(&serde_json::to_string_pretty(value)?)
}

/// Write one already-rendered stdout body. See [`print_json`] for the
/// broken-pipe policy.
fn write_stdout(rendered: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    match std::io::Write::write_fmt(&mut stdout, format_args!("{rendered}\n")) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(|error| anyhow::anyhow!("cannot write to stdout: {error}")),
    }
}

/// Report a command's error body on stderr.
///
/// The body is the detail; the exit status is the machine-readable verdict,
/// and the README documents the error code as a contract. A failed write here
/// must therefore not replace that status: `eprintln!` panics on a write error
/// (a full filesystem behind a redirected log, a pipe whose reader left) and
/// answered 101 instead of the documented error code.
fn print_error_json(body: &serde_json::Value) {
    let rendered = serde_json::to_string_pretty(body).unwrap_or_else(|_| body.to_string());
    let mut stderr = std::io::stderr().lock();
    let _ = std::io::Write::write_fmt(&mut stderr, format_args!("{rendered}\n"));
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
    let predicted_score = predicted_score(&eval, &recs);

    let recs_json: Vec<serde_json::Value> = recs.iter().map(rec_json).collect();

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
    print_json(&output)?;

    let code = if recs.is_empty() { 0 } else { 1 };
    Ok(code)
}

/// Why a real `tune` run leaves a recommendation out: the parameter is not
/// writable here. A parameter that is both unwritable and runtime-dangerous
/// is reported as `unwritable` only, so the reasons partition the skipped set
/// exactly once — the counts in `tune_short_circuit` and the entries
/// `would_skip` lists both rely on that.
const UNWRITABLE: &str = "unwritable";
/// Why a real `tune` run leaves a writable recommendation out: writing it at
/// runtime is unsafe.
const RUNTIME_DANGEROUS: &str = "runtime_dangerous";
/// Why a real `tune` run leaves out a recommendation the operator named in
/// `--exclude`. Unlike the other two this is the operator's own instruction,
/// so it is reported first and for every matching entry: a parameter the run
/// was told to leave alone is dropped whether or not this environment could
/// have written it.
const EXCLUDED: &str = "excluded";

/// Whether a user-selected parameter name (`fix <param>`, `why <param>`,
/// `tune --exclude <param>`) addresses `candidate`: the verbatim spelling
/// first, then the sysctl alias (slash/dot and case) with sysfs identities
/// and network interface names preserved. One predicate so every consumer
/// accepts exactly the same spellings.
fn param_matches(candidate: &str, requested: &str) -> bool {
    candidate == requested || normalize_param(candidate) == normalize_param(requested)
}

/// Whether `--exclude` names this parameter.
fn is_excluded(param: &str, excludes: &[String]) -> bool {
    excludes.iter().any(|exclude| param_matches(param, exclude))
}

/// Why a real `tune` run would leave `rec` out, or `None` when this
/// environment can take it. Pure, so the applicability filter, the
/// short-circuit counts and the dry-run preview classify one list
/// identically. The operator's own exclusion outranks the environment
/// reasons: it is the reason THIS run drops the entry.
fn skip_reason(rec: &Recommendation, excludes: &[String]) -> Option<&'static str> {
    if is_excluded(&rec.param, excludes) {
        Some(EXCLUDED)
    } else if !rec.writable {
        Some(UNWRITABLE)
    } else if category::is_runtime_dangerous(&rec.param) {
        Some(RUNTIME_DANGEROUS)
    } else {
        None
    }
}

/// What `score` would report once this environment's plan has run: the
/// penalty of everything the plan leaves, i.e. the findings outside `view`
/// plus the entries `skip_reason` says no write path will take. Not
/// `score + shown weight`: `score` is floored at 30, so adding the shown
/// weight counts that floor as a gain and promises points the untouched
/// findings still keep off the board (on a 66-finding host, `--conservative`
/// promised 72 while applying it lands on the floor, 30). And not the whole
/// view either, which is the same promise one step further out: counting the
/// skipped entries predicted a score `tune` cannot deliver — on a container
/// whose /proc/sys is read-only every entry is skipped and `check` reported
/// the tuned score while the plan answered "blocked, applied 0".
fn predicted_score(eval: &rules::EvalResult, view: &[Recommendation]) -> usize {
    let applicable: Vec<Recommendation> = view
        .iter()
        .filter(|rec| skip_reason(rec, &[]).is_none())
        .cloned()
        .collect();
    eval.score_after_applying(&applicable)
}

/// The `would_skip` payload: every in-scope recommendation a real run would
/// leave out, in plan order, with the reason it is left out.
fn would_skip_json(in_scope: &[Recommendation], excludes: &[String]) -> Vec<serde_json::Value> {
    in_scope
        .iter()
        .filter_map(|rec| {
            skip_reason(rec, excludes).map(|reason| json!({ "param": rec.param, "reason": reason }))
        })
        .collect()
}

/// The `unmatched_exclude` report: the `--exclude` names that dropped nothing
/// from this run's plan, in the order given. A name that matches no in-scope
/// recommendation — a typo, or one outside `--category`/`--conservative` — is
/// not an error (the same command is scripted across hosts whose plans
/// differ), but an inert exclusion must be visible or the caller cannot tell
/// that its instruction did nothing. An empty plan drops nothing, so every
/// given name is reported there too. `None` when every name matched: the key
/// appears only when there is something to report, so a run without
/// `--exclude` (or with every exclusion effective) keeps its exact old shape.
fn unmatched_exclude_json(
    in_scope: &[Recommendation],
    excludes: &[String],
) -> Option<serde_json::Value> {
    let unmatched: Vec<&str> = excludes
        .iter()
        .filter(|exclude| {
            !in_scope
                .iter()
                .any(|rec| param_matches(&rec.param, exclude))
        })
        .map(String::as_str)
        .collect();
    (!unmatched.is_empty()).then(|| json!(unmatched))
}

/// Why `ktuner tune` exited without applying anything. Pure so the
/// status/exit-code decision is unit-testable without touching the system.
///
/// `in_scope` is the recommendation list after the category/conservative
/// filters but BEFORE the skip classification; `applicable` is the number
/// that survives it (writable, not runtime-dangerous, and not excluded by the
/// operator). Returns `None` when tune should proceed (at least one
/// applicable recommendation).
fn tune_short_circuit(
    in_scope: &[Recommendation],
    applicable: usize,
    excludes: &[String],
) -> Option<(serde_json::Value, i32)> {
    if applicable > 0 {
        return None;
    }
    if in_scope.is_empty() {
        // Genuinely nothing to recommend in scope — unchanged output and code.
        // An --exclude name cannot have dropped anything from an empty plan,
        // so every given name is inert here too: the report follows the same
        // one rule on every shape — a name that removed nothing from this
        // run's plan appears in `unmatched_exclude` — instead of the key
        // vanishing exactly on the hosts with the least to reconcile.
        let mut body = json!({ "status": "optimal", "applied": 0 });
        if let Some(unmatched) = unmatched_exclude_json(in_scope, excludes) {
            body["unmatched_exclude"] = unmatched;
        }
        return Some((body, 0));
    }
    // Recommendations exist but every one was filtered out before any write.
    // Reporting "optimal" here is false: `check` exits 1 on the same host.
    // skip_reason classifies each rec exactly once, so the counts always add
    // up to in_scope.len(); `blocked_excluded` joins the partition when
    // --exclude dropped something. A non-dry-run tune answers with this body
    // too, and the counts alone cannot be reconciled with what `check` keeps
    // reporting — `would_skip` names the entries, same shape as the preview.
    let unwritable = in_scope
        .iter()
        .filter(|r| skip_reason(r, excludes) == Some(UNWRITABLE))
        .count();
    let runtime_dangerous = in_scope
        .iter()
        .filter(|r| skip_reason(r, excludes) == Some(RUNTIME_DANGEROUS))
        .count();
    let excluded = in_scope
        .iter()
        .filter(|r| skip_reason(r, excludes) == Some(EXCLUDED))
        .count();
    let mut body = json!({
        "status": "blocked",
        "applied": 0,
        "recommendations": in_scope.len(),
        "blocked_unwritable": unwritable,
        "blocked_runtime_dangerous": runtime_dangerous,
        "would_skip": would_skip_json(in_scope, excludes),
    });
    // The --exclude-only keys, and only when they have something to report: a
    // run without the flag keeps the exact shape its consumers already read.
    if excluded > 0 {
        body["blocked_excluded"] = json!(excluded);
    }
    if let Some(unmatched) = unmatched_exclude_json(in_scope, excludes) {
        body["unmatched_exclude"] = unmatched;
    }
    Some((
        body,
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
/// reserves for a host with nothing to recommend. `would_apply` lists the
/// entries a real run would write; `would_skip` names the ones it would leave
/// out — this environment filtered them (unwritable, or runtime-dangerous) or
/// the operator excluded them — with the reason, so the parameters a partial
/// plan leaves behind are visible and not just counted. `blocked` stays their
/// count.
fn dry_run_output(
    in_scope: &[Recommendation],
    applicable: &[Recommendation],
    excludes: &[String],
) -> serde_json::Value {
    let recs_json: Vec<serde_json::Value> = applicable.iter().map(rec_json).collect();
    let would_skip = would_skip_json(in_scope, excludes);
    let mut body = json!({
        "dry_run": true,
        "status": "planned",
        "blocked": would_skip.len(),
        "would_apply": recs_json,
        "would_skip": would_skip,
    });
    if let Some(unmatched) = unmatched_exclude_json(in_scope, excludes) {
        body["unmatched_exclude"] = unmatched;
    }
    body
}

/// Extend a short-circuit body with the keys a `--dry-run` caller reads.
///
/// `--dry-run` answers with `dry_run`, `would_apply`, `would_skip` and
/// `blocked` on every host: the short-circuit shapes predate the flag, so a
/// host where every recommendation was filtered out answered with none of
/// them and a script could not tell that invocation from a non-dry-run one —
/// it read `would_apply`, found nothing and had no way to distinguish
/// "nothing to plan" from "the flag was ignored". `would_apply` is empty here
/// because nothing is applicable, `would_skip` lists everything in scope
/// (environment-filtered or operator-excluded), `blocked` stays its length
/// (the count the planned shape reports, and the sum of the short-circuit's
/// `blocked_unwritable` / `blocked_runtime_dangerous` / `blocked_excluded`
/// partitions), and `status` keeps the short-circuit vocabulary (`optimal` /
/// `blocked`). The short-circuit body already carries `unmatched_exclude`
/// when an `--exclude` name was inert, so the preview inherits it.
fn dry_run_preview(
    mut body: serde_json::Value,
    in_scope: &[Recommendation],
    excludes: &[String],
) -> serde_json::Value {
    if let Some(object) = body.as_object_mut() {
        object.insert("dry_run".to_string(), json!(true));
        object
            .entry("would_apply".to_string())
            .or_insert_with(|| json!([]));
        let would_skip = would_skip_json(in_scope, excludes);
        object.insert("blocked".to_string(), json!(would_skip.len()));
        object.insert("would_skip".to_string(), json!(would_skip));
    }
    body
}

/// JSON body of a real (non-dry-run) `tune` run. Pure so the body stays
/// unit-testable without root: a real `tune` only runs as root, and its
/// accounting — what a partial plan wrote, what failed, what the kernel
/// adjusted — must stay assertable on a dev host.
///
/// `would_skip` names the entries this run left out (unwritable,
/// runtime-dangerous, or excluded by the operator), in the dry-run preview's
/// shape: a partial tune answers exit 0 while `check` keeps exiting 1 on the
/// same host, and the filtered entries are exactly the difference between the
/// two — without them in the body, a caller cannot reconcile a real `tune`
/// with `check` or with its own dry-run preview.
fn tune_output(
    in_scope: &[Recommendation],
    outcome: &tuner::ApplyOutcome,
    score_before: usize,
    score_after: usize,
    excludes: &[String],
) -> serde_json::Value {
    let failed: Vec<serde_json::Value> = outcome
        .failed
        .iter()
        .map(|f| json!({ "param": f.param, "error": f.error }))
        .collect();
    // Disjoint from `failed`: parameters the kernel accepted but adjusted.
    // They ARE applied (with the kernel's value) and are recorded in the
    // rollback ledger / sysctl.d; the note makes the delta visible (#4160).
    let clamped = serde_json::to_value(&outcome.clamped).unwrap_or_else(|_| json!([]));
    let mut body = json!({
        "applied": outcome.applied,
        "failed": failed,
        "clamped": clamped,
        "score_before": score_before,
        "score_after": score_after,
        "would_skip": would_skip_json(in_scope, excludes),
    });
    if let Some(unmatched) = unmatched_exclude_json(in_scope, excludes) {
        body["unmatched_exclude"] = unmatched;
    }
    body
}

fn cmd_tune(
    dry_run: bool,
    conservative: bool,
    cat: Option<String>,
    exclude: Vec<String>,
) -> Result<i32> {
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
    // check's exit 1 on the same host. The dry-run preview lists the filtered
    // entries themselves through the same skip_reason, and `--exclude` enters
    // that classification here, after the category/conservative filters.
    let applicable: Vec<Recommendation> = recs
        .iter()
        .filter(|r| skip_reason(r, &exclude).is_none())
        .cloned()
        .collect();
    if let Some((output, code)) = tune_short_circuit(&recs, applicable.len(), &exclude) {
        let output = if dry_run {
            dry_run_preview(output, &recs, &exclude)
        } else {
            output
        };
        print_json(&output)?;
        return Ok(code);
    }

    if dry_run {
        let output = dry_run_output(&recs, &applicable, &exclude);
        print_json(&output)?;
        return Ok(0);
    }

    let outcome = tuner::apply_quiet(&applicable)?;
    let (_, eval_after) = gather()?;
    let score_after = eval_after.score();

    let output = tune_output(&recs, &outcome, score_before, score_after, &exclude);
    print_json(&output)?;
    // Mirror `check`'s exit convention (1 = attention needed): a tune that
    // failed some or all writes must not report success — the old code exited
    // 0 even when every write failed (e.g. read-only /proc/sys in a container).
    // A clamped write is NOT a failure: the change took effect.
    Ok(if outcome.failed.is_empty() { 0 } else { 1 })
}

/// Normalize a user-supplied parameter name for lookup: sysfs names
/// (`block/...`, `transparent_hugepage/...`) are filesystem identities and
/// must stay verbatim. Sysctl namespaces accept slash/dot and case variants,
/// but network interface identities retain their case and literal dots.
fn normalize_param(param: &str) -> String {
    if param.starts_with("block/") || param.starts_with("transparent_hugepage/") {
        return param.to_string();
    }
    for proto in ["ipv4", "ipv6"] {
        for family in ["conf", "neigh"] {
            let prefix = format!("net.{proto}.{family}.");
            if param
                .get(..prefix.len())
                .is_some_and(|p| p.replace('/', ".").eq_ignore_ascii_case(&prefix))
            {
                let rest = &param[prefix.len()..];
                if let Some((iface, property)) =
                    rest.rsplit_once('/').or_else(|| rest.rsplit_once('.'))
                {
                    return format!(
                        "net.{proto}.{family}.{iface}.{}",
                        property.to_ascii_lowercase()
                    );
                }
                return format!("net.{proto}.{family}.{rest}");
            }
        }
    }
    param.replace('/', ".").to_lowercase()
}

/// Find the recommendation for a user-supplied parameter name, accepting the
/// same aliases in every consumer (why / fix / `tune --exclude`): the verbatim
/// form first, then the normalized form. Returns `None` when no
/// recommendation matches.
fn find_recommendation<'a>(
    eval: &'a rules::EvalResult,
    param: &str,
) -> Option<&'a rules::Recommendation> {
    eval.recommendations
        .iter()
        .find(|r| param_matches(&r.param, param))
}

fn cmd_fix(param: &str, dry_run: bool) -> Result<i32> {
    let is_root = unsafe { libc::geteuid() } == 0;
    // Mirrors cmd_tune: the read-only preview needs no root, while a real run
    // keeps the gate exactly where it has always been — before any system
    // read, so a failing diagnosis can never replace the root error.
    fix_root_gate(param, dry_run, is_root)?;
    let (_, eval) = gather()?;
    fix_with(param, dry_run, is_root, &eval)
}

/// The root gate `fix` has always had: a real run needs root, the read-only
/// preview does not. One expression, so the mode the gate applies to and its
/// position (before any system read) cannot drift between the CLI entry point
/// and the decision function the tests drive.
fn fix_root_gate(param: &str, dry_run: bool, is_root: bool) -> Result<()> {
    if !dry_run && !is_root {
        anyhow::bail!("fix requires root (sudo ktuner fix {param})");
    }
    Ok(())
}

/// The recommendation `fix` would write, or the refusal the command answers
/// with. One classification for the real run and its `--dry-run` preview: the
/// lookup accepts the same spellings both use, and the environment refusals
/// come from the plan's own `skip_reason`, so the preview cannot disagree
/// with the command it previews.
fn fix_target<'a>(eval: &'a rules::EvalResult, param: &str) -> Result<&'a Recommendation> {
    let rec = find_recommendation(eval, param)
        .ok_or_else(|| anyhow::anyhow!("parameter not found or already optimal: {param}"))?;
    // The excludes are empty because a single fix has no --exclude, so only
    // the two environment reasons can come back — unwritable outranks
    // runtime-dangerous exactly as it does in the plan.
    match skip_reason(rec, &[]) {
        Some(UNWRITABLE) => anyhow::bail!("parameter {param} is read-only in this environment"),
        Some(RUNTIME_DANGEROUS) => anyhow::bail!(
            "parameter {param} is dangerous to write at runtime, persist to /etc/sysctl.d instead"
        ),
        _ => Ok(rec),
    }
}

/// `fix` over an already-gathered diagnosis: the shared classification, the
/// preview rendering and the real write. `cmd_fix` supplies the live root
/// fact and eval; a unit test drives both modes over one fixture and asserts
/// they refuse identically.
fn fix_with(param: &str, dry_run: bool, is_root: bool, eval: &rules::EvalResult) -> Result<i32> {
    // cmd_fix ran the same gate before gathering (that position is the
    // contract); stating it here keeps this function's verdict complete for
    // its callers — the unit tests drive it without an euid of their own.
    fix_root_gate(param, dry_run, is_root)?;
    let rec = fix_target(eval, param)?;
    if dry_run {
        // Read-only: no write, no ledger lock, no record, no persistence and
        // no cleanup — only the diagnosis above and the preview body.
        print_json(&fix_dry_run_output(rec))?;
        return Ok(0);
    }
    let fix = tuner::apply_one(rec)?;
    let (_, eval_after) = gather()?;
    let mut output = json!({
        "fixed": param,
        // The original the ledger recorded under the transaction lock — the
        // value `ktuner rollback` restores. The gathered rec.current_value
        // can be stale (the knob moved between gather and apply), and
        // reporting it here contradicted the ledger and `rollback --list`.
        "previous": fix.recorded_previous,
        // What the kernel actually took — equals the recommendation unless
        // the kernel clamped/normalized the write (#4160).
        "applied": fix.outcome.effective,
        "score_after": eval_after.score(),
        "remaining": eval_after.recommendations.len(),
    });
    if fix.outcome.clamped {
        output["requested"] = json!(rec.recommended_value);
        output["note"] = json!("内核实际生效值与推荐值不同（已按实际生效值记录并持久化，可回滚）");
    }
    print_json(&output)?;
    Ok(0)
}

/// JSON body of `fix --dry-run` — the `tune --dry-run` shape scoped to one
/// parameter, so one parser reads both previews. Printed only when the shared
/// classification found a writable, runtime-safe recommendation: every
/// refusal answers with the plain command's error instead (same stderr JSON,
/// same exit code), so `status` is always `"planned"` and `would_skip` is
/// always empty here. The keys stay because the shape is the contract.
///
/// A write that zeroes the kernel's mutually exclusive twin names it in
/// `would_clear`, from the write path's own twin table ([`tuner::cleared_sibling`]):
/// the preview must not derive the pair relationship a second time. The key
/// is absent when the parameter has no twin.
fn fix_dry_run_output(rec: &Recommendation) -> serde_json::Value {
    let mut body = json!({
        "dry_run": true,
        "status": "planned",
        "blocked": 0,
        "would_apply": [rec_json(rec)],
        "would_skip": [],
    });
    if let Some(twin) = tuner::cleared_sibling(&rec.param) {
        body["would_clear"] = json!([twin]);
    }
    body
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
    print_json(&output)?;
    Ok(code)
}

/// The value a parameter currently holds, read the way every other consumer
/// reads it: the shared [`tuner::active_value`] — the same reading the rules
/// store as a recommendation's `current`, the ledger now records as an
/// original, and `classify_readback` returns as the effective value. Keeping
/// one implementation in the library is what makes the claim true: a second
/// copy here would drift the moment either side learned a new file shape.
fn active_value(value: &str) -> String {
    tuner::active_value(value)
}

fn why_with(
    param: &str,
    eval: &rules::EvalResult,
    read_current: impl FnOnce(&str) -> Result<Option<String>, std::io::Error>,
) -> Result<(serde_json::Value, i32)> {
    // sysfs names are filesystem identities, while sysctl names accept aliases.
    let normalized = normalize_param(param);
    if let Some(rec) = find_recommendation(eval, param) {
        let mut output = json!({
            "param": rec.param,
            "current": rec.current_value,
            "recommended": rec.recommended_value,
            "reason": rec.reason,
            "confidence": format!("{:?}", rec.confidence).to_lowercase(),
            "category": format!("{:?}", rec.category).to_lowercase(),
            "subcategory": category::param_subcategory(&rec.param),
            "writable": rec.writable,
        });
        // The same skip classification the plan uses (#6134's would_skip):
        // a recommendation `why` explains can be one no write path will ever
        // take — unwritable here, or runtime-dangerous, which tune skips and
        // fix refuses outright. Without the reason, `writable: true` on a
        // runtime-dangerous knob told the agent the opposite of what every
        // apply path does. `why` is read-only and has no `--exclude`, so no
        // exclusion applies to this classification.
        if let Some(reason) = skip_reason(rec, &[]) {
            output["skip_reason"] = json!(reason);
        }
        return Ok((output, 1));
    }
    let path = tuner::param_to_path(&normalized);
    match read_current(&path) {
        Ok(Some(val)) => {
            let output =
                json!({ "param": normalized, "current": active_value(&val), "status": "optimal" });
            Ok((output, 0))
        }
        Ok(None) => anyhow::bail!("parameter not found: {param}"),
        // README exit-code contract: 2 = error, details in stderr JSON.
        // Claiming "optimal" from a value that was never read would make
        // the structured output untrustworthy for agents.
        Err(err) => anyhow::bail!("cannot read current value of {param} at {path}: {err}"),
    }
}

/// JSON shape of `ktuner rollback --list`. Pure so the agent-facing contract
/// (key names, count, entry fields, ordering) is unit-testable without a
/// ledger on disk. Entries arrive as `rollback_preview` returns them, sorted
/// by param (BTreeMap order); `live` and `drifted` arrive as `Option`s so an
/// unreadable path publishes JSON `null` instead of a guessed value.
fn rollback_list_output(entries: &[tuner::RollbackPreview]) -> serde_json::Value {
    json!({
        "count": entries.len(),
        "pending": entries
            .iter()
            .map(|entry| {
                json!({
                    "param": entry.param,
                    "applied": entry.applied,
                    "previous": entry.previous,
                    "live": entry.live,
                    "drifted": entry.drifted,
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// JSON body of `ktuner rollback`. One builder for both shapes: a full
/// rollback keeps exactly the four keys it always had, while restoring a single
/// parameter adds `param` — the ledger key it retired, spelled the way
/// `rollback --list` publishes it, so the entry can be reconciled with the
/// preview. Keys stay in alphabetical order (serde_json's map is sorted):
/// `failed` < `param` < `restored` < `skipped` < `status`.
fn rollback_output(param: Option<&str>, outcome: &tuner::RollbackOutcome) -> serde_json::Value {
    let status = tuner::classify_rollback(outcome);
    let mut body = json!({
        "restored": outcome.restored,
        "failed": outcome.failed,
        "skipped": outcome.skipped,
        "status": format!("{status:?}"),
    });
    if let Some(param) = param {
        body["param"] = json!(param);
    }
    body
}

/// JSON body for two or more positionals: the same four keys and the same
/// status vocabulary, with `params` (an array) in place of `param` (a string).
/// The two never appear together, and nothing here is added to a body of one
/// or zero positionals.
///
/// `params` carries the ledger key each positional resolved to — the spelling
/// `rollback --list` publishes — in the order given and deduplicated, so a
/// caller can read back the batch it asked for. A mutually exclusive twin
/// restored with an entry is counted in `restored` but not named here, exactly
/// as the single-parameter body does not name it; whether the batch restored
/// everything is `status`'s answer, not the array's.
fn rollback_params_output(keys: &[String], outcome: &tuner::RollbackOutcome) -> serde_json::Value {
    let mut body = rollback_output(None, outcome);
    body["params"] = json!(keys);
    body
}

/// JSON body of `ktuner rollback` for the positionals the caller named.
///
/// The shape follows the INVOCATION, not how many ledger keys it resolved to:
/// exactly one positional keeps the body it has had since the positional was
/// introduced (byte for byte), while two or more carry `params` even when they
/// name one entry between them (`rollback vm/swappiness vm.swappiness`) —
/// a consumer that asked for a batch reads the batch shape, and one rule
/// covers every arity. No positional keeps the four-key full-rollback body.
fn rollback_body(
    positionals: usize,
    keys: &[String],
    outcome: &tuner::RollbackOutcome,
) -> serde_json::Value {
    match positionals {
        0 => rollback_output(None, outcome),
        1 => rollback_output(keys.first().map(String::as_str), outcome),
        _ => rollback_params_output(keys, outcome),
    }
}

/// The positional spellings `rollback` hands to the engine: every name goes
/// through normalize_param, the policy fix/why share, so the spellings this
/// command accepts cannot drift from those commands whichever position a name
/// sits in.
fn normalize_params(params: &[String]) -> Vec<String> {
    params.iter().map(|param| normalize_param(param)).collect()
}

fn cmd_rollback(params: &[String], list: bool) -> Result<i32> {
    let is_root = unsafe { libc::geteuid() } == 0;
    if !is_root {
        anyhow::bail!("rollback requires root (sudo ktuner rollback)");
    }
    if list {
        // Read-only: no writes, no ledger deletion, no systemd changes. The
        // ledger is 0600 in a 0700 root-owned dir, so --list shares
        // rollback's root requirement; a corrupt ledger surfaces as an error
        // here WITHOUT the destructive path having run first. Positionals
        // narrow the preview to the entries `rollback <param>...` would
        // restore, named the way fix/why/rollback normalize them; a name the
        // ledger does not record is the same command error the restore gives.
        let entries = if params.is_empty() {
            tuner::rollback_preview()?
        } else {
            tuner::rollback_preview_params(&normalize_params(params))?
        };
        let output = rollback_list_output(&entries);
        print_json(&output)?;
        return Ok(0);
    }
    if params.is_empty() {
        let outcome = tuner::rollback_quiet()?;
        print_json(&rollback_body(0, &[], &outcome))?;
        return Ok(rollback_exit_code(&outcome));
    }
    // The CLI owns the alias policy (fix/why normalize the same way); the
    // engine matches the normalized spellings against the ledger and answers
    // with one ledger key per positional, in the order they were given.
    let (keys, outcome) = tuner::rollback_params(&normalize_params(params))?;
    print_json(&rollback_body(params.len(), &keys, &outcome))?;
    Ok(rollback_exit_code(&outcome))
}

fn rollback_exit_code(outcome: &tuner::RollbackOutcome) -> i32 {
    if outcome.is_complete() {
        0
    } else {
        1
    }
}

fn gather() -> Result<(detect::SystemInfo, rules::EvalResult)> {
    let info = detect::gather_system_info()?;
    let eval = evaluate(&info)?;
    Ok((info, eval))
}

/// One builder for every agent-facing recommendation entry: `check`'s
/// `recommendations` and `tune --dry-run`'s `would_apply` serialize the same
/// object, so a consumer can reconcile the plan against the diagnosis. This
/// used to exist in two shapes — rec_json dropped `subcategory` and
/// `writable`, leaving dry-run entries with 6 keys where check emits 8 — so
/// an agent diffing the two views saw the same `param` under two schemas.
///
/// An entry no write path will take carries the same `skip_reason` the plan
/// publishes (`unwritable` or `runtime_dangerous`, from the shared helper
/// behind `would_skip`), so `check` — like `why` — never contradicts the
/// plan it is reconciled against. `check` is read-only and has no
/// `--exclude`, so no exclusion applies to this classification.
fn rec_json(r: &Recommendation) -> serde_json::Value {
    let mut value = json!({
        "param": r.param,
        "current": r.current_value,
        "recommended": r.recommended_value,
        "reason": r.reason,
        "confidence": format!("{:?}", r.confidence).to_lowercase(),
        "category": format!("{:?}", r.category).to_lowercase(),
        "subcategory": category::param_subcategory(&r.param),
        "writable": r.writable,
    });
    if let Some(reason) = skip_reason(r, &[]) {
        value["skip_reason"] = json!(reason);
    }
    value
}

#[cfg(test)]
mod tests {
    #[test]
    fn network_normalization_preserves_interface_identity() {
        for proto in ["ipv4", "ipv6"] {
            for iface in ["Br0", "Br0.100", "br0.100", "lo"] {
                for input in [
                    format!("net/{proto}/conf/{iface}/forwarding"),
                    format!("net.{proto}.conf.{iface}.forwarding"),
                    format!("NET/{proto}/CONF/{iface}/FORWARDING"),
                    format!("net/{proto}.conf/{iface}/forwarding"),
                ] {
                    assert_eq!(
                        normalize_param(&input),
                        format!("net.{proto}.conf.{iface}.forwarding")
                    );
                }
            }
        }
        assert_eq!(normalize_param("VM/SWAPPINESS"), "vm.swappiness");
        assert_eq!(
            normalize_param("block/Disk.0/scheduler"),
            "block/Disk.0/scheduler"
        );
    }

    #[test]
    fn network_lookup_does_not_match_a_different_interface() {
        let eval = rules::EvalResult {
            recommendations: vec![
                rec("net.ipv4.conf.br0.100.forwarding", true),
                rec("net/ipv4/conf/Br0.100/forwarding", true),
            ],
            total_checked: 2,
        };
        let found = find_recommendation(&eval, "net.ipv4.conf.Br0.100.forwarding").unwrap();
        assert_eq!(found.param, "net/ipv4/conf/Br0.100/forwarding");
        assert!(find_recommendation(&eval, "net.ipv4.conf.BR0.100.forwarding").is_none());
    }

    #[test]
    fn network_why_reads_the_requested_interface_path() {
        let eval = rules::EvalResult {
            recommendations: vec![],
            total_checked: 0,
        };
        for proto in ["ipv4", "ipv6"] {
            for input in [
                format!("net/{proto}/conf/Br0.100/forwarding"),
                format!("net.{proto}.conf.Br0.100.forwarding"),
            ] {
                let (output, code) = why_with(&input, &eval, |path| {
                    assert_eq!(
                        path,
                        format!("/proc/sys/net/{proto}/conf/Br0.100/forwarding")
                    );
                    Ok(Some("1\n".into()))
                })
                .unwrap();
                assert_eq!(code, 0);
                assert_eq!(output["current"], "1");
            }
        }
    }

    #[test]
    fn neighbour_normalization_preserves_interface_identity() {
        // The neighbour family has the same literal-dot interfaces as conf,
        // so the same identity rule applies: the interface segment keeps its
        // case and dots, the property is lower-cased.
        for proto in ["ipv4", "ipv6"] {
            for iface in ["Br0", "Br0.100", "br0.100", "lo"] {
                for input in [
                    format!("net/{proto}/neigh/{iface}/gc_thresh3"),
                    format!("net.{proto}.neigh.{iface}.gc_thresh3"),
                    format!("NET/{proto}/NEIGH/{iface}/GC_THRESH3"),
                    format!("net/{proto}.neigh/{iface}/gc_thresh3"),
                ] {
                    assert_eq!(
                        normalize_param(&input),
                        format!("net.{proto}.neigh.{iface}.gc_thresh3")
                    );
                }
            }
        }
    }

    #[test]
    fn neighbour_why_reads_the_requested_interface_path() {
        let eval = rules::EvalResult {
            recommendations: vec![],
            total_checked: 0,
        };
        for proto in ["ipv4", "ipv6"] {
            for input in [
                format!("net/{proto}/neigh/Br0.100/gc_thresh3"),
                format!("net.{proto}.neigh.Br0.100.gc_thresh3"),
            ] {
                let (output, code) = why_with(&input, &eval, |path| {
                    assert_eq!(
                        path,
                        format!("/proc/sys/net/{proto}/neigh/Br0.100/gc_thresh3")
                    );
                    Ok(Some("8192\n".into()))
                })
                .unwrap();
                assert_eq!(code, 0);
                assert_eq!(output["current"], "8192");
            }
        }
    }

    use super::*;

    #[test]
    fn rollback_exit_code_reports_complete_and_incomplete_outcomes() {
        for (name, restored, failed, skipped, expected) in [
            ("empty ledger", 0, 0, 0, 0),
            ("fully restored", 3, 0, 0, 0),
            ("all failed", 0, 2, 0, 1),
            ("all skipped", 0, 0, 2, 1),
            ("failed and skipped", 0, 1, 1, 1),
            ("partially failed", 2, 1, 0, 1),
            ("partially skipped", 2, 0, 1, 1),
            ("mixed incomplete", 2, 1, 1, 1),
        ] {
            let outcome = tuner::RollbackOutcome {
                restored,
                failed,
                skipped,
            };
            assert_eq!(rollback_exit_code(&outcome), expected, "{name}");
        }
    }

    fn preview_entry(
        param: &str,
        applied: &str,
        previous: &str,
        live: Option<&str>,
        drifted: Option<bool>,
    ) -> tuner::RollbackPreview {
        tuner::RollbackPreview {
            param: param.to_string(),
            applied: applied.to_string(),
            previous: previous.to_string(),
            live: live.map(str::to_string),
            drifted,
        }
    }

    #[test]
    fn rollback_list_output_empty() {
        // Empty ledger: count 0, empty pending — still a valid listing.
        assert_eq!(
            rollback_list_output(&[]),
            json!({ "count": 0, "pending": [] })
        );
    }

    #[test]
    fn rollback_list_output_maps_every_field() {
        let entries = vec![
            preview_entry("vm.swappiness", "1", "60", Some("1"), Some(false)),
            preview_entry("block/sda/scheduler", "none", "mq-deadline", None, None),
        ];
        assert_eq!(
            rollback_list_output(&entries),
            json!({
                "count": 2,
                "pending": [
                    {
                        "applied": "1",
                        "drifted": false,
                        "live": "1",
                        "param": "vm.swappiness",
                        "previous": "60",
                    },
                    {
                        "applied": "none",
                        "drifted": null,
                        "live": null,
                        "param": "block/sda/scheduler",
                        "previous": "mq-deadline",
                    },
                ]
            })
        );
    }

    /// The regression this feature owes: the two new keys are the ONLY change
    /// to the published shape. Stripping them from the output must reproduce,
    /// field for field, what `rollback --list` emitted before they existed.
    #[test]
    fn rollback_list_output_adds_only_live_and_drifted() {
        let entries = vec![
            preview_entry("vm.swappiness", "1", "60", Some("60"), Some(true)),
            preview_entry("block/sda/scheduler", "none", "mq-deadline", None, None),
        ];
        let mut out = rollback_list_output(&entries);
        for entry in out["pending"].as_array_mut().unwrap() {
            let object = entry.as_object_mut().unwrap();
            object.remove("live");
            object.remove("drifted");
        }
        assert_eq!(
            out,
            json!({
                "count": 2,
                "pending": [
                    { "param": "vm.swappiness", "applied": "1", "previous": "60" },
                    { "param": "block/sda/scheduler", "applied": "none", "previous": "mq-deadline" },
                ]
            })
        );
    }

    #[test]
    fn rollback_list_output_keys_are_alphabetical() {
        // The README's "Object keys are emitted in alphabetical order" is a
        // hard contract for consumers: the serialized entry must carry
        // applied < drifted < live < param < previous.
        let entries = vec![preview_entry(
            "vm.swappiness",
            "1",
            "60",
            Some("1"),
            Some(false),
        )];
        assert_eq!(
            serde_json::to_string(&rollback_list_output(&entries)).unwrap(),
            r#"{"count":1,"pending":[{"applied":"1","drifted":false,"live":"1","param":"vm.swappiness","previous":"60"}]}"#
        );
    }

    #[test]
    fn rollback_list_output_serializes_unknown_readings_as_null() {
        // A path that could not be read has no value to publish and no
        // comparison to report: the keys stay present with null and are never
        // dropped, so a consumer never branches on key presence.
        let entries = vec![preview_entry("vm.swappiness", "1", "60", None, None)];
        let out = rollback_list_output(&entries);
        let entry = &out["pending"][0];
        assert!(entry.get("live").is_some_and(serde_json::Value::is_null));
        assert!(entry.get("drifted").is_some_and(serde_json::Value::is_null));
        assert_eq!(entry.as_object().unwrap().len(), 5, "entry keys: {entry}");
    }

    #[test]
    fn rollback_list_output_preserves_entry_order() {
        // The shaper must not re-sort: rollback_preview's BTreeMap order is
        // the contract.
        let entries = vec![
            preview_entry("zzz", "1", "2", None, None),
            preview_entry("aaa", "3", "4", None, None),
        ];
        let out = rollback_list_output(&entries);
        assert_eq!(out["pending"][0]["param"], json!("zzz"));
        assert_eq!(out["pending"][1]["param"], json!("aaa"));
    }

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
        let (output, code) = tune_short_circuit(&[], 0, &[]).expect("must short-circuit");
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
        let (output, code) =
            tune_short_circuit(&recs, 0, &[]).expect("must short-circuit when blocked");
        assert_eq!(code, 1);
        assert_eq!(
            output,
            json!({
                "status": "blocked",
                "applied": 0,
                "recommendations": 3,
                "blocked_unwritable": 3,
                "blocked_runtime_dangerous": 0,
                "would_skip": [
                    { "param": "vm.swappiness", "reason": "unwritable" },
                    { "param": "fs.file-max", "reason": "unwritable" },
                    { "param": "net.core.somaxconn", "reason": "unwritable" },
                ],
            })
        );
    }

    #[test]
    fn tune_short_circuit_blocked_when_only_rec_is_dangerous() {
        // A host whose only recommendation is the runtime-dangerous
        // vm.nr_hugepages: writable, but excluded from runtime writes.
        let recs = vec![rec("vm.nr_hugepages", true)];
        let (output, code) =
            tune_short_circuit(&recs, 0, &[]).expect("must short-circuit when blocked");
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
        let (output, _) = tune_short_circuit(&recs, 0, &[]).expect("blocked");
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
        assert!(tune_short_circuit(&recs, 1, &[]).is_none());
    }

    #[test]
    fn tune_short_circuit_blocked_names_every_skipped_entry() {
        // A real (non-dry-run) blocked tune answers with this body as-is: the
        // counts alone cannot be reconciled with the recommendations `check`
        // keeps reporting (exit 1) on the same host. The dry-run preview got
        // the names (#6134); the real run needs the same list, or a caller
        // reading plain `tune` output still cannot tell which parameters the
        // environment dropped.
        let recs = vec![
            rec("vm.swappiness", false),  // unwritable
            rec("vm.nr_hugepages", true), // writable but runtime-dangerous
        ];
        let (output, code) = tune_short_circuit(&recs, 0, &[]).expect("all blocked short-circuits");
        assert_eq!(code, 1);
        assert_eq!(
            output["would_skip"],
            json!([
                { "param": "vm.swappiness", "reason": "unwritable" },
                { "param": "vm.nr_hugepages", "reason": "runtime_dangerous" },
            ]),
            "a blocked tune must name what it dropped, not only count it: {output}"
        );
    }

    #[test]
    fn tune_output_names_what_the_environment_skipped() {
        // A partial real tune never short-circuits, so the filtered entries
        // are only visible in its output body: without `would_skip` a tune
        // that applied something answers exit 0 while `check` keeps exiting 1
        // on the same host, and nothing in the output explains the
        // difference. The list must carry the dry-run preview's shape so a
        // script can diff the preview against the real run.
        let in_scope = vec![
            rec("vm.swappiness", true),   // applicable
            rec("fs.file-max", false),    // unwritable
            rec("vm.nr_hugepages", true), // writable but runtime-dangerous
        ];
        let outcome = tuner::ApplyOutcome {
            applied: 1,
            failed: vec![],
            clamped: vec![],
        };
        let output = tune_output(&in_scope, &outcome, 30, 35, &[]);
        assert_eq!(output["applied"], json!(1));
        assert_eq!(output["failed"], json!([]));
        assert_eq!(
            output["would_skip"],
            json!([
                { "param": "fs.file-max", "reason": "unwritable" },
                { "param": "vm.nr_hugepages", "reason": "runtime_dangerous" },
            ]),
            "a real tune must name what the environment filtered out: {output}"
        );
        // Nothing failed or clamped here, so applied + would_skip partitions
        // the in-scope recommendations exactly.
        assert_eq!(
            output["applied"].as_u64().unwrap()
                + output["would_skip"].as_array().unwrap().len() as u64,
            in_scope.len() as u64
        );
        // A host with nothing filtered reports the same keys, empty list.
        let clean = tune_output(&[rec("vm.swappiness", true)], &outcome, 30, 35, &[]);
        assert_eq!(clean["would_skip"], json!([]));
    }

    #[test]
    fn dry_run_output_reports_planned_and_blocked_count() {
        // Two applicable recommendations out of three gathered: the preview
        // must carry the short-circuit status vocabulary ("planned", never
        // "optimal") and name the one this environment filtered out, so a
        // script can tell a partial plan from a complete one.
        let recs = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let in_scope = vec![
            rec("vm.swappiness", true),
            rec("net.core.somaxconn", false),
            rec("fs.file-max", true),
        ];
        let output = dry_run_output(&in_scope, &recs, &[]);
        assert_eq!(output["dry_run"], json!(true));
        assert_eq!(output["status"], json!("planned"));
        assert_eq!(output["blocked"], json!(1));
        assert_eq!(output["would_apply"].as_array().map(Vec::len), Some(2));
        assert_eq!(
            output["would_skip"],
            json!([{ "param": "net.core.somaxconn", "reason": "unwritable" }])
        );
        // A fully applicable plan reports nothing blocked or skipped.
        let full = dry_run_output(&recs, &recs, &[]);
        assert_eq!(full["status"], json!("planned"));
        assert_eq!(full["blocked"], json!(0));
        assert_eq!(full["would_skip"], json!([]));
    }

    #[test]
    fn dry_run_output_lists_every_skipped_entry_with_its_reason() {
        // The preview has to name what a real run leaves out, not just count
        // it: a script reconciling the plan against `check` needs the
        // parameters, and the reason has to match the classification the
        // short-circuit counts use.
        let in_scope = vec![
            rec("vm.swappiness", true),   // applicable
            rec("fs.file-max", false),    // unwritable
            rec("vm.nr_hugepages", true), // writable but runtime-dangerous
        ];
        let output = dry_run_output(&in_scope, &[rec("vm.swappiness", true)], &[]);
        assert_eq!(
            output["would_skip"],
            json!([
                { "param": "fs.file-max", "reason": "unwritable" },
                { "param": "vm.nr_hugepages", "reason": "runtime_dangerous" },
            ])
        );
        assert_eq!(
            output["blocked"].as_u64().unwrap(),
            output["would_skip"].as_array().unwrap().len() as u64,
            "blocked stays the skip-list length: {output}"
        );
        // A parameter that is both unwritable and runtime-dangerous counts as
        // unwritable only, exactly like the short-circuit partition.
        let both = vec![rec("vm.nr_hugepages", false)];
        let output = dry_run_output(&both, &[], &[]);
        assert_eq!(
            output["would_skip"],
            json!([{ "param": "vm.nr_hugepages", "reason": "unwritable" }])
        );
    }

    #[test]
    fn dry_run_preview_keeps_its_keys_on_a_short_circuit() {
        // A host whose every recommendation is filtered out short-circuits
        // before the preview is built. Under --dry-run the answer must still
        // carry the keys that flag promises: a script reading `would_apply`
        // has no other way to tell "nothing to plan" from "the flag was
        // ignored", and `would_skip` names the parameters it is missing.
        let recs = vec![rec("vm.swappiness", false)];
        let (blocked, code) =
            tune_short_circuit(&recs, 0, &[]).expect("everything blocked short-circuits");
        assert_eq!(code, 1);

        let preview = dry_run_preview(blocked, &recs, &[]);
        assert_eq!(preview["dry_run"], json!(true));
        assert_eq!(preview["would_apply"], json!([]));
        assert_eq!(
            preview["would_skip"],
            json!([{ "param": "vm.swappiness", "reason": "unwritable" }])
        );
        // `blocked` stays the skip-list count on every dry-run shape, the way
        // the planned shape already reports it: a script reading the key must
        // not see it vanish exactly on the host where everything is blocked.
        assert_eq!(preview["blocked"], json!(1));
        // The short-circuit vocabulary and its counts are untouched.
        assert_eq!(preview["status"], json!("blocked"));
        assert_eq!(preview["recommendations"], json!(1));
        assert_eq!(preview["blocked_unwritable"], json!(1));

        // The truly-optimal body takes the same keys, with an empty list.
        let (optimal, code) = tune_short_circuit(&[], 0, &[]).expect("nothing to recommend");
        assert_eq!(code, 0);
        let preview = dry_run_preview(optimal, &[], &[]);
        assert_eq!(preview["dry_run"], json!(true));
        assert_eq!(preview["would_apply"], json!([]));
        assert_eq!(preview["would_skip"], json!([]));
        assert_eq!(preview["blocked"], json!(0));
        assert_eq!(preview["status"], json!("optimal"));
    }

    #[test]
    fn exclude_matches_the_same_aliases_as_fix_and_why() {
        // `--exclude` must address a parameter in exactly the spellings
        // fix/why resolve (the shared param_matches predicate), while sysfs
        // identities stay verbatim and network interface names keep their
        // case and literal dots.
        for spelling in ["vm.swappiness", "vm/swappiness", "VM.SWAPPINESS"] {
            assert!(
                is_excluded("vm.swappiness", &[spelling.to_string()]),
                "{spelling} must exclude vm.swappiness"
            );
        }
        // Sysfs identities are filesystem names: no dot folding, no case
        // folding — the same spellings `why` refuses.
        for spelling in ["block.Disk.0.scheduler", "BLOCK/Disk.0/scheduler"] {
            assert!(
                !is_excluded("block/Disk.0/scheduler", &[spelling.to_string()]),
                "{spelling} must not address the sysfs identity"
            );
        }
        // Network identities keep case and literal dots: the dotted alias
        // addresses Br0.100, a lower-cased interface name is another identity.
        assert!(is_excluded(
            "net/ipv4/conf/Br0.100/forwarding",
            &["net.ipv4.conf.Br0.100.forwarding".to_string()]
        ));
        assert!(!is_excluded(
            "net/ipv4/conf/Br0.100/forwarding",
            &["net.ipv4.conf.br0.100.forwarding".to_string()]
        ));
    }

    #[test]
    fn exclude_outranks_the_environment_reasons() {
        // The operator's own instruction is the reason THIS run drops the
        // entry: reporting `unwritable` for a parameter the caller explicitly
        // excluded would explain the run in terms the caller did not ask
        // about, and on a read-only host every excluded entry would flip to
        // the environment's reason.
        let excludes = vec!["kernel.shmmax".to_string()];
        // kernel.shmmax is both unwritable and runtime-dangerous in the
        // fixtures below; the exclusion still wins.
        assert_eq!(
            skip_reason(&rec("kernel.shmmax", false), &excludes),
            Some(EXCLUDED)
        );
        assert_eq!(
            skip_reason(&rec("kernel.shmmax", false), &[]),
            Some(UNWRITABLE),
            "without an exclusion the environment reasons are unchanged"
        );
        assert_eq!(
            skip_reason(&rec("vm.nr_hugepages", true), &[]),
            Some(RUNTIME_DANGEROUS)
        );
        assert_eq!(
            skip_reason(
                &rec("vm.nr_hugepages", true),
                &["vm.nr_hugepages".to_string()]
            ),
            Some(EXCLUDED)
        );
        assert_eq!(skip_reason(&rec("vm.swappiness", true), &[]), None);
    }

    #[test]
    fn would_skip_names_excluded_entries_in_plan_order() {
        // The skip list keeps plan order and one reason per entry: an
        // exclusion joins the environment-filtered entries instead of
        // replacing or reordering them.
        let in_scope = vec![
            rec("vm.swappiness", true),
            rec("fs.file-max", false),
            rec("vm.nr_hugepages", true),
        ];
        assert_eq!(
            json!(would_skip_json(
                &in_scope,
                &["vm.swappiness".to_string(), "vm.nr_hugepages".to_string()]
            )),
            json!([
                { "param": "vm.swappiness", "reason": "excluded" },
                { "param": "fs.file-max", "reason": "unwritable" },
                { "param": "vm.nr_hugepages", "reason": "excluded" },
            ])
        );
    }

    #[test]
    fn dry_run_output_moves_excluded_entries_out_of_the_plan() {
        // The preview must show the plan the operator asked for: the excluded
        // entry leaves would_apply and is named in would_skip with the
        // operator's reason, so a script can tell why the parameter `check`
        // still reports is not scheduled.
        let in_scope = vec![rec("vm.swappiness", true), rec("net.core.somaxconn", true)];
        let output = dry_run_output(
            &in_scope,
            &[rec("net.core.somaxconn", true)],
            &["vm.swappiness".to_string()],
        );
        assert_eq!(output["status"], json!("planned"));
        assert_eq!(output["would_apply"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            output["would_skip"],
            json!([{ "param": "vm.swappiness", "reason": "excluded" }])
        );
        assert_eq!(output["blocked"], json!(1));
        // A run without --exclude keeps the exact old body: no new keys.
        let plain = dry_run_output(&in_scope, &in_scope, &[]);
        assert_eq!(plain["would_skip"], json!([]));
        assert_eq!(plain["blocked"], json!(0));
        assert!(
            plain.get("unmatched_exclude").is_none() && plain.get("blocked_excluded").is_none(),
            "the compatibility shape must not grow the new keys: {plain}"
        );
    }

    #[test]
    fn tune_short_circuit_blocked_counts_excluded_entries() {
        // Everything excluded: the operator's exclusion is why nothing is
        // applicable, so the body reports the blocked verdict check's exit 1
        // demands — never "optimal" — and the new count names the reason
        // alongside the environment partitions.
        let in_scope = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let excludes = vec!["vm.swappiness".to_string(), "fs/file-max".to_string()];
        let (output, code) =
            tune_short_circuit(&in_scope, 0, &excludes).expect("nothing applicable short-circuits");
        assert_eq!(code, 1);
        assert_eq!(output["status"], json!("blocked"));
        assert_eq!(output["recommendations"], json!(2));
        assert_eq!(output["blocked_excluded"], json!(2));
        assert_eq!(output["blocked_unwritable"], json!(0));
        assert_eq!(output["blocked_runtime_dangerous"], json!(0));
        assert_eq!(
            output["would_skip"],
            json!([
                { "param": "vm.swappiness", "reason": "excluded" },
                { "param": "fs.file-max", "reason": "excluded" },
            ])
        );

        // Mixed environment reasons and an exclusion: the three counts
        // partition in_scope exactly.
        let mixed = vec![
            rec("vm.swappiness", false),
            rec("vm.nr_hugepages", true),
            rec("fs.file-max", true),
        ];
        let (output, code) = tune_short_circuit(&mixed, 0, &["fs.file-max".to_string()])
            .expect("blocked short-circuits");
        assert_eq!(code, 1);
        assert_eq!(
            output["would_skip"],
            json!([
                { "param": "vm.swappiness", "reason": "unwritable" },
                { "param": "vm.nr_hugepages", "reason": "runtime_dangerous" },
                { "param": "fs.file-max", "reason": "excluded" },
            ])
        );
        assert_eq!(output["recommendations"], json!(3));
        assert_eq!(output["blocked_unwritable"], json!(1));
        assert_eq!(output["blocked_runtime_dangerous"], json!(1));
        assert_eq!(output["blocked_excluded"], json!(1));
        assert_eq!(
            output["blocked_unwritable"].as_u64().unwrap()
                + output["blocked_runtime_dangerous"].as_u64().unwrap()
                + output["blocked_excluded"].as_u64().unwrap(),
            output["recommendations"].as_u64().unwrap()
        );

        // Without --exclude the blocked body keeps its exact key set: the
        // count of an operator's exclusion cannot appear on its own.
        let (plain, _) = tune_short_circuit(&mixed, 0, &[]).expect("blocked short-circuits");
        assert!(
            plain.get("blocked_excluded").is_none() && plain.get("unmatched_exclude").is_none(),
            "the compatibility shape must not grow the new keys: {plain}"
        );
    }

    #[test]
    fn tune_output_names_excluded_entries() {
        // The real-run body lists the excluded entries in the preview's
        // shape: a partial tune answers exit 0 while check keeps exiting 1 on
        // the same parameters, and the exclusion is the difference.
        let in_scope = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let outcome = tuner::ApplyOutcome {
            applied: 1,
            failed: vec![],
            clamped: vec![],
        };
        let output = tune_output(&in_scope, &outcome, 30, 35, &["fs/file-max".to_string()]);
        assert_eq!(output["applied"], json!(1));
        assert_eq!(
            output["would_skip"],
            json!([{ "param": "fs.file-max", "reason": "excluded" }])
        );
        assert_eq!(
            output["applied"].as_u64().unwrap()
                + output["would_skip"].as_array().unwrap().len() as u64,
            in_scope.len() as u64
        );
        let plain = tune_output(&in_scope, &outcome, 30, 35, &[]);
        assert_eq!(plain["would_skip"], json!([]));
        assert!(
            plain.get("unmatched_exclude").is_none(),
            "the compatibility shape must not grow the new key: {plain}"
        );
    }

    #[test]
    fn unmatched_exclude_reports_only_inert_names() {
        // A name that drops nothing is not an error, but it must be visible:
        // listing it (the spelling as given, in order) is the only way a
        // caller can tell "you excluded something that is not planned here"
        // from a silent no-op. A matched name leaves no trace beyond the
        // `excluded` entry in would_skip, and the key disappears entirely
        // when nothing is inert, so a run without the flag cannot grow it.
        let in_scope = vec![rec("vm.swappiness", true)];
        assert_eq!(
            unmatched_exclude_json(
                &in_scope,
                &["no_such_param".to_string(), "vm/swappiness".to_string()]
            ),
            Some(json!(["no_such_param"]))
        );
        assert_eq!(
            unmatched_exclude_json(&in_scope, &["vm.swappiness".to_string()]),
            None
        );
        assert_eq!(unmatched_exclude_json(&in_scope, &[]), None);
        // A recommendation outside --category/--conservative is not in scope,
        // so its exclusion is inert too.
        assert_eq!(
            unmatched_exclude_json(&[], &["vm.swappiness".to_string()]),
            Some(json!(["vm.swappiness"]))
        );
    }

    #[test]
    fn an_empty_plan_reports_every_exclude_name_as_inert() {
        // The optimal shape follows the same one rule as every other shape:
        // an --exclude name that removed nothing from this run's plan is
        // reported in unmatched_exclude. An empty plan removed nothing, so
        // every given name is inert — the key cannot vanish exactly on the
        // hosts where the caller has the least output to reconcile it with.
        let excludes = vec!["vm.swappiness".to_string(), "no_such_param".to_string()];
        let (output, code) = tune_short_circuit(&[], 0, &excludes).expect("nothing to recommend");
        assert_eq!(code, 0);
        assert_eq!(
            output,
            json!({
                "status": "optimal",
                "applied": 0,
                "unmatched_exclude": ["vm.swappiness", "no_such_param"],
            })
        );
        // The preview keeps its keys and inherits the report.
        let preview = dry_run_preview(output, &[], &excludes);
        assert_eq!(preview["dry_run"], json!(true));
        assert_eq!(preview["status"], json!("optimal"));
        assert_eq!(preview["applied"], json!(0));
        assert_eq!(preview["would_apply"], json!([]));
        assert_eq!(preview["would_skip"], json!([]));
        assert_eq!(preview["blocked"], json!(0));
        assert_eq!(
            preview["unmatched_exclude"],
            json!(["vm.swappiness", "no_such_param"])
        );
        // Without --exclude the optimal shape stays byte-identical: no new
        // key can appear on its own.
        let (plain, code) = tune_short_circuit(&[], 0, &[]).expect("nothing to recommend");
        assert_eq!(code, 0);
        assert_eq!(plain, json!({ "status": "optimal", "applied": 0 }));
        assert_eq!(
            dry_run_preview(plain, &[], &[]),
            json!({
                "dry_run": true,
                "status": "optimal",
                "applied": 0,
                "would_apply": [],
                "would_skip": [],
                "blocked": 0,
            })
        );
    }

    #[test]
    fn exclusion_is_exact_and_leaves_the_twin_in_the_plan() {
        // The mutually exclusive twins (vm.dirty_bytes / vm.dirty_ratio and
        // their overcommit counterparts) clear each other in the kernel on
        // every changed write (v6.6 mm/page-writeback.c:518-544,
        // mm/util.c:817,869), but that side effect is not plan membership:
        // `--exclude` names parameters, so excluding one half must not
        // silently drop advice the caller never named. Clearing an excluded
        // twin is a property of writing the other half — the ledger still
        // records the cleared original (cleared_sibling_entry) so rollback
        // can bring it back; the README states the same conclusion.
        let in_scope = vec![
            rec("vm.dirty_ratio", true),
            rec("vm.dirty_background_ratio", true),
        ];
        // Excluding the bytes half (not in the plan) drops nothing: it is
        // reported as an inert exclusion, not spread to its ratio twin.
        let excludes = vec!["vm.dirty_bytes".to_string()];
        assert_eq!(
            unmatched_exclude_json(&in_scope, &excludes),
            Some(json!(["vm.dirty_bytes"]))
        );
        for entry in &in_scope {
            assert!(!is_excluded(&entry.param, &excludes));
        }
        // Excluding the ratio half drops exactly that half; the other
        // dimension's knob keeps its advice.
        let excludes = vec!["vm.dirty_ratio".to_string()];
        assert_eq!(
            skip_reason(&rec("vm.dirty_ratio", true), &excludes),
            Some(EXCLUDED)
        );
        assert_eq!(
            skip_reason(&rec("vm.dirty_background_ratio", true), &excludes),
            None
        );
    }

    #[test]
    fn dry_run_entries_carry_the_check_recommendation_shape() {
        // would_apply and check's recommendations are the same object and
        // must serialize identically: rec_json used to drop subcategory and
        // writable from the preview, so an agent reconciling the plan
        // against the diagnosis saw the same param under two schemas
        // (6 keys vs the documented 8). The fixture is plan-consistent —
        // would_apply only ever lists applicable entries, so none of them
        // carries a skip_reason; the skipped-entry shape is covered by
        // check_entries_carry_the_reason_the_plan_skips_them.
        let recs = vec![rec("vm.swappiness", true), rec("net.core.somaxconn", true)];
        let output = dry_run_output(&recs, &recs, &[]);
        let entries = output["would_apply"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        // The recovered fields carry real values, not nulls.
        assert_eq!(entries[0].get("subcategory"), Some(&json!("memory")));
        assert_eq!(entries[0].get("writable"), Some(&json!(true)));
        assert_eq!(entries[1].get("subcategory"), Some(&json!("network")));
        assert_eq!(entries[1].get("writable"), Some(&json!(true)));
        // cmd_check builds its recommendations array with the same rec_json,
        // so the key set below is exactly check's entry shape, not a subset.
        for entry in entries {
            let mut keys: Vec<&str> = entry
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                vec![
                    "category",
                    "confidence",
                    "current",
                    "param",
                    "reason",
                    "recommended",
                    "subcategory",
                    "writable",
                ]
            );
        }
    }

    #[test]
    fn check_entries_carry_the_reason_the_plan_skips_them() {
        // check's recommendations are the diagnosis the plan is reconciled
        // against — #6288's rationale applies here word for word:
        // `writable: true` on vm.nr_hugepages with no reason told the agent
        // the opposite of what every apply path does (tune skips it, fix
        // refuses it). The reason must ride the shared entry shape, not
        // `why`'s bespoke output alone.
        let recs = vec![
            rec("vm.nr_hugepages", true), // writable, runtime-dangerous
            rec("vm.swappiness", false),  // unwritable in this environment
            rec("fs.file-max", true),     // applicable: no reason to carry
        ];
        let entries: Vec<serde_json::Value> = recs.iter().map(rec_json).collect();
        assert_eq!(
            entries[0]["skip_reason"],
            json!("runtime_dangerous"),
            "a writable runtime-dangerous entry names why the plan drops it"
        );
        assert_eq!(entries[1]["skip_reason"], json!("unwritable"));
        assert!(
            entries[2].get("skip_reason").is_none(),
            "an entry the plan writes carries no skip reason"
        );
    }

    #[test]
    fn predicted_score_ignores_the_entries_the_plan_skips() {
        // The prediction is the score after TUNING, and tuning here is the
        // plan: `tune` skips vm.nr_hugepages (runtime-dangerous) and every
        // unwritable entry, `fix` refuses both. Counting them promised points
        // no write path delivers — on a container whose /proc/sys is
        // read-only, `check` reported the fully tuned score while
        // `tune --dry-run` answered "blocked" and a real tune applied 0.
        let view = vec![
            rec("vm.nr_hugepages", true), // writable but runtime-dangerous
            rec("vm.swappiness", false),  // unwritable
            rec("fs.file-max", true),     // applicable: the only plan entry
        ];
        let eval = evaluation(view.clone());
        // Only fs.file-max's penalty (3) comes off: the two skipped entries
        // keep theirs, so the plan reaches 94, not 100.
        assert_eq!(predicted_score(&eval, &view), 94);

        // A fully blocked plan delivers nothing, so the prediction is the
        // score the host already has — never the tuned score.
        let blocked = vec![rec("vm.swappiness", false), rec("vm.nr_hugepages", true)];
        let eval = evaluation(blocked.clone());
        assert_eq!(predicted_score(&eval, &blocked), eval.score());
        assert!(
            predicted_score(&eval, &blocked) < 100,
            "a blocked plan must not promise the tuned score"
        );
    }

    #[test]
    fn tune_short_circuit_empty_after_category_filter_is_optimal() {
        // "blocked" only fires when recommendations exist IN SCOPE, so
        // `--category net` on a net-clean host keeps today's optimal output.
        let (output, code) = tune_short_circuit(&[], 0, &[]).expect("short-circuits");
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

    /// The stderr document a refusal would print, as the bytes `main` writes:
    /// the command-error arm is the only path an Err takes, so equal
    /// documents mean equal exit status (2) and equal stderr.
    fn refusal_document(error: &anyhow::Error) -> String {
        serde_json::to_string_pretty(&error_body(error)).unwrap()
    }

    #[test]
    fn fix_dry_run_refuses_exactly_what_fix_refuses() {
        // One classification, two modes: the preview answers every refusal
        // with the plain command's own error — same text, same exit 2 — so
        // `ktuner fix X --dry-run && ktuner fix X` cannot be misled by a
        // preview that disagrees with the command it previews.
        let eval = evaluation(vec![
            rec("vm.swappiness", false),  // unwritable here
            rec("vm.nr_hugepages", true), // writable, runtime-dangerous
        ]);
        for (param, expected) in [
            (
                "vm.swappiness",
                "parameter vm.swappiness is read-only in this environment",
            ),
            (
                "vm.nr_hugepages",
                "parameter vm.nr_hugepages is dangerous to write at runtime, persist to /etc/sysctl.d instead",
            ),
            (
                "no_such_ktuner_parameter",
                "parameter not found or already optimal: no_such_ktuner_parameter",
            ),
            // Deny-listed by the write path and never in the plan (no built-in
            // rule recommends one), so both modes refuse it the way the plain
            // command always has: the lookup miss. The unconditional write
            // choke point is unchanged.
            (
                "kernel.core_pattern",
                "parameter not found or already optimal: kernel.core_pattern",
            ),
        ] {
            // The real mode runs as root here (euid is the caller's fact in
            // cmd_fix); the preview is not root-gated, so both modes reach the
            // same classification.
            let real = fix_with(param, false, true, &eval).expect_err("the real run must refuse");
            let preview = fix_with(param, true, false, &eval).expect_err("the preview must refuse");
            assert_eq!(
                refusal_document(&real),
                refusal_document(&preview),
                "{param}: the preview must print the command's own error"
            );
            assert_eq!(
                refusal_document(&preview),
                format!("{{\n  \"error\": \"{expected}\"\n}}"),
                "{param}"
            );
        }
    }

    #[test]
    fn fix_root_gate_applies_to_the_real_run_only() {
        // The two modes side by side: a non-root real run is stopped by the
        // gate, while the same non-root preview reaches the parameter's own
        // verdict — the flag is read-only, like `tune --dry-run`.
        let eval = evaluation(vec![rec("vm.swappiness", false)]);
        let root_error = "{\n  \"error\": \"fix requires root (sudo ktuner fix vm.swappiness)\"\n}";
        let gate = fix_with("vm.swappiness", false, false, &eval)
            .expect_err("a real fix must refuse without root");
        assert_eq!(refusal_document(&gate), root_error);
        let without_root = fix_with("vm.swappiness", true, false, &eval)
            .expect_err("the preview must still refuse the unwritable parameter");
        assert_eq!(
            refusal_document(&without_root),
            "{\n  \"error\": \"parameter vm.swappiness is read-only in this environment\"\n}",
            "the preview must not stop at the root gate"
        );
        // And with root the same real call proceeds to the classification.
        assert_eq!(
            refusal_document(&fix_with("vm.swappiness", false, true, &eval).unwrap_err()),
            "{\n  \"error\": \"parameter vm.swappiness is read-only in this environment\"\n}"
        );
    }

    #[test]
    fn fix_target_classifies_through_the_plans_skip_reason() {
        // The plan and the single-parameter command share one classification:
        // what tune drops with `unwritable` / `runtime_dangerous` is exactly
        // what fix refuses, with the plan's own reason, so fix can never
        // write an entry the plan skips.
        let eval = evaluation(vec![
            rec("vm.swappiness", false),
            rec("vm.nr_hugepages", true),
            rec("fs.file-max", true),
        ]);
        for (param, reason) in [
            ("vm.swappiness", UNWRITABLE),
            ("vm.nr_hugepages", RUNTIME_DANGEROUS),
        ] {
            let recommendation = find_recommendation(&eval, param).unwrap();
            assert_eq!(skip_reason(recommendation, &[]), Some(reason), "{param}");
            assert!(fix_target(&eval, param).is_err(), "{param}");
        }
        let writable = find_recommendation(&eval, "fs.file-max").unwrap();
        assert_eq!(skip_reason(writable, &[]), None);
        assert_eq!(
            fix_target(&eval, "fs.file-max").unwrap().param,
            "fs.file-max"
        );
    }

    #[test]
    fn fix_dry_run_accepts_the_same_spellings_as_fix() {
        // The preview resolves the parameter through the same lookup fix and
        // why use, so every alias they accept reaches the same recommendation
        // and the entry names its canonical spelling; sysfs identities are
        // filesystem names and stay verbatim.
        let eval = evaluation(vec![rec("vm.swappiness", true)]);
        for alias in ["vm.swappiness", "vm/swappiness", "VM.SWAPPINESS"] {
            let target = fix_target(&eval, alias).expect("alias must resolve");
            assert_eq!(target.param, "vm.swappiness");
            assert_eq!(
                fix_dry_run_output(target)["would_apply"][0]["param"],
                json!("vm.swappiness"),
                "{alias}"
            );
        }
        let sysfs = evaluation(vec![rec("block/sda/scheduler", true)]);
        assert!(fix_target(&sysfs, "block/sda/scheduler").is_ok());
        assert!(
            fix_target(&sysfs, "block.sda.scheduler").is_err(),
            "sysfs names must not be dot-folded into a match"
        );
    }

    #[test]
    fn fix_dry_run_output_is_the_tune_preview_shape() {
        // The body a one-parameter preview prints: the tune --dry-run keys,
        // status "planned" with nothing blocked, and would_apply carrying the
        // check entry itself (rec_json), so one parser reconciles the
        // documents. would_skip stays present and empty — a refusal never
        // reaches this shape, it answers with the command's error instead.
        let recommendation = rec("vm.swappiness", true);
        let body = fix_dry_run_output(&recommendation);
        let keys: Vec<&str> = body
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            vec!["blocked", "dry_run", "status", "would_apply", "would_skip"]
        );
        assert_eq!(body["dry_run"], json!(true));
        assert_eq!(body["status"], json!("planned"));
        assert_eq!(body["blocked"], json!(0));
        assert_eq!(body["would_skip"], json!([]));
        assert_eq!(body["would_apply"], json!([rec_json(&recommendation)]));
    }

    #[test]
    fn fix_dry_run_names_the_twin_the_write_clears() {
        // The mutually exclusive pairs come from the write path's own table
        // (tuner::cleared_sibling): the preview must name the half a write
        // would zero — a second copy of the relationship could drift from
        // what the ledger records — and a parameter without a twin carries
        // no key at all.
        for (param, twin) in [
            ("vm.dirty_bytes", "vm.dirty_ratio"),
            ("vm.dirty_ratio", "vm.dirty_bytes"),
            ("vm.dirty_background_bytes", "vm.dirty_background_ratio"),
            ("vm.dirty_background_ratio", "vm.dirty_background_bytes"),
            ("vm.overcommit_kbytes", "vm.overcommit_ratio"),
            ("vm.overcommit_ratio", "vm.overcommit_kbytes"),
        ] {
            let body = fix_dry_run_output(&rec(param, true));
            assert_eq!(body["would_clear"], json!([twin]), "{param}");
        }
        let body = fix_dry_run_output(&rec("vm.swappiness", true));
        assert!(
            body.get("would_clear").is_none(),
            "a parameter without a twin must not grow the key: {body}"
        );
    }

    #[test]
    fn why_reads_sysfs_fallback_without_rewriting_identity() {
        let eval = evaluation(Vec::new());
        // The fallback reports the VALUE, not the file's rendering: the
        // parameter is bracketed option lists, and `current` must be the
        // active option — the format `check`, a recommendation's `current`
        // and the ledger's recorded original all use, so the field does not
        // flip when the recommendation disappears.
        for (param, path, value, active) in [
            (
                "transparent_hugepage/enabled",
                "/sys/kernel/mm/transparent_hugepage/enabled",
                "always [madvise] never\n",
                "madvise",
            ),
            (
                "transparent_hugepage/defrag",
                "/sys/kernel/mm/transparent_hugepage/defrag",
                "always defer defer+madvise [madvise] never\n",
                "madvise",
            ),
            (
                "block/Disk.0/scheduler",
                "/sys/block/Disk.0/queue/scheduler",
                "[none] mq-deadline\n",
                "none",
            ),
        ] {
            let current = CurrentFile::new(value);
            let (output, code) =
                why_with(param, &eval, |actual| Ok(current.read_for(actual, path)))
                    .expect("existing sysfs parameter must be readable without a recommendation");
            assert_eq!(code, 0);
            assert_eq!(
                output,
                json!({ "param": param, "current": active, "status": "optimal" })
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
    fn why_reports_one_value_format_for_a_multi_value_knob() {
        // The recommendation branch publishes the rules' reading, which
        // collapses the kernel's field separators to single spaces
        // (read_sysctl_string). The fallback branch reads the file itself,
        // and the kernel separates the fields of net.ipv4.tcp_rmem /
        // kernel.sem with TABs: publishing that raw line flipped the format
        // of `current` exactly when the recommendation disappeared — the same
        // flip the bracketed option lists used to have — so an agent polling
        // `why` saw "4096 131072 6291456" turn into
        // "4096\t131072\t6291456".
        let eval = evaluation(Vec::new());
        let current = CurrentFile::new("4096\t131072\t6291456\n");
        let (output, code) = why_with("net.ipv4.tcp_rmem", &eval, |path| {
            Ok(current.read_for(path, "/proc/sys/net/ipv4/tcp_rmem"))
        })
        .expect("a readable parameter without a recommendation");
        assert_eq!(code, 0);
        assert_eq!(
            output,
            json!({
                "param": "net.ipv4.tcp_rmem",
                "current": "4096 131072 6291456",
                "status": "optimal"
            })
        );
        // The recommendation branch reports the same knob in the same
        // format, so the two branches cannot disagree about the value.
        let eval = evaluation(vec![Recommendation {
            param: "net.ipv4.tcp_rmem".to_string(),
            current_value: "4096 131072 6291456".to_string(),
            recommended_value: "4096 87380 16777216".to_string(),
            reason: "existing reason".to_string(),
            writable: true,
            ..Default::default()
        }]);
        let (with_rec, code) = why_with("net.ipv4.tcp_rmem", &eval, |_| {
            panic!("a recommendation must not fall through to a filesystem read")
        })
        .unwrap();
        assert_eq!(code, 1);
        assert_eq!(with_rec["current"], output["current"]);
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
    fn why_names_the_reason_the_plan_skips_a_recommendation() {
        // The same classification the plan publishes (#6134's would_skip): a
        // recommendation `why` explains can be one no write path will ever
        // take. `writable: true` on vm.nr_hugepages without the skip reason
        // told an agent the opposite of what every apply path does — tune
        // skips it (would_skip names it runtime_dangerous) and fix refuses it
        // outright — so the explanation command must carry the reason too.
        let eval = evaluation(vec![
            rec("vm.nr_hugepages", true),
            rec("vm.swappiness", false),
        ]);
        let (output, code) = why_with("vm.nr_hugepages", &eval, |_| {
            panic!("a recommendation must not fall through to a filesystem read")
        })
        .unwrap();
        assert_eq!(code, 1);
        assert_eq!(output["skip_reason"], json!("runtime_dangerous"));
        assert_eq!(output["writable"], json!(true));

        let (output, _) = why_with("vm.swappiness", &eval, |_| {
            panic!("a recommendation must not fall through to a filesystem read")
        })
        .unwrap();
        assert_eq!(output["skip_reason"], json!("unwritable"));
        assert_eq!(output["writable"], json!(false));

        // An applicable recommendation carries no skip reason: the plan will
        // write it, so there is nothing to explain away.
        let eval = evaluation(vec![rec("fs.file-max", true)]);
        let (output, _) = why_with("fs.file-max", &eval, |_| {
            panic!("a recommendation must not fall through to a filesystem read")
        })
        .unwrap();
        assert!(
            output.get("skip_reason").is_none(),
            "an applicable entry must not claim a skip reason: {output}"
        );
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

    #[test]
    fn rollback_output_matches_the_full_rollback_body() {
        // The positional is additive: a full rollback's body must stay exactly
        // the four keys it had, in the same byte layout (keys alphabetical,
        // two-space indent, no trailing newline here — the CLI adds one).
        let full = tuner::RollbackOutcome {
            restored: 5,
            failed: 0,
            skipped: 0,
        };
        let body = rollback_output(None, &full);
        println!(
            "full rollback body:\n{}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"restored\": 5,\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        for (restored, failed, skipped, status) in [
            (2, 1, 0, "Partial"),
            (0, 1, 0, "Nothing"),
            // 0 restored is Nothing whichever counter is nonzero: there is no
            // "partial" restore to report when nothing was restored.
            (0, 0, 1, "Nothing"),
        ] {
            let outcome = tuner::RollbackOutcome {
                restored,
                failed,
                skipped,
            };
            assert_eq!(
                rollback_output(None, &outcome),
                json!({
                    "restored": restored,
                    "failed": failed,
                    "skipped": skipped,
                    "status": status,
                })
            );
        }
    }

    #[test]
    fn rollback_output_names_the_restored_param() {
        // The single-parameter body adds exactly one key, carrying the ledger
        // entry that was retired — the spelling `rollback --list` publishes.
        // A recorded mutually exclusive twin is restored and counted too, so
        // `restored` may be 2 while `param` names the entry the caller chose.
        let twins = tuner::RollbackOutcome {
            restored: 2,
            failed: 0,
            skipped: 0,
        };
        let body = rollback_output(Some("vm.dirty_bytes"), &twins);
        println!(
            "single-parameter rollback body:\n{}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"param\": \"vm.dirty_bytes\",\n  \"restored\": 2,\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        let skipped = tuner::RollbackOutcome {
            restored: 0,
            failed: 0,
            skipped: 1,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_output(Some("vm.swappiness"), &skipped))
                .unwrap(),
            "{\n  \"failed\": 0,\n  \"param\": \"vm.swappiness\",\n  \"restored\": 0,\n  \"skipped\": 1,\n  \"status\": \"Nothing\"\n}"
        );
    }

    #[test]
    fn rollback_param_follows_the_fix_why_alias_policy() {
        // cmd_rollback feeds the engine normalize_param's output — the same
        // function fix/why use — so the spellings this command accepts cannot
        // drift from those commands, and a ledger key normalizes to itself
        // (otherwise the lookup would miss the entry the CLI just spelled).
        for (input, expected) in [
            ("vm/swappiness", "vm.swappiness"),
            ("VM.SWAPPINESS", "vm.swappiness"),
            (
                "net/ipv4/conf/Br0.100/forwarding",
                "net.ipv4.conf.Br0.100.forwarding",
            ),
            ("block/sda/scheduler", "block/sda/scheduler"),
            (
                "transparent_hugepage/enabled",
                "transparent_hugepage/enabled",
            ),
        ] {
            assert_eq!(normalize_param(input), expected, "{input}");
            assert_eq!(
                normalize_param(expected),
                expected,
                "a ledger key must normalize to itself: {expected}"
            );
        }
    }

    #[test]
    fn rollback_body_keeps_the_single_param_bytes() {
        // One positional keeps the body it has had since the positional was
        // introduced, byte for byte: the same four keys plus `param` naming
        // the ledger entry that was addressed.
        let twins = tuner::RollbackOutcome {
            restored: 2,
            failed: 0,
            skipped: 0,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_body(
                1,
                &["vm.dirty_bytes".to_string()],
                &twins
            ))
            .unwrap(),
            "{\n  \"failed\": 0,\n  \"param\": \"vm.dirty_bytes\",\n  \"restored\": 2,\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        let skipped = tuner::RollbackOutcome {
            restored: 0,
            failed: 0,
            skipped: 1,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_body(1, &["vm.swappiness".to_string()], &skipped))
                .unwrap(),
            "{\n  \"failed\": 0,\n  \"param\": \"vm.swappiness\",\n  \"restored\": 0,\n  \"skipped\": 1,\n  \"status\": \"Nothing\"\n}"
        );
    }

    #[test]
    fn rollback_body_renders_the_multi_param_shape() {
        // Two or more positionals replace `param` (string) with `params`
        // (array) and keep the aggregate counters, so a consumer reads the
        // same four keys whatever it asked for. The array is the ledger key
        // of each positional, in the order given.
        let outcome = tuner::RollbackOutcome {
            restored: 3,
            failed: 0,
            skipped: 0,
        };
        let keys = vec![
            "net.core.somaxconn".to_string(),
            "vm.swappiness".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        let body = rollback_body(keys.len(), &keys, &outcome);
        println!(
            "multi-parameter rollback body:\n{}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"params\": [\n    \"net.core.somaxconn\",\n    \"vm.swappiness\",\n    \"vm.vfs_cache_pressure\"\n  ],\n  \"restored\": 3,\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        assert!(
            body.get("param").is_none(),
            "`params` replaces `param`, the two never appear together: {body}"
        );
        // Only the shape changes: a partial batch reports the same status
        // vocabulary and counts as the single-parameter form.
        let partial = tuner::RollbackOutcome {
            restored: 1,
            failed: 1,
            skipped: 1,
        };
        let body = rollback_body(
            2,
            &[
                "vm.swappiness".to_string(),
                "vm.vfs_cache_pressure".to_string(),
            ],
            &partial,
        );
        assert_eq!(
            body,
            json!({
                "restored": 1,
                "failed": 1,
                "skipped": 1,
                "status": "Partial",
                "params": ["vm.swappiness", "vm.vfs_cache_pressure"],
            })
        );
        assert!(body.get("param").is_none(), "{body}");
        // The keys are the resolved ledger keys the engine returns: the shaper
        // renders what it is given, in the order it is given.
        assert_eq!(
            rollback_body(
                2,
                &[
                    "vm/swappiness".to_string(),
                    "net.core.somaxconn".to_string()
                ],
                &outcome
            )["params"],
            json!(["vm/swappiness", "net.core.somaxconn"]),
            "the shaper must not re-sort or re-spell the engine's keys"
        );
    }

    #[test]
    fn rollback_body_keeps_the_params_shape_after_dedup() {
        // The shape follows the invocation, not how many ledger keys it
        // resolved to: two positionals that turn out to name one entry still
        // answer with `params` (one element), because a consumer that asked
        // for a batch must be able to read the batch shape. `param` stays
        // reserved for the single-positional body, byte for byte.
        let outcome = tuner::RollbackOutcome {
            restored: 1,
            failed: 0,
            skipped: 0,
        };
        for keys in [
            vec!["vm.swappiness".to_string()],
            vec!["vm.swappiness".to_string(), "vm.swappiness".to_string()],
        ] {
            let body = rollback_body(2, &keys, &outcome);
            assert!(
                body.get("param").is_none(),
                "a two-positional invocation must not fall back to the single-parameter body: {body}"
            );
            assert_eq!(
                body["params"].as_array().map(Vec::len),
                Some(keys.len()),
                "{body}"
            );
        }
        assert_eq!(
            rollback_body(2, &["vm.swappiness".to_string()], &outcome),
            json!({
                "restored": 1,
                "failed": 0,
                "skipped": 0,
                "status": "Full",
                "params": ["vm.swappiness"],
            })
        );
    }

    #[test]
    fn rollback_body_keeps_the_full_rollback_bytes() {
        // No positional keeps the four-key body of a full rollback and gains
        // no `param`/`params` key: the batch shape must not leak into it.
        let full = tuner::RollbackOutcome {
            restored: 5,
            failed: 0,
            skipped: 0,
        };
        assert_eq!(rollback_body(0, &[], &full), rollback_output(None, &full));
        assert_eq!(
            serde_json::to_string_pretty(&rollback_body(0, &[], &full)).unwrap(),
            "{\n  \"failed\": 0,\n  \"restored\": 5,\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
    }

    #[test]
    fn rollback_normalizes_every_positional() {
        // Every positional goes through normalize_param, the policy fix/why
        // share, so the spellings this command accepts cannot drift from
        // those commands whichever position a name sits in.
        assert_eq!(
            normalize_params(&[
                "vm/swappiness".to_string(),
                "VM.SWAPPINESS".to_string(),
                "net/ipv4/conf/Br0.100/forwarding".to_string(),
                "block/sda/scheduler".to_string(),
            ]),
            vec![
                "vm.swappiness".to_string(),
                "vm.swappiness".to_string(),
                "net.ipv4.conf.Br0.100.forwarding".to_string(),
                "block/sda/scheduler".to_string(),
            ]
        );
        assert!(normalize_params(&[]).is_empty());
    }
}
