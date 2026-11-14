#!/usr/bin/env bash
# Piped installation must not infer a source checkout from the caller's cwd.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install-anolisa.sh"
TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

unset ANOLISA_BIN_URL ANOLISA_MANIFEST_BUNDLE_URL ANOLISA_BIN_SHA256
unset ANOLISA_MANIFEST_BUNDLE_SHA256 ANOLISA_PREFIX ANOLISA_INSTALL_MODE
export ANOLISA_PREFIX="$TEMPORARY/prefix"
export ANOLISA_MIRROR="https://example.invalid"

mkdir -p "$TEMPORARY/plain" "$TEMPORARY/checkout/manifests/osbase" \
  "$TEMPORARY/checkout/templates" "$TEMPORARY/checkout/scripts" \
  "$TEMPORARY/checkout/target/release"
cp "$INSTALLER" "$TEMPORARY/checkout/scripts/install-anolisa.sh"
printf '#!/bin/sh\nexit 0\n' > "$TEMPORARY/checkout/target/release/anolisa"
chmod 0755 "$TEMPORARY/checkout/target/release/anolisa"
printf 'version = "0.0.0-test"\n' > "$TEMPORARY/checkout/manifests/osbase/base.toml"

assert_url_plan() {
  local cwd="$1" label="$2"
  (
    cd "$cwd"
    # Redirection keeps stdin semantics without a producer that can SIGPIPE
    # when Bash exits early and the pipe buffer cannot hold the whole script.
    bash -s -- --dry-run < "$INSTALLER"
  ) > "$TEMPORARY/$label.out" 2> "$TEMPORARY/$label.err"
  grep -F 'mode            : url-fetch' "$TEMPORARY/$label.out"
  grep -F 'plan-only (no curl, no tar)' "$TEMPORARY/$label.out"
  test ! -s "$TEMPORARY/$label.err"
  test ! -e "$ANOLISA_PREFIX"
}

assert_url_plan "$TEMPORARY/plain" plain
assert_url_plan "$TEMPORARY/checkout/scripts" checkout_parent

bash -s -- --help < "$INSTALLER" \
  > "$TEMPORARY/help.out" 2> "$TEMPORARY/help.err"
grep -F 'Usage: install-anolisa.sh' "$TEMPORARY/help.out"
test ! -s "$TEMPORARY/help.err"

# An explicit source remains supported when the installer arrives on stdin.
bash -s -- --from-local "$TEMPORARY/checkout" --dry-run < "$INSTALLER" \
  > "$TEMPORARY/local.out" 2> "$TEMPORARY/local.err"
grep -F 'mode            : from-local' "$TEMPORARY/local.out"
test ! -s "$TEMPORARY/local.err"
test ! -e "$ANOLISA_PREFIX"

# A real file still resolves its own checkout, independent of the caller.
(
  cd "$TEMPORARY/plain"
  bash "$TEMPORARY/checkout/scripts/install-anolisa.sh" --dry-run
) > "$TEMPORARY/file.out" 2> "$TEMPORARY/file.err"
grep -F 'mode            : auto-checkout' "$TEMPORARY/file.out"
test ! -s "$TEMPORARY/file.err"
test ! -e "$ANOLISA_PREFIX"

echo 'ok - stdin installation stays in URL mode unless a source is explicit'
