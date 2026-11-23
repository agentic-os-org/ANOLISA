#!/usr/bin/env bash
# Check dependency planning without package installation or network access.
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$TEST_DIR/.." && pwd)"
TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT
# shellcheck source=../scripts/lib/build-env.sh
source "$PROJECT_ROOT/scripts/lib/build-env.sh"

fail() { printf 'not ok - %s\n' "$*" >&2; exit 1; }
die() { fail "$*"; }
info() { :; }
warn() { printf '%s\n' "$*"; }
run_cmd() { fail "unexpected installation command: $*"; }
as_root() { fail "unexpected privileged command: $*"; }
curl() { fail "unexpected network access: $*"; }
wget() { fail "unexpected network access: $*"; }
rustup() { fail "unexpected rustup execution: $*"; }
rustc() { fail "unexpected rustc execution: $*"; }
cargo() { fail "unexpected Cargo execution: $*"; }
node() { fail "unexpected Node.js execution: $*"; }
npm() { fail "unexpected npm execution: $*"; }

# Select a package family independently of the test host's installed managers.
command() {
    if [[ "${1:-}" == -v ]]; then
        case "${2:-}" in
            apt-get|dnf|yum)
                [[ "$2" == "${TEST_PACKAGE_MANAGER:-apt-get}" ]]
                return
                ;;
        esac
    fi
    builtin command "$@"
}

uname() {
    case "${1:-}" in
        -s) printf 'Linux\n' ;;
        -m) printf 'x86_64\n' ;;
        *) builtin command uname "$@" ;;
    esac
}

has_package() { [[ " ${BUILD_ENV_PACKAGES[*]} " == *" $1 "* ]]; }
has_toolchain() { [[ " ${BUILD_ENV_TOOLCHAINS[*]} " == *" $1 "* ]]; }

test_all_components_plan_build_dependencies_without_runtime_gates() {
    COMPONENTS=(cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw aw-provider-sec-core aw-provider-tokenless)
    resolve_build_environment
    $BUILD_ENV_NEEDS_RUST || fail 'Rust missing from all-component requirements'
    $BUILD_ENV_NEEDS_NODE || fail 'Node missing from AgentSight dashboard requirements'
    $BUILD_ENV_NEEDS_JUST || fail 'just missing from Tokenless requirements'
    has_package python3 || fail 'system Python helper was omitted'
    has_package libfuse3-dev || fail 'SkillFS FUSE development dependency was omitted'
    has_package libbpf-dev || fail 'AgentSight libbpf dependency was omitted'
    has_package libclang-dev || fail 'AgentSight libclang dependency was omitted'
    has_package libc6-dev || fail 'static sandbox libc dependency was omitted'
    has_toolchain stable || fail 'cosh-ng toolchain declaration was ignored'
    has_toolchain 1.93.0 || fail 'sandbox toolchain declaration was ignored'
    has_toolchain 1.97.1 || fail 'AW toolchain declaration was ignored'
    [[ "$(printf '%s\n' "${BUILD_ENV_TOOLCHAINS[@]}" | grep -c '^1.97.1$')" -eq 1 ]] || fail 'shared AW toolchain was prepared repeatedly'
    local package
    for package in uv python3.11 python3.11-dev python3-venv btrfs-progs bubblewrap fuse3 systemd; do
        if has_package "$package"; then fail "runtime or legacy dependency leaked into builds: $package"; fi
    done
    [[ "$(printf '%s\n' "${BUILD_ENV_PACKAGES[@]}" | sort | uniq -d | wc -l)" -eq 0 ]] || fail 'package requirements were not deduplicated'
}

test_tokenless_and_checkpoint_each_prepare_node() {
    local component
    for component in tokenless ws-ckpt; do
        COMPONENTS=("$component")
        resolve_build_environment
        $BUILD_ENV_NEEDS_NODE || fail "$component does not prepare Node/npm"
        if has_package libbpf-dev; then fail 'partial build prepared AgentSight dependencies'; fi
    done
}

test_aw_build_only_requires_node_for_the_agentsight_dashboard() {
    COMPONENTS=(aw tokenless ws-ckpt agent-sec-core agent-memory)
    resolve_build_environment
    if $BUILD_ENV_NEEDS_NODE; then fail 'AW build planned Node for skipped plugins'; fi
    $BUILD_ENV_NEEDS_JUST || fail 'AW build dropped the Tokenless RTK prerequisite'
    has_package libc6-dev || fail 'AW build dropped the static sandbox prerequisite'
    has_package cmake || fail 'AW build dropped the memory core prerequisite'

    COMPONENTS+=(cosh-ng)
    resolve_build_environment
    if $BUILD_ENV_NEEDS_NODE; then fail 'Native cosh-ng extensions should not require Node'; fi

    COMPONENTS+=(agentsight)
    resolve_build_environment
    $BUILD_ENV_NEEDS_NODE || fail 'AW build dropped the AgentSight dashboard prerequisite'
}

