#!/usr/bin/env bash
# The installer help must not promise an index cache that no CLI path
# uses, and must scope index re-fetching to the raw backend resolve path
# — mirroring README.md ("The raw backend re-fetches the distribution
# index on every resolve") and repo_config.rs's inert-cache contract.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install-anolisa.sh"

HELP="$(bash "$INSTALLER" --help)"

assert_contains() {
    if [[ "$HELP" != *"$1"* ]]; then
        printf 'ERROR: --help is missing expected wording: %s\n' "$1" >&2
        exit 1
    fi
}

assert_absent() {
    if [[ "$HELP" == *"$1"* ]]; then
        printf 'ERROR: --help still contains stale wording: %s\n' "$1" >&2
        exit 1
    fi
}

assert_contains "the raw backend re-fetches the index"
assert_contains "on every resolve"
assert_absent "downloads and caches"
assert_absent "caches it"
printf 'install help index wording OK\n'
