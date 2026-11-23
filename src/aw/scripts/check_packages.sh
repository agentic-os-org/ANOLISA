#!/usr/bin/env bash
# Exercise the Python-free installer and preserve bounded failure diagnostics.
set -euo pipefail

output=${1:?usage: check_packages.sh EVIDENCE_DIRECTORY}
mkdir -p "$output"
output=$(cd "$output" && pwd)

snapshot() {
    local path value code
    date -u +%FT%TZ
    uname -a
    printf 'test_threads=2\n'
    getconf _NPROCESSORS_ONLN
    cat /proc/self/cgroup
    for path in /sys/fs/cgroup/{cpu.max,cpu.stat,cpu.pressure,memory.max,memory.events,io.stat,io.pressure}; do
        printf '\n%s\n' "$path"
        # A readable cgroup file can still reject reads when PSI is unavailable.
        if value=$(cat "$path" 2>&1); then
            printf '%s\n' "$value"
        else
            code=$?
            printf 'unavailable (exit_code=%s): %s\n' "$code" "$value"
        fi
    done
    df -T "$output" /tmp
}

run_tests() {
    local name=$1 started=$SECONDS code
    shift
    snapshot > "$output/$name-before.log"
    printf 'command:' > "$output/$name-command.log"
    printf ' %q' "$@" >> "$output/$name-command.log"
    printf '\ndeadline_seconds=600\n' >> "$output/$name-command.log"
    # Preserve the failing status through tee. Each CLI fixture owns its children;
    # the outer deadline also bounds a stuck compiler or test harness.
    set +e
    timeout --signal=INT --kill-after=15s 600 "$@" 2>&1 | tee "$output/$name.log"
    local statuses=("${PIPESTATUS[@]}")
    set -e
    code=${statuses[0]}
    if (( code == 0 && statuses[1] != 0 )); then code=${statuses[1]}; fi
    snapshot > "$output/$name-after.log"
    printf 'elapsed_seconds=%s\nexit_code=%s\n' "$((SECONDS-started))" "$code" >> "$output/$name-command.log"
    return "$code"
}

# Nested job containers can expose the host CPU count above their runner quota.
# Keep real installed executables and bound parallel checksum/transaction work.
run_tests package cargo test --locked --manifest-path src/aw/Cargo.toml -p aw-package -- --test-threads=2
run_tests cli cargo test --locked --manifest-path src/aw/Cargo.toml -p aw-cli \
    --test installation --test config_edit_cli --test standalone --test packages -- --test-threads=2 --show-output
