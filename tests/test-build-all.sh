#!/usr/bin/env bash
# Exercise build orchestration with recorded commands, never real toolchains.
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_ROOT="$(cd "$TEST_DIR/.." && pwd)"
TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

fail() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
contains() { grep -Fq -- "$2" "$1" || fail "missing '$2' in $1"; }
absent() {
    if grep -Fq -- "$2" "$1"; then fail "unexpected '$2' in $1"; fi
}
absent_event() {
    if grep -Fxq -- "$1" "$EVENT_LOG"; then fail "unexpected event '$1'"; fi
}
absent_install() {
    if grep -Eq '^make\|[^|]+\|install[^|]*\|' "$EVENT_LOG"; then fail 'unexpected installation'; fi
}
assert_component_event() {
    local event="$1" expected="$2" actual
    actual="$(awk -F '|' -v event="$event" '$1 == event { print $2 }' "$EVENT_LOG")"
    [[ "$(tr ' ' '\n' <<< "$actual" | sort)" == "$(tr ' ' '\n' <<< "$expected" | sort)" ]] ||
        fail "unexpected $event selection: $actual"
}

make_provider_fixture() {
    local backend="$1" package="$2"
    mkdir -p "$FIXTURE_ROOT/providers/$backend"
    : > "$FIXTURE_ROOT/providers/$backend/Makefile"
    printf '[package]\nname = "%s"\nversion = "0.1.0"\n' "$package" > "$FIXTURE_ROOT/providers/$backend/Cargo.toml"
}

setup_case() {
    local name="$1" component
    CASE_ROOT="$TEST_TMP/$name"
    FIXTURE_ROOT="$CASE_ROOT/repository with spaces"
    TEST_HOME="$CASE_ROOT/home"
    FAKE_BIN="$CASE_ROOT/bin"
    EVENT_LOG="$CASE_ROOT/events"
    OUTPUT="$CASE_ROOT/output"
    mkdir -p "$FIXTURE_ROOT/scripts" "$FIXTURE_ROOT/target" "$TEST_HOME" "$FAKE_BIN"
    cp "$SOURCE_ROOT/scripts/build-all.sh" "$FIXTURE_ROOT/scripts/"
    cp -R "$SOURCE_ROOT/scripts/lib" "$FIXTURE_ROOT/scripts/"
    for component in cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw agent-memory os-skills; do
        mkdir -p "$FIXTURE_ROOT/src/$component"
        : > "$FIXTURE_ROOT/src/$component/Makefile"
    done
    make_provider_fixture agent-sec-core aw-provider-sec-core
    make_provider_fixture tokenless aw-provider-tokenless
    printf 'preserve build cache\n' > "$FIXTURE_ROOT/target/sentinel"
    printf 'preserve user configuration\n' > "$TEST_HOME/.bashrc"
    : > "$EVENT_LOG"

    cat > "$FAKE_BIN/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
directory='' target='' need_directory=false
for argument in "$@"; do
    if $need_directory; then directory="$argument"; need_directory=false; continue; fi
    case "$argument" in
        -C) need_directory=true ;;
        build|build-core|build-core-with-cosh|build-all|install|install-core|install-core-with-cosh|install-core-and-skills|install-core-and-skills-with-cosh) target="$argument" ;;
    esac
done
component="${directory##*/}"
case "$directory" in
    */providers/*) component="$(awk -F '"' '/^name[[:space:]]*=/ { print $2; exit }' "$directory/Cargo.toml")" ;;
esac
printf 'make|%s|%s' "$component" "$target" >> "$EVENT_LOG"
printf '|%s' "$@" >> "$EVENT_LOG"
printf '|CARGO_BUILD_JOBS=%s' "${CARGO_BUILD_JOBS:-}" >> "$EVENT_LOG"
printf '\n' >> "$EVENT_LOG"
if [[ "$target" == build* && "$component" == "${FAIL_COMPONENT:-}" ]]; then exit 23; fi
if [[ -n "${NATIVE_MAKE:-}" ]]; then
    if [[ "$component" == agent-sec-core && "$target" == build ]]; then
        # Keep native BUILD_DIR resolution and delivery; replace compilation only.
        native_arguments=()
        for argument in "$@"; do
            if [[ "$argument" == build ]]; then native_arguments+=(fixture-build)
            else native_arguments+=("$argument"); fi
        done
        exec "$NATIVE_MAKE" "${native_arguments[@]}" -f Makefile -f "$directory/fixture-build.mk"
    fi
    exec "$NATIVE_MAKE" "$@" "CARGO=${BASH_SOURCE[0]%/*}/cargo"
fi
SH
    chmod +x "$FAKE_BIN/make"
    # A regression must fail locally instead of reaching the network or host.
    for component in curl wget uv pip pip3 npm rustup rustc cargo apt-get dnf yum sudo systemctl; do
        cat > "$FAKE_BIN/$component" <<'SH'
#!/usr/bin/env bash
printf 'forbidden|%s|%s\n' "${0##*/}" "$*" >> "$EVENT_LOG"
exit 97
SH
        chmod +x "$FAKE_BIN/$component"
    done

    cat > "$CASE_ROOT/runner" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
