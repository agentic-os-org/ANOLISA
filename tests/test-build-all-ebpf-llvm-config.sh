#!/usr/bin/env bash
set -euo pipefail

# The eBPF dependency check must accept a versioned llvm-config-NN as
# evidence that llvm is installed: `command -v "llvm-config-*"` never
# matches anything because command -v does not glob its argument.

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/.." && pwd)"

# build-all.sh guards main when sourced, which lets this test exercise
# check_ebpf_deps with a mocked PATH without building components or
# mutating host package state.
# shellcheck source=../scripts/build-all.sh
source "$PROJECT_ROOT/scripts/build-all.sh"

TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

fail() {
    echo "not ok - $*" >&2
    return 1
}

# Run check_ebpf_deps with $1 as the only PATH entry and echo the
# "Missing eBPF packages:" list (empty when nothing was reported missing).
missing_list() {
    local mock_bin="$1"
    (
        PATH="$mock_bin"
        # shellcheck disable=SC2034  # consumed by check_ebpf_deps
        PKG_BASE="rpm"
        # shellcheck disable=SC2034  # consumed by check_ebpf_deps
        INSTALL_DEPS=false
        check_ebpf_deps
    ) 2>/dev/null | sed -n 's/^.*Missing eBPF packages: //p'
}

# Mock PATH holding only clang and a versioned llvm-config.
install -d "$TEST_TMP/bin-versioned"
printf '#!/bin/sh\nexit 0\n' > "$TEST_TMP/bin-versioned/clang"
printf '#!/bin/sh\nexit 0\n' > "$TEST_TMP/bin-versioned/llvm-config-15"
chmod 0755 "$TEST_TMP/bin-versioned/clang" "$TEST_TMP/bin-versioned/llvm-config-15"

# Mock PATH holding clang and a plain (unversioned) llvm-config.
install -d "$TEST_TMP/bin-plain"
printf '#!/bin/sh\nexit 0\n' > "$TEST_TMP/bin-plain/clang"
printf '#!/bin/sh\nexit 0\n' > "$TEST_TMP/bin-plain/llvm-config"
chmod 0755 "$TEST_TMP/bin-plain/clang" "$TEST_TMP/bin-plain/llvm-config"

# Mock PATH holding only clang: no llvm-config of any name.
install -d "$TEST_TMP/bin-none"
printf '#!/bin/sh\nexit 0\n' > "$TEST_TMP/bin-none/clang"
chmod 0755 "$TEST_TMP/bin-none/clang"

LIST_VERSIOND="$(missing_list "$TEST_TMP/bin-versioned")"
if grep -qw llvm <<<"$LIST_VERSIOND"; then
    fail "versioned llvm-config-15 must not be reported as missing"
fi

LIST_PLAIN="$(missing_list "$TEST_TMP/bin-plain")"
if grep -qw llvm <<<"$LIST_PLAIN"; then
    fail "plain llvm-config must not be reported as missing"
fi

LIST_NONE="$(missing_list "$TEST_TMP/bin-none")"
grep -qw llvm <<<"$LIST_NONE" || fail "absent llvm-config must be reported as missing"

echo "ok - versioned llvm-config-NN satisfies the eBPF llvm dependency check"