test_provider_only_uses_its_pinned_toolchain() {
    COMPONENTS=(aw-provider-tokenless)
    resolve_build_environment
    has_toolchain 1.97.1 || fail 'provider pin was not selected'
    $BUILD_ENV_NEEDS_RUST || fail 'provider Rust dependency omitted'
    if $BUILD_ENV_NEEDS_NODE || $BUILD_ENV_NEEDS_JUST; then fail 'provider build pulled in backend tools'; fi
    if has_package patch || has_package libbpf-dev; then fail 'provider build pulled in unrelated backend packages'; fi
}

test_future_provider_uses_its_manifest_directory_and_pin() {
    local PROJECT_ROOT="$TEST_TMP/future-provider-repository with spaces"
    local directory="$PROJECT_ROOT/providers/skillfs"
    mkdir -p "$directory"
    : > "$directory/Makefile"
    printf '[package]\nname = "aw-provider-skillfs"\nversion = "0.1.0"\nrust-version = "1.95"\n' > "$directory/Cargo.toml"
    printf '[toolchain]\nchannel = "1.96.0"\ncomponents = ["rustfmt"]\n' > "$directory/rust-toolchain.toml"
    COMPONENTS=(aw-provider-skillfs)
    resolve_build_environment
    has_toolchain 1.96.0 || fail 'future provider did not use its own directory pin'
    [[ "$BUILD_ENV_MIN_RUST" == 1.95 ]] || fail 'future provider manifest was not read'
    [[ "${BUILD_ENV_RUST_COMPONENTS[*]}" == '1.96.0:rustfmt' ]] || fail 'future provider toolchain components were ignored'
    if has_toolchain stable; then fail 'future provider used a nonexistent backend manifest'; fi
    if $BUILD_ENV_NEEDS_NODE || $BUILD_ENV_NEEDS_JUST || has_package libfuse3-dev; then
        fail 'future provider-only build pulled in its backend tools'
    fi
}

test_minimum_rust_versions_are_not_installed_as_pins() {
    COMPONENTS=(skillfs tokenless)
    resolve_build_environment
    build_env_version_at_least "$BUILD_ENV_MIN_RUST" 1.91 || fail 'maximum selected MSRV was not computed'
    if has_toolchain 1.86 || has_toolchain 1.86.0 || has_toolchain 1.91 || has_toolchain 1.91.0; then
        fail 'MSRV declarations were treated as fixed toolchains'
    fi
}

test_agentsight_only_declares_its_skeleton_formatter_and_edition_floor() {
    COMPONENTS=(agentsight)
    resolve_build_environment
    [[ "${BUILD_ENV_TOOLCHAINS[*]}" == stable ]] || fail 'AgentSight selected an unrelated compiler'
    [[ "${BUILD_ENV_RUST_COMPONENTS[*]}" == stable:rustfmt ]] || fail 'AgentSight omitted its skeleton formatter'
    [[ "$(build_env_rust_minimum stable)" == 1.85 ]] || fail 'Rust 2024 syntax floor was not inferred'
    if has_toolchain 1.85 || has_toolchain 1.85.0; then fail 'edition floor was installed as a fixed toolchain'; fi
}

test_agentsight_skeleton_formatter_follows_a_directory_pin() {
    local PROJECT_ROOT="$TEST_TMP/pinned-agentsight-repository"
    local directory="$PROJECT_ROOT/src/agentsight"
    mkdir -p "$directory"
    printf '[package]\nname = "agentsight"\nversion = "0.1.0"\nedition = "2024"\n' > "$directory/Cargo.toml"
    printf '[toolchain]\nchannel = "1.96.0"\ncomponents = ["clippy", "rustfmt"]\n' > "$directory/rust-toolchain.toml"
    COMPONENTS=(agentsight)
    resolve_build_environment
    [[ "${BUILD_ENV_TOOLCHAINS[*]}" == 1.96.0 ]] || fail 'AgentSight directory pin was ignored'
    [[ "${BUILD_ENV_RUST_COMPONENTS[*]}" == '1.96.0:clippy 1.96.0:rustfmt' ]] || fail 'skeleton formatter was duplicated or attached to another toolchain'
    [[ "${BUILD_ENV_RUST_MINIMUMS[*]}" == 1.96.0:1.85 ]] || fail 'AgentSight edition floor ignored its directory pin'
    [[ -z "$BUILD_FALLBACK_TOOLCHAIN" ]] || fail 'pinned AgentSight selected an unrelated fallback'
}

