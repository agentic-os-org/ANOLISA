#!/bin/bash
# test-module-lsmod.sh - regression test for the module test suite's lsmod probe
#
# test-module.sh probes module state with `lsmod | grep -q "^${MODULE_NAME} "`.
# The trailing space anchors the match to lsmod's first column; without it a
# module whose name merely starts with the module under test (e.g. hello vs a
# loaded hello_module) causes a false "already loaded", a false "Module is
# loaded" pass, and a false "failed to unload" failure. Run the suite against
# a stateful fake lsmod/insmod/rmmod (no root or real modules needed) with a
# longer-named sibling module staying loaded throughout.
#
# Usage: ./test-module-lsmod.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_MODULE="$SCRIPT_DIR/test-module.sh"

TMP="$(mktemp -d /tmp/test-module-lsmod.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/scripts" "$TMP/bin"

# Test copy of the suite: only the root-privilege gate is neutralized (the
# defect under test is the lsmod probing, which needs no privileges).
sed 's/\[ "$EUID" -ne 0 \]/[ 0 -ne 0 ]/' "$TEST_MODULE" > "$TMP/scripts/test-module.sh"
chmod +x "$TMP/scripts/test-module.sh"
: > "$TMP/scripts/hello.ko"

# Stateful module table: fake lsmod cats it, fake insmod registers the module
# under the .ko basename, fake rmmod removes the module's own line.
# hello_module is never touched and stays loaded throughout.
LSMOD_TABLE="$TMP/lsmod.txt"
export LSMOD_TABLE
printf 'Module          Size  Used by\nhello_module    16384  0\n' > "$LSMOD_TABLE"

cat > "$TMP/bin/lsmod" <<'EOF'
#!/bin/bash
cat "$LSMOD_TABLE"
EOF
cat > "$TMP/bin/modinfo" <<'EOF'
#!/bin/bash
echo "stub modinfo: $*"
EOF
cat > "$TMP/bin/insmod" <<'EOF'
#!/bin/bash
echo "$(basename "$1" .ko) 16384 0" >> "$LSMOD_TABLE"
EOF
cat > "$TMP/bin/rmmod" <<'EOF'
#!/bin/bash
grep -v "^$1 " "$LSMOD_TABLE" > "$LSMOD_TABLE.tmp"
mv "$LSMOD_TABLE.tmp" "$LSMOD_TABLE"
EOF
cat > "$TMP/bin/dmesg" <<'EOF'
#!/bin/bash
exit 0
EOF
cat > "$TMP/bin/sleep" <<'EOF'
#!/bin/bash
exit 0
EOF
chmod +x "$TMP/bin"/*
export PATH="$TMP/bin:$PATH"

fail() { echo "FAIL: $1" >&2; exit 1; }

out="$TMP/out.txt"
if ! "$TMP/scripts/test-module.sh" hello > "$out" 2>&1; then
    fail "suite failed for module 'hello' while only 'hello_module' is loaded"
fi
if grep -q "already loaded" "$out"; then
    fail "hello falsely matched the loaded hello_module in step 2"
fi
if ! grep -q "All tests passed for hello" "$out"; then
    fail "suite did not report a completed pass"
fi
echo "ok: hello completes cleanly beside a loaded hello_module"

echo "PASS: test-module-lsmod"
