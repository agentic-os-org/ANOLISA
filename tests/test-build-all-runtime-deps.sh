#!/usr/bin/env bash
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/.." && pwd)"

# build-all.sh guards main when sourced, which lets these tests exercise the
# preflight without building components or mutating the host package state.
# shellcheck source=../scripts/build-all.sh
source "$PROJECT_ROOT/scripts/build-all.sh"
# shellcheck source=../scripts/lib/build-runtime.sh
source "$PROJECT_ROOT/scripts/lib/build-runtime.sh"

TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT

fail() {
    echo "not ok - $*" >&2
    return 1
}

assert_contains() {
    local file="$1" expected="$2"
    grep -Fq -- "$expected" "$file" || fail "expected '$expected' in output"
}

assert_not_contains() {
    local file="$1" unexpected="$2"
    if grep -Fq -- "$unexpected" "$file"; then
        fail "did not expect '$unexpected' in output"
    fi
}

declare -a TEST_DEPENDENCIES=()
declare -A PRESENT_DEPENDENCIES=()

selected_runtime_dependencies() {
    printf '%s\n' "${TEST_DEPENDENCIES[@]}"
}

runtime_dependency_present() {
    local name="$1"
    RUNTIME_DEP_DETAIL="missing test dependency"
    [[ "${PRESENT_DEPENDENCIES[$name]:-false}" == "true" ]]
}

id() {
    if [[ "${1:-}" == "-u" ]]; then
        echo 1000
    else
        command id "$@"
    fi
}

as_root() {
    AS_ROOT_CALLS=$((AS_ROOT_CALLS + 1))
    AS_ROOT_HISTORY+="$*"$'\n'
    if [[ "$*" == "apt-get update" ]]; then
        [[ "$APT_UPDATE_RESULT" == "success" ]]
        return
    fi
    AS_ROOT_ARGS="$*"
    if [[ "$INSTALL_RESULT" == "success" ]]; then
        local record name
        for record in "${TEST_DEPENDENCIES[@]}"; do
            IFS='|' read -r _ name _ <<< "$record"
            PRESENT_DEPENDENCIES[$name]=true
        done
        return 0
    fi
    [[ "$INSTALL_RESULT" != "error" ]]
}

reset_preflight_stubs() {
    INSTALL_MODE="user"
    INSTALL_DEPS=true
    DO_INSTALL=true
    DEPS_ONLY=false
    DRY_RUN=false
    PKG_BASE="deb"
    PKG_INSTALL="apt-get install -y"
    COMPONENTS=(sec-core tokenless ws-ckpt)
    AS_ROOT_CALLS=0
    AS_ROOT_ARGS=""
    AS_ROOT_HISTORY=""
    APT_UPDATE_RESULT="success"
    INSTALL_RESULT="none"
    TEST_STATUS=0
    TEST_OUTPUT="$TEST_TMP/output"
    RUNTIME_SYSTEM_PATH="$TEST_TMP/system-bin"
    rm -rf "$RUNTIME_SYSTEM_PATH"
    mkdir -p "$RUNTIME_SYSTEM_PATH"
    : > "$TEST_OUTPUT"
    TEST_DEPENDENCIES=(
        'sec-core|bubblewrap|system-package|bwrap --version|bubblewrap|bubblewrap||||'
        'tokenless|bash|system-package|bash --version|bash|bash||||'
        'ws-ckpt|rsync|system-package|rsync --version|rsync|rsync||||'
    )
    PRESENT_DEPENDENCIES=()
}

run_preflight() {
    set +e
    preflight_runtime_dependencies > "$TEST_OUTPUT" 2>&1
    TEST_STATUS=$?
    set -e
}

test_manifest_parser_uses_toml_keys_not_order() {
    reset_preflight_stubs
    local manifest="$TEST_TMP/reordered-component.toml"
    local output="$TEST_TMP/reordered-dependencies"
    printf '%s\n' \
        '[[component.dependencies]] # reordered but equivalent' \
        'packages={ deb = '\''gnupg'\'',rpm="gnupg2" }' \
        'probe = '\''gpg --version'\''' \
        'kind="system-package"' \
        'name = "gnupg"' \
        > "$manifest"

    runtime_dependencies_for_manifest sec-core "$manifest" > "$output"

    assert_contains "$output" 'sec-core|gnupg|system-package|gpg --version|gnupg2|gnupg|||'
    [[ "$(wc -l < "$output")" -eq 1 ]] || fail "reordered TOML emitted extra records"
}

