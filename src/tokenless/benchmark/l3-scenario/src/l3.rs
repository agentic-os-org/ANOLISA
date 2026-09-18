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

//! Building blocks of the L3 scenario comparison.

pub mod asset;
pub mod headroom_side;
pub mod probe;
pub mod report;
pub mod retention;
pub mod stats;
pub mod tokenizer;
pub mod tokenless_side;

use thiserror::Error;

/// Failure modes of the L3 harness.
#[derive(Debug, Error)]
pub enum L3Error {
    /// A scenario asset could not be read from disk.
    #[error("failed to read scenario asset {path}: {source}")]
    AssetRead {
        /// Path that failed to load.
        path: String,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// A scenario asset was not valid JSON, or did not match the expected shape.
    #[error("failed to parse scenario asset {path}: {source}")]
    AssetParse {
        /// Path that failed to parse.
        path: String,
        /// Underlying deserialization failure.
        #[source]
        source: serde_json::Error,
    },

    /// The asset directory held no scenarios, which almost always means the
    /// harness was pointed at the wrong path rather than that the suite is
    /// legitimately empty.
    #[error("no scenario assets found under {0}")]
    NoAssets(String),

    /// A tiktoken encoding could not be constructed.
    #[error("failed to load tokenizer {name}: {message}")]
    Tokenizer {
        /// Encoding that failed to load.
        name: String,
        /// Reason reported by tiktoken-rs.
        message: String,
    },
}

/// Which of the reference's two benchmark suites a scenario came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Suite {
    /// `bench_transforms.py::TestPipelinePerformance` — whole-conversation
    /// shapes, each with a latency target the reference sets for itself.
    Pipeline,
    /// `bench_latency.py::generate_scenarios()` — one content type per point,
    /// across size steps.
    Scenario,
}

impl Suite {
    /// Directory name holding this suite's assets.
    pub fn dir(self) -> &'static str {
        match self {
            Suite::Pipeline => "pipeline",
            Suite::Scenario => "scenario",
        }
    }
}
