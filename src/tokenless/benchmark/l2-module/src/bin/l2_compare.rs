// Copyright 2026 Alibaba Cloud
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! L2 comparison harness entry point.
//!
//! Orchestrates the full run: load samples → pilot to size N → paired
//! collection per category → retention → optional semantic probe → stats →
//! task simulations → report. Every unavailable toolchain degrades the run
//! (recorded in the report) instead of aborting, so a partial environment
//! still yields a labelled, honest report.
//!
//! Usage:
//!
//! ```text
//! l2_compare [--categories all|json,command,grep,code,diff]
//!            [--n auto|<int>] [--no-probe] [--model <id>]
//!            [--report-dir <dir>]
//! ```

use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use tokenless_l2_bench::l2::report::{
    CategoryComparison, GapStats, Report, RunSummary, SideAggregate, TaskComparison,
};
use tokenless_l2_bench::l2::samples::{self, CommandSpec, ProbeQuestion};
use tokenless_l2_bench::l2::task_sim::{self, SampleSideStats, SideLookup};
use tokenless_l2_bench::l2::{
    Category, GroundTruth, L2Error, SampleRecord, headroom_side, probe, retention, rtk_side, stats,
    tokenizer, tokenless_side,
};

/// Parsed CLI options.
struct Options {
    categories: Vec<Category>,
    /// `None` = auto (pilot-inferred N); `Some(n)` = fixed repetition count.
    n: Option<usize>,
    no_probe: bool,
    model: Option<String>,
    report_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Options> {
    let mut opts = Options {
        categories: Category::ALL.to_vec(),
        n: None,
        no_probe: false,
        model: None,
        report_dir: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--categories" => {
                let value = args.next().context("--categories requires a value")?;
                if value != "all" {
                    let mut cats = Vec::new();
                    for name in value.split(',') {
                        let cat = Category::parse(name.trim())
                            .with_context(|| format!("unknown category {name:?}"))?;
                        if !cats.contains(&cat) {
                            cats.push(cat);
                        }
                    }
                    if cats.is_empty() {
                        bail!("--categories parsed to an empty set");
                    }
                    opts.categories = cats;
                }
            }
            "--n" => {
                let value = args.next().context("--n requires a value")?;
                if value != "auto" {
                    let n: usize = value
                        .parse()
                        .with_context(|| format!("bad --n {value:?}"))?;
                    if n == 0 {
                        bail!("--n must be at least 1");
                    }
                    opts.n = Some(n);
                }
            }
            "--no-probe" => opts.no_probe = true,
            "--model" => opts.model = Some(args.next().context("--model requires a value")?),
            "--report-dir" => {
                opts.report_dir = Some(PathBuf::from(
                    args.next().context("--report-dir requires a value")?,
                ));
            }
            other => bail!("unknown argument {other:?} (see file-top usage)"),
        }
    }
    Ok(opts)
}

/// One collected measurement row: one side of one sample repetition.
struct Measure {
    sample_id: String,
    /// Repetition index the row was collected in — the pairing key that
    /// keeps gap statistics aligned when one side degrades mid-run.
    rep: usize,
    /// Hex digest of the exact payload this row measured.
    ///
    /// Two rows with the same digest measured byte-identical input, so they
    /// carry no independent information for compression or retention no matter
    /// which category they came from. Deriving independence from the data
    /// rather than from a per-category assumption is what keeps a
    /// deterministic-in-practice command (`git log`, `git diff`) from inflating
    /// N just because its category is nominally "dynamic".
    payload_hash: String,
    compression_rate: f64,
    /// Side-report rate under cl100k_base (tokenizer-sensitivity check;
    /// never gated, o200k stays the headline metric).
    compression_rate_cl100k: f64,
    latency_s: f64,
    latency_basis: &'static str,
    tokens_before: usize,
    tokens_after: usize,
    /// headroom's self-reported token counts, kept as cross-check evidence
    /// against the tiktoken headline numbers; `None` on the other sides.
    hr_tokens_before: Option<u64>,
    hr_tokens_after: Option<u64>,
    retention_passed: usize,
    retention_total: usize,
    retention_failures: Vec<String>,
}

/// `1 - after/before` guarded against an empty input.
fn compression_rate(before: usize, after: usize) -> f64 {
    if before == 0 {
        0.0
    } else {
        1.0 - after as f64 / before as f64
    }
}

/// Per-category collection state for one side.
#[derive(Default)]
struct SideSeries {
    measures: Vec<Measure>,
    /// Representative (original, compressed) texts per sample for probing —
    /// taken from the first repetition, which is deterministic for static
    /// samples and near-deterministic for the spec'd git/rg commands.
    probe_texts: HashMap<String, (String, String)>,
    /// Per-sample content ground truth for building probe questions where a
    /// category's samples are heterogeneous (diff, whose entry points range
    /// from full passthrough to a two-line summary). Empty for categories that
    /// share one asset-defined probe set: a single question set applied across
    /// samples with different content silently drops any sample whose original
    /// does not contain those facts, hiding the loss it was meant to catch.
    probe_ground_truth: HashMap<String, Vec<GroundTruth>>,
}

