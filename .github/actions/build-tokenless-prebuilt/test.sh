#!/usr/bin/env bash
# Exercise Tokenless action input binding and shared multi-project SBOM generation.
set -euo pipefail

ACTION_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
COMMON_DIR="$(cd "$ACTION_DIR/../prebuilt-rust-common" && pwd)"
REPO_ROOT="$(git -C "$ACTION_DIR" rev-parse --show-toplevel)"
TEMPORARY="$(mktemp -d)"
HISTORICAL_ARCHIVE_NAME="tokenless-historical-${TEMPORARY##*/}"
HISTORICAL_BUILD="/tmp/build/$HISTORICAL_ARCHIVE_NAME"
HISTORICAL_DRIFT_BUILD="${HISTORICAL_BUILD}-drift"
trap 'rm -rf -- "$TEMPORARY" "$HISTORICAL_BUILD" "$HISTORICAL_DRIFT_BUILD"' EXIT

python3 - \
    "$ACTION_DIR/action.yaml" \
    "$REPO_ROOT/.github/workflows/release-preview.yaml" \
    "$ACTION_DIR/build.sh" <<'PY'
import sys
from pathlib import Path


action = Path(sys.argv[1]).read_text(encoding="utf-8")
pinned_uv = "uses: astral-sh/setup-uv@v8.0.0\n      with:\n        version: '0.11.7'"
if pinned_uv not in action:
    raise SystemExit("composite action does not install the pinned uv release")
expected_bindings = (
    "TOKENLESS_WHEEL_DIR: ${{ runner.temp }}/tokenless-python-wheels-${{ inputs.target-os }}-${{ inputs.target-arch }}",
    "TOKENLESS_SOURCE_WORKTREE: ${{ runner.temp }}/anolisa-raw-release-source-worktrees/tokenless",
    "TOKENLESS_VERSION: ${{ inputs.version }}",
    "TOKENLESS_TARGET_OS: ${{ inputs.target-os }}",
    "TOKENLESS_TARGET_ARCH: ${{ inputs.target-arch }}",
    "TOKENLESS_PROFILE: ${{ inputs.profile }}",
    "TOKENLESS_TAG: ${{ inputs.tag }}",
    "wheel-directory=$TOKENLESS_WHEEL_DIR",
)
for binding in expected_bindings:
    if binding not in action:
        raise SystemExit(f"composite action is missing environment binding: {binding}")

marker = "      run: |\n"
if marker not in action:
    raise SystemExit("composite action is missing its Bash run block")
run_block = action.split(marker, 1)[1]
if "${{ inputs." in run_block:
    raise SystemExit("composite action interpolates an input directly into Bash")

preview = Path(sys.argv[2]).read_text(encoding="utf-8")
expected_preview_bindings = (
    "      - uses: actions/checkout@v4\n\n      - name: Checkout Tokenless source tag",
    "if: inputs.tokenless_source_tag != ''",
    "description: 'Optional exact Tokenless source tag with Python packaging (v0.7.7+, e.g. tokenless/v0.7.14)'",
    'if [ "$SOURCE_TAG" != "tokenless/v$VERSION" ]; then',
    "ref: ${{ format('refs/tags/{0}', inputs.tokenless_source_tag) }}",
    "path: tokenless-source",
    "tokenless-source-root: ${{ inputs.tokenless_source_tag != '' && 'tokenless-source' || '.' }}",
    "python3 .github/actions/prebuilt-rust-common/plan_matrix.py",
)
for binding in expected_preview_bindings:
    if binding not in preview:
        raise SystemExit(f"preview workflow does not bind every artifact to the source tag: {binding}")

build = Path(sys.argv[3]).read_text(encoding="utf-8")
if 'bash "$ACTION_DIR/setup-rtk.sh" "$COMPONENT_ROOT"' not in build:
    raise SystemExit("prebuilt build does not use the shared immutable RTK setup")
maturin = build.index('uvx --from "maturin==$MATURIN_VERSION" maturin build')
wrapper = build.rfind('python3 "$COMMON_DIR/reproducible-build.py"', 0, maturin)
if wrapper == -1:
    raise SystemExit("Maturin build does not use the reproducible environment")
wrapped_command = build[wrapper:maturin]
for binding in (
    '--source-root "$COMPONENT_ROOT"',
    '--source-date-epoch "$SOURCE_DATE_EPOCH"',
):
    if binding not in wrapped_command:
        raise SystemExit(f"Maturin reproducible build is missing: {binding}")