source "$FIXTURE_ROOT/scripts/build-all.sh"
prepare_build_environment() {
    printf 'prepare\n' >> "$EVENT_LOG"
    printf 'prepare-components|%s\n' "${COMPONENTS[*]}" >> "$EVENT_LOG"
    printf 'prepare-policy|%s|%s\n' "$INSTALL_MODE" "${INSTALL_SYSTEM_DEPS:-auto}" >> "$EVENT_LOG"
    [[ "${FAIL_PREPARE:-false}" != true ]]
}
check_build_environment() {
    printf 'check\n' >> "$EVENT_LOG"
    printf 'check-components|%s\n' "${COMPONENTS[*]}" >> "$EVENT_LOG"
    [[ "${FAIL_CHECK:-false}" != true ]]
}
preflight_runtime_dependencies() {
    printf 'runtime\n' >> "$EVENT_LOG"
    [[ "${FAIL_RUNTIME:-false}" != true ]]
}
as_root() {
    if $DRY_RUN; then printf 'DRY-RUN: privileged file installation\n'; return 0; fi
    printf 'root\n' >> "$EVENT_LOG"
    env -u CARGO_TARGET_DIR -u BUILD_DIR "$@"
}
main "$@"
SH
}

invoke() {
    STATUS=0
    HOME="$TEST_HOME" PATH="$FAKE_BIN:/usr/bin:/bin" \
        FIXTURE_ROOT="$FIXTURE_ROOT" EVENT_LOG="$EVENT_LOG" \
        FAIL_COMPONENT="${FAIL_COMPONENT:-}" FAIL_RUNTIME="${FAIL_RUNTIME:-false}" \
        FAIL_PREPARE="${FAIL_PREPARE:-false}" FAIL_CHECK="${FAIL_CHECK:-false}" \
        NATIVE_MAKE="${NATIVE_MAKE:-}" \
        bash "$CASE_ROOT/runner" "$@" > "$OUTPUT" 2>&1 || STATUS=$?
}

success() {
    if [[ "$STATUS" -ne 0 ]]; then cat "$OUTPUT" >&2; fail "unexpected exit $STATUS"; fi
    absent "$EVENT_LOG" 'forbidden|'
}

test_default_builds_all_before_install() {
    setup_case default
    invoke --ignore-deps
    success
    local expected='cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw'
    local actual
    actual="$(awk -F '|' '$1 == "make" && $3 ~ /^build/ && $2 !~ /^aw-provider-/ { print $2 }' "$EVENT_LOG" | paste -sd ' ' -)"
    [[ "$actual" == "$expected" ]] || fail "unexpected default backend build order: $actual"
    contains "$EVENT_LOG" 'make|aw-provider-sec-core|build|'
    contains "$EVENT_LOG" 'make|aw-provider-tokenless|build|'
    [[ "$(grep -Ec '^make\|[^|]+\|build[^|]*\|' "$EVENT_LOG")" -eq 9 ]] || fail 'expected nine builds'
    [[ "$(grep -Ec '^make\|[^|]+\|install[^|]*\|' "$EVENT_LOG")" -eq 9 ]] || fail 'expected nine installs'
    contains "$EVENT_LOG" 'make|ws-ckpt|build-core|'
    contains "$EVENT_LOG" 'make|ws-ckpt|install-core-and-skills|'
    contains "$EVENT_LOG" 'make|agent-sec-core|build-core-with-cosh|'
    contains "$EVENT_LOG" 'make|agent-sec-core|install-core-and-skills-with-cosh|'
    contains "$EVENT_LOG" 'make|tokenless|build-core|'
    contains "$EVENT_LOG" 'make|tokenless|install-core-with-cosh|'
    contains "$EVENT_LOG" 'make|agentsight|build-all|'
    contains "$EVENT_LOG" 'make|agentsight|install-core|'
    local last_build first_install
    last_build="$(awk -F '|' '$1 == "make" && $3 ~ /^build/ { n = NR } END { print n }' "$EVENT_LOG")"
    first_install="$(awk -F '|' '$1 == "make" && $3 ~ /^install/ { print NR; exit }' "$EVENT_LOG")"
    (( last_build < first_install )) || fail 'component installation preceded a later build'
    contains "$EVENT_LOG" 'INSTALL_PROFILE=system'
    contains "$EVENT_LOG" 'PREFIX=/usr/local'
    absent "$EVENT_LOG" 'legacy'
    [[ "$(cat "$FIXTURE_ROOT/target/sentinel")" == 'preserve build cache' ]] || fail 'build cache was removed'
    for component in prepare check runtime; do absent_event "$component"; done
}

