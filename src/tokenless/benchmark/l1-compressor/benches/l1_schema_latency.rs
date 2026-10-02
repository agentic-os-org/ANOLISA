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

//! Schema compression latency benchmarks.
//!
//! Mirrors the schema_latency measurement points: simple_3fields,
//! complex_branching_20fields_depth3, batch_uniform_simple/{10,50,100},
//! batch_diverse/{10,50,100}, long_description. Plus a `canonical` point that
//! compresses the shared `fixtures/schema_search.json` payload — the SAME bytes
//! the Python compression-rate scripts measure.
//!
//! Methodology: the compressor is constructed ONCE per benchmark (outside the
//! timed closure) and reused; only the `compress()` call is measured. Compressor
//! setup and teardown are deliberately excluded so the numbers are comparable
//! with headroom, which likewise does not count startup/teardown time.

use criterion::{Criterion, criterion_group, criterion_main};
use std::hint::black_box;
use tokenless_bench::{
    schema_batch, schema_batch_diverse, schema_canonical, schema_complex, schema_long_description,
    schema_with_params,
};
use tokenless_schema::SchemaCompressor;

fn bench_schema(c: &mut Criterion) {
    let mut group = c.benchmark_group("schema_latency");
    group.sample_size(100);

    let simple = schema_with_params(3);
    group.bench_function("simple_3fields", |b| {
        let compressor = SchemaCompressor::new();
        b.iter(|| compressor.compress(black_box(&simple)));
    });

    let complex = schema_complex(20, 3);
    group.bench_function("complex_branching_20fields_depth3", |b| {
        let compressor = SchemaCompressor::new();
        b.iter(|| compressor.compress(black_box(&complex)));
    });

    // Uniform batch: all schemas are simple 3-param tools.
    for n in [10usize, 50, 100] {
        let batch = schema_batch(n);
        group.bench_function(format!("batch_uniform_simple/{n}"), |b| {
            let compressor = SchemaCompressor::new();
            b.iter(|| {
                let arr = black_box(&batch).as_array().unwrap();
                let out: Vec<_> = arr.iter().map(|s| compressor.compress(s)).collect();
                black_box(out)
            });
        });
    }

    // Diverse batch: mixed complexity (4 simple + 3 medium + 2 long + 1 complex per 10).
    for n in [10usize, 50, 100] {
        let batch = schema_batch_diverse(n);
        group.bench_function(format!("batch_diverse/{n}"), |b| {
            let compressor = SchemaCompressor::new();
            b.iter(|| {
                let arr = black_box(&batch).as_array().unwrap();
                let out: Vec<_> = arr.iter().map(|s| compressor.compress(s)).collect();
                black_box(out)
            });
        });
    }

    let long = schema_long_description();
    group.bench_function("long_description", |b| {
        let compressor = SchemaCompressor::new();
        b.iter(|| compressor.compress(black_box(&long)));
    });

    // Shared canonical schema — identical bytes to what Python compresses.
    let canonical = schema_canonical();
    group.bench_function("canonical", |b| {
        let compressor = SchemaCompressor::new();
        b.iter(|| compressor.compress(black_box(&canonical)));
    });

    group.finish();
}

criterion_group!(benches, bench_schema);
criterion_main!(benches);
