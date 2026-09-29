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

//! Subprocess-protocol checks with fake workers: the headroom line protocol
//! is exercised against tiny `/bin/sh` scripts and the rtk paired-run flow
//! against plain shell commands — no Python, headroom, rtk, or network.

use std::fs;
use std::path::{Path, PathBuf};
use tokenless_l2_bench::l2::L2Error;
use tokenless_l2_bench::l2::headroom_side::HeadroomWorker;
use tokenless_l2_bench::l2::rtk_side::{merge_streams, run_paired};

/// Writes a fake worker script into a per-test temp dir; `/bin/sh` plays the
/// role of `$HEADROOM_PYTHON`, so no exec bit or interpreter is needed.
fn write_script(name: &str, body: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("l2_protocol_{}_{name}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join(format!("{name}.sh"));
    fs::write(&path, body).expect("write fake worker");
    path
}

const GOOD_WORKER: &str = r#"
echo '{"ready":true}'
n=0
while IFS= read -r line; do
  n=$((n+1))
  printf '{"id":"s%s","compressed":"squeezed","strategy_used":"fake","wall_time_s":0.0015,"hr_tokens_before":100,"hr_tokens_after":40}\n' "$n"
done
"#;

#[test]
fn handshake_and_round_trips_succeed() {
    let script = write_script("good", GOOD_WORKER);
    let mut worker = HeadroomWorker::spawn("/bin/sh", &script).expect("handshake");

    // Two consecutive requests: internal ids s1/s2 must line up with the
    // fake worker's own counter, proving request/response stay paired.
    for _ in 0..2 {
        let resp = worker.compress("payload text", "").expect("round trip");
        assert_eq!(resp.compressed.as_deref(), Some("squeezed"));
        assert_eq!(resp.strategy_used.as_deref(), Some("fake"));
        assert_eq!(resp.hr_tokens_before, Some(100));
        assert_eq!(resp.hr_tokens_after, Some(40));
        assert!(resp.wall_time_s.unwrap_or(-1.0) > 0.0);
    }
    // Drop closes stdin (EOF) and reaps the child without hanging.
}

#[test]
fn failed_handshake_degrades_to_headroom_unavailable() {
    let script = write_script(
        "notready",
        "echo '{\"ready\":false,\"error\":\"import headroom failed\"}'\nexit 1\n",
    );
    let err = HeadroomWorker::spawn("/bin/sh", &script).expect_err("must not come up");
    match err {
        L2Error::HeadroomUnavailable(msg) => assert!(msg.contains("import headroom failed")),
        other => panic!("expected HeadroomUnavailable, got {other:?}"),
    }
}

#[test]
fn eof_before_handshake_degrades_to_headroom_unavailable() {
    let script = write_script("silent", "exit 0\n");
    let err = HeadroomWorker::spawn("/bin/sh", &script).expect_err("must not come up");
    assert!(
        matches!(err, L2Error::HeadroomUnavailable(_)),
        "got {err:?}"
    );
}

#[test]
fn spawn_failure_degrades_to_headroom_unavailable() {
    let err = HeadroomWorker::spawn("/nonexistent/python", Path::new("worker.py"))
        .expect_err("must not spawn");
    assert!(
        matches!(err, L2Error::HeadroomUnavailable(_)),
        "got {err:?}"
    );
}

#[test]
fn mismatched_response_id_is_a_protocol_error() {
    let script = write_script(
        "mismatch",
        "echo '{\"ready\":true}'\nwhile IFS= read -r line; do\n  echo '{\"id\":\"s999\",\"compressed\":\"x\"}'\ndone\n",
    );
    let mut worker = HeadroomWorker::spawn("/bin/sh", &script).expect("handshake");
    let err = worker
        .compress("payload", "")
        .expect_err("id mismatch must surface");
    assert!(matches!(err, L2Error::Protocol(_)), "got {err:?}");
}

#[test]
fn rtk_paired_run_captures_both_sides() {
    // `/bin/echo` stands in for rtk: the wrapped run prepends the argv, so
    // the two outputs differ and both wall clocks are observable.
    let cwd = std::env::temp_dir();
    let argv: Vec<String> = ["/bin/echo", "hello", "world"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let run = run_paired(Path::new("/bin/echo"), &argv, &argv, &cwd).expect("paired run");
    assert!(
        run.raw_text.contains("hello world"),
        "raw: {:?}",
        run.raw_text
    );
    assert!(
        run.rtk_text.contains("hello world"),
        "wrapped: {:?}",
        run.rtk_text
    );
    assert!(run.raw_wall_s >= 0.0 && run.rtk_wall_s >= 0.0);
    // The overhead estimate is floored at zero by construction.
    assert!(run.rtk_overhead_s() >= 0.0);
}

#[test]
fn rtk_paired_run_rejects_empty_argv() {
    let err = run_paired(Path::new("/bin/echo"), &[], &[], &std::env::temp_dir())
        .expect_err("empty argv must fail");
    assert!(matches!(err, L2Error::Command(_)), "got {err:?}");
}

#[test]
fn rtk_paired_run_rejects_empty_rtk_argv() {
    // rtk with no subcommand prints usage and exits zero, so an empty rtk_argv
    // would be measured as compressed output rather than failing. The raw argv
    // here cannot spawn: if the guard were dropped, the raw command would run
    // first and the error would name the spawn failure, so asserting on the
    // message keeps this test from passing for the wrong reason.
    let argv = vec!["/nonexistent/l2-raw-command".to_string()];
    let err = run_paired(Path::new("/bin/echo"), &argv, &[], &std::env::temp_dir())
        .expect_err("empty rtk_argv must fail");
    let L2Error::Command(message) = &err else {
        panic!("got {err:?}");
    };
    assert!(
        message.contains("empty rtk_argv"),
        "must be rejected before either process is spawned, got: {message}"
    );
}

#[test]
fn merge_streams_never_fuses_stdout_and_stderr_lines() {
    // The ground-truth regexes are line-anchored, so an unterminated stdout
    // must not run into the first stderr line. This is also the invariant that
    // keeps the raw (rtk-unavailable) path byte-identical to the paired path.
    assert_eq!(
        merge_streams(b"last line", b"warning"),
        "last line\nwarning"
    );
    // An existing terminator is not doubled.
    assert_eq!(merge_streams(b"line\n", b"warning"), "line\nwarning");
    // Empty stderr leaves stdout untouched — no stray trailing newline.
    assert_eq!(merge_streams(b"only stdout", b""), "only stdout");
    // Empty stdout must not gain a leading newline.
    assert_eq!(merge_streams(b"", b"only stderr"), "only stderr");
}

#[test]
fn rtk_paired_run_fails_on_raw_command_failure() {
    // The spec'd commands are expected to succeed; a failing raw run must be
    // an error, not a silent zero-length sample.
    let argv: Vec<String> = ["/bin/sh", "-c", "exit 3"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let err = run_paired(Path::new("/bin/echo"), &argv, &argv, &std::env::temp_dir())
        .expect_err("raw failure must surface");
    assert!(matches!(err, L2Error::Command(_)), "got {err:?}");
}
