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

//! tokenless discovery, shared by the L2 harness (`l2::rtk_side`) so it
//! reuses the same `$TOKENLESS_BIN` → Tokenless release build → `PATH` order
//! as the L1 suite, keeping discovery consistent across layers.

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

/// Locate the `tokenless` whose `rtk` subcommand runs RTK: `$TOKENLESS_BIN`
/// when set, else the release build of the enclosing Tokenless workspace, then
/// `tokenless` on `PATH`. Returns `None` when none is runnable.
///
/// Kept byte-identical, with [`rtk_command`] and `is_runnable`, to
/// `l1-compressor/src/metrics.rs::find_tokenless_binary` so the two suites
/// agree on where RTK comes from. The workspaces are independent, so this is a
/// manual sync obligation, not a shared function: change one side and you must
/// change the other.
pub fn find_tokenless_binary() -> Option<PathBuf> {
    // An explicit choice is final: falling back would measure another build.
    if let Some(program) = std::env::var_os("TOKENLESS_BIN").filter(|p| !p.is_empty()) {
        let program = PathBuf::from(program);
        return is_runnable(&program).then_some(program);
    }
    [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/release/tokenless"),
        PathBuf::from("tokenless"),
    ]
    .into_iter()
    .find(|program| is_runnable(program))
}

/// Starts `<tokenless> rtk`; callers append RTK's own arguments.
pub fn rtk_command(tokenless: &Path) -> Command {
    let mut command = Command::new(tokenless);
    command.arg("rtk");
    command
}

/// A path must be executable; a bare name must answer `rtk --version` from
/// `PATH`, which also rejects a `tokenless` older than `tokenless rtk`.
fn is_runnable(program: &Path) -> bool {
    if program.components().count() > 1 {
        return is_executable(program);
    }
    rtk_command(program)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}
