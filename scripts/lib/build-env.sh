#!/usr/bin/env bash
# Native build prerequisites shared by the unified build entrypoint.
# Component Makefiles retain ownership of Cargo/npm dependencies and source caches.

# shellcheck source=build-components.sh
source "$(dirname "${BASH_SOURCE[0]}")/build-components.sh"

build_env_add_package() {
    local package existing
    for package in "$@"; do
        for existing in "${BUILD_ENV_PACKAGES[@]}"; do
            [[ "$existing" != "$package" ]] || continue 2
        done
        BUILD_ENV_PACKAGES+=("$package")
    done
}

build_env_add_toolchain() {
    local channel="$1" existing
    [[ -n "$channel" ]] || return 0
    for existing in "${BUILD_ENV_TOOLCHAINS[@]}"; do
        [[ "$existing" != "$channel" ]] || return 0
    done
    BUILD_ENV_TOOLCHAINS+=("$channel")
}

build_env_version_at_least() {
    local actual="$1" required="$2"
    [[ -n "$actual" && -n "$required" ]] || return 1
    [[ "$(printf '%s\n%s\n' "$actual" "$required" | sort -V | head -n 1)" == "$required" ]]
}

build_env_channel() {
    local path="$1/rust-toolchain.toml"
    [[ -f "$path" ]] || return 0
    awk -F '"' '/^[[:space:]]*channel[[:space:]]*=/ { print $2; exit }' "$path"
}

