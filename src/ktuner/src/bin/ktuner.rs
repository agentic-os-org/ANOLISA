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
        /// Keep only this category; may be repeated (the union is kept)
        #[arg(long)]
        category: Vec<String>,
        #[arg(long)]
        conservative: bool,
        /// Leave this parameter out of the report; may be repeated
        #[arg(long)]
        exclude: Vec<String>,
        /// Report only this parameter; may be repeated
        #[arg(long)]
        only: Vec<String>,
    },
    /// Apply tuning recommendations
    Tune {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        conservative: bool,
        /// Keep only this category; may be repeated (the union is kept)
        #[arg(long)]
        category: Vec<String>,
        /// Apply everything but this parameter; may be repeated
        #[arg(long)]
        exclude: Vec<String>,
        /// Apply only this parameter; may be repeated
        #[arg(long)]
        only: Vec<String>,
    },
    /// Fix a single parameter
    Fix {
        param: String,
        /// Show what this fix would write, without changing anything
        #[arg(long)]
        dry_run: bool,
    },
    /// Explain why one or more parameters should be changed
    Why {
        /// Required, so a bare `why` keeps answering with the parser's usage
        /// error instead of a batch body of zero parameters
        #[arg(required = true)]
        params: Vec<String>,
    },
    /// Roll back all applied changes, one or more recorded parameters, or
    /// the recorded entries of a category
    Rollback {
        /// Restore only these recorded parameters, leaving the other entries
        /// in the ledger in place
        params: Vec<String>,
        /// Restore only the recorded entries of this category; may be
        /// repeated (the union is kept)
        #[arg(long)]
        category: Vec<String>,
        /// Restore everything but this recorded parameter; may be repeated
        #[arg(long)]
        exclude: Vec<String>,
        /// Show what a rollback would restore, without changing anything;
        /// with parameters, only the entries restoring them would touch
        #[arg(long)]
        list: bool,
        /// With --list, list only the entries whose recorded value no longer
        /// matches the kernel
        #[arg(long)]
        drifted: bool,
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
            category: cats,
            conservative,
            exclude,
            only,
        } => cmd_check(cats, conservative, exclude, only),
        Commands::Tune {
            dry_run,
            conservative,
            category: cats,
            exclude,
            only,
        } => cmd_tune(dry_run, conservative, cats, exclude, only),
        Commands::Fix { param, dry_run } => cmd_fix(&param, dry_run),
        Commands::Why { params } => cmd_why(&params),
        Commands::Rollback {
            params,
            category,
            exclude,
            list,
            drifted,
        } => cmd_rollback(&params, &category, &exclude, list, drifted),
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

fn cmd_check(
    cats: Vec<String>,
    conservative: bool,
    exclude: Vec<String>,
    only: Vec<String>,
) -> Result<i32> {
    // The two flags are opposite directions of one instruction, so a run that
    // carried both would have to guess which one wins; refusing is a usage
    // error (the README's stderr JSON, exit 2), while ignoring one of them
    // would silently answer a different question than the one asked. Checked
    // before the category and before the system is read, so it fails as an
    // argument error, like the parser's own.
    let selection = check_selection(&exclude, &only)?;
    for cat in &cats {
        category::validate_category(cat)?;
    }
    let info = detect::gather_system_info()?;
    let eval = evaluate(&info)?;
    let workload = engine::classify(&info);
    let runtime_env = detect::detect_runtime_env();
    let detected_services = services::detect_services(&info);

    let mut recs = eval.recommendations.clone();
    if !cats.is_empty() {
        recs = category::filter_by_categories(recs, &cats);
    }
    if conservative {
        recs.retain(|r| r.confidence == rules::Confidence::High);
    }
    // The operator's selection narrows the in-scope list last, exactly like
    // tune's: a name that addresses nothing here is reported in the
    // selection's own key, and the entries the selection drops leave the
    // report the way a `--category` miss does.
    let (recs, unmatched) = apply_check_selection(recs, selection);

    let score = eval.score();
    let counts = category::RecCounts::from_recs(&recs);
    let predicted_score = predicted_score(&eval, &recs);

    let recs_json: Vec<serde_json::Value> = recs.iter().map(rec_json).collect();

    let mut output = json!({
        "score": score,
        "predicted_score": predicted_score,
        "total_checked": eval.total_checked,
        "recommendations": recs_json,
        "counts": {
            "performance": counts.perf,
            "security": counts.sec,
            "high_confidence": counts.high,
            "writable": counts.writable,
            // The performance findings in each `--category` bucket; security
            // findings are counted once, under `security`.
            "network": counts.net,
            "memory": counts.mem,
            "io": counts.io,
            "cpu": counts.cpu,
        },
        "system": system_json(&info),
        "environment": format!("{runtime_env}"),
        "workload": format!("{workload}"),
        "services": detected_services,
    });
    // The selection's inert-name report, and only when it has something to
    // report: a run without the flag keeps the exact key set its consumers
    // already read.
    if let Some((key, names)) = unmatched {
        output[key] = names;
    }
    print_json(&output)?;

    let code = if recs.is_empty() { 0 } else { 1 };
    Ok(code)
}

/// The `system` section of the check report: the machine as the engine saw
/// it, so a caller can identify the host behind the findings without
/// re-reading it. The disk entries carry the fields the disk rules gate on
/// (`hidden`, `holders`, `available_schedulers`), so a suggestion those
/// rules did not make can be explained from the report alone.
///
/// A queue attribute the engine could not read (`read_ahead_kb`,
/// `nr_requests`, `rq_affinity`) prints as 0, the value the key has always
/// carried in that case, so the key keeps its type; the rules see the missing
/// reading and do not advise on it.
///
/// `cpu_cores` and `memory_gb` are the effective values the rules scale by,
/// clamped to the cgroup limits that bind the process; `cpu_cores_host` and
/// `memory_gb_host` are the host's own readings, so a report tells whether
/// and from what those values were clamped.
fn system_json(info: &detect::SystemInfo) -> serde_json::Value {
    let disks: Vec<serde_json::Value> = info
        .disks
        .iter()
        .map(|disk| {
            json!({
                "name": disk.name,
                "type": disk.disk_type.to_string(),
                "scheduler": disk.scheduler,
                "available_schedulers": disk.available_schedulers,
                "nr_requests": disk.nr_requests.unwrap_or(0),
                "read_ahead_kb": disk.read_ahead_kb.unwrap_or(0),
                "rq_affinity": disk.rq_affinity.unwrap_or(0),
                "hidden": disk.hidden,
                "holders": disk.holders,
            })
        })
        .collect();
    let network: Vec<serde_json::Value> = info
        .network
        .iter()
        .map(|link| json!({"name": link.name, "speed_mbps": link.speed_mbps}))
        .collect();
    json!({
        "kernel": info.kernel_version,
        "cpu_cores": info.cpu_cores,
        "cpu_cores_host": info.cpu_cores_host,
        "memory_gb": info.memory_total_gb,
        "memory_gb_host": info.memory_gb_host,
        "numa_nodes": info.numa_nodes,
        "os_distro": info.os_distro,
        "cpu_model": info.cpu_model,
        "disks": disks,
        "network": network,
    })
}

/// The one place `check` reads `--only`/`--exclude`: `--only` names the
/// recommendations to keep, `--exclude` the ones to leave out, and neither
/// flag selects the whole report. The two flags are opposite directions of
/// one instruction, so carrying both is refused as a usage error (the
/// README's stderr JSON, exit 2) instead of guessing which one wins. Pure, so
/// the guard is decided before the category is validated and before
/// [`detect::gather_system_info`] reads the system.
fn check_selection<'a>(exclude: &'a [String], only: &'a [String]) -> Result<Selection<'a>> {
    if !exclude.is_empty() && !only.is_empty() {
        anyhow::bail!("check takes either --only or --exclude, not both");
    }
    Ok(if !only.is_empty() {
        Selection::Only(only)
    } else if !exclude.is_empty() {
        Selection::Except(exclude)
    } else {
        Selection::All
    })
}

/// Narrow the in-scope recommendations to the operator's selection and
/// return what the report keeps, plus the inert-name report the body owes:
/// the selection's names that addressed nothing here, in the order given,
/// under the selection's own key. The report is computed against the
/// in-scope list before the drop, so a name that addressed a recommendation
/// in scope is never called inert. `None` when no flag was given or every
/// name matched, so a run without a selection cannot grow the key.
fn apply_check_selection(
    mut in_scope: Vec<Recommendation>,
    selection: Selection<'_>,
) -> (
    Vec<Recommendation>,
    Option<(&'static str, serde_json::Value)>,
) {
    let unmatched = selection.unmatched_report(&in_scope);
    in_scope.retain(|rec| !selection.drops(&rec.param));
    (in_scope, unmatched)
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
/// Why a real `tune` run leaves out an in-scope recommendation the operator
/// did not name in `--only`. Like `excluded` this is the operator's own
/// instruction, so it is reported first and for every other entry: the named
/// parameters are the plan this run was asked for, and everything else is
/// dropped whether or not this environment could have written it.
const NOT_SELECTED: &str = "not_selected";

/// Whether a user-selected parameter name (`fix <param>`, `why <param>`,
/// `tune --exclude <param>`, `tune --only <param>`) addresses `candidate`:
/// the verbatim spelling first, then the sysctl alias (slash/dot and case)
/// with sysfs identities and network interface names preserved. One predicate
/// so every consumer accepts exactly the same spellings; `--exclude` drops
/// what it addresses, `--only` keeps it.
fn param_matches(candidate: &str, requested: &str) -> bool {
    candidate == requested || normalize_param(candidate) == normalize_param(requested)
}

/// Whether one of `names` addresses `param`: the one predicate behind every
/// operator selection. `--exclude` drops the parameters its names address,
/// `--only` drops the ones they do not.
fn is_excluded(param: &str, names: &[String]) -> bool {
    names.iter().any(|name| param_matches(param, name))
}

/// The operator's plan selection: `--exclude` names the recommendations to
/// leave out, `--only` names the ones to keep, and neither flag selects the
/// whole plan. Both flags name parameters the same way (the shared
/// [`is_excluded`] predicate) and feed the same [`skip_reason`] classification
/// and [`would_skip_json`] list, so they differ only in the sense of a match.
#[derive(Clone, Copy)]
enum Selection<'a> {
    /// No flag: every in-scope recommendation is selected.
    All,
    /// `--exclude`: everything but the named parameters.
    Except(&'a [String]),
    /// `--only`: nothing but the named parameters.
    Only(&'a [String]),
}

impl<'a> Selection<'a> {
    /// The names the operator gave, whichever flag carried them.
    fn names(self) -> &'a [String] {
        match self {
            Selection::All => &[],
            Selection::Except(names) | Selection::Only(names) => names,
        }
    }

    /// Whether the operator's instruction drops `param`: `--exclude` drops
    /// what its names address, `--only` drops what they do not.
    fn drops(self, param: &str) -> bool {
        let named = is_excluded(param, self.names());
        match self {
            Selection::All => false,
            Selection::Except(_) => named,
            Selection::Only(_) => !named,
        }
    }

    /// The reason a dropped entry is reported with, or `None` when no flag
    /// dropped anything.
    fn drop_reason(self) -> Option<&'static str> {
        match self {
            Selection::All => None,
            Selection::Except(_) => Some(EXCLUDED),
            Selection::Only(_) => Some(NOT_SELECTED),
        }
    }

    /// The key the inert-name report is published under, or `None` when no
    /// flag was given.
    fn unmatched_key(self) -> Option<&'static str> {
        match self {
            Selection::All => None,
            Selection::Except(_) => Some("unmatched_exclude"),
            Selection::Only(_) => Some("unmatched_only"),
        }
    }

    /// The inert-name report this selection owes, under the flag's own key:
    /// the names that addressed no in-scope recommendation, in the order
    /// given. `None` when no flag was given or every name matched.
    fn unmatched_report(
        self,
        in_scope: &[Recommendation],
    ) -> Option<(&'static str, serde_json::Value)> {
        let key = self.unmatched_key()?;
        let names = unmatched_exclude_json(in_scope, self)?;
        Some((key, names))
    }
}

/// A plain name list reads as `--exclude`: the spelling the existing call
/// sites use, where an empty list (no names) selects the whole plan.
impl<'a, T> From<&'a T> for Selection<'a>
where
    T: AsRef<[String]> + ?Sized,
{
    fn from(names: &'a T) -> Self {
        Selection::Except(names.as_ref())
    }
}

/// Why a real `tune` run would leave `rec` out, or `None` when this
/// environment can take it. Pure, so the applicability filter, the
/// short-circuit counts and the dry-run preview classify one list
/// identically. The operator's own selection outranks the environment
/// reasons: it is the reason THIS run drops the entry.
fn skip_reason<'a>(
    rec: &Recommendation,
    selection: impl Into<Selection<'a>>,
) -> Option<&'static str> {
    let selection = selection.into();
    if selection.drops(&rec.param) {
        selection.drop_reason()
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
fn would_skip_json<'a>(
    in_scope: &[Recommendation],
    selection: impl Into<Selection<'a>>,
) -> Vec<serde_json::Value> {
    let selection = selection.into();
    in_scope
        .iter()
        .filter_map(|rec| {
            skip_reason(rec, selection)
                .map(|reason| json!({ "param": rec.param, "reason": reason }))
        })
        .collect()
}

/// The inert-name report: the selection's names that addressed nothing in
/// this run's plan, in the order given. A name that matches no in-scope
/// recommendation — a typo, or one outside `--category`/`--conservative` — is
/// not an error (the same command is scripted across hosts whose plans
/// differ), but an inert instruction must be visible or the caller cannot
/// tell that it did nothing. An empty plan addresses nothing, so every given
/// name is reported there too. `None` when every name matched: the report
/// appears only when there is something to say, under the key
/// [`Selection::unmatched_key`] names, so a run without a selection keeps its
/// exact old shape.
fn unmatched_exclude_json<'a>(
    in_scope: &[Recommendation],
    selection: impl Into<Selection<'a>>,
) -> Option<serde_json::Value> {
    let selection = selection.into();
    let unmatched: Vec<&str> = selection
        .names()
        .iter()
        .filter(|name| !in_scope.iter().any(|rec| param_matches(&rec.param, name)))
        .map(String::as_str)
        .collect();
    (!unmatched.is_empty()).then(|| json!(unmatched))
}

