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

//! RTK binary & rewrite exit-code/output protocol tests.
//!
//! tokenless does not link rtk; adapters shell out to `rtk rewrite <cmd>` and
//! rely on its exit-code protocol:
//!   0 = rewrite available (Allow)   1 = no equivalent (passthrough)
//!   2 = deny rule matched           3 = Ask/Default (rewrite available)
//!
//! These 9 tests validate that contract against the real rtk binary. They do
//! NOT assert anything about compressor output format — only that rtk’s own
//! invocation interface (exit codes + stdout) behaves as documented. When rtk
//! is not built/available they SKIP (pass) rather than fail, so the suite runs
//! on machines without the vendored third-party binary.

use std::process::Command;

use tokenless_bench::metrics::find_rtk_binary as find_rtk;

/// Documented rtk-rewrite exit codes.
const PROTOCOL_CODES: [i32; 4] = [0, 1, 2, 3];

/// Run `rtk rewrite <cmd>`, returning (exit_code, stdout). `None` if rtk is
/// unavailable (caller should skip).
fn rtk_rewrite(cmd: &str) -> Option<(i32, String)> {
    let bin = find_rtk()?;
    let out = Command::new(&bin).arg("rewrite").arg(cmd).output().ok()?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some((code, stdout))
}

/// Emit a skip notice and return true when rtk is missing.
fn skip_if_no_rtk(test: &str) -> bool {
    if find_rtk().is_none() {
        eprintln!(
            "[SKIPPED] {test}: RTK binary not found \
             (set RTK_BIN or install rtk >= 0.35.0)"
        );
        return true;
    }
    false
}

#[test]
fn rtk_binary_is_runnable() {
    if skip_if_no_rtk("rtk_binary_is_runnable") {
        return;
    }
    let bin = find_rtk().unwrap();
    let out = Command::new(&bin).arg("--version").output().unwrap();
    assert!(out.status.success(), "rtk --version should succeed");
    let v = String::from_utf8_lossy(&out.stdout);
    assert!(v.to_lowercase().contains("rtk") || v.chars().any(|c| c.is_ascii_digit()));
}

#[test]
fn rtk_version_meets_minimum() {
    if skip_if_no_rtk("rtk_version_meets_minimum") {
        return;
    }
    let bin = find_rtk().unwrap();
    let out = Command::new(&bin).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    // Extract the first dotted version triple and require >= 0.35.0.
    let ver: Vec<u32> = text
        .split_whitespace()
        .find(|t| t.contains('.') && t.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .map(|t| {
            t.split('.')
                .take(3)
                .filter_map(|n| n.trim_matches(|c: char| !c.is_ascii_digit()).parse().ok())
                .collect()
        })
        .unwrap_or_default();
    if ver.len() >= 2 {
        let ok = ver[0] > 0 || (ver[0] == 0 && ver[1] >= 35);
        assert!(ok, "rtk version {ver:?} must be >= 0.35.0");
    }
}

#[test]
fn rewrite_common_command_uses_protocol_code() {
    if skip_if_no_rtk("rewrite_common_command_uses_protocol_code") {
        return;
    }
    let (code, _) = rtk_rewrite("cat README.md").unwrap();
    assert!(
        PROTOCOL_CODES.contains(&code),
        "unexpected exit code {code}"
    );
}

#[test]
fn rewrite_available_yields_nonempty_stdout() {
    if skip_if_no_rtk("rewrite_available_yields_nonempty_stdout") {
        return;
    }
    let (code, stdout) = rtk_rewrite("grep -rn TODO src/").unwrap();
    assert!(PROTOCOL_CODES.contains(&code));
    // Codes 0 and 3 mean "rewrite available" — stdout must carry the rewrite.
    if code == 0 || code == 3 {
        assert!(!stdout.is_empty(), "rewrite-available must emit a command");
    }
}

#[test]
fn rewrite_is_deterministic() {
    if skip_if_no_rtk("rewrite_is_deterministic") {
        return;
    }
    let a = rtk_rewrite("ls -la /tmp").unwrap();
    let b = rtk_rewrite("ls -la /tmp").unwrap();
    assert_eq!(a, b, "same input must produce same code+output");
}

#[test]
fn rewrite_unknown_command_passthrough() {
    if skip_if_no_rtk("rewrite_unknown_command_passthrough") {
        return;
    }
    // A command with no RTK equivalent should not crash; protocol code only.
    let (code, _) = rtk_rewrite("some_totally_unknown_binary_xyz --flag").unwrap();
    assert!(PROTOCOL_CODES.contains(&code));
}

#[test]
fn rewrite_handles_pipeline() {
    if skip_if_no_rtk("rewrite_handles_pipeline") {
        return;
    }
    let (code, _) = rtk_rewrite("cat access.log | grep ERROR | wc -l").unwrap();
    assert!(PROTOCOL_CODES.contains(&code));
}

#[test]
fn rewrite_does_not_mangle_when_passthrough() {
    if skip_if_no_rtk("rewrite_does_not_mangle_when_passthrough") {
        return;
    }
    let input = "echo hello";
    let (code, stdout) = rtk_rewrite(input).unwrap();
    assert!(PROTOCOL_CODES.contains(&code));
    // Passthrough (code 1) means "no equivalent" — output, if any, must not
    // silently corrupt into an unrelated command.
    if code == 1 && !stdout.is_empty() {
        assert!(stdout.contains("echo") || stdout == input);
    }
}

#[test]
fn rewrite_empty_command_is_safe() {
    if skip_if_no_rtk("rewrite_empty_command_is_safe") {
        return;
    }
    // An empty command must not crash rtk: all we require is that the process
    // runs to completion with some status. Exit-code values are deliberately
    // unconstrained — an earlier revision accepted "any protocol code or any
    // other value", which was a near-tautological predicate.
    let bin = find_rtk().unwrap();
    let out = Command::new(&bin).arg("rewrite").arg("").output();
    assert!(
        out.is_ok(),
        "rtk must handle an empty command without failing to run"
    );
}