test_edition_floor_is_scoped_to_the_manifest_toolchain() {
    local PROJECT_ROOT="$TEST_TMP/edition-provider-repository"
    local directory="$PROJECT_ROOT/providers/skillfs"
    mkdir -p "$directory"
    : > "$directory/Makefile"
    printf '[package]\nname = "aw-provider-skillfs"\nversion = "0.1.0"\nedition = "2024"\n' > "$directory/Cargo.toml"
    printf '[toolchain]\nchannel = "1.96.0"\n' > "$directory/rust-toolchain.toml"
    COMPONENTS=(aw-provider-skillfs)
    resolve_build_environment
    [[ "${BUILD_ENV_TOOLCHAINS[*]}" == 1.96.0 ]] || fail 'edition floor changed the declared toolchain'
    [[ "${BUILD_ENV_RUST_MINIMUMS[*]}" == 1.96.0:1.85 ]] || fail 'edition floor was attached to the wrong toolchain'
    printf 'rust-version = "1.95"\n' >> "$directory/Cargo.toml"
    resolve_build_environment
    [[ "${BUILD_ENV_RUST_MINIMUMS[*]}" == 1.96.0:1.95 ]] || fail 'edition floor weakened an explicit MSRV'
}

agentsight_minimal_toolchain_fixture() {
    HOME="$TEST_TMP/$1-home"
    XDG_CACHE_HOME="$TEST_TMP/$1-cache"
    DRY_RUN=false
    COMPONENTS=(agentsight)
    TOOLCHAIN_CALLS="$TEST_TMP/$1-commands"
    : > "$TOOLCHAIN_CALLS"
    TEST_STABLE_VERSION=1.98.0
    TEST_RUSTFMT_AVAILABLE=false
    build_env_package_present() { return 0; }
    build_env_node_present() { return 0; }
    cargo() { :; }
    rustup() {
        case "$*" in
            'toolchain list') printf 'stable-x86_64-unknown-linux-gnu\n' ;;
            'run stable rustc --version') printf 'rustc %s\n' "$TEST_STABLE_VERSION" ;;
            'component list --installed --toolchain stable')
                printf 'rustc-x86_64-unknown-linux-gnu\n'
                if $TEST_RUSTFMT_AVAILABLE; then printf 'rustfmt-x86_64-unknown-linux-gnu\n'; fi
                ;;
            'toolchain install stable --profile minimal') TEST_STABLE_VERSION=1.98.0 ;;
            'component add rustfmt --toolchain stable') TEST_RUSTFMT_AVAILABLE=true ;;
            *) fail "unexpected AgentSight toolchain operation: $*" ;;
        esac
    }
    run_cmd() {
        printf '%s\n' "$*" >> "$TOOLCHAIN_CALLS"
        "$@"
    }
}

test_agentsight_only_missing_skeleton_formatter_is_checked_then_prepared() {
    agentsight_minimal_toolchain_fixture agentsight-rustfmt
    local status=0 output="$TEST_TMP/agentsight-rustfmt-output"
    check_build_environment > "$output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'AgentSight accepted a minimal toolchain without rustfmt'
    grep -Fq 'Missing Rust component: rustfmt (stable)' "$output" || fail 'missing skeleton formatter was not reported'
    [[ ! -s "$TOOLCHAIN_CALLS" ]] || fail 'dependency check installed the missing formatter'
    prepare_build_environment
    [[ "$(cat "$TOOLCHAIN_CALLS")" == 'rustup component add rustfmt --toolchain stable' ]] || fail 'AgentSight did not prepare only its missing formatter'
    [[ ! -e "$HOME" && ! -e "$XDG_CACHE_HOME" ]] || fail 'fixture performed a real installation'
}

test_agentsight_only_old_stable_is_checked_then_updated() {
    agentsight_minimal_toolchain_fixture agentsight-old-stable
    TEST_STABLE_VERSION=1.84.0
    TEST_RUSTFMT_AVAILABLE=true
    local status=0 output="$TEST_TMP/agentsight-old-stable-output"
    check_build_environment > "$output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'AgentSight accepted a compiler without Rust 2024 support'
    grep -Fq 'Rust toolchain stable must provide rustc >=1.85' "$output" || fail 'edition compiler floor was not reported'
    [[ ! -s "$TOOLCHAIN_CALLS" ]] || fail 'dependency check updated the old compiler'
    prepare_build_environment
    [[ "$(cat "$TOOLCHAIN_CALLS")" == 'rustup toolchain install stable --profile minimal' ]] || fail 'AgentSight did not update its existing stable channel'
    [[ ! -e "$HOME" && ! -e "$XDG_CACHE_HOME" ]] || fail 'fixture performed a real installation'
}

test_rpm_package_mapping_has_no_debian_packages() {
    TEST_PACKAGE_MANAGER=dnf
    COMPONENTS=(skillfs agentsight agent-sec-core)
    resolve_build_environment
    [[ "$BUILD_ENV_PACKAGE_KIND" == rpm ]] || fail 'RPM package family was not selected'
    has_package fuse3-devel || fail 'RPM FUSE development package omitted'
    has_package glibc-static || fail 'RPM sandbox static libc package omitted'
    has_package elfutils-libelf-devel || fail 'RPM ELF development package omitted'
    if has_package xz-utils || has_package libfuse3-dev || has_package build-essential; then
        fail 'Debian packages leaked into RPM requirements'
    fi
}

