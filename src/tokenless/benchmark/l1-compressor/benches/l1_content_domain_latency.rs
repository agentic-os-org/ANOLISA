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

//! Scaled component-call latency, including wire parsing and serialization.
//! Per-call construction/stash costs are included; corpus construction and tokenization are not.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use std::hint::black_box;
use std::time::Duration;
use tokenless_bench::component_cases::{cases, compress};

fn benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("content_domain_component");
    group.sample_size(100);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(3));
    for case in cases() {
        group.throughput(Throughput::Bytes(case.input.len() as u64));
        compress(&case).expect("component preflight succeeds");
        group.bench_function(&case.id, |b| {
            b.iter(|| compress(black_box(&case)).expect("component invocation succeeds"))
        });
    }
    group.finish();
}

criterion_group!(benches, benchmark);
criterion_main!(benches);