impl SideSeries {
    fn record(&mut self, m: Measure, original: &str, compressed: &str) {
        self.probe_texts
            .entry(m.sample_id.clone())
            .or_insert_with(|| (original.to_string(), compressed.to_string()));
        self.measures.push(m);
    }

    /// Records a sample's own content ground truth for per-sample probing.
    /// No-op for empty truth, so categories that probe from an asset are left
    /// on the category-global path.
    fn record_probe_ground_truth(&mut self, sample_id: &str, ground_truth: &[GroundTruth]) {
        if !ground_truth.is_empty() {
            self.probe_ground_truth
                .entry(sample_id.to_string())
                .or_insert_with(|| ground_truth.to_vec());
        }
    }
}

/// Everything gathered for one category.
struct CategoryData {
    category: Category,
    reps: usize,
    tokenless: SideSeries,
    headroom: SideSeries,
}

fn main() -> Result<()> {
    let opts = parse_args()?;
    let l2_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");
    // Command specs run relative to the repository root (l2-module/ is
    // four levels below <repo>/src/tokenless/benchmark/l2-module).
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .context("cannot resolve repository root")?;
    // Reports go to the workspace's own reports/ directory (a sibling of
    // assets/, gitignored) so L1 and L2 results never mix and no run artifact
    // can be committed.
    let report_dir = opts
        .report_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("reports"));

    let mut degradations: Vec<String> = Vec::new();

    // --- toolchain discovery -------------------------------------------
    let mut worker = match spawn_worker(&l2_dir) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("[l2] headroom degraded: {e}");
            degradations.push(format!("headroom side skipped: {e}"));
            None
        }
    };
    let rtk_bin = match rtk_side::locate_rtk() {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("[l2] rtk degraded: {e}");
            degradations.push(format!("rtk side skipped: {e}"));
            None
        }
    };
    let probe_client = if opts.no_probe {
        degradations.push("semantic probe disabled by --no-probe".to_string());
        None
    } else {
        match probe::ProbeClient::new(&l2_dir, opts.model.as_deref()) {
            Some(c) => Some(c),
            None => {
                degradations.push(
                    "semantic probe disabled: DASHSCOPE_API_KEY unset (all S = None)".to_string(),
                );
                None
            }
        }
    };

    // --- per-category collection ----------------------------------------
    let command_specs = samples::load_command_specs(&l2_dir)?;
    let mut collected: Vec<CategoryData> = Vec::new();
    for &category in &opts.categories {
        eprintln!("[l2] collecting category {category}");
        let data = if category.is_dynamic() {
            collect_dynamic(
                category,
                &command_specs,
                &repo_root,
                rtk_bin.as_deref(),
                worker.as_mut(),
                opts.n,
                &mut degradations,
            )?
        } else {
            collect_static(
                category,
                &l2_dir,
                worker.as_mut(),
                opts.n,
                &mut degradations,
            )?
        };
        collected.push(data);
    }

    // --- probe + aggregation ---------------------------------------------
    let mut categories = Vec::new();
    for data in &collected {
        let questions: Option<Vec<ProbeQuestion>> = if probe_client.is_some() {
            // diff builds its probe questions per sample from each sample's own
            // content (see SideSeries::probe_ground_truth): its entry points are
            // heterogeneous, so one shared question set would silently skip the
            // samples that lost everything. probe_file_stem returns None for it.
            match samples::probe_file_stem(data.category) {
                Some(stem) => Some(samples::load_probe_questions(&l2_dir, stem)?),
                None => None,
            }
        } else {
            None
        };
        let tokenless = aggregate_side(
            &data.tokenless,
            data.reps,
            probe_client.as_ref(),
            questions.as_deref(),
        );
        let headroom = aggregate_side(
            &data.headroom,
            data.reps,
            probe_client.as_ref(),
            questions.as_deref(),
        );
        categories.push(CategoryComparison {
            category: data.category,
            // Withhold the gap outright when the sides were fed different
            // bytes: a number here would be read as a head-to-head result.
            compression_gap: if data.category.has_symmetric_inputs() {
                compression_gap(&data.tokenless, &data.headroom)
            } else {
                None
            },
            input_asymmetry: data
                .category
                .input_asymmetry_reason()
                .map(|s| s.to_string()),
            tokenless,
            headroom,
        });
    }
    if let Some(Err(e)) = probe_client.as_ref().map(|client| client.save_cache()) {
        eprintln!("[l2] warning: probe cache not saved: {e}");
    }

    // --- task simulations --------------------------------------------------
    let tl_lookup = side_lookup(&collected, |d| &d.tokenless);
    let hr_lookup = side_lookup(&collected, |d| &d.headroom);
    let tasks = task_sim::tasks()
        .iter()
        .map(|task| {
            // Surface the asymmetry at task level too: a task that replays a
            // code sample carries the same non-comparability into its totals.
            let mut asymmetric: Vec<String> = task
                .steps
                .iter()
                .filter(|s| !s.category.has_symmetric_inputs())
                .map(|s| s.category.name().to_string())
                .collect();
            asymmetric.sort();
            asymmetric.dedup();
            TaskComparison {
                name: task.name.to_string(),
                notes: task.notes.to_string(),
                asymmetric_categories: asymmetric,
                tokenless: replay_if_covered(task, &tl_lookup),
                headroom: replay_if_covered(task, &hr_lookup),
            }
        })
        .collect();

    // --- report --------------------------------------------------------------
    // Capture comparator provenance before the worker is dropped: a report that
    // names only the anolisa sha cannot distinguish two runs whose headroom
    // builds differed.
    let hr_provenance = worker
        .as_ref()
        .map(|w| w.provenance().clone())
        .unwrap_or_default();
    // rtk provenance: the binary lives at <rtk>/target/release/rtk, so its
    // source tree is three levels up. Recorded because that tree is a pinned
    // clone kept out of version control — without it a run reads no rtk revision
    // at all.
    let (rtk_revision, rtk_dirty) = match rtk_bin.as_deref() {
        Some(bin) => rtk_provenance(bin),
        None => (None, None),
    };
    let report = Report {
        summary: RunSummary {
            date: chrono::Utc::now().to_rfc3339(),
            git_sha: git_sha(&repo_root),
            git_dirty: git_dirty(&repo_root),
            untracked_build_inputs: untracked_build_inputs(&repo_root),
            headroom_revision: hr_provenance.revision,
            headroom_dirty: hr_provenance.dirty,
            headroom_untracked: hr_provenance.untracked,
            rtk_revision,
            rtk_dirty,
            platform: format!("{}/{}", std::env::consts::OS, std::env::consts::ARCH),
            headroom_available: worker.is_some(),
            rtk_available: rtk_bin.is_some(),
            probe_model: probe_client.as_ref().map(|c| c.model().to_string()),
            degradations,
        },
        categories,
        tasks,
    };
    report.write(&report_dir)?;
    println!("report written to {}", report_dir.display());
    for finding in report.quality_gate() {
        println!("quality-gate: {finding}");
    }
    Ok(())
}

