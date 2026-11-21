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

//! Export private wire inputs and candidates for same-input Headroom comparisons.
//!
//! Synthetic cases are the default; --shared selects the requested L2 mode and
//! records its production outcomes. Complete tool and stash text must remain private.

use serde_json::json;
use std::path::PathBuf;
use tokenless_bench::component_cases::{cases, measure};
use tokenless_bench::shared_set;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut shared: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut l2_mode = "default".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shared" => shared = Some(args.next().ok_or("--shared requires a path")?.into()),
            "--output" => output = Some(args.next().ok_or("--output requires a path")?.into()),
            "--help" => {
                println!(
                    "component_cases --output result.json [--shared measurements.json] [--l2-mode default|no-stash|diff-opt-in]\nThe output contains complete tool/stash text. Keep it private; never paste it into public issues or logs. Rust regex syntax only (no lookaround or backreferences)."
                );
                return Ok(());
            }
            "--l2-mode" => l2_mode = args.next().ok_or("--l2-mode requires a value")?,
            other => {
                return Err(format!(
                    "unknown argument {other:?} (accepts --shared, --l2-mode, --output, --help)"
                )
                .into());
            }
        }
    }
    let output = output.ok_or(
        "--output is required: complete tool/stash text must not be printed to shared stdout",
    )?;
    if !matches!(l2_mode.as_str(), "default" | "no-stash" | "diff-opt-in") {
        return Err("unsupported --l2-mode".into());
    }
    if shared.as_ref().is_some_and(|input| {
        input == &output
            || input
                .canonicalize()
                .ok()
                .zip(output.canonicalize().ok())
                .is_some_and(|(input, output)| input == output)
    }) {
        return Err("output must not overwrite the input measurement file".into());
    }
    let (set, rows) = match &shared {
        Some(path) => (
            "shared-real",
            shared_set::measure_all(&shared_set::load(path, &l2_mode)?)?,
        ),
        None => (
            "synthetic",
            cases().iter().map(measure).collect::<Result<Vec<_>, _>>()?,
        ),
    };
    let count = rows.len();
    let document = serde_json::to_string_pretty(&json!({
        "schema_version": 2,
        "set": set,
        "l2_mode": shared.as_ref().map(|_| l2_mode.as_str()),
        "headline_tokenizer": "o200k_base",
        "runtime_acceptance": if shared.is_some() { "paired from L2 measurements" } else { "not measured" },
        "rows": rows,
    }))?;
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, document + "\n")?;
    println!(
        "wrote {count} component rows; complete text is retained only in the private output file"
    );
    Ok(())
}
