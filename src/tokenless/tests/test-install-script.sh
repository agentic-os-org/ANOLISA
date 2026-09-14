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
#   5. A failed write (install(1), ln, mkdir) fails the run instead of being
#      swallowed by the `||`/`elif` condition that disables errexit, and never
#      records a receipt or a pre-existing foreign binary.
#   6. Re-running with another method retires the previous receipt's artefacts
#      (rtk link, npm global package, adapter tree), and uninstall.sh refuses a
#      recorded path whose content another installer has since replaced.
#   7. uninstall.sh deregisters an enabled framework adapter (real qwencode
#      scripts, link-type registration) before deleting the adapter resources.

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
    adapters="$HOME/.local/share/anolisa/adapters/tokenless"
    rm -rf "$adapters"
    mkdir -p "$adapters/claude-code/scripts"
    printf '#!/usr/bin/env bash\n' > "$adapters/claude-code/scripts/install.sh"
    # package-npm.js stamps adapters/tokenless/manifest.json with the release
    # version; install.sh uses its digest as the adapter tree's identity.
    printf '{"component":"tokenless","version":"%s"}\n' "${FAKE_VERSION:-0.7.9}" > "$adapters/manifest.json"
    # Optionally ship a real adapter payload so the enable/disable chain can be
    # exercised against the repository's own scripts.
    if [ -n "${NPM_STUB_ADAPTER_SRC:-}" ] && [ -d "${NPM_STUB_ADAPTER_SRC}/qwencode" ]; then
      cp -R "${NPM_STUB_ADAPTER_SRC}/qwencode" "$adapters/qwencode"
    fi
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
if [ "${CARGO_STUB_FAIL:-0}" = "1" ]; then
  echo "error: could not compile \`tokenless-cli\`" >&2; exit 101
fi
mkdir -p target/release
printf '#!/usr/bin/env bash\necho "tokenless %s-src"\n' "${FAKE_VERSION:-0.7.9}" > target/release/tokenless
chmod +x target/release/tokenless
exit 0
STUB

# install(1) and ln pass through to the real binaries unless a scenario asks
# for a write failure. Both are called from functions that main() uses as
# `||`/`elif` conditions, where Bash disables errexit, so their status has to be
# checked explicitly by the installer — these stubs prove that it is.
cat > "$STUB_DIR/install" <<'STUB'
#!/usr/bin/env bash
if [ -n "${INSTALL_STUB_STATUS:-}" ]; then
  echo "install: cannot create regular file: Permission denied" >&2
  exit "${INSTALL_STUB_STATUS}"
fi
real="${REAL_INSTALL_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/install /bin/install; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "install stub: no real install(1) found" >&2; exit 127; }
exec "$real" "$@"
STUB

cat > "$STUB_DIR/ln" <<'STUB'
#!/usr/bin/env bash
if [ "${LN_STUB_FAIL:-0}" = "1" ]; then
  echo "ln: failed to create symbolic link: Permission denied" >&2
  exit 1
fi
real="${REAL_LN_BIN:-}"
if [ -z "$real" ]; then
  for c in /usr/bin/ln /bin/ln; do [ -x "$c" ] && real="$c" && break; done
fi
[ -n "$real" ] || { echo "ln stub: no real ln found" >&2; exit 127; }
exec "$real" "$@"
STUB

# Minimal `qwen` CLI so the real qwencode adapter scripts can link and unlink
# an extension that points into the adapter tree.
cat > "$STUB_DIR/qwen" <<'STUB'
#!/usr/bin/env bash
ext_dir="$HOME/.qwen/extensions"
case "$1 $2" in
  "extensions link")
    mkdir -p "$ext_dir"
    ln -sfn "$3" "$ext_dir/tokenless"
    echo "linked $3" ;;
  "extensions list")
    [ -e "$ext_dir/tokenless" ] && echo "tokenless" ;;
  "extensions uninstall")
    rm -rf "$ext_dir/tokenless"
    echo "uninstalled tokenless" ;;
esac
exit 0
STUB

REAL_INSTALL_BIN="$(command -v install || true)"
REAL_LN_BIN="$(command -v ln || true)"
[ -n "$REAL_INSTALL_BIN" ] && [ -n "$REAL_LN_BIN" ] \
  || { echo "FAIL the host provides no install(1) or ln" >&2; exit 1; }