build_env_declared_components() {
    local path="$1/rust-toolchain.toml"
    [[ -f "$path" ]] || return 0
    # Bootstrap cannot depend on a Python TOML parser before Python is installed.
    awk '
        /^[[:space:]]*\[/ && !reading {
            toolchain = ($0 ~ /^[[:space:]]*\[toolchain\][[:space:]]*(#.*)?$/)
        }
        toolchain && /^[[:space:]]*components[[:space:]]*=/ {
            reading = 1
            sub(/^[^=]*=/, "")
        }
        reading {
            sub(/#.*/, "")
            while (match($0, /"[^"]+"/)) {
                print substr($0, RSTART + 1, RLENGTH - 2)
                $0 = substr($0, RSTART + RLENGTH)
            }
            if (index($0, "]")) reading = 0
        }
    ' "$path"
}

build_env_add_rust_component() {
    local requirement="$1:$2" existing
    for existing in "${BUILD_ENV_RUST_COMPONENTS[@]}"; do
        [[ "$existing" != "$requirement" ]] || return 0
    done
    BUILD_ENV_RUST_COMPONENTS+=("$requirement")
}

build_env_add_rust_minimum() {
    local channel="$1" minimum="$2" index existing
    for index in "${!BUILD_ENV_RUST_MINIMUMS[@]}"; do
        existing="${BUILD_ENV_RUST_MINIMUMS[$index]}"
        [[ "${existing%%:*}" == "$channel" ]] || continue
        if ! build_env_version_at_least "${existing#*:}" "$minimum"; then
            BUILD_ENV_RUST_MINIMUMS[$index]="$channel:$minimum"
        fi
        return 0
    done
    BUILD_ENV_RUST_MINIMUMS+=("$channel:$minimum")
}

build_env_rust_minimum() {
    local requirement
    for requirement in "${BUILD_ENV_RUST_MINIMUMS[@]}"; do
        if [[ "${requirement%%:*}" == "$1" ]]; then
            printf '%s\n' "${requirement#*:}"
            return 0
        fi
    done
    printf '1.0.0\n'
}

build_env_require_rust() {
    local directory="$1" manifest="${2:-$1/Cargo.toml}" channel minimum component
    BUILD_ENV_NEEDS_RUST=true
    channel="$(build_env_channel "$directory")"
    if [[ -z "$channel" ]]; then
        # Unpinned workspaces use one explicit compiler, independent of an old
        # user default. More specific directory pins still win in the wrapper.
        channel=stable
        BUILD_FALLBACK_TOOLCHAIN=stable
    fi
    build_env_add_toolchain "$channel"
    while IFS= read -r component; do
        [[ -z "$component" ]] || build_env_add_rust_component "$channel" "$component"
    done < <(build_env_declared_components "$directory")
    if [[ -f "$manifest" ]]; then
        minimum="$(awk -F '"' '/^[[:space:]]*rust-version[[:space:]]*=[[:space:]]*"[0-9]/ { print $2; exit }' "$manifest")"
        # Edition syntax also imposes a compiler floor when no MSRV is declared.
        if awk '/^[[:space:]]*edition[[:space:]]*=[[:space:]]*"2024"/ { found = 1 } END { exit !found }' "$manifest" &&
            ! build_env_version_at_least "$minimum" 1.85; then
            minimum=1.85
        fi
        if [[ -n "$minimum" ]]; then
            build_env_add_rust_minimum "$channel" "$minimum"
            if ! build_env_version_at_least "$BUILD_ENV_MIN_RUST" "$minimum"; then
                BUILD_ENV_MIN_RUST="$minimum"
            fi
        fi
    fi
}

resolve_build_environment() {
    [[ "$(uname -s)" == Linux ]] || die "Unified native builds require Linux"
    if command -v apt-get >/dev/null 2>&1; then
        BUILD_ENV_PACKAGE_MANAGER=apt-get
        BUILD_ENV_PACKAGE_KIND=deb
    elif command -v dnf >/dev/null 2>&1; then
        BUILD_ENV_PACKAGE_MANAGER=dnf
        BUILD_ENV_PACKAGE_KIND=rpm
    elif command -v yum >/dev/null 2>&1; then
        BUILD_ENV_PACKAGE_MANAGER=yum
        BUILD_ENV_PACKAGE_KIND=rpm
    else
        die "Dependency preparation supports apt-get, dnf and yum; provision this host and use --ignore-deps"
    fi

    BUILD_ENV_PACKAGES=()
    BUILD_ENV_TOOLCHAINS=()
    BUILD_ENV_RUST_COMPONENTS=()
    BUILD_ENV_RUST_MINIMUMS=()
    BUILD_ENV_NEEDS_RUST=false
    BUILD_ENV_NEEDS_NODE=false
    BUILD_ENV_NEEDS_JUST=false
    BUILD_ENV_MIN_RUST=1.0.0
    BUILD_FALLBACK_TOOLCHAIN=""
    build_env_add_package make git curl ca-certificates tar gzip xz-utils python3
    if [[ "$BUILD_ENV_PACKAGE_KIND" == rpm ]]; then
        # RPM distributions package xz under its upstream name.
        BUILD_ENV_PACKAGES=(make git curl ca-certificates tar gzip xz python3)
    fi

    local component directory channel
    for component in "${COMPONENTS[@]}"; do
        directory="$(component_dir "$component")" || die "Missing provider source for $component"
        case "$component" in
            os-skills) continue ;;
            agent-sec-core)
                build_env_require_rust "$directory/v2"
                build_env_require_rust "$directory/linux-sandbox"
                if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
                    build_env_add_package libc6-dev
                else
                    build_env_add_package glibc-static
                fi
                ;;
            ws-ckpt)
                build_env_require_rust "$directory/src"
                ;;
            *) build_env_require_rust "$directory" ;;
        esac
        case "$component" in
            agent-memory|agent-sec-core|tokenless|ws-ckpt)
                if ! skip_component_plugins "$component"; then BUILD_ENV_NEEDS_NODE=true; fi ;;
        esac
        case "$component" in
            tokenless) BUILD_ENV_NEEDS_JUST=true; build_env_add_package patch ;;
            agentsight)
                # The embedded dashboard is part of the core build.
                BUILD_ENV_NEEDS_NODE=true
                # libbpf-cargo formats production eBPF skeletons with rustfmt.
                channel="$(build_env_channel "$directory")"
                build_env_add_rust_component "${channel:-stable}" rustfmt
                build_env_add_package clang llvm perl patch
                if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
                    build_env_add_package libclang-dev libbpf-dev libelf-dev zlib1g-dev libssl-dev
                else
                    build_env_add_package clang-devel libbpf-devel libbpf-static elfutils-libelf-devel zlib-devel openssl-devel perl-IPC-Cmd perl-FindBin
                fi
                ;;
            skillfs)
                if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
                    build_env_add_package libfuse3-dev
                else
                    build_env_add_package fuse3-devel
                fi
                ;;
            agent-memory)
                build_env_add_package cmake
                if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
                    build_env_add_package libsystemd-dev
                else
                    build_env_add_package systemd-devel
                fi
                ;;
        esac
        # OpenSSL and the native compiler are shared by several Rust dependencies.
        if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
            build_env_add_package build-essential pkg-config libssl-dev
        else
            build_env_add_package gcc gcc-c++ pkgconfig openssl-devel
        fi
    done
}