if 'TOKENLESS_CARGO_MANIFEST="$COMPONENT_ROOT/python/tokenless/Cargo.toml"' not in build:
    raise SystemExit("Maturin Cargo shim does not bind the expected manifest")
if 'TOKENLESS_CROSS_PROJECT_ROOT="$COMPONENT_ROOT"' not in build:
    raise SystemExit("Maturin Cargo shim does not bind the Cross project root")
if 'TOKENLESS_CARGO_OUTPUT_REWRITER="$ACTION_DIR/rewrite-cross-cargo-output.py"' not in build:
    raise SystemExit("Maturin Cargo shim does not bind the output rewriter")
PY

python3 - "$REPO_ROOT/.github/actions/package-source/action.yaml" <<'PY'
import sys
from pathlib import Path


action = Path(sys.argv[1]).read_text(encoding="utf-8")
setup_command = (
    'bash "$GITHUB_ACTION_PATH/../build-tokenless-prebuilt/setup-rtk.sh" "$PWD"'
)
if setup_command not in action:
    raise SystemExit("source packaging does not use the shared immutable RTK setup")
if "git clone --depth 1 --branch" in action:
    raise SystemExit("source packaging still resolves RTK through a mutable tag")
expected_source_root_bindings = (
    "tokenless-source-root:",
    "TOKENLESS_SOURCE_ROOT: ${{ inputs.tokenless-source-root }}",
    'tokenless)     SRC_DIR="${TOKENLESS_SOURCE_ROOT}/src/tokenless" ;;',
)
for binding in expected_source_root_bindings:
    if binding not in action:
        raise SystemExit(f"source packaging does not isolate tagged Tokenless sources: {binding}")
PY

install -d -m 0755 "$HISTORICAL_BUILD"
HISTORICAL_SOURCE_REF="refs/tags/tokenless/v0.7.12"
if ! git cat-file -e "${HISTORICAL_SOURCE_REF}^{commit}" 2>/dev/null; then
    git fetch --no-tags --depth=1 origin "$HISTORICAL_SOURCE_REF"
    HISTORICAL_SOURCE_REF="FETCH_HEAD"
fi
git archive "${HISTORICAL_SOURCE_REF}:src/tokenless" | tar -x -C "$HISTORICAL_BUILD"
[ ! -f "$HISTORICAL_BUILD/scripts/setup-rtk.sh" ] || {
    printf 'ERROR: historical source fixture unexpectedly has setup-rtk.sh\n' >&2
    exit 1
}
cp -a "$HISTORICAL_BUILD" "$HISTORICAL_DRIFT_BUILD"

HISTORICAL_RTK_SETUP="$ACTION_DIR/setup-rtk.sh"
PINNED_RTK_COMMIT="$(
    sed -n 's/^[[:space:]]*v0\.43\.0) RTK_COMMIT="\([0-9a-f]\{40\}\)".*/\1/p' \
        "$HISTORICAL_RTK_SETUP"
)"
CURRENT_RTK_COMMIT="$(
    sed -n 's/^RTK_COMMIT="\([0-9a-f]\{40\}\)"$/\1/p' \
        "$REPO_ROOT/src/tokenless/scripts/setup-rtk.sh"
)"
[ -n "$PINNED_RTK_COMMIT" ] || {
    printf 'ERROR: historical RTK setup has no pinned 40-character commit\n' >&2
    exit 1
}
[ -n "$CURRENT_RTK_COMMIT" ] || {
    printf 'ERROR: current RTK setup has no pinned 40-character commit\n' >&2
    exit 1
}

HISTORICAL_FAKE_BIN="$TEMPORARY/historical-fake-bin"
install -d -m 0755 "$HISTORICAL_FAKE_BIN"
# shellcheck disable=SC2016  # Expand the fixture variables when the fake runs.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s\n" "$*" >> "$HISTORICAL_GIT_LOG"' \
    'case "${1:-}" in' \
    '    init)' \
    '        destination="${!#}"' \
    '        mkdir -p "$destination/.git"' \
    '        ;;' \
    '    -C)' \
    '        repository="$2"' \
    '        shift 2' \
    '        case "${1:-}" in' \
    '            remote | fetch) ;;' \
    '            checkout)' \
    '                printf "[workspace]\n" > "$repository/Cargo.toml"' \
    '                printf "version = 3\n" > "$repository/Cargo.lock"' \
    '                ;;' \
    '            rev-parse) printf "%s\n" "$HISTORICAL_FAKE_HEAD" ;;' \
    '            *) exit 91 ;;' \
    '        esac' \
    '        ;;' \
    '    *) exit 92 ;;' \
    'esac' \
    > "$HISTORICAL_FAKE_BIN/git"
