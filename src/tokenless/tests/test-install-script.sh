#!/usr/bin/env bash
# Regression tests for the standalone curl installer and its uninstaller:
#   scripts/install.sh / scripts/uninstall.sh
#
# Covered behaviours (all offline — curl, npm and cargo are stubbed):
#   1. The source-build fallback locates src/tokenless/Cargo.toml inside a real
#      GitHub archive layout, where the manifest sits four levels below the
#      temporary directory.
#   2. A version pin whose tag does not exist fails hard and never requests the
#      `main` branch archive.
#   3. The temporary source tree is removed and the EXIT trap does not report
#      "tmpdir: unbound variable" after try_source_build() has returned.
#   4. install.sh writes a receipt; uninstall.sh removes only the recorded
#      paths, so the npm / source / custom-install-dir scenarios each keep
#      files they do not own.

set -euo pipefail

SCRIPT_DIR="$(CDPATH='' cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
TOKENLESS_ROOT="$(CDPATH='' cd "$SCRIPT_DIR/.." && pwd -P)"
INSTALL_SH="$TOKENLESS_ROOT/scripts/install.sh"
UNINSTALL_SH="$TOKENLESS_ROOT/scripts/uninstall.sh"

TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

STUB_DIR="$TEST_DIR/stubs"
DIST_DIR="$TEST_DIR/dist"
mkdir -p "$STUB_DIR" "$DIST_DIR"

FAKE_VERSION="0.7.9"
ARCHIVE_ROOT="ANOLISA-tokenless-v${FAKE_VERSION}"

pass() { printf 'ok   %s\n' "$1"; }
fail() { printf 'FAIL %s\n' "$1" >&2; exit 1; }

assert_file() { [ -e "$2" ] || fail "$1: expected file $2"; pass "$1"; }
assert_no_file() { [ ! -e "$2" ] && [ ! -L "$2" ] || fail "$1: unexpected file $2"; pass "$1"; }
assert_contains() {
  case "$2" in
    *"$3"*) pass "$1" ;;
    *) fail "$1: expected substring '$3' in: $2" ;;
  esac
}
assert_not_contains() {
  case "$2" in
    *"$3"*) fail "$1: unexpected substring '$3' in: $2" ;;
    *) pass "$1" ;;
  esac
}
assert_eq() { [ "$2" = "$3" ] || fail "$1: expected '$3', got '$2'"; pass "$1"; }

# --- a GitHub-shaped source archive -----------------------------------------
# <archive-root>/src/tokenless/Cargo.toml is depth 4 from the extraction dir;
# a nested crate manifest must not be picked up instead.
build_fake_archive() {
  local stage="$TEST_DIR/stage/$ARCHIVE_ROOT"
  rm -rf "$TEST_DIR/stage"
  mkdir -p "$stage/src/tokenless/crates/tokenless-cli"
  printf '[workspace]\nmembers = ["crates/tokenless-cli"]\n' > "$stage/src/tokenless/Cargo.toml"
  printf '[package]\nname = "tokenless-cli"\n' > "$stage/src/tokenless/crates/tokenless-cli/Cargo.toml"
  printf '# ANOLISA\n' > "$stage/README.md"
  tar -czf "$DIST_DIR/tag.tar.gz" -C "$TEST_DIR/stage" "$ARCHIVE_ROOT"
  printf '%s\n' "$DIST_DIR/tag.tar.gz"
}
FAKE_TARBALL="$(build_fake_archive)"

# --- stubs -------------------------------------------------------------------
cat > "$STUB_DIR/curl" <<'STUB'
#!/usr/bin/env bash
url=""; out=""; args=("$@"); i=0
while [ "$i" -lt "${#args[@]}" ]; do
  a="${args[$i]}"
  case "$a" in
    -o) out="${args[$((i+1))]}"; i=$((i+2)); continue ;;
    -*) i=$((i+1)); continue ;;
    *)  url="$a"; i=$((i+1)); continue ;;
  esac
