#!/usr/bin/env bash
# Regression test for npm/scripts/postinstall.js.
#
# The package postinstall copies the bundled adapters into the *shared* user data
# directory ~/.local/share/anolisa/adapters/tokenless, which the anolisa CLI and
# a hand-made copy use as well. It used to `rm -rf` that directory first, so a
# plain `npm install -g anolisa-tokenless` on a machine with a managed Tokenless
# component silently replaced that component's adapter resources while leaving
# its component record and every framework registration pointing at them.
#
# Covered here, against the real postinstall.js and a fake package layout:
#   1. a fresh home gets the adapters plus an ownership marker
#   2. an anolisa-managed tree (component contract present) is preserved
#   3. ANOLISA_TOKENLESS_FORCE_ADAPTERS=1 takes it over anyway
#   4. a tree this package placed is refreshed, not preserved
#   5. a tree marked by somebody else is preserved, while a marker from this
#      installer's own family (npm or the standalone curl installer) is refreshed
#
# Skipped when node is unavailable: the assertions need the real script.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TOKENLESS_ROOT="$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)"
POSTINSTALL="$TOKENLESS_ROOT/npm/scripts/postinstall.js"

[ -f "$POSTINSTALL" ] || { echo "FAIL missing $POSTINSTALL" >&2; exit 1; }

# node is invoked by absolute path: the scenarios below run the script under
# `env -i` with a minimal PATH, which is not where a CI runner keeps its node.
NODE_BIN="${NODE_BIN:-$(command -v node || true)}"
if [ -z "$NODE_BIN" ]; then
  echo "SKIP node is not available; postinstall.js adapter ownership not exercised"
  exit 0
fi

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

FAKE_VERSION="9.9.9"
PKG="$TEST_DIR/pkg"

pass() { printf 'ok   %s\n' "$1"; }
# The captured postinstall output goes with the failure: without it a non-zero
# exit in somebody else's environment is undiagnosable from the CI log alone.
fail() {
  printf 'FAIL %s\n' "$1" >&2
  if [ -n "${OUT:-}" ]; then
    printf 'node %s said:\n' "$("$NODE_BIN" --version 2>&1)" >&2
    printf '%s\n' "$OUT" | sed 's/^/    /' >&2
  fi
  exit 1
}
assert_file() { [ -e "$2" ] || fail "$1: expected file $2"; pass "$1"; }
assert_no_file() { [ ! -e "$2" ] && [ ! -L "$2" ] || fail "$1: unexpected file $2"; pass "$1"; }
assert_eq() { [ "$2" = "$3" ] || fail "$1: expected '$3', got '$2'"; pass "$1"; }
assert_contains() {
  case "$2" in
    *"$3"*) pass "$1" ;;
    *) fail "$1: expected substring '$3' in: $2" ;;
  esac
}

# --- a package layout postinstall.js can resolve ----------------------------
# Filtered rather than taken verbatim: a node that is a wrapper (nvm, snap, a
# toolchain shim) can put its own noise on stdout, and a polluted key would make
# the fake platform package unfindable for reasons that have nothing to do with
# the behaviour under test.
PLATFORM_KEY="$("$NODE_BIN" -e 'process.stdout.write(`${process.platform}-${process.arch}`)' 2>/dev/null \
  | tr -d '\r\n' | grep -oE '(linux|darwin)-(x64|arm64)' | head -1)"
case "$PLATFORM_KEY" in
  linux-x64|linux-arm64|darwin-x64|darwin-arm64) ;;
  *)
    echo "SKIP cannot determine a supported platform key from $NODE_BIN (got '${PLATFORM_KEY:-}')"
    exit 0 ;;
esac
PLATFORM_PKG="@anolisa/tokenless-${PLATFORM_KEY}"

mkdir -p "$PKG/scripts" "$PKG/adapters/tokenless/claude-code/scripts" \
         "$PKG/node_modules/$PLATFORM_PKG/bin"
cp "$POSTINSTALL" "$PKG/scripts/postinstall.js"
printf '{"name":"anolisa-tokenless","version":"%s","type":"module"}\n' "$FAKE_VERSION" \
  > "$PKG/package.json"
printf '{"component":"tokenless","version":"%s"}\n' "$FAKE_VERSION" \
  > "$PKG/adapters/tokenless/manifest.json"
printf '#!/usr/bin/env bash\n' > "$PKG/adapters/tokenless/claude-code/scripts/install.sh"
printf '{"name":"%s","version":"%s"}\n' "$PLATFORM_PKG" "$FAKE_VERSION" \
  > "$PKG/node_modules/$PLATFORM_PKG/package.json"
for b in tokenless rtk; do
  printf '#!/usr/bin/env bash\necho "%s"\n' "$b" > "$PKG/node_modules/$PLATFORM_PKG/bin/$b"
  chmod +x "$PKG/node_modules/$PLATFORM_PKG/bin/$b"
