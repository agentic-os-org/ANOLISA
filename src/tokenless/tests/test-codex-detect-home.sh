#!/usr/bin/env bash
# Regression: Codex detect.sh must stay fail-open when HOME is unset.
#
# With set -euo pipefail, expanding bare $HOME in the fallback path list
# aborts before the script can print {"installed": false}. The adapter
# README documents that missing-CLI probes exit 0 with that JSON.
#
# The assertion is about the abort, not about the object printed: env -i
# scrubs PATH and HOME but cannot remove the absolute fallbacks detect.sh
# probes, so on a host with a system-wide install the detector legitimately
# reports installed and an exact-stdout comparison would fail on the very
# commit that fixes the bug. Exit status plus "unbound variable" on stderr
# is what holds on every host.
set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
DETECT="$SCRIPT_DIR/../adapters/tokenless/codex/scripts/detect.sh"

[ -f "$DETECT" ] || {
  echo "FAIL missing detect script: $DETECT" >&2
  exit 1
}

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; exit 1; }

ERR_FILE="$(mktemp)"
trap 'rm -f "$ERR_FILE" || true' EXIT

set +e
# The interpreter that runs this file, not an assumed /bin/bash: that path does
# not exist on a non-usr-merged image or under NixOS.
OUT="$(env -i PATH=/bin "${BASH:-bash}" "$DETECT" 2>"$ERR_FILE")"
STATUS=$?
set -e
ERR="$(cat "$ERR_FILE")"

[ "$STATUS" -eq 0 ] || fail "HOME unset: expected exit 0, got $STATUS (stderr: $ERR)"
case "$ERR" in
  *"unbound variable"*) fail "HOME unset: an unguarded expansion aborted the probe: $ERR" ;;
esac
[ -n "$OUT" ] || fail "HOME unset: expected JSON on stdout, got nothing"
# A bare "{" would accept any object, including the {"error": ...} a detector
# could print under the same set -u pressure. The field the README documents is
# the one that has to be there.
case "$OUT" in
  *'"installed"'*) ;;
  *) fail "HOME unset: expected an installed field, got: $OUT" ;;
esac

pass "codex detect stays fail-open when HOME is unset"
echo "codex-detect-home test passed"
