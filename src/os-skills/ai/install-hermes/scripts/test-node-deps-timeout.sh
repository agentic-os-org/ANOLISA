#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install.sh (install-hermes) — install_node_deps ran
# the WhatsApp Bridge install as `timeout 60 npm install ...`. GNU timeout
# does not exist on macOS (a supported OS per detect_os), so npm was never
# invoked there and the failure was misreported as "timed out or failed".
#
# The real install_node_deps function is extracted from install.sh and
# driven with a curated PATH containing only stubbed npm/npx (so `timeout`
# is absent by construction, on any host). No dependencies; run directly:
#     bash test-node-deps-timeout.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/install.sh"

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

# Extract install_node_deps (a top-level function) from the real installer.
awk '/^install_node_deps\(\) \{/,/^\}/' "$SCRIPT" > "$tmp/node_deps.fn"
if ! grep -q '^install_node_deps()' "$tmp/node_deps.fn"; then
    echo "Bail out! could not extract install_node_deps() from $SCRIPT"
    exit 1
fi

# A fake project layout: root package.json plus a WhatsApp Bridge one.
proj="$tmp/proj"
mkdir -p "$proj/scripts/whatsapp-bridge"
echo '{}' > "$proj/package.json"
echo '{}' > "$proj/scripts/whatsapp-bridge/package.json"

# make_stubbin <dir> <with-timeout> — npm/npx record their cwd and argv.
make_stubbin() {
    mkdir -p "$1"
    printf '#!/bin/sh\necho "npm :: $PWD :: $*" >> "%s/record"\nexit 0\n' "$tmp" > "$1/npm"
    printf '#!/bin/sh\necho "npx :: $*" >> "%s/record"\nexit 0\n' "$tmp" > "$1/npx"
    if [ "$2" = true ]; then
        printf '#!/bin/sh\necho "timeout :: $*" >> "%s/record"\nshift\nexec "$@"\n' "$tmp" > "$1/timeout"
        chmod +x "$1/npm" "$1/npx" "$1/timeout"
    else
        chmod +x "$1/npm" "$1/npx"
    fi
}

# run_node_deps <stubbin> — run the extracted function with curated env.
run_node_deps() {
    local frag="$tmp/fragment.sh"
    {
        echo '#!/bin/bash'
        echo 'set -e'
        echo 'log_info()    { echo "[info] $*"; }'
        echo 'log_success() { echo "[ok] $*"; }'
        echo 'log_warn()    { echo "[warn] $*"; }'
        echo 'log_error()   { echo "[error] $*"; }'
        echo 'HAS_NODE=true'
        echo 'INSTALL_MINIMAL=false'
        echo 'DISTRO=macos'
        echo "INSTALL_DIR=\"$proj\""
        echo 'NPM_REGISTRY=https://registry.npmmirror.com'
        echo ". \"$tmp/node_deps.fn\""
        echo 'install_node_deps'
        echo 'echo "FRAGMENT-DONE"'
    } > "$frag"
    : > "$tmp/record"
    PATH="$1" "${BASH:-/bin/bash}" "$frag" > "$tmp/out" 2>&1
}

# ---------------------------------------------------------------------------
# Scenario 1 (the bug): PATH without timeout(1) — npm must still be invoked
# for the WhatsApp Bridge and report success.
# ---------------------------------------------------------------------------
make_stubbin "$tmp/bin-notimeout" false
run_node_deps "$tmp/bin-notimeout"
code=$?
echo "# scenario1 exit=$code"
sed 's/^/#     /' "$tmp/out"
if [ "$code" -eq 0 ] && grep -q 'FRAGMENT-DONE' "$tmp/out"; then
    ok_ "install_node_deps completes"
else
    not_ok_ "install_node_deps completes (exit=$code)"
fi
check_contains "npm invoked from the whatsapp-bridge dir" "$tmp/record" \
    "npm :: .*scripts/whatsapp-bridge :: install --registry"
check_contains "bridge install reported as installed" "$tmp/out" \
    "WhatsApp Bridge dependencies installed"
check_not_contains "no timeout stub was used" "$tmp/record" "timeout ::"

# ---------------------------------------------------------------------------
# Scenario 2 (control): with timeout(1) available the wrapper is used and
# npm still runs.
# ---------------------------------------------------------------------------
make_stubbin "$tmp/bin-timeout" true
run_node_deps "$tmp/bin-timeout"
code=$?
echo "# scenario2 exit=$code"
if [ "$code" -eq 0 ] && grep -q 'FRAGMENT-DONE' "$tmp/out"; then
    ok_ "install_node_deps completes with timeout available"
else
    not_ok_ "install_node_deps completes with timeout available (exit=$code)"
fi
check_contains "timeout wraps the bridge npm install" "$tmp/record" \
    "timeout :: 60 npm install"
check_contains "npm invoked from the whatsapp-bridge dir (timeout host)" "$tmp/record" \
    "npm :: .*scripts/whatsapp-bridge :: install --registry"

echo "1..$((pass + fail))"
if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
