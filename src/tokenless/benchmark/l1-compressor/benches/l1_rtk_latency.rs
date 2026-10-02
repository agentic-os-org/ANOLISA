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

//! RTK command-rewrite latency benchmarks.
//!
//! METHODOLOGY NOTE — not comparable with the in-process compressor benches:
//! rtk is an external binary, so every measurement here is a full subprocess
//! round-trip (`rtk rewrite <cmd>`) INCLUDING process spawn/teardown overhead,
//! which puts these numbers in the millisecond range. The response/schema/TOON
//! benches call library code in-process and land in the microsecond range.
//! The two latency classes must not be added together or compared directly.
//!
//! When the rtk binary is unavailable the bench prints a skip notice and
//! returns without registering measurement points — never panics — so
//! `cargo bench` stays green on machines without rtk.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;
use tokenless_bench::metrics::{TimedRun, find_rtk_binary, run_rewrite_with_timeout};
use tokenless_bench::rtk_command_samples;

fn bench_rtk(c: &mut Criterion) {
    let Some(bin) = find_rtk_binary() else {
        eprintln!("[skip] rtk_latency: rtk binary not available (set RTK_BIN or install rtk)");
        return;
    };

    let mut group = c.benchmark_group("rtk_latency");
    // Every iteration runs through `run_rewrite_with_timeout` (5s hard
    // deadline + SIGKILL), so a hung rtk aborts the bench with a clear panic
    // instead of blocking `cargo bench` indefinitely.
    group.measurement_time(Duration::from_secs(30));
    // RTK benchmarks use sample_size(20) instead of the standard 100 because each
    // sample spawns a subprocess (fork+exec+wait). With 7 benchmarks × 20 samples,
    // the group already takes ~30s. Increasing to 100 would push to 2+ minutes with
    // diminishing statistical benefit for ms-range measurements. For report-grade
    // precision, run `cargo bench rtk_latency` 3 times and average medians.
    // Subprocess spawns are slow (ms-range); keep sample counts moderate so
    // the whole group finishes in reasonable time.
    group.sample_size(20);

    for (label, cmd) in rtk_command_samples() {
        group.bench_function(format!("rewrite/{label}"), |b| {
            b.iter(|| {
                // Full subprocess round-trip, spawn included. Any exit code is
                // valid here (0/1/2/3 protocol) — we measure latency only. A
                // timeout or spawn failure invalidates the measurement, so it
                // panics rather than being silently recorded as a data point.
                match run_rewrite_with_timeout(&bin, black_box(cmd), Duration::from_secs(5)) {
                    TimedRun::Completed(output) => output,
                    _ => panic!("rtk rewrite {label:?} timed out or failed to run"),
                }
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_rtk);
criterion_main!(benches);
