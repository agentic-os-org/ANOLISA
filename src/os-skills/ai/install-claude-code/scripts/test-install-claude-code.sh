#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install-claude-code.sh — declining the API key prompt
# must skip configuration gracefully (WARN + continue with the remaining
# install steps), not abort the installer. main() runs under
# `set -euo pipefail`, so a non-zero return from the bare `write_config`
# call used to kill the script before Step 6.
#
# Runs the real script's functions with the trailing `main "$@"` invocation
# stripped, then appends the scenario calls. No dependencies; run directly:
#     bash test-install-claude-code.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/install-claude-code.sh"

pass=0
fail=0

# Build a runnable harness: the script body (sans `main "$@"`) plus the
# scenario lines, executed as one script — reproducing main's errexit
# environment exactly.
make_harness() {
    local out="$1"
    shift
    grep -v '^[[:space:]]*main "\$@"' "$SCRIPT" > "$out"
    for line in "$@"; do
        echo "$line" >> "$out"
    done
}

# scenario <name> <stdin-file> <home-dir> [extra env assignments...]
# Asserts the skip path: WARN printed, execution continues, exit 0.
check_skip_scenario() {
    local name="$1" stdin_file="$2" home_dir="$3"
    shift 3
    local harness
    harness="$(mktemp)"
    make_harness "$harness" "write_config" 'echo "STEP6_REACHED"'
    local out exit_code
    out="$(HOME="$home_dir" bash "$harness" < "$stdin_file" 2>&1)"
    exit_code=$?
    if [[ "$out" == *"skipping configuration"* && "$out" == *"STEP6_REACHED"* && "$exit_code" -eq 0 ]]; then
        echo "ok 1 - $name"
        pass=$((pass + 1))
    else
        echo "not ok 1 - $name (exit=$exit_code)"
        echo "$out" | sed 's/^/    /'
        fail=$((fail + 1))
    fi
    rm -f "$harness"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Interactive decline: user presses Enter at the prompt.
printf '\n' > "$tmp/enter"
# Non-interactive: stdin already closed.
: > "$tmp/eof"

check_skip_scenario "decline with Enter still reaches step 6" "$tmp/enter" "$tmp/home1"
check_skip_scenario "EOF stdin still reaches step 6" "$tmp/eof" "$tmp/home2"

# Positive path: with an API key provided, settings are written and the
# script continues.
harness="$tmp/harness-pos"
make_harness "$harness" 'write_config' 'echo "STEP6_REACHED"'
pos_home="$tmp/poshome"
pos_out="$(HOME="$pos_home" CLAUDE_API_KEY="sk-test" bash "$harness" < /dev/null 2>&1)"
pos_code=$?
if [[ "$pos_code" -eq 0 && "$pos_out" == *"Configuration written"* && "$pos_out" == *"STEP6_REACHED"* \
      && -f "$pos_home/.claude/settings.json" ]]; then
    echo "ok 2 - provided key writes settings and continues"
    pass=$((pass + 1))
else
    echo "not ok 2 - provided key writes settings and continues (exit=$pos_code)"
    echo "$pos_out" | sed 's/^/    /'
    fail=$((fail + 1))
fi

echo "1..$((pass + fail))"
if [[ "$fail" -gt 0 ]]; then
    exit 1
fi
exit 0