test_node_version_and_npm_are_both_required() {
    node() { printf 'v20.20.2\n'; }
    npm() { printf '10.0.0\n'; }
    if build_env_node_present; then fail 'Node 20 accepted for current plugins'; fi
    node() { printf 'v22.23.2\n'; }
    build_env_node_present || fail 'Node 22 and npm were rejected'
    unset -f npm
    command() {
        if [[ "${1:-}" == -v && "${2:-}" == npm ]]; then return 1; fi
        builtin command "$@"
    }
    if build_env_node_present; then fail 'Node without npm was accepted'; fi
}

download_fixture() {
    DOWNLOAD_ROOT="$TEST_TMP/$1-downloads"
    DOWNLOAD_LOG="$DOWNLOAD_ROOT/requests"
    DRY_RUN=false
    XDG_CACHE_HOME="$TEST_TMP/$1-cache"
    mkdir -p "$DOWNLOAD_ROOT"
    : > "$DOWNLOAD_LOG"
    curl() {
        local url="" output="" protocol="" redirect_protocol=""
        while (( $# )); do
            case "$1" in
                --proto) protocol="$2"; shift 2 ;;
                --proto-redir) redirect_protocol="$2"; shift 2 ;;
                --retry) shift 2 ;;
                -fsSL) shift ;;
                -o) output="$2"; shift 2 ;;
                https://*) url="$1"; shift ;;
                *) fail "unexpected download argument: $1" ;;
            esac
        done
        [[ "$protocol" == '=https' && "$redirect_protocol" == '=https' ]] || fail 'download allowed a non-HTTPS protocol'
        [[ -n "$url" && -n "$output" ]] || fail 'download omitted its URL or destination'
        printf '%s\n' "$url" >> "$DOWNLOAD_LOG"
        if [[ "${url##*/}" == SHASUMS256.txt ]]; then
            cp "$DOWNLOAD_ROOT/SHASUMS256.txt" "$output"
        elif [[ "${url##*/}" == "${NODE_FIXTURE_ARCHIVE:-}" ]]; then
            cp "$DOWNLOAD_ROOT/$NODE_FIXTURE_ARCHIVE" "$output"
        else
            cp "$DOWNLOAD_ROOT/rustup-init.sh" "$output"
        fi
    }
}

node_download_fixture() {
    download_fixture "$1"
    NODE_FIXTURE_ARCHIVE=node-v22.99.0-linux-x64.tar.xz
    local payload="$DOWNLOAD_ROOT/payload/node-v22.99.0-linux-x64"
    mkdir -p "$payload/bin"
    printf '#!/bin/sh\nprintf "v22.99.0\\n"\n' > "$payload/bin/node"
    printf '#!/bin/sh\nexit 0\n' > "$payload/bin/npm"
    chmod +x "$payload/bin/node" "$payload/bin/npm"
    tar -cJf "$DOWNLOAD_ROOT/$NODE_FIXTURE_ARCHIVE" -C "$DOWNLOAD_ROOT/payload" node-v22.99.0-linux-x64
    (cd "$DOWNLOAD_ROOT" && sha256sum "$NODE_FIXTURE_ARCHIVE" > SHASUMS256.txt)
}

test_node_download_defaults_to_official_source_and_verifies_archive() {
    unset NODE_DIST_MIRROR
    node_download_fixture node-default
    build_env_install_node
    cmp -s "$DOWNLOAD_LOG" <(printf '%s\n' \
        https://nodejs.org/dist/latest-v22.x/SHASUMS256.txt \
        "https://nodejs.org/dist/latest-v22.x/$NODE_FIXTURE_ARCHIVE") || fail 'default Node download source changed'
    cmp -s "$DOWNLOAD_ROOT/payload/node-v22.99.0-linux-x64/bin/node" \
        "$XDG_CACHE_HOME/anolisa/build-tools/node/bin/node" || fail 'verified Node archive was not installed'
}

test_node_mirror_downloads_checksum_and_archive_from_its_directory() {
    local suffix
    for suffix in '' /; do
        node_download_fixture "node-mirror${suffix:+-slash}"
        NODE_DIST_MIRROR="https://mirror.example.test/custom/node$suffix"
        build_env_install_node
        cmp -s "$DOWNLOAD_LOG" <(printf '%s\n' \
            https://mirror.example.test/custom/node/latest-v22.x/SHASUMS256.txt \
            "https://mirror.example.test/custom/node/latest-v22.x/$NODE_FIXTURE_ARCHIVE") || fail 'Node mirror was not used for both downloads'
        [[ -x "$XDG_CACHE_HOME/anolisa/build-tools/node/bin/node" ]] || fail 'mirror archive was not installed'
    done
}