test_user_reports_all_components_once_without_root() {
    reset_preflight_stubs
    TEST_DEPENDENCIES+=(
        'cosh-ng|bash|system-package|bash --version|bash|bash||||'
        'ws-ckpt|btrfs|platform-capability||||btrfs||5.4|'
    )

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "user preflight unexpectedly succeeded"
    [[ $AS_ROOT_CALLS -eq 0 ]] || fail "user preflight invoked as_root"
    assert_contains "$TEST_OUTPUT" 'sec-core: bubblewrap [system-package]'
    assert_contains "$TEST_OUTPUT" 'tokenless: bash [system-package]'
    assert_contains "$TEST_OUTPUT" 'cosh-ng: bash [system-package]'
    assert_contains "$TEST_OUTPUT" 'ws-ckpt: btrfs [platform-capability]'
    assert_contains "$TEST_OUTPUT" 'sudo apt-get install -y bubblewrap bash rsync'
    [[ "$(grep -o ' apt-get install ' "$TEST_OUTPUT" | wc -l)" -eq 1 ]] || \
        fail "expected one aggregated install command"
}

test_system_installs_packages_once_and_reprobes() {
    reset_preflight_stubs
    INSTALL_MODE="system"
    INSTALL_RESULT="success"

    run_preflight

    [[ $TEST_STATUS -eq 0 ]] || fail "system preflight did not recover"
    [[ $AS_ROOT_CALLS -eq 2 ]] || fail "expected one APT refresh and package transaction"
    [[ "$AS_ROOT_HISTORY" == \
        $'apt-get update\napt-get install -y bubblewrap bash rsync\n' ]] || \
        fail "unexpected APT transaction order: $AS_ROOT_HISTORY"
    [[ "$AS_ROOT_ARGS" == 'apt-get install -y bubblewrap bash rsync' ]] || \
        fail "unexpected package transaction: $AS_ROOT_ARGS"
    assert_contains "$TEST_OUTPUT" 'installed and verified'
}

test_system_stops_before_packages_for_platform_blocker() {
    reset_preflight_stubs
    INSTALL_MODE="system"
    INSTALL_RESULT="success"
    TEST_DEPENDENCIES+=(
        'sight|ebpf-btf|platform-capability||||btf||5.8|'
    )

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "platform blocker unexpectedly succeeded"
    [[ $AS_ROOT_CALLS -eq 0 ]] || fail "packages changed before platform validation"
    assert_contains "$TEST_OUTPUT" 'sight: ebpf-btf [platform-capability]'
}

test_system_reprobe_failure_reports_every_dependency() {
    reset_preflight_stubs
    INSTALL_MODE="system"
    INSTALL_RESULT="none"

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "failed re-probe unexpectedly succeeded"
    [[ $AS_ROOT_CALLS -eq 2 ]] || fail "expected one APT refresh and package transaction"
    assert_contains "$TEST_OUTPUT" 'still missing after package installation'
    assert_contains "$TEST_OUTPUT" 'sec-core: bubblewrap'
    assert_contains "$TEST_OUTPUT" 'ws-ckpt: rsync'
}

test_system_apt_update_failure_stops_before_install() {
    reset_preflight_stubs
    INSTALL_MODE="system"
    INSTALL_RESULT="success"
    APT_UPDATE_RESULT="error"

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "failed APT refresh unexpectedly continued"
    [[ $AS_ROOT_CALLS -eq 1 ]] || fail "package install ran after failed APT refresh"
    [[ "$AS_ROOT_HISTORY" == $'apt-get update\n' ]] || \
        fail "unexpected commands after failed APT refresh: $AS_ROOT_HISTORY"
    assert_contains "$TEST_OUTPUT" \
        'Failed to refresh APT package indexes; no runtime packages were installed.'
}

test_unknown_package_manager_reports_aggregate() {
    reset_preflight_stubs
    PKG_BASE=""
    PKG_INSTALL=""
    detect_runtime_package_manager() { return 1; }

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "unknown package manager unexpectedly succeeded"
    [[ $AS_ROOT_CALLS -eq 0 ]] || fail "unknown package manager invoked as_root"
    assert_contains "$TEST_OUTPUT" 'sec-core: bubblewrap'
    assert_contains "$TEST_OUTPUT" 'ws-ckpt: rsync'
    assert_contains "$TEST_OUTPUT" 'Cannot determine a supported deb/rpm package manager'
    assert_not_contains "$TEST_OUTPUT" 'Install them with:'
}

