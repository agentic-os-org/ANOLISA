#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/.." && pwd)"
TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

# Exercise the real CLI with no make on PATH. Planning prerequisites must
# work before those prerequisites have been installed.
mkdir "$TEST_TMP/bin"
ln -s "$(command -v dirname)" "$TEST_TMP/bin/dirname"
BASH_BIN="$(command -v bash)"
for component in cosh-ng sec-core; do
    PATH="$TEST_TMP/bin" "$BASH_BIN" "$PROJECT_ROOT/scripts/build-all.sh" \
        --component "$component" --deps-only --dry-run > "$TEST_TMP/plan"
    grep -Fq 'check/install build tools if needed' "$TEST_TMP/plan"
    grep -Fq 'Deps-only mode, skipping build.' "$TEST_TMP/plan"
done
echo 'ok - dependency plans do not require make'

# Real Makefile evaluation still supplies sec-core paths when installing.
for mode in user system; do
    (
        source "$PROJECT_ROOT/scripts/build-all.sh"
        INSTALL_MODE="$mode"
        DRY_RUN=true
        ensure_user_mode
        [[ -z "$SEC_CORE_BIN_DIR" && -z "$SEC_CORE_LIB_DIR" ]]
        install_sec_core > "$TEST_TMP/install-$mode"
        [[ "$SEC_CORE_BIN_DIR" == "$(makefile_var "$PROJECT_ROOT/src/agent-sec-core" system BINDIR)" ]]
        [[ "$SEC_CORE_LIB_DIR" == "$(makefile_var "$PROJECT_ROOT/src/agent-sec-core" "$mode" LIBDIR)" ]]
        [[ -n "$SEC_CORE_BIN_DIR" && -n "$SEC_CORE_LIB_DIR" ]]
    )
done
echo 'ok - sec-core install resolves its real Makefile paths'