# shellcheck disable=SC2016  # Expand the fixture variables when the fake runs.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "%s\n" "$*" >> "$HISTORICAL_PATCH_LOG"' \
    'cat >/dev/null' \
    > "$HISTORICAL_FAKE_BIN/patch"
chmod 0755 "$HISTORICAL_FAKE_BIN/git" "$HISTORICAL_FAKE_BIN/patch"

python3 - "$REPO_ROOT/.github/actions/package-source/action.yaml" \
    "$TEMPORARY/vendor-rtk-step.sh" <<'PY'
import sys
from pathlib import Path


lines = Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()
start = lines.index("    - name: Vendor rtk source (tokenless)")
run = lines.index("      run: |", start) + 1
end = next(
    index for index in range(run, len(lines)) if lines[index].startswith("    - name:")
)
script = "\n".join(line[8:] for line in lines[run:end]) + "\n"
Path(sys.argv[2]).write_text(script, encoding="utf-8")
PY
HISTORICAL_GIT_LOG="$TEMPORARY/historical-git.log"
HISTORICAL_PATCH_LOG="$TEMPORARY/historical-patch.log"
PATH="$HISTORICAL_FAKE_BIN:$PATH" \
    ARCHIVE_NAME="$HISTORICAL_ARCHIVE_NAME" \
    GITHUB_ACTION_PATH="$REPO_ROOT/.github/actions/package-source" \
    HISTORICAL_FAKE_HEAD="$PINNED_RTK_COMMIT" \
    HISTORICAL_GIT_LOG="$HISTORICAL_GIT_LOG" \
    HISTORICAL_PATCH_LOG="$HISTORICAL_PATCH_LOG" \
    bash "$TEMPORARY/vendor-rtk-step.sh"
grep -Fq "fetch --quiet --depth 1 origin $PINNED_RTK_COMMIT" \
    "$HISTORICAL_GIT_LOG"
if grep -Fq -- '--branch' "$HISTORICAL_GIT_LOG"; then
    printf 'ERROR: historical RTK setup fetched a mutable tag\n' >&2
    exit 1
fi
[ "$(wc -l < "$HISTORICAL_PATCH_LOG")" -eq 2 ]
[ -f "$HISTORICAL_BUILD/third_party/rtk/Cargo.toml" ]
[ -f "$HISTORICAL_BUILD/third_party/rtk/Cargo.lock" ]
[ "$(cat "$HISTORICAL_BUILD/third_party/rtk/.anolisa-rtk-commit")" = \
    "$PINNED_RTK_COMMIT" ]
[ ! -e "$HISTORICAL_BUILD/third_party/rtk/.git" ]
make -C "$HISTORICAL_BUILD" generate-component-contract >/dev/null
[ -f "$HISTORICAL_BUILD/.anolisa/component.toml" ]

if PATH="$HISTORICAL_FAKE_BIN:$PATH" \
    HISTORICAL_FAKE_HEAD="$(printf '%040d' 0)" \
    HISTORICAL_GIT_LOG="$TEMPORARY/historical-drift-git.log" \
    HISTORICAL_PATCH_LOG="$TEMPORARY/historical-drift-patch.log" \
    bash "$HISTORICAL_RTK_SETUP" "$HISTORICAL_DRIFT_BUILD" \
        >"$TEMPORARY/historical-drift.log" 2>&1; then
    printf 'ERROR: historical RTK setup accepted the wrong commit\n' >&2
    exit 1
fi
grep -Fq "does not match pinned commit $PINNED_RTK_COMMIT" \
    "$TEMPORARY/historical-drift.log"
[ ! -e "$HISTORICAL_DRIFT_BUILD/third_party/rtk" ]

