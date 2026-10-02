#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

list_tests() {
  cargo test --locked -p "$1" "${@:2}" -- --list 2>/dev/null |
    sed -n 's/: test$//p' |
    sort
}

require_test() {
  local package="$1"
  local test_name="$2"
  shift 2
  if ! list_tests "$package" "$@" | grep -Fxq "$test_name"; then
    echo "required test is missing from Cargo inventory: $test_name" >&2
    exit 1
  fi
}

run_canonical_units() {
  local package="$1"
  local binary="$2"
  local test_threads="${3:-4}"
  local work_dir
  local lib_tests
  local bin_tests
  local overlap
  local -a skip_args=()

  work_dir="$(mktemp -d "${TMPDIR:-/tmp}/cosh-test-gates.XXXXXX")"
  lib_tests="$work_dir/lib-tests"
  bin_tests="$work_dir/bin-tests"
  overlap="$work_dir/overlap"
  trap 'rm -rf "$work_dir"' RETURN

  list_tests "$package" --lib >"$lib_tests"
  list_tests "$package" --bin "$binary" >"$bin_tests"
  comm -12 "$lib_tests" "$bin_tests" >"$overlap"

  cargo test --locked -p "$package" --lib -- --test-threads="$test_threads"
  while IFS= read -r test_name; do
    [[ -n "$test_name" ]] && skip_args+=(--skip "$test_name")
  done <"$overlap"
  cargo test --locked -p "$package" --bin "$binary" -- \
    --test-threads="$test_threads" "${skip_args[@]}"

  echo "$package: skipped $(wc -l <"$overlap" | tr -d ' ') exact lib/bin duplicate executions"
}

run_core_integrations() {
  local found=0
  local target

  while IFS= read -r target; do
    found=1
    cargo test --locked -p cosh-core --test "$target"
  done < <(
    cargo metadata --locked --no-deps --format-version 1 |
      python3 -c '
import json
import sys

metadata = json.load(sys.stdin)
package = next(package for package in metadata["packages"] if package["name"] == "cosh-core")
for target in sorted(target["name"] for target in package["targets"] if "test" in target["kind"]):
    print(target)
'
  )

  if [[ "$found" -eq 0 ]]; then
    echo "cosh-core has no Cargo integration test targets" >&2
    exit 1
  fi
}

run_shell_integrations() {
  cargo test --locked -p cosh-shell --test logic
  cargo test --locked -p cosh-shell --test protocol -- --test-threads=4
  # raw_cli concurrency is governed by the in-tree shared/exclusive gate
  # (RAW_CLI_SHARED_PARALLELISM / COSH_RAW_CLI_TEST_PARALLELISM); give
  # libtest enough threads so that gate, not the runner, is the limiter.
  cargo test --locked -p cosh-shell --test raw_cli -- --test-threads=16
  cargo test --locked -p cosh-shell --test shell_host -- --test-threads=1
}

run_heavy() {
  local core_test="recommendation::personal_analysis_runtime::tests::gate4_real_core_uses_one_bare_toolless_request_without_touching_foreground_state"
  local host_test="heavy::raw_relay_host_runs_fullscreen_programs_and_keeps_shell_usable"
  local native_test="native::raw_cli_zsh_native_path_slash_and_tab_stay_in_shell"

  require_test cosh-shell "$core_test" --bin cosh-shell
  require_test cosh-shell "$host_test" --test shell_host
  require_test cosh-shell "$native_test" --test raw_cli
  cargo build --locked -p cosh-core
  cargo test --locked -p cosh-shell --bin cosh-shell \
    "$core_test" \
    -- --exact --ignored --test-threads=1
  cargo test --locked -p cosh-shell --test shell_host \
    "$host_test" \
    -- --exact --ignored --test-threads=1
  cargo test --locked -p cosh-shell --test raw_cli \
    "$native_test" \
    -- --exact --ignored --test-threads=1
}

run_raw_packaging() {
  if ! command -v shellcheck >/dev/null 2>&1; then
    echo "shellcheck is required by the raw packaging gate" >&2
    return 1
  fi
  shellcheck \
    packaging/raw/package.sh \
    packaging/raw/assets/bin/cosh \
    packaging/raw/assets/bin/cosh-switch \
    scripts/managed-task-dev.sh \
    tests/test-managed-task-dev.sh \
    tests/test-package-raw.sh
  bash tests/test-managed-task-dev.sh
  bash tests/test-package-raw.sh
}

run_rpm_packaging() {
  if ! command -v shellcheck >/dev/null 2>&1; then
    echo "shellcheck is required by the rpm packaging gate" >&2
    return 1
  fi
  shellcheck tests/test-package-rpm.sh
  bash tests/test-package-rpm.sh
}

case "${1:-all}" in
  fast)
    scripts/check-test-inventory.sh
    scripts/check-source-layout.sh
    crates/cosh-shell/scripts/check-layout.sh
    run_raw_packaging
    run_rpm_packaging
    cargo test --locked --workspace --exclude cosh-core --exclude cosh-shell
    run_canonical_units cosh-core cosh-core
    run_canonical_units cosh-shell cosh-shell 1
    cargo test --locked -p cosh-shell --test logic
    cargo test --locked -p cosh-shell --test protocol -- --test-threads=4
    ;;
  integration)
    run_core_integrations
    run_shell_integrations
    ;;
  heavy)
    run_heavy
    ;;
  all)
    scripts/check-test-inventory.sh
    scripts/check-source-layout.sh
    crates/cosh-shell/scripts/check-layout.sh
    run_raw_packaging
    run_rpm_packaging
    cargo test --locked --workspace --exclude cosh-core --exclude cosh-shell
    run_canonical_units cosh-core cosh-core
    run_core_integrations
    run_canonical_units cosh-shell cosh-shell 1
    run_shell_integrations
    ;;
  *)
    echo "usage: $0 [fast|integration|heavy|all]" >&2
    exit 2
    ;;
esac