chmod +x "$STUB_DIR/curl" "$STUB_DIR/npm" "$STUB_DIR/cargo" \
         "$STUB_DIR/install" "$STUB_DIR/ln" "$STUB_DIR/qwen"

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
      REAL_INSTALL_BIN="$REAL_INSTALL_BIN" \
      REAL_LN_BIN="$REAL_LN_BIN" \
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

# =============================================================================
# Scenario 7 — a failed install(1) must fail the run, not report success
# =============================================================================
# main() calls try_source_build as an `||`/`elif` condition, so errexit is off
# inside it: without an explicit status check, `install` exiting 73 used to be
# swallowed and the run still wrote a receipt and claimed success.
run_script "$INSTALL_SH" install-write-failure \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1" \
  "INSTALL_STUB_STATUS=73"
[ "$RUN_STATUS" -ne 0 ] || fail "install(1) exit 73: expected a non-zero exit"
pass "a failed install(1) fails the run"
assert_contains "reports the install(1) exit status" "$RUN_OUTPUT" "install(1) failed with exit 73"
assert_not_contains "never claims a successful install" "$RUN_OUTPUT" "installed successfully"
assert_no_file "writes no binary" "$TEST_DIR/install-write-failure/home/.local/bin/tokenless"
assert_no_file "writes no receipt" "$(receipt_of install-write-failure)"

# Same contract on the automatic npm -> source chain: neither method may report
# success when the write fails.
run_script "$INSTALL_SH" install-write-failure-fallback \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_FAIL=1" \
  "INSTALL_STUB_STATUS=73"
[ "$RUN_STATUS" -ne 0 ] || fail "npm failure + install(1) exit 73: expected a non-zero exit"
pass "a failed install(1) also fails the npm fallback chain"
assert_no_file "npm fallback writes no receipt" "$(receipt_of install-write-failure-fallback)"

# =============================================================================
# Scenario 8 — an install directory that cannot be created fails loudly
# =============================================================================
BLOCKED_DIR="$TEST_DIR/blocked-dir/blocked"
mkdir -p "$TEST_DIR/blocked-dir"
printf 'not a directory\n' > "$BLOCKED_DIR"
run_script "$INSTALL_SH" blocked-dir \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_INSTALL_DIR=$BLOCKED_DIR" \
  "CARGO_STUB_FAIL=1"
[ "$RUN_STATUS" -ne 0 ] || fail "uncreatable install dir: expected a non-zero exit"
pass "an uncreatable install directory fails the run"
assert_contains "reports the mkdir failure" "$RUN_OUTPUT" "Cannot create the install directory"
assert_no_file "uncreatable install dir writes no receipt" "$(receipt_of blocked-dir)"

# =============================================================================
# Scenario 9 — a failed ln must not adopt a pre-existing foreign binary
# =============================================================================
S9_DIR="$TEST_DIR/stale-link/home/.local/bin"
mkdir -p "$S9_DIR"
printf '#!/bin/sh\necho foreign-tokenless\n' > "$S9_DIR/tokenless"
chmod +x "$S9_DIR/tokenless"
run_script "$INSTALL_SH" stale-link \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "LN_STUB_FAIL=1" \
  "CARGO_STUB_FAIL=1"
[ "$RUN_STATUS" -ne 0 ] || fail "failed ln: expected a non-zero exit"
pass "a failed ln fails the run"
assert_contains "reports the failed link" "$RUN_OUTPUT" "Cannot write"
assert_file "leaves the pre-existing binary alone" "$S9_DIR/tokenless"
assert_contains "the pre-existing binary was not overwritten" \
  "$(cat "$S9_DIR/tokenless")" "foreign-tokenless"
assert_no_file "failed ln writes no receipt" "$(receipt_of stale-link)"
assert_not_contains "never claims a successful install" "$RUN_OUTPUT" "installed successfully"

# =============================================================================
# Scenario 10 — switching method retires the previous receipt's artefacts
# =============================================================================
run_script "$INSTALL_SH" switch-method "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "npm install exits 0 before the switch" "$RUN_STATUS" "0"
S10_DIR="$TEST_DIR/switch-method/home/.local/bin"
S10_NPM="$TEST_DIR/switch-method/npm-prefix/lib/node_modules/anolisa-tokenless"
S10_ADAPTERS="$TEST_DIR/switch-method/home/.local/share/anolisa/adapters/tokenless"
assert_file "npm path linked rtk" "$S10_DIR/rtk"
assert_file "npm path installed the global package" "$S10_NPM"
assert_file "npm path placed the adapter tree" "$S10_ADAPTERS/manifest.json"
assert_eq "receipt records the npm method before the switch" "$(receipt_value switch-method method)" "npm"