done

# run_postinstall <scenario> [ENV=VAL ...]
OUT=""
STATUS=0
ISOLATED_OUT=""
NODE_DIR="$(dirname "$NODE_BIN")"
RUN_MODE="isolated"
run_postinstall() {
  local scenario="$1"; shift
  local home="$TEST_DIR/$scenario/home"
  mkdir -p "$home"
  if [ "$RUN_MODE" = "isolated" ]; then
    OUT="$(env -i PATH="$NODE_DIR:/usr/local/bin:/usr/bin:/bin" HOME="$home" "$@" \
            "$NODE_BIN" "$PKG/scripts/postinstall.js" 2>&1)" && STATUS=0 || STATUS=$?
  else
    # A node that is itself a wrapper script needs the environment it was
    # installed into. Isolation is then limited to HOME, and the override
    # variable is pinned empty so an ambient value cannot leak into a scenario.
    OUT="$(HOME="$home" ANOLISA_TOKENLESS_FORCE_ADAPTERS='' "$@" \
            "$NODE_BIN" "$PKG/scripts/postinstall.js" 2>&1)" && STATUS=0 || STATUS=$?
  fi
}

# Preflight. That the fake package resolves and that this node can run the ESM
# script at all (cpSync needs >= 16.7) are properties of the environment, not of
# the ownership behaviour under test, so a failure here is reported with
# everything needed to tell the two apart instead of as a bare "expected 0, got 1".
run_postinstall preflight
if [ "$STATUS" != "0" ]; then
  # Retry with the inherited environment before blaming the package layout.
  ISOLATED_OUT="$OUT"
  RUN_MODE="inherited"
  rm -rf "$TEST_DIR/preflight"
  run_postinstall preflight
fi
if [ "$STATUS" != "0" ]; then
  case "$OUT$ISOLATED_OUT" in
    *"cpSync"*|*"rmSync"*|*"is not a function"*|*"SyntaxError"*|*"Unexpected token"*\
    |*"ERR_UNKNOWN_BUILTIN_MODULE"*|*"Cannot use import statement"*)
      echo "SKIP node $("$NODE_BIN" --version 2>&1) cannot run postinstall.js at all:"
      printf '%s\n' "$OUT" | sed 's/^/    /'
      exit 0 ;;
  esac
  echo "FAIL postinstall.js did not run against the fake package layout" >&2
  echo "  node: $("$NODE_BIN" --version 2>&1) ($NODE_BIN)" >&2
  echo "  platform key: ${PLATFORM_KEY} -> ${PLATFORM_PKG}" >&2
  echo "  isolated env said:" >&2
  printf '%s\n' "$ISOLATED_OUT" | sed 's/^/    /' >&2
  echo "  inherited env said:" >&2
  printf '%s\n' "$OUT" | sed 's/^/    /' >&2
  echo "  fake package layout:" >&2
  ( cd "$PKG" && find . -maxdepth 4 | sort | sed 's/^/    /' ) >&2
  exit 1
fi
pass "postinstall.js runs against the fake package layout (${RUN_MODE} env)"

adapters_of() { printf '%s\n' "$TEST_DIR/$1/home/.local/share/anolisa/adapters/tokenless"; }
contract_of() { printf '%s\n' "$TEST_DIR/$1/home/.local/share/anolisa/components/tokenless/component.toml"; }

# Seed an adapter tree that belongs to somebody else: an anolisa component
# installation keeps its contract next to the shared adapters directory.
seed_anolisa() {
  local scenario="$1" adapters contract
  adapters="$(adapters_of "$scenario")"
  contract="$(contract_of "$scenario")"
  mkdir -p "$adapters/claude-code/scripts" "$(dirname "$contract")"
  printf '{"component":"tokenless","version":"0.6.0"}\n' > "$adapters/manifest.json"
  printf '#!/usr/bin/env bash\n' > "$adapters/claude-code/scripts/install.sh"
  printf 'name = "tokenless"\nversion = "0.6.0"\n' > "$contract"
}

# =============================================================================
# 1. A fresh home gets the adapters, the launcher links and an ownership marker
# =============================================================================
run_postinstall fresh
assert_eq "postinstall on a fresh home exits 0" "$STATUS" "0"
FRESH="$(adapters_of fresh)"
assert_file "copies the bundled adapters" "$FRESH/manifest.json"
assert_eq "stamps the copied manifest with the package version" \
  "$(cat "$FRESH/manifest.json")" "{\"component\":\"tokenless\",\"version\":\"$FAKE_VERSION\"}"
assert_eq "marks the tree it placed" "$(cat "$FRESH/.tokenless-owner")" \
  "npm:anolisa-tokenless@$FAKE_VERSION"