build_env_package_present() {
    if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
        [[ "$(dpkg-query -W -f='${Status}' "$1" 2>/dev/null || true)" == 'install ok installed' ]]
    else
        rpm -q "$1" >/dev/null 2>&1 || rpm -q --whatprovides "$1" >/dev/null 2>&1
    fi
}

build_env_missing_packages() {
    local package
    BUILD_ENV_MISSING_PACKAGES=()
    for package in "${BUILD_ENV_PACKAGES[@]}"; do
        if ! build_env_package_present "$package"; then
            BUILD_ENV_MISSING_PACKAGES+=("$package")
        fi
    done
}

build_env_rust_version() {
    local output channel
    if command -v rustup >/dev/null 2>&1; then
        # Invoking a rustup shim directly can install a missing directory pin,
        # which would make --check-deps unexpectedly mutate the toolchain store.
        channel="${1:-${RUSTUP_TOOLCHAIN:-$(rustup default 2>/dev/null | awk '{ print $1 }' || true)}}"
        [[ -n "$channel" ]] && build_env_toolchain_present "$channel" || return 0
        output="$(rustup run "$channel" rustc --version 2>/dev/null)" || return 0
    else
        output="$(rustc --version 2>/dev/null)" || return 0
    fi
    printf '%s\n' "$output" | awk '{ print $2 }'
}

build_env_rust_component_present() {
    local channel="$1" component="$2"
    build_env_toolchain_present "$channel" &&
        rustup component list --installed --toolchain "$channel" 2>/dev/null | \
        awk -v wanted="$component" '$1 == wanted || index($1, wanted "-") == 1 { found = 1 } END { exit !found }'
}

build_env_toolchain_present() {
    command -v rustup >/dev/null 2>&1 &&
        rustup toolchain list 2>/dev/null | awk '{ print $1 }' | \
        awk -v wanted="$1" '$0 == wanted || index($0, wanted "-") == 1 { found = 1 } END { exit !found }'
}

build_env_node_present() {
    local major
    command -v node >/dev/null 2>&1 && command -v npm >/dev/null 2>&1 || return 1
    major="$(node --version 2>/dev/null)" || return 1
    major="${major#v}"
    major="${major%%.*}"
    [[ "$major" =~ ^[0-9]+$ ]] && (( major >= 22 ))
}

build_env_use_cached_tools() {
    local node_root="${XDG_CACHE_HOME:-$HOME/.cache}/anolisa/build-tools/node"
    export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$node_root/bin:$PATH"
    hash -r
}

