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

//! RTK side: paired execution of one command spec — raw argv first, then the
//! same argv wrapped by the rtk binary — in the same working directory and
//! the same round.
//!
//! rtk is an output-filtering command wrapper, not a payload compressor, so
//! it cannot compress pre-captured text; both runs must happen live. Running
//! them back-to-back keeps the repository state identical for the pair, and
//! the raw output doubles as the headroom-side input and the source for
//! dynamic ground-truth extraction.
//!
//! Latency basis: **wrapped-minus-raw wall clock** — the only observable
//! cost signal for a cross-process wrapper. It includes rtk process
//! startup, so it is NOT comparable with the in-process bases; the report
//! labels it accordingly.

use crate::l2::L2Error;
use crate::metrics::find_rtk_binary;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Latency-basis label stamped on every rtk-side result row.
pub const LATENCY_BASIS: &str = "wrapped-minus-raw wall clock";

/// Outcome of one paired (raw, rtk-wrapped) execution.
#[derive(Debug, Clone)]
pub struct PairedRun {
    /// stdout+stderr of the raw command — headroom input and ground-truth
    /// source.
    pub raw_text: String,
    /// stdout+stderr of the rtk-wrapped command — the "compressed" view.
    pub rtk_text: String,
    /// Raw command wall-clock seconds.
    pub raw_wall_s: f64,
    /// Wrapped command wall-clock seconds.
    pub rtk_wall_s: f64,
}

impl PairedRun {
    /// rtk overhead estimate: wrapped minus raw wall clock, floored at zero
    /// because scheduler jitter can make the wrapped run appear faster.
    pub fn rtk_overhead_s(&self) -> f64 {
        (self.rtk_wall_s - self.raw_wall_s).max(0.0)
    }
}

/// Locates the rtk binary, reusing the L0/L1 discovery order
/// (`$RTK_BIN` → vendored release build → `PATH`).
///
/// # Errors
///
/// Returns [`L2Error::RtkUnavailable`] when nothing runnable is found —
/// callers degrade the command/grep/diff categories rather than abort.
pub fn locate_rtk() -> Result<PathBuf, L2Error> {
    find_rtk_binary().ok_or_else(|| {
        L2Error::RtkUnavailable("no rtk binary via $RTK_BIN, vendored build, or PATH".to_string())
    })
}

/// Runs `argv` raw and then `rtk_argv` wrapped by `rtk_bin`, both under `cwd`.
///
/// `rtk_argv` is normally the same argv; it differs where rtk's filter is
/// selected by a subcommand name that is not an executable program, so the
/// unfiltered baseline needs a different invocation for the same content.
///
/// Output is captured as stdout followed by stderr: agents see both streams,
/// so both count as payload. A non-zero raw exit status is an error (the
/// spec'd commands are expected to succeed deterministically); a non-zero
/// wrapped status is tolerated because rtk uses exit codes as protocol.
///
/// # Errors
///
/// [`L2Error::Command`] when either process cannot be spawned or the raw
/// command fails.
pub fn run_paired(
    rtk_bin: &Path,
    argv: &[String],
    rtk_argv: &[String],
    cwd: &Path,
) -> Result<PairedRun, L2Error> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| L2Error::Command("command spec has an empty argv".to_string()))?;
    // An empty rtk_argv would invoke rtk with no subcommand, which succeeds and
    // prints usage: the run would record that as a compression measurement.
    // Fail instead of measuring the help text.
    if rtk_argv.is_empty() {
        return Err(L2Error::Command(format!(
            "command spec {argv:?} has an empty rtk_argv: rtk would run with no \
             subcommand and its usage output would be measured as compression"
        )));
    }

    let start = Instant::now();
    let raw = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| L2Error::Command(format!("spawn {program:?} failed: {e}")))?;
    let raw_wall_s = start.elapsed().as_secs_f64();
    if !raw.status.success() {
        return Err(L2Error::Command(format!(
            "raw command {argv:?} exited with {}: {}",
            raw.status,
            String::from_utf8_lossy(&raw.stderr).trim()
        )));
    }

    let start = Instant::now();
    let wrapped = Command::new(rtk_bin)
        .args(rtk_argv)
        .current_dir(cwd)
        .output()
        .map_err(|e| L2Error::Command(format!("spawn rtk {:?} failed: {e}", rtk_bin.display())))?;
    let rtk_wall_s = start.elapsed().as_secs_f64();

    Ok(PairedRun {
        raw_text: merge_streams(&raw.stdout, &raw.stderr),
        rtk_text: merge_streams(&wrapped.stdout, &wrapped.stderr),
        raw_wall_s,
        rtk_wall_s,
    })
}

/// Merges a captured `stdout`/`stderr` pair into the single payload text an
/// agent would see.
///
/// Inserts a newline when `stdout` does not already end with one, so the last
/// stdout line and the first stderr line never fuse into one line: the
/// ground-truth regexes in [`samples`](crate::l2::samples) are line-anchored,
/// and a fused line silently drops a match. Empty `stderr` is appended as
/// nothing at all, keeping the text byte-identical to stdout alone.
///
/// Shared with the raw (rtk-unavailable) path in `l2_compare` so both
/// capture paths produce identical text for identical process output.
pub fn merge_streams(stdout: &[u8], stderr: &[u8]) -> String {
    let mut text = String::from_utf8_lossy(stdout).into_owned();
    if !stderr.is_empty() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(stderr));
    }
    text
}
