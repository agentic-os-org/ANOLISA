#!/usr/bin/env bash
set -u

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
crate_dir="$(cd "$script_dir/.." && pwd)"
repo_root="$(cd "$crate_dir/../.." && pwd)"

cd "$repo_root" || exit 2

if ! command -v rg >/dev/null 2>&1; then
  echo "error: rg is required" >&2
  exit 2
fi

category_for_path() {
  case "$1" in
    crates/cosh-shell/tests/raw_cli.rs|crates/cosh-shell/tests/raw_cli/*) echo "raw_cli" ;;
    crates/cosh-shell/tests/shell_host.rs|crates/cosh-shell/tests/shell_host/*|crates/cosh-shell/tests/support/shell_host.rs) echo "shell_host" ;;
    crates/cosh-shell/tests/protocol.rs|crates/cosh-shell/tests/protocol/*|crates/cosh-shell/tests/control_protocol.rs|crates/cosh-shell/tests/support/control_protocol.rs) echo "protocol" ;;
    crates/cosh-shell/tests/logic.rs|crates/cosh-shell/tests/logic/*|crates/cosh-shell/tests/mvp_loop.rs) echo "logic" ;;
    crates/cosh-shell/src/*) echo "unit_or_component" ;;
    *) echo "uncategorized" ;;
  esac
}

owner_for_path() {
  local path="$1"
  if [[ "$path" == crates/cosh-shell/src/* ]]; then
    local rest="${path#crates/cosh-shell/src/}"
    echo "${rest%%/*}" | sed 's/\.rs$//'
    return
  fi
  case "$path" in
    crates/cosh-shell/tests/raw_cli.rs|crates/cosh-shell/tests/raw_cli/*) echo "raw_cli" ;;
    crates/cosh-shell/tests/shell_host.rs|crates/cosh-shell/tests/shell_host/*|crates/cosh-shell/tests/support/shell_host.rs) echo "shell_host" ;;
    crates/cosh-shell/tests/protocol.rs|crates/cosh-shell/tests/protocol/*|crates/cosh-shell/tests/control_protocol.rs|crates/cosh-shell/tests/support/control_protocol.rs) echo "adapter/control_protocol" ;;
    crates/cosh-shell/tests/logic.rs|crates/cosh-shell/tests/logic/*|crates/cosh-shell/tests/mvp_loop.rs) echo "runtime" ;;
    *) echo "unknown" ;;
  esac
}

target_for_path() {
  case "$1" in
    crates/cosh-shell/tests/raw_cli.rs|crates/cosh-shell/tests/raw_cli/*) echo "raw_cli" ;;
    crates/cosh-shell/tests/shell_host.rs|crates/cosh-shell/tests/shell_host/*|crates/cosh-shell/tests/support/shell_host.rs) echo "shell_host" ;;
    crates/cosh-shell/tests/protocol.rs|crates/cosh-shell/tests/protocol/*) echo "protocol" ;;
    crates/cosh-shell/tests/control_protocol.rs|crates/cosh-shell/tests/support/control_protocol.rs) echo "control_protocol_legacy" ;;
    crates/cosh-shell/tests/logic.rs|crates/cosh-shell/tests/logic/*) echo "logic" ;;
    crates/cosh-shell/tests/mvp_loop.rs) echo "mvp_loop_legacy" ;;
    crates/cosh-shell/src/*) echo "crate_unit_target" ;;
    *) echo "unknown" ;;
  esac
}

decision_for_path() {
  case "$1" in
    crates/cosh-shell/tests/raw_cli.rs) echo "split_into_tests/raw_cli_modules;make_file_aggregator_only" ;;
    crates/cosh-shell/tests/raw_cli/*) echo "keep_under_tests/raw_cli;move_shared_helpers_to_support" ;;
    crates/cosh-shell/tests/shell_host.rs) echo "split_into_tests/shell_host_modules;make_file_aggregator_only" ;;
    crates/cosh-shell/tests/shell_host/*) echo "keep_under_tests/shell_host;move_shared_helpers_to_support" ;;
    crates/cosh-shell/tests/protocol.rs) echo "keep_aggregator_only" ;;
    crates/cosh-shell/tests/protocol/*) echo "keep_under_tests/protocol" ;;
    crates/cosh-shell/tests/control_protocol.rs) echo "move_or_alias_to_protocol_target" ;;
    crates/cosh-shell/tests/logic.rs) echo "keep_aggregator_only" ;;
    crates/cosh-shell/tests/logic/*) echo "keep_under_tests/logic" ;;
    crates/cosh-shell/tests/mvp_loop.rs) echo "move_or_alias_to_logic_target" ;;
    crates/cosh-shell/src/*/tests.rs|crates/cosh-shell/src/*/tests/*|crates/cosh-shell/src/*/runtime_tests/*|crates/cosh-shell/src/*/hook_tests.rs) echo "keep_if_pure_logic_or_light_component;split_large_test_module" ;;
    crates/cosh-shell/src/*) echo "keep_in_src_only_if_no_binary_pty_real_home_provider_network" ;;
    *) echo "needs_manual_classification" ;;
  esac
}

gate_for_path() {
  case "$1" in
    crates/cosh-shell/tests/raw_cli.rs|crates/cosh-shell/tests/raw_cli/*) echo "focused_raw_cli_serial" ;;
    crates/cosh-shell/tests/shell_host.rs|crates/cosh-shell/tests/shell_host/*|crates/cosh-shell/tests/support/shell_host.rs) echo "focused_shell_host_serial" ;;
    crates/cosh-shell/tests/protocol.rs|crates/cosh-shell/tests/protocol/*|crates/cosh-shell/tests/control_protocol.rs|crates/cosh-shell/tests/support/control_protocol.rs) echo "default_protocol" ;;
    crates/cosh-shell/tests/logic.rs|crates/cosh-shell/tests/logic/*|crates/cosh-shell/tests/mvp_loop.rs) echo "default_logic" ;;
    crates/cosh-shell/src/*) echo "default_unit" ;;
    *) echo "unknown" ;;
  esac
}

print_test_rows_for_file() {
  local file="$1"
  local category owner target decision gate
  category="$(category_for_path "$file")"
  owner="$(owner_for_path "$file")"
  target="$(target_for_path "$file")"
  decision="$(decision_for_path "$file")"
  gate="$(gate_for_path "$file")"

  awk -v file="$file" \
      -v category="$category" \
      -v owner="$owner" \
      -v target="$target" \
      -v decision="$decision" \
      -v gate="$gate" '
    /^[[:space:]]*#\[(tokio::test|test)\]/ {
      attr_line = NR
      next
    }
    attr_line && /^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+[A-Za-z_][A-Za-z0-9_]*/ {
      name = $0
      sub(/^[[:space:]]*(pub[[:space:]]+)?(async[[:space:]]+)?fn[[:space:]]+/, "", name)
      sub(/\(.*/, "", name)
      printf "%s,%s,%s,%s,%s,%s,%s,%s\n", category, target, file, attr_line, name, owner, gate, decision
      attr_line = 0
    }
  ' "$file"
}

