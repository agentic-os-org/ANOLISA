#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

# One-shot runner for the tokenless benchmark suite.
#
#   ./run-benchmarks.sh            # build, tests, benches, compression report
#   ./run-benchmarks.sh --quick    # skip criterion benches (tests + report only)
#
# The criterion benches follow the report methodology: run this 3 times and
# average the per-benchmark medians (criterion itself uses 100 samples/bench).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

QUICK=0
[[ "${1:-}" == "--quick" ]] && QUICK=1

# Record source identity for traceability. Provenance covers everything the
# report attributes results to: git rev, full working-tree dirtiness
# (staged + unstaged + untracked via `status --porcelain`), the exact
# Cargo.lock and fixture bytes, and the rtk version. Hostname is deliberately
# NOT recorded — it leaks infrastructure naming into shareable artifacts.
sha256_of() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$@" 2>/dev/null | awk '{print $1}' | tail -1
    else
        shasum -a 256 "$@" 2>/dev/null | awk '{print $1}' | tail -1
    fi
}

IDENTITY_FILE="$SCRIPT_DIR/benchmark_identity.json"
GIT_REV=$(git -C "$SCRIPT_DIR" rev-parse --short HEAD 2>/dev/null || echo "unknown")
if [[ -n "$(git -C "$SCRIPT_DIR" status --porcelain 2>/dev/null)" ]]; then
    GIT_DIRTY=true
else
    GIT_DIRTY=false
fi
LOCK_SHA=$(sha256_of "$SCRIPT_DIR/Cargo.lock" || echo "unknown")
FIXTURES_SHA=$(cat "$SCRIPT_DIR"/fixtures/*.json 2>/dev/null | sha256_of /dev/stdin || echo "unknown")
RTK_BIN_PATH="${RTK_BIN:-$SCRIPT_DIR/../../third_party/rtk/target/release/rtk}"

# Resolve an rtk reference to a concrete executable path, mirroring the Rust
# `find_rtk_binary` convention. `$RTK_BIN` may be a bare command name
# (`RTK_BIN=rtk`), which the shell would exec through `PATH` — so testing `-x`
# on the raw reference only inspects the CWD and would report a perfectly
# usable `PATH` rtk as "unavailable" while the Rust side still finds and runs
# it. Paths containing a slash are checked as given.
resolve_rtk_bin() {
    local ref="$1" resolved=""
    if [[ "$ref" == */* ]]; then
        resolved="$ref"
    else
        resolved="$(command -v -- "$ref" 2>/dev/null)" || resolved=""
    fi
    if [[ -n "$resolved" && -f "$resolved" && -x "$resolved" ]]; then
        printf '%s' "$resolved"
        return 0
    fi
    return 1
}

# Best-effort RTK version for traceability only: it must never hang the suite.
# Where the host has a timeout(1) helper (`gtimeout` covers macOS with
# coreutils installed) the probe runs under it, so a slow-starting or hung rtk
# costs at most the deadline instead of stalling the build/test steps below.
RTK_VERSION_TIMEOUT_SECS="${RTK_VERSION_TIMEOUT_SECS:-5}"
if command -v timeout > /dev/null 2>&1; then
    RTK_TIMEOUT_CMD="timeout"
elif command -v gtimeout > /dev/null 2>&1; then
    RTK_TIMEOUT_CMD="gtimeout"
else
    RTK_TIMEOUT_CMD=""
fi