fn spawn_worker(l2_dir: &Path) -> Result<headroom_side::HeadroomWorker, L2Error> {
    let python = std::env::var("HEADROOM_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let script = l2_dir.join("worker/headroom_worker.py");
    headroom_side::HeadroomWorker::spawn(&python, &script)
}

/// Static categories (json/code): both sides compress the committed sample
/// text; repetitions re-measure latency while compression output stays
/// deterministic.
fn collect_static(
    category: Category,
    l2_dir: &Path,
    mut worker: Option<&mut headroom_side::HeadroomWorker>,
    fixed_n: Option<usize>,
    degradations: &mut Vec<String>,
) -> Result<CategoryData> {
    let records = match category {
        Category::Json => samples::load_json_samples(l2_dir)?,
        Category::Code => samples::load_code_samples(l2_dir)?,
        // collect_static is only dispatched for static categories.
        _ => unreachable!("collect_static called with dynamic category {category}"),
    };
    let mut data = CategoryData {
        category,
        reps: 0,
        tokenless: SideSeries::default(),
        headroom: SideSeries::default(),
    };

    let mut rep = 0usize;
    let mut target = fixed_n.unwrap_or(stats::PILOT_N);
    let mut pilot_latency: Vec<f64> = Vec::new();
    while rep < target {
        let mut rep_latency = Vec::new();
        for record in &records {
            match measure_tokenless(record) {
                Ok((mut m, original, compressed)) => {
                    m.rep = rep;
                    rep_latency.push(m.latency_s);
                    data.tokenless.record(m, &original, &compressed);
                }
                Err(e) => {
                    degradations.push(format!("tokenless {category}/{} rep {rep}: {e}", record.id))
                }
            }
            if let Some(w) = worker.as_deref_mut() {
                match measure_headroom(
                    w,
                    record.category,
                    &record.id,
                    &record.content,
                    &record.ground_truth,
                ) {
                    Ok((mut m, original, compressed)) => {
                        m.rep = rep;
                        data.headroom.record(m, &original, &compressed);
                    }
                    Err(e) => degradations
                        .push(format!("headroom {category}/{} rep {rep}: {e}", record.id)),
                }
            }
        }
        rep += 1;
        // Auto sizing: infer N once from the pilot's per-rep mean latency.
        // Compression output is deterministic here, so latency is the only
        // metric with run-to-run variance worth sizing for.
        if fixed_n.is_none() {
            pilot_latency.push(stats::mean(&rep_latency));
            if rep == stats::PILOT_N {
                target = stats::infer_n(&pilot_latency);
            }
        }
    }
    data.reps = rep;
    Ok(data)
}

/// Dynamic categories (command/grep/diff): each repetition executes the
/// paired raw + rtk-wrapped command; ground truth is extracted per rep from
/// the raw output of that very run.
fn collect_dynamic(
    category: Category,
    specs: &[CommandSpec],
    repo_root: &Path,
    rtk_bin: Option<&Path>,
    mut worker: Option<&mut headroom_side::HeadroomWorker>,
    fixed_n: Option<usize>,
    degradations: &mut Vec<String>,
) -> Result<CategoryData> {
    let specs: Vec<&CommandSpec> = specs
        .iter()
        .filter(|s| s.category == category.name())
        .collect();
    let mut data = CategoryData {
        category,
        reps: 0,
        tokenless: SideSeries::default(),
        headroom: SideSeries::default(),
    };
    if specs.is_empty() {
        degradations.push(format!("no command specs found for category {category}"));
        return Ok(data);
    }

    let mut rep = 0usize;
    let mut target = fixed_n.unwrap_or(stats::PILOT_N);
    let mut pilot_latency: Vec<f64> = Vec::new();
    while rep < target {
        let mut rep_latency = Vec::new();
        for spec in &specs {
            let cwd = repo_root.join(&spec.cwd_rel);
            let (raw_text, rtk_result) = match rtk_bin {
                Some(bin) => {
                    match rtk_side::run_paired(bin, &spec.argv, spec.rtk_invocation(), &cwd) {
                        Ok(pair) => (pair.raw_text.clone(), Some(pair)),
                        Err(e) => {
                            degradations.push(format!("{category}/{} rep {rep}: {e}", spec.id));
                            continue;
                        }
                    }
                }
                // Without rtk the raw command still runs so the headroom
                // side keeps its full sample set.
                None => match run_raw(&spec.argv, &cwd) {
                    Ok(text) => (text, None),
                    Err(e) => {
                        degradations.push(format!("{category}/{} rep {rep}: {e}", spec.id));
                        continue;
                    }
                },
            };
            let ground_truth = samples::extract_dynamic_ground_truth(category, &raw_text)?;
            // For diff, register each sample's own content facts so the probe
            // asks about what THIS sample changed. The facts come from the raw
            // (uncompressed) output, so a sample that loses everything under
            // compression is scored against the identifiers it should have
            // kept, not against another sample's. Both sides share the facts;
            // the map ignores samples a side never probes.
            if category == Category::Diff {
                data.tokenless
                    .record_probe_ground_truth(&spec.id, &ground_truth);
                data.headroom
                    .record_probe_ground_truth(&spec.id, &ground_truth);
            }

            if let Some(pair) = rtk_result {
                match measure_rtk(&spec.id, &pair, &ground_truth) {
                    Ok(mut m) => {
                        m.rep = rep;
                        rep_latency.push(m.latency_s);
                        data.tokenless.record(m, &pair.raw_text, &pair.rtk_text);
                    }
                    Err(e) => {
                        degradations.push(format!("rtk {category}/{} rep {rep}: {e}", spec.id))
                    }
                }
            }
            if let Some(w) = worker.as_deref_mut() {
                match measure_headroom(w, category, &spec.id, &raw_text, &ground_truth) {
                    Ok((mut m, original, compressed)) => {
                        m.rep = rep;
                        data.headroom.record(m, &original, &compressed);
                    }
                    Err(e) => {
                        degradations.push(format!("headroom {category}/{} rep {rep}: {e}", spec.id))
                    }
                }
            }
        }
        rep += 1;
        if fixed_n.is_none() {
            pilot_latency.push(stats::mean(&rep_latency));
            if rep == stats::PILOT_N {
                target = stats::infer_n(&pilot_latency);
            }
        }
    }
    data.reps = rep;
    Ok(data)
}

fn run_raw(argv: &[String], cwd: &Path) -> Result<String, L2Error> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| L2Error::Command("empty argv".to_string()))?;
    let out = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| L2Error::Command(format!("spawn {program:?} failed: {e}")))?;
    if !out.status.success() {
        return Err(L2Error::Command(format!(
            "raw command {argv:?} exited with {}",
            out.status
        )));
    }
    // Same merge as the rtk-wrapped path: a missing newline between stdout and
    // stderr would fuse two lines and defeat the line-anchored ground-truth
    // regexes, making this degraded path disagree with the paired path.
    Ok(rtk_side::merge_streams(&out.stdout, &out.stderr))
}