done
printf '%s\n' "$url" >> "$CURL_LOG"
case "$url" in
  *"/archive/refs/tags/"*)
    [ "${CURL_TAG_STATUS:-0}" = "0" ] || exit "${CURL_TAG_STATUS}"
    [ -n "$out" ] && cp "$CURL_TAG_TARBALL" "$out"
    exit 0 ;;
  *"/archive/refs/heads/main"*)
    [ -n "$out" ] && cp "${CURL_MAIN_TARBALL:-/dev/null}" "$out"
    exit 0 ;;
  *"registry.npmjs.org"*)
    printf '{"name":"anolisa-tokenless","version":"%s"}\n' "${CURL_NPM_LATEST:-0.7.9}"
    exit 0 ;;
esac
exit 22
STUB

cat > "$STUB_DIR/npm" <<'STUB'
#!/usr/bin/env bash
pkg_prefix() {
  local prev="" a
  for a in "$@"; do
    if [ "$prev" = "--prefix" ]; then printf '%s\n' "$a"; return 0; fi
    prev="$a"
  done
  printf '%s\n' "${NPM_STUB_PREFIX:-$HOME/.npm-global}"
}
case "$1" in
  config)
    printf '%s\n' "${NPM_STUB_PREFIX:-$HOME/.npm-global}"; exit 0 ;;
  install)
    if [ "${NPM_STUB_FAIL:-0}" = "1" ]; then
      echo "npm ERR! code EACCES" >&2; exit 243
    fi
    prefix="$(pkg_prefix "$@")"
    mkdir -p "$prefix/bin" "$prefix/lib/node_modules/anolisa-tokenless"
    for b in tokenless rtk; do
      printf '#!/usr/bin/env bash\necho "%s %s-npm"\n' "$b" "${FAKE_VERSION:-0.7.9}" > "$prefix/bin/$b"
      chmod +x "$prefix/bin/$b"
    done
    # Mimic npm/scripts/postinstall.js: bundled adapters are copied into the
    # user data dir, replacing whatever was there.
    mkdir -p "$HOME/.local/share/anolisa/adapters/tokenless/claude-code/scripts"
    printf '#!/usr/bin/env bash\n' > "$HOME/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh"
    echo "added 1 package"; exit 0 ;;
  uninstall)
    prefix="$(pkg_prefix "$@")"
    rm -rf "$prefix/lib/node_modules/anolisa-tokenless"
    rm -f "$prefix/bin/tokenless" "$prefix/bin/rtk"
    echo "removed 1 package"; exit 0 ;;
esac
exit 0
STUB

cat > "$STUB_DIR/cargo" <<'STUB'
#!/usr/bin/env bash
# `cargo build --release --locked -p tokenless-cli`, run from src/tokenless.
mkdir -p target/release
printf '#!/usr/bin/env bash\necho "tokenless %s-src"\n' "${FAKE_VERSION:-0.7.9}" > target/release/tokenless
chmod +x target/release/tokenless
exit 0
STUB

chmod +x "$STUB_DIR/curl" "$STUB_DIR/npm" "$STUB_DIR/cargo"

# --- harness -----------------------------------------------------------------
# run_script <script> <scenario> [ENV=VAL ...] [-- <script-arg> ...]
# Runs a script in an isolated HOME with only the stubs on PATH; combined output
# lands in RUN_OUTPUT and the exit status in RUN_STATUS.
RUN_STATUS=0
RUN_OUTPUT=""
run_script() {
  local script="$1"; shift
  local scenario="$1"; shift
  local home="$TEST_DIR/$scenario/home"
  local tmp="$TEST_DIR/$scenario/tmp"
  local envs=() script_args=() seen_sep=0 a
  for a in "$@"; do
    if [ "$seen_sep" = "1" ]; then script_args+=("$a"); continue; fi
    if [ "$a" = "--" ]; then seen_sep=1; continue; fi
    envs+=("$a")
  done
  mkdir -p "$home" "$tmp"
  RUN_OUTPUT="$(
    env -i \
      PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
      HOME="$home" \
      SHELL=/bin/bash \
      TMPDIR="$tmp" \
      CURL_LOG="$TEST_DIR/$scenario/curl.log" \
      CURL_TAG_TARBALL="$FAKE_TARBALL" \
      CURL_MAIN_TARBALL="$DIST_DIR/main.tar.gz" \
      FAKE_VERSION="$FAKE_VERSION" \
      NPM_STUB_PREFIX="$TEST_DIR/$scenario/npm-prefix" \
      ${envs[@]+"${envs[@]}"} \
      bash "$script" ${script_args[@]+"${script_args[@]}"} 2>&1
  )" && RUN_STATUS=0 || RUN_STATUS=$?
  touch "$TEST_DIR/$scenario/curl.log"
}

