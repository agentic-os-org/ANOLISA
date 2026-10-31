#!/usr/bin/env bash
# Regression: Codex detect.sh must stay fail-open when HOME is unset.
#
# With set -euo pipefail, expanding bare $HOME in the fallback path list
# aborts before the script can print {"installed": false}. The adapter
# README documents that missing-CLI probes exit 0 with that JSON.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
DETECT="$SCRIPT_DIR/../adapters/tokenless/codex/scripts/detect.sh"

[ -f "$DETECT" ] || {
  echo "FAIL missing detect script: $DETECT" >&2
  exit 1
}

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; exit 1; }

set +e
OUT="$(env -i PATH=/bin bash "$DETECT" 2>&1)"
STATUS=$?
set -e

[ "$STATUS" -eq 0 ] || fail "HOME unset: expected exit 0, got $STATUS (output: $OUT)"
[ "$OUT" = '{"installed": false}' ] || \
  fail "HOME unset: expected compact not-installed JSON, got: $OUT"

pass "codex detect prints not-installed JSON when HOME is unset"
echo "codex-detect-home test passed"