# SIGTERM alone is not a deadline: a binary that traps or ignores it keeps
# timeout(1) — and the command substitution around it — waiting long past the
# deadline. Prefer GNU's second, unignorable SIGKILL deadline; else ask for
# SIGKILL outright (busybox timeout has no --kill-after); else fall back to the
# helper's default SIGTERM, which only bounds binaries that honour it. The two
# capability probes cost one short-lived subprocess each, once per run.
#
# So the worst case for the probe is RTK_VERSION_TIMEOUT_SECS plus the 1s
# SIGKILL grace on helpers that support it, and RTK_VERSION_TIMEOUT_SECS alone
# where only SIGTERM is available.
#
# The result is a command PREFIX that already carries the deadline
# ("timeout --kill-after=1 5"), and is empty when no helper exists — the probe
# then runs unbounded, as it did before any of this guarding. It is expanded
# unquoted on purpose so it splits into words; a plain string is used instead of
# an array because `"${empty_array[@]}"` aborts under `set -u` on bash 3.2,
# which is still the stock bash on macOS.
RTK_TIMEOUT_PREFIX=""
if [[ -n "$RTK_TIMEOUT_CMD" ]]; then
    if "$RTK_TIMEOUT_CMD" --kill-after=1 1 true > /dev/null 2>&1; then
        RTK_TIMEOUT_PREFIX="$RTK_TIMEOUT_CMD --kill-after=1 $RTK_VERSION_TIMEOUT_SECS"
    elif "$RTK_TIMEOUT_CMD" -s KILL 1 true > /dev/null 2>&1; then
        RTK_TIMEOUT_PREFIX="$RTK_TIMEOUT_CMD -s KILL $RTK_VERSION_TIMEOUT_SECS"
    else
        RTK_TIMEOUT_PREFIX="$RTK_TIMEOUT_CMD $RTK_VERSION_TIMEOUT_SECS"
    fi
fi

rtk_version_probe() {
    # Prints the first line of `<bin> --version`, and ONLY when the probe
    # succeeded: exit status 0 within the deadline. A timed-out, killed or
    # otherwise failing probe prints nothing, so the caller records
    # "unavailable" — partial stdout from a probe we had to abandon is worse
    # traceability than an honest sentinel, and a non-zero exit means the
    # string cannot be attributed to a working rtk. Output is captured whole
    # and cut at the first newline rather than piped through `head -1`: that
    # pipe would exit non-zero via SIGPIPE on a chatty binary and, under
    # `pipefail`, mask the probe's own status.
    local bin="$1" out="" status=0
    # shellcheck disable=SC2086 # RTK_TIMEOUT_PREFIX is an intentional flag list.
    out="$($RTK_TIMEOUT_PREFIX "$bin" --version 2>/dev/null)" || status=$?
    [[ "$status" -eq 0 ]] || out=""
    printf '%s' "${out%%$'\n'*}"
}

if RTK_BIN_RESOLVED="$(resolve_rtk_bin "$RTK_BIN_PATH")"; then
    RTK_VERSION="$(rtk_version_probe "$RTK_BIN_RESOLVED")"
else
    RTK_VERSION=""
fi
# Unresolvable binary, non-zero exit, empty stdout and an expired deadline all
# collapse to the same sentinel the README documents.
[[ -n "$RTK_VERSION" ]] || RTK_VERSION="unavailable"
TOKENLESS_VERSION=$(grep -m1 '^version' "$SCRIPT_DIR/../../Cargo.toml" 2>/dev/null | sed 's/.*"\(.*\)".*/\1/' || echo "unknown")
cat > "$IDENTITY_FILE" <<EOF
{
  "git_rev": "$GIT_REV",
  "dirty": $GIT_DIRTY,
  "timestamp": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "cargo_lock_sha256": "$LOCK_SHA",
  "fixtures_sha256": "$FIXTURES_SHA",
  "rtk_version": "$RTK_VERSION",
  "tokenless_workspace_version": "$TOKENLESS_VERSION"
}
EOF
echo "==> Source identity recorded: $IDENTITY_FILE"

echo "==> Building benchmark suite (release)"
cargo build --release

echo "==> Quality + adversarial tests (cargo test)"
cargo test --release

if [[ "$QUICK" -eq 0 ]]; then
    LOG_FILE="benchmark_output_$(date +%Y%m%d_%H%M%S).log"
    echo "==> Performance benchmarks (criterion, 100 samples each)"
    cargo bench 2>&1 | tee "$LOG_FILE"
    echo "==> Benchmark output saved to $LOG_FILE"
fi

echo "==> Compression-rate report (Rust in-process)"
cargo run --release --bin compression_rate

echo "==> Done. Criterion HTML reports under target/criterion/."
