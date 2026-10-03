#!/usr/bin/env bash
# Promotion must normalize permissions only on the paths this installer
# staged (bin/anolisa and share/anolisa). Install into a fake prefix
# pre-populated with files owned by other packages and assert their modes
# survive the install untouched.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install-anolisa.sh"

TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

# Mode of a path as 4-digit octal, portable across BSD (macOS) and GNU
# coreutils stat (both print 3 digits when the leading digit is 0).
file_mode() {
    local mode
    if [[ "$(uname)" == "Darwin" ]]; then
        mode="$(stat -f %Lp "$1")"
    else
        mode="$(stat -c %a "$1")"
    fi
    printf '%04o' "$((8#$mode))"
}

assert_mode() {
    local path="$1" expected="$2" actual
    actual="$(file_mode "$path")"
    if [[ "$actual" != "$expected" ]]; then
        printf 'ERROR: %s has mode %s, expected %s\n' "$path" "$actual" "$expected" >&2
        exit 1
    fi
}

PREFIX="$TEMPORARY/prefix"
SRC="$TEMPORARY/src"

# Pre-existing content owned by other packages under the shared prefix.
install -d -m 0755 "$PREFIX/bin" "$PREFIX/bin/other-pkg" "$PREFIX/share/other-pkg"
printf '#!/usr/bin/env sh\nexit 0\n' > "$PREFIX/bin/other-pkg/tool.sh"
chmod 0755 "$PREFIX/bin/other-pkg/tool.sh"
printf 'not a program\n' > "$PREFIX/bin/other-tool-data"
chmod 0644 "$PREFIX/bin/other-tool-data"
printf '#!/usr/bin/env sh\nexit 0\n' > "$PREFIX/share/other-pkg/tool.sh"
chmod 0755 "$PREFIX/share/other-pkg/tool.sh"
printf 'private\n' > "$PREFIX/share/other-pkg/data.txt"
chmod 0600 "$PREFIX/share/other-pkg/data.txt"

# The prefix root itself pre-exists (like /usr/local does in system mode) and
# may carry a mode chosen by other packages; the installer must not touch it.
chmod 0711 "$PREFIX"

# Stub anolisa source tree: an executable release binary (so the installer
# does not invoke cargo) plus one osbase manifest to stage. The empty
# templates/ directory only satisfies the checkout-layout validation.
install -d "$SRC/target/release" "$SRC/manifests/osbase" "$SRC/templates"
printf '#!/usr/bin/env sh\nexit 0\n' > "$SRC/target/release/anolisa"
chmod 0755 "$SRC/target/release/anolisa"
printf 'version = "0.0.0-test"\n' > "$SRC/manifests/osbase/base.toml"

ANOLISA_PREFIX="$PREFIX" bash "$INSTALLER" --from-local "$SRC" > "$TEMPORARY/install.log"

# Files the installer did NOT stage must keep their original modes, including
# the pre-existing prefix root.
assert_mode "$PREFIX" 0711
assert_mode "$PREFIX/bin/other-pkg/tool.sh" 0755
assert_mode "$PREFIX/bin/other-tool-data" 0644
assert_mode "$PREFIX/share/other-pkg/tool.sh" 0755
assert_mode "$PREFIX/share/other-pkg/data.txt" 0600

# Staged paths still get the intended modes.
assert_mode "$PREFIX/bin/anolisa" 0755
assert_mode "$PREFIX/share/anolisa/manifests/osbase/base.toml" 0644
assert_mode "$PREFIX/share/anolisa/manifests/osbase" 0755
assert_mode "$PREFIX/share/anolisa/manifests" 0755

# A fresh prefix installed under a restrictive umask must still yield
# traversable bin/ and share/ parent directories (0755); other users need
# to traverse them to reach the staged binary and data even though the
# staged paths themselves are 0755. The prefix root is the outermost of
# those parents, so it needs the same treatment.
PREFIX2="$TEMPORARY/prefix2"
(umask 077 && ANOLISA_PREFIX="$PREFIX2" bash "$INSTALLER" --from-local "$SRC" \
    > "$TEMPORARY/install2.log")
assert_mode "$PREFIX2" 0755
assert_mode "$PREFIX2/bin" 0755
assert_mode "$PREFIX2/share" 0755
assert_mode "$PREFIX2/bin/anolisa" 0755
assert_mode "$PREFIX2/share/anolisa/manifests/osbase" 0755

# A prefix nested below a directory that is missing too: `mkdir -p` creates
# both levels, so both must come out traversable.
PREFIX3="$TEMPORARY/fresh-parent/fresh-prefix"
(umask 077 && ANOLISA_PREFIX="$PREFIX3" bash "$INSTALLER" --from-local "$SRC" \
    > "$TEMPORARY/install3.log")
assert_mode "$TEMPORARY/fresh-parent" 0755
assert_mode "$PREFIX3" 0755
assert_mode "$PREFIX3/bin" 0755

# A `..` component below a level that does not exist yet: `mkdir -p` resolves
# the `..`, and a raw-string parent walk must not treat `missing-level/..` as
# a created directory — that path names the pre-existing guard, whose mode is
# not this run's to change.
GUARD="$TEMPORARY/guard"
install -d -m 0700 "$GUARD"
PREFIX4="$GUARD/missing-level/../fresh-prefix"
(umask 077 && ANOLISA_PREFIX="$PREFIX4" bash "$INSTALLER" --from-local "$SRC" \
    > "$TEMPORARY/install4.log")
assert_mode "$GUARD" 0700
assert_mode "$GUARD/fresh-prefix" 0755

# The same through a relative prefix, the reviewer's repro: `mkdir -p`
# resolves `fresh/..` to the installer's working directory, so the walk must
# not descend past the `..` component and chmod it.
RELATIVE_WORK="$TEMPORARY/relative-work"
install -d -m 0700 "$RELATIVE_WORK"
(
    cd "$RELATIVE_WORK" && umask 077 && ANOLISA_PREFIX='fresh/../out' \
        bash "$INSTALLER" --from-local "$SRC" > "$TEMPORARY/install5.log"
)
assert_mode "$RELATIVE_WORK" 0700
assert_mode "$RELATIVE_WORK/out" 0755
assert_mode "$RELATIVE_WORK/out/bin" 0755

echo "ok - install promotion chmod stays scoped to staged paths"
