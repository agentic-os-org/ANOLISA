#!/usr/bin/env bash
# `--strict` must refuse to finish when any fetch this run performs has no
# checksum. The binary is fetched not only in url-fetch mode: local staging
# honors an explicit ANOLISA_BIN_URL as an opt-in, so that run downloads a
# binary too and must be gated the same way. The help text and the
# "pass --strict to refuse" warning both promise this.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER="$ROOT/scripts/install-anolisa.sh"

TEMPORARY="$(mktemp -d)"
trap 'rm -rf -- "$TEMPORARY"' EXIT

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

fail() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

# Stub anolisa source tree: manifests/ + templates/ satisfy the checkout
# layout check, and a prebuilt release binary covers the pure-local run
# (nothing is fetched there, so --strict has nothing to require).
SRC="$TEMPORARY/src"
install -d "$SRC/manifests/osbase" "$SRC/templates" "$SRC/target/release"
printf 'version = "0.0.0-test"\n' > "$SRC/manifests/osbase/base.toml"
printf '#!/usr/bin/env sh\necho local\n' > "$SRC/target/release/anolisa"
chmod 0755 "$SRC/target/release/anolisa"

FETCHED="$TEMPORARY/fetched-anolisa"
printf '#!/usr/bin/env sh\necho fetched\nexit 0\n' > "$FETCHED"
chmod 0755 "$FETCHED"
FETCHED_SHA="$(sha256_of "$FETCHED")"

# 1. Local staging + explicit ANOLISA_BIN_URL and no checksum: --strict must
#    refuse before downloading anything and leave the prefix untouched.
PREFIX1="$TEMPORARY/prefix1"
set +e
OUT1="$(env ANOLISA_PREFIX="$PREFIX1" ANOLISA_BIN_URL="file://$FETCHED" \
    bash "$INSTALLER" --from-local "$SRC" --strict 2>&1)"
CODE1=$?
set -e
if [ "$CODE1" -ne 2 ]; then
    fail "--strict accepted an unverified fetched binary (exit $CODE1): $OUT1"
fi
case "$OUT1" in
    *ANOLISA_BIN_SHA256*) : ;;
    *) fail "--strict refusal must name the missing ANOLISA_BIN_SHA256: $OUT1" ;;
esac
[ ! -e "$PREFIX1" ] || fail "refused run must not write the prefix: $PREFIX1"

# 2. The same fetch with the matching checksum still installs the binary.
PREFIX2="$TEMPORARY/prefix2"
set +e
OUT2="$(env ANOLISA_PREFIX="$PREFIX2" ANOLISA_BIN_URL="file://$FETCHED" \
    ANOLISA_BIN_SHA256="$FETCHED_SHA" \
    bash "$INSTALLER" --from-local "$SRC" --strict 2>&1)"
CODE2=$?
set -e
[ "$CODE2" -eq 0 ] || fail "--strict rejected a verified binary (exit $CODE2): $OUT2"
case "$OUT2" in
    *"verified binary sha256"*) : ;;
    *) fail "verified run must report the checksum check: $OUT2" ;;
esac
cmp -s "$PREFIX2/bin/anolisa" "$FETCHED" || fail "staged binary differs from the fetched artifact"

# 3. A wrong checksum must still fail the run entirely.
PREFIX3="$TEMPORARY/prefix3"
set +e
OUT3="$(env ANOLISA_PREFIX="$PREFIX3" ANOLISA_BIN_URL="file://$FETCHED" \
    ANOLISA_BIN_SHA256="0000000000000000000000000000000000000000000000000000000000000000" \
    bash "$INSTALLER" --from-local "$SRC" --strict 2>&1)"
CODE3=$?
set -e
[ "$CODE3" -ne 0 ] || fail "wrong checksum must fail: $OUT3"
[ ! -e "$PREFIX3/bin/anolisa" ] || fail "checksum failure must not install the binary"

