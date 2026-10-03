#!/bin/bash
# test-module-params.sh - regression test for test-module.sh parameter passing
#
# Parameters were joined into a string and re-expanded unquoted into insmod,
# so values with spaces were word-split ("greeting=Hello World" became the
# two arguments greeting=Hello and World, which the kernel rejects) and
# values with glob characters were expanded against the cwd. Drive the load
# path with a stub insmod (no kernel, no root) and assert every parameter
# arrives as exactly one argument.
#
# Usage: ./test-module-params.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_MODULE="$SCRIPT_DIR/test-module.sh"

TMP="$(mktemp -d /tmp/test-module-params.XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

mkdir -p "$TMP/bin"
: > "$TMP/capture"

# Stub toolchain: insmod records its arguments one per line and marks the
# module loaded; lsmod reports the recorded state (grep -q still consumes a
# call, so state must be tracked, not call order); rmmod clears it.
cat > "$TMP/bin/insmod" <<EOF
#!/bin/bash
shift  # module file
for arg in "\$@"; do
    printf '%s\n' "\$arg" >> "$TMP/capture"
done
: > "$TMP/loaded"
EOF
cat > "$TMP/bin/lsmod" <<EOF
#!/bin/bash
if [ -f "$TMP/loaded" ]; then
    printf 'mymod 1234 0 - Loaded\n'
else
    :
fi
EOF
cat > "$TMP/bin/rmmod" <<EOF
#!/bin/bash
rm -f "$TMP/loaded"
EOF
printf '#!/bin/bash\n' > "$TMP/bin/dmesg"
printf '#!/bin/bash\n' > "$TMP/bin/modinfo"
chmod +x "$TMP/bin"/*

# Unprivileged copy: only the root check and the sleep duration are rewritten;
# the lines under test (argument collection and the insmod invocation) are
# untouched.
# shellcheck disable=SC2016  # the sed pattern must stay unexpanded
sed -e 's|\[ "\$EUID" -ne 0 \]|[ "0" -ne "0" ]|' \
    -e 's|^sleep 3$|sleep 0|' \
    "$TEST_MODULE" > "$TMP/test-module.sh"
chmod +x "$TMP/test-module.sh"
: > "$TMP/mymod.ko"

# Fixed cwd with a matching marker file, so the buggy re-globbing is
# deterministic: the unquoted token pattern=* expands to any file named
# pattern=... in the cwd (the marker here), not to whatever cwd the runner has.
: > "$TMP/pattern=glob-marker"
cd "$TMP"

PATH="$TMP/bin:$PATH" "$TMP/test-module.sh" mymod 'greeting=Hello World' 'repeat_count=3' 'pattern=*' > "$TMP/out" 2>&1 || {
    echo "FAIL: test-module.sh exited non-zero"
    cat "$TMP/out"
    exit 1
}

# Each parameter must reach insmod as exactly one argument, including the
# value with a space and the glob character.
EXPECTED=$'greeting=Hello World\nrepeat_count=3\npattern=*'
if [ "$(cat "$TMP/capture")" != "$EXPECTED" ]; then
    echo "FAIL: insmod received wrong arguments:"
    cat "$TMP/capture"
    exit 1
fi

echo "OK: parameters passed to insmod intact"
