#!/usr/bin/env bash
# Exercise the cosh-wsl preview launcher with stub wsl.exe/wslpath/cosh-shell
# binaries so argument passing, cwd conversion, and failure branches stay
# verifiable on Linux CI. `pwd -W` exists only in Git Bash/MSYS2, so the
# launcher is invoked from a bash subprocess that exports a stub `pwd`
# function returning a controlled Windows path; the production script keeps
# its real failure semantics. These stubs do not replace real
# Windows/Git Bash/WSL2 acceptance testing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP="$(mktemp -d /tmp/cosh-ng-wsl-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

LOG_ARGS="$TMP/wsl-args.log"
LOG_ENV="$TMP/wsl-env.log"
LOG_CWD="$TMP/cosh-cwd.log"

BASE_BIN="$TMP/base-bin"
install -d -m 0755 "$BASE_BIN"
ln -s "$(command -v bash)" "$BASE_BIN/bash"

make_stub_wsl() {
    local dir="$1"
    cat > "$dir/wsl.exe" <<'STUB'
#!/usr/bin/env bash
# Record argv and the argument-conversion guard, then emulate wsl.exe by
# running the bash -lc payload without the login flag: a login shell would
# reload the host profile and drop the stub PATH used by this test.
printf '%s\n' "$@" >> "$COSH_WSL_STUB_ARGS"
printf 'MSYS2_ARG_CONV_EXCL=%s\n' "${MSYS2_ARG_CONV_EXCL-UNSET}" >> "$COSH_WSL_STUB_ENV"
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-lc" ]; then
        script="$2"
        shift 2
        exec bash -c "$script" "$@"
    fi
    shift
done
echo "stub wsl.exe: no -lc payload in arguments" >&2
exit 2
STUB
    chmod 0755 "$dir/wsl.exe"
}

make_stub_wslpath() {
    local dir="$1"
    cat > "$dir/wslpath" <<'STUB'
#!/usr/bin/env bash
if [ -n "${COSH_WSL_STUB_WSLPATH_FAIL:-}" ]; then
    echo "wslpath: stub conversion failure" >&2
    exit 1
fi
printf '%s\n' "$COSH_WSL_STUB_WSLPATH_OUT"
STUB
    chmod 0755 "$dir/wslpath"
}

make_stub_cosh_shell() {
    local dir="$1"
    cat > "$dir/cosh-shell" <<'STUB'
#!/usr/bin/env bash
printf '%s\n' "$(pwd -P)" >> "$COSH_WSL_STUB_CWD"
exit "${COSH_WSL_STUB_COSH_EXIT:-0}"
STUB
    chmod 0755 "$dir/cosh-shell"
}

# run_launcher <stub-bin> <work-dir> <stderr-file> [extra-env ...]
# Runs the launcher from a bash subprocess whose PATH only contains the stub
# bin directory and a bare bash, with a stub `pwd` exported so that `pwd -W`
# returns $COSH_WSL_STUB_WINDOWS_CWD (an MSYS2-only builtin simulated for the
# test only). Extra arguments are NAME=VALUE assignments exported for the
# launcher; plain `export` is used because the restricted PATH cannot resolve
# an external `env`.
run_launcher() {
    local stub_bin="$1"
    local work_dir="$2"
    local stderr_file="$3"
    shift 3
    (
        cd "$work_dir"
        unset COSH_WSL_DISTRO
        export PATH="$stub_bin:$BASE_BIN"
        export COSH_WSL_STUB_ARGS="$LOG_ARGS"
        export COSH_WSL_STUB_ENV="$LOG_ENV"
        export COSH_WSL_STUB_CWD="$LOG_CWD"
        pwd() {
            if [ "${1:-}" = "-W" ]; then
                printf '%s\n' "$COSH_WSL_STUB_WINDOWS_CWD"
            else
                command pwd "$@"
            fi
        }
        export -f pwd
        local assignment
        for assignment in "$@"; do
            export "$assignment"
        done
        bash "$ROOT/scripts/cosh-wsl" 2>"$stderr_file"
    )
}

reset_logs() {
    : >"$LOG_ARGS"
    : >"$LOG_ENV"
    : >"$LOG_CWD"
}

passed=0
pass() {
    passed=$((passed + 1))
    echo "PASS $1"
}

# Case 1: the default distribution is Ubuntu.
stub_bin="$TMP/case1"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
reset_logs
run_launcher "$stub_bin" "$TMP" "$TMP/case1.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case1' \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP"
test "$(awk 'prev == "-d" { print; exit } { prev = $0 }' "$LOG_ARGS")" = "Ubuntu"
pass "default distro is Ubuntu"

# Case 2: COSH_WSL_DISTRO overrides the default distribution.
stub_bin="$TMP/case2"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
reset_logs
run_launcher "$stub_bin" "$TMP" "$TMP/case2.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case2' \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP" \
    "COSH_WSL_DISTRO=Debian"
test "$(awk 'prev == "-d" { print; exit } { prev = $0 }' "$LOG_ARGS")" = "Debian"
pass "COSH_WSL_DISTRO overrides the distro"

# Case 3: MSYS2_ARG_CONV_EXCL='*' guards the wsl.exe invocation.
stub_bin="$TMP/case3"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
reset_logs
run_launcher "$stub_bin" "$TMP" "$TMP/case3.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case3' \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP"
grep -Fxq 'MSYS2_ARG_CONV_EXCL=*' "$LOG_ENV"
pass "MSYS2_ARG_CONV_EXCL guards the wsl.exe call"

