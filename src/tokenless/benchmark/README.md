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

The benchmark suite is split into two **independent Cargo workspaces**, one per
layer of the four-layer tokenless benchmark plan:

| Workspace | Layer | What it measures |
|---|---|---|
| [`l1-compressor/`](l1-compressor) | **L1** — component | Single compressors (schema, response, TOON, RTK rewrite) in isolation; criterion latency benches, quality/adversarial tests, in-process compression-rate report. |
| [`l2-module/`](l2-module) | **L2** — module | Paired tokenless-vs-headroom comparison on identical one-round tool outputs; real-tokenizer (tiktoken-rs) deltas, retention, semantic probing, remote-run scripts. |

Each subdirectory is a standalone workspace (its own `Cargo.toml` with an empty
`[workspace]` table) kept out of the main tokenless workspace on purpose — see
the per-workspace `README.md` files for build/run instructions and methodology.

Each layer also keeps its results in its own `reports/` directory
(`l1-compressor/reports/`, `l2-module/reports/`) so the two layers' numbers
never mix. Both directories are gitignored: benchmark reports are
run/machine-specific artifacts and are never committed — regenerate them
locally or attach them to the PR as CI artifacts.