receipt_of() { printf '%s\n' "$TEST_DIR/$1/home/.local/share/tokenless/install-receipt"; }
receipt_value() { sed -n "s/^$2=//p" "$(receipt_of "$1")" | head -1; }
receipt_files() { sed -n 's/^file=//p' "$(receipt_of "$1")"; }
curl_log() { cat "$TEST_DIR/$1/curl.log"; }

# =============================================================================
# Scenario 1 — forced source build against a real GitHub archive layout
# =============================================================================
run_script "$INSTALL_SH" source-build \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "source build exits 0" "$RUN_STATUS" "0"
S1_DIR="$TEST_DIR/source-build/home/.local/bin"
assert_file "source build installs the CLI" "$S1_DIR/tokenless"
assert_no_file "source build does not install rtk" "$S1_DIR/rtk"
assert_not_contains "no unbound-variable error from the EXIT trap" "$RUN_OUTPUT" "unbound variable"
assert_not_contains "no leftover-tempdir warning" "$RUN_OUTPUT" "No such file or directory"
assert_eq "no temporary source tree left behind" "$(find "$TEST_DIR/source-build/tmp" -mindepth 1 | wc -l | tr -d ' ')" "0"
assert_contains "requests the version tag archive" "$(curl_log source-build)" "archive/refs/tags/tokenless/v${FAKE_VERSION}.tar.gz"
assert_not_contains "never requests the main branch archive" "$(curl_log source-build)" "refs/heads/main"
assert_eq "receipt records the source method" "$(receipt_value source-build method)" "source"
assert_eq "receipt records the version" "$(receipt_value source-build version)" "$FAKE_VERSION"
assert_eq "receipt records only the CLI" "$(receipt_files source-build)" "$S1_DIR/tokenless"
assert_eq "source receipt has no npm prefix" "$(receipt_value source-build npm_prefix)" ""
assert_eq "source receipt has no adapters dir" "$(receipt_value source-build adapters_dir)" ""
assert_contains "reports CLI-only scope" "$RUN_OUTPUT" "CLI only"

# =============================================================================
# Scenario 2 — source build must not remove files it never installed
# =============================================================================
S2_DIR="$TEST_DIR/source-ownership/home/.local/bin"
run_script "$INSTALL_SH" source-ownership \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "TOKENLESS_INSTALL_DIR=$S2_DIR"
assert_eq "custom install dir source build exits 0" "$RUN_STATUS" "0"
# Foreign artefacts from another installation method (anolisa CLI / npm).
mkdir -p "$S2_DIR" "$TEST_DIR/source-ownership/home/.local/share/anolisa/adapters/tokenless/qoder"
printf '#!/bin/sh\n' > "$S2_DIR/rtk"
printf '#!/bin/sh\n' > "$S2_DIR/toon"
printf 'foreign\n' > "$TEST_DIR/source-ownership/home/.local/share/anolisa/adapters/tokenless/qoder/keep"
mkdir -p "$TEST_DIR/source-ownership/home/.tokenless"
printf 'stats\n' > "$TEST_DIR/source-ownership/home/.tokenless/stats.db"
assert_eq "custom install dir recorded" "$(receipt_value source-ownership install_dir)" "$S2_DIR"
assert_contains "installer appended a PATH entry before uninstall" \
  "$(cat "$TEST_DIR/source-ownership/home/.bashrc")" "# Added by tokenless installer"