# 4. Pure local staging fetches nothing, so --strict has no checksum to
#    require and the prebuilt checkout binary is installed as before.
PREFIX4="$TEMPORARY/prefix4"
set +e
OUT4="$(env ANOLISA_PREFIX="$PREFIX4" bash "$INSTALLER" --from-local "$SRC" --strict 2>&1)"
CODE4=$?
set -e
[ "$CODE4" -eq 0 ] || fail "pure local --strict run must succeed (exit $CODE4): $OUT4"
cmp -s "$PREFIX4/bin/anolisa" "$SRC/target/release/anolisa" \
    || fail "pure local run must stage the checkout binary"

# 5. url-fetch mode still requires both checksums, and only those fetches
#    (the gate exits before any network access). Run a copy outside the
#    checkout so mode selection cannot pick up the surrounding manifests/ and
#    templates/ as an auto-checkout.
STANDALONE_INSTALLER="$TEMPORARY/install-anolisa.sh"
cp "$INSTALLER" "$STANDALONE_INSTALLER"
PREFIX5="$TEMPORARY/prefix5"
set +e
OUT5="$(env ANOLISA_PREFIX="$PREFIX5" ANOLISA_MIRROR="file://$TEMPORARY/empty-mirror" \
    bash "$STANDALONE_INSTALLER" --strict 2>&1)"
CODE5=$?
set -e
[ "$CODE5" -eq 2 ] || fail "url-fetch --strict must refuse without checksums (exit $CODE5): $OUT5"
case "$OUT5" in
    *ANOLISA_BIN_SHA256*) : ;;
    *) fail "url-fetch refusal must name ANOLISA_BIN_SHA256: $OUT5" ;;
esac
case "$OUT5" in
    *ANOLISA_MANIFEST_BUNDLE_SHA256*) : ;;
    *) fail "url-fetch refusal must name ANOLISA_MANIFEST_BUNDLE_SHA256: $OUT5" ;;
esac

# 5b. url-fetch + --dry-run is plan-only: it performs no fetch, so --strict
#     has no checksum to require and must print the plan instead of exiting 2.
set +e
OUT5B="$(env ANOLISA_PREFIX="$TEMPORARY/prefix5b" ANOLISA_MIRROR="file://$TEMPORARY/empty-mirror" \
    bash "$STANDALONE_INSTALLER" --strict --dry-run 2>&1)"
CODE5B=$?
set -e
[ "$CODE5B" -eq 0 ] || fail "plan-only --dry-run must not require checksums (exit $CODE5B): $OUT5B"
case "$OUT5B" in
    *"would fetch binary"*) : ;;
    *) fail "plan-only --dry-run must print the fetch plan: $OUT5B" ;;
esac

# 5c. Local staging with an explicit ANOLISA_BIN_URL still fetches the binary
#     during staging, even under --dry-run, so --strict keeps requiring its
#     checksum there.
set +e
OUT5C="$(env ANOLISA_PREFIX="$TEMPORARY/prefix5c" ANOLISA_BIN_URL="file://$FETCHED" \
    bash "$INSTALLER" --from-local "$SRC" --strict --dry-run 2>&1)"
CODE5C=$?
set -e
[ "$CODE5C" -eq 2 ] || fail "staged fetch under --dry-run must still be gated (exit $CODE5C): $OUT5C"
case "$OUT5C" in
    *ANOLISA_BIN_SHA256*) : ;;
    *) fail "staged-fetch refusal must name ANOLISA_BIN_SHA256: $OUT5C" ;;
esac

# 6. Without --strict the explicit-URL opt-in keeps working and warns.
PREFIX6="$TEMPORARY/prefix6"
set +e
OUT6="$(env ANOLISA_PREFIX="$PREFIX6" ANOLISA_BIN_URL="file://$FETCHED" \
    bash "$INSTALLER" --from-local "$SRC" 2>&1)"
CODE6=$?
set -e
[ "$CODE6" -eq 0 ] || fail "non-strict explicit-URL run must succeed (exit $CODE6): $OUT6"
case "$OUT6" in
    *"skipping binary checksum"*) : ;;
    *) fail "non-strict run must warn about the skipped checksum: $OUT6" ;;
esac

echo "ok - --strict gates every fetch, including an explicit binary URL from local staging"