/// Why `ktuner tune` exited without applying anything. Pure so the
/// status/exit-code decision is unit-testable without touching the system.
///
/// `in_scope` is the recommendation list after the category/conservative
/// filters but BEFORE the skip classification; `applicable` is the number
/// that survives it (writable, not runtime-dangerous, and not dropped by the
/// operator's selection). Returns `None` when tune should proceed (at least
/// one applicable recommendation).
fn tune_short_circuit<'a>(
    in_scope: &[Recommendation],
    applicable: usize,
    selection: impl Into<Selection<'a>>,
) -> Option<(serde_json::Value, i32)> {
    let selection = selection.into();
    if applicable > 0 {
        return None;
    }
    if in_scope.is_empty() {
        // Genuinely nothing to recommend in scope — unchanged output and code.
        // A selection name cannot have acted on an empty plan, so every given
        // name is inert here too: the report follows the same one rule on
        // every shape — a name that did nothing to this run's plan appears in
        // the selection's own key — instead of vanishing exactly on the hosts
        // with the least output to reconcile.
        let mut body = json!({ "status": "optimal", "applied": 0 });
        if let Some((key, unmatched)) = selection.unmatched_report(in_scope) {
            body[key] = unmatched;
        }
        return Some((body, 0));
    }
    // Recommendations exist but every one was filtered out before any write.
    // Reporting "optimal" here is false: `check` exits 1 on the same host.
    // skip_reason classifies each rec exactly once, so the counts always add
    // up to in_scope.len(); `blocked_excluded` and `blocked_not_selected` join
    // the partition when the selection dropped something. A non-dry-run tune
    // answers with this body too, and the counts alone cannot be reconciled
    // with what `check` keeps reporting — `would_skip` names the entries, same
    // shape as the preview.
    let unwritable = in_scope
        .iter()
        .filter(|r| skip_reason(r, selection) == Some(UNWRITABLE))
        .count();
    let runtime_dangerous = in_scope
        .iter()
        .filter(|r| skip_reason(r, selection) == Some(RUNTIME_DANGEROUS))
        .count();
    let excluded = in_scope
        .iter()
        .filter(|r| skip_reason(r, selection) == Some(EXCLUDED))
        .count();
    let not_selected = in_scope
        .iter()
        .filter(|r| skip_reason(r, selection) == Some(NOT_SELECTED))
        .count();
    let mut body = json!({
        "status": "blocked",
        "applied": 0,
        "recommendations": in_scope.len(),
        "blocked_unwritable": unwritable,
        "blocked_runtime_dangerous": runtime_dangerous,
        "would_skip": would_skip_json(in_scope, selection),
    });
    // The selection-only keys, and only when they have something to report: a
    // run without the flag keeps the exact shape its consumers already read.
    if excluded > 0 {
        body["blocked_excluded"] = json!(excluded);
    }
    if not_selected > 0 {
        body["blocked_not_selected"] = json!(not_selected);
    }
    if let Some((key, unmatched)) = selection.unmatched_report(in_scope) {
        body[key] = unmatched;
    }
    Some((
        body,
        // Mirror check's exit-1 "has recommendations" convention: the system
        // is not optimal, tune simply cannot act on it in this environment.
        1,
    ))
}

/// The twins this plan's writes clear in the kernel, in plan order and
/// deduplicated: the mutually exclusive partner of every parameter the plan
/// applies, from the write path's own twin table ([`tuner::cleared_sibling`]),
/// so the preview names what a real run clears instead of deriving the pair
/// relationship a second time. The built-in rules never plan both halves of a
/// pair, so a twin named here is cleared by the plan and not also applied by
/// it. Empty when no applied parameter has a twin; the caller omits the key
/// then, the same rule `fix --dry-run` follows.
fn would_clear_json(applicable: &[Recommendation]) -> Vec<&'static str> {
    let mut twins: Vec<&'static str> = Vec::new();
    for rec in applicable {
        if let Some(twin) = tuner::cleared_sibling(&rec.param) {
            if !twins.contains(&twin) {
                twins.push(twin);
            }
        }
    }
    twins
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
/// the operator's selection dropped them — with the reason, so the parameters
/// a partial plan leaves behind are visible and not just counted. `blocked`
/// stays their count. `would_clear` names the mutually exclusive twins those
/// writes zero — `fix --dry-run`'s key and meaning, for the whole plan: a
/// write clears its twin in the kernel whether or not the plan selected the
/// twin, so a selection that leaves the twin out does not leave the kernel
/// alone, and the preview saying so is the same "one table, named once" rule
/// the single-parameter preview follows.
fn dry_run_output<'a>(
    in_scope: &[Recommendation],
    applicable: &[Recommendation],
    selection: impl Into<Selection<'a>>,
) -> serde_json::Value {
    let selection = selection.into();
    let recs_json: Vec<serde_json::Value> = applicable.iter().map(rec_json).collect();
    let would_skip = would_skip_json(in_scope, selection);
    let mut body = json!({
        "dry_run": true,
        "status": "planned",
        "blocked": would_skip.len(),
        "would_apply": recs_json,
        "would_skip": would_skip,
    });
    let would_clear = would_clear_json(applicable);
    if !would_clear.is_empty() {
        body["would_clear"] = json!(would_clear);
    }
    if let Some((key, unmatched)) = selection.unmatched_report(in_scope) {
        body[key] = unmatched;
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
/// (environment-filtered or dropped by the operator's selection), `blocked`
/// stays its length (the count the planned shape reports, and the sum of the
/// short-circuit's `blocked_unwritable` / `blocked_runtime_dangerous` /
/// `blocked_excluded` / `blocked_not_selected` partitions), and `status` keeps
/// the short-circuit vocabulary (`optimal` / `blocked`). The short-circuit
/// body already carries the inert-name report when a selection name did
/// nothing, so the preview inherits it.
fn dry_run_preview<'a>(
    mut body: serde_json::Value,
    in_scope: &[Recommendation],
    selection: impl Into<Selection<'a>>,
) -> serde_json::Value {
    let selection = selection.into();
    if let Some(object) = body.as_object_mut() {
        object.insert("dry_run".to_string(), json!(true));
        object
            .entry("would_apply".to_string())
            .or_insert_with(|| json!([]));
        let would_skip = would_skip_json(in_scope, selection);
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
/// runtime-dangerous, or dropped by the operator's selection), in the dry-run
/// preview's shape: a partial tune answers exit 0 while `check` keeps exiting
/// 1 on the same host, and the filtered entries are exactly the difference
/// between the two — without them in the body, a caller cannot reconcile a
/// real `tune` with `check` or with its own dry-run preview.
///
/// `cleared` names the mutually exclusive twins the writes zeroed as a kernel
/// side effect — the twins the ledger recorded, so `rollback` brings them
/// back. The key is absent when no write cleared anything, like the
/// preview's `would_clear`.
fn tune_output<'a>(
    in_scope: &[Recommendation],
    outcome: &tuner::ApplyOutcome,
    score_before: usize,
    score_after: usize,
    selection: impl Into<Selection<'a>>,
) -> serde_json::Value {
    let selection = selection.into();
    let failed: Vec<serde_json::Value> = outcome
        .failed
        .iter()
        .map(|f| json!({ "param": f.param, "error": f.error }))
        .collect();
    // Disjoint from `failed`: parameters the kernel accepted but adjusted.
    // They ARE applied (with the kernel's value) and are recorded in the
    // rollback ledger / sysctl.d; the note makes the delta visible (#4160).
    let clamped = serde_json::to_value(&outcome.clamped).unwrap_or_else(|_| json!([]));
    // What this run wrote, in `fix`'s vocabulary: the original the ledger
    // recorded (`previous`, the value `rollback` restores) and the value the
    // kernel took (`applied`, the clamped one when the kernel adjusted the
    // request). Always present like `failed` — a batch that wrote nothing
    // answers an empty list, not a missing key — so a caller diffing the run
    // against `rollback --list` reads the same pair without a second root read
    // of the ledger. A write-only tunable reports a null `previous`: the write
    // landed and nothing is restorable, exactly as `fix` reports it.
    let written: Vec<serde_json::Value> = outcome
        .written
        .iter()
        .map(|w| json!({ "param": w.param, "previous": w.previous, "applied": w.applied }))
        .collect();
    let mut body = json!({
        "applied": outcome.applied(),
        "failed": failed,
        "clamped": clamped,
        "written": written,
        "score_before": score_before,
        "score_after": score_after,
        "would_skip": would_skip_json(in_scope, selection),
        // The boot replay's verdict must be readable: `tune` applies through
        // the quiet path, so the warning `apply_locked` prints for a failed
        // `systemctl enable` never reaches this command's output, and a run
        // that dropped block/THP values out of the boot replay would report
        // nothing but success. False means those values will not be replayed
        // after a reboot; true also covers a run with nothing non-sysctl to
        // persist.
        "boot_unit_enabled": outcome.boot_unit_enabled,
    });
    if !outcome.cleared.is_empty() {
        body["cleared"] = json!(outcome.cleared);
    }
    if let Some((key, unmatched)) = selection.unmatched_report(in_scope) {
        body[key] = unmatched;
    }
    body
}

/// The gates `tune` applies before it reads the system: the argument errors
/// first, then the root requirement.
///
/// An unknown category is an argument error like the flag pair, and the README
/// states the rule for the whole family — a usage error is "refused before the
/// root gate like the other argument errors" — so `tune` answers it the way
/// `check` and `rollback` answer it. `is_root` is a parameter so a test can
/// drive both callers' verdicts without an euid of its own, the way
/// [`fix_root_gate`] does.
fn tune_gates(cats: &[String], dry_run: bool, is_root: bool) -> Result<()> {
    for cat in cats {
        category::validate_category(cat)?;
    }
    if !dry_run && !is_root {
        anyhow::bail!("tune requires root (sudo ktuner tune)");
    }
    Ok(())
}

fn cmd_tune(
    dry_run: bool,
    conservative: bool,
    cats: Vec<String>,
    exclude: Vec<String>,
    only: Vec<String>,
) -> Result<i32> {
    // The two flags are opposite directions of one instruction, so a run that
    // carried both would have to guess which one wins; refusing is a usage
    // error (the README's stderr JSON, exit 2), while ignoring one of them
    // would silently answer a different question than the one asked. Checked
    // before the root gate so it fails as an argument error, like the
    // parser's own.
    if !exclude.is_empty() && !only.is_empty() {
        anyhow::bail!("tune takes either --only or --exclude, not both");
    }
    let selection = if !only.is_empty() {
        Selection::Only(&only)
    } else if !exclude.is_empty() {
        Selection::Except(&exclude)
    } else {
        Selection::All
    };
    tune_gates(&cats, dry_run, unsafe { libc::geteuid() } == 0)?;
    let (_info, eval) = gather()?;
    let score_before = eval.score();
    let mut recs = eval.recommendations;
    if !cats.is_empty() {
        recs = category::filter_by_categories(recs, &cats);
    }
    if conservative {
        recs.retain(|r| r.confidence == rules::Confidence::High);
    }
    // Keep the pre-filter set so the early exit can distinguish "nothing to
    // recommend" from "recommended, but nothing applicable here" (e.g. root
    // in a container with read-only /proc/sys, where every rec is refreshed
    // as unwritable) — reporting optimal in the latter case contradicts
    // check's exit 1 on the same host. The dry-run preview lists the filtered
    // entries themselves through the same skip_reason, and the operator's
    // selection (`--exclude` / `--only`) enters that classification here,
    // after the category/conservative filters.
    let applicable: Vec<Recommendation> = recs
        .iter()
        .filter(|r| skip_reason(r, selection).is_none())
        .cloned()
        .collect();
    if let Some((output, code)) = tune_short_circuit(&recs, applicable.len(), selection) {
        let output = if dry_run {
            dry_run_preview(output, &recs, selection)
        } else {
            output
        };
        print_json(&output)?;
        return Ok(code);
    }

    if dry_run {
        let output = dry_run_output(&recs, &applicable, selection);
        print_json(&output)?;
        return Ok(0);
    }

    let outcome = tuner::apply_quiet(&applicable)?;
    let (_, eval_after) = gather()?;
    let score_after = eval_after.score();

    let output = tune_output(&recs, &outcome, score_before, score_after, selection);
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
    print_json(&fix_output(param, &fix, &eval_after, rec))?;
    Ok(0)
}

/// JSON body of a real `fix` run — the shape `fix --dry-run` previews and
/// `rollback` reverses. Split out like [`fix_dry_run_output`] so the body is
/// assertable without a live write: every field comes from the recorded
/// original, the write outcome and the re-gathered diagnosis, never from the
/// plan.
///
/// `cleared` names the twin the write zeroed, in `fix --dry-run`'s
/// `would_clear` shape; it is absent when the write cleared nothing.
///
/// `fixed` echoes the name the operator typed, while `param` is the ledger
/// key the write was recorded under — the recommendation's own spelling, the
/// one `rollback --list` publishes and `tune`'s `written` entries carry. An
/// alias (`VM/SWAPPINESS`, `vm/swappiness`) or a dotted interface name
/// otherwise leaves a caller no way to find the entry it just created.
fn fix_output(
    param: &str,
    fix: &tuner::AppliedFix,
    eval_after: &rules::EvalResult,
    rec: &Recommendation,
) -> serde_json::Value {
    let mut output = json!({
        "fixed": param,
        "param": rec.param,
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
        // `fix` persists through the same choke point as `tune`, so a refused
        // `systemctl enable` must be reportable here too: the clamped note
        // below claims the fix was persisted, and a non-sysctl parameter
        // whose unit is not enabled is not replayed after a reboot.
        "boot_unit_enabled": fix.boot_unit_enabled,
    });
    if let Some(twin) = &fix.cleared {
        output["cleared"] = json!([twin]);
    }
    if fix.outcome.clamped {
        output["requested"] = json!(rec.recommended_value);
        output["note"] = json!("内核实际生效值与推荐值不同（已按实际生效值记录并持久化，可回滚）");
    }
    output
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

/// The reader `why` uses for a parameter without a recommendation: the
/// sysfs/sysctl file behind the normalized name, read as-is. A path that does
/// not exist is `None`; a failed read is an error, never a value — write-only
/// sysctls (mode 0200, e.g. vm.compact_memory) and transient EIO must not
/// collapse into an empty "current". One named reader so the single-parameter
/// path and the batch read every file by the same policy.
///
/// The bytes are decoded lossily. A string sysctl holds raw bytes (a
/// latin-1 `kernel.hostname`, a legacy-encoded `kernel.core_pattern`), and
/// `read_to_string` rejected the whole file over one of them, so `why`
/// refused a parameter whose value is right there with the error code the
/// README reserves for a file that cannot be read at all — the same
/// rejection detect's comm / cmdline / cpuinfo readers decode lossily to
/// avoid. Only the encoding stops being a failure: a read that really failed
/// still is one.
fn read_current_value(path: &str) -> Result<Option<String>, std::io::Error> {
    let path = std::path::Path::new(path);
    if !path.exists() {
        return Ok(None);
    }
    std::fs::read(path).map(|bytes| Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// JSON body of `ktuner why` for two or more positionals: `count` and one
/// entry per parameter, in the order given and deduplicated by the spelling
/// the lookup uses ([`normalize_param`]), so two aliases of one parameter are
/// not answered twice. Every entry is the exact object the same parameter
/// answers alone — the batch adds no keys to it — and the caller reads each
/// verdict from its element while the exit code aggregates them: any
/// recommendation keeps `check`'s exit 1, all optimal exits 0.
///
/// The eval arrives as a parameter: `cmd_why` runs one diagnosis for the
/// whole invocation and this renderer cannot start another. A parameter with
/// no file, or an unreadable one, refuses the WHOLE command with
/// [`why_with`]'s own error naming it — every entry is rendered into values
/// before anything is printed, so a failed batch never leaves a partial
/// document on stdout.
fn why_params_output(
    params: &[String],
    eval: &rules::EvalResult,
    read_current: &impl Fn(&str) -> Result<Option<String>, std::io::Error>,
) -> Result<(serde_json::Value, i32)> {
    let mut seen = std::collections::HashSet::new();
    let mut entries = Vec::new();
    let mut code = 0;
    for param in params {
        if !seen.insert(normalize_param(param)) {
            continue;
        }
        let (entry, entry_code) = why_with(param, eval, read_current)?;
        code |= entry_code;
        entries.push(entry);
    }
    Ok((json!({ "count": entries.len(), "params": entries }), code))
}

fn cmd_why(params: &[String]) -> Result<i32> {
    let (_, eval) = gather()?;
    // The shape follows the INVOCATION: one positional keeps the flat
    // single-parameter object byte for byte, while two or more carry the
    // batch body even when they name one parameter between them
    // (`why vm/swappiness vm.swappiness` still answers as a batch), so a
    // consumer that asked for several reads one shape whatever it named —
    // the rule `rollback` follows.
    if let [param] = params {
        let (output, code) = why_with(param, &eval, read_current_value)?;
        print_json(&output)?;
        return Ok(code);
    }
    let (output, code) = why_params_output(params, &eval, &read_current_value)?;
    print_json(&output)?;
    Ok(code)
}

/// The value a parameter currently holds, read the way every other consumer
/// reads it: the shared [`tuner::active_value_for_path`] — the same reading the rules
/// store as a recommendation's `current`, the ledger now records as an
/// original, and `classify_readback` returns as the effective value. Keeping
/// one implementation in the library is what makes the claim true: a second
/// copy here would drift the moment either side learned a new file shape.
fn active_value(path: &str, value: &str) -> String {
    tuner::active_value_for_path(path, value)
}

fn why_with(
    param: &str,
    eval: &rules::EvalResult,
    read_current: impl FnOnce(&str) -> Result<Option<String>, std::io::Error>,
) -> Result<(serde_json::Value, i32)> {
    // A degenerate separator spelling is not a parameter identity, and must not
    // reach the filesystem: `param_to_path` turns dots into slashes and the
    // kernel collapses the empty segment such a name carries, so
    // `vm..swappiness` resolved to the real file and answered "optimal" (exit
    // 0) — contradicting `check` and `fix`, which treat the same spelling as an
    // unmatched name, and the README's refusal of a parameter that cannot be
    // found. The engine already owns this predicate (`tuner::is_safe_param`);
    // it was wired into the import path only.
    if !tuner::is_safe_param(param) {
        anyhow::bail!("parameter not found: {param}");
    }
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
            let output = json!({ "param": normalized, "current": active_value(&path, &val), "status": "optimal" });
            Ok((output, 0))
        }
        Ok(None) => anyhow::bail!("parameter not found: {param}"),
        // README exit-code contract: 2 = error, details in stderr JSON.
        // Claiming "optimal" from a value that was never read would make
        // the structured output untrustworthy for agents.
        Err(err) => anyhow::bail!("cannot read current value of {param} at {path}: {err}"),
    }
}

/// The entries `rollback --list --drifted` keeps: the ones whose live value
/// is known to differ from the recorded one. `drifted` is `Option<bool>` so an
/// unreadable path publishes `null` instead of a guess, and that unknown state
/// is not evidence of drift — the plain `--list` still shows those entries.
fn drifted_entries(entries: Vec<tuner::RollbackPreview>) -> Vec<tuner::RollbackPreview> {
    entries
        .into_iter()
        .filter(|entry| entry.drifted == Some(true))
        .collect()
}

/// JSON shape of `ktuner rollback --list`. Pure so the agent-facing contract
/// (key names, count, entry fields, ordering) is unit-testable without a
/// ledger on disk. Entries arrive as `rollback_preview` returns them, sorted
/// by param (BTreeMap order); `live` and `drifted` arrive as `Option`s so an
/// unreadable path publishes JSON `null` instead of a guessed value, and
/// `category` as one so a record that carries none publishes `null` the same
/// way — the keys stay present on every entry. `cleared_by` follows them: the
/// ledger key of the entry whose write zeroed this one, `null` for every entry
/// no twin cleared, and never absent, so a consumer reads the reason a row
/// holds an `applied` of 0 without branching on key presence.
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
                    "category": entry.category,
                    "live": entry.live,
                    "drifted": entry.drifted,
                    "cleared_by": entry.cleared_by,
                })
            })
            .collect::<Vec<_>>(),
    })
}

