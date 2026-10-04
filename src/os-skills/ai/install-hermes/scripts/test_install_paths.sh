#!/usr/bin/env bash
# Check option precedence without running the installer or writing files.
set -euo pipefail

installer="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/install.sh"

check_path() {
    local expected="$1" actual
    shift
    actual="$(bash -c '
        source <(sed -n "1,/^# ============================================================================/p" "$0")
        printf "%s" "$INSTALL_DIR"
    ' "$installer" "$@")"
    if [ "$actual" != "$expected" ]; then
        printf 'expected install path %s, got %s\n' "$expected" "$actual" >&2
        return 1
    fi
}

check_path "/e/hermes-data/hermes-agent" --hermes-home /e/hermes-data
check_path "/e/explicit" --hermes-home /e/hermes-data --dir /e/explicit
check_path "/e/explicit" --dir /e/explicit --hermes-home /e/hermes-data
(
    export HERMES_INSTALL_DIR=/e/from-env
    check_path "/e/from-env" --hermes-home /e/hermes-data
)
printf '4 install-path checks passed\n'