# Case 4: a Windows cwd with spaces, CJK, quotes, backslash, and substitution
# metacharacters reaches the inner script byte-for-byte without word
# splitting or injection.
stub_bin="$TMP/case4"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
tricky='C:\work\tricky dir 中文 '"'"' " \ `touch pwned` $(touch pwned2)'
reset_logs
run_launcher "$stub_bin" "$TMP" "$TMP/case4.err" \
    "COSH_WSL_STUB_WINDOWS_CWD=$tricky" \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP"
test "$(tail -n 1 "$LOG_ARGS")" = "$tricky"
test -z "$(find "$TMP" -name 'pwned*' -print -quit)"
pass "tricky cwd arrives byte-for-byte without injection"

# Case 5: the wslpath result, not the original cwd, becomes the cosh-shell
# working directory.
stub_bin="$TMP/case5"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
converted_dir="$TMP/case5-mnt-c style"
install -d -m 0755 "$converted_dir"
reset_logs
run_launcher "$stub_bin" "$TMP" "$TMP/case5.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case5' \
    "COSH_WSL_STUB_WSLPATH_OUT=$converted_dir"
test "$(tail -n 1 "$LOG_CWD")" = "$(cd "$converted_dir" && pwd -P)"
pass "wslpath result becomes the cosh-shell cwd"

# Case 6: exec semantics — the cosh-shell exit code propagates unchanged.
stub_bin="$TMP/case6"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
reset_logs
rc=0
run_launcher "$stub_bin" "$TMP" "$TMP/case6.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case6' \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP" \
    "COSH_WSL_STUB_COSH_EXIT=42" || rc=$?
test "$rc" -eq 42
pass "cosh-shell exit code propagates unchanged"

# Case 7: a missing wsl.exe fails with an actionable install hint.
rc=0
(
    cd "$TMP"
    export PATH="$BASE_BIN"
    bash "$ROOT/scripts/cosh-wsl" 2>"$TMP/case7.err"
) || rc=$?
test "$rc" -ne 0
grep -Fq 'wsl --install' "$TMP/case7.err"
pass "missing wsl.exe reports an install hint"

# Case 8: a wslpath conversion failure reports the failed path.
stub_bin="$TMP/case8"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
failed_path='C:\work\case8-unconvertible'
rc=0
run_launcher "$stub_bin" "$TMP" "$TMP/case8.err" \
    "COSH_WSL_STUB_WINDOWS_CWD=$failed_path" \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP" \
    "COSH_WSL_STUB_WSLPATH_FAIL=1" || rc=$?
test "$rc" -ne 0
grep -Fq "$failed_path" "$TMP/case8.err"
pass "wslpath failure reports the failed path"

# Case 9: an inaccessible converted directory reports the target path.
stub_bin="$TMP/case9"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
missing_dir="$TMP/case9-does-not-exist"
rc=0
run_launcher "$stub_bin" "$TMP" "$TMP/case9.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case9' \
    "COSH_WSL_STUB_WSLPATH_OUT=$missing_dir" || rc=$?
test "$rc" -ne 0
grep -Fq "$missing_dir" "$TMP/case9.err"
pass "inaccessible directory reports the target path"

# Case 10: a missing cosh-shell reports an in-distro install hint.
stub_bin="$TMP/case10"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
rc=0
run_launcher "$stub_bin" "$TMP" "$TMP/case10.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case10' \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP" || rc=$?
test "$rc" -ne 0
grep -Fq 'Install cosh-ng inside the distribution' "$TMP/case10.err"
pass "missing cosh-shell reports an in-distro install hint"

# Case 11: the startup banner declares the four preview status points.
stub_bin="$TMP/case11"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
make_stub_wslpath "$stub_bin"
make_stub_cosh_shell "$stub_bin"
reset_logs
run_launcher "$stub_bin" "$TMP" "$TMP/case11.err" \
    'COSH_WSL_STUB_WINDOWS_CWD=C:\work\case11' \
    "COSH_WSL_STUB_WSLPATH_OUT=$TMP"
grep -Fq 'runs on Linux inside WSL2' "$TMP/case11.err"
grep -Fq 'Git Bash is only the Windows-side entry point' "$TMP/case11.err"
grep -Fq '/mnt/c' "$TMP/case11.err"
grep -Fq 'Configuration and credentials stay in the WSL user environment' "$TMP/case11.err"
pass "startup banner declares the preview status"

# Case 12: without the MSYS2 `pwd -W` extension the launcher fails
# explicitly with an actionable hint instead of silently guessing a path.
stub_bin="$TMP/case12"
install -d -m 0755 "$stub_bin"
make_stub_wsl "$stub_bin"
rc=0
(
    cd "$TMP"
    unset COSH_WSL_DISTRO
    export PATH="$stub_bin:$BASE_BIN"
    bash "$ROOT/scripts/cosh-wsl" 2>"$TMP/case12.err"
) || rc=$?
test "$rc" -ne 0
grep -Fq 'pwd -W' "$TMP/case12.err"
grep -Fq 'Git Bash' "$TMP/case12.err"
pass "missing pwd -W fails explicitly with an actionable hint"

echo "$passed tests passed"
test "$passed" -eq 12