build_env_validate_download_url() {
    local url="$1" name="$2" authority
    authority="${url#https://}"
    authority="${authority%%[/?#]*}"
    [[ "$url" == https://* && -n "$authority" && "$authority" != *@* && "$url" != *[[:space:]]* ]] ||
        die "$name must be an absolute HTTPS URL without credentials or whitespace"
}

build_env_install_node() (
    # Install into the caller's cache so system Node and shell startup files are untouched.
    local architecture archive checksum temporary node_root mirror="${NODE_DIST_MIRROR-https://nodejs.org/dist}"
    build_env_validate_download_url "$mirror" NODE_DIST_MIRROR
    [[ "$mirror" != *[?#]* ]] || die 'NODE_DIST_MIRROR must not contain a query or fragment'
    mirror="${mirror%/}"
    case "$(uname -m)" in
        x86_64) architecture=x64 ;;
        aarch64|arm64) architecture=arm64 ;;
        *) die "Automatic Node.js setup supports Linux x86_64 and aarch64; provide Node.js >=22 and npm" ;;
    esac
    node_root="${XDG_CACHE_HOME:-$HOME/.cache}/anolisa/build-tools/node"
    if $DRY_RUN; then
        info "Would install checksum-verified Node.js 22 into $node_root"
        return 0
    fi
    temporary="$(mktemp -d)"
    trap 'rm -rf -- "$temporary"' EXIT
    curl --proto '=https' --proto-redir '=https' -fsSL --retry 3 "$mirror/latest-v22.x/SHASUMS256.txt" -o "$temporary/SHASUMS256.txt"
    archive="$(awk -v arch="$architecture" '$2 ~ ("^node-v22\\.[0-9]+\\.[0-9]+-linux-" arch "\\.tar\\.xz$") { print $2; exit }' "$temporary/SHASUMS256.txt")"
    [[ -n "$archive" ]] || die "Node.js 22 checksums contain no Linux $architecture archive"
    checksum="$(awk -v archive="$archive" '$2 == archive { print $1; exit }' "$temporary/SHASUMS256.txt")"
    [[ "$checksum" =~ ^[0-9a-f]{64}$ ]] || die "Invalid checksum for $archive"
    curl --proto '=https' --proto-redir '=https' -fsSL --retry 3 "$mirror/latest-v22.x/$archive" -o "$temporary/$archive"
    (cd "$temporary" && printf '%s  %s\n' "$checksum" "$archive" | sha256sum -c -) ||
        die "Node.js archive checksum verification failed"
    mkdir -p "$temporary/node" "$(dirname "$node_root")"
    tar -xJf "$temporary/$archive" --strip-components=1 -C "$temporary/node"
    [[ -x "$temporary/node/bin/node" && -f "$temporary/node/bin/npm" ]] || die "Incomplete Node.js archive"
    mkdir -p "$node_root"
    cp -a "$temporary/node/." "$node_root/"
)

build_env_install_rustup() (
    local temporary url="${RUSTUP_INIT_SCRIPT_URL-https://sh.rustup.rs}"
    build_env_validate_download_url "$url" RUSTUP_INIT_SCRIPT_URL
    if $DRY_RUN; then
        info "Would install rustup with the minimal stable toolchain for the current user"
        return 0
    fi
    temporary="$(mktemp -d)"
    trap 'rm -rf -- "$temporary"' EXIT
    curl --proto '=https' --proto-redir '=https' -fsSL --retry 3 "$url" -o "$temporary/rustup-init.sh"
    sh "$temporary/rustup-init.sh" -y --profile minimal --default-toolchain stable --no-modify-path
)

build_env_system_packages_allowed() {
    case "${INSTALL_SYSTEM_DEPS:-auto}" in
        true) return 0 ;;
        auto) [[ "${INSTALL_MODE:-system}" == system ]] ;;
        *) return 1 ;;
    esac
}

