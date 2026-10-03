#!/usr/bin/env bash
# SPDX-License-Identifier: MIT
# Regression test for install-qwenpaw setup.sh — the primary install path
# runs `uv pip install qwenpaw` without an active virtual environment and
# with stderr discarded. Real uv refuses to install outside a venv unless
# --system is passed, so the aliyun-mirror path always failed silently and
# every install degraded to the curl|bash fallback.
#
# Runs the real setup.sh end-to-end against a stubbed PATH (uv records its
# argv; curl/sleep/pgrep/nohup stubbed; HOME redirected). No dependencies:
#     bash test-setup-uv-system.sh

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$SCRIPT_DIR/setup.sh"

pass=0
fail=0
check_n=0

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

ok_()     { echo "ok - $1"; pass=$((pass + 1)); }
not_ok_() { echo "not ok - $1"; fail=$((fail + 1)); }
check_contains() {  # <name> <file-or-string> <needle> [var]
    check_n=$((check_n + 1))
    local hay
    hay="$(cat "$2" 2>/dev/null || printf '%s' "$2")"
    case "$hay" in
        *"$3"*) ok_ "$1" ;;
        *)      not_ok_ "$1" ;;
    esac
}

# make_stubbin <dir> — uv records argv (UV_STUB_PIP_FAIL=1 makes `uv pip`
# fail on stderr), curl answers the localhost probe, noisy commands stubbed.
make_stubbin() {
    mkdir -p "$1"
    cat > "$1/uv" <<'STUB'
#!/bin/sh
echo "uv argv: $*" >> "$UV_STUB_RECORD"
case "$1" in
  --version) echo "uv 0.11.32 (stub)"; exit 0 ;;
  pip)
    if [ -n "${UV_STUB_PIP_FAIL:-}" ]; then
        echo "uv-stub: simulated failure: no virtual environment found" >&2
        exit 2
    fi
    exit 0
    ;;
esac
exit 0
STUB
    cat > "$1/curl" <<'STUB'
#!/bin/sh
case "$*" in
  *localhost:8088*) echo "404" ;;
  *install.sh*) echo "true" ;;
  *) echo "curl-stub $*" ;;
esac
STUB
    printf '#!/bin/sh\nexit 0\n' > "$1/sleep"
    printf '#!/bin/sh\nexit 1\n' > "$1/pgrep"
    printf '#!/bin/sh\nexit 0\n' > "$1/nohup"
    chmod +x "$1/uv" "$1/curl" "$1/sleep" "$1/pgrep" "$1/nohup"
}

# run_setup <stubbin> <home> <outfile> <errfile> [extra env...]
run_setup() {
    local stubbin="$1" home="$2" outfile="$3" errfile="$4"
    shift 4
    (
        umask 022
        cd "$tmp" || exit 1
        export HOME="$home" PATH="$stubbin:/usr/bin:/bin:/usr/sbin:/sbin"
        export UV_STUB_RECORD="$tmp/uv.calls"
        env "$@" bash "$SCRIPT" sk-testkey123456 dingtestclient secret-test-value \
            > "$outfile" 2> "$errfile"
    )
}

# ---------------------------------------------------------------------------
# Scenario 1: primary path succeeds — uv must be invoked with --system and
# the aliyun mirror, and the mirror path is reported as used.
# ---------------------------------------------------------------------------
: > "$tmp/uv.calls"
mkdir -p "$tmp/home1" "$tmp/home2"
make_stubbin "$tmp/bin-ok"
run_setup "$tmp/bin-ok" "$tmp/home1" "$tmp/out1" "$tmp/err1"; code=$?
echo "# scenario1 exit=$code"
if [ "$code" -eq 0 ]; then ok_ "setup completes via mirror path"; else not_ok_ "setup completes via mirror path (exit=$code)"; sed 's/^/#     /' "$tmp/out1" "$tmp/err1"; fi
check_contains "uv invoked with pip install" "$tmp/uv.calls" "pip install"
check_contains "uv invoked with --system" "$tmp/uv.calls" "--system"
check_contains "uv invoked for qwenpaw" "$tmp/uv.calls" "qwenpaw"
check_contains "uv keeps the aliyun mirror index" "$tmp/uv.calls" "--index-url https://mirrors.aliyun.com/pypi/simple/"
check_contains "mirror path reported as used" "$tmp/out1" "安装完成 (阿里云镜像)"

# ---------------------------------------------------------------------------
# Scenario 2: uv fails — its stderr must reach the user (no 2>/dev/null)
# before the fallback message.
# ---------------------------------------------------------------------------
: > "$tmp/uv.calls"
make_stubbin "$tmp/bin-fail"
run_setup "$tmp/bin-fail" "$tmp/home2" "$tmp/out2" "$tmp/err2" UV_STUB_PIP_FAIL=1; code=$?
echo "# scenario2 exit=$code"
cat "$tmp/out2" "$tmp/err2" > "$tmp/combined2"
check_contains "uv failure reason visible to the user" "$tmp/combined2" "no virtual environment"
check_contains "fallback message shown after failure" "$tmp/out2" "阿里云镜像安装失败"

echo "1..$((pass + fail))"
if [ "$fail" -gt 0 ]; then
    exit 1
fi
exit 0
