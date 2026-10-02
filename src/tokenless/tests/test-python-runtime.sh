#!/usr/bin/env bash
# Build, install, and exercise the native Tokenless Python wheel.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WHEEL_DIR="$ROOT/target/wheels"
TEST_ENV="$(mktemp -d /tmp/tokenless-python-wheel-test.XXXXXX)"
trap 'rm -rf "$TEST_ENV"' EXIT

mapfile -t WHEELS < <(find "$WHEEL_DIR" -maxdepth 1 -type f \
    -name 'anolisa_tokenless-*.whl' -print)
if [ "${#WHEELS[@]}" -ne 1 ]; then
    printf 'ERROR: expected exactly one anolisa-tokenless wheel, found %s\n' \
        "${#WHEELS[@]}" >&2
    exit 1
fi

case "${WHEELS[0]}" in
    *-cp311-abi3-*.whl) ;;
    *)
        printf 'ERROR: wheel is not tagged for the CPython 3.11 stable ABI: %s\n' \
            "${WHEELS[0]}" >&2
        exit 1
        ;;
esac

python3 -m venv "$TEST_ENV/venv"
"$TEST_ENV/venv/bin/python" -m pip install \
    --disable-pip-version-check --no-deps "${WHEELS[0]}" >/dev/null
env PATH=/usr/bin:/bin "$TEST_ENV/venv/bin/python" -m unittest discover \
    -s "$ROOT/python/tokenless/tests" -v

env PATH=/usr/bin:/bin "$TEST_ENV/venv/bin/python" - <<'PY'
import os
from importlib.resources import files

rtk = files("anolisa_tokenless").joinpath("_bin", "rtk")
assert rtk.is_file()
assert os.access(rtk, os.X_OK)
PY

EXPECTED_VERSION="$(sed -n \
    's/^version = "\([^"]*\)"/\1/p' "$ROOT/Cargo.toml" | head -1)"
ACTUAL_VERSION="$("$TEST_ENV/venv/bin/python" -c \
    'import anolisa_tokenless; print(anolisa_tokenless.__version__)')"
if [ "$ACTUAL_VERSION" != "$EXPECTED_VERSION" ]; then
    printf 'ERROR: Python wheel version %s does not match workspace version %s\n' \
        "$ACTUAL_VERSION" "$EXPECTED_VERSION" >&2
    exit 1
fi

printf 'Tokenless Python wheel smoke test passed (%s)\n' "$ACTUAL_VERSION"
