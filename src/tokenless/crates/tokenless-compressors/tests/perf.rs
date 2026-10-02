//! Build-log measurement harness. Not part of the normal test run:
//! `cargo test -p tokenless-compressors --release --test perf -- --ignored --nocapture`

use std::time::Instant;

use tokenless_ccr::InMemoryStore;
use tokenless_compressors::BuildLogCompressor;

/// Synthetic ~5 MiB build log: dense noise with periodic errors, ANSI
/// coloring, and CR progress redraws.
fn synthetic_log() -> String {
    let mut out = String::with_capacity(5 * 1024 * 1024 + 4096);
    out.push_str("$ cargo build --release\n");
    let mut i = 0usize;
    while out.len() < 5 * 1024 * 1024 {
        if i % 500 == 499 {
            out.push_str(&format!(
                "\u{1b}[1m\u{1b}[33mwarning\u{1b}[0m: unused variable `v{i}` in src/gen_{i}.rs\n"
            ));
        } else if i % 97 == 96 {
            out.push_str(&format!("step {i}\rstep {i} done\n"));
        } else {
            out.push_str(&format!(
                "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m synth-crate-{i} v0.{}.{}\n",
                i % 90,
                i % 10
            ));
        }
        i += 1;
    }
    out
}

#[test]
#[ignore = "perf harness: run explicitly in release with --nocapture"]
fn measure_percentiles_on_a_5mib_log() {
    let log = synthetic_log();
    let iterations = 30;
    let mut samples_ms: Vec<f64> = Vec::with_capacity(iterations);
    let mut last_len = 0;
    for _ in 0..iterations {
        let store = InMemoryStore::new();
        let start = Instant::now();
        let outcome = BuildLogCompressor.compress(&log, Some(&store));
        samples_ms.push(start.elapsed().as_secs_f64() * 1e3);
        last_len = outcome.output.len();
    }
    samples_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let pct = |p: f64| samples_ms[((samples_ms.len() as f64 - 1.0) * p).round() as usize];
    println!(
        "build-log 5MiB ({} bytes in, {} bytes out): p50={:.1}ms p95={:.1}ms p99={:.1}ms",
        log.len(),
        last_len,
        pct(0.50),
        pct(0.95),
        pct(0.99),
    );
}
