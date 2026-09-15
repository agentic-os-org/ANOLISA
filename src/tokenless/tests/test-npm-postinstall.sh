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

if ! command -v node >/dev/null 2>&1; then
  echo "SKIP node is not available; postinstall.js adapter ownership not exercised"
  exit 0
fi

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

FAKE_VERSION="9.9.9"
PKG="$TEST_DIR/pkg"

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; exit 1; }
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
PLATFORM_KEY="$(node -e 'process.stdout.write(`${process.platform}-${process.arch}`)')"
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
run_postinstall() {
  local scenario="$1"; shift
  local home="$TEST_DIR/$scenario/home"
  mkdir -p "$home"
  OUT="$(env -i PATH="/usr/local/bin:/usr/bin:/bin" HOME="$home" "$@" \
          node "$PKG/scripts/postinstall.js" 2>&1)" && STATUS=0 || STATUS=$?
}

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