fn measure_tokenless(record: &SampleRecord) -> Result<(Measure, String, String), L2Error> {
    let before_wire = tokenless_side::wire_before(record.category, &record.content)?;
    let output = tokenless_side::compress(record.category, &record.content)?;
    let before = tokenizer::count(&before_wire)?;
    let after = tokenizer::count(&output.compressed)?;
    let ret = retention::check(&record.ground_truth, &output.retention_text)?;
    let measure = Measure {
        sample_id: record.id.clone(),
        rep: 0,
        payload_hash: payload_hash(&before_wire),
        compression_rate: compression_rate(before.o200k, after.o200k),
        compression_rate_cl100k: compression_rate(before.cl100k, after.cl100k),
        latency_s: output.latency_s,
        latency_basis: tokenless_side::LATENCY_BASIS,
        tokens_before: before.o200k,
        tokens_after: after.o200k,
        hr_tokens_before: None,
        hr_tokens_after: None,
        retention_passed: ret.passed,
        retention_total: ret.total,
        retention_failures: ret.failures,
    };
    Ok((measure, before_wire, output.compressed))
}

fn measure_headroom(
    worker: &mut headroom_side::HeadroomWorker,
    _category: Category,
    sample_id: &str,
    content: &str,
    ground_truth: &[GroundTruth],
) -> Result<(Measure, String, String), L2Error> {
    let resp = worker.compress(content, "")?;
    // `compress` guarantees `compressed` is present on the Ok path.
    let compressed = resp.compressed.unwrap_or_default();
    let before = tokenizer::count(content)?;
    let after = tokenizer::count(&compressed)?;
    let ret = retention::check(ground_truth, &compressed)?;
    let measure = Measure {
        sample_id: sample_id.to_string(),
        rep: 0,
        payload_hash: payload_hash(content),
        compression_rate: compression_rate(before.o200k, after.o200k),
        compression_rate_cl100k: compression_rate(before.cl100k, after.cl100k),
        latency_s: resp.wall_time_s.unwrap_or(0.0),
        latency_basis: headroom_side::LATENCY_BASIS,
        tokens_before: before.o200k,
        tokens_after: after.o200k,
        hr_tokens_before: resp.hr_tokens_before,
        hr_tokens_after: resp.hr_tokens_after,
        retention_passed: ret.passed,
        retention_total: ret.total,
        retention_failures: ret.failures,
    };
    Ok((measure, content.to_string(), compressed))
}

