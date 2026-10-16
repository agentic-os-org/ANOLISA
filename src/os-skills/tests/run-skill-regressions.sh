#!/usr/bin/env bash
# Run every skill regression that lives next to the script it covers.
#
# Skills keep their regressions beside their scripts and run them directly
# (`others/xlsx/scripts/test_style_audit_sheets.py`,
# `ai/install-qwenpaw/scripts/test-setup-template-substitution.sh`, ...), which
# means nothing executes them unless something looks for them — a broken
# installer would ship with a green package job. The package workflow calls this
# runner for exactly that reason; discovery keeps it from needing an edit per
# skill, and `test-module.sh`-style system scripts (test- rather than test_) are
# deliberately out of reach.
#
# Usage: bash src/os-skills/tests/run-skill-regressions.sh
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
status=0

run() {  # <relative path> <interpreter> <absolute path>
  echo "=== $1"
  "$2" "$3" || { echo "FAILED: $1" >&2; status=1; }
}

while IFS= read -r script; do
  relative="${script#"$ROOT"/}"
  case "$script" in
    *.py) run "$relative" python3 "$script" ;;
    *.sh) run "$relative" bash "$script" ;;
  esac
done < <(find "$ROOT" \( -path '*/scripts/test_*.sh' -o -path '*/scripts/test_*.py' \) | sort)

if [ "$status" -ne 0 ]; then
  echo "skill regressions failed" >&2
  exit 1
fi
echo "all skill regressions passed"
