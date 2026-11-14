#!/usr/bin/env bash
# An explicitly empty --from-local must fail, not silently fetch the mirror.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install-anolisa.sh"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

assert_rejected() {
  local label="$1"
  shift
  local rc=0
  bash "$INSTALLER" "$@" --dry-run \
    > "$TEMPORARY/$label.out" 2> "$TEMPORARY/$label.err" || rc=$?
  if [ "$rc" -ne 2 ]; then
    echo "FAIL $label: expected exit 2, got $rc" >&2
    cat "$TEMPORARY/$label.out" "$TEMPORARY/$label.err" >&2
    return 1
  fi
  if ! grep -F -- '--from-local requires a non-empty path' "$TEMPORARY/$label.err" >/dev/null; then
    echo "FAIL $label: missing diagnostic" >&2
    cat "$TEMPORARY/$label.err" >&2
    return 1
  fi
}

assert_rejected space --from-local ""
assert_rejected equals --from-local=

# A populated local source tree still selects from-local mode.
SOURCE="$TEMPORARY/source"
mkdir -p "$SOURCE/manifests/osbase" "$SOURCE/templates" "$SOURCE/scripts" \
  "$SOURCE/target/release"
printf 'version = "0.0.0-test"\n' > "$SOURCE/manifests/osbase/base.toml"
for component in cosh-ng sec-core agentsight tokenless skillfs ws-ckpt agent-memory; do
  mkdir -p "$SOURCE/manifests/components/$component"
  printf 'name = "%s"\n' "$component" \
    > "$SOURCE/manifests/components/$component/component.toml"
done
printf '#!/bin/sh\nexit 0\n' > "$SOURCE/target/release/anolisa"
chmod 0755 "$SOURCE/target/release/anolisa"

rc=0
bash "$INSTALLER" --from-local "$SOURCE" --dry-run \
  > "$TEMPORARY/valid.out" 2> "$TEMPORARY/valid.err" || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "FAIL valid: expected exit 0, got $rc" >&2
  cat "$TEMPORARY/valid.out" "$TEMPORARY/valid.err" >&2
  exit 1
fi
grep -F 'mode            : from-local' "$TEMPORARY/valid.out" >/dev/null || {
  echo "FAIL valid: did not select from-local mode" >&2
  cat "$TEMPORARY/valid.out" >&2
  exit 1
}
if [ -s "$TEMPORARY/valid.err" ]; then
  echo "FAIL valid: unexpected stderr" >&2
  cat "$TEMPORARY/valid.err" >&2
  exit 1
fi

echo 'ok - installer rejects empty --from-local values'