test_aliases_are_normalized_and_deduplicated() {
    setup_case aliases
    invoke --component sec-core --component agent-sec-core --component sight --component agentsight --ignore-deps --no-install
    success
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 3 ]] || fail 'aliases or their bound provider were built repeatedly'
    contains "$EVENT_LOG" 'make|agent-sec-core|build|'
    contains "$EVENT_LOG" 'make|agentsight|build|'
    contains "$EVENT_LOG" 'make|aw-provider-sec-core|build|'
    absent "$EVENT_LOG" 'aw-provider-agent-sec-core'
    absent "$EVENT_LOG" '|install|'
}

test_partial_backend_builds_its_provider() {
    local backend provider
    for backend in tokenless sec-core; do
        setup_case "backend-$backend"
        if [[ "$backend" == tokenless ]]; then provider=aw-provider-tokenless
        else provider=aw-provider-sec-core; fi
        invoke --component "$backend" --no-install
        success
        [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 2 ]] || fail 'partial build omitted its provider or selected unrelated components'
        contains "$EVENT_LOG" "make|$provider|build|"
        contains "$EVENT_LOG" "prepare-components|${backend/sec-core/agent-sec-core} $provider"
        absent "$EVENT_LOG" '|install|'
    done
}

test_standalone_plugins_are_kept_when_aw_is_not_selected() {
    setup_case without-aw
    invoke --component tokenless --component agent-memory --ignore-deps --destdir "$CASE_ROOT/stage"
    success
    for component in tokenless agent-memory; do
        contains "$EVENT_LOG" "make|$component|build|"
        contains "$EVENT_LOG" "make|$component|install|"
    done
    contains "$EVENT_LOG" 'make|aw-provider-tokenless|install|'

    setup_case memory-with-aw
    invoke --component agent-memory --component aw --ignore-deps --destdir "$CASE_ROOT/stage"
    success
    contains "$EVENT_LOG" 'make|agent-memory|build-core|'
    contains "$EVENT_LOG" 'make|agent-memory|install-core|'
}

test_provider_selected_explicitly_and_implicitly_is_built_once() {
    setup_case provider-deduplicated
    invoke --component tokenless --component aw-provider-tokenless --ignore-deps --no-install
    success
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 2 ]] || fail 'explicit and companion provider selections were not deduplicated'
}

test_future_provider_is_discovered_from_the_selected_backend() {
    setup_case future-provider
    make_provider_fixture skillfs aw-provider-skillfs
    make_provider_fixture agent-memory aw-provider-memory
    invoke --component skillfs --no-install
    success
    assert_component_event prepare-components 'skillfs aw-provider-skillfs'
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 2 ]] || fail 'future provider was not bound to its backend'
    contains "$EVENT_LOG" 'make|aw-provider-skillfs|build|'
    contains "$EVENT_LOG" "-C|$FIXTURE_ROOT/providers/skillfs|build"
    absent "$EVENT_LOG" 'make|aw-provider-memory|'

    : > "$EVENT_LOG"
    invoke --all --check-deps
    success
    assert_component_event check-components 'cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw aw-provider-tokenless aw-provider-sec-core aw-provider-skillfs'

    : > "$EVENT_LOG"
    invoke --component aw-provider-skillfs --ignore-deps --no-install
    success
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 1 ]] || fail 'explicit future provider added its backend'
    contains "$EVENT_LOG" 'make|aw-provider-skillfs|build|'
}

test_default_build_does_not_require_provider_directories() {
    setup_case default-without-providers
    rm -rf "$FIXTURE_ROOT/providers"
    invoke --all --ignore-deps --no-install
    success
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 7 ]] || fail 'default selection still required missing providers'
    absent "$EVENT_LOG" 'aw-provider-'
}

test_backend_without_buildable_provider_does_not_add_a_target() {
    local missing
    for missing in directory makefile manifest provider-package; do
        setup_case "missing-provider-$missing"
        case "$missing" in
            directory) rm -rf "$FIXTURE_ROOT/providers/tokenless" ;;
            makefile) rm "$FIXTURE_ROOT/providers/tokenless/Makefile" ;;
            manifest) rm "$FIXTURE_ROOT/providers/tokenless/Cargo.toml" ;;
            provider-package) make_provider_fixture tokenless tokenless-helper ;;
        esac
        invoke --component tokenless --no-install
        success
        assert_component_event prepare-components tokenless
        [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 1 ]] || fail "unbuildable provider added a target: $missing"
        contains "$EVENT_LOG" 'make|tokenless|build|'
    done
}

test_partial_provider_build_does_not_add_backends() {
    setup_case provider
    invoke --component aw-provider-tokenless --ignore-deps --no-install
    success
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 1 ]] || fail 'provider selection unexpectedly built other components'
    contains "$EVENT_LOG" 'make|aw-provider-tokenless|build|'
}

test_deps_only_does_not_build_or_check_runtime() {
    setup_case deps
    invoke --component tokenless --deps-only
    success
    contains "$EVENT_LOG" 'prepare'
    absent "$EVENT_LOG" 'make|'
    absent_event runtime
    absent_event root
}

