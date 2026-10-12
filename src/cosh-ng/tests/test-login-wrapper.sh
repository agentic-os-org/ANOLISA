#!/usr/bin/env bash
# Exercise the cosh-login wrapper (packaging/login/cosh-login) against
# fixture cosh shapes: healthy ELF, damaged content with the exec bit kept
# (the ANOLISA #6792 lockout form), missing/dangling/unreadable entries.
# The wrapper's two path assignments are rewritten to fixture paths, and both
# exec targets are compiled ELF stubs: scripts would lose the exec -a argv[0]
# to binfmt_script, which is exactly the behavior under test.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WRAPPER_SRC="$ROOT/packaging/login/cosh-login"
TMP="$(mktemp -d /tmp/cosh-ng-login-wrapper-test.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

FIXTURE="$TMP/bin"
install -d -m 0755 "$FIXTURE"

# Both exec targets share one stub source; a compile-time marker tells the
# two binaries apart. The stub reports argv[0], $SHELL, and every argument so
# the tests can assert the exec contract byte-exactly.
cat > "$TMP/stub.c" << 'EOF'
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    const char *shell = getenv("SHELL");
    int i;
    printf("marker=%s\n", MARKER);
    printf("argv0=%s\n", argv[0]);
    printf("SHELL=%s\n", shell ? shell : "");
    for (i = 1; i < argc; i++) {
        printf("arg%d=%s\n", i, argv[i]);
    }
    return 0;
}
EOF

cc -DMARKER='"cosh-stub"' -o "$TMP/cosh-elf" "$TMP/stub.c"
cc -DMARKER='"bash-stub"' -o "$FIXTURE/bash" "$TMP/stub.c"

# Redirect the wrapper at the fixture: only the two assignment lines may be
# rewritten, and the rewrite must provably happen (a silent sed no-op would
# point the test at this machine's real /usr/bin/cosh and /bin/bash).
WRAPPER="$TMP/cosh-login"
sed -e "s|^cosh=/usr/bin/cosh\$|cosh=$FIXTURE/cosh|" \
    -e "s|^fallback_shell=/bin/bash\$|fallback_shell=$FIXTURE/bash|" \
    "$WRAPPER_SRC" > "$WRAPPER"
grep -Fxq "cosh=$FIXTURE/cosh" "$WRAPPER"
grep -Fxq "fallback_shell=$FIXTURE/bash" "$WRAPPER"
if grep -Fxq 'cosh=/usr/bin/cosh' "$WRAPPER"; then
    echo "ERROR: wrapper still targets /usr/bin/cosh after fixture rewrite" >&2
    exit 1
fi
if grep -Fxq 'fallback_shell=/bin/bash' "$WRAPPER"; then
    echo "ERROR: wrapper still targets /bin/bash after fixture rewrite" >&2
    exit 1
fi

pass_count=0

pass() {
    pass_count=$((pass_count + 1))
    echo "PASS: $1"
}

fail() {
    echo "ERROR: $1" >&2
    exit 1
}

run_wrapper() {
    bash "$WRAPPER" "$@" >"$TMP/out" 2>"$TMP/err"
}

expect_out_line() {
    grep -Fxq "$1" "$TMP/out" || {
        echo "ERROR: expected stdout line '$1', got:" >&2
        cat "$TMP/out" >&2
        exit 1
    }
}

expect_fallback() {
    # $1: case name; remaining args: forwarded to the wrapper.
    local name="$1"
    shift
    run_wrapper "$@"
    expect_out_line "marker=bash-stub"
    expect_out_line "argv0=-bash"
    expect_out_line "SHELL=$FIXTURE/bash"
    [ "$(wc -l < "$TMP/err" | tr -d ' ')" -eq 1 ] ||
        fail "$name: fallback must print exactly one stderr line"
    grep -Fq "Error: cosh-ng login runtime unavailable" "$TMP/err" ||
        fail "$name: fallback stderr missing the English notice"
    if grep -Fq "command not found" "$TMP/err"; then
        fail "$name: damaged content was interpreted as a shell script"
    fi
    pass "$name"
}

install_healthy_cosh() {
    cp "$TMP/cosh-elf" "$FIXTURE/cosh"
    chmod 0755 "$FIXTURE/cosh"
}

# --- healthy ELF: exec into cosh with the login argv[0] contract ---
install_healthy_cosh
run_wrapper
expect_out_line "marker=cosh-stub"
expect_out_line "argv0=-cosh"
expect_out_line "SHELL=$FIXTURE/cosh"
[ ! -s "$TMP/err" ] || fail "healthy path must not write to stderr"
pass "healthy ELF execs cosh with login argv0 and SHELL"

# --- healthy ELF: argument passthrough is byte-exact ---
install_healthy_cosh
run_wrapper foo 'b ar' --flag
expect_out_line "arg1=foo"
expect_out_line "arg2=b ar"
expect_out_line "arg3=--flag"
pass "healthy path forwards arguments byte-exactly"

# --- healthy ELF: non-interactive -c forwarding ---
install_healthy_cosh
run_wrapper -c 'echo hi'
expect_out_line "marker=cosh-stub"
expect_out_line "arg1=-c"
expect_out_line "arg2=echo hi"
pass "healthy path forwards -c command"

# --- damaged content with exec bit kept (ANOLISA #6792 repro shape) ---
printf 'GARBAGE-NOT-AN-ELF' > "$FIXTURE/cosh"
chmod 0755 "$FIXTURE/cosh"
expect_fallback "garbage content with exec bit falls back to bash"

# --- damaged fallback still forwards -c ---
printf 'GARBAGE-NOT-AN-ELF' > "$FIXTURE/cosh"
chmod 0755 "$FIXTURE/cosh"
expect_fallback "fallback forwards -c command" -c 'echo hi'
expect_out_line "arg1=-c"
expect_out_line "arg2=echo hi"

# --- missing entry ---
rm -f "$FIXTURE/cosh"
expect_fallback "missing cosh falls back to bash"

# --- dangling symlink ---
ln -s "$FIXTURE/does-not-exist" "$FIXTURE/cosh"
expect_fallback "dangling cosh symlink falls back to bash"
rm -f "$FIXTURE/cosh"

# --- empty file with exec bit ---
: > "$FIXTURE/cosh"
chmod 0755 "$FIXTURE/cosh"
expect_fallback "empty executable falls back to bash"

# --- ELF content without the exec bit ---
cp "$TMP/cosh-elf" "$FIXTURE/cosh"
chmod 0644 "$FIXTURE/cosh"
expect_fallback "ELF without exec bit falls back to bash"

# --- unreadable ELF (mode 0000): the magic read fails closed ---
# Root bypasses permission checks, so this shape is only meaningful for
# unprivileged runs.
if [ "$(id -u)" -ne 0 ]; then
    cp "$TMP/cosh-elf" "$FIXTURE/cosh"
    chmod 0000 "$FIXTURE/cosh"
    expect_fallback "unreadable ELF falls back to bash"
else
    echo "SKIP: unreadable-ELF case requires a non-root user"
fi

# --- sanity: the assertions above actually ran ---
if [ "$pass_count" -lt 9 ]; then
    echo "ERROR: expected at least 9 exercised cases, got $pass_count" >&2
    exit 1
fi

echo "cosh-ng login wrapper tests passed ($pass_count cases)"