test_rpm_report_uses_manifest_package_names() {
    reset_preflight_stubs
    PKG_BASE="rpm"
    PKG_INSTALL="dnf install -y"
    COMPONENTS=(sec-core sight)
    TEST_DEPENDENCIES=(
        'sec-core|gnupg|system-package|gpg --version|gnupg2|gnupg||||'
        'sight|elfutils-libelf|system-package|grep -aqF libelf.so.1 /etc/ld.so.cache|elfutils-libelf|libelf1||||'
    )

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "rpm user preflight unexpectedly succeeded"
    [[ $AS_ROOT_CALLS -eq 0 ]] || fail "rpm user preflight invoked as_root"
    assert_contains "$TEST_OUTPUT" 'sudo dnf install -y gnupg2 elfutils-libelf'
}

test_ignore_deps_never_installs_packages() {
    reset_preflight_stubs
    INSTALL_MODE="system"
    INSTALL_DEPS=false

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "--ignore-deps preflight unexpectedly succeeded"
    [[ $AS_ROOT_CALLS -eq 0 ]] || fail "--ignore-deps invoked as_root"
    assert_contains "$TEST_OUTPUT" '--ignore-deps'
}

test_system_node_rejects_user_nvm_fallback() {
    reset_preflight_stubs
    local user_bin="$TEST_TMP/user-bin"
    mkdir -p "$user_bin"
    printf '#!/bin/bash\necho v24.15.0\n' > "$user_bin/node"
    chmod +x "$user_bin/node"
    printf '#!/bin/bash\necho v18.19.0\n' > "$RUNTIME_SYSTEM_PATH/node"
    chmod +x "$RUNTIME_SYSTEM_PATH/node"
    PATH="$user_bin:$PATH"

    INSTALL_MODE="system"
    if runtime_probe_succeeds node 'node --version' '>=20'; then
        fail "system preflight accepted the installing user's nvm Node"
    fi

    INSTALL_MODE="user"
    runtime_probe_succeeds node 'node --version' '>=20' || \
        fail "user preflight did not preserve the user-local Node priority"
}

test_system_language_runtime_never_auto_installs() {
    reset_preflight_stubs
    INSTALL_MODE="system"
    INSTALL_RESULT="success"
    TEST_DEPENDENCIES=(
        'cosh|node|language-runtime|node --version|nodejs|nodejs||>=20|'
        'sec-core|bubblewrap|system-package|bwrap --version|bubblewrap|bubblewrap||||'
    )
    REPO_QUERY_CALLS=0
    query_repo_ver() {
        REPO_QUERY_CALLS=$((REPO_QUERY_CALLS + 1))
        echo 24.19.0
    }

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "language runtime was auto-installed"
    [[ $AS_ROOT_CALLS -eq 0 ]] || fail "packages changed before manual runtime was resolved"
    [[ $REPO_QUERY_CALLS -eq 0 ]] || fail "preflight tried to select a Node repository version"
    assert_contains "$TEST_OUTPUT" 'sudo apt-get install -y bubblewrap'
    assert_not_contains "$TEST_OUTPUT" 'apt-get install -y nodejs'
}

test_system_package_probe_ignores_user_path() {
    reset_preflight_stubs
    local user_bin="$TEST_TMP/user-bin"
    mkdir -p "$user_bin"
    printf '#!/bin/bash\necho jq-1.7\n' > "$user_bin/jq"
    chmod +x "$user_bin/jq"
    PATH="$user_bin:$PATH"

    INSTALL_MODE="system"
    if runtime_probe_succeeds jq 'jq --version' ''; then
        fail "system package probe accepted jq from the user's PATH"
    fi
}

test_language_runtime_version_is_enforced() {
    reset_preflight_stubs
    local node="$RUNTIME_SYSTEM_PATH/node"
    local user_bin="$TEST_TMP/user-bin"
    mkdir -p "$user_bin"
    printf '#!/bin/bash\necho v18.20.0\n' > "$user_bin/node"
    chmod +x "$user_bin/node"
    PATH="$user_bin:$PATH"
    printf '#!/bin/bash\necho v18.20.0\n' > "$node"
    chmod +x "$node"
    INSTALL_MODE="system"
    if runtime_probe_succeeds node 'node --version' '>=20'; then
        fail "Node 18 unexpectedly satisfied >=20"
    fi
    printf '#!/bin/bash\necho v20.1.0\n' > "$node"
    runtime_probe_succeeds node 'node --version' '>=20' || \
        fail "Node 20 did not satisfy >=20"
}