test_node_mirror_rejects_mismatched_checksum_before_installation() {
    node_download_fixture node-checksum
    NODE_DIST_MIRROR=https://mirror.example.test/node
    printf '%064d  %s\n' 0 "$NODE_FIXTURE_ARCHIVE" > "$DOWNLOAD_ROOT/SHASUMS256.txt"
    local status=0 output="$DOWNLOAD_ROOT/output"
    build_env_install_node > "$output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'Node accepted an archive with a mismatched checksum'
    grep -Fq 'checksum verification failed' "$output" || fail 'Node did not report the failed integrity check'
    [[ ! -e "$XDG_CACHE_HOME" ]] || fail 'Node installed files before validating the archive'
}

rustup_download_fixture() {
    download_fixture "$1"
    export BOOTSTRAP_ARGUMENTS="$DOWNLOAD_ROOT/arguments"
    export BOOTSTRAP_ENVIRONMENT="$DOWNLOAD_ROOT/environment"
    cat > "$DOWNLOAD_ROOT/rustup-init.sh" <<'SH'
#!/bin/sh
set -eu
printf '%s\n' "$@" > "$BOOTSTRAP_ARGUMENTS"
printf '%s\n' "${RUSTUP_DIST_SERVER-unset}" "${RUSTUP_UPDATE_ROOT-unset}" > "$BOOTSTRAP_ENVIRONMENT"
SH
}

test_rustup_bootstrap_defaults_to_official_source() {
    unset RUSTUP_INIT_SCRIPT_URL
    rustup_download_fixture rustup-default
    build_env_install_rustup
    [[ "$(cat "$DOWNLOAD_LOG")" == https://sh.rustup.rs ]] || fail 'default rustup bootstrap source changed'
    cmp -s "$BOOTSTRAP_ARGUMENTS" <(printf '%s\n' -y --profile minimal --default-toolchain stable --no-modify-path) || fail 'rustup bootstrap arguments changed'
}

test_rustup_bootstrap_override_preserves_native_mirror_environment() {
    rustup_download_fixture rustup-mirror
    RUSTUP_INIT_SCRIPT_URL=https://mirror.example.test/custom/rustup-init.sh
    export RUSTUP_DIST_SERVER=https://mirror.example.test/rust
    export RUSTUP_UPDATE_ROOT=https://mirror.example.test/rustup
    build_env_install_rustup
    [[ "$(cat "$DOWNLOAD_LOG")" == "$RUSTUP_INIT_SCRIPT_URL" ]] || fail 'rustup bootstrap URL override was ignored'
    cmp -s "$BOOTSTRAP_ENVIRONMENT" <(printf '%s\n' "$RUSTUP_DIST_SERVER" "$RUSTUP_UPDATE_ROOT") || fail 'rustup native mirror configuration was changed'
}

test_tool_download_overrides_reject_invalid_or_insecure_urls() {
    download_fixture invalid-download-url
    local url status
    for url in '' node.example.test/dist http://mirror.example.test/dist file:///tmp/tool https:///dist 'https://mirror.example.test/with space' https://user:password@mirror.example.test/dist; do
        NODE_DIST_MIRROR="$url"
        RUSTUP_INIT_SCRIPT_URL="$url"
        status=0
        build_env_install_node > "$DOWNLOAD_ROOT/output" 2>&1 || status=$?
        [[ "$status" -ne 0 ]] || fail 'Node accepted an invalid download URL'
        status=0
        build_env_install_rustup > "$DOWNLOAD_ROOT/output" 2>&1 || status=$?
        [[ "$status" -ne 0 ]] || fail 'rustup accepted an invalid bootstrap URL'
    done
    for url in 'https://mirror.example.test/dist?version=22' 'https://mirror.example.test/dist#fragment'; do
        NODE_DIST_MIRROR="$url"
        status=0
        build_env_install_node > "$DOWNLOAD_ROOT/output" 2>&1 || status=$?
        [[ "$status" -ne 0 ]] || fail 'Node accepted an ambiguous mirror base URL'
    done
    [[ ! -s "$DOWNLOAD_LOG" && ! -e "$XDG_CACHE_HOME" ]] || fail 'invalid URL triggered a tool download or installation'
}

test_check_only_reports_missing_dependencies_without_installing() {
    HOME="$TEST_TMP/check-home"
    XDG_CACHE_HOME="$TEST_TMP/check-cache"
    mkdir -p "$HOME"
    printf 'keep\n' > "$HOME/.bashrc"
    COMPONENTS=(aw-provider-sec-core)
    INSTALL_MODE=user
    INSTALL_SYSTEM_DEPS=true
    build_env_package_present() { [[ "$1" != python3 ]]; }
    build_env_toolchain_present() { return 0; }
    build_env_rust_component_present() { return 0; }
    build_env_rust_version() { printf '1.98.0\n'; }
    cargo() { :; }
    local before after status=0
    before="$(find "$HOME" -type f -exec sha256sum '{}' +)"
    check_build_environment > "$TEST_TMP/check-output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'check accepted a missing build dependency'
    grep -Fq python3 "$TEST_TMP/check-output" || fail 'missing dependency was not reported'
    after="$(find "$HOME" -type f -exec sha256sum '{}' +)"
    [[ "$before" == "$after" && ! -e "$XDG_CACHE_HOME" ]] || fail 'check changed files or created a cache'
}

test_user_missing_native_packages_stop_before_any_installation_or_download() {
    HOME="$TEST_TMP/user-missing-home"
    XDG_CACHE_HOME="$TEST_TMP/user-missing-cache"
    INSTALL_MODE=user
    INSTALL_SYSTEM_DEPS=auto
    DRY_RUN=false
    COMPONENTS=(tokenless)
    local calls="$TEST_TMP/user-missing-commands" output="$TEST_TMP/user-missing-output" status=0
    : > "$calls"
    # The user profile must remain unprivileged even when invoked by root.
    id() { printf '0\n'; }
    build_env_package_present() { [[ "$1" != patch && "$1" != libssl-dev ]]; }
    build_env_toolchain_present() { return 1; }
    build_env_rust_component_present() { return 1; }
    build_env_node_present() { return 1; }
    build_env_install_rustup() { printf 'download-rustup\n' >> "$calls"; }
    build_env_install_node() { printf 'download-node\n' >> "$calls"; }
    build_env_check_resolved() { printf 'resolved-check\n' >> "$calls"; return 0; }
    run_cmd() { printf 'run|%s\n' "$*" >> "$calls"; }
    as_root() { printf 'root|%s\n' "$*" >> "$calls"; }
    curl() { printf 'curl|%s\n' "$*" >> "$calls"; }
    wget() { printf 'wget|%s\n' "$*" >> "$calls"; }
    command() {
        if [[ "${1:-}" == -v ]]; then
            case "${2:-}" in
                rustup|rustc|just|dnf|yum) return 1 ;;
                apt-get) return 0 ;;
            esac
        fi
        builtin command "$@"
    }
    ( prepare_build_environment ) > "$output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'user preparation accepted missing native packages without permission'
    grep -Fq patch "$output" || fail 'missing patch package was not reported'
    grep -Fq libssl-dev "$output" || fail 'missing OpenSSL development package was not reported'
    grep -Fq -- --install-system-deps "$output" || fail 'missing package error omitted the explicit permission option'
    [[ ! -s "$calls" ]] || fail "missing user packages triggered installation work: $(cat "$calls")"
    [[ ! -e "$HOME" && ! -e "$XDG_CACHE_HOME" ]] || fail 'rejected user preparation created installation directories'
}