# Current sources delegate to their component setup and can pin a newer RTK.
CURRENT_BUILD="$TEMPORARY/current-source"
install -d -m 0755 "$CURRENT_BUILD/scripts" "$CURRENT_BUILD/third_party/patches"
cp "$REPO_ROOT/src/tokenless/scripts/setup-rtk.sh" "$CURRENT_BUILD/scripts/"
cp "$REPO_ROOT/src/tokenless/third_party/patches/rtk-tokenless-stats.patch" \
    "$REPO_ROOT/src/tokenless/third_party/patches/rtk-pytest-error-report.patch" \
    "$CURRENT_BUILD/third_party/patches/"
PATH="$HISTORICAL_FAKE_BIN:$PATH" \
    HISTORICAL_FAKE_HEAD="$CURRENT_RTK_COMMIT" \
    HISTORICAL_GIT_LOG="$TEMPORARY/current-git.log" \
    HISTORICAL_PATCH_LOG="$TEMPORARY/current-patch.log" \
    bash "$HISTORICAL_RTK_SETUP" "$CURRENT_BUILD"
grep -Fq "fetch --quiet --depth 1 origin $CURRENT_RTK_COMMIT" \
    "$TEMPORARY/current-git.log"
[ "$(wc -l < "$TEMPORARY/current-patch.log")" -eq 2 ]
[ "$(cat "$CURRENT_BUILD/third_party/rtk/.anolisa-rtk-commit")" = \
    "$CURRENT_RTK_COMMIT" ]

RTK_DRIFT="$TEMPORARY/rtk-drift"
install -d -m 0755 "$RTK_DRIFT"
printf '[package]\nname = "rtk"\nversion = "0.0.0"\n' > "$RTK_DRIFT/Cargo.toml"
printf '%040d\n' 0 > "$RTK_DRIFT/.anolisa-rtk-commit"
if bash "$REPO_ROOT/src/tokenless/scripts/setup-rtk.sh" "$RTK_DRIFT" \
    >"$TEMPORARY/rtk-drift.log" 2>&1; then
    printf 'ERROR: mismatched RTK revision marker was accepted\n' >&2
    exit 1
fi
grep -Fq "does not match pinned commit $CURRENT_RTK_COMMIT" \
    "$TEMPORARY/rtk-drift.log"

OPENCLAW_ROOT="$REPO_ROOT/src/tokenless/adapters/tokenless/openclaw"
OPENCLAW_FIXTURE="$TEMPORARY/openclaw"
TOKENLESS_VERSION="$(python3 - "$REPO_ROOT/src/tokenless/Cargo.toml" <<'PY'
import sys
import tomllib
from pathlib import Path


manifest = tomllib.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
print(manifest["workspace"]["package"]["version"])
PY
)"
python3 - "$OPENCLAW_ROOT/package-lock.json" "$TOKENLESS_VERSION" <<'PY'
import json
import sys
from pathlib import Path


lockfile = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
expected = sys.argv[2]
versions = {
    lockfile.get("version"),
    lockfile.get("packages", {}).get("", {}).get("version"),
}
if versions != {expected}:
    raise SystemExit(f"OpenClaw lockfile version does not match Tokenless {expected}")
PY
install -d -m 0755 "$OPENCLAW_FIXTURE"
sed "s/@VERSION@/$TOKENLESS_VERSION/g" "$OPENCLAW_ROOT/package.json.in" \
    > "$OPENCLAW_FIXTURE/package.json"
cp "$OPENCLAW_ROOT/package-lock.json" "$OPENCLAW_FIXTURE/package-lock.json"
(
    cd "$OPENCLAW_FIXTURE"
    npm ci --legacy-peer-deps --ignore-scripts --no-audit --no-fund >/dev/null
)
sed -i 's/"typescript": "\^5.8.0"/"typescript": "0.0.1"/' \
    "$OPENCLAW_FIXTURE/package.json"
if (
    cd "$OPENCLAW_FIXTURE"
    npm ci --legacy-peer-deps --ignore-scripts --no-audit --no-fund \
        >"$TEMPORARY/npm-drift.log" 2>&1
); then
    printf 'ERROR: npm lockfile drift was accepted\n' >&2
    exit 1
fi

MARKER="$TEMPORARY/injected"
if TOKENLESS_SOURCE_WORKTREE="$TEMPORARY/version-worktree/tokenless" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/version-output" \
        --wheel-output-dir "$TEMPORARY/version-wheels" \
        --version "0.7.12\$(touch ${MARKER})" \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.17-x86_64 \
        --tag '' >"$TEMPORARY/version.log" 2>&1; then
    printf 'ERROR: malicious version input was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: malicious version input executed a command\n' >&2
    exit 1
}

