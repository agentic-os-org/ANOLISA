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
#   6. a tree with no marker at all is preserved: absence of a marker is not
#      proof of ownership, and an older release or a manual copy leaves none
#
# Skipped when node is unavailable: the assertions need the real script.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TOKENLESS_ROOT="$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)"
POSTINSTALL="$TOKENLESS_ROOT/npm/scripts/postinstall.js"

[ -f "$POSTINSTALL" ] || { echo "FAIL missing $POSTINSTALL" >&2; exit 1; }

# TOKENLESS_REQUIRE_NODE=1 turns an unusable interpreter into a failure instead
# of a skip. That is what a job which has just installed a supported Node should
# ask for: a green run then means the ownership logic was executed, not that the
# test declined to run.
STRICT="${TOKENLESS_REQUIRE_NODE:-0}"

# Candidate interpreters, best first: an explicit override, whatever is on PATH,
# then the places CI images and version managers keep node. What is on PATH is
# not necessarily usable — a runner can carry an old system node ahead of the
# version its job installed — so each candidate has to prove it can run the
# script before any assertion is made.
NODE_OVERRIDE="${NODE_BIN:-}"
node_candidates() {
  if [ -n "$NODE_OVERRIDE" ]; then
    printf '%s\n' "$NODE_OVERRIDE"
    return 0
  fi
  command -v node 2>/dev/null || true
  local c
  for c in /opt/hostedtoolcache/node/*/*/bin/node \
           "${HOME:-/nonexistent}"/.nvm/versions/node/*/bin/node \
           /usr/local/bin/node; do
    [ -x "$c" ] && printf '%s\n' "$c"
  done
  return 0
}

NODE_BIN=""
if [ -z "$(node_candidates)" ]; then
  if [ "$STRICT" = "1" ]; then
    echo "FAIL TOKENLESS_REQUIRE_NODE=1 but no node interpreter was found" >&2
    exit 1
  fi
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
# the behaviour under test. Any candidate answers this one — platform and arch do
# not depend on which interpreter is picked.
PROBE_NODE="$(node_candidates | head -1 || true)"
PLATFORM_KEY="$("$PROBE_NODE" -e 'process.stdout.write(`${process.platform}-${process.arch}`)' 2>/dev/null \
  | tr -d '\r\n' | grep -oE '(linux|darwin)-(x64|arm64)' | head -1 || true)"
case "$PLATFORM_KEY" in
  linux-x64|linux-arm64|darwin-x64|darwin-arm64) ;;
  *)
    if [ "$STRICT" = "1" ]; then
      echo "FAIL TOKENLESS_REQUIRE_NODE=1 but no supported platform key came from ${PROBE_NODE:-node} (got '${PLATFORM_KEY:-}')" >&2
      exit 1
    fi
    echo "SKIP cannot determine a supported platform key from ${PROBE_NODE:-node} (got '${PLATFORM_KEY:-}')"
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
RUN_MODE="isolated"
run_postinstall() {
  local scenario="$1"; shift
  local home="$TEST_DIR/$scenario/home"
  local node_dir
  node_dir="$(dirname "$NODE_BIN")"
  mkdir -p "$home"
  if [ "$RUN_MODE" = "isolated" ]; then
    OUT="$(env -i PATH="$node_dir:/usr/local/bin:/usr/bin:/bin" HOME="$home" "$@" \
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
CANDIDATES_TRIED=0
PREFLIGHT_OK=0
PREFLIGHT_LOG=""
while IFS= read -r candidate; do
  [ -n "$candidate" ] || continue
  CANDIDATES_TRIED=$((CANDIDATES_TRIED + 1))
  NODE_BIN="$candidate"
  rm -rf "$TEST_DIR/preflight"
  RUN_MODE="isolated"
  run_postinstall preflight
  if [ "$STATUS" = "0" ]; then
    PREFLIGHT_OK=1
    break
  fi
  ISOLATED_OUT="$OUT"
  # A node that is itself a wrapper script needs the environment it was installed
  # into; retry with the inherited one before giving up on this candidate.
  RUN_MODE="inherited"
  rm -rf "$TEST_DIR/preflight"
  run_postinstall preflight
  if [ "$STATUS" = "0" ]; then
    PREFLIGHT_OK=1
    break
  fi
  PREFLIGHT_LOG="${PREFLIGHT_LOG}
--- ${candidate} ($("$candidate" --version 2>&1)) ---
isolated: ${ISOLATED_OUT}
inherited: ${OUT}"
  RUN_MODE="isolated"
done <<CANDIDATES
$(node_candidates)
CANDIDATES

if [ "$PREFLIGHT_OK" != "1" ]; then
  echo "postinstall.js could not be run by any node interpreter found:" >&2
  printf '%s\n' "$PREFLIGHT_LOG" | sed 's/^/  /' >&2
  echo "  platform key: ${PLATFORM_KEY} -> ${PLATFORM_PKG}" >&2
  echo "  fake package layout:" >&2
  ( cd "$PKG" && find . -maxdepth 4 | sort | sed 's/^/    /' ) >&2
  if [ "$STRICT" = "1" ]; then
    echo "FAIL TOKENLESS_REQUIRE_NODE=1: ${CANDIDATES_TRIED} node interpreter(s) found, none can run postinstall.js" >&2
    exit 1
  fi
  echo "SKIP no usable node (tried ${CANDIDATES_TRIED}); postinstall.js adapter ownership not exercised"
  exit 0
fi
OUT=""
ISOLATED_OUT=""
pass "postinstall.js runs against the fake package layout (node $("$NODE_BIN" --version 2>&1), ${RUN_MODE} env)"

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

# =============================================================================
# 6. An unmarked tree is preserved, not treated as free to replace
# =============================================================================
# Nothing about the content of ~/.local/share/anolisa/adapters/tokenless says who
# put it there, so "no marker" has to mean "not proven mine". A release from
# before the marker existed, or a directory somebody copied by hand, would
# otherwise be deleted by a plain `npm install -g` together with every framework
# registration pointing into it.
seed_anolisa unmarked
rm -f "$(contract_of unmarked)"
UNMARKED="$(adapters_of unmarked)"
assert_no_file "the fixture really has no ownership marker" "$UNMARKED/.tokenless-owner"
UNMARKED_BEFORE="$(cat "$UNMARKED/manifest.json")"
run_postinstall unmarked
assert_eq "postinstall next to an unmarked tree exits 0" "$STATUS" "0"
assert_eq "keeps an unmarked tree byte for byte" "$(cat "$UNMARKED/manifest.json")" "$UNMARKED_BEFORE"
assert_no_file "writes no marker into a tree it does not own" "$UNMARKED/.tokenless-owner"
assert_contains "says the tree has no ownership marker" "$OUT" "no ownership marker"
assert_contains "points at the resources inside the package" "$OUT" "$PKG/adapters/tokenless"
assert_contains "documents the override" "$OUT" "ANOLISA_TOKENLESS_FORCE_ADAPTERS=1"
assert_file "still links the launcher binaries" "$PKG/bin/tokenless"

run_postinstall unmarked "ANOLISA_TOKENLESS_FORCE_ADAPTERS=1"
assert_eq "the override on an unmarked tree exits 0" "$STATUS" "0"
assert_eq "the override replaces the unmarked tree" "$(cat "$UNMARKED/manifest.json")" \
  "{\"component\":\"tokenless\",\"version\":\"$FAKE_VERSION\"}"
assert_eq "the override marks the tree it placed" "$(cat "$UNMARKED/.tokenless-owner")" \
  "npm:anolisa-tokenless@$FAKE_VERSION"

# =============================================================================
# 7. A copy that fails must not cost the tree that is already there
# =============================================================================
# Deleting the existing tree before copying the new one means an ENOSPC or a
# permission failure during the copy leaves the shared directory empty or partial,
# with every framework registration still pointing into it — and the install still
# reports success. The swap has to be copy-then-move.
if [ "$(id -u)" = "0" ]; then
  echo "SKIP the unwritable-parent scenario cannot be exercised as root"
else
  run_postinstall copy-fails
  assert_eq "the first install exits 0" "$STATUS" "0"
  COPYFAIL="$(adapters_of copy-fails)"
  COPYFAIL_PARENT="$(dirname "$COPYFAIL")"
  COPYFAIL_BEFORE="$(cat "$COPYFAIL/manifest.json")"
  chmod 0555 "$COPYFAIL_PARENT"
  run_postinstall copy-fails
  assert_eq "postinstall still exits 0 when it cannot replace the tree" "$STATUS" "0"
  assert_eq "the tree that was already there is untouched" \
    "$(cat "$COPYFAIL/manifest.json")" "$COPYFAIL_BEFORE"
  assert_eq "its ownership marker is untouched" "$(cat "$COPYFAIL/.tokenless-owner")" \
    "npm:anolisa-tokenless@$FAKE_VERSION"
  assert_contains "says the existing resources were left in place" "$OUT" "were left in place"
  chmod 0755 "$COPYFAIL_PARENT"
  leftovers=0
  for entry in "$COPYFAIL_PARENT"/tokenless.tokenless-*; do
    [ -e "$entry" ] && leftovers=$((leftovers + 1))
  done
  assert_eq "leaves no staging directory behind" "$leftovers" "0"
fi

# =============================================================================
# 8. A staging directory left behind by a failed run cannot corrupt this one
# =============================================================================
# A fixed staging name collides with whatever a previous failure left there, and
# copying into an existing directory nests the payload one level down instead of
# failing — which is how a "restored" tree ends up incomplete.
run_postinstall leftover
LEFT="$(adapters_of leftover)"
mkdir -p "$LEFT.tokenless-new-12345"
printf '{"stale":true}\n' > "$LEFT.tokenless-new-12345/manifest.json"
run_postinstall leftover
assert_eq "postinstall over a leftover staging directory exits 0" "$STATUS" "0"
assert_eq "the tree is the package's own payload" "$(cat "$LEFT/manifest.json")" \
  "{\"component\":\"tokenless\",\"version\":\"$FAKE_VERSION\"}"
assert_no_file "the payload was not nested one level down" "$LEFT/leftover/manifest.json"
assert_file "the unrelated leftover is not touched" "$LEFT.tokenless-new-12345/manifest.json"
assert_eq "the leftover is still exactly what it was" \
  "$(cat "$LEFT.tokenless-new-12345/manifest.json")" '{"stale":true}'

echo "npm-postinstall test passed"
