<!-- Copyright 2026 Alibaba Cloud

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

    http://www.apache.org/licenses/LICENSE-2.0

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License. -->

# Tokenless Benchmark Suite

One subdirectory per layer of the four-layer tokenless benchmark plan:

| Directory | Layer | What it measures |
|---|---|---|
| [`l1-compressor/`](l1-compressor) | **L1** — component | Single compressors (schema, response, TOON, RTK rewrite) in isolation; criterion latency benches, quality/adversarial tests, in-process compression-rate report. |
| [`l2-module/`](l2-module) | **L2** — module | Paired tokenless-vs-headroom comparison on identical one-round tool outputs; real-tokenizer (tiktoken-rs) deltas, retention, semantic probing, remote-run scripts. |
| [`l3-scenario/`](l3-scenario) | **L3** — scenario | Both sides on a whole conversation rather than one response; total message-list tokens before → after, critical-item retention, semantic probing. |
| [`l4-end-to-end/`](l4-end-to-end) | **L4** — end-to-end | A complete agent task with compression active throughout: task outcome, whole-task token cost, added wall-clock. Results in the L4 section of [`docs/BENCHMARKS.md`](../../../docs/BENCHMARKS.md); the run trees are machine-specific and gitignored. |

L1–L3 are standalone Cargo workspaces (each with its own `Cargo.toml` carrying an
empty `[workspace]` table) kept out of the main tokenless workspace on purpose.
**L4 is not a Cargo workspace**: it drives an agent against real repositories, so
its harness is the Python runner in [`src/benchmark/swe-runner`](../../benchmark/swe-runner)
plus docker, and it borrows only the naming and reporting conventions. See the
per-layer `README.md` files for build/run instructions and methodology.

Each layer keeps its results in its own `reports/` directory so the layers'
numbers never mix. Every one of those directories is gitignored: benchmark
reports are run/machine-specific artifacts and are never committed — regenerate
them locally or attach them to the PR as CI artifacts. The suite-wide
[`.gitignore`](.gitignore) is the backstop, so a new layer cannot leak reports
before someone remembers to add its own rules.