assert_native_package_setup_continues() {
    local name="$1" calls="$TEST_TMP/$1-commands"
    HOME="$TEST_TMP/$name-home"
    XDG_CACHE_HOME="$TEST_TMP/$name-cache"
    DRY_RUN=false
    COMPONENTS=(tokenless)
    : > "$calls"
    build_env_package_present() { [[ "$1" != patch && "$1" != libssl-dev ]]; }
    build_env_toolchain_present() { return 1; }
    build_env_rust_component_present() { return 1; }
    build_env_node_present() { return 1; }
    build_env_install_rustup() { printf 'download-rustup\n' >> "$calls"; }
    build_env_install_node() { printf 'download-node\n' >> "$calls"; }
    build_env_check_resolved() { printf 'resolved-check\n' >> "$calls"; return 0; }
    run_cmd() { printf 'run|%s\n' "$*" >> "$calls"; }
    command() {
        if [[ "${1:-}" == -v ]]; then
            case "${2:-}" in
                rustup|rustc|dnf|yum) return 1 ;;
                apt-get|just) return 0 ;;
            esac
        fi
        builtin command "$@"
    }
    prepare_build_environment
    [[ "$(head -1 "$calls")" == 'run|as_root apt-get update' ]] || fail 'native package preparation was not first'
    grep -Fq 'run|as_root apt-get install -y patch libssl-dev' "$calls" || fail 'missing native packages were not installed together'
    grep -Fxq download-rustup "$calls" || fail 'permitted package preparation did not continue to Rust setup'
    grep -Fxq download-node "$calls" || fail 'permitted package preparation did not continue to Node setup'
    [[ "$(tail -1 "$calls")" == resolved-check ]] || fail 'permitted package preparation did not verify the final environment'
    local packages_line rust_line node_line
    packages_line="$(grep -n '^run|as_root apt-get install ' "$calls" | cut -d: -f1)"
    rust_line="$(grep -n '^download-rustup$' "$calls" | cut -d: -f1)"
    node_line="$(grep -n '^download-node$' "$calls" | cut -d: -f1)"
    (( packages_line < rust_line && packages_line < node_line )) || fail 'toolchain downloads preceded the native package transaction'
    [[ ! -e "$HOME" && ! -e "$XDG_CACHE_HOME" ]] || fail 'recorded setup executed a real installation'
}