run_script "$INSTALL_SH" switch-method \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "source reinstall over an npm install exits 0" "$RUN_STATUS" "0"
assert_eq "receipt now records the source method" "$(receipt_value switch-method method)" "source"
assert_eq "receipt records only the CLI" "$(receipt_files switch-method)" "$S10_DIR/tokenless"
assert_file "the CLI itself survives the switch" "$S10_DIR/tokenless"
assert_no_file "removes the rtk link the npm method left behind" "$S10_DIR/rtk"
assert_no_file "removes the npm global package the previous method installed" "$S10_NPM"
assert_no_file "removes the adapter tree the previous method owned" "$S10_ADAPTERS"
assert_contains "reports what it retired" "$RUN_OUTPUT" "left behind by the previous npm install"

# The retired tree must also be gone for a later uninstall.sh run, and the new
# receipt must not claim artefacts this run never created.
run_script "$UNINSTALL_SH" switch-method
assert_eq "uninstall after the switch exits 0" "$RUN_STATUS" "0"
assert_no_file "removes the source-built CLI" "$S10_DIR/tokenless"
assert_no_file "no adapter tree is left to clean" "$S10_ADAPTERS"

# =============================================================================
# Scenario 11 — a recorded path taken over by another installer is kept
# =============================================================================
run_script "$INSTALL_SH" taken-over \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "TOKENLESS_FORCE_BUILD=1"
assert_eq "taken-over scenario install exits 0" "$RUN_STATUS" "0"
S11_CLI="$TEST_DIR/taken-over/home/.local/bin/tokenless"
assert_file "records the CLI it installed" "$S11_CLI"
# anolisa (or a manual npm install) later replaces the very same path.
printf '#!/bin/sh\necho "anolisa-managed tokenless"\n' > "$S11_CLI"
chmod +x "$S11_CLI"
run_script "$UNINSTALL_SH" taken-over
assert_eq "uninstall with a replaced file exits 0" "$RUN_STATUS" "0"
assert_file "keeps the file another installer put at the recorded path" "$S11_CLI"
assert_contains "the surviving file is the foreign one" "$(cat "$S11_CLI")" "anolisa-managed"
assert_contains "explains why the path was skipped" "$RUN_OUTPUT" "another installation has taken over that path"
assert_no_file "still removes the receipt" "$(receipt_of taken-over)"

# Same protection for the adapter tree.
run_script "$INSTALL_SH" adapters-taken-over "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "adapters-taken-over install exits 0" "$RUN_STATUS" "0"
S11_ADAPTERS="$TEST_DIR/adapters-taken-over/home/.local/share/anolisa/adapters/tokenless"
printf '{"component":"tokenless","version":"99.0.0-replaced"}\n' > "$S11_ADAPTERS/manifest.json"
run_script "$UNINSTALL_SH" adapters-taken-over
assert_eq "uninstall with a replaced adapter tree exits 0" "$RUN_STATUS" "0"
assert_file "keeps an adapter tree another installer replaced" "$S11_ADAPTERS/manifest.json"
assert_contains "explains why the adapter tree was skipped" "$RUN_OUTPUT" \
  "another installation has taken over that adapter tree"

# A schema-1 receipt carries no identity, so the uninstaller says so instead of
# silently deleting by path.
S11_LEGACY="$TEST_DIR/legacy-receipt/home"
mkdir -p "$S11_LEGACY/.local/share/tokenless" "$S11_LEGACY/.local/bin"
printf '#!/bin/sh\necho legacy\n' > "$S11_LEGACY/.local/bin/tokenless"
cat > "$S11_LEGACY/.local/share/tokenless/install-receipt" <<LEGACY
# Tokenless installer receipt (schema 1).
schema=1
method=source
version=$FAKE_VERSION
install_dir=$S11_LEGACY/.local/bin
file=$S11_LEGACY/.local/bin/tokenless
LEGACY
run_script "$UNINSTALL_SH" legacy-receipt
assert_eq "schema-1 uninstall exits 0" "$RUN_STATUS" "0"
assert_contains "warns that a schema-1 receipt has no identity" "$RUN_OUTPUT" "records no file identity"
assert_no_file "still removes the recorded path" "$S11_LEGACY/.local/bin/tokenless"