if TOKENLESS_SOURCE_WORKTREE="$TEMPORARY/tag-worktree/tokenless" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$REPO_ROOT" \
        --output-dir "$TEMPORARY/tag-output" \
        --wheel-output-dir "$TEMPORARY/tag-wheels" \
        --version 0.7.12 \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.17-x86_64 \
        --tag "tokenless/v0.7.12\$(touch ${MARKER})" \
        >"$TEMPORARY/tag.log" 2>&1; then
    printf 'ERROR: malicious tag input was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: malicious tag input executed a command\n' >&2
    exit 1
}

SOURCE_FIXTURE="$TEMPORARY/source-fixture"
install -d -m 0755 "$SOURCE_FIXTURE/src/tokenless"
git -C "$SOURCE_FIXTURE" init -q
git -C "$SOURCE_FIXTURE" config user.name 'Tokenless CI Test'
git -C "$SOURCE_FIXTURE" config user.email 'tokenless-ci@example.com'
printf 'tag source\n' > "$SOURCE_FIXTURE/src/tokenless/source.txt"
git -C "$SOURCE_FIXTURE" add src/tokenless/source.txt
git -C "$SOURCE_FIXTURE" commit -q -m 'tag source'
TAG_COMMIT="$(git -C "$SOURCE_FIXTURE" rev-parse HEAD)"
git -C "$SOURCE_FIXTURE" tag tokenless/v1.2.3
printf 'checkout source\n' > "$SOURCE_FIXTURE/src/tokenless/source.txt"
git -C "$SOURCE_FIXTURE" commit -q -am 'checkout source'
git -C "$SOURCE_FIXTURE" branch tokenless/v1.2.3

FAKE_BIN="$TEMPORARY/fake-bin"
install -d -m 0755 "$FAKE_BIN"
# shellcheck disable=SC2016  # Expand the fixture variable when the fake runs.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'git rev-parse HEAD > "$SOURCE_SELECTION_RESULT"' \
    'exit 73' > "$FAKE_BIN/make"
printf '%s\n' '#!/usr/bin/env bash' 'exit 74' > "$FAKE_BIN/uvx"
printf '%s\n' '#!/usr/bin/env bash' 'exit 75' > "$FAKE_BIN/just"
chmod 0755 "$FAKE_BIN/make" "$FAKE_BIN/uvx" "$FAKE_BIN/just"
SOURCE_SELECTION_RESULT="$TEMPORARY/source-selection"
if PATH="$FAKE_BIN:$PATH" \
    SOURCE_SELECTION_RESULT="$SOURCE_SELECTION_RESULT" \
    TOKENLESS_SOURCE_WORKTREE="$TEMPORARY/source-worktree/tokenless" \
    "$ACTION_DIR/build.sh" \
        --source-repo "$SOURCE_FIXTURE" \
        --output-dir "$TEMPORARY/source-output" \
        --wheel-output-dir "$TEMPORARY/source-wheels" \
        --version 1.2.3 \
        --target-os linux \
        --target-arch x86_64 \
        --profile gnu2.17-x86_64 \
        --tag tokenless/v1.2.3 >"$TEMPORARY/source.log" 2>&1; then
    printf 'ERROR: source selection fixture unexpectedly completed\n' >&2
    exit 1
fi
[ -f "$SOURCE_SELECTION_RESULT" ] || {
    cat "$TEMPORARY/source.log" >&2
    printf 'ERROR: source selection did not reach the tagged worktree\n' >&2
    exit 1
}
[ "$(cat "$SOURCE_SELECTION_RESULT")" = "$TAG_COMMIT" ] || {
    printf 'ERROR: tagged build used the checkout commit instead of the tag commit\n' >&2
    exit 1
}

SHIM_LOG="$TEMPORARY/cargo-shim.log"
SHIM_RUSTFLAGS_LOG="$TEMPORARY/cargo-shim-rustflags.log"
SHIM_COMPONENT="$TEMPORARY/shim-component"
install -d -m 0755 "$SHIM_COMPONENT"
touch "$SHIM_COMPONENT/Cargo.toml"
SHIM_BIN="$SHIM_COMPONENT/target/maturin-cargo-shim"
install -d -m 0755 "$SHIM_BIN"
ln -s "$ACTION_DIR/cargo-shim.sh" "$SHIM_BIN/cargo"
# shellcheck disable=SC2016  # Expand shim arguments and log paths in the fakes.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "cargo" >> "$SHIM_LOG"' \
    'printf " <%s>" "$@" >> "$SHIM_LOG"' \
    'printf "\n" >> "$SHIM_LOG"' > "$FAKE_BIN/host-cargo"