prepare_build_environment() {
    resolve_build_environment
    build_env_use_cached_tools
    build_env_missing_packages
    if (( ${#BUILD_ENV_MISSING_PACKAGES[@]} )); then
        # Decide before any toolchain download or privileged package operation.
        if ! build_env_system_packages_allowed; then
            warn "Missing native build packages: ${BUILD_ENV_MISSING_PACKAGES[*]}"
            warn 'User mode does not install system packages. Install them separately or rerun with --install-system-deps.'
            return 1
        fi
        info "Preparing native build packages: ${BUILD_ENV_MISSING_PACKAGES[*]}"
        if [[ "$BUILD_ENV_PACKAGE_KIND" == deb ]]; then
            run_cmd as_root apt-get update
        fi
        run_cmd as_root "$BUILD_ENV_PACKAGE_MANAGER" install -y "${BUILD_ENV_MISSING_PACKAGES[@]}"
    fi

    if $BUILD_ENV_NEEDS_RUST; then
        if ! command -v rustup >/dev/null 2>&1; then
            build_env_install_rustup
            build_env_use_cached_tools
        fi
        local channel current minimum requirement component stable_requested=false
        for channel in "${BUILD_ENV_TOOLCHAINS[@]}"; do
            if ! build_env_toolchain_present "$channel"; then
                run_cmd rustup toolchain install "$channel" --profile minimal
            elif [[ "$channel" == stable ]]; then
                current="$(build_env_rust_version "$channel")"
                minimum="$(build_env_rust_minimum "$channel")"
                if ! build_env_version_at_least "$current" "$minimum"; then
                    run_cmd rustup toolchain install stable --profile minimal
                fi
            fi
            [[ "$channel" != stable ]] || stable_requested=true
        done
        for requirement in "${BUILD_ENV_RUST_COMPONENTS[@]}"; do
            channel="${requirement%%:*}"
            component="${requirement#*:}"
            if ! build_env_rust_component_present "$channel" "$component"; then
                run_cmd rustup component add "$component" --toolchain "$channel"
            fi
        done
    fi
    if $BUILD_ENV_NEEDS_NODE && ! build_env_node_present; then
        build_env_install_node
        build_env_use_cached_tools
    fi
    if $BUILD_ENV_NEEDS_JUST && ! command -v just >/dev/null 2>&1; then
        if ! build_env_toolchain_present stable && ! ${stable_requested:-false}; then
            run_cmd rustup toolchain install stable --profile minimal
        fi
        run_cmd rustup run stable cargo install just --locked
    fi
    if ! $DRY_RUN; then
        build_env_check_resolved || die "Build dependencies remain unavailable after preparation; see the missing requirements above"
    fi
}

build_env_check_resolved() {
    local failures=0 channel current minimum requirement component
    build_env_missing_packages
    if (( ${#BUILD_ENV_MISSING_PACKAGES[@]} )); then
        warn "Missing native build packages: ${BUILD_ENV_MISSING_PACKAGES[*]}"
        failures=$((failures + 1))
    fi
    if $BUILD_ENV_NEEDS_RUST; then
        for channel in "${BUILD_ENV_TOOLCHAINS[@]}"; do
            if ! build_env_toolchain_present "$channel"; then
                warn "Missing Rust toolchain: $channel"
                failures=$((failures + 1))
                continue
            fi
            current="$(build_env_rust_version "$channel")"
            minimum="$(build_env_rust_minimum "$channel")"
            if ! build_env_version_at_least "$current" "$minimum"; then
                warn "Rust toolchain $channel must provide rustc >=$minimum"
                failures=$((failures + 1))
            fi
        done
        if ! command -v cargo >/dev/null 2>&1; then
            warn "Cargo is required for the selected components"
            failures=$((failures + 1))
        fi
        for requirement in "${BUILD_ENV_RUST_COMPONENTS[@]}"; do
            channel="${requirement%%:*}"
            component="${requirement#*:}"
            if ! build_env_rust_component_present "$channel" "$component"; then
                warn "Missing Rust component: $component ($channel)"
                failures=$((failures + 1))
            fi
        done
    fi
    if $BUILD_ENV_NEEDS_NODE && ! build_env_node_present; then
        warn "Node.js >=22 and npm are required for the selected component plugins"
        failures=$((failures + 1))
    fi
    if $BUILD_ENV_NEEDS_JUST && ! command -v just >/dev/null 2>&1; then
        warn "Tokenless builds require just"
        failures=$((failures + 1))
    fi
    # File-layout checks use built-in generic annotations, available since Python 3.9.
    if ! command -v python3 >/dev/null 2>&1 || ! python3 -c 'import sys; sys.exit(sys.version_info < (3, 9))'; then
        warn "System python3 >=3.9 is required for build and file-layout helpers; uv and a virtual environment are unnecessary"
        failures=$((failures + 1))
    fi
    (( failures == 0 ))
}

check_build_environment() {
    resolve_build_environment
    build_env_use_cached_tools
    build_env_check_resolved
}
