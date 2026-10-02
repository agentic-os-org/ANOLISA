#!/usr/bin/env bash
set -euo pipefail

# Keep newly introduced Gateway production modules reviewable. Test modules are
# excluded, while every production fragment remains counted even when its owner
# assembles that fragment with `include!`.
export LC_ALL=C

workspace_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$workspace_root"

warning_lines=600
maximum_lines=700
waiver_absolute_ceiling=1500
waiver_file="scripts/large-production-file-waivers.txt"
failures=0

declare -A waiver_limits=()
declare -A waiver_metadata=()
declare -A production_paths=()

# Optional waiver rows use:
# path|max_lines|owner|tracking_issue|why_splitting_breaks_an_invariant
# A waiver is a non-growing ceiling, not permission to keep expanding a file.
if [[ -f "$waiver_file" ]]; then
  while IFS='|' read -r path limit owner tracking reason; do
    [[ -n "$path" && "${path:0:1}" != "#" ]] || continue
    if [[ ! "$limit" =~ ^[0-9]+$ || "$limit" -lt "$maximum_lines" || \
      "$limit" -gt "$waiver_absolute_ceiling" || \
      -z "$owner" || -z "$tracking" || -z "$reason" ]]; then
      echo "invalid large-file waiver: $path" >&2
      failures=$((failures + 1))
      continue
    fi
    if [[ -n "${waiver_limits[$path]:-}" ]]; then
      echo "duplicate large-file waiver: $path" >&2
      failures=$((failures + 1))
      continue
    fi
    waiver_limits["$path"]="$limit"
    waiver_metadata["$path"]="$owner|$tracking|$reason"
  done <"$waiver_file"
fi

mapfile -d '' production_files < <(
  find crates/cosh-gateway/src crates/cosh-gateway-app/src \
    crates/cosh-gateway-contracts/src \
    -type f -name '*.rs' \
    ! -path '*/tests/*' \
    ! -name 'tests.rs' \
    ! -name '*_tests.rs' \
    -print0
)

mapfile -d '' source_symlinks < <(
  find crates/cosh-gateway/src crates/cosh-gateway-app/src \
    crates/cosh-gateway-contracts/src \
    -type l -name '*.rs' -print0
)

for path in "${source_symlinks[@]}"; do
  echo "violation: Rust source symlink is outside the layout audit: $path" >&2
  failures=$((failures + 1))
done

for path in "${production_files[@]}"; do
  production_paths["$path"]=1
  lines="$(wc -l <"$path" | tr -d ' ')"
  waiver_limit="${waiver_limits[$path]:-}"

  if (( lines >= maximum_lines )); then
    if [[ -z "$waiver_limit" ]]; then
      echo "violation: $path has $lines lines (limit: $maximum_lines)" >&2
      failures=$((failures + 1))
    elif (( lines > waiver_limit )); then
      echo "violation: $path has $lines lines (waiver ceiling: $waiver_limit)" >&2
      failures=$((failures + 1))
    else
      echo "waived: $path has $lines lines (ceiling: $waiver_limit; ${waiver_metadata[$path]})"
    fi
  elif (( lines >= warning_lines )); then
    echo "warning: $path has $lines lines (failure at: $maximum_lines)"
  elif [[ -n "$waiver_limit" ]]; then
    echo "violation: stale waiver for $path; file is below $maximum_lines lines" >&2
    failures=$((failures + 1))
  fi
done

for path in "${!waiver_limits[@]}"; do
  if [[ ! -f "$path" ]]; then
    echo "violation: waiver references missing file $path" >&2
    failures=$((failures + 1))
  elif [[ -z "${production_paths[$path]:-}" ]]; then
    echo "violation: waiver references excluded production path $path" >&2
    failures=$((failures + 1))
  fi
done

if (( failures > 0 )); then
  echo "Gateway source layout audit failed with $failures violation(s)" >&2
  exit 1
fi

echo "Gateway source layout audit passed"