# shellcheck disable=SC2016  # Expand shim arguments and log paths in the fakes.
printf '%s\n' \
    '#!/usr/bin/env bash' \
    'printf "cross" >> "$SHIM_LOG"' \
    'printf " <%s>" "$@" >> "$SHIM_LOG"' \
    'printf "\n" >> "$SHIM_LOG"' \
    'printf "%s\n" "${CARGO_ENCODED_RUSTFLAGS:-}" >> "$SHIM_RUSTFLAGS_LOG"' \
    'printf "%s\n" '\''{"reason":"compiler-artifact","package_id":"path+file:///project#tokenless-python@0.7.14","filenames":["/target/x86_64-unknown-linux-gnu/python-release/libanolisa_tokenless.so"],"manifest_path":"/project/Cargo.toml"}'\''' \
    > "$FAKE_BIN/cross-profile"
chmod 0755 "$FAKE_BIN/host-cargo" "$FAKE_BIN/cross-profile"
SHIM_ENV=(
    TOKENLESS_HOST_CARGO="$FAKE_BIN/host-cargo"
    TOKENLESS_CROSS_PROFILE_SCRIPT="$FAKE_BIN/cross-profile"
    TOKENLESS_CROSS_PROFILE=gnu2.17-x86_64
    TOKENLESS_RUST_TARGET=x86_64-unknown-linux-gnu
    TOKENLESS_CARGO_MANIFEST="$SHIM_COMPONENT/Cargo.toml"
    TOKENLESS_CROSS_PROJECT_ROOT="$SHIM_COMPONENT"
    TOKENLESS_CARGO_OUTPUT_REWRITER="$ACTION_DIR/rewrite-cross-cargo-output.py"
    CARGO_ENCODED_RUSTFLAGS=--remap-path-prefix=/source=/workspace
    SHIM_LOG="$SHIM_LOG"
    SHIM_RUSTFLAGS_LOG="$SHIM_RUSTFLAGS_LOG"
)
env "${SHIM_ENV[@]}" "$SHIM_BIN/cargo" metadata --locked
SHIM_OUTPUT="$TEMPORARY/cargo-shim-output.json"
(
    cd "$SHIM_COMPONENT"
    env "${SHIM_ENV[@]}" "$SHIM_BIN/cargo" rustc \
        --target x86_64-unknown-linux-gnu \
        --manifest-path "$SHIM_COMPONENT/Cargo.toml" \
        --profile python-release > "$SHIM_OUTPUT"
)
grep -Fxq 'cargo <metadata> <--locked>' "$SHIM_LOG"
grep -Fxq \
    'cross <gnu2.17-x86_64> <rustc> <--manifest-path> <Cargo.toml> <--profile> <python-release>' \
    "$SHIM_LOG"
grep -Fxq -- '--remap-path-prefix=/source=/workspace' "$SHIM_RUSTFLAGS_LOG"
grep -Fq \
    "\"package_id\":\"path+file://$SHIM_COMPONENT#tokenless-python@0.7.14\"" \
    "$SHIM_OUTPUT"
grep -Fq \
    "\"filenames\":[\"$SHIM_COMPONENT/target/x86_64-unknown-linux-gnu/python-release/libanolisa_tokenless.so\"]" \
    "$SHIM_OUTPUT"
grep -Fq "\"manifest_path\":\"$SHIM_COMPONENT/Cargo.toml\"" "$SHIM_OUTPUT"
if env "${SHIM_ENV[@]}" "$SHIM_BIN/cargo" rustc \
    --target "x86_64-unknown-linux-gnu;\$(touch ${MARKER})" \
    >"$TEMPORARY/shim-target.log" 2>&1; then
    printf 'ERROR: mismatched Cargo shim target was accepted\n' >&2
    exit 1
