#!/usr/bin/env bash
# Run every skill regression that lives next to the script it covers.
#
# Naming contract for a skill regression — this is the rule the discoverer
# applies, so a name outside it is skipped by CI without a word:
#
#     <skill>/scripts/test_*.sh    <skill>/scripts/test-*.sh
#     <skill>/scripts/test_*.py    <skill>/scripts/test-*.py
#
# Both separators are accepted on purpose: this repository's shell tests mostly
# use a dash (agentsight/scripts/test-package-raw.sh,
# cosh-ng/scripts/test-gateway-containment.sh) while its Python ones use an
# underscore (others/xlsx/scripts/test_style_audit_sheets.py).
#
# A discovered script must be safe on a shared runner: standard library and the
# shell only, no network, no dependence on the working directory (resolve paths
# from ${BASH_SOURCE[0]} / __file__), no writes outside a temp directory, and a
# bounded runtime. Scripts that need a real machine are named in SYSTEM_SCRIPTS
# below and skipped — they belong in the component's own workflow instead.
#
# The runner reports how many regressions it ran and **fails when it found
# none**, because an entry whose discovery has drifted away from the real file
# names still prints success otherwise (review of #7785, round 3).
#
# Usage:
#   bash src/os-skills/tests/run-skill-regressions.sh           # run them
#   bash src/os-skills/tests/run-skill-regressions.sh --list     # list, no run
set -uo pipefail

LIST_ONLY=0
[ "${1:-}" = "--list" ] && LIST_ONLY=1

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
status=0
ran=0

# Scripts that drive a real machine rather than a sandbox. Add a basename here,
# with a comment saying what it needs, instead of loosening the pattern.
SYSTEM_SCRIPTS=("test-module.sh")

is_system_script() {
  local name
  for name in "${SYSTEM_SCRIPTS[@]}"; do
    [ "$(basename "$1")" = "$name" ] && return 0
  done
  return 1
}

run() {  # <relative path> <interpreter> <absolute path>
  echo "=== $1"
  ran=$((ran + 1))
  "$2" "$3" || { echo "FAILED: $1" >&2; status=1; }
}

while IFS= read -r script; do
  relative="${script#"$ROOT"/}"
  if is_system_script "$script"; then
    echo "=== $relative (needs a real machine, skipped)"
    continue
  fi
  if [ "$LIST_ONLY" = "1" ]; then
    echo "$relative"
    ran=$((ran + 1))
    continue
  fi
  case "$script" in
    *.py) run "$relative" python3 "$script" ;;
    *.sh) run "$relative" bash "$script" ;;
  esac
done < <(find "$ROOT" \
  \( -path '*/scripts/test_*.sh' -o -path '*/scripts/test_*.py' \
     -o -path '*/scripts/test-*.sh' -o -path '*/scripts/test-*.py' \) | sort)

if [ "$ran" -eq 0 ]; then
  echo "no skill regressions found under $ROOT — the discovery rule and the file names have drifted apart" >&2
  exit 1
fi

if [ "$LIST_ONLY" = "1" ]; then
  echo "found $ran skill regression(s)"
  exit 0
fi

echo "ran $ran skill regression(s)"
if [ "$status" -ne 0 ]; then
  echo "skill regressions failed" >&2
  exit 1
fi
echo "all skill regressions passed"
