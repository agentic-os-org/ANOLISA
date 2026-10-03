#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

failures=0

fail() {
  echo "violation: $*" >&2
  failures=$((failures + 1))
}

source_count() {
  rg -n '^[[:space:]]*#\[(tokio::)?test[\](]' "$1" -g '*.rs' | wc -l | tr -d ' '
}

test_list() {
  cargo test --locked -p "$1" "${@:2}" -- --list 2>/dev/null |
    sed -n 's/: test$//p' |
    sort
}

check_source_floor() {
  local crate="$1"
  local floor="$2"
  local actual
  actual="$(source_count "crates/$crate")"
  echo "$crate source tests: $actual (floor $floor)"
  if [[ "$actual" -lt "$floor" ]]; then
    fail "$crate source inventory fell below $floor to $actual; audit removals before lowering the floor"
  fi
}

check_overlap_ceiling() {
  local package="$1"
  local binary="$2"
  local ceiling="$3"
  local actual
  actual="$(
    comm -12 \
      <(test_list "$package" --lib) \
      <(test_list "$package" --bin "$binary") |
      wc -l |
      tr -d ' '
  )"
  echo "$package exact lib/bin overlap: $actual (ceiling $ceiling)"
  if [[ "$actual" -gt "$ceiling" ]]; then
    fail "$package exact lib/bin overlap increased"
  fi
}

# These are regression floors, not exact snapshots. Feature branches must not
# update them when adding tests; raise them periodically in a dedicated change.
check_source_floor cosh-types 24
check_source_floor cosh-platform 279
check_source_floor cosh-cli 75
check_source_floor cosh-core 696
check_source_floor cosh-shell 2683

ignored_count="$(rg -n '^[[:space:]]*#\[ignore' crates -g '*.rs' | wc -l | tr -d ' ')"
echo "ignored tests: $ignored_count (ceiling 3)"
if [[ "$ignored_count" -gt 3 ]]; then
  fail "ignored test inventory increased above 3 to $ignored_count"
fi

check_overlap_ceiling cosh-core cosh-core 4
check_overlap_ceiling cosh-shell cosh-shell 696

if [[ "$failures" -ne 0 ]]; then
  echo "test inventory audit failed with $failures violation(s)" >&2
  exit 1
fi

echo "test inventory audit passed"
"$repo_root/scripts/check-test-necessity.sh"