test_user_explicit_permission_allows_native_packages_then_tool_setup() {
    INSTALL_MODE=user
    INSTALL_SYSTEM_DEPS=true
    assert_native_package_setup_continues user-permitted
}

test_system_preparation_installs_native_packages_by_default() {
    INSTALL_MODE=system
    INSTALL_SYSTEM_DEPS=auto
    assert_native_package_setup_continues system-default
}

test_preparation_reuses_satisfied_tools_without_install_commands() {
    HOME="$TEST_TMP/satisfied-home"
    XDG_CACHE_HOME="$TEST_TMP/satisfied-cache"
    DRY_RUN=false
    COMPONENTS=(cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw aw-provider-sec-core aw-provider-tokenless)
    build_env_package_present() { return 0; }
    node() { printf 'v22.23.2\n'; }
    npm() { printf '10.0.0\n'; }
    just() { :; }
    cargo() { :; }
    rustup() {
        case "$*" in
            default) printf '1.98.0-x86_64-unknown-linux-gnu (default)\n' ;;
            'toolchain list') printf '%s\n' stable-x86_64-unknown-linux-gnu 1.93.0-x86_64-unknown-linux-gnu 1.97.1-x86_64-unknown-linux-gnu 1.98.0-x86_64-unknown-linux-gnu ;;
            'run stable rustc --version') printf 'rustc 1.98.0\n' ;;
            'run 1.93.0 rustc --version') printf 'rustc 1.93.0\n' ;;
            'run 1.97.1 rustc --version') printf 'rustc 1.97.1\n' ;;
            'run 1.98.0-x86_64-unknown-linux-gnu rustc --version') printf 'rustc 1.98.0\n' ;;
            'component list --installed --toolchain '*) printf '%s\n' rust-src clippy-x86_64-unknown-linux-gnu rustfmt-x86_64-unknown-linux-gnu ;;
            *) fail "unexpected rustup command: $*" ;;
        esac
    }
    prepare_build_environment
    [[ ! -e "$HOME" && ! -e "$XDG_CACHE_HOME" ]] || fail 'satisfied preparation created directories'
}

test_dry_run_missing_tools_never_downloads_or_creates_directories() {
    HOME="$TEST_TMP/dry-home"
    XDG_CACHE_HOME="$TEST_TMP/dry-cache"
    DRY_RUN=true
    COMPONENTS=(tokenless)
    build_env_package_present() { return 1; }
    build_env_node_present() { return 1; }
    command() {
        if [[ "${1:-}" == -v ]]; then
            case "${2:-}" in
                rustup|rustc|just) return 1 ;;
                apt-get) return 0 ;;
                dnf|yum) return 1 ;;
            esac
        fi
        builtin command "$@"
    }
    rustc() { return 127; }
    rustup() { fail "dry-run called rustup: $*"; }
    curl() { fail 'dry-run downloaded a tool'; }
    mkdir() { fail 'dry-run created a directory'; }
    local calls="$TEST_TMP/dry-commands"
    run_cmd() { printf '%s\n' "$*" >> "$calls"; }
    prepare_build_environment
    [[ ! -e "$HOME" && ! -e "$XDG_CACHE_HOME" ]] || fail 'dry-run created an installation directory'
    grep -Fq 'apt-get install' "$calls" || fail 'dry-run omitted missing package plan'
    grep -Fq 'cargo install just' "$calls" || fail 'dry-run omitted Tokenless just requirement'
}

test_pin_only_build_ignores_an_old_user_default() {
    HOME="$TEST_TMP/pin-home"
    XDG_CACHE_HOME="$TEST_TMP/pin-cache"
    COMPONENTS=(aw-provider-sec-core)
    build_env_package_present() { return 0; }
    build_env_toolchain_present() { [[ "$1" == 1.97.1 ]]; }
    build_env_rust_component_present() { return 0; }
    build_env_rust_version() {
        if [[ "${1:-}" == 1.97.1 ]]; then printf '1.97.1\n'; else printf '1.50.0\n'; fi
    }
    cargo() { :; }
    check_build_environment || fail 'a satisfied pinned build depended on the user default'
    [[ -z "$BUILD_FALLBACK_TOOLCHAIN" ]] || fail 'pin-only build selected an unrelated fallback'
}