/// JSON body of `ktuner rollback`. One builder for every shape: a full
/// rollback keeps the four counters it always had, while restoring a single
/// parameter adds `param` — the ledger key it retired, spelled the way
/// `rollback --list` publishes it, so the entry can be reconciled with the
/// preview. Keys stay in alphabetical order (serde_json's map is sorted):
/// `boot_unit_enabled` < `failed` < `failures` < `missing` < `param` <
/// `params` < `restored` < `restored_params` < `skipped` < `status`.
fn rollback_output(param: Option<&str>, outcome: &tuner::RollbackOutcome) -> serde_json::Value {
    let status = tuner::classify_rollback(outcome);
    let mut body = json!({
        "restored": outcome.restored(),
        "failed": outcome.failed(),
        "skipped": outcome.skipped(),
        "status": format!("{status:?}"),
    });
    if let Some(param) = param {
        body["param"] = json!(param);
    }
    // A count says how many parameters are still tuned, never which ones, so a
    // caller that had to undo one of them (or explain the exit code) had to
    // re-run `rollback --list` as root to find out. Name them the way `tune`
    // names the parameters it failed to write, in the same {param, error}
    // shape. Both keys appear only when they have something to say, like
    // `unmatched_exclude`, `unclassified` and `boot_unit_enabled` below, so a
    // rollback that left nothing keeps the body it has always had.
    if !outcome.failures.is_empty() {
        body["failures"] = json!(outcome
            .failures
            .iter()
            .map(|failure| json!({ "param": failure.param, "error": failure.error }))
            .collect::<Vec<_>>());
    }
    if !outcome.missing.is_empty() {
        body["missing"] = json!(outcome.missing);
    }
    // The other half of `failures`/`missing`: the keys this attempt retired.
    // A count says how many entries are back to their originals, never which
    // ones, and the entries are not the parameters the caller named — a
    // mutually exclusive twin is restored with the entry that clears it, and
    // the selection is expanded inside the engine. Named the way
    // `rollback --list` publishes them, so the run reconciles with the preview
    // and, once the ledger is deleted, remains the only record of what was
    // undone. The key appears only when something was restored, like the two
    // reports above, so a rollback that restored nothing keeps the body it has
    // always had.
    if !outcome.restored_params.is_empty() {
        body["restored_params"] = json!(outcome.restored_params);
    }
    // A partial rollback regenerates the boot replay for the entries it keeps,
    // and a refused `systemctl enable` there was dropped: the run reported
    // success while the block/THP values still recorded will not be replayed
    // after a reboot. The key is the one `tune` and `fix` publish, and it
    // appears only when the replay is not armed, so every other rollback keeps
    // the body it has always had.
    if !outcome.boot_unit_enabled {
        body["boot_unit_enabled"] = json!(false);
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
/// restored with an entry is named by `restored_params`, not here: the array
/// answers what the batch asked for, the report answers what it retired.
/// Whether the batch restored everything is `status`'s answer, not the array's.
/// The reports of what the batch left (`failures`, `missing`) and of what it
/// restored come from the shared base, so a batch carries them exactly as a
/// full rollback does.
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
/// covers every arity. No positional keeps the full-rollback body it always
/// had (the four counters, plus the reports when they have something to say).
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

/// The spellings `rollback` hands to the engine: every name — a positional or
/// an `--exclude` value — goes through normalize_param, the policy fix/why
/// share, so the spellings this command accepts cannot drift from those
/// commands whichever flag a name sits under.
fn normalize_params(params: &[String]) -> Vec<String> {
    params.iter().map(|param| normalize_param(param)).collect()
}

/// Attach the inert-name report to a body when there is one. Same key and
/// shape `tune --exclude` uses, and it appears only when a name addressed no
/// ledger entry, so a run whose exclusions all matched keeps exactly the keys
/// the same invocation answers without the flag.
fn with_unmatched_exclude(body: serde_json::Value, unmatched: &[String]) -> serde_json::Value {
    let mut body = body;
    if !unmatched.is_empty() {
        body["unmatched_exclude"] = json!(unmatched);
    }
    body
}

/// Attach the unclassified report to a body when there is one: the recorded
/// entries no category could select, by name. Same non-empty-only rule as
/// [`with_unmatched_exclude`], and only a `--category` invocation can have
/// one, so every other form keeps exactly the keys it always answered.
fn with_unclassified(body: serde_json::Value, unclassified: &[String]) -> serde_json::Value {
    let mut body = body;
    if !unclassified.is_empty() {
        body["unclassified"] = json!(unclassified);
    }
    body
}

fn cmd_rollback(
    params: &[String],
    cats: &[String],
    exclude: &[String],
    list: bool,
    drifted: bool,
) -> Result<i32> {
    // `--drifted` filters the `--list` preview, so it means nothing without
    // it: refusing is a usage error (the README's stderr JSON, exit 2), checked
    // here so it fails as an argument error before the root gate rather than
    // as a root complaint.
    if drifted && !list {
        anyhow::bail!("rollback --drifted requires --list");
    }
    // Restoring named parameters and excluding named parameters are opposite
    // instructions, so a run that carried both would have to guess which one
    // wins; refusing is a usage error (the README's stderr JSON, exit 2),
    // checked before the root gate like the parser's own errors. It holds for
    // `--list` too: positionals narrow the preview to the named entries while
    // `--exclude` narrows it to everything else.
    if !params.is_empty() && !exclude.is_empty() {
        anyhow::bail!("rollback takes either parameters or --exclude, not both");
    }
    // Selecting named parameters and selecting the entries of a category are
    // opposite instructions as well ("restore exactly these" / "restore the
    // ones this rule category wrote"), and `--category` already selects a
    // whole set — naming parameters on top of it would have to guess which
    // one wins. Same usage error, refused before the root gate.
    if !params.is_empty() && !cats.is_empty() {
        anyhow::bail!("rollback takes either parameters or --category, not both");
    }
    // The same validator and the same message `check` and `tune` use: an
    // unknown category is an argument error, so it is refused before the
    // root gate and before any system read, like the checks above.
    for cat in cats {
        category::validate_category(cat)?;
    }
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
        // restore, `--exclude` to the entries a rollback of everything else
        // would restore, `--category` to the entries whose recorded category
        // matches (with `--exclude` stacking on that selection), all named
        // the way fix/why/rollback normalize them; a name the ledger does not
        // record is the restore's own command error with positionals, and an
        // inert, reported exclusion with `--exclude`. `--drifted` then
        // filters what remains.
        let (unmatched, unclassified, entries) = if !cats.is_empty() {
            let preview = tuner::rollback_preview_categories(cats, &normalize_params(exclude))?;
            (preview.unmatched, preview.unclassified, preview.entries)
        } else if !exclude.is_empty() {
            let (unmatched, entries) =
                tuner::rollback_preview_excluding(&normalize_params(exclude))?;
            (unmatched, Vec::new(), entries)
        } else if params.is_empty() {
            (Vec::new(), Vec::new(), tuner::rollback_preview()?)
        } else {
            (
                Vec::new(),
                Vec::new(),
                tuner::rollback_preview_params(&normalize_params(params))?,
            )
        };
        let entries = if drifted {
            drifted_entries(entries)
        } else {
            entries
        };
        let output = with_unclassified(
            with_unmatched_exclude(rollback_list_output(&entries), &unmatched),
            &unclassified,
        );
        print_json(&output)?;
        return Ok(0);
    }
    if !cats.is_empty() {
        // Restore the categories' entries as one transaction; the body keeps
        // the full-rollback keys and adds the two reports only when they have
        // something to say — the inert `--exclude` names, and the records no
        // category could select.
        let selection = tuner::rollback_categories(cats, &normalize_params(exclude))?;
        let output = with_unclassified(
            with_unmatched_exclude(
                rollback_body(0, &[], &selection.outcome),
                &selection.unmatched,
            ),
            &selection.unclassified,
        );
        print_json(&output)?;
        return Ok(rollback_exit_code(&selection.outcome));
    }
    if !exclude.is_empty() {
        // Restore everything else as one transaction; the body keeps the
        // full-rollback keys and adds the inert-name report only when a name
        // addressed no ledger entry.
        let (unmatched, outcome) = tuner::rollback_excluding(&normalize_params(exclude))?;
        let output = with_unmatched_exclude(rollback_body(0, &[], &outcome), &unmatched);
        print_json(&output)?;
        return Ok(rollback_exit_code(&outcome));
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
    // Counted in test builds only: `why` shares one diagnosis across every
    // parameter of an invocation, and the bytes an invocation prints look
    // identical whether it gathered once or once per parameter — so the
    // contract is pinned by counting calls, the way the rollback probes count
    // ledger writes instead of inferring them from the end state.
    #[cfg(test)]
    tests::gather_probe();
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
    /// Invocations of `gather()` in this test binary, advanced by the probe
    /// `gather` itself calls and read by the test that pins the batch's
    /// one-diagnosis contract. That test is the only writer (no other unit
    /// test reaches `gather`), so the count is deterministic.
    pub(super) static GATHER_CALLS: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// `gather`'s one-diagnosis probe: without it the property is invisible,
    /// because gathering per parameter would print the same bytes.
    pub(super) fn gather_probe() {
        GATHER_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

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

    /// An outcome whose failure list has `n` entries: the body tests pin the
    /// keys and the names, so the fixture only has to be well formed.
    fn test_failures(n: usize) -> Vec<tuner::ApplyFailure> {
        (0..n)
            .map(|i| tuner::ApplyFailure {
                param: format!("vm.ktuner_failed_{i}"),
                error: "write refused".to_string(),
            })
            .collect()
    }

    /// An outcome's missing list of length `n`, the path-absent entries.
    /// An outcome's restored list of length `n`, the ledger keys a batch
    /// landed. The body tests pin the keys and the names, so the fixture only
    /// has to be well formed and carry the length under test.
    fn test_restored(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("vm.ktuner_restored_{i}")).collect()
    }

    fn test_missing(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("vm.ktuner_missing_{i}")).collect()
    }

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
                restored_params: test_restored(restored),
                failures: test_failures(failed),
                missing: test_missing(skipped),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
            };
            assert_eq!(rollback_exit_code(&outcome), expected, "{name}");
        }
    }

    fn preview_entry(
        param: &str,
        applied: &str,
        previous: &str,
        category: Option<rules::Category>,
        live: Option<&str>,
        drifted: Option<bool>,
    ) -> tuner::RollbackPreview {
        tuner::RollbackPreview {
            param: param.to_string(),
            applied: applied.to_string(),
            previous: previous.to_string(),
            category,
            live: live.map(str::to_string),
            drifted,
            cleared_by: None,
        }
    }

    /// One `written` entry for the output-shape tests: a write that landed,
    /// with the original the ledger recorded and the value the kernel took.
    fn written_entry(param: &str, previous: &str, applied: &str) -> tuner::WrittenEntry {
        tuner::WrittenEntry {
            param: param.to_string(),
            previous: Some(previous.to_string()),
            applied: applied.to_string(),
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
    fn drifted_entries_keep_only_known_drift() {
        // `drifted` is `Option<bool>`: only a live value that is known to
        // differ is drift. A matching value is not, and neither is an
        // unreadable path (`None`) — an unreadable value is not evidence of
        // drift and the plain `--list` still shows those entries.
        let entries = vec![
            preview_entry("vm.dirty_ratio", "0", "20", None, Some("20"), Some(true)),
            preview_entry("vm.swappiness", "1", "60", None, Some("1"), Some(false)),
            preview_entry(
                "block/sda/scheduler",
                "none",
                "mq-deadline",
                None,
                None,
                None,
            ),
        ];
        let kept = drifted_entries(entries);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].param, "vm.dirty_ratio");
        // The listing shape is the plain one; only the count moves.
        assert_eq!(
            rollback_list_output(&kept),
            json!({ "count": 1, "pending": [
                {"applied": "0", "category": null, "cleared_by": null, "drifted": true, "live": "20", "param": "vm.dirty_ratio", "previous": "20"}
            ]})
        );
        assert_eq!(
            rollback_list_output(&drifted_entries(Vec::new())),
            json!({ "count": 0, "pending": [] })
        );
    }

    #[test]
    fn drifted_without_list_is_refused_before_the_root_gate() {
        // The filter means nothing without the preview, so the pair is a
        // usage error, not a root complaint: an unprivileged host must see
        // the argument message, and the call returns before any ledger read.
        // (With --list the pair is accepted past this check and falls through
        // to the root gate, which this test cannot assert without root.)
        let err =
            cmd_rollback(&[], &[], &[], false, true).expect_err("--drifted alone must be refused");
        assert_eq!(format!("{err:#}"), "rollback --drifted requires --list");
    }

    #[test]
    fn parameters_and_exclude_are_refused_before_the_root_gate() {
        // Restoring named parameters and excluding named parameters are
        // opposite instructions, so carrying both is a usage error, not a
        // root complaint: an unprivileged host must see the argument message,
        // and the call returns before any ledger read. The same refusal
        // covers the `--list` forms, before the preview reads anything.
        let err = cmd_rollback(
            &["vm.swappiness".to_string()],
            &[],
            &["vm.dirty_ratio".to_string()],
            false,
            false,
        )
        .expect_err("both forms must be refused");
        assert_eq!(
            format!("{err:#}"),
            "rollback takes either parameters or --exclude, not both"
        );
    }

    #[test]
    fn parameters_and_category_are_refused_before_the_root_gate() {
        // Selecting named parameters and selecting the entries of a category
        // are opposite instructions too: the refusal is the same argument
        // error, returned before the root gate (this unprivileged call proves
        // the order). The older `--exclude` refusal keeps its place when both
        // misuses are present.
        let err = cmd_rollback(
            &["vm.swappiness".to_string()],
            &["net".to_string()],
            &[],
            false,
            false,
        )
        .expect_err("both forms must be refused");
        assert_eq!(
            format!("{err:#}"),
            "rollback takes either parameters or --category, not both"
        );
        let err = cmd_rollback(
            &["vm.swappiness".to_string()],
            &["net".to_string()],
            &["vm.dirty_ratio".to_string()],
            false,
            false,
        )
        .expect_err("both forms must be refused");
        assert_eq!(
            format!("{err:#}"),
            "rollback takes either parameters or --exclude, not both"
        );
    }

    #[test]
    fn an_unknown_category_is_refused_before_the_root_gate() {
        // The category is validated with the same function and the same
        // message `check` and `tune` use, and before the root gate: an
        // unprivileged call answers the argument error, not the root one.
        let err = cmd_rollback(&[], &["garbage".to_string()], &[], false, false)
            .expect_err("an unknown category must be refused");
        assert_eq!(
            format!("{err:#}"),
            "未知分类: garbage（支持: net, mem, io, cpu, security）"
        );
    }

    #[test]
    fn unclassified_extends_the_body_only_when_there_is_one() {
        // The report is the only thing `--category` adds to a body, and it
        // exists only for records no category could select: an empty one
        // leaves the body exactly as the same invocation without the flag
        // answers, which is what the unchanged invocations rely on.
        let body = json!({ "failed": 0, "restored": 0, "skipped": 0, "status": "Nothing" });
        assert_eq!(with_unclassified(body.clone(), &[]), body);
        assert_eq!(
            with_unclassified(body, &["vm.swappiness".to_string()]),
            json!({
                "failed": 0,
                "restored": 0,
                "skipped": 0,
                "status": "Nothing",
                "unclassified": ["vm.swappiness"],
            })
        );
    }

    #[test]
    fn unmatched_exclude_extends_the_body_only_when_there_is_one() {
        // The report is the only thing `--exclude` adds to a body, and it
        // exists only when a name addressed no ledger entry: a run whose
        // exclusions all matched keeps exactly the keys the invocation
        // answers without the flag.
        let body = json!({ "count": 0, "pending": [] });
        assert_eq!(with_unmatched_exclude(body.clone(), &[]), body);
        assert_eq!(
            with_unmatched_exclude(body, &["vm.typo".to_string()]),
            json!({ "count": 0, "pending": [], "unmatched_exclude": ["vm.typo"] })
        );
    }

    #[test]
    fn rollback_list_output_maps_every_field() {
        let entries = vec![
            preview_entry(
                "vm.swappiness",
                "1",
                "60",
                Some(rules::Category::Performance),
                Some("1"),
                Some(false),
            ),
            preview_entry(
                "block/sda/scheduler",
                "none",
                "mq-deadline",
                None,
                None,
                None,
            ),
        ];
        assert_eq!(
            rollback_list_output(&entries),
            json!({
                "count": 2,
                "pending": [
                    {
                        "applied": "1",
                        "category": "performance",
                        "cleared_by": null,
                        "drifted": false,
                        "live": "1",
                        "param": "vm.swappiness",
                        "previous": "60",
                    },
                    {
                        "applied": "none",
                        "category": null,
                        "cleared_by": null,
                        "drifted": null,
                        "live": null,
                        "param": "block/sda/scheduler",
                        "previous": "mq-deadline",
                    },
                ]
            })
        );
    }

    /// The regression this feature owes: the keys beyond the recorded triple
    /// are the ONLY change to the published shape. Stripping them from the
    /// output must reproduce, field for field, what `rollback --list` emitted
    /// before they existed.
    #[test]
    fn rollback_list_output_adds_only_the_readings_and_category() {
        let entries = vec![
            preview_entry(
                "vm.swappiness",
                "1",
                "60",
                Some(rules::Category::Security),
                Some("60"),
                Some(true),
            ),
            preview_entry(
                "block/sda/scheduler",
                "none",
                "mq-deadline",
                None,
                None,
                None,
            ),
        ];
        let mut out = rollback_list_output(&entries);
        for entry in out["pending"].as_array_mut().unwrap() {
            let object = entry.as_object_mut().unwrap();
            object.remove("live");
            object.remove("drifted");
            object.remove("category");
            object.remove("cleared_by");
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
        // applied < category < cleared_by < drifted < live < param < previous.
        let entries = vec![preview_entry(
            "vm.swappiness",
            "1",
            "60",
            Some(rules::Category::Security),
            Some("1"),
            Some(false),
        )];
        assert_eq!(
            serde_json::to_string(&rollback_list_output(&entries)).unwrap(),
            r#"{"count":1,"pending":[{"applied":"1","category":"security","cleared_by":null,"drifted":false,"live":"1","param":"vm.swappiness","previous":"60"}]}"#
        );
    }

    #[test]
    fn rollback_list_output_serializes_unknown_readings_as_null() {
        // A path that could not be read has no value to publish and no
        // comparison to report, and a record that carries no category has none
        // to publish, and an entry no twin cleared has no clearer to name: the
        // keys stay present with null and are never dropped, so a consumer
        // never branches on key presence.
        let entries = vec![preview_entry("vm.swappiness", "1", "60", None, None, None)];
        let out = rollback_list_output(&entries);
        let entry = &out["pending"][0];
        assert!(entry.get("live").is_some_and(serde_json::Value::is_null));
        assert!(entry.get("drifted").is_some_and(serde_json::Value::is_null));
        assert!(entry
            .get("category")
            .is_some_and(serde_json::Value::is_null));
        assert!(entry
            .get("cleared_by")
            .is_some_and(serde_json::Value::is_null));
        assert_eq!(entry.as_object().unwrap().len(), 7, "entry keys: {entry}");
    }

    #[test]
    fn rollback_list_output_names_the_clearer_of_a_zeroed_twin() {
        // Writing one half of a mutually exclusive sysctl pair zeroes the
        // other in the kernel, and the ledger records the zeroed half with
        // `applied = "0"` so a rollback can bring it back. Read on its own
        // that row says ktuner chose to zero a knob nobody mentioned;
        // `cleared_by` names the entry whose write put the 0 there — in the
        // spelling the other row publishes as `param`, so the two are
        // reconcilable without knowing the pair table.
        let cleared = |param: &str, clearer: &str| tuner::RollbackPreview {
            cleared_by: Some(clearer.to_string()),
            ..preview_entry(
                param,
                "0",
                "20",
                Some(rules::Category::Performance),
                Some("0"),
                Some(false),
            )
        };
        let clearer = preview_entry(
            "vm.dirty_bytes",
            "1073741824",
            "0",
            Some(rules::Category::Performance),
            Some("1073741824"),
            Some(false),
        );
        let out = rollback_list_output(&[cleared("vm.dirty_ratio", "vm.dirty_bytes"), clearer]);
        assert_eq!(out["pending"][0]["cleared_by"], json!("vm.dirty_bytes"));
        // The half that did the clearing was asked for: it names nobody, and
        // carries the key as null rather than dropping it.
        assert!(out["pending"][1]
            .get("cleared_by")
            .is_some_and(serde_json::Value::is_null));
    }

    #[test]
    fn rollback_list_output_names_the_other_half_for_both_zeroed_halves() {
        // The ledger can hold both halves zeroed — writing either one zeroes
        // the other — and then each row's 0 has an explanation in the listing.
        // A zero the caller did ask for is the only case left with nothing to
        // name, so the key is not a synonym for `applied == "0"`.
        let entries = vec![
            tuner::RollbackPreview {
                cleared_by: Some("vm.dirty_ratio".to_string()),
                ..preview_entry(
                    "vm.dirty_bytes",
                    "0",
                    "1073741824",
                    None,
                    Some("0"),
                    Some(false),
                )
            },
            tuner::RollbackPreview {
                cleared_by: Some("vm.dirty_bytes".to_string()),
                ..preview_entry("vm.dirty_ratio", "0", "20", None, Some("0"), Some(false))
            },
            // A recorded 0 outside any pair: the value the caller asked for.
            preview_entry("vm.swappiness", "0", "60", None, Some("0"), Some(false)),
        ];
        let out = rollback_list_output(&entries);
        assert_eq!(out["pending"][0]["cleared_by"], json!("vm.dirty_ratio"));
        assert_eq!(out["pending"][1]["cleared_by"], json!("vm.dirty_bytes"));
        assert!(out["pending"][2]
            .get("cleared_by")
            .is_some_and(serde_json::Value::is_null));
    }

    #[test]
    fn rollback_list_output_serializes_the_recorded_category() {
        // The category is published with the serde spelling `check` uses for
        // the same field, carried from the record rather than derived from the
        // name: the security entry sits on a `net.*` knob.
        let entries = vec![
            preview_entry(
                "vm.swappiness",
                "1",
                "60",
                Some(rules::Category::Performance),
                None,
                None,
            ),
            preview_entry(
                "net.core.bpf_jit_harden",
                "0",
                "1",
                Some(rules::Category::Security),
                None,
                None,
            ),
        ];
        let out = rollback_list_output(&entries);
        assert_eq!(out["pending"][0]["category"], json!("performance"), "{out}");
        assert_eq!(out["pending"][1]["category"], json!("security"), "{out}");
    }

    #[test]
    fn rollback_list_output_preserves_entry_order() {
        // The shaper must not re-sort: rollback_preview's BTreeMap order is
        // the contract.
        let entries = vec![
            preview_entry("zzz", "1", "2", None, None, None),
            preview_entry("aaa", "3", "4", None, None, None),
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
            written: vec![written_entry("vm.swappiness", "60", "10")],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: Vec::new(),
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
    fn tune_output_names_the_twins_its_writes_cleared() {
        // Writing one knob of a mutually exclusive pair zeroes the other, and
        // the ledger records it so `rollback` can bring it back. The real
        // run's body names those twins, the way the preview's `would_clear`
        // does, or a caller sees a second parameter change that nothing in
        // the output accounts for.
        let in_scope = vec![rec("vm.dirty_bytes", true), rec("vm.swappiness", true)];
        let outcome = tuner::ApplyOutcome {
            written: vec![
                written_entry("vm.dirty_bytes", "2097152", "1073741824"),
                written_entry("vm.swappiness", "60", "10"),
            ],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: vec!["vm.dirty_ratio".to_string()],
        };
        let output = tune_output(&in_scope, &outcome, 30, 35, &[]);
        assert_eq!(output["cleared"], json!(["vm.dirty_ratio"]));
        // Nothing cleared: the key is absent and the body is the one it was.
        let quiet = tuner::ApplyOutcome {
            cleared: Vec::new(),
            ..outcome
        };
        let output = tune_output(&in_scope, &quiet, 30, 35, &[]);
        assert!(output.get("cleared").is_none(), "{output}");
    }

    #[test]
    fn tune_output_names_what_it_wrote() {
        // The batch must answer "which parameters changed, and from what" from
        // the run itself: `applied` is a count, so a caller wanting the names
        // and the originals had to read the ledger again as root. The pair
        // reported here is the ledger's own — `previous` is what `rollback`
        // restores and `applied` is what the kernel took — so a script can
        // diff the run against `rollback --list`.
        let in_scope = vec![rec("vm.dirty_bytes", true), rec("vm.swappiness", true)];
        let outcome = tuner::ApplyOutcome {
            written: vec![
                written_entry("vm.dirty_bytes", "2097152", "1073741824"),
                written_entry("vm.swappiness", "60", "10"),
            ],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: vec!["vm.dirty_ratio".to_string()],
        };
        let output = tune_output(&in_scope, &outcome, 30, 32, &[]);
        assert_eq!(
            output["written"],
            json!([
                { "param": "vm.dirty_bytes", "previous": "2097152", "applied": "1073741824" },
                { "param": "vm.swappiness", "previous": "60", "applied": "10" },
            ]),
            "the run must name every write with its recorded original: {output}"
        );
        // The count and the list are one value: one entry per write, and the
        // twin the write cleared stays in `cleared` — it is a ledger record of
        // a kernel side effect, not a write this run asked for.
        assert_eq!(output["applied"], json!(2));
        assert_eq!(output["written"].as_array().unwrap().len(), 2);
        assert_eq!(output["cleared"], json!(["vm.dirty_ratio"]));
    }

    #[test]
    fn tune_output_keeps_a_write_it_cannot_record() {
        // A write-only tunable has no original to record, and `fix` answers a
        // null `previous` for it; the batch reports the entry the same way
        // instead of dropping a write that did land. A run that wrote nothing
        // keeps the key as an empty list, like `failed`, so a reader never has
        // to branch on key presence.
        let in_scope = vec![rec("vm.swappiness", true), rec("vm.ktuner_probe", true)];
        let outcome = tuner::ApplyOutcome {
            written: vec![tuner::WrittenEntry {
                param: "vm.ktuner_probe".to_string(),
                previous: None,
                applied: "10".to_string(),
            }],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: Vec::new(),
        };
        let output = tune_output(&in_scope, &outcome, 40, 39, &[]);
        assert_eq!(
            output["written"],
            json!([{ "param": "vm.ktuner_probe", "previous": null, "applied": "10" }])
        );
        assert_eq!(output["applied"], json!(1));

        let empty = tuner::ApplyOutcome {
            written: vec![],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: Vec::new(),
        };
        let output = tune_output(&in_scope, &empty, 40, 40, &[]);
        assert_eq!(output["written"], json!([]), "present and empty: {output}");
        assert_eq!(output["applied"], json!(0));
    }

    #[test]
    fn tune_output_reports_the_clamped_value_it_recorded() {
        // A clamped write is applied with the kernel's value and recorded as
        // such, so the entry must carry that value and not the request: the
        // request would contradict both the ledger and `rollback --list` on
        // the knob this run just wrote.
        let in_scope = vec![rec("vm.swappiness", true)];
        let outcome = tuner::ApplyOutcome {
            written: vec![written_entry("vm.swappiness", "60", "50")],
            failed: vec![],
            clamped: vec![tuner::ClampNote {
                param: "vm.swappiness".to_string(),
                requested: "10".to_string(),
                effective: "50".to_string(),
            }],
            boot_unit_enabled: true,
            cleared: Vec::new(),
        };
        let output = tune_output(&in_scope, &outcome, 30, 31, &[]);
        assert_eq!(
            output["written"],
            json!([{ "param": "vm.swappiness", "previous": "60", "applied": "50" }]),
            "a clamped write reports the recorded value, not the request: {output}"
        );
        assert_eq!(output["clamped"][0]["requested"], json!("10"));
        assert_eq!(output["clamped"][0]["effective"], json!("50"));
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
    fn dry_run_output_names_the_twins_the_plan_clears() {
        // A write zeroes its mutually exclusive twin in the kernel whether or
        // not the plan selected the twin (mm/page-writeback.c, mm/util.c), and
        // the ledger records that cleared original — so a preview that stays
        // silent about it hides a change the run makes. `fix --dry-run` names
        // its one twin in `would_clear`; the whole-plan preview names every
        // twin its writes clear, in plan order and once each, from the same
        // table the write path records from.
        let recs = vec![
            rec("vm.dirty_bytes", true),
            rec("vm.swappiness", true),
            rec("vm.overcommit_ratio", true),
        ];
        let output = dry_run_output(&recs, &recs, &[]);
        assert_eq!(
            output["would_clear"],
            json!(["vm.dirty_ratio", "vm.overcommit_kbytes"])
        );
        // The single-parameter preview answers the same thing for the same
        // parameter, so a script reading either one learns the same pair.
        assert_eq!(
            fix_dry_run_output(&rec("vm.dirty_bytes", true))["would_clear"],
            json!(["vm.dirty_ratio"])
        );
        // A parameter the plan carries twice names its twin once: the list is
        // the set of twins the plan clears, not one entry per write.
        let duplicate = vec![
            rec("vm.dirty_background_ratio", true),
            rec("vm.dirty_background_ratio", true),
        ];
        assert_eq!(
            dry_run_output(&duplicate, &duplicate, &[])["would_clear"],
            json!(["vm.dirty_background_bytes"])
        );
        // A plan whose parameters have no twin keeps the shape it always had:
        // the key is absent, exactly like `fix --dry-run`'s.
        let plain = vec![rec("vm.swappiness", true)];
        let output = dry_run_output(&plain, &plain, &[]);
        assert!(output.get("would_clear").is_none(), "{output}");
        // The skipped entries' params do not contribute: only the writes the
        // plan would actually make clear a twin.
        let in_scope = vec![
            rec("vm.overcommit_kbytes", true),
            rec("vm.swappiness", false),
        ];
        let output = dry_run_output(&in_scope, &in_scope[..1], &[]);
        assert_eq!(output["would_clear"], json!(["vm.overcommit_ratio"]));
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
    fn tune_output_reports_an_unarmed_boot_replay() {
        // The warning added for #6887 sits inside `if !quiet`, but the only
        // production caller is `apply_quiet`, so a tune whose unit could not
        // be enabled reported nothing at all. The verdict must reach the body:
        // the block/THP values were written yet will not be replayed after a
        // reboot.
        let in_scope = vec![rec("vm.swappiness", true)];
        let unarmed = tuner::ApplyOutcome {
            written: vec![written_entry("vm.swappiness", "60", "10")],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: false,
            cleared: Vec::new(),
        };
        let output = tune_output(&in_scope, &unarmed, 30, 34, &[]);
        assert_eq!(
            output["boot_unit_enabled"],
            json!(false),
            "a tune whose boot replay is not armed must say so: {output}"
        );

        let armed = tuner::ApplyOutcome {
            boot_unit_enabled: true,
            cleared: Vec::new(),
            ..unarmed
        };
        let output = tune_output(&in_scope, &armed, 30, 34, &[]);
        assert_eq!(output["boot_unit_enabled"], json!(true));
    }

    #[test]
    fn tune_output_names_excluded_entries() {
        // The real-run body lists the excluded entries in the preview's
        // shape: a partial tune answers exit 0 while check keeps exiting 1 on
        // the same parameters, and the exclusion is the difference.
        let in_scope = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let outcome = tuner::ApplyOutcome {
            written: vec![written_entry("vm.swappiness", "60", "10")],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: Vec::new(),
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
    fn only_matches_the_same_aliases_as_fix_and_why() {
        // `--only` addresses a parameter in exactly the spellings fix/why
        // resolve (the shared param_matches predicate) and keeps it: the
        // selection is the operator's instruction, so the named parameter is
        // in the plan and every other parameter is dropped as not_selected.
        for spelling in ["vm.swappiness", "vm/swappiness", "VM.SWAPPINESS"] {
            assert_eq!(
                skip_reason(
                    &rec("vm.swappiness", true),
                    Selection::Only(&[spelling.to_string()])
                ),
                None,
                "{spelling} must select vm.swappiness"
            );
        }
        // Sysfs identities are filesystem names: no dot folding, no case
        // folding — the dotted spelling selects nothing, so the identity is
        // dropped.
        assert_eq!(
            skip_reason(
                &rec("block/Disk.0/scheduler", true),
                Selection::Only(&["block.Disk.0.scheduler".to_string()])
            ),
            Some(NOT_SELECTED)
        );
        // Network identities keep case and literal dots: the dotted alias
        // selects Br0.100, while the lower-cased interface name is another
        // identity and selects nothing.
        assert_eq!(
            skip_reason(
                &rec("net/ipv4/conf/Br0.100/forwarding", true),
                Selection::Only(&["net.ipv4.conf.Br0.100.forwarding".to_string()])
            ),
            None
        );
        assert_eq!(
            skip_reason(
                &rec("net/ipv4/conf/Br0.100/forwarding", true),
                Selection::Only(&["net.ipv4.conf.br0.100.forwarding".to_string()])
            ),
            Some(NOT_SELECTED)
        );
    }

    #[test]
    fn not_selected_outranks_the_environment_reasons() {
        // The operator's selection is the reason THIS run drops the entry: a
        // parameter outside `--only` is reported as not_selected whether or
        // not this environment could have written it, exactly as `excluded`
        // outranks the environment reasons on the other flag. A parameter the
        // selection does name keeps its environment classification.
        let only = vec!["vm.swappiness".to_string()];
        assert_eq!(
            skip_reason(&rec("kernel.shmmax", false), Selection::Only(&only)),
            Some(NOT_SELECTED)
        );
        assert_eq!(
            skip_reason(&rec("vm.nr_hugepages", true), Selection::Only(&only)),
            Some(NOT_SELECTED)
        );
        assert_eq!(
            skip_reason(&rec("vm.swappiness", false), Selection::Only(&only)),
            Some(UNWRITABLE)
        );
        assert_eq!(
            skip_reason(
                &rec("vm.nr_hugepages", true),
                Selection::Only(&["vm.nr_hugepages".to_string()])
            ),
            Some(RUNTIME_DANGEROUS)
        );
        assert_eq!(
            skip_reason(&rec("vm.swappiness", true), Selection::Only(&only)),
            None
        );
        // Without a selection the environment reasons are unchanged.
        assert_eq!(
            skip_reason(&rec("kernel.shmmax", false), &[]),
            Some(UNWRITABLE)
        );
    }

    #[test]
    fn would_skip_names_unselected_entries_in_plan_order() {
        // The skip list keeps plan order and one reason per entry: an
        // unselected entry joins a selected-but-unwritable one instead of
        // replacing or reordering it, and the selection outranks the
        // environment only for the entries it actually drops.
        let in_scope = vec![
            rec("vm.swappiness", true),
            rec("fs.file-max", false),
            rec("vm.nr_hugepages", true),
        ];
        assert_eq!(
            json!(would_skip_json(
                &in_scope,
                Selection::Only(&["vm.swappiness".to_string(), "fs.file-max".to_string()])
            )),
            json!([
                { "param": "fs.file-max", "reason": "unwritable" },
                { "param": "vm.nr_hugepages", "reason": "not_selected" },
            ])
        );
    }

    #[test]
    fn dry_run_output_keeps_only_the_selected_entries() {
        // The preview must show the plan the operator asked for: only the
        // named parameter stays applicable and every other entry is named in
        // would_skip with the selection's reason, so a script can tell why
        // the parameters `check` still reports are not scheduled.
        let in_scope = vec![rec("vm.swappiness", true), rec("net.core.somaxconn", true)];
        let output = dry_run_output(
            &in_scope,
            &[rec("vm.swappiness", true)],
            Selection::Only(&["vm.swappiness".to_string()]),
        );
        assert_eq!(output["status"], json!("planned"));
        assert_eq!(output["would_apply"].as_array().map(Vec::len), Some(1));
        assert_eq!(output["would_apply"][0]["param"], json!("vm.swappiness"));
        assert_eq!(
            output["would_skip"],
            json!([{ "param": "net.core.somaxconn", "reason": "not_selected" }])
        );
        assert_eq!(output["blocked"], json!(1));
        // An inert name is reported under the only key, never the exclude one.
        let inert = dry_run_output(
            &in_scope,
            &[rec("vm.swappiness", true)],
            Selection::Only(&["vm.swappiness".to_string(), "no_such_param".to_string()]),
        );
        assert_eq!(inert["unmatched_only"], json!(["no_such_param"]));
        assert!(inert.get("unmatched_exclude").is_none());
        // A run without --only keeps the exact old body: no new keys.
        let plain = dry_run_output(&in_scope, &in_scope, &[]);
        assert_eq!(plain["would_skip"], json!([]));
        assert!(
            plain.get("unmatched_only").is_none() && plain.get("blocked_not_selected").is_none(),
            "the compatibility shape must not grow the new keys: {plain}"
        );
    }

    #[test]
    fn tune_short_circuit_blocked_counts_not_selected_entries() {
        // Nothing outside the selection is applicable: the operator's own
        // selection is why nothing can be written, so the body reports the
        // blocked verdict check's exit 1 demands — never "optimal" — and the
        // new count names the reason alongside the environment partitions.
        let in_scope = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let only = vec!["net.core.somaxconn".to_string()];
        let (output, code) = tune_short_circuit(&in_scope, 0, Selection::Only(&only))
            .expect("nothing applicable short-circuits");
        assert_eq!(code, 1);
        assert_eq!(output["status"], json!("blocked"));
        assert_eq!(output["recommendations"], json!(2));
        assert_eq!(output["blocked_not_selected"], json!(2));
        assert_eq!(output["blocked_unwritable"], json!(0));
        assert_eq!(output["blocked_runtime_dangerous"], json!(0));
        assert!(
            output.get("blocked_excluded").is_none(),
            "the exclude count belongs to the other flag: {output}"
        );
        assert_eq!(
            output["would_skip"],
            json!([
                { "param": "vm.swappiness", "reason": "not_selected" },
                { "param": "fs.file-max", "reason": "not_selected" },
            ])
        );

        // Mixed selection and environment reasons: the counts partition
        // in_scope exactly.
        let mixed = vec![
            rec("vm.swappiness", true),
            rec("vm.nr_hugepages", true),
            rec("fs.file-max", false),
        ];
        let (output, _) =
            tune_short_circuit(&mixed, 0, Selection::Only(&["fs.file-max".to_string()]))
                .expect("blocked short-circuits");
        assert_eq!(output["recommendations"], json!(3));
        assert_eq!(output["blocked_not_selected"], json!(2));
        assert_eq!(output["blocked_unwritable"], json!(1));
        assert_eq!(output["blocked_runtime_dangerous"], json!(0));
        assert_eq!(
            output["blocked_unwritable"].as_u64().unwrap()
                + output["blocked_runtime_dangerous"].as_u64().unwrap()
                + output["blocked_not_selected"].as_u64().unwrap(),
            output["recommendations"].as_u64().unwrap()
        );

        // Without --only the blocked body keeps its exact key set: the
        // selection's count cannot appear on its own.
        let (plain, _) = tune_short_circuit(&mixed, 0, &[]).expect("blocked short-circuits");
        assert!(
            plain.get("blocked_not_selected").is_none() && plain.get("unmatched_only").is_none(),
            "the compatibility shape must not grow the new keys: {plain}"
        );
    }

    #[test]
    fn tune_output_names_not_selected_entries() {
        // The real-run body lists the unselected entries in the preview's
        // shape: a partial tune answers exit 0 while check keeps exiting 1 on
        // the same parameters, and the selection is the difference.
        let in_scope = vec![rec("vm.swappiness", true), rec("fs.file-max", true)];
        let outcome = tuner::ApplyOutcome {
            written: vec![written_entry("vm.swappiness", "60", "10")],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: Vec::new(),
        };
        let output = tune_output(
            &in_scope,
            &outcome,
            30,
            35,
            Selection::Only(&["vm/swappiness".to_string()]),
        );
        assert_eq!(output["applied"], json!(1));
        assert_eq!(
            output["would_skip"],
            json!([{ "param": "fs.file-max", "reason": "not_selected" }])
        );
        assert_eq!(
            output["applied"].as_u64().unwrap()
                + output["would_skip"].as_array().unwrap().len() as u64,
            in_scope.len() as u64
        );
    }

    #[test]
    fn unmatched_only_reports_only_inert_names() {
        // A name that selects nothing is not an error, but it must be
        // visible: listing it (the spelling as given, in order) is the only
        // way a caller can tell "you selected something that is not planned
        // here" from a silent empty plan. A matched name leaves no trace
        // beyond the entries left out.
        let in_scope = vec![rec("vm.swappiness", true)];
        assert_eq!(
            unmatched_exclude_json(
                &in_scope,
                Selection::Only(&["no_such_param".to_string(), "vm/swappiness".to_string()])
            ),
            Some(json!(["no_such_param"]))
        );
        assert_eq!(
            unmatched_exclude_json(&in_scope, Selection::Only(&["vm.swappiness".to_string()])),
            None
        );
        // An empty plan selects nothing, so every given name is inert.
        assert_eq!(
            unmatched_exclude_json(&[], Selection::Only(&["vm.swappiness".to_string()])),
            Some(json!(["vm.swappiness"]))
        );
        // No flag: no report, whatever the plan.
        assert_eq!(unmatched_exclude_json(&in_scope, &[]), None);
    }

    #[test]
    fn an_empty_plan_reports_every_only_name_as_inert() {
        // The optimal shape follows the same one rule as every other shape:
        // an --only name that selected nothing from this run's plan is
        // reported in unmatched_only. An empty plan selected nothing, so
        // every given name is inert — the key cannot vanish exactly on the
        // hosts where the caller has the least output to reconcile it with.
        let only = vec!["vm.swappiness".to_string(), "no_such_param".to_string()];
        let (output, code) =
            tune_short_circuit(&[], 0, Selection::Only(&only)).expect("nothing to recommend");
        assert_eq!(code, 0);
        assert_eq!(
            output,
            json!({
                "status": "optimal",
                "applied": 0,
                "unmatched_only": ["vm.swappiness", "no_such_param"],
            })
        );
        let preview = dry_run_preview(output, &[], Selection::Only(&only));
        assert_eq!(preview["status"], json!("optimal"));
        assert_eq!(preview["would_apply"], json!([]));
        assert_eq!(preview["would_skip"], json!([]));
        assert_eq!(preview["blocked"], json!(0));
        assert_eq!(
            preview["unmatched_only"],
            json!(["vm.swappiness", "no_such_param"])
        );
    }

    #[test]
    fn only_and_exclude_are_rejected_before_the_root_gate() {
        // The two flags together are refused before the root gate: on an
        // unprivileged host a root-first order would answer "tune requires
        // root", so the usage message proves the guard runs first. The call
        // returns before gather() either way, so it cannot touch the system.
        let err = cmd_tune(
            false,
            false,
            Vec::new(),
            vec!["vm.dirty_ratio".to_string()],
            vec!["vm.swappiness".to_string()],
        )
        .expect_err("both flags must be refused");
        assert_eq!(
            format!("{err:#}"),
            "tune takes either --only or --exclude, not both"
        );
    }

    #[test]
    fn a_run_without_only_keeps_the_old_bodies() {
        // The compatibility contract at the builder level: without --only the
        // four shapes are exactly the bodies the exclude-era tests pin, and
        // none of them can grow an only key or a not_selected reason on its
        // own — the new vocabulary belongs to the flag that introduced it.
        let in_scope = vec![rec("vm.swappiness", true), rec("fs.file-max", false)];
        let skip = json!([{ "param": "fs.file-max", "reason": "unwritable" }]);
        let dry = dry_run_output(&in_scope, &in_scope[..1], &[]);
        assert_eq!(dry["would_skip"], skip);
        assert!(
            dry.get("unmatched_only").is_none() && dry.get("blocked_not_selected").is_none(),
            "a no-flag preview must keep its old key set: {dry}"
        );
        let (blocked, _) = tune_short_circuit(&in_scope, 0, &[]).expect("blocked short-circuits");
        assert_eq!(
            blocked,
            json!({
                "status": "blocked",
                "applied": 0,
                "recommendations": 2,
                "blocked_unwritable": 1,
                "blocked_runtime_dangerous": 0,
                "would_skip": [{ "param": "fs.file-max", "reason": "unwritable" }],
            })
        );
        let preview = dry_run_preview(blocked, &in_scope, &[]);
        assert!(
            preview.get("unmatched_only").is_none()
                && preview.get("blocked_not_selected").is_none(),
            "a no-flag preview must keep its old key set: {preview}"
        );
        let outcome = tuner::ApplyOutcome {
            written: vec![written_entry("vm.swappiness", "60", "10")],
            failed: vec![],
            clamped: vec![],
            boot_unit_enabled: true,
            cleared: Vec::new(),
        };
        let real = tune_output(&in_scope, &outcome, 30, 35, &[]);
        assert_eq!(real["would_skip"], skip);
        assert!(
            real.get("unmatched_only").is_none() && real.get("blocked_not_selected").is_none(),
            "a no-flag run body must keep its old key set: {real}"
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

    #[test]
    fn tune_short_circuit_empty_after_multiple_categories_is_optimal() {
        // Same rule for the union: when `--category net --category mem` keeps
        // nothing (a host whose only recommendations are io/cpu), the run is
        // genuinely optimal — not "blocked" — and keeps the same output.
        let recs = vec![
            rec("fs.file-max", true),
            rec("kernel.sched_latency_ns", true),
        ];
        let in_scope =
            ktuner_engine::category::filter_by_categories(recs, &["net".into(), "mem".into()]);
        assert!(in_scope.is_empty());
        let (output, code) = tune_short_circuit(&in_scope, 0, &[]).expect("short-circuits");
        assert_eq!(code, 0);
        assert_eq!(output, json!({ "status": "optimal", "applied": 0 }));
    }

    #[test]
    fn tune_short_circuit_blocked_after_multiple_categories() {
        // When the union keeps entries but every one is unwritable, the run
        // must answer "blocked" with the counts of the union-filtered set —
        // the same body a single --category would produce for these entries.
        let recs = vec![
            rec("net.core.somaxconn", false),
            rec("vm.swappiness", false),
            rec("fs.file-max", false),
        ];
        let in_scope =
            ktuner_engine::category::filter_by_categories(recs, &["network".into(), "内存".into()]);
        assert_eq!(in_scope.len(), 2);
        let (output, code) = tune_short_circuit(&in_scope, 0, &[]).expect("short-circuits");
        assert_eq!(code, 1);
        assert_eq!(output["status"], json!("blocked"));
        assert_eq!(output["recommendations"], json!(2));
        assert_eq!(output["blocked_unwritable"], json!(2));
        assert_eq!(
            output["would_skip"],
            json!([
                {"param": "net.core.somaxconn", "reason": "unwritable"},
                {"param": "vm.swappiness", "reason": "unwritable"}
            ])
        );
    }

    #[test]
    fn check_selection_guards_the_two_flags_and_builds_the_enum() {
        // The flags are two directions of one instruction, so both together
        // are refused as a usage error; each alone becomes the selection the
        // report is narrowed to, and no flag selects the whole report.
        assert!(matches!(check_selection(&[], &[]), Ok(Selection::All)));
        assert!(matches!(
            check_selection(&[], &["vm.swappiness".to_string()]),
            Ok(Selection::Only(_))
        ));
        assert!(matches!(
            check_selection(&["vm.swappiness".to_string()], &[]),
            Ok(Selection::Except(_))
        ));
        let err = match check_selection(
            &["vm.dirty_ratio".to_string()],
            &["vm.swappiness".to_string()],
        ) {
            Err(err) => err,
            Ok(_) => panic!("both flags must be refused"),
        };
        assert_eq!(
            format!("{err:#}"),
            "check takes either --only or --exclude, not both"
        );
    }

    #[test]
    fn check_rejects_the_flag_pair_before_the_category_gate() {
        // The guard is the first thing cmd_check does: with a category that
        // is itself invalid, the usage error must win — which proves it runs
        // before validate_category and hence before gather_system_info; the
        // call returns before either, so it cannot touch the system. Without
        // the pair the same call answers with the category's own error, so
        // the assertion above cannot hold for the wrong reason.
        let err = cmd_check(
            vec!["no_such_category".to_string()],
            false,
            vec!["vm.dirty_ratio".to_string()],
            vec!["vm.swappiness".to_string()],
        )
        .expect_err("both flags must be refused");
        assert_eq!(
            format!("{err:#}"),
            "check takes either --only or --exclude, not both"
        );
        let err = cmd_check(vec!["no_such_category".to_string()], false, vec![], vec![])
            .expect_err("an unknown category is refused");
        assert!(
            format!("{err:#}").contains("no_such_category"),
            "the category check must be the one that fires without the pair: {err:#}"
        );
    }

    #[test]
    fn tune_rejects_an_unknown_category_before_the_root_gate() {
        // `check` and `rollback` answer an unknown category as an argument
        // error, and the README's rollback section states the rule for the
        // family: a usage error is "refused before the root gate like the
        // other argument errors", with "an unknown category ... refused there
        // too". A real `tune` ran its root gate first, so a non-root caller
        // who mistyped a category was told to use sudo instead of which value
        // was wrong — while the same command with `--dry-run` answered the
        // category error.
        let err = tune_gates(&["no_such_category".to_string()], false, false)
            .expect_err("an unknown category is an argument error");
        assert!(
            format!("{err:#}").contains("no_such_category"),
            "the category must be the error a non-root caller sees: {err:#}"
        );
        // The root error still fires when the arguments are well formed, so
        // the assertion above cannot hold for the wrong reason.
        let err = tune_gates(&["net".to_string()], false, false)
            .expect_err("a real run without root is refused");
        assert_eq!(format!("{err:#}"), "tune requires root (sudo ktuner tune)");
        // `--dry-run` needs no root, with or without a category.
        assert!(tune_gates(&["net".to_string()], true, false).is_ok());
        assert!(tune_gates(&[], true, false).is_ok());
    }

    #[test]
    fn check_selection_keeps_or_drops_named_recs_in_report_order() {
        // `--only` keeps the named recommendations and `--exclude` drops
        // them, both with the same aliases `fix`/`why` accept and in the
        // report's own order; no flag is the identity. A matched name leaves
        // no inert-name report behind.
        let in_scope = vec![
            rec("vm.swappiness", true),
            rec("net.core.somaxconn", true),
            rec("fs.file-max", true),
        ];
        let (kept, unmatched) = apply_check_selection(
            in_scope.clone(),
            Selection::Only(&["fs.file-max".to_string(), "vm/swappiness".to_string()]),
        );
        assert_eq!(
            kept.iter().map(|r| r.param.as_str()).collect::<Vec<_>>(),
            ["vm.swappiness", "fs.file-max"],
            "only the named recs stay, in report order, aliases included"
        );
        assert!(unmatched.is_none(), "every name addressed an in-scope rec");

        let (kept, unmatched) = apply_check_selection(
            in_scope.clone(),
            Selection::Except(&["net/core/somaxconn".to_string()]),
        );
        assert_eq!(
            kept.iter().map(|r| r.param.as_str()).collect::<Vec<_>>(),
            ["vm.swappiness", "fs.file-max"],
            "the named rec is dropped through its slash spelling too"
        );
        assert!(unmatched.is_none());

        let (kept, unmatched) = apply_check_selection(in_scope, Selection::All);
        assert_eq!(kept.len(), 3, "no flag keeps the whole report");
        assert!(unmatched.is_none(), "no flag owes no inert-name report");
    }

    #[test]
    fn check_selection_reports_names_against_the_in_scope_list() {
        // The inert-name report is computed on the list the selection could
        // actually act on (after the category/conservative filters), so a
        // name whose recommendation those filters removed is reported, in
        // the spelling given. An empty in-scope list makes every name inert,
        // under the flag's own key.
        let (kept, unmatched) = apply_check_selection(
            vec![rec("vm.swappiness", true)],
            Selection::Only(&[
                "net.core.somaxconn".to_string(),
                "vm/swappiness".to_string(),
            ]),
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(
            unmatched,
            Some(("unmatched_only", json!(["net.core.somaxconn"])))
        );

        let (kept, unmatched) =
            apply_check_selection(vec![], Selection::Except(&["vm.swappiness".to_string()]));
        assert!(kept.is_empty());
        assert_eq!(
            unmatched,
            Some(("unmatched_exclude", json!(["vm.swappiness"])))
        );
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
    fn fix_output_names_the_ledger_key_it_recorded() {
        // The operator may type an alias; the ledger records the
        // recommendation's own key, and that key is what `rollback --list`
        // publishes and `rollback <param>` answers with. `fixed` keeps the
        // spelling as typed, `param` carries the recorded key.
        let rec = rec("vm.swappiness", true);
        let fix = tuner::AppliedFix {
            recorded_previous: Some("60".to_string()),
            outcome: tuner::WriteOutcome {
                effective: "10".to_string(),
                clamped: false,
            },
            boot_unit_enabled: true,
            cleared: None,
        };
        let after = evaluation(vec![]);
        let output = fix_output("VM/SWAPPINESS", &fix, &after, &rec);
        assert_eq!(output["fixed"], json!("VM/SWAPPINESS"));
        assert_eq!(
            output["param"],
            json!("vm.swappiness"),
            "the ledger key the write was recorded under: {output}"
        );
    }

    #[test]
    fn fix_output_reports_the_boot_replay_verdict() {
        // `fix` persists through the same choke point as `tune`, so the
        // verdict must be in this body too: the clamp note in the same body
        // claims the write was persisted, and a refused `systemctl enable`
        // means a non-sysctl fix is not replayed after a reboot. Every field
        // comes from the recorded original, the write outcome and the
        // re-gathered diagnosis, so the body is asserted through its seam
        // instead of a live write.
        let rec = rec("vm.swappiness", true);
        let unarmed = tuner::AppliedFix {
            recorded_previous: Some("60".to_string()),
            outcome: tuner::WriteOutcome {
                effective: "10".to_string(),
                clamped: false,
            },
            boot_unit_enabled: false,
            cleared: None,
        };
        let after = evaluation(vec![rec.clone()]);
        let output = fix_output("vm.swappiness", &unarmed, &after, &rec);
        assert_eq!(output["fixed"], json!("vm.swappiness"));
        assert_eq!(
            output["previous"],
            json!("60"),
            "the recorded original, not the recommendation's gathered current"
        );
        assert_eq!(output["applied"], json!("10"));
        assert_eq!(output["score_after"], json!(after.score()));
        assert_eq!(output["remaining"], json!(1));
        assert_eq!(
            output["boot_unit_enabled"],
            json!(false),
            "an unarmed boot replay must be visible here too: {output}"
        );
        assert!(output.get("note").is_none(), "no clamp, no note: {output}");
        assert!(
            output.get("cleared").is_none(),
            "a write without a twin names none: {output}"
        );

        let clamped = tuner::AppliedFix {
            outcome: tuner::WriteOutcome {
                effective: "2".to_string(),
                clamped: true,
            },
            boot_unit_enabled: true,
            ..unarmed
        };
        let output = fix_output("vm.swappiness", &clamped, &after, &rec);
        assert_eq!(output["boot_unit_enabled"], json!(true));
        assert_eq!(output["applied"], json!("2"));
        assert_eq!(output["requested"], json!(rec.recommended_value));
        assert!(
            output["note"]
                .as_str()
                .is_some_and(|note| note.contains("持久化")),
            "a clamped write keeps its note: {output}"
        );
    }

    #[test]
    fn fix_output_names_the_twin_its_write_cleared() {
        // `fix --dry-run` previews the twin in `would_clear`; the real fix
        // names the one the ledger recorded, in the same shape, so one parser
        // reads both.
        let rec = rec("vm.dirty_bytes", true);
        let fix = tuner::AppliedFix {
            recorded_previous: Some("0".to_string()),
            outcome: tuner::WriteOutcome {
                effective: "1073741824".to_string(),
                clamped: false,
            },
            boot_unit_enabled: true,
            cleared: Some("vm.dirty_ratio".to_string()),
        };
        let after = evaluation(vec![]);
        let output = fix_output("vm.dirty_bytes", &fix, &after, &rec);
        assert_eq!(output["cleared"], json!(["vm.dirty_ratio"]));
        assert_eq!(
            output["cleared"],
            fix_dry_run_output(&rec)["would_clear"],
            "the run names what the preview announced"
        );
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
    fn why_preserves_literal_brackets_in_proc_scalar_strings() {
        let eval = evaluation(Vec::new());
        let mut mismatches = Vec::new();
        for (param, path) in [
            ("kernel.hostname", "/proc/sys/kernel/hostname"),
            ("kernel/domainname", "/proc/sys/kernel/domainname"),
        ] {
            for value in [
                "[production]",
                "node[production]",
                "release[blue",
                "release [blue]",
                "[]",
                "[none]",
            ] {
                let current = CurrentFile::new(&format!("{value}\n"));
                let (output, code) =
                    why_with(param, &eval, |actual| Ok(current.read_for(actual, path)))
                        .expect("read a temporary proc string fixture");
                assert_eq!(code, 0);
                if output["current"] != json!(value) {
                    mismatches.push((param, value, output["current"].clone()));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "literal strings changed: {mismatches:?}"
        );
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
                "memory",
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
    fn why_rejects_degenerate_separator_spellings() {
        // `vm..swappiness` and friends used to resolve: the dot-to-slash
        // translation produced a doubled separator, the kernel collapsed it, and
        // `why` answered "optimal" (exit 0) for a name `check --only` reports as
        // unmatched and `fix` refuses. The spelling is not a parameter identity
        // (`tuner::is_safe_param` rejects the empty segment), so it must get the
        // documented not-found error instead of a verdict about the file it
        // happens to collide with.
        let eval = evaluation(Vec::new());
        let current = CurrentFile::new("60\n");
        // The kernel collapses repeated and trailing separators, so a reader
        // comparing the raw path verbatim would hide the very collision this
        // test is about: model the resolution.
        fn collapse(path: &str) -> String {
            let joined = path
                .split('/')
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            format!("/{joined}")
        }
        let reader = |path: &str| Ok(current.read_for(&collapse(path), "/proc/sys/vm/swappiness"));
        for param in [
            "vm..swappiness",
            "vm//swappiness",
            "vm/swappiness/",
            "/vm.swappiness",
            "net/ipv4//tcp_rfc1337",
        ] {
            let error = why_with(param, &eval, reader)
                .expect_err("a degenerate spelling must not resolve to a parameter");
            assert_eq!(error.to_string(), format!("parameter not found: {param}"));
        }
        // The canonical and the documented slash alias still answer.
        for param in ["vm.swappiness", "vm/swappiness"] {
            let (output, code) =
                why_with(param, &eval, reader).expect("the aliases must keep resolving");
            assert_eq!(output["param"], json!("vm.swappiness"), "{param}");
            assert_eq!(output["status"], json!("optimal"), "{param}");
            assert_eq!(code, 0);
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

    /// A string sysctl is stored as raw bytes: `sethostname(2)` accepts any
    /// non-NUL byte up to the length limit (the util-linux `hostname` tool's
    /// charset check and systemd's hostname validation are policy, not the
    /// kernel), so one non-UTF-8 byte in `kernel.hostname` — or in
    /// `kernel.domainname` / `kernel.core_pattern`, which root writes as raw
    /// bytes — made `read_to_string` reject the whole file. `why` then
    /// refused a parameter that is right there with exit 2, contradicting the
    /// README ("read from its own sysfs/sysctl file … answers with its
    /// current value and exits 0"), while every other reader in the crate
    /// decodes such files lossily (detect's comm / cmdline / cpuinfo readers).
    /// Only the encoding is no longer a failure: an absent path is still "not
    /// found" and a read that really failed is still an error.
    #[test]
    fn why_reads_a_value_the_kernel_stored_as_raw_bytes() {
        let dir = std::env::temp_dir().join(format!("ktuner-why-raw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("hostname");
        std::fs::write(&raw, b"caf\xe9-host\n").unwrap();
        let value = read_current_value(raw.to_str().unwrap())
            .expect("a readable file must not fail on its encoding")
            .expect("an existing path reads as a value");
        assert_eq!(value.trim(), "caf\u{fffd}-host");
        // The two states that are still not values: an absent path is the
        // "not found" answer, and a read that really failed (the path is a
        // directory, like a write-only knob's EACCES) stays an error.
        assert!(read_current_value(dir.join("gone").to_str().unwrap())
            .expect("an absent path is not an error")
            .is_none());
        assert!(read_current_value(dir.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same value through the command's own answer: `why kernel.hostname`
    /// on a host whose hostname holds a non-UTF-8 byte reads `optimal` with
    /// the (lossily decoded) value and exits 0, instead of refusing the
    /// parameter with the README's error code.
    #[test]
    fn why_answers_for_a_file_the_kernel_stored_as_raw_bytes() {
        let dir =
            std::env::temp_dir().join(format!("ktuner-why-raw-answer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("hostname");
        std::fs::write(&raw, b"caf\xe9-host\n").unwrap();
        let eval = evaluation(Vec::new());
        // The reader is injected (the why_with seam): the path under test is
        // the fixture, not /proc/sys/kernel/hostname.
        let read = |_path: &str| read_current_value(raw.to_str().unwrap());
        let (output, code) = why_with("kernel.hostname", &eval, read)
            .expect("a readable parameter must be answered, not refused");
        assert_eq!(output["param"], json!("kernel.hostname"));
        assert_eq!(output["current"], json!("caf\u{fffd}-host"));
        assert_eq!(output["status"], json!("optimal"));
        assert_eq!(code, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn why_batch_entries_equal_the_single_param_bodies() {
        // The batch wraps `count` and `params` around the entries and adds
        // nothing inside them: each element is the object the same parameter
        // answers alone, byte for byte, so one parser reads an entry whatever
        // arity the caller asked for.
        let eval = evaluation(vec![rec("vm.swappiness", true)]);
        let current = CurrentFile::new("Linux\n");
        let read = |path: &str| Ok(current.read_for(path, "/proc/sys/kernel/ostype"));
        let params = vec!["vm.swappiness".to_string(), "kernel.ostype".to_string()];
        let (batch, code) =
            why_params_output(&params, &eval, &read).expect("both parameters must answer");
        assert_eq!(code, 1, "one recommendation keeps the batch at exit 1");
        assert_eq!(batch["count"], json!(2));
        let (single_rec, single_rec_code) =
            why_with("vm.swappiness", &eval, |_| Ok(None)).expect("the recommendation must answer");
        assert_eq!(single_rec_code, 1);
        let (single_optimal, single_optimal_code) = why_with("kernel.ostype", &eval, |path| {
            Ok(current.read_for(path, "/proc/sys/kernel/ostype"))
        })
        .expect("the fallback must answer");
        assert_eq!(single_optimal_code, 0);
        assert_eq!(
            serde_json::to_string_pretty(&batch["params"][0]).unwrap(),
            serde_json::to_string_pretty(&single_rec).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&batch["params"][1]).unwrap(),
            serde_json::to_string_pretty(&single_optimal).unwrap()
        );
    }

    #[test]
    fn why_batch_renders_each_parameter_in_the_order_given() {
        // The array follows the invocation, not a set order: entries keep the
        // order the caller gave, and `count` is the array's length so a
        // consumer can size the batch before parsing it.
        let eval = evaluation(vec![rec("vm.swappiness", true)]);
        let read = |path: &str| match path {
            "/proc/sys/kernel/ostype" => Ok(Some("Linux\n".into())),
            "/proc/sys/kernel/hostname" => Ok(Some("host\n".into())),
            _ => Ok(None),
        };
        let params = vec![
            "kernel.ostype".to_string(),
            "vm.swappiness".to_string(),
            "kernel.hostname".to_string(),
        ];
        let (batch, code) = why_params_output(&params, &eval, &read).expect("answers");
        assert_eq!(code, 1);
        assert_eq!(batch["count"], json!(3));
        let entries = batch["params"].as_array().expect("an array");
        assert_eq!(entries[0]["param"], json!("kernel.ostype"));
        assert_eq!(entries[0]["status"], json!("optimal"));
        assert_eq!(entries[1]["param"], json!("vm.swappiness"));
        assert_eq!(entries[2]["param"], json!("kernel.hostname"));
        assert_eq!(entries[2]["current"], json!("host"));
    }

    #[test]
    fn why_batch_dedupes_the_normalized_aliases() {
        // Two spellings of one parameter answer once, at the first position:
        // dedup uses the same normalize_param policy the lookup does, so the
        // entries and the count cannot drift from what each spelling answers
        // alone. Sysfs names are filesystem identities — a case change there
        // is a different parameter, exactly as the lookup treats it.
        let eval = evaluation(Vec::new());
        let current = CurrentFile::new("Linux\n");
        let read = |path: &str| Ok(current.read_for(path, "/proc/sys/kernel/ostype"));
        for aliases in [
            vec!["kernel.ostype", "kernel/ostype", "KERNEL.OSTYPE"],
            vec!["kernel.ostype", "KERNEL/OSTYPE"],
        ] {
            let params: Vec<String> = aliases.iter().map(|s| (*s).to_string()).collect();
            let (batch, code) = why_params_output(&params, &eval, &read).expect("answers");
            assert_eq!(code, 0);
            assert_eq!(batch["count"], json!(1), "{aliases:?}");
            assert_eq!(batch["params"].as_array().map(Vec::len), Some(1));
            assert_eq!(batch["params"][0]["param"], json!("kernel.ostype"));
            assert_eq!(batch["params"][0]["current"], json!("Linux"));
        }
        let sysfs = |path: &str| match path {
            "/sys/block/Disk.0/queue/scheduler" | "/sys/block/disk.0/queue/scheduler" => {
                Ok(Some("[none] mq-deadline\n".into()))
            }
            _ => Ok(None),
        };
        let params = vec![
            "block/Disk.0/scheduler".to_string(),
            "block/disk.0/scheduler".to_string(),
        ];
        let (batch, _) = why_params_output(&params, &eval, &sysfs).expect("answers");
        assert_eq!(
            batch["count"],
            json!(2),
            "sysfs identities stay verbatim: two parameters"
        );
    }

    #[test]
    fn why_batch_exit_code_aggregates_the_recommendations() {
        // Any recommendation keeps `check`'s verdict for the batch (exit 1,
        // attention needed) wherever it sits; a batch of optimal parameters
        // exits 0 like the single-parameter command.
        let eval = evaluation(vec![rec("vm.swappiness", true)]);
        let read = |path: &str| match path {
            "/proc/sys/kernel/ostype" => Ok(Some("Linux\n".into())),
            "/proc/sys/kernel/hostname" => Ok(Some("host\n".into())),
            _ => Ok(None),
        };
        let (batch, code) = why_params_output(
            &["kernel.ostype".to_string(), "kernel.hostname".to_string()],
            &eval,
            &read,
        )
        .expect("answers");
        assert_eq!(code, 0, "every entry optimal: {batch}");
        for params in [
            vec!["vm.swappiness", "kernel.ostype"],
            vec!["kernel.ostype", "vm.swappiness"],
            vec!["kernel.ostype", "kernel.hostname", "vm.swappiness"],
        ] {
            let params: Vec<String> = params.iter().map(|s| (*s).to_string()).collect();
            let (batch, code) = why_params_output(&params, &eval, &read).expect("answers");
            assert_eq!(code, 1, "{params:?}: {batch}");
        }
    }

    #[test]
    fn why_batch_refuses_the_whole_command_on_a_missing_parameter() {
        // One miss refuses the batch with the same command error a single
        // parameter gets, naming the parameter: the other entries were
        // answerable, but a partial document would let the batch mean
        // something other than what was asked. `cmd_why` prints only after
        // this renderer returns, so the refusal leaves stdout empty.
        let eval = evaluation(Vec::new());
        let current = CurrentFile::new("Linux\n");
        let read = |path: &str| Ok(current.read_for(path, "/proc/sys/kernel/ostype"));
        let error = why_params_output(
            &["kernel.ostype".to_string(), "no/such/param".to_string()],
            &eval,
            &read,
        )
        .expect_err("a miss must refuse the batch");
        assert_eq!(
            refusal_document(&error),
            "{\n  \"error\": \"parameter not found: no/such/param\"\n}"
        );
        // An unreadable parameter refuses the same way, with the read error.
        let unreadable = |path: &str| match path {
            "/proc/sys/vm/drop_caches" => Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "permission denied",
            )),
            _ => Ok(None),
        };
        let error = why_params_output(
            &["vm.drop_caches".to_string(), "no/such/param".to_string()],
            &eval,
            &unreadable,
        )
        .expect_err("an unreadable parameter must refuse the batch");
        let msg = error.to_string();
        assert!(
            msg.contains("cannot read current value of vm.drop_caches"),
            "{msg}"
        );
    }

    #[test]
    fn why_gathers_one_diagnosis_for_the_whole_invocation() {
        use std::sync::atomic::Ordering;
        // The batch's reason to exist: several parameters, one diagnosis. The
        // bodies would be identical if each parameter re-gathered, so the
        // probe counts instead — exactly one `gather()` for one and for three
        // parameters alike. The answers themselves are host-dependent, so
        // only the count and the exit code's range are asserted.
        for params in [
            vec!["kernel.ostype".to_string()],
            vec!["kernel.ostype".to_string(), "vm.swappiness".to_string()],
            vec![
                "kernel.ostype".to_string(),
                "vm.swappiness".to_string(),
                "kernel.hostname".to_string(),
            ],
        ] {
            GATHER_CALLS.store(0, Ordering::SeqCst);
            let code = cmd_why(&params).expect("a read-only why answers on a Linux host");
            assert!(matches!(code, 0 | 1), "{params:?}");
            assert_eq!(
                GATHER_CALLS.load(Ordering::SeqCst),
                1,
                "{params:?}: one invocation must gather one diagnosis"
            );
        }
    }

    #[test]
    fn rollback_output_matches_the_full_rollback_body() {
        // The positional is additive: a full rollback's body keeps its four
        // counters and gains no `param`/`params` key, in the same byte layout
        // (keys alphabetical, two-space indent, no trailing newline here — the
        // CLI adds one). The entries it restored are named beside them.
        let full = tuner::RollbackOutcome {
            restored_params: vec![
                "net.core.somaxconn".to_string(),
                "vm.swappiness".to_string(),
            ],
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        let body = rollback_output(None, &full);
        println!(
            "full rollback body:\n{}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"restored\": 2,\n  \"restored_params\": [\n    \"net.core.somaxconn\",\n    \"vm.swappiness\"\n  ],\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        for (restored, failed, skipped, status) in [
            (2, 1, 0, "Partial"),
            (0, 1, 0, "Nothing"),
            // 0 restored is Nothing whichever counter is nonzero: there is no
            // "partial" restore to report when nothing was restored.
            (0, 0, 1, "Nothing"),
        ] {
            let outcome = tuner::RollbackOutcome {
                restored_params: test_restored(restored),
                failures: test_failures(failed),
                missing: test_missing(skipped),
                leftovers: Vec::new(),
                boot_unit_enabled: true,
            };
            let mut expected = json!({
                "restored": restored,
                "failed": failed,
                "skipped": skipped,
                "status": status,
            });
            // The counts agree with the reports, which arrive only when they
            // have something to say.
            if failed > 0 {
                expected["failures"] = json!(test_failures(failed)
                    .iter()
                    .map(|failure| json!({
                        "param": failure.param,
                        "error": failure.error
                    }))
                    .collect::<Vec<_>>());
            }
            if skipped > 0 {
                expected["missing"] = json!(test_missing(skipped));
            }
            if restored > 0 {
                expected["restored_params"] = json!(test_restored(restored));
            }
            assert_eq!(rollback_output(None, &outcome), expected);
        }
    }

    /// An outcome that left entries names them: the counts stay what they
    /// were, and the two reports arrive beside them in the {param, error}
    /// shape `tune` uses for the same news.
    #[test]
    fn rollback_output_names_the_entries_a_rollback_left() {
        let outcome = tuner::RollbackOutcome {
            restored_params: test_restored(1),
            failures: test_failures(2),
            missing: test_missing(1),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_output(None, &outcome)).unwrap(),
            "{\n  \"failed\": 2,\n  \"failures\": [\n    {\n      \"error\": \"write refused\",\n      \"param\": \"vm.ktuner_failed_0\"\n    },\n    {\n      \"error\": \"write refused\",\n      \"param\": \"vm.ktuner_failed_1\"\n    }\n  ],\n  \"missing\": [\n    \"vm.ktuner_missing_0\"\n  ],\n  \"restored\": 1,\n  \"restored_params\": [\n    \"vm.ktuner_restored_0\"\n  ],\n  \"skipped\": 1,\n  \"status\": \"Partial\"\n}"
        );
    }

    /// Every invocation shape reports them, because every shape is built on
    /// the same base: the plain rollback, the single positional, the batch,
    /// and the bodies `--exclude` and `--category` wrap. A caller that named
    /// one parameter reads the same two keys as one that rolled back
    /// everything.
    #[test]
    fn rollback_body_names_the_entries_in_every_shape() {
        let outcome = tuner::RollbackOutcome {
            restored_params: test_restored(0),
            failures: test_failures(1),
            missing: test_missing(1),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        let one = ["vm.swappiness".to_string()];
        let two = ["vm.swappiness".to_string(), "vm.dirty_ratio".to_string()];
        let excluded = ["vm.swappiness".to_string()];
        for (shape, body) in [
            ("plain", rollback_body(0, &[], &outcome)),
            ("single positional", rollback_body(1, &one, &outcome)),
            ("batch", rollback_body(2, &two, &outcome)),
            // `--exclude` and `--category` answer with the full-rollback body
            // plus their own reports, added by the same wrapper.
            (
                "exclude",
                with_unmatched_exclude(rollback_body(0, &[], &outcome), &excluded),
            ),
        ] {
            assert_eq!(
                body["failures"],
                json!([{ "param": "vm.ktuner_failed_0", "error": "write refused" }]),
                "{shape}: the failed entries are named"
            );
            assert_eq!(
                body["missing"],
                json!(["vm.ktuner_missing_0"]),
                "{shape}: the missing entries are named"
            );
            // The counts keep their meaning: `failed` counts the entries the
            // report names, and a clean rollback's untouched body stays clean.
            assert_eq!(body["failed"], json!(1), "{shape}: failed count");
            assert_eq!(body["skipped"], json!(1), "{shape}: skipped count");
        }
    }

    /// A rollback that left nothing answers exactly as it did before the
    /// reports existed: the keys appear only when they have something to say,
    /// the rule `unmatched_exclude`, `unclassified` and `boot_unit_enabled`
    /// follow.
    #[test]
    fn rollback_output_hides_the_reports_when_nothing_was_left() {
        // An empty ledger: nothing was restored and nothing was left, so the
        // body is the four counters it has always been — the reports, the
        // restored entries included, appear only when they have something to
        // say.
        let empty = tuner::RollbackOutcome {
            restored_params: test_restored(0),
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        let body = rollback_output(None, &empty);
        assert!(body.get("failures").is_none(), "{body}");
        assert!(body.get("missing").is_none(), "{body}");
        assert!(body.get("restored_params").is_none(), "{body}");
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"restored\": 0,\n  \"skipped\": 0,\n  \"status\": \"Nothing\"\n}"
        );
        // A clean rollback that restored entries adds exactly that one key.
        let clean = tuner::RollbackOutcome {
            restored_params: test_restored(2),
            ..empty
        };
        let body = rollback_output(None, &clean);
        assert!(body.get("failures").is_none(), "{body}");
        assert!(body.get("missing").is_none(), "{body}");
        assert_eq!(
            body["restored_params"],
            json!(["vm.ktuner_restored_0", "vm.ktuner_restored_1"]),
            "{body}"
        );
    }

    #[test]
    fn rollback_output_reports_an_unarmed_boot_replay() {
        // A partial rollback re-runs `systemctl enable` for the entries it
        // keeps; a refused enable must reach the body under the key `tune` and
        // `fix` publish, and an armed replay keeps the body unchanged.
        let unarmed = tuner::RollbackOutcome {
            restored_params: test_restored(1),
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: false,
        };
        for body in [
            rollback_output(None, &unarmed),
            rollback_output(Some("vm.swappiness"), &unarmed),
            rollback_params_output(&["vm.swappiness".to_string()], &unarmed),
        ] {
            assert_eq!(body["boot_unit_enabled"], json!(false), "{body}");
        }

        let armed = tuner::RollbackOutcome {
            leftovers: Vec::new(),
            boot_unit_enabled: true,
            ..unarmed
        };
        assert!(rollback_output(Some("vm.swappiness"), &armed)
            .get("boot_unit_enabled")
            .is_none());
    }

    #[test]
    fn rollback_output_names_the_restored_param() {
        // The single-parameter body adds exactly one key, carrying the ledger
        // entry that was retired — the spelling `rollback --list` publishes.
        // A recorded mutually exclusive twin is restored and counted too, so
        // `restored` may be 2 while `param` names the entry the caller chose.
        let twins = tuner::RollbackOutcome {
            restored_params: vec!["vm.dirty_bytes".to_string(), "vm.dirty_ratio".to_string()],
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        let body = rollback_output(Some("vm.dirty_bytes"), &twins);
        println!(
            "single-parameter rollback body:\n{}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"param\": \"vm.dirty_bytes\",\n  \"restored\": 2,\n  \"restored_params\": [\n    \"vm.dirty_bytes\",\n    \"vm.dirty_ratio\"\n  ],\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        assert_eq!(
            body["restored_params"],
            json!(["vm.dirty_bytes", "vm.dirty_ratio"]),
            "the twin the caller never named is the point of the report: {body}"
        );
        let skipped = tuner::RollbackOutcome {
            restored_params: test_restored(0),
            failures: test_failures(0),
            missing: test_missing(1),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_output(Some("vm.swappiness"), &skipped))
                .unwrap(),
            "{\n  \"failed\": 0,\n  \"missing\": [\n    \"vm.ktuner_missing_0\"\n  ],\n  \"param\": \"vm.swappiness\",\n  \"restored\": 0,\n  \"skipped\": 1,\n  \"status\": \"Nothing\"\n}"
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
        // introduced: the same four counters plus `param` naming the ledger
        // entry that was addressed, and the entries the attempt restored.
        let twins = tuner::RollbackOutcome {
            restored_params: vec!["vm.dirty_bytes".to_string(), "vm.dirty_ratio".to_string()],
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_body(
                1,
                &["vm.dirty_bytes".to_string()],
                &twins
            ))
            .unwrap(),
            "{\n  \"failed\": 0,\n  \"param\": \"vm.dirty_bytes\",\n  \"restored\": 2,\n  \"restored_params\": [\n    \"vm.dirty_bytes\",\n    \"vm.dirty_ratio\"\n  ],\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        let skipped = tuner::RollbackOutcome {
            restored_params: test_restored(0),
            failures: test_failures(0),
            missing: test_missing(1),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        assert_eq!(
            serde_json::to_string_pretty(&rollback_body(1, &["vm.swappiness".to_string()], &skipped))
                .unwrap(),
            "{\n  \"failed\": 0,\n  \"missing\": [\n    \"vm.ktuner_missing_0\"\n  ],\n  \"param\": \"vm.swappiness\",\n  \"restored\": 0,\n  \"skipped\": 1,\n  \"status\": \"Nothing\"\n}"
        );
    }

    #[test]
    fn rollback_body_renders_the_multi_param_shape() {
        // Two or more positionals replace `param` (string) with `params`
        // (array) and keep the aggregate counters, so a consumer reads the
        // same four keys whatever it asked for. The array is the ledger key
        // of each positional, in the order given.
        let keys = vec![
            "net.core.somaxconn".to_string(),
            "vm.swappiness".to_string(),
            "vm.vfs_cache_pressure".to_string(),
        ];
        let outcome = tuner::RollbackOutcome {
            restored_params: keys.clone(),
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        let body = rollback_body(keys.len(), &keys, &outcome);
        println!(
            "multi-parameter rollback body:\n{}",
            serde_json::to_string_pretty(&body).unwrap()
        );
        assert_eq!(
            serde_json::to_string_pretty(&body).unwrap(),
            "{\n  \"failed\": 0,\n  \"params\": [\n    \"net.core.somaxconn\",\n    \"vm.swappiness\",\n    \"vm.vfs_cache_pressure\"\n  ],\n  \"restored\": 3,\n  \"restored_params\": [\n    \"net.core.somaxconn\",\n    \"vm.swappiness\",\n    \"vm.vfs_cache_pressure\"\n  ],\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
        );
        assert!(
            body.get("param").is_none(),
            "`params` replaces `param`, the two never appear together: {body}"
        );
        // Only the shape changes: a partial batch reports the same status
        // vocabulary and counts as the single-parameter form.
        let partial = tuner::RollbackOutcome {
            restored_params: test_restored(1),
            failures: test_failures(1),
            missing: test_missing(1),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
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
                "failures": [{ "param": "vm.ktuner_failed_0", "error": "write refused" }],
                "missing": ["vm.ktuner_missing_0"],
                "restored_params": test_restored(1),
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
            restored_params: test_restored(1),
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
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
                "restored_params": test_restored(1),
            })
        );
    }

    #[test]
    fn rollback_body_keeps_the_full_rollback_bytes() {
        // No positional keeps the full-rollback body and gains no
        // `param`/`params` key: the batch shape must not leak into it. The
        // entries it restored are named, because this body is the only record
        // of what a full rollback undid once the ledger is deleted.
        let full = tuner::RollbackOutcome {
            restored_params: vec![
                "net.core.somaxconn".to_string(),
                "vm.swappiness".to_string(),
            ],
            failures: test_failures(0),
            missing: test_missing(0),
            leftovers: Vec::new(),
            boot_unit_enabled: true,
        };
        assert_eq!(rollback_body(0, &[], &full), rollback_output(None, &full));
        assert_eq!(
            serde_json::to_string_pretty(&rollback_body(0, &[], &full)).unwrap(),
            "{\n  \"failed\": 0,\n  \"restored\": 2,\n  \"restored_params\": [\n    \"net.core.somaxconn\",\n    \"vm.swappiness\"\n  ],\n  \"skipped\": 0,\n  \"status\": \"Full\"\n}"
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

    #[test]
    fn system_json_reports_the_collected_system_identity() {
        // The `system` section is the report's host identity, read from the
        // same `SystemInfo` the rules diagnosed: a caller can identify the
        // host and explain a disk or link suggestion the rules did not make
        // without re-reading it. Every assertion compares a key against the
        // gathered value, so the section cannot drift from what the rules
        // saw, and the key sets pin the exact shape each entry publishes.
        let info = detect::gather_system_info().expect("read this host's system info");
        let system = system_json(&info);
        let mut keys: Vec<&str> = system
            .as_object()
            .expect("system_json builds an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "cpu_cores",
                "cpu_cores_host",
                "cpu_model",
                "disks",
                "kernel",
                "memory_gb",
                "memory_gb_host",
                "network",
                "numa_nodes",
                "os_distro",
            ]
        );
        assert_eq!(system["kernel"], json!(info.kernel_version.as_str()));
        assert_eq!(system["cpu_cores"], json!(info.cpu_cores));
        assert_eq!(system["cpu_cores_host"], json!(info.cpu_cores_host));
        assert_eq!(system["memory_gb"], json!(info.memory_total_gb));
        assert_eq!(system["memory_gb_host"], json!(info.memory_gb_host));
        assert_eq!(system["numa_nodes"], json!(info.numa_nodes));
        assert_eq!(system["os_distro"], json!(info.os_distro.as_str()));
        assert_eq!(system["cpu_model"], json!(info.cpu_model.as_str()));
        // The host readings are the un-clamped pair: at or above the values
        // the rules scale by, equal exactly when no cgroup limit binds.
        assert!(info.cpu_cores_host >= info.cpu_cores);
        assert!(info.memory_gb_host >= info.memory_total_gb);

        let disks = system["disks"].as_array().expect("disks is an array");
        assert_eq!(disks.len(), info.disks.len());
        for (entry, disk) in disks.iter().zip(&info.disks) {
            let mut keys: Vec<&str> = entry
                .as_object()
                .expect("a disk entry is an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(
                keys,
                [
                    "available_schedulers",
                    "hidden",
                    "holders",
                    "name",
                    "nr_requests",
                    "read_ahead_kb",
                    "rq_affinity",
                    "scheduler",
                    "type",
                ]
            );
            assert_eq!(entry["name"], json!(disk.name.as_str()));
            assert_eq!(entry["type"], json!(disk.disk_type.to_string()));
            assert_eq!(entry["scheduler"], json!(disk.scheduler.as_str()));
            assert_eq!(
                entry["available_schedulers"],
                json!(disk.available_schedulers)
            );
            assert_eq!(entry["nr_requests"], json!(disk.nr_requests.unwrap_or(0)));
            assert_eq!(
                entry["read_ahead_kb"],
                json!(disk.read_ahead_kb.unwrap_or(0))
            );
            assert_eq!(entry["rq_affinity"], json!(disk.rq_affinity.unwrap_or(0)));
            assert_eq!(entry["hidden"], json!(disk.hidden));
            assert_eq!(entry["holders"], json!(disk.holders));
        }

        let network = system["network"].as_array().expect("network is an array");
        assert_eq!(network.len(), info.network.len());
        for (entry, link) in network.iter().zip(&info.network) {
            assert_eq!(
                entry,
                &json!({"name": link.name.as_str(), "speed_mbps": link.speed_mbps})
            );
        }
    }
}
