#!/usr/bin/env bash
# Documentation lint — enforces specs/documentation-standard.md:
#   1. Chinese docs must use the `_zh.md` suffix (no _CN/_cn/-CN/.zh variants).
#   2. User, developer, and contribution guides must mirror en/ and zh/ trees.
#
# Files awaiting translation may be listed in .github/docs-lint-exemptions.txt
# (paths relative to docs/user-guide/<lang>/, one per line, `#` for comments).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

fail=0

# --- 1. Bilingual naming convention -----------------------------------------
bad=$(git ls-files '*.md' | grep -E '(_CN|_cn|-CN|-cn)\.md$|\.zh\.md$' || true)
if [ -n "$bad" ]; then
  echo "✗ Illegal Chinese doc naming — rename to *_zh.md (see specs/documentation-standard.md §1):"
  echo "$bad" | sed 's/^/    /'
  fail=1
else
  echo "✓ Naming convention OK"
fi

# --- 2. en/zh tree parity for docs/user-guide --------------------------------
exemptions_file=".github/docs-lint-exemptions.txt"
exemptions=$(grep -v '^\s*#' "$exemptions_file" 2>/dev/null | grep -v '^\s*$' || true)

list_tree() {
  (cd "docs/user-guide/$1" && find . -name '*.md' | sort)
}

filter_exempt() {
  if [ -n "$exemptions" ]; then
    grep -Fxv -f <(echo "$exemptions") || true
  else
    cat
  fi
}

en_tree=$(list_tree en | filter_exempt)
zh_tree=$(list_tree zh | filter_exempt)

if [ "$en_tree" != "$zh_tree" ]; then
  echo "✗ docs/user-guide en/zh trees are not mirrored (spec §4.6):"
  diff <(echo "$en_tree") <(echo "$zh_tree") | sed 's/^/    /' || true
  echo "  (files pending translation can be exempted in $exemptions_file)"
  fail=1
else
  echo "✓ en/zh tree parity OK"
fi

for directory in docs/developer-guide contribute; do
  en_tree=$(cd "$directory/en" && find . -name '*.md' | sort)
  zh_tree=$(cd "$directory/zh" && find . -name '*.md' | sort)
  if [ "$en_tree" != "$zh_tree" ]; then
    echo "✗ $directory en/zh trees are not mirrored:"
    diff <(echo "$en_tree") <(echo "$zh_tree") | sed 's/^/    /' || true
    fail=1
  else
    echo "✓ $directory en/zh tree parity OK"
  fi
done

exit $fail