run_script "$UNINSTALL_SH" source-ownership
assert_eq "uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the CLI it installed" "$S2_DIR/tokenless"
assert_file "keeps a foreign rtk" "$S2_DIR/rtk"
assert_file "keeps a foreign retired toon binary" "$S2_DIR/toon"
assert_file "keeps an adapter tree it never installed" \
  "$TEST_DIR/source-ownership/home/.local/share/anolisa/adapters/tokenless/qoder/keep"
assert_file "keeps runtime data without --purge" \
  "$TEST_DIR/source-ownership/home/.tokenless/stats.db"
assert_no_file "removes the receipt" "$(receipt_of source-ownership)"
assert_not_contains "strips only the PATH entry it appended" \
  "$(cat "$TEST_DIR/source-ownership/home/.bashrc")" "tokenless installer"

# =============================================================================
# Scenario 3 — pinned version with a missing tag must fail and never fetch main
# =============================================================================
run_script "$INSTALL_SH" missing-tag \
  "TOKENLESS_VERSION=does-not-exist" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_TAG_STATUS=22"
[ "$RUN_STATUS" -ne 0 ] || fail "missing tag: expected a non-zero exit"
pass "missing tag fails the install"
assert_contains "requests the pinned tag" "$(curl_log missing-tag)" "archive/refs/tags/tokenless/vdoes-not-exist.tar.gz"
assert_not_contains "never falls back to main" "$(curl_log missing-tag)" "refs/heads/main"
assert_contains "explains the no-main-fallback contract" "$RUN_OUTPUT" "never falls back to the 'main' branch"
assert_contains "reports the missing tag" "$RUN_OUTPUT" "does not exist on alibaba/anolisa"
assert_no_file "installs nothing" "$TEST_DIR/missing-tag/home/.local/bin/tokenless"

# A transport failure (not a 404) must also stop the install without fetching main.
run_script "$INSTALL_SH" transport-failure \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_TAG_STATUS=52"
[ "$RUN_STATUS" -ne 0 ] || fail "transport failure: expected a non-zero exit"
pass "transport failure fails the install"
assert_contains "reports a transport failure, not a missing tag" "$RUN_OUTPUT" "transport failure"
assert_not_contains "transport failure never requests main" "$(curl_log transport-failure)" "refs/heads/main"
assert_no_file "transport failure installs nothing" "$TEST_DIR/transport-failure/home/.local/bin/tokenless"

# Same contract on the automatic npm -> source fallback chain.
run_script "$INSTALL_SH" missing-tag-npm-fallback \
  "TOKENLESS_VERSION=does-not-exist" \
  "NPM_STUB_FAIL=1" \
  "CURL_TAG_STATUS=22"
[ "$RUN_STATUS" -ne 0 ] || fail "npm fallback + missing tag: expected a non-zero exit"
pass "npm failure then missing tag fails the install"
assert_not_contains "npm fallback never requests main" "$(curl_log missing-tag-npm-fallback)" "refs/heads/main"

# =============================================================================
# Scenario 4 — npm path: receipt contents and ownership-aware uninstall
# =============================================================================
run_script "$INSTALL_SH" npm-install "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "npm install exits 0" "$RUN_STATUS" "0"
S4_DIR="$TEST_DIR/npm-install/home/.local/bin"
assert_file "npm path links tokenless" "$S4_DIR/tokenless"
assert_file "npm path links rtk" "$S4_DIR/rtk"
assert_no_file "npm path does not create the retired toon binary" "$S4_DIR/toon"
assert_eq "receipt records the npm method" "$(receipt_value npm-install method)" "npm"
assert_eq "receipt records the npm prefix" "$(receipt_value npm-install npm_prefix)" "$TEST_DIR/npm-install/npm-prefix"
assert_eq "receipt records the adapters dir" "$(receipt_value npm-install adapters_dir)" \
  "$TEST_DIR/npm-install/home/.local/share/anolisa/adapters/tokenless"