test_btrfs_module_probe_uses_system_path() {
    reset_preflight_stubs
    RUNTIME_PROC_FILESYSTEMS="$TEST_TMP/filesystems"
    : > "$RUNTIME_PROC_FILESYSTEMS"
    local user_bin="$TEST_TMP/user-bin"
    rm -rf "$user_bin"
    mkdir -p "$user_bin"
    PATH="$user_bin:/usr/bin:/bin"
    local modprobe="$RUNTIME_SYSTEM_PATH/modprobe"
    printf '#!/bin/bash\nexit 0\n' > "$modprobe"
    chmod +x "$modprobe"

    runtime_btrfs_available || fail "loadable btrfs module was rejected"
    printf '#!/bin/bash\nexit 1\n' > "$modprobe"
    if runtime_btrfs_available; then
        fail "unavailable btrfs capability unexpectedly succeeded"
    fi
}

test_btrfs_progs_probe_includes_system_sbin() {
    reset_preflight_stubs
    local user_bin="$TEST_TMP/user-bin"
    rm -rf "$user_bin"
    mkdir -p "$user_bin"
    printf '#!/bin/bash\necho btrfs-progs v6.6\n' > "$user_bin/btrfs"
    chmod +x "$user_bin/btrfs"
    PATH="$user_bin:/usr/bin:/bin"

    if runtime_probe_succeeds btrfs-progs 'mkfs.btrfs --version' ''; then
        fail "btrfs-progs probe passed without mkfs.btrfs"
    fi

    printf '#!/bin/bash\necho mkfs.btrfs, part of btrfs-progs v6.6\n' > \
        "$RUNTIME_SYSTEM_PATH/mkfs.btrfs"
    chmod +x "$RUNTIME_SYSTEM_PATH/mkfs.btrfs"
    runtime_probe_succeeds btrfs-progs 'mkfs.btrfs --version' '' || \
        fail "system mkfs.btrfs was hidden by the restricted user PATH"
}

test_manifest_load_failure_is_not_silently_ignored() {
    reset_preflight_stubs
    selected_runtime_dependencies() { return 1; }

    run_preflight

    [[ $TEST_STATUS -ne 0 ]] || fail "manifest load failure unexpectedly succeeded"
    assert_contains "$TEST_OUTPUT" 'Failed to load runtime dependency manifests'
}

test_manifest_parser_covers_maintained_component_dependencies() {
    reset_preflight_stubs
    local output="$TEST_TMP/manifests" component manifest
    : > "$output"
    for component in cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core; do
        manifest="$(runtime_manifest_path "$component")"
        runtime_dependencies_for_manifest "$component" "$manifest" >> "$output"
    done
    assert_contains "$output" 'agent-sec-core|bubblewrap|system-package|bwrap --version|bubblewrap|bubblewrap|||'
    assert_contains "$output" 'tokenless|python3|system-package|python3 --version|python3|python3|||'
    assert_contains "$output" 'skillfs|fuse3|system-package||fuse3|fuse3|||'
    assert_contains "$output" 'ws-ckpt|btrfs|platform-capability||||btrfs||5.4'
    assert_contains "$output" 'agentsight|ebpf-btf|platform-capability||||btf||5.8'
    assert_not_contains "$output" 'libssl1.1'

    # Building TypeScript is separate from installing inactive adapter files.
    [[ -z "$(source_build_runtime_dependencies)" ]] || fail 'build-time tools leaked into runtime requirements'
    [[ -z "$(runtime_dependency_for_source_build 'agent-sec-core|nodejs|system-package|node --version|nodejs|nodejs||||')" ]] || fail 'adapter Node requirement was retained'
    [[ "$(runtime_dependency_for_source_build 'agent-sec-core|systemd|system-package|systemctl --version|systemd|systemd||||')" == 'agent-sec-core|systemd|system-package|systemctl --version|systemd|systemd||||' ]] || fail 'daemon systemd requirement was dropped'
    for component in aw aw-provider-sec-core aw-provider-tokenless; do
        [[ -z "$(runtime_manifest_path "$component")" ]] || fail "AW requirements duplicated a backend: $component"
    done
}

test_selected_dependencies_keep_system_only_components() {
    reset_preflight_stubs
    local output="$TEST_TMP/selected-dependencies"
    bash -c '
        source "$1"
        source "$2"
        INSTALL_MODE=user
        COMPONENTS=(ws-ckpt skillfs agentsight agent-sec-core aw aw-provider-sec-core aw-provider-tokenless)
        selected_runtime_dependencies
    ' bash "$PROJECT_ROOT/scripts/build-all.sh" "$PROJECT_ROOT/scripts/lib/build-runtime.sh" > "$output"
    assert_contains "$output" 'ws-ckpt|btrfs|platform-capability'
    assert_contains "$output" 'skillfs|fuse3|system-package'
    assert_contains "$output" 'agentsight|ebpf-btf|platform-capability'
    assert_contains "$output" 'agent-sec-core|systemd|system-package'
    assert_not_contains "$output" '|node|language-runtime'
    assert_not_contains "$output" 'aw-provider-'
}

