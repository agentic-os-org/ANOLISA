#!/usr/bin/env bash
# Pin the fail-fast contract of `make install`: a failed skill copy must abort
# the target and name the offending skill instead of being masked by a later
# successful iteration, and the trailing component.toml steps must not run
# against an incomplete skills tree. Uses a staging DESTDIR and a stubbed `cp`
# that fails on a chosen invocation; `install` itself is not stubbed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMPONENT_DIR="$SCRIPT_DIR/.."
SANDBOX="$(mktemp -d -t os-skills-install.XXXXXX)"
trap 'rm -r -- "$SANDBOX"' EXIT

# Fixture component: the real Makefile and manifest plus two stub skills.
# Fixtures live only in the sandbox — a SKILL.md inside the repository tree
# would itself be picked up by `find` during real packaging.
mkdir -p "$SANDBOX/component"
cp "$COMPONENT_DIR/Makefile" "$COMPONENT_DIR/component.toml" "$SANDBOX/component/"
for skill in alpha beta; do
    mkdir -p "$SANDBOX/component/skill-$skill"
    printf '# %s\n' "$skill" > "$SANDBOX/component/skill-$skill/SKILL.md"
done

# The stub `cp` delegates to the real one but fails on invocation $CP_FAIL_ON.
mkdir -p "$SANDBOX/bin"
cat > "$SANDBOX/bin/cp" <<'STUB'
#!/usr/bin/env bash
set -euo pipefail
count=$(( $(cat "${CP_COUNTER:?}" 2>/dev/null || echo 0) + 1 ))
echo "$count" > "$CP_COUNTER"
if [ "$count" = "${CP_FAIL_ON:?}" ]; then
    echo "stub cp: simulated failure (invocation $count): $*" >&2
    exit 1
fi
exec /bin/cp "$@"
STUB
chmod +x "$SANDBOX/bin/cp"

# Mirror the Makefile's own discovery order so per-skill assertions hold even
# if the filesystem returns a different readdir order.
skill_at() {
    (cd "$SANDBOX/component" && find . -name SKILL.md -print) \
        | sed -n "$1p" | sed 's|^\./||; s|/SKILL.md$||'
}
first="$(skill_at 1)"
second="$(skill_at 2)"

skills_root() { echo "$1/usr/share/anolisa/skills"; }
manifest() { echo "$1/usr/share/anolisa/components/os-skills/component.toml"; }

# Run `make install` into a fresh staging root; returns make's exit status.
run_install() {  # <fail-on-invocation|none> <name>
    local dest="$SANDBOX/$2"
    rm -rf "$dest"
    echo 0 > "$SANDBOX/counter"
    PATH="$SANDBOX/bin:$PATH" CP_COUNTER="$SANDBOX/counter" CP_FAIL_ON="$1" \
        make -C "$SANDBOX/component" install DESTDIR="$dest" \
        > "$SANDBOX/$2.log" 2>&1
}

expect_failure() {  # <name> <expected skill in the error>
    if run_install "$3" "$1"; then
        echo "ERROR: make install succeeded although a copy failed ($1)" >&2
        cat "$SANDBOX/$1.log" >&2
        exit 1
    fi
    if ! grep -qF "ERROR: failed to install skill $2" "$SANDBOX/$1.log"; then
        echo "ERROR: $1 log does not name the failing skill '$2'" >&2
        cat "$SANDBOX/$1.log" >&2
        exit 1
    fi
    if [ -e "$(manifest "$SANDBOX/$1")" ]; then
        echo "ERROR: $1 installed the manifest against an incomplete tree" >&2
        exit 1
    fi
}

# A failure on the first copy must abort the target...
expect_failure fail-first "$first" 1
# ...and so must a failure on a later copy (the previously masked case).
expect_failure fail-second "$second" 2

# The clean path still installs every skill and the manifest.
if ! run_install none clean; then
    echo "ERROR: clean install failed" >&2
    cat "$SANDBOX/clean.log" >&2
    exit 1
fi
for skill in "$first" "$second"; do
    if [ ! -f "$(skills_root "$SANDBOX/clean")/$skill/SKILL.md" ]; then
        echo "ERROR: clean install is missing skill '$skill'" >&2
        exit 1
    fi
done
if [ ! -f "$(manifest "$SANDBOX/clean")" ]; then
    echo "ERROR: clean install is missing the component manifest" >&2
    exit 1
fi

echo "os-skills install fail-fast: OK"
