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

//! rtk binary discovery, shared by the L2 harness (`l2::rtk_side`) so it
//! reuses the same `$RTK_BIN` → vendored release build → `PATH` order as the
//! L1 suite, keeping discovery consistent across layers.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Checks that a path points to a regular file with at least one execute bit set.
fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// Locate the rtk binary: `$RTK_BIN`, the vendored release build next to this
/// crate, or `rtk` on `PATH`. Returns `None` when none is runnable.
///
/// Kept byte-identical to `l1-compressor/src/metrics.rs::find_rtk_binary` so
/// the two suites agree on where `rtk` comes from. The workspaces are
/// independent, so this is a manual sync obligation, not a shared function:
/// change one side and you must change the other.
pub fn find_rtk_binary() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RTK_BIN") {
        let pb = PathBuf::from(p);
        if is_executable(&pb) {
            return Some(pb);
        }
    }
    let vendored =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../third_party/rtk/target/release/rtk");
    if is_executable(&vendored) {
        return Some(vendored);
    }
    // Fall back to PATH: only accept it if `--version` actually runs.
    if Command::new("rtk").arg("--version").output().is_ok() {
        return Some(PathBuf::from("rtk"));
    }
    None
}