test_check_deps_is_read_only() {
    setup_case check
    invoke --all --check-deps
    success
    assert_component_event check-components 'cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw aw-provider-sec-core aw-provider-tokenless'
    [[ "$(wc -l < "$EVENT_LOG")" -eq 2 ]] || fail 'check-deps performed other work'
}

test_no_install_prepares_then_builds_only() {
    setup_case no-install
    invoke --component tokenless --no-install
    success
    [[ "$(head -1 "$EVENT_LOG")" == prepare ]] || fail 'environment preparation did not precede build'
    assert_component_event prepare-components 'tokenless aw-provider-tokenless'
    contains "$EVENT_LOG" 'make|tokenless|build|'
    absent "$EVENT_LOG" '|install|'
    absent_event runtime
    absent_event root
}

test_staging_has_no_runtime_or_privileged_install() {
    setup_case staging
    local destination="$CASE_ROOT/package root"
    invoke --component sec-core --component tokenless --component aw --ignore-deps --no-install --destdir "$destination" --prefix /opt/anolisa
    success
    contains "$EVENT_LOG" 'make|agent-sec-core|build-core|'
    contains "$EVENT_LOG" 'make|agent-sec-core|install-core-and-skills|'
    contains "$EVENT_LOG" 'make|tokenless|install-core|'
    contains "$EVENT_LOG" 'make|aw|install|'
    contains "$EVENT_LOG" 'make|aw-provider-sec-core|install|'
    contains "$EVENT_LOG" "DESTDIR=$destination"
    contains "$EVENT_LOG" 'PREFIX=/opt/anolisa'
    absent_event runtime
    absent_event root
}

test_build_failure_prevents_every_install() {
    setup_case failed-build
    FAIL_COMPONENT=tokenless
    invoke --component cosh-ng --component tokenless --component aw --ignore-deps
    unset FAIL_COMPONENT
    [[ "$STATUS" -ne 0 ]] || fail 'build failure was ignored'
    contains "$EVENT_LOG" 'make|tokenless|build-core|'
    absent "$EVENT_LOG" 'make|aw|'
    absent_install
}

test_runtime_failure_prevents_every_install() {
    setup_case failed-runtime
    FAIL_RUNTIME=true
    invoke --component tokenless
    unset FAIL_RUNTIME
    [[ "$STATUS" -ne 0 ]] || fail 'runtime failure was ignored'
    contains "$EVENT_LOG" runtime
    absent "$EVENT_LOG" '|install|'
}

test_prepare_failure_prevents_builds() {
    setup_case failed-prepare
    FAIL_PREPARE=true
    invoke --component tokenless --no-install
    unset FAIL_PREPARE
    [[ "$STATUS" -ne 0 ]] || fail 'environment preparation failure was ignored'
    absent "$EVENT_LOG" 'make|'
}

test_dry_run_preserves_home_and_build_cache() {
    setup_case dry-run
    local before after
    before="$(find "$TEST_HOME" "$FIXTURE_ROOT/target" -type f -exec sha256sum '{}' + | sort)"
    invoke --all --dry-run
    success
    absent "$EVENT_LOG" 'make|'
    absent_event root
    after="$(find "$TEST_HOME" "$FIXTURE_ROOT/target" -type f -exec sha256sum '{}' + | sort)"
    [[ "$before" == "$after" ]] || fail 'dry-run changed the home or build tree'
    contains "$OUTPUT" 'aw-provider-tokenless'
    contains "$OUTPUT" 'skillfs'
}

test_invalid_selection_and_paths_fail_before_work() {
    local value
    for value in ../escape unknown cosh; do
        setup_case "invalid-${value//\//_}"
        invoke --component "$value" --ignore-deps --no-install
        [[ "$STATUS" -ne 0 ]] || fail "invalid component accepted: $value"
        [[ ! -s "$EVENT_LOG" ]] || fail 'invalid selection dispatched commands'
    done
    for value in relative / /tmp/../unsafe; do
        setup_case "invalid-root-${value//\//_}"
        invoke --component aw --destdir "$value" --ignore-deps
        [[ "$STATUS" -ne 0 ]] || fail "invalid package root accepted: $value"
        [[ ! -s "$EVENT_LOG" ]] || fail 'invalid package root dispatched commands'
    done
}

test_job_budget_reaches_component_builds() {
    setup_case jobs
    invoke --component aw --no-install --ignore-deps --jobs 3
    success
    contains "$EVENT_LOG" 'CARGO_BUILD_JOBS=3'
    local value
    for value in 0 -1 invalid 1.5; do
        setup_case "invalid-jobs-$value"
        invoke --component aw --no-install --ignore-deps --jobs "$value"
        [[ "$STATUS" -ne 0 ]] || fail "invalid job count accepted: $value"
        [[ ! -s "$EVENT_LOG" ]] || fail 'invalid job count dispatched commands'
    done
}