assert_file "links the tokenless launcher" "$PKG/bin/tokenless"
assert_file "links the rtk launcher" "$PKG/bin/rtk"

# =============================================================================
# 2. An anolisa-managed tree is preserved
# =============================================================================
seed_anolisa managed
BEFORE="$(cat "$(adapters_of managed)/manifest.json")"
run_postinstall managed
assert_eq "postinstall next to a managed component exits 0" "$STATUS" "0"
MANAGED="$(adapters_of managed)"
assert_eq "keeps the managed adapter tree untouched" "$(cat "$MANAGED/manifest.json")" "$BEFORE"
assert_no_file "writes no ownership marker into a tree it does not own" \
  "$MANAGED/.tokenless-owner"
assert_file "the managed component contract survives" "$(contract_of managed)"
assert_contains "says whose tree it kept" "$OUT" "belongs to an anolisa component installation"
assert_contains "points at the resources inside the package" "$OUT" "$PKG/adapters/tokenless"
assert_contains "documents the override" "$OUT" "ANOLISA_TOKENLESS_FORCE_ADAPTERS=1"
assert_file "still links the launcher binaries" "$PKG/bin/tokenless"

# =============================================================================
# 3. The documented override does take the tree over
# =============================================================================
seed_anolisa forced
run_postinstall forced "ANOLISA_TOKENLESS_FORCE_ADAPTERS=1"
assert_eq "the override exits 0" "$STATUS" "0"
FORCED="$(adapters_of forced)"
assert_eq "the override replaces the managed tree" \
  "$(cat "$FORCED/manifest.json")" "{\"component\":\"tokenless\",\"version\":\"$FAKE_VERSION\"}"
assert_eq "the override marks the tree it placed" "$(cat "$FORCED/.tokenless-owner")" \
  "npm:anolisa-tokenless@$FAKE_VERSION"
assert_contains "warns about what it replaced" "$OUT" "replacing"

# =============================================================================
# 4. A tree this package placed is refreshed, not preserved
# =============================================================================
run_postinstall upgrade
assert_eq "first install exits 0" "$STATUS" "0"
UPGRADE="$(adapters_of upgrade)"
printf '{"component":"tokenless","version":"0.6.0-stale"}\n' > "$UPGRADE/manifest.json"
run_postinstall upgrade
assert_eq "reinstall over its own tree exits 0" "$STATUS" "0"
assert_eq "refreshes the tree it owns" "$(cat "$UPGRADE/manifest.json")" \
  "{\"component\":\"tokenless\",\"version\":\"$FAKE_VERSION\"}"
assert_eq "re-stamps its ownership marker" "$(cat "$UPGRADE/.tokenless-owner")" \
  "npm:anolisa-tokenless@$FAKE_VERSION"

# =============================================================================
# 5. A tree marked by somebody else is preserved; a family marker is refreshed
# =============================================================================
seed_anolisa foreign-marker
rm -f "$(contract_of foreign-marker)"
printf 'some-other-tool:tokenless@0.6.0\n' \
  > "$(adapters_of foreign-marker)/.tokenless-owner"
BEFORE="$(cat "$(adapters_of foreign-marker)/manifest.json")"
run_postinstall foreign-marker
assert_eq "postinstall next to a foreign marker exits 0" "$STATUS" "0"
assert_eq "keeps a tree another owner marked" \
  "$(cat "$(adapters_of foreign-marker)/manifest.json")" "$BEFORE"
assert_eq "leaves the foreign marker alone" \
  "$(cat "$(adapters_of foreign-marker)/.tokenless-owner")" "some-other-tool:tokenless@0.6.0"
assert_contains "says whose marker it found" "$OUT" "some-other-tool:tokenless@0.6.0"

# The standalone curl installer runs this same postinstall and then claims the
# tree for its own receipt, so its marker is family and must stay refreshable —
# otherwise a curl reinstall would pin the adapter resources to an old version.
seed_anolisa family-marker
rm -f "$(contract_of family-marker)"
printf 'curl-installer:20260101000000-1-deadbeef\n' \
  > "$(adapters_of family-marker)/.tokenless-owner"
run_postinstall family-marker
assert_eq "postinstall over a curl-installer marker exits 0" "$STATUS" "0"
assert_eq "refreshes a tree the curl installer claimed" \
  "$(cat "$(adapters_of family-marker)/manifest.json")" \
  "{\"component\":\"tokenless\",\"version\":\"$FAKE_VERSION\"}"
assert_eq "re-stamps the marker as this package's" \
  "$(cat "$(adapters_of family-marker)/.tokenless-owner")" \
  "npm:anolisa-tokenless@$FAKE_VERSION"

echo "npm-postinstall test passed"