assert_eq "receipt records both binaries" "$(receipt_files npm-install | tr '\n' ' ')" "$S4_DIR/tokenless $S4_DIR/rtk "
assert_file "records the rc file it modified" "$TEST_DIR/npm-install/home/.bashrc"
assert_contains "rc file carries the installer marker" \
  "$(cat "$TEST_DIR/npm-install/home/.bashrc")" "# Added by tokenless installer"

# Foreign neighbours in the same install dir must survive.
printf '#!/bin/sh\n' > "$S4_DIR/toon"
printf '#!/bin/sh\n' > "$S4_DIR/anolisa"
mkdir -p "$TEST_DIR/npm-install/home/.tokenless"
printf 'stats\n' > "$TEST_DIR/npm-install/home/.tokenless/stats.db"

run_script "$UNINSTALL_SH" npm-install -- "--dry-run"
assert_eq "dry run exits 0" "$RUN_STATUS" "0"
assert_file "dry run keeps the CLI" "$S4_DIR/tokenless"
assert_file "dry run keeps the receipt" "$(receipt_of npm-install)"

run_script "$UNINSTALL_SH" npm-install
assert_eq "npm uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the tokenless link" "$S4_DIR/tokenless"
assert_no_file "removes the rtk link" "$S4_DIR/rtk"
assert_file "keeps a foreign toon binary" "$S4_DIR/toon"
assert_file "keeps a foreign anolisa binary" "$S4_DIR/anolisa"
assert_no_file "removes the npm-owned adapters dir" \
  "$TEST_DIR/npm-install/home/.local/share/anolisa/adapters/tokenless"
assert_file "keeps runtime data without --purge" "$TEST_DIR/npm-install/home/.tokenless/stats.db"
assert_not_contains "removes the PATH entry it appended" \
  "$(cat "$TEST_DIR/npm-install/home/.bashrc")" "tokenless installer"
assert_no_file "removes the receipt" "$(receipt_of npm-install)"
assert_contains "npm global package removed" "$RUN_OUTPUT" "removed 1 package"

run_script "$UNINSTALL_SH" npm-install
[ "$RUN_STATUS" -ne 0 ] || fail "second uninstall without a receipt: expected a non-zero exit"
pass "uninstall without a receipt refuses to guess"
assert_contains "points at the per-method manual steps" "$RUN_OUTPUT" "npm uninstall -g anolisa-tokenless"

# =============================================================================
# Scenario 5 — latest version resolution still uses a tag, never main
# =============================================================================
run_script "$INSTALL_SH" latest-source \
  "TOKENLESS_FORCE_BUILD=1" \
  "CURL_NPM_LATEST=$FAKE_VERSION"
assert_eq "unpinned source build exits 0" "$RUN_STATUS" "0"
assert_contains "resolves the latest version from the npm registry" "$(curl_log latest-source)" \
  "registry.npmjs.org/anolisa-tokenless/latest"
assert_contains "downloads the resolved tag" "$(curl_log latest-source)" \
  "archive/refs/tags/tokenless/v${FAKE_VERSION}.tar.gz"
assert_not_contains "unpinned build never requests main" "$(curl_log latest-source)" "refs/heads/main"

# =============================================================================
# Scenario 6 — --purge removes the runtime data directory
# =============================================================================
run_script "$INSTALL_SH" purge "TOKENLESS_VERSION=$FAKE_VERSION" "TOKENLESS_FORCE_BUILD=1"
assert_eq "purge scenario install exits 0" "$RUN_STATUS" "0"
mkdir -p "$TEST_DIR/purge/home/.tokenless"
printf 'stash\n' > "$TEST_DIR/purge/home/.tokenless/stash.db"
run_script "$UNINSTALL_SH" purge -- "--purge"
assert_eq "purge uninstall exits 0" "$RUN_STATUS" "0"
assert_no_file "--purge removes runtime data" "$TEST_DIR/purge/home/.tokenless/stash.db"

echo "install-script test passed"