test_dry_run_does_not_probe_or_install_runtime_dependencies() {
    reset_preflight_stubs
    DRY_RUN=true
    INSTALL_MODE=system
    selected_runtime_dependencies() { echo 'UNEXPECTED_MANIFEST_READ'; return 1; }
    run_preflight
    [[ "$TEST_STATUS" -eq 0 ]] || fail 'dry-run runtime preflight failed'
    [[ "$AS_ROOT_CALLS" -eq 0 ]] || fail 'dry-run mutated system packages'
    assert_contains "$TEST_OUTPUT" 'DRY-RUN'
    assert_not_contains "$TEST_OUTPUT" 'UNEXPECTED_MANIFEST_READ'
}

assert_retry_roundtrip() {
    local retry="$1" expected_mode="$INSTALL_MODE" expected_prefix="$PREFIX"
    local expected_supported="$SUPPORTED_ONLY" expected_system_deps="$INSTALL_SYSTEM_DEPS"
    local expected_install_deps="$INSTALL_DEPS" expected_deps_only="$DEPS_ONLY"
    local -a expected_components=("${COMPONENTS[@]}")
    (
        source "$PROJECT_ROOT/scripts/build-all.sh"
        eval "set -- $retry"
        [[ "$1" == "$PROJECT_ROOT/scripts/build-all.sh" ]] || fail 'retry uses a different entrypoint'
        shift
        parse_args "$@"
        select_components
        validate_options
        [[ "$INSTALL_MODE" == "$expected_mode" ]] || fail 'retry changed installation mode'
        [[ "$PREFIX" == "$expected_prefix" ]] || fail 'retry changed installation prefix'
        [[ "$SUPPORTED_ONLY" == "$expected_supported" ]] || fail 'retry changed user filtering'
        [[ "$INSTALL_SYSTEM_DEPS" == "$expected_system_deps" ]] || fail 'retry changed system-package permission'
        [[ "$INSTALL_DEPS" == "$expected_install_deps" ]] || fail 'retry changed dependency preparation'
        [[ "$DEPS_ONLY" == "$expected_deps_only" ]] || fail 'retry changed dependency-only mode'
        [[ "${COMPONENTS[*]}" == "${expected_components[*]}" ]] || fail 'retry changed selected components'
    )
}

test_retry_command_preserves_component_and_scope() {
    reset_preflight_stubs
    COMPONENTS=(agent-memory)
    INSTALL_MODE=system
    PREFIX="$TEST_TMP/system prefix's files"
    INSTALL_DEPS=false
    local retry
    retry="$(cd "$PROJECT_ROOT/src/agent-memory" && runtime_retry_command)"
    assert_contains <(printf '%s\n' "$retry") "$PROJECT_ROOT/scripts/build-all.sh"
    assert_contains <(printf '%s\n' "$retry") '--component agent-memory'
    assert_contains <(printf '%s\n' "$retry") '--system'
    assert_contains <(printf '%s\n' "$retry") '--ignore-deps'
    assert_retry_roundtrip "$retry"
}

test_user_retry_roundtrip_preserves_installation_scope() {
    reset_preflight_stubs
    COMPONENTS=("${DEFAULT_COMPONENTS[@]}")
    PREFIX="$TEST_TMP/user prefix's files"
    SUPPORTED_ONLY=true
    INSTALL_SYSTEM_DEPS=true
    select_components
    validate_options
    TEST_DEPENDENCIES=('tokenless|python3|language-runtime|python3 --version|||python-version|>=3.9||')

    run_preflight

    [[ "$TEST_STATUS" -ne 0 ]] || fail 'user preflight unexpectedly succeeded'
    [[ "$AS_ROOT_CALLS" -eq 0 ]] || fail 'user preflight invoked as_root'
    local retry
    retry="$(runtime_retry_command)"
    assert_contains "$TEST_OUTPUT" "$retry"
    assert_retry_roundtrip "$retry"
}

run_test() {
    local test="$1"
    ( "$test" )
    printf 'ok - %s\n' "$test"
}

for test in $(declare -F | awk '{ print $3 }' | sort); do
    [[ "$test" == test_* ]] || continue
    run_test "$test"
done
