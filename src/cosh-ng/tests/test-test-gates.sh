#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/cosh-test-gates-contract.XXXXXX")"
trap 'rm -rf "$temp_root"' EXIT
mkdir "$temp_root/bin"
cat >"$temp_root/bin/cargo" <<'PY'
#!/usr/bin/env python3
import os
import sys

if '--list' in sys.argv:
    mode = os.environ['COSH_GATE_FIXTURE_MODE']
    names = [
        'recommendation::personal_analysis_runtime::tests::gate4_real_core_uses_one_bare_toolless_request_without_touching_foreground_state',
        'heavy::raw_relay_host_runs_fullscreen_programs_and_keeps_shell_usable',
        'native::raw_cli_zsh_native_path_slash_and_tab_stay_in_shell',
    ]
    for name in names[1:] if mode == 'missing' else names:
        print(f'{name}: test')
    for index in range(20000):
        print(f'zzzz_inventory_padding_{index:08d}_xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx: test')
    if mode == 'failed':
        sys.exit(7)
PY
chmod +x "$temp_root/bin/cargo"

if ! PATH="$temp_root/bin:$PATH" COSH_GATE_FIXTURE_MODE=complete \
    bash "$repo_root/scripts/run-test-gates.sh" heavy >"$temp_root/complete.log" 2>&1; then
    cat "$temp_root/complete.log" >&2
    exit 1
fi
for mode in missing failed; do
    if PATH="$temp_root/bin:$PATH" COSH_GATE_FIXTURE_MODE="$mode" \
        bash "$repo_root/scripts/run-test-gates.sh" heavy >"$temp_root/$mode.log" 2>&1; then
        printf 'FAIL: %s inventory unexpectedly passed\n' "$mode" >&2
        exit 1
    fi
    grep -Fq 'required test is missing from Cargo inventory:' "$temp_root/$mode.log"
done
printf 'test gate contract tests passed\n'