fn measure_rtk(
    sample_id: &str,
    pair: &rtk_side::PairedRun,
    ground_truth: &[GroundTruth],
) -> Result<Measure, L2Error> {
    let before = tokenizer::count(&pair.raw_text)?;
    let after = tokenizer::count(&pair.rtk_text)?;
    let ret = retention::check(ground_truth, &pair.rtk_text)?;
    Ok(Measure {
        sample_id: sample_id.to_string(),
        rep: 0,
        payload_hash: payload_hash(&pair.raw_text),
        compression_rate: compression_rate(before.o200k, after.o200k),
        compression_rate_cl100k: compression_rate(before.cl100k, after.cl100k),
        latency_s: pair.rtk_overhead_s(),
        latency_basis: rtk_side::LATENCY_BASIS,
        tokens_before: before.o200k,
        tokens_after: after.o200k,
        hr_tokens_before: None,
        hr_tokens_after: None,
        retention_passed: ret.passed,
        retention_total: ret.total,
        retention_failures: ret.failures,
    })
}

/// Hex SHA-256 of a payload, used as the independence key for statistics.
fn payload_hash(payload: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(payload.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// The measures that count as *independent* observations for compression and
/// retention statistics: one per distinct payload.
///
/// Repeating a sample only produces new information when the payload actually
/// differs. Static categories compress deterministically, and the spec'd
/// `git log` / `git diff` commands are byte-identical across repetitions on a
/// fixed checkout too — measured, not assumed: five reruns of each yield one
/// unique output. Feeding those copies into bootstrap/Wilson is
/// pseudo-replication; it narrows the intervals without adding a single new
/// payload. Keying on the payload digest rather than on the category means a
/// nominally "dynamic" command that turns out to be deterministic is collapsed
/// correctly, and a nominally "static" one that turns out to vary is kept.
///
/// Latency deliberately does **not** go through this filter — it varies
/// run-to-run even when the payload does not, which is the whole reason the
/// repetitions exist.
fn independent_measures(series: &SideSeries) -> Vec<&Measure> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    series
        .measures
        .iter()
        .filter(|m| seen.insert((m.sample_id.as_str(), m.payload_hash.as_str())))
        .collect()
}

fn aggregate_side(
    series: &SideSeries,
    reps: usize,
    probe_client: Option<&probe::ProbeClient>,
    questions: Option<&[ProbeQuestion]>,
) -> Option<SideAggregate> {
    if series.measures.is_empty() {
        return None;
    }
    let obs = independent_measures(series);
    let rates: Vec<f64> = obs.iter().map(|m| m.compression_rate).collect();
    let rates_cl100k: Vec<f64> = obs.iter().map(|m| m.compression_rate_cl100k).collect();
    let latencies_ms: Vec<f64> = series
        .measures
        .iter()
        .map(|m| m.latency_s * 1000.0)
        .collect();
    let retention_passed: usize = obs.iter().map(|m| m.retention_passed).sum();
    let retention_total: usize = obs.iter().map(|m| m.retention_total).sum();
    let retention_failures: Vec<String> = obs
        .iter()
        .flat_map(|m| m.retention_failures.clone())
        .collect();
    let retention_missing = SideAggregate::collect_missing(retention_failures);

    // Semantic score is pooled over samples (sum of per-question outcomes)
    // rather than averaged per sample, so samples with few answerable
    // questions do not dominate the ratio. The numerator counts only questions
    // the ORIGINAL text already answered, so losing a baseline-answerable fact
    // cannot be offset by a question that compression made answerable.
    let mut probed_samples = 0usize;
    let mut unprobed_samples = 0usize;
    let semantic_score = match probe_client {
        Some(client) => {
            let mut correct_unc = 0usize;
            let mut retained = 0usize;
            let mut asked_any = false;
            for (sample_id, (original, compressed)) in &series.probe_texts {
                // Per-sample questions where the sample carries its own content
                // facts (diff); otherwise the category-global asset questions.
                // Building diff questions per sample is what stops a sample that
                // lost everything from being dropped for lacking another
                // sample's facts.
                let per_sample = series
                    .probe_ground_truth
                    .get(sample_id)
                    .map(|gt| samples::diff_probe_questions(gt));
                let qs: &[ProbeQuestion] = match (&per_sample, questions) {
                    (Some(q), _) => q,
                    (None, Some(q)) => q,
                    (None, None) => {
                        unprobed_samples += 1;
                        continue;
                    }
                };
                if qs.is_empty() {
                    unprobed_samples += 1;
                    continue;
                }
                asked_any = true;
                probed_samples += 1;
                let score = client.score(qs, original, compressed);
                correct_unc += score.correct_uncompressed;
                retained += score.retained;
            }
            if !asked_any || correct_unc == 0 {
                None
            } else {
                Some(retained as f64 / correct_unc as f64)
            }
        }
        None => None,
    };

    // headroom self-reported counts, averaged as cross-check evidence for
    // the tiktoken headline numbers (None on sides that never report them).
    let mut hr_before_sum = 0u64;
    let mut hr_after_sum = 0u64;
    let mut hr_count = 0u64;
    for m in &series.measures {
        if let (Some(b), Some(a)) = (m.hr_tokens_before, m.hr_tokens_after) {
            hr_before_sum += b;
            hr_after_sum += a;
            hr_count += 1;
        }
    }
    let hr_tokens_evidence = match (
        hr_before_sum.checked_div(hr_count),
        hr_after_sum.checked_div(hr_count),
    ) {
        (Some(before), Some(after)) => Some((before, after)),
        _ => None,
    };

    Some(SideAggregate {
        n: obs.len(),
        reps,
        tokens_before_mean: stats::mean(
            &obs.iter()
                .map(|m| m.tokens_before as f64)
                .collect::<Vec<f64>>(),
        ),
        compression_mean: stats::mean(&rates),
        compression_ci: stats::bootstrap_ci_mean(&rates),
        compression_mean_cl100k: stats::mean(&rates_cl100k),
        compression_ci_cl100k: stats::bootstrap_ci_mean(&rates_cl100k),
        retention_passed,
        retention_total,
        retention_ci: stats::wilson_interval(retention_passed, retention_total, 1.96),
        retention_missing,
        semantic_score,
        probed_samples,
        unprobed_samples,
        latency_ms: stats::latency_percentiles(&latencies_ms),
        latency_basis: series
            .measures
            .first()
            .map(|m| m.latency_basis.to_string())
            .unwrap_or_default(),
        hr_tokens_evidence,
    })
}

/// Paired compression-rate gap (tokenless − headroom, o200k) over aligned
/// `(sample, repetition)` pairs.
///
/// Pairs successful rows *first*, then collapses the paired diffs to one per
/// distinct payload pair. Deduplicating each side independently beforehand is
/// wrong: if one side fails a repetition the survivors' `rep` indices diverge,
/// their `(sample_id, rep)` intersection can be empty, and the whole category
/// gap silently vanishes even though both sides measured the sample at some
/// shared rep. Pairing first keeps every rep where *both* sides succeeded;
/// collapsing after keeps the pseudo-replication fix (a deterministic payload
/// pair still counts once). Pairs are emitted in sorted key order so the
/// seeded bootstrap CI is reproducible. Returns `None` with fewer than two
/// independent pairs.
fn compression_gap(tokenless: &SideSeries, headroom: &SideSeries) -> Option<GapStats> {
    // All tokenless rows keyed by (sample, rep) — no per-side dedup here.
    let mut tl_by_rep: HashMap<(&str, usize), &Measure> = HashMap::new();
    for m in &tokenless.measures {
        tl_by_rep.insert((m.sample_id.as_str(), m.rep), m);
    }
    // Pair on the rep both sides actually produced, then dedup the pairs by
    // (sample, tokenless payload, headroom payload): identical payload pairs
    // are pseudo-replication and count once, whichever rep they came from.
    let mut seen: HashSet<(&str, &str, &str)> = HashSet::new();
    let mut pairs: Vec<((&str, &str, &str), f64)> = Vec::new();
    for hr in &headroom.measures {
        let Some(tl) = tl_by_rep.get(&(hr.sample_id.as_str(), hr.rep)) else {
            continue;
        };
        let key = (
            hr.sample_id.as_str(),
            tl.payload_hash.as_str(),
            hr.payload_hash.as_str(),
        );
        if seen.insert(key) {
            pairs.push((key, tl.compression_rate - hr.compression_rate));
        }
    }
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let diffs: Vec<f64> = pairs.into_iter().map(|(_, d)| d).collect();
    if diffs.len() < 2 {
        return None;
    }
    Some(GapStats {
        mean: stats::mean(&diffs),
        ci: stats::bootstrap_ci_mean(&diffs),
        n_pairs: diffs.len(),
    })
}

fn side_lookup<F>(collected: &[CategoryData], pick: F) -> SideLookup
where
    F: Fn(&CategoryData) -> &SideSeries,
{
    let mut lookup: SideLookup = HashMap::new();
    for data in collected {
        let series = pick(data);
        // Per-sample means over all repetitions.
        let mut sums: HashMap<&str, (f64, f64, f64, usize)> = HashMap::new();
        for m in &series.measures {
            let entry = sums
                .entry(m.sample_id.as_str())
                .or_insert((0.0, 0.0, 0.0, 0));
            entry.0 += m.tokens_before as f64;
            entry.1 += m.tokens_after as f64;
            entry.2 += m.latency_s;
            entry.3 += 1;
        }
        for (sample_id, (before, after, lat, count)) in sums {
            let c = count as f64;
            lookup.insert(
                (data.category.name().to_string(), sample_id.to_string()),
                SampleSideStats {
                    tokens_before: before / c,
                    tokens_after: after / c,
                    latency_s: lat / c,
                },
            );
        }
    }
    lookup
}

fn replay_if_covered(
    task: &task_sim::TaskDef,
    lookup: &SideLookup,
) -> Option<task_sim::TaskSideReplay> {
    let replay = task_sim::replay(task, lookup);
    // A replay covering zero steps carries no information; report it as a
    // degraded (missing) side instead of a row of zeros.
    if replay.covered_steps == 0 {
        None
    } else {
        Some(replay)
    }
}

fn git_sha(repo_root: &Path) -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Whether the measured checkout has uncommitted changes to tracked files.
///
/// Tracked-only, matching `git describe --dirty`. Untracked files are reported
/// separately by [`untracked_build_inputs`] rather than folded in here: the two
/// states have different causes and different remedies, and merging them makes
/// the flag fire on unrelated strays.
///
/// `None` when git could not answer (not a checkout, git missing) — reported as
/// unknown rather than as clean, so a report never overstates its provenance.
fn git_dirty(repo_root: &Path) -> Option<bool> {
    let out = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// Count of untracked files sitting inside the paths that feed the measurement.
///
/// Scoped to the whole `src` tree, not just `src/tokenless`: the `rg_fn_main`
/// sample scans repository-wide `src/`, so an untracked `.rs` file in any
/// component changes the captured payload and its retention ground truth, and
/// cargo compiles whatever lands under `src/tokenless`. `remote_sync.sh` copies
/// untracked sources (it excludes only build dirs), so such a file changes what
/// was measured while leaving `git_sha` and the tracked-dirty flag untouched;
/// the count is reported on its own so a non-zero value flags that the commit
/// alone does not identify the measured code.
///
/// `None` when git could not answer.
fn untracked_build_inputs(repo_root: &Path) -> Option<usize> {
    let out = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "--", "src"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count(),
    )
}

/// Revision and tracked-dirty state of the rtk source tree a binary was built
/// from, or `(None, None)` when it cannot be resolved.
///
/// Resolves `<rtk>/target/release/rtk` back to `<rtk>` (three parents) and
/// queries git there. A `$RTK_BIN` pointing somewhere else, or a `PATH` rtk,
/// yields `None` rather than a wrong attribution. `safe.directory` is passed
/// per-invocation because the tree is rsynced in and owned by another uid.
fn rtk_provenance(rtk_bin: &Path) -> (Option<String>, Option<bool>) {
    let Some(src) = rtk_bin
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
    else {
        return (None, None);
    };
    if !src.join(".git").exists() {
        return (None, None);
    }
    let git = |args: &[&str]| -> Option<std::process::Output> {
        Command::new("git")
            .arg("-c")
            .arg(format!("safe.directory={}", src.display()))
            .args(args)
            .current_dir(src)
            .output()
            .ok()
            .filter(|o| o.status.success())
    };
    let revision =
        git(&["rev-parse", "HEAD"]).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
        .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty());
    (revision, dirty)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(sample_id: &str, rep: usize, rate: f64) -> Measure {
        measure_with_payload(sample_id, rep, rate, "same-payload")
    }

    fn measure_with_payload(sample_id: &str, rep: usize, rate: f64, payload: &str) -> Measure {
        Measure {
            sample_id: sample_id.to_string(),
            rep,
            payload_hash: payload_hash(payload),
            compression_rate: rate,
            compression_rate_cl100k: rate,
            latency_s: 0.001,
            latency_basis: "in-process",
            tokens_before: 100,
            tokens_after: 50,
            hr_tokens_before: None,
            hr_tokens_after: None,
            retention_passed: 8,
            retention_total: 11,
            retention_failures: Vec::new(),
        }
    }

    /// Two samples repeated five times each with identical payloads. Only the 2
    /// distinct payloads are independent observations — counting all 10 would
    /// shrink every interval and turn a real 8/11 retention into a fictitious
    /// 40/55.
    #[test]
    fn identical_payloads_collapse_to_one_observation() {
        let mut series = SideSeries::default();
        for rep in 0..5 {
            series.record(measure("a", rep, 0.4), "orig-a", "comp-a");
            series.record(measure("b", rep, 0.6), "orig-b", "comp-b");
        }
        assert_eq!(series.measures.len(), 10, "all rows are still collected");

        let obs = independent_measures(&series);
        assert_eq!(obs.len(), 2);
        let retention: usize = obs.iter().map(|m| m.retention_total).sum();
        assert_eq!(retention, 22, "11 checks per unique payload, not per rep");
        assert!(obs.iter().all(|m| m.rep == 0), "keeps the first repetition");
    }

    /// A nominally "dynamic" category whose command is deterministic in practice
    /// (`git log`, `git diff`: five reruns yield one unique output) must collapse
    /// too — independence comes from the payload, not from the category label.
    #[test]
    fn deterministic_dynamic_command_collapses() {
        let mut series = SideSeries::default();
        for rep in 0..5 {
            series.record(measure("git_log_oneline", rep, 0.1), "orig", "comp");
        }
        assert_eq!(independent_measures(&series).len(), 1);
    }

    /// A command that genuinely varies between repetitions keeps every distinct
    /// payload.
    #[test]
    fn varying_payloads_are_all_kept() {
        let mut series = SideSeries::default();
        for rep in 0..5 {
            let payload = format!("traversal-order-{rep}");
            series.record(
                measure_with_payload("rg_fn_main", rep, 0.1, &payload),
                "orig",
                "comp",
            );
        }
        assert_eq!(independent_measures(&series).len(), 5);
    }

    /// Only the non-JSON static category is input-asymmetric: tokenless needs a
    /// JSON envelope there while headroom sees raw text.
    #[test]
    fn only_code_is_input_asymmetric() {
        assert!(!Category::Code.has_symmetric_inputs());
        assert!(Category::Code.input_asymmetry_reason().is_some());
        for c in [
            Category::Json,
            Category::Command,
            Category::Grep,
            Category::Diff,
        ] {
            assert!(c.has_symmetric_inputs(), "{c} should be comparable");
            assert!(c.input_asymmetry_reason().is_none());
        }
    }

    /// An intermittent one-side failure must not wipe the whole category gap.
    /// tokenless produces both reps; headroom fails rep 0 and succeeds rep 1.
    /// Deduplicating each side first would keep tl rep 0 vs hr rep 1 and never
    /// pair them; pairing on the shared rep first keeps the sample.
    #[test]
    fn gap_survives_intermittent_one_side_failure() {
        let mut tl = SideSeries::default();
        let mut hr = SideSeries::default();
        // sample "a": headroom's rep 0 failed, only rep 1 landed.
        tl.record(measure_with_payload("a", 0, 0.5, "Pa"), "o", "c");
        tl.record(measure_with_payload("a", 1, 0.5, "Pa"), "o", "c");
        hr.record(measure_with_payload("a", 1, 0.2, "Qa"), "o", "c");
        // sample "b": both reps landed on both sides.
        tl.record(measure_with_payload("b", 0, 0.6, "Pb"), "o", "c");
        tl.record(measure_with_payload("b", 1, 0.6, "Pb"), "o", "c");
        hr.record(measure_with_payload("b", 0, 0.1, "Qb"), "o", "c");
        hr.record(measure_with_payload("b", 1, 0.1, "Qb"), "o", "c");

        let gap = compression_gap(&tl, &hr).expect("both samples must still pair");
        // One pair per sample after payload-pair dedup: a (rep 1) and b.
        assert_eq!(gap.n_pairs, 2);
    }

    /// Retention failures from independent observations must be deduplicated
    /// and surfaced in the aggregated SideAggregate as retention_missing.
    #[test]
    fn retention_failures_propagate_to_aggregate() {
        let raw: Vec<String> = vec![
            "substring not retained: \"req-8f3a91\"".to_string(),
            "regex not matched: \"req-[0-9a-f]{6}\"".to_string(),
            // Duplicate — same failure from a second observation.
            "substring not retained: \"req-8f3a91\"".to_string(),
        ];
        let missing = SideAggregate::collect_missing(raw);
        assert_eq!(missing.len(), 2, "duplicate must be collapsed");
        assert!(missing[0].contains("req-8f3a91"));
        assert!(missing[1].contains("req-[0-9a-f]"));
    }

    /// When failures exceed the cap, the list is truncated with an overflow
    /// suffix rather than silently dropping items.
    #[test]
    fn retention_missing_is_capped() {
        let raw: Vec<String> = (0..15)
            .map(|i| format!("substring not retained: \"item-{i}\""))
            .collect();
        let missing = SideAggregate::collect_missing(raw);
        assert_eq!(
            missing.len(),
            SideAggregate::RETENTION_MISSING_CAP + 1,
            "cap entries + overflow suffix"
        );
        assert!(missing.last().unwrap().starts_with("… and"));
    }
}
