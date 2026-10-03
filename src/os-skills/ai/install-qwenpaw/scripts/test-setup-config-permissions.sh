#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install-qwenpaw setup.sh — config.json receives the
# DingTalk client_secret via sed but was left at the default creation mode,
# so under umask 022 the bot credential sat world-readable (0644) while
# dashscope.json in the 0700 secret tree is chmod 600.
#
# Runs the real setup.sh end-to-end against a stubbed PATH (uv/curl/sleep/
# pgrep/nohup stubbed; HOME redirected) under umask 022, then checks the
# permission bits of the written credential files. No dependencies:
#     bash test-setup-config-permissions.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/setup.sh"

pass=0
fail=0

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

ok_()     { echo "ok - $1"; pass=$((pass + 1)); }
not_ok_() { echo "not ok - $1"; fail=$((fail + 1)); }

check_mode() {  # <name> <file> <expected-octal>
    local mode
    mode="$(stat -c '%a' "$2" 2>/dev/null || stat -f '%Lp' "$2" 2>/dev/null)"
    if [ "$mode" = "$3" ]; then
        ok_ "$1"
    else
        not_ok_ "$1 (mode=$mode, want $3)"
    fi
}

check_contains() {  # <name> <file> <needle>
    if grep -q "$3" "$2" 2>/dev/null; then
        ok_ "$1"
    else
        not_ok_ "$1"
    fi
}

# Stubbed environment: uv succeeds (no --system nuance needed here), curl
# answers the localhost probe, noisy commands are stubbed out.
mkdir -p "$tmp/bin" "$tmp/home"
printf '#!/bin/sh\ncase "$1" in --version) echo "uv 0.11.32 (stub)";; esac\nexit 0\n' > "$tmp/bin/uv"
cat > "$tmp/bin/curl" <<'STUB'
#!/bin/sh
case "$*" in
  *localhost:8088*) echo "404" ;;
  *) echo "curl-stub $*" ;;
esac
STUB
printf '#!/bin/sh\nexit 0\n' > "$tmp/bin/sleep"
printf '#!/bin/sh\nexit 1\n' > "$tmp/bin/pgrep"
printf '#!/bin/sh\nexit 0\n' > "$tmp/bin/nohup"
chmod +x "$tmp/bin/uv" "$tmp/bin/curl" "$tmp/bin/sleep" "$tmp/bin/pgrep" "$tmp/bin/nohup"

(
    umask 022
    cd "$tmp" || exit 1
    HOME="$tmp/home" PATH="$tmp/bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    bash "$SCRIPT" sk-testkey123456 dingtestclient secret-test-value \
        > "$tmp/out" 2> "$tmp/err"
)
code=$?
echo "# setup exit=$code"
sed 's/^/#     /' "$tmp/out"

if [ "$code" -eq 0 ]; then
    ok_ "setup completes"
else
    not_ok_ "setup completes (exit=$code)"
fi

check_contains "config.json contains the substituted client_secret" \
    "$tmp/home/.qwenpaw/config.json" "secret-test-value"
check_mode "config.json is 0600 (was 0644)" "$tmp/home/.qwenpaw/config.json" 600
check_mode "dashscope.json stays 0600" \
    "$tmp/home/.qwenpaw.secret/providers/builtin/dashscope.json" 600
check_mode "secret tree stays 0700" "$tmp/home/.qwenpaw.secret" 700

echo "1..$((pass + fail))"
if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
