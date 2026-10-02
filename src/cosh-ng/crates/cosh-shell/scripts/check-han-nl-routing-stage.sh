#!/usr/bin/env bash
set -euo pipefail

stage="${1:-}"
allow_capability_skip="${2:-}"
case "$stage" in
  C0|C1|C2|C3|C4) ;;
  *) echo "usage: $0 C0|C1|C2|C3|C4 [--allow-capability-skip]" >&2; exit 2 ;;
esac
if [[ -n "$allow_capability_skip" && "$allow_capability_skip" != "--allow-capability-skip" ]]; then
  echo "usage: $0 C0|C1|C2|C3|C4 [--allow-capability-skip]" >&2
  exit 2
fi

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../../.." && pwd)"
cd "$repo_root"

shell_host_tests="$(cargo test --locked -p cosh-shell --test shell_host -- --list 2>/dev/null)"
raw_cli_tests=""
lib_tests="$(cargo test --locked -p cosh-shell --lib -- --list 2>/dev/null)"

require_selector() {
  local inventory="$1"
  local selector="$2"
  [[ "$inventory" == *"$selector"* ]] || {
    echo "missing required test selector: $selector" >&2
    exit 1
  }
}

require_shell_capabilities() {
  local bash_major
  bash_major="$(bash -c 'printf %s "${BASH_VERSINFO[0]:-0}"')"
  if (( bash_major < 4 )) || ! command -v zsh >/dev/null 2>&1; then
    if [[ "$allow_capability_skip" == "--allow-capability-skip" ]]; then
      echo "SKIP: $stage requires Bash >= 4 and Zsh" >&2
      return 1
    fi
    echo "$stage requires Bash >= 4 and Zsh" >&2
    exit 1
  fi
}

run_c0() {
  require_selector "$lib_tests" "path_prompt_tests::path_provably_missing_requires_enoent_proof"
  require_selector "$shell_host_tests" "marker::shell_host_bash_stale_history_guard_still_intercepts_deduped_repeats"
  require_selector "$shell_host_tests" "relay::raw_relay_bash_invalid_utf8_never_enters_event_provenance"
  require_selector "$lib_tests" "slash::registry::tests::shell_marker_exact_tokens_match_registry"
  cargo test --locked -p cosh-shell --lib path_prompt_tests::path_provably_missing_requires_enoent_proof -- --exact
  cargo test --locked -p cosh-shell --test shell_host marker::shell_host_bash_stale_history_guard_still_intercepts_deduped_repeats -- --exact
  cargo test --locked -p cosh-shell --test shell_host relay::raw_relay_bash_invalid_utf8_never_enters_event_provenance -- --exact
  cargo test --locked -p cosh-shell --lib shell_marker_exact_tokens_match_registry
}

run_prefixed_stage() {
  local prefix="$1"
  shift
  require_shell_capabilities || return 0
  for category in "$@"; do
    require_selector "$shell_host_tests" "${prefix}${category}"
  done
  cargo test --locked -p cosh-shell --test shell_host "$prefix" -- --test-threads=1
}

run_c0
[[ "$stage" == C0 ]] && exit 0
run_prefixed_stage routing_c1_ classifier cnf missing_path stale_history tier_b_side_effect zsh_glob_qualifier valid_han_command
[[ "$stage" == C1 ]] && exit 0
run_prefixed_stage routing_c2_ matcher_table bash_quote_cnf zsh_quote_cnf expansion_drift nested_provenance delegate_unsupported valid_quoted_command
[[ "$stage" == C2 ]] && exit 0

require_shell_capabilities || exit 0
raw_cli_tests="$(cargo test --locked -p cosh-shell --test raw_cli -- --list 2>/dev/null)"
for category in typed_passthrough wrapped_paste unwrapped_paste mirror eof_partial_line eof_session_shutdown eof_submitted_no_drift eof_error driver_result signal_status valid_slash provider_no_regression explicit_draft; do
  if [[ "$shell_host_tests" != *"routing_c3_${category}"* && "$raw_cli_tests" != *"routing_c3_${category}"* ]]; then
    echo "missing required test selector: routing_c3_${category}" >&2
    exit 1
  fi
done
cargo test --locked -p cosh-shell --test shell_host routing_c3_ -- --test-threads=1
cargo test --locked -p cosh-shell --test raw_cli routing_c3_ -- --test-threads=1
cargo test --locked -p cosh-shell --test raw_cli agent_input -- --test-threads=1
[[ "$stage" == C3 ]] && exit 0

for category in per_shell_registry zsh_stubs bash_route zsh_rust_route draft_grammar_no_drift history_privacy; do
  if [[ "$lib_tests" != *"routing_c4_${category}"* && "$shell_host_tests" != *"routing_c4_${category}"* ]]; then
    echo "missing required test selector: routing_c4_${category}" >&2
    exit 1
  fi
done
cargo test --locked -p cosh-shell --lib routing_c4_
cargo test --locked -p cosh-shell --test shell_host routing_c4_ -- --test-threads=1