setup_native_aw_case() {
    setup_case "$1"
    local old_root="$FIXTURE_ROOT" relative directory file
    FIXTURE_ROOT="$CASE_ROOT/native-repository"
    cp -R "$old_root" "$FIXTURE_ROOT"
    NATIVE_MAKE="$(command -v make)"
    printf 'fixture license\n' > "$FIXTURE_ROOT/LICENSE"
    for relative in src/aw providers/agent-sec-core providers/tokenless; do
        directory="$FIXTURE_ROOT/$relative"
        mkdir -p "$directory/packaging"
        for file in Makefile packaging/build.mk packaging/install.mk; do
            cp "$SOURCE_ROOT/$relative/$file" "$directory/$file"
        done
        printf 'fixture documentation\n' > "$directory/README.md"
        printf 'fixture documentation\n' > "$directory/README_zh.md"
        if [[ "$relative" == providers/* ]]; then
            mkdir -p "$directory/schemas" "$directory/examples"
            printf '{}\n' > "$directory/schemas/health-v1alpha1.schema.json"
            printf 'fixture\n' > "$directory/examples/aw.yaml"
        fi
        local binary=aw
        if [[ "$relative" == providers/agent-sec-core ]]; then binary=aw-provider-sec-core
        elif [[ "$relative" == providers/tokenless ]]; then binary=aw-provider-tokenless; fi
        mkdir -p "$directory/target/release"
        printf 'stale %s\n' "$binary" > "$directory/target/release/$binary"
        chmod +x "$directory/target/release/$binary"
    done
    cat > "$FAKE_BIN/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
target='' binary=''
while (( $# )); do
    case "$1" in
        --target-dir) target="$2"; shift 2 ;;
        --bin) binary="$2"; shift 2 ;;
        *) shift ;;
    esac
done
[[ -n "$target" && -n "$binary" ]] || exit 98
mkdir -p "$target/release"
printf 'fresh %s\n' "$binary" > "$target/release/$binary"
chmod +x "$target/release/$binary"
SH
    chmod +x "$FAKE_BIN/cargo"
}

assert_native_aw_installation() {
    local prefix="$1" backend binary installed
    for backend in sec-core tokenless; do
        binary="aw-provider-$backend"
        installed="$prefix/libexec/anolisa/aw/providers/$backend/$binary"
        [[ "$(cat "$installed")" == "fresh $binary" ]] || fail "$binary installed a stale binary"
    done
    [[ "$(cat "$prefix/bin/aw")" == 'fresh aw' ]] || fail 'AW installed a stale binary'
}

test_aw_native_custom_targets_survive_privilege_filtering() {
    local NATIVE_MAKE kind output prefix
    for kind in absolute relative; do
        setup_native_aw_case "native-target-$kind"
        prefix="$CASE_ROOT/prefix"
        if [[ "$kind" == absolute ]]; then output="$CASE_ROOT/custom output"
        else output='relative output'; fi
        CARGO_TARGET_DIR="$output" invoke --component aw --component aw-provider-sec-core \
            --component aw-provider-tokenless --ignore-deps --prefix "$prefix"
        success
        assert_native_aw_installation "$prefix"
        [[ "$(grep -c '^root$' "$EVENT_LOG")" -eq 3 ]] || fail 'system install did not exercise privilege filtering'
        if [[ "$kind" == relative ]]; then
            contains "$EVENT_LOG" "CARGO_TARGET_DIR=$FIXTURE_ROOT/src/aw/$output"
            contains "$EVENT_LOG" "CARGO_TARGET_DIR=$FIXTURE_ROOT/providers/agent-sec-core/$output"
            contains "$EVENT_LOG" "CARGO_TARGET_DIR=$FIXTURE_ROOT/providers/tokenless/$output"
        fi
    done
}

test_aw_native_default_target_paths_are_preserved() {
    local NATIVE_MAKE CARGO_TARGET_DIR
    unset CARGO_TARGET_DIR
    setup_native_aw_case native-target-default
    local prefix="$CASE_ROOT/prefix"
    invoke --component aw --component aw-provider-sec-core --component aw-provider-tokenless \
        --ignore-deps --prefix "$prefix"
    success
    assert_native_aw_installation "$prefix"
    absent "$EVENT_LOG" '|CARGO_TARGET_DIR='
}

test_aw_native_custom_targets_work_without_elevation() {
    local NATIVE_MAKE mode prefix destination
    local -a options
    for mode in user staging; do
        setup_native_aw_case "native-target-$mode"
        if [[ "$mode" == user ]]; then
            prefix="$TEST_HOME/.local"
            options=(--user)
        else
            destination="$CASE_ROOT/stage"
            prefix="$destination/usr"
            options=(--destdir "$destination" --prefix /usr)
        fi
        CARGO_TARGET_DIR='relative output' invoke --component aw --component aw-provider-sec-core \
            --component aw-provider-tokenless --ignore-deps "${options[@]}"
        success
        assert_native_aw_installation "$prefix"
        absent_event root
    done
}

setup_native_sec_core_case() {
    setup_case "$1"
    local old_root="$FIXTURE_ROOT" directory
    FIXTURE_ROOT="$CASE_ROOT/native-repository"
    cp -R "$old_root" "$FIXTURE_ROOT"
    rm -rf "$FIXTURE_ROOT/providers"
    NATIVE_MAKE="$(command -v make)"
    directory="$FIXTURE_ROOT/src/agent-sec-core"
    python3 - "$SOURCE_ROOT" "$directory" <<'PY'
from pathlib import Path
import runpy
import shutil
import sys

source, destination = map(Path, sys.argv[1:])
scope = runpy.run_path(str(source / "src/agent-sec-core/tests/v2/packaging/test_delivery.py"))
fixture = scope["DeliveryTests"]()
try:
    fixture.setUp()
    binaries = ("v2/bin/agent-sec-cli", "v2/bin/agent-sec-daemon", "v2/bin/agent-sec-daemon-health", "linux-sandbox")
    for name in binaries:
        fixture.artifact(name, f"#!/bin/sh\n# stale {Path(name).name}\nexit 0\n", executable=True)
    shutil.copytree(fixture.source, destination, dirs_exist_ok=True)
    fresh = destination / "fixture-artifacts"
    shutil.copytree(destination / "target", fresh)
    for name in binaries:
        (fresh / name).write_text(f"#!/bin/sh\n# fresh {Path(name).name}\nexit 0\n")
finally:
    fixture.doCleanups()
PY
    cat > "$directory/fixture-build.mk" <<'MAKE'
.PHONY: fixture-build
fixture-build:
	install -d -m 0755 "$(BUILD_DIR)"
	cp -a fixture-artifacts/. "$(BUILD_DIR)/"
MAKE
}

assert_native_sec_core_installation() {
    local prefix="$1" binary
    for binary in agent-sec-cli agent-sec-daemon agent-sec-daemon-health linux-sandbox; do
        grep -Fxq "# fresh $binary" "$prefix/bin/$binary" || fail "sec-core installed a stale $binary"
    done
}

test_sec_core_custom_build_dir_survives_privilege_filtering() {
    local NATIVE_MAKE kind output prefix
    for kind in absolute relative; do
        setup_native_sec_core_case "native-sec-core-$kind"
        prefix="$CASE_ROOT/prefix"
        if [[ "$kind" == absolute ]]; then output="$CASE_ROOT/custom-build"
        else output=custom-build; fi
        BUILD_DIR="$output" invoke --component sec-core --ignore-deps --prefix "$prefix"
        success
        assert_native_sec_core_installation "$prefix"
        [[ "$(grep -c '^root$' "$EVENT_LOG")" -eq 1 ]] || fail 'sec-core install did not exercise privilege filtering'
        if [[ "$kind" == relative ]]; then
            contains "$EVENT_LOG" "|BUILD_DIR=$output"
        fi
    done
}

test_sec_core_default_build_dir_is_preserved() {
    local NATIVE_MAKE BUILD_DIR
    unset BUILD_DIR
    setup_native_sec_core_case native-sec-core-default
    local prefix="$CASE_ROOT/prefix"
    invoke --component sec-core --ignore-deps --prefix "$prefix"
    success
    assert_native_sec_core_installation "$prefix"
    absent "$EVENT_LOG" '|BUILD_DIR='
}

test_sec_core_custom_build_dir_supports_build_only_and_staging() {
    local NATIVE_MAKE mode
    for mode in build-only staging; do
        setup_native_sec_core_case "native-sec-core-$mode"
        if [[ "$mode" == build-only ]]; then
            BUILD_DIR=custom-build invoke --component sec-core --ignore-deps --user --no-install
            success
            grep -Fxq '# fresh agent-sec-cli' "$FIXTURE_ROOT/src/agent-sec-core/custom-build/v2/bin/agent-sec-cli" || fail 'sec-core build did not use the selected output'
            absent "$EVENT_LOG" '|install|'
        else
            BUILD_DIR=custom-build invoke --component sec-core --ignore-deps --prefix /usr --destdir "$CASE_ROOT/stage"
            success
            assert_native_sec_core_installation "$CASE_ROOT/stage/usr"
        fi
        absent_event root
    done
}

test_sec_core_relative_build_dir_keeps_native_staging_in_a_spaced_checkout() {
    local NATIVE_MAKE directory staged
    setup_case native-sec-core-spaced-checkout
    rm -rf "$FIXTURE_ROOT/providers"
    NATIVE_MAKE="$(command -v make)"
    directory="$FIXTURE_ROOT/src/agent-sec-core"
    mkdir -p "$directory/packaging" "$directory/hermes-plugin/src" "$directory/hermes-plugin/scripts"
    cp "$SOURCE_ROOT/src/agent-sec-core/packaging/build.mk" "$directory/packaging/build.mk"
    printf 'include packaging/build.mk\n' > "$directory/Makefile"
    printf '.PHONY: fixture-build\nfixture-build: build-hermes-plugin\n' > "$directory/fixture-build.mk"
    printf 'fresh native staging\n' > "$directory/hermes-plugin/src/plugin.yaml"
    printf 'fresh native script\n' > "$directory/hermes-plugin/scripts/deploy.sh"

    BUILD_DIR=custom-build invoke --component sec-core --ignore-deps --user --no-install

    success
    staged="$directory/custom-build/hermes-plugin"
    cmp -s "$directory/hermes-plugin/src/plugin.yaml" "$staged/src/plugin.yaml" || fail 'relative BUILD_DIR changed native staging paths'
    cmp -s "$directory/hermes-plugin/scripts/deploy.sh" "$staged/scripts/deploy.sh" || fail 'relative BUILD_DIR changed native script staging'
    contains "$EVENT_LOG" '|BUILD_DIR=custom-build'
    absent_event root
}

test_user_profile_can_compile_system_only_components() {
    setup_case user-build
    invoke --all --user --no-install
    success
    [[ "$(grep -Ec '^make\|[^|]+\|build[^|]*\|' "$EVENT_LOG")" -eq 9 ]] || fail 'user build omitted system-only components'
    assert_component_event prepare-components 'cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw aw-provider-sec-core aw-provider-tokenless'
    contains "$EVENT_LOG" 'make|agent-sec-core|build-core-with-cosh|'
    contains "$EVENT_LOG" 'INSTALL_PROFILE=user'
    absent_install
    absent_event root
    absent_event runtime
}

test_user_install_rejection_precedes_environment_changes() {
    setup_case user-install
    invoke --component ws-ckpt --user
    [[ "$STATUS" -ne 0 ]] || fail 'unsupported user service installation was accepted'
    [[ ! -s "$EVENT_LOG" ]] || fail 'unsupported user installation prepared or built components'
}

test_user_all_install_reports_every_unsupported_component_before_work() {
    setup_case user-all-install
    invoke --all --user
    [[ "$STATUS" -ne 0 ]] || fail 'unsupported all-component user installation was accepted'
    local component
    for component in skillfs ws-ckpt agentsight agent-sec-core; do
        contains "$OUTPUT" "$component"
    done
    [[ ! -s "$EVENT_LOG" ]] || fail 'unsupported user installation prepared or built components'
}

test_user_all_dependency_actions_keep_the_complete_selection() {
    local action expected_event
    for action in --deps-only --check-deps; do
        setup_case "user-all-${action#--}"
        invoke --all --user "$action"
        success
        if [[ "$action" == --deps-only ]]; then expected_event=prepare; else expected_event=check; fi
        assert_component_event "$expected_event-components" 'cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw aw-provider-sec-core aw-provider-tokenless'
        absent "$EVENT_LOG" 'make|'
        absent_event root
        absent_event runtime
    done
}

test_supported_only_filters_before_dependency_preparation_and_installation() {
    local mode component actual
    local expected='cosh-ng tokenless aw aw-provider-tokenless'
    for mode in install staging; do
        setup_case "supported-only-$mode"
        local -a options=(--all --user --supported-only)
        if [[ "$mode" == staging ]]; then options+=(--destdir "$CASE_ROOT/package root"); fi
        invoke "${options[@]}"
        success
        contains "$EVENT_LOG" "prepare-components|$expected"
        contains "$EVENT_LOG" 'prepare-policy|user|auto'
        actual="$(awk -F '|' '$1 == "make" && $3 ~ /^build/ { print $2 }' "$EVENT_LOG" | paste -sd ' ' -)"
        [[ "$actual" == "$expected" ]] || fail "unexpected supported-only build set: $actual"
        actual="$(awk -F '|' '$1 == "make" && $3 ~ /^install/ { print $2 }' "$EVENT_LOG" | paste -sd ' ' -)"
        [[ "$actual" == "$expected" ]] || fail "unexpected supported-only install set: $actual"
        contains "$EVENT_LOG" 'make|tokenless|install-core-with-cosh|'
        contains "$EVENT_LOG" 'INSTALL_PROFILE=user'
        contains "$EVENT_LOG" "PREFIX=$TEST_HOME/.local"
        for component in skillfs ws-ckpt agentsight agent-sec-core; do
            contains "$OUTPUT" "$component"
            absent "$EVENT_LOG" "make|$component|"
        done
        absent "$EVENT_LOG" 'make|aw-provider-sec-core|'
        grep -Eiq 'skip(ping|ped)?' "$OUTPUT" || fail 'filtered components were not reported as skipped'
        absent_event root
        if [[ "$mode" == staging ]]; then
            contains "$EVENT_LOG" "DESTDIR=$CASE_ROOT/package root"
            absent_event runtime
        fi
    done
}

test_supported_only_filters_build_and_dependency_actions() {
    local action expected_event
    local expected='cosh-ng tokenless aw aw-provider-tokenless'
    for action in --no-install --deps-only --check-deps; do
        setup_case "supported-only-${action#--}"
        invoke --all --user --supported-only "$action"
        success
        if [[ "$action" == --check-deps ]]; then expected_event=check; else expected_event=prepare; fi
        contains "$EVENT_LOG" "$expected_event-components|$expected"
        if [[ "$action" == --no-install ]]; then
            [[ "$(grep -Ec '^make\|[^|]+\|build(-core)?\|' "$EVENT_LOG")" -eq 4 ]] || fail 'supported-only build did not select four components'
        else absent "$EVENT_LOG" 'make|'; fi
        absent_install
        absent_event root
        absent_event runtime
    done
}

test_supported_only_filters_explicit_components_and_rejects_an_empty_selection() {
    setup_case supported-only-partial
    invoke --component skillfs --component tokenless --component sec-core --user --supported-only
    success
    assert_component_event prepare-components 'tokenless aw-provider-tokenless'
    [[ "$(grep -c '^make|' "$EVENT_LOG")" -eq 4 ]] || fail 'explicit supported-only selection dispatched unexpected components'
    absent "$EVENT_LOG" 'make|aw-provider-sec-core|'
    contains "$OUTPUT" skillfs
    contains "$OUTPUT" agent-sec-core
    absent_event root

    setup_case supported-only-empty
    invoke --component skillfs --component sec-core --user --supported-only
    [[ "$STATUS" -ne 0 ]] || fail 'fully filtered selection was accepted'
    [[ ! -s "$EVENT_LOG" ]] || fail 'fully filtered selection dispatched commands'
}

test_supported_only_requires_the_user_profile() {
    setup_case supported-only-system
    invoke --all --system --supported-only --no-install
    [[ "$STATUS" -ne 0 ]] || fail 'supported-only accepted the system profile'
    [[ ! -s "$EVENT_LOG" ]] || fail 'invalid supported-only profile dispatched commands'
}

test_explicit_system_dependency_permission_reaches_preparation() {
    setup_case user-system-deps
    invoke --component tokenless --user --no-install --install-system-deps
    success
    contains "$EVENT_LOG" 'prepare-policy|user|true'
    absent_event root
}

test_system_prefix_defaults_and_overrides() {
    local profile
    local -a options
    for profile in system install-mode; do
        setup_case "system-prefix-$profile"
        if [[ "$profile" == system ]]; then options=(--system)
        else options=(--install-mode system); fi
        invoke --component aw "${options[@]}" --no-install --ignore-deps
        success
        contains "$EVENT_LOG" 'INSTALL_PROFILE=system'
        contains "$EVENT_LOG" '|PREFIX=/usr/local|'
    done

    setup_case package-prefix
    invoke --component aw --system --ignore-deps --destdir "$CASE_ROOT/package root" --prefix /usr
    success
    contains "$EVENT_LOG" '|PREFIX=/usr|'
    absent "$EVENT_LOG" '|PREFIX=/usr/local|'
    absent_event root

    setup_case user-prefix
    invoke --component aw --user --ignore-deps
    success
    contains "$EVENT_LOG" "|PREFIX=$TEST_HOME/.local|"
    absent_event root
}

test_legacy_usr_option_is_rejected_before_any_changes() {
    setup_case rejected-usr
    invoke --component aw --usr --no-install --ignore-deps
    [[ "$STATUS" -ne 0 ]] || fail 'legacy --usr option was accepted'
    contains "$OUTPUT" '--system'
    contains "$OUTPUT" '--prefix /usr'
    [[ ! -s "$EVENT_LOG" ]] || fail 'rejected --usr option dispatched commands'

    setup_case help-without-usr
    invoke --help
    success
    if grep -Eq -- '(^|[[:space:]|,])--usr([[:space:]|,]|$)' "$OUTPUT"; then
        fail 'help still advertised --usr'
    fi
}

test_runtime_is_checked_once_before_any_host_install() {
    setup_case install
    invoke --component tokenless --component aw
    success
    [[ "$(grep -c '^runtime$' "$EVENT_LOG")" -eq 1 ]] || fail 'runtime preflight did not run once for the complete selection'
    local runtime_line first_install
    runtime_line="$(grep -n '^runtime$' "$EVENT_LOG" | cut -d: -f1)"
    first_install="$(awk -F '|' '$1 == "make" && $3 ~ /^install/ { print NR; exit }' "$EVENT_LOG")"
    (( runtime_line < first_install )) || fail 'runtime preflight ran after files were installed'
}

test_legacy_uninstall_is_rejected_before_any_changes() {
    local component
    for component in ws-ckpt cosh-ng sec-core; do
        setup_case "uninstall-$component"
        invoke --component "$component" --uninstall
        [[ "$STATUS" -ne 0 ]] || fail 'legacy uninstall was accepted'
        contains "$OUTPUT" 'component-specific removal procedure'
        [[ ! -s "$EVENT_LOG" ]] || fail 'rejected uninstall dispatched commands'
    done
}

for test in $(declare -F | awk '{ print $3 }' | sort); do
    [[ "$test" == test_* ]] || continue
    "$test"
    printf 'ok - %s\n' "$test"
done