case "${1:-csv}" in
  csv)
    echo "category,target,path,line,test_name,owner,gate,migration_decision"
    while IFS= read -r file; do
      print_test_rows_for_file "$file"
    done < <(rg -l '^[[:space:]]*#\[(tokio::test|test)\]' crates/cosh-shell/src crates/cosh-shell/tests | sort)
    ;;
  summary)
    echo "== test files by category =="
    while IFS= read -r file; do
      count="$(rg -n '^[[:space:]]*#\[(tokio::test|test)\]' "$file" | wc -l | tr -d ' ')"
      printf "%5s  %-18s  %s\n" "$count" "$(category_for_path "$file")" "$file"
    done < <(rg -l '^[[:space:]]*#\[(tokio::test|test)\]' crates/cosh-shell/src crates/cosh-shell/tests | sort)
    echo
    echo "== totals by category =="
    "$0" csv | awk -F, 'NR > 1 { count[$1]++ } END { for (category in count) printf "%5d  %s\n", count[category], category }' | sort -nr
    echo
    echo "== source-test files requiring src-vs-migrate review =="
    "$0" csv | awk -F, 'NR > 1 && $3 ~ /^crates\/cosh-shell\/src\// { count[$3]++ } END { for (file in count) printf "%5d  %s\n", count[file], file }' | sort -nr
    ;;
  *)
    echo "usage: $0 [csv|summary]" >&2
    exit 2
    ;;
esac
