//! Tokenless schema compression benchmark — throughput and latency metrics.
//!
//! Run: `cargo run --example bench --release -p tokenless-schema`

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokenless_ccr::{InMemoryStore, StashStore};
use tokenless_schema::SchemaCompressor;

fn load_fixture(dir: &str, name: &str) -> Value {
    let manifest = env!("CARGO_MANIFEST_DIR");
    let path = format!("{manifest}/tests/fixtures/{dir}/{name}");
    let content = std::fs::read_to_string(&path).expect("load fixture");
    serde_json::from_str(&content).expect("parse fixture")
}

const ITERATIONS: u32 = 5_000;

struct BenchResult {
    name: String,
    iterations: u32,
    total: Duration,
    orig_bytes: usize,
    comp_bytes: usize,
    stash_count: usize,
}

impl BenchResult {
    fn throughput_mbps(&self) -> f64 {
        let total_bytes = self.orig_bytes as f64 * self.iterations as f64;
        total_bytes / self.total.as_secs_f64() / 1_000_000.0
    }

    fn avg_latency_us(&self) -> f64 {
        self.total.as_micros() as f64 / self.iterations as f64
    }

    fn compression_ratio(&self) -> f64 {
        (1.0 - self.comp_bytes as f64 / self.orig_bytes as f64) * 100.0
    }
}

fn run_schema_bench(name: &str, fixture: &Value, stash: bool) -> BenchResult {
    let store = Arc::new(InMemoryStore::new());
    let compressor = if stash {
        SchemaCompressor::new().with_stash_store(store.clone() as Arc<dyn StashStore>)
    } else {
        SchemaCompressor::new()
    };

    let orig_bytes = serde_json::to_string(fixture).unwrap().len();

    let start = Instant::now();
    let mut comp_bytes = 0usize;
    for _ in 0..ITERATIONS {
        let compressed = compressor.compress(fixture);
        comp_bytes = serde_json::to_string(&compressed).unwrap().len();
    }
    let total = start.elapsed();

    BenchResult {
        name: name.to_string(),
        iterations: ITERATIONS,
        total,
        orig_bytes,
        comp_bytes,
        stash_count: store.len(),
    }
}

fn print_result(r: &BenchResult) {
    println!(
        "  {:<40} {:>7.1} MB/s  {:>7.1} µs/op  {:>5.1}% saved  stash={}",
        r.name,
        r.throughput_mbps(),
        r.avg_latency_us(),
        r.compression_ratio(),
        r.stash_count,
    );
}

fn main() {
    let schema_fixtures: Vec<(&str, Value)> = [
        "simple_calculator.json",
        "hubspot_contact.json",
        "stripe_payment.json",
        "github_create_issue.json",
        "slack_send_message.json",
        "aws_describe_instances.json",
    ]
    .iter()
    .map(|name| (*name, load_fixture("schemas", name)))
    .collect();

    println!("Tokenless Benchmark ({ITERATIONS} iterations per test)\n");

    println!("L1 — SchemaCompressor (no stash)");
    for (name, fixture) in &schema_fixtures {
        print_result(&run_schema_bench(&format!("schema/{name}"), fixture, false));
    }

    println!("\nL1 — SchemaCompressor + Stash (reversible)");
    for (name, fixture) in &schema_fixtures {
        print_result(&run_schema_bench(
            &format!("schema+stash/{name}"),
            fixture,
            true,
        ));
    }
}
