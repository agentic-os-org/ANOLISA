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

//! L2 module-level comparison benchmark harness: tokenless vs headroom on
//! identical one-round tool outputs.
//!
//! This crate is the **L2 — module layer** of the four-layer tokenless
//! benchmark plan. Unlike L1 (which measures tokenless in isolation with a
//! bytes/4 heuristic), L2 runs PAIRED comparisons — both compressors see the
//! same bytes in the same round — and counts tokens with a real tokenizer
//! (tiktoken-rs, `o200k_base` headline / `cl100k_base` side report), so the
//! resulting deltas are attributable to compressor behaviour rather than
//! sampling or counting differences.
//!
//! The harness lives in module [`l2`] (see `src/l2.rs`); the `l2_compare`
//! binary orchestrates a full run and the `l2_`-prefixed tests under `tests/`
//! guard the protocol, samples, stats and retention logic. Static samples,
//! probes, remote-run scripts and the headroom worker ship under `assets/`.
//!
//! [`rtk_side`](l2::rtk_side) reuses this crate's [`metrics::find_rtk_binary`]
//! for rtk discovery. That function is a deliberate copy of the L1 suite's
//! (`l1-compressor/src/metrics.rs`), not a shared dependency: the two
//! benchmark workspaces are independent by design, so discovery order must be
//! kept in sync by hand whenever either side changes.

pub mod l2;
pub mod metrics;