test_unpinned_build_reports_missing_stable_without_installing() {
    HOME="$TEST_TMP/no-stable-home"
    XDG_CACHE_HOME="$TEST_TMP/no-stable-cache"
    COMPONENTS=(tokenless)
    build_env_package_present() { return 0; }
    build_env_toolchain_present() { return 1; }
    build_env_node_present() { return 0; }
    just() { :; }
    cargo() { :; }
    local status=0
    check_build_environment > "$TEST_TMP/no-stable-output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'an unpinned build accepted a missing compiler'
    grep -Fq 'Missing Rust toolchain: stable' "$TEST_TMP/no-stable-output" || fail 'missing stable was not reported'
    [[ "$BUILD_FALLBACK_TOOLCHAIN" == stable ]] || fail 'unpinned build has no explicit compiler selection'
}

test_missing_sandbox_rust_src_is_checked_then_prepared() {
    HOME="$TEST_TMP/rust-src-home"
    XDG_CACHE_HOME="$TEST_TMP/rust-src-cache"
    DRY_RUN=false
    COMPONENTS=(agent-sec-core)
    build_env_package_present() { return 0; }
    build_env_toolchain_present() { return 0; }
    build_env_rust_version() { printf '1.98.0\n'; }
    build_env_node_present() { return 0; }
    cargo() { :; }
    SRC_AVAILABLE=false
    build_env_rust_component_present() {
        [[ "$2" != rust-src ]] || $SRC_AVAILABLE
    }
    local status=0 calls="$TEST_TMP/rust-src-commands"
    check_build_environment > "$TEST_TMP/rust-src-output" 2>&1 || status=$?
    [[ "$status" -ne 0 ]] || fail 'check accepted missing sandbox rust-src'
    grep -Fq 'rust-src (1.93.0)' "$TEST_TMP/rust-src-output" || fail 'missing sandbox component was not reported'
    run_cmd() {
        printf '%s\n' "$*" >> "$calls"
        [[ "$*" == 'rustup component add rust-src --toolchain 1.93.0' ]] || fail "unexpected setup command: $*"
        SRC_AVAILABLE=true
    }
    prepare_build_environment
    [[ "$(cat "$calls")" == 'rustup component add rust-src --toolchain 1.93.0' ]] || fail 'sandbox component was not prepared once'
}

make_wrapper_fixture() {
    WRAPPER_ROOT="$TEST_TMP/$1"
    WRAPPER_LOG="$WRAPPER_ROOT/arguments"
    mkdir -p "$WRAPPER_ROOT/bin" "$WRAPPER_ROOT/work/nested directory"
    cat > "$WRAPPER_ROOT/bin/rustup" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" > "$WRAPPER_LOG"
SH
    chmod +x "$WRAPPER_ROOT/bin/rustup"
}

run_wrapper() {
    local directory="$1"; shift
    (cd "$directory" && PATH="$WRAPPER_ROOT/bin:/usr/bin:/bin" \
        WRAPPER_LOG="$WRAPPER_LOG" ANOLISA_BUILD_TOOLCHAIN=stable \
        RUSTUP_TOOLCHAIN=external-override \
        "$PROJECT_ROOT/scripts/lib/build-toolchain/cargo" "$@")
}

test_cargo_wrapper_uses_fallback_and_preserves_spaced_arguments() {
    make_wrapper_fixture wrapper-fallback
    run_wrapper "$WRAPPER_ROOT/work" build --manifest-path 'directory with spaces/Cargo.toml' --features 'first second'
    cmp -s "$WRAPPER_LOG" <(printf '%s\n' run stable cargo build --manifest-path 'directory with spaces/Cargo.toml' --features 'first second') || fail 'fallback or Cargo arguments changed'
}

test_cargo_wrapper_honors_parent_toolchain_pin() {
    make_wrapper_fixture wrapper-parent
    printf '[toolchain]\nchannel = "1.93.0"\n' > "$WRAPPER_ROOT/work/rust-toolchain.toml"
    run_wrapper "$WRAPPER_ROOT/work/nested directory" rustc --bin linux-sandbox -- --print cfg
    cmp -s "$WRAPPER_LOG" <(printf '%s\n' run 1.93.0 cargo rustc --bin linux-sandbox -- --print cfg) || fail 'parent sandbox pin did not override the fallback and external environment'
}

test_cargo_wrapper_prefers_nearest_pin() {
    make_wrapper_fixture wrapper-nearest
    printf '[toolchain]\nchannel = "1.93.0"\n' > "$WRAPPER_ROOT/work/rust-toolchain.toml"
    printf '[toolchain]\nchannel = "1.97.1"\n' > "$WRAPPER_ROOT/work/nested directory/rust-toolchain.toml"
    run_wrapper "$WRAPPER_ROOT/work/nested directory" build --locked
    cmp -s "$WRAPPER_LOG" <(printf '%s\n' run 1.97.1 cargo build --locked) || fail 'nearest AW pin was not selected'
}

for test in $(declare -F | awk '{ print $3 }' | sort); do
    [[ "$test" == test_* ]] || continue
    ( "$test" )
    printf 'ok - %s\n' "$test"
done