# =============================================================================
# Scenario 12 — adapter install -> enable -> uninstall, with a link-type adapter
# =============================================================================
run_script "$INSTALL_SH" adapter-lifecycle \
  "TOKENLESS_VERSION=$FAKE_VERSION" \
  "NPM_STUB_ADAPTER_SRC=$TOKENLESS_ROOT/adapters/tokenless"
assert_eq "install with a bundled adapter payload exits 0" "$RUN_STATUS" "0"
S12_ADAPTERS="$TEST_DIR/adapter-lifecycle/home/.local/share/anolisa/adapters/tokenless"
assert_file "postinstall placed the real qwencode adapter" "$S12_ADAPTERS/qwencode/scripts/uninstall.sh"

# Enable: run the repository's own adapter install script, which links the
# extension into the framework home.
S12_HOME="$TEST_DIR/adapter-lifecycle/home"
RUN_OUTPUT="$(
  env -i \
    PATH="$STUB_DIR:/usr/local/bin:/usr/bin:/bin" \
    HOME="$S12_HOME" \
    SHELL=/bin/bash \
    REAL_INSTALL_BIN="$REAL_INSTALL_BIN" \
    REAL_LN_BIN="$REAL_LN_BIN" \
    bash "$S12_ADAPTERS/qwencode/scripts/install.sh" 2>&1
)" && RUN_STATUS=0 || RUN_STATUS=$?
assert_eq "adapter enable exits 0" "$RUN_STATUS" "0"
S12_LINK="$S12_HOME/.qwen/extensions/tokenless"
assert_file "enable linked the extension into the framework home" "$S12_LINK"
assert_eq "the framework link points into the adapter tree" \
  "$(readlink "$S12_LINK")" "$S12_ADAPTERS/qwencode"

run_script "$UNINSTALL_SH" adapter-lifecycle -- "--dry-run"
assert_eq "dry run exits 0" "$RUN_STATUS" "0"
assert_contains "dry run announces the deregistration" "$RUN_OUTPUT" \
  "[dry-run] would deregister the qwencode adapter"
assert_file "dry run keeps the framework link" "$S12_LINK"
assert_file "dry run keeps the adapter tree" "$S12_ADAPTERS/qwencode/scripts/uninstall.sh"

run_script "$UNINSTALL_SH" adapter-lifecycle
assert_eq "uninstall after enable exits 0" "$RUN_STATUS" "0"
assert_contains "deregisters the framework adapter" "$RUN_OUTPUT" "Deregistered the qwencode adapter"
assert_no_file "leaves no dangling framework link behind" "$S12_LINK"
assert_no_file "removes the adapter resources after deregistering" "$S12_ADAPTERS"
assert_no_file "removes the npm-owned CLI link" "$TEST_DIR/adapter-lifecycle/home/.local/bin/tokenless"

# =============================================================================
# Scenario 13 — re-running must not stack PATH entries uninstall.sh cannot strip
# =============================================================================
run_script "$INSTALL_SH" path-idempotent "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "first install exits 0" "$RUN_STATUS" "0"
S13_RC="$TEST_DIR/path-idempotent/home/.bashrc"
run_script "$INSTALL_SH" path-idempotent "TOKENLESS_VERSION=$FAKE_VERSION"
assert_eq "second install exits 0" "$RUN_STATUS" "0"
assert_eq "the rc file carries exactly one installer marker" \
  "$(grep -cF "# Added by tokenless installer" "$S13_RC")" "1"
assert_contains "the second run reports the entry it already owns" "$RUN_OUTPUT" "already adds"
assert_eq "receipt records the rc file once" \
  "$(sed -n 's/^path_rc_file=//p' "$(receipt_of path-idempotent)" | wc -l | tr -d ' ')" "1"
run_script "$UNINSTALL_SH" path-idempotent
assert_eq "uninstall after the re-run exits 0" "$RUN_STATUS" "0"
assert_not_contains "no PATH entry survives the uninstall" "$(cat "$S13_RC")" "tokenless installer"

echo "install-script test passed"