fi
[ ! -e "$MARKER" ] || {
    printf 'ERROR: Cargo shim target executed a command\n' >&2
    exit 1
}
if (
    cd "$SHIM_COMPONENT"
    env "${SHIM_ENV[@]}" "$SHIM_BIN/cargo" rustc \
        --target x86_64-unknown-linux-gnu \
        --manifest-path "$TEMPORARY/other/Cargo.toml"
) >"$TEMPORARY/shim-manifest.log" 2>&1; then
    printf 'ERROR: unexpected Cargo shim manifest path was accepted\n' >&2
    exit 1
fi

python3 "$ACTION_DIR/test_verify_wheels.py"

for project in tokenless-fixture rtk-fixture; do
    install -d -m 0755 "$TEMPORARY/$project/src"
    printf 'fn main() {}\n' > "$TEMPORARY/$project/src/main.rs"
    sed "s/@NAME@/$project/" > "$TEMPORARY/$project/Cargo.toml" <<'EOF'
[workspace]

[package]
name = "@NAME@"
version = "1.0.0"
edition = "2021"
EOF
    cargo generate-lockfile --manifest-path "$TEMPORARY/$project/Cargo.toml"
done

printf 'Tokenless SBOM fixture\n' > "$TEMPORARY/payload.txt"
ARTIFACT_ROOT="$TEMPORARY/artifacts"
for target in \
    'linux x86_64 x86_64-unknown-linux-gnu' \
    'linux aarch64 aarch64-unknown-linux-gnu' \
    'macos aarch64 aarch64-apple-darwin'; do
    read -r target_os target_arch target_triple <<<"$target"
    artifact_dir="$ARTIFACT_ROOT/tokenless-prebuilt-1.0.0-$target_os-$target_arch"
    archive="$artifact_dir/tokenless-1.0.0-$target_os-$target_arch.tar.gz"
    install -d -m 0755 "$artifact_dir"
    tar -C "$TEMPORARY" -czf "$archive" payload.txt
    (
        cd "$artifact_dir"
        sha256sum "${archive##*/}" > "${archive##*/}.sha256"
    )
    python3 "$COMMON_DIR/generate-sbom.py" \
        --artifact "$archive" \
        --component tokenless \
        --version 1.0.0 \
        --os "$target_os" \
        --arch "$target_arch" \
        --target "$target_triple" \
        --project-dir "$TEMPORARY/tokenless-fixture" \
        --project-dir "$TEMPORARY/rtk-fixture" \
        --source-date-epoch 0 >/dev/null
    python3 "$COMMON_DIR/verify-artifacts.py" \
        --directory "$artifact_dir" \
        --component tokenless \
        --version 1.0.0 \
        --layout flat \
        --os "$target_os" \
        --arch "$target_arch"
done

python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$ARTIFACT_ROOT" \
    --component tokenless \
    --version 1.0.0 \
    --layout actions

python3 - "$ARTIFACT_ROOT/tokenless-prebuilt-1.0.0-linux-x86_64/tokenless-1.0.0-linux-x86_64.tar.gz.cdx.json" <<'PY'
import json
import sys
from pathlib import Path


document = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
components = list(document["components"])
components.extend(document["metadata"]["component"].get("components", []))
names = {component["name"] for component in components}
expected = {"tokenless-fixture", "rtk-fixture"}
missing = expected - names
if missing:
    raise SystemExit(f"multi-project SBOM is missing components: {sorted(missing)}")
PY

MISSING_ROOT="$TEMPORARY/missing-artifacts"
cp -a "$ARTIFACT_ROOT" "$MISSING_ROOT"
rm "$MISSING_ROOT/tokenless-prebuilt-1.0.0-linux-x86_64/"*.sha256
if python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$MISSING_ROOT" \
    --component tokenless \
    --version 1.0.0 \
    --layout actions >/dev/null 2>&1; then
    printf 'ERROR: incomplete Actions Artifact set was accepted\n' >&2
    exit 1
fi

EXTRA_ROOT="$TEMPORARY/extra-artifacts"
cp -a "$ARTIFACT_ROOT" "$EXTRA_ROOT"
touch "$EXTRA_ROOT/tokenless-prebuilt-1.0.0-linux-x86_64/unexpected"
if python3 "$COMMON_DIR/verify-artifacts.py" \
    --directory "$EXTRA_ROOT" \
    --component tokenless \
    --version 1.0.0 \
    --layout actions >/dev/null 2>&1; then
    printf 'ERROR: Actions Artifact set with an extra file was accepted\n' >&2
    exit 1
fi

printf 'Tokenless prebuilt action tests passed\n'
