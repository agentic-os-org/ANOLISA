#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install-openclaw.sh — the wrapper ends with
# `exec python3 ... "${args[@]}"` under `set -u`. On bash < 4.4 (macOS
# /bin/bash 3.2, Alinux 3's bash 4.2) expanding an empty array is an
# "unbound variable" error, so the zero-arg invocation — the one that
# should print the installer's usage — died before reaching python3.
#
# Drives the real wrapper with a stubbed python3 that records argv and
# prints a usage line. No dependencies; run directly:
#     bash test-install-openclaw-bash3.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/install-openclaw.sh"

pass=0
fail=0

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

ok_()     { echo "ok - $1"; pass=$((pass + 1)); }
not_ok_() { echo "not ok - $1"; fail=$((fail + 1)); }

check_contains() {  # <name> <file> <needle>
    if grep -q "$3" "$2" 2>/dev/null; then
        ok_ "$1"
    else
        not_ok_ "$1"
    fi
}

check_not_contains() {  # <name> <file> <needle>
    if grep -q "$3" "$2" 2>/dev/null; then
        not_ok_ "$1"
    else
        ok_ "$1"
    fi
}

# A python3 stub: records its argv, answers with a usage line, exit 2.
mkdir -p "$tmp/bin"
cat > "$tmp/bin/python3" <<'STUB'
#!/bin/sh
echo "python3-stub argv: $*" >> "$PY_STUB_RECORD"
echo "usage: install_openclaw.py [options] (stub)"
exit 2
STUB
chmod +x "$tmp/bin/python3"

# run_wrapper <bash-binary> <args...> — leaves output in $tmp/out,
# recorded argv in $tmp/record, exit code in $wrapper_code.
run_wrapper() {
    local sh="$1"
    shift
    : > "$tmp/record"
    PY_STUB_RECORD="$tmp/record" PATH="$tmp/bin:$PATH" \
        "$sh" "$SCRIPT" "$@" > "$tmp/out" 2>&1
    wrapper_code=$?
}

# bash_before_44 <binary> — succeed when the shell is bash < 4.4.
bash_before_44() {
    local v
    v="$("$1" -c 'echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}"' 2>/dev/null)" || return 1
    [ "$(printf '%s\n' "$v" | awk -F. '{print $1 * 100 + $2}')" -lt 404 ]
}

# ---------------------------------------------------------------------------
# Scenario 1: zero arguments on bash < 4.4 must reach python3 (usage output),
# not die with "unbound variable". Skipped when no bash < 4.4 is available.
# ---------------------------------------------------------------------------
OLD_BASH=""
for candidate in /bin/bash bash; do
    command -v "$candidate" >/dev/null 2>&1 || continue
    if bash_before_44 "$(command -v "$candidate")"; then
        OLD_BASH="$(command -v "$candidate")"
        break
    fi
done

if [ -n "$OLD_BASH" ]; then
    echo "# using $OLD_BASH ($("$OLD_BASH" -c 'echo "${BASH_VERSINFO[0]}.${BASH_VERSINFO[1]}"'))"
    run_wrapper "$OLD_BASH"
    echo "# scenario1 exit=$wrapper_code"
    sed 's/^/#     /' "$tmp/out"
    if [ "$wrapper_code" -eq 2 ]; then
        ok_ "zero-arg run reaches python3 (exit 2 from stub, got $wrapper_code)"
    else
        not_ok_ "zero-arg run reaches python3 (got $wrapper_code)"
    fi
    check_contains "usage output shown" "$tmp/out" "usage"
    check_not_contains "no unbound-variable crash" "$tmp/out" "unbound variable"
    check_contains "python3 invoked with the installer script" "$tmp/record" "install_openclaw.py"
else
    echo "ok - # SKIP zero-arg bash<4.4 scenario: no bash < 4.4 found on this host"
    pass=$((pass + 1))
fi

# ---------------------------------------------------------------------------
# Scenario 2: argument translation still works on any bash — --apikey maps
# to --api-key, aliyun provider maps to --billing payg.
# ---------------------------------------------------------------------------
run_wrapper bash --apikey sk-test-123 --provider aliyun
echo "# scenario2 exit=$wrapper_code"
sed 's/^/#     /' "$tmp/out"
if [ "$wrapper_code" -eq 2 ]; then
    ok_ "arg run reaches python3 (exit 2 from stub, got $wrapper_code)"
else
    not_ok_ "arg run reaches python3 (got $wrapper_code)"
fi
check_contains "--apikey translated to --api-key with value" "$tmp/record" "api-key sk-test-123"
check_contains "aliyun provider translated to payg billing" "$tmp/record" "billing payg"

echo "1..$((pass + fail))"
if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
