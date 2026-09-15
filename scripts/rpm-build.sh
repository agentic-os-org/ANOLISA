#!/bin/bash
# =============================================================================
# Unified RPM build script for ANOLISA
# Usage:
#   ./scripts/rpm-build.sh <package>        Build a single package
#   ./scripts/rpm-build.sh all              Build all packages
#
# Packages: copilot-shell, agent-sec-core, os-skills, agentsight, tokenless, agent-memory, skillfs, ktuner, anolisa, cosh-ng
#
# Environment variables:
#   VERSION    Override version for .spec.in templates (default: auto-detect)
#   RPMBUILD   Path to rpmbuild binary (default: rpmbuild)
# =============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(dirname "$SCRIPT_DIR")"
BUILD_DIR="${SCRIPT_DIR}/rpmbuild"
RPMBUILD="${RPMBUILD:-rpmbuild}"

# Source directories
SHELL_DIR="${ROOT_DIR}/src/copilot-shell"
SEC_DIR="${ROOT_DIR}/src/agent-sec-core"
SKILLS_DIR="${ROOT_DIR}/src/os-skills"
SIGHT_DIR="${ROOT_DIR}/src/agentsight"
TOKEN_DIR="${ROOT_DIR}/src/tokenless"
MEM_DIR="${ROOT_DIR}/src/agent-memory"
SKILLFS_DIR="${ROOT_DIR}/src/skillfs"
KTUNER_DIR="${ROOT_DIR}/src/ktuner"
COSH_DIR="${ROOT_DIR}/src/cosh-ng"
SANDBOX_PKG_DIR="${ROOT_DIR}/src/anolisa/packaging/sandbox"

# gVisor upstream release (overridable via env). Format: YYYYMMDD
GVISOR_RELEASE="${GVISOR_RELEASE:-20260601}"
GVISOR_RELEASE_VERSION="${GVISOR_RELEASE}.0"
GVISOR_BASE_URL="${GVISOR_BASE_URL:-https://storage.googleapis.com/gvisor/releases/release}"

# Distro tag override (e.g. .alinux4 / .alinux3). Empty = let rpmbuild choose.
DIST_TAG="${DIST_TAG:-}"

# Architecture detection (host arch, override with TARGET_ARCH=x86_64|aarch64)
HOST_ARCH="${TARGET_ARCH:-$(uname -m)}"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log()  { echo -e "${CYAN}[INFO]${NC} $*" >&2; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*" >&2; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }
ok()   { echo -e "${GREEN}[OK]${NC} $*" >&2; }

# -----------------------------------------------------------------------------
# Install a package using the available package manager
# -----------------------------------------------------------------------------
install_package() {
    local pkg="$1"
    if command -v dnf &>/dev/null; then
        dnf install -y "$pkg"
    elif command -v yum &>/dev/null; then
        yum install -y "$pkg"
    else
        err "No supported package manager found (dnf/yum)"
        return 1
    fi
}

# -----------------------------------------------------------------------------
# Setup rpmbuild directory tree under scripts/rpmbuild/
# -----------------------------------------------------------------------------
setup_rpmbuild() {
    log "Setting up rpmbuild tree at ${BUILD_DIR}"
    mkdir -p "${BUILD_DIR}"/{BUILD,RPMS,SOURCES,SPECS,SRPMS}
}

# -----------------------------------------------------------------------------
# Parse version from a spec or spec.in file
# -----------------------------------------------------------------------------
parse_spec_version() {
    local spec_file="$1"
    grep -E '^Version:' "$spec_file" | awk '{print $2}' | tr -d ' \t'
}

# -----------------------------------------------------------------------------
# Parse name from a spec or spec.in file
# -----------------------------------------------------------------------------
parse_spec_name() {
    local spec_file="$1"
    grep -E '^Name:' "$spec_file" | awk '{print $2}' | tr -d ' \t'
}

# -----------------------------------------------------------------------------
# Process .spec.in template -> .spec (replace @VERSION@)
# Returns the path of the generated .spec file
# -----------------------------------------------------------------------------
process_spec_template() {
    local spec_in="$1"
    local version="$2"
    local spec_out="${BUILD_DIR}/SPECS/$(basename "${spec_in%.in}")"

    log "Processing template: $(basename "$spec_in") -> $(basename "$spec_out") (version=${version})"
    sed "s/@VERSION@/${version}/g" "$spec_in" > "$spec_out"
    echo "$spec_out"
}

# =============================================================================
# copilot-shell
# =============================================================================
build_copilot_shell() {
    log "=========================================="
    log "Building RPM: copilot-shell"
    log "=========================================="

    local spec_in="${SHELL_DIR}/copilot-shell.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version from package.json or env
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(node -p "require('${SHELL_DIR}/package.json').version" 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        err "Cannot determine copilot-shell version. Set VERSION env or ensure package.json exists."
        return 1
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    # Step 1: Process spec template
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    # Step 2: Build bundle (npm install + bundle + prepare:package)
    log "Step 1/3: Building copilot-shell bundle..."
    (
        cd "$SHELL_DIR"
        npm install --ignore-scripts
        npm run bundle
        npm run prepare:package
    )

    # Step 3: Create source tarball
    log "Step 2/3: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    # Copy the necessary files (same as spec %install expects)
    cp -rp "${SHELL_DIR}/dist"/* "$pkg_dir/"
    [ -f "${SHELL_DIR}/LICENSE" ] && cp "${SHELL_DIR}/LICENSE" "$pkg_dir/"
    [ -f "${SHELL_DIR}/README.md" ] && cp "${SHELL_DIR}/README.md" "$pkg_dir/"
    # Also include full source for rpmbuild %build section
    local excludes=(
        --exclude='.git'
        --exclude='node_modules'
        --exclude='dist'
        --exclude='coverage'
        --exclude='.DS_Store'
    )
    rm -rf "$pkg_dir"
    mkdir -p "$pkg_dir"
    tar -cf - -C "$SHELL_DIR" "${excludes[@]}" . | tar -xf - -C "$pkg_dir"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    # Step 4: rpmbuild (--nodeps: BuildRequires are handled by yum-builddep in CI)
    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "copilot-shell RPM built successfully"
}

# =============================================================================
# agent-sec-core: payload shared by the V1 and V2 source tarballs
#
# Args: <pkg_dir>  destination staging directory (must already exist)
#
# Only the CLI/daemon layer differs between V1 and V2, so both callers stage the
# same component payload here and then add their own layer. Keeping the trees
# separate matters: the V1 tarball must keep its original payload inventory, so
# nothing V2-specific may leak into this helper.
#
# Note: rust-toolchain.toml is intentionally excluded from the tarball. The
# source file pins a Rust version that rpmbuild environments may not have, so by
# omitting it cargo falls back to whatever Rust the build environment provides.
# =============================================================================
stage_sec_core_payload() {
    local pkg_dir="$1"

    mkdir -p "$pkg_dir"/{skills,linux-sandbox,cosh-extension,openclaw-plugin,hermes-plugin,qwen-code-extension,qoder-plugin,tools}

    # skills: use cp -rp dir/. to include hidden files/directories
    cp -rp "${SEC_DIR}/skills/." "$pkg_dir/skills/"
    cp -rp "${SEC_DIR}/linux-sandbox/"* "$pkg_dir/linux-sandbox/"
    rm -f "$pkg_dir/linux-sandbox/rust-toolchain.toml"
    cp -rp "${SEC_DIR}/cosh-extension/"* "$pkg_dir/cosh-extension/"
    cp -p "${SEC_DIR}/tools/sign-skill.sh" "$pkg_dir/tools/"
    cp "${SEC_DIR}/Makefile" "$pkg_dir/"
    tar -cf - -C "${SEC_DIR}" \
        .anolisa/ | tar -xf - -C "$pkg_dir/"
    [ -f "${SEC_DIR}/LICENSE" ] && cp "${SEC_DIR}/LICENSE" "$pkg_dir/"
    [ -f "${SEC_DIR}/README.md" ] && cp "${SEC_DIR}/README.md" "$pkg_dir/"

    # openclaw-plugin (exclude node_modules and dev artifacts)
    tar -cf - -C "${SEC_DIR}" \
        --exclude='node_modules' \
        --exclude='.tsbuildinfo' \
        openclaw-plugin/ | tar -xf - -C "$pkg_dir/"

    # hermes-plugin (exclude __pycache__ and dev artifacts)
    tar -cf - -C "${SEC_DIR}" \
        --exclude='__pycache__' \
        hermes-plugin/src hermes-plugin/scripts | tar -xf - -C "$pkg_dir/"

    # qwen-code-extension (exclude Python cache artifacts)
    tar -cf - -C "${SEC_DIR}" \
        --exclude='__pycache__' \
        qwen-code-extension/ | tar -xf - -C "$pkg_dir/"

    # codex-plugin (hooks + install script + .agents registry, exclude __pycache__)
    tar -cf - -C "${SEC_DIR}" \
        --exclude='__pycache__' \
        codex-plugin/hooks-plugin codex-plugin/install.sh codex-plugin/.agents | tar -xf - -C "$pkg_dir/"

    # qoder-plugin (hooks + install script, exclude __pycache__)
    tar -cf - -C "${SEC_DIR}" \
        --exclude='__pycache__' \
        qoder-plugin/ | tar -xf - -C "$pkg_dir/"
}

# =============================================================================
# agent-sec-core
# =============================================================================
build_agent_sec_core() {
    log "=========================================="
    log "Building RPM: agent-sec-core"
    log "=========================================="

    local spec_in="${SEC_DIR}/agent-sec-core.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version: prefer $VERSION env (set by nightly CI), fallback to pyproject.toml
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version' "${SEC_DIR}/agent-sec-cli/pyproject.toml" | sed 's/.*"\(.*\)"/\1/')
    fi
    if [ -z "$version" ]; then
        err "Cannot determine agent-sec-core version. Set VERSION env or ensure pyproject.toml exists."
        return 1
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    # Step 1: Process spec template (@VERSION@ -> actual version)
    log "Step 1/3: Preparing spec file..."
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    # Step 2: Create source tarball
    log "Step 2/3: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"/{agent-sec-cli,scripts}
    stage_sec_core_payload "$pkg_dir"
    mkdir -p "$pkg_dir/packaging/systemd"
    cp -p "${SEC_DIR}/packaging/systemd/agent-sec-core.service.in" "$pkg_dir/packaging/systemd/"

    cp -p "${SEC_DIR}/scripts/agent-sec-cli-wrapper.sh" "$pkg_dir/scripts/"
    cp -p "${SEC_DIR}/scripts/agent-sec-daemon-wrapper.sh" "$pkg_dir/scripts/"

    # Include agent-sec-cli source for maturin wheel build
    # Exclude development artifacts (.venv, target, __pycache__, .egg-info, dist)
    tar -cf - -C "${SEC_DIR}" \
        --exclude='.venv' \
        --exclude='target' \
        --exclude='__pycache__' \
        --exclude='*.egg-info' \
        --exclude='dist' \
        --exclude='.pytest_cache' \
        agent-sec-cli/ | tar -xf - -C "$pkg_dir/"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    # Step 3: rpmbuild (--nodeps: BuildRequires are handled by yum-builddep in CI)
    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "agent-sec-core RPM built successfully"
}

# =============================================================================
# agent-sec-core-v2 (V2 Rust CLI/daemon instead of the Python wheel)
# =============================================================================
build_agent_sec_core_v2() {
    log "=========================================="
    log "Building RPM: agent-sec-core (V2)"
    log "=========================================="

    local spec_in="${SEC_DIR}/agent-sec-core.spec.v2.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version: prefer $VERSION env, otherwise the V2 workspace owns its version.
    # V2 deliberately does not read agent-sec-cli/pyproject.toml: the Rust tree
    # must stand on its own once the Python CLI is retired.
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(awk '
            $0 == "[workspace.package]" { in_section = 1; next }
            in_section && /^\[/ { exit }
            in_section && /^version = / {
                value = $0
                sub(/^version = "/, "", value)
                sub(/"$/, "", value)
                print value
                exit
            }
        ' "${SEC_DIR}/v2/Cargo.toml")
    fi
    if [ -z "$version" ]; then
        err "Cannot determine agent-sec-core V2 version. Set VERSION env or ensure v2/Cargo.toml has [workspace.package] version."
        return 1
    fi

    # Both versions are bumped together by scripts/bump-version.sh; warn (do not
    # fail) on drift, because V1 is on its way out and must not gate a V2 build.
    local v1_version
    v1_version=$(grep -m1 '^version' "${SEC_DIR}/agent-sec-cli/pyproject.toml" 2>/dev/null | sed 's/.*"\(.*\)"/\1/' || true)
    if [ -n "$v1_version" ] && [ "$v1_version" != "$version" ]; then
        warn "V1 version ($v1_version) differs from V2 version ($version); bump-version.sh may have missed a file"
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    # Step 1: Process spec template. The generated name is explicit instead of
    # derived, so the template suffix does not produce agent-sec-core.spec.v2.
    log "Step 1/3: Preparing spec file..."
    local spec_file="${BUILD_DIR}/SPECS/${pkg_name}-v2.spec"
    log "Processing template: $(basename "$spec_in") -> $(basename "$spec_file") (version=${version})"
    sed "s/@VERSION@/${version}/g" "$spec_in" > "$spec_file"

    # Step 2: Create source tarball. Same top-level directory as V1 because the
    # spec still unpacks %{name}-%{version} via `%setup -q`.
    log "Step 2/3: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"
    stage_sec_core_payload "$pkg_dir"
    mkdir -p "$pkg_dir/packaging/systemd"
    cp -p "${SEC_DIR}/packaging/systemd/agent-sec-core-v2.service.in" \
        "${SEC_DIR}/packaging/systemd/agent-sec-core-v2.sysusers" "$pkg_dir/packaging/systemd/"

    # V2 layer: the Rust workspace replaces agent-sec-cli/ and the two Python
    # wrapper scripts, which are therefore absent from this tarball.
    tar -cf - -C "${SEC_DIR}" \
        --exclude='target' \
        v2/ | tar -xf - -C "$pkg_dir/"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    # Step 3: rpmbuild (--nodeps: BuildRequires are handled by yum-builddep in CI)
    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "agent-sec-core V2 RPM built successfully"
}

# =============================================================================
# os-skills
# =============================================================================
build_agentic_os_skills() {
    log "=========================================="
    log "Building RPM: os-skills"
    log "=========================================="

    local spec_in="${SKILLS_DIR}/os-skills.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version from env or default
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        # Try to read from spec changelog (first entry)
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        version="0.0.1"
        warn "No version specified for os-skills, using default: ${version}"
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    # Step 1: Process spec template
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    # Step 2: Create source tarball
    log "Step 1/2: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    # Copy skill directories
    for dir in ai aliyun devops monitor-perf others security system-admin; do
        [ -d "${SKILLS_DIR}/${dir}" ] && cp -rp "${SKILLS_DIR}/${dir}" "$pkg_dir/"
    done
    
    if [ -f "${SKILLS_DIR}/LICENSE" ]; then
        cp -L "${SKILLS_DIR}/LICENSE" "$pkg_dir/"
    elif [ -f "${ROOT_DIR}/LICENSE" ]; then
        cp "${ROOT_DIR}/LICENSE" "$pkg_dir/"
    fi

    # component.toml — spec %install installs it to %{_datadir}/anolisa/components/os-skills/
    [ -f "${SKILLS_DIR}/component.toml" ] && cp "${SKILLS_DIR}/component.toml" "$pkg_dir/"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    # Step 3: rpmbuild (--nodeps: BuildRequires are handled by yum-builddep in CI)
    log "Step 2/2: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "os-skills RPM built successfully"
}

# =============================================================================
# agentsight
# =============================================================================
build_agentsight() {
    log "=========================================="
    log "Building RPM: agentsight"
    log "=========================================="

    local spec_in="${SIGHT_DIR}/agentsight.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version from env or Cargo.toml
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version' "${SIGHT_DIR}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        version="0.0.1"
        warn "No version specified for agentsight, using default: ${version}"
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    log "Step 1/3: Building agentsight..."
    if ! command -v clang &>/dev/null; then
        log "clang not found, installing..."
        install_package clang || { err "Failed to install clang"; return 1; }
    fi
    (
        cd "$SIGHT_DIR"
        # Build frontend (embed into Rust binary via include_dir!)
        if [ -d "${SIGHT_DIR}/dashboard" ] && command -v npm &>/dev/null; then
            log "Building frontend..."
            cd "${SIGHT_DIR}/dashboard"
            npm install
            npm run build:embed
            cd "$SIGHT_DIR"
        else
            local reason=""
            [ -d "${SIGHT_DIR}/dashboard" ] || reason="${reason}dashboard/ not found at ${SIGHT_DIR}/dashboard; "
            command -v npm &>/dev/null || reason="${reason}npm not available in PATH; "
            err "Cannot build agentsight frontend: ${reason}"
            err "Frontend embedding is required for WebUI/serve tests. Aborting build."
            exit 1
        fi
        cargo build --release --bin agentsight
        ./scripts/build-enforcer.sh
    )

    # Step 2: Process spec template and create tarball
    log "Step 2/3: Preparing spec and source tarball..."
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    "${SIGHT_DIR}/scripts/stage-rpm-payload.sh" "$pkg_dir"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "agentsight RPM built successfully"
}

# =============================================================================
# tokenless
# =============================================================================
build_tokenless() {
    log "=========================================="
    log "Building RPM: tokenless"
    log "=========================================="

    local spec_in="${TOKEN_DIR}/tokenless.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version from env or Cargo.toml workspace
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version' "${TOKEN_DIR}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        version="0.0.1"
        warn "No version specified for tokenless, using default: ${version}"
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    # Step 1: Process spec template
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    log "Step 1/3: Setting up rtk vendored source..."
    command -v just &>/dev/null || { err "'just' is required for RPM build. Install: cargo install just"; exit 1; }
    (
        cd "$TOKEN_DIR"
        # Clone rtk into third_party/ (no submodule — uses justfile setup-rtk)
        # Note: rtk source in tarball is already patched via justfile setup-rtk
        if [ ! -d "third_party/rtk/.git" ]; then
            just setup-rtk
        fi
    )

    log "Step 2/3: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    # Copy full source tree (including vendored rtk), excluding build artifacts and VCS
    # Note: third_party/rtk must be included — it's built separately via --manifest-path
    # Adapter config files (manifest.json, package.json, openclaw.plugin.json, plugin.yaml)
    # and the component contract (component.toml) are excluded because they are
    # generated from .in templates by stamp-adapter-templates during rpmbuild
    # %build (make build-openclaw-plugin). Excluding them ensures the RPM build
    # always regenerates from the authoritative .in template, preventing stale
    # contracts from shipping (see GH-1470).
    tar -cf - -C "$TOKEN_DIR" \
        --exclude='target' \
        --exclude='.git' \
        --exclude='node_modules' \
        --exclude='__pycache__' \
        --exclude='*.pyc' \
        --exclude='.anolisa/component.toml' \
        --exclude='adapters/tokenless/manifest.json' \
        --exclude='adapters/tokenless/openclaw/package.json' \
        --exclude='adapters/tokenless/openclaw/openclaw.plugin.json' \
        --exclude='adapters/tokenless/hermes/plugin.yaml' \
        --exclude='adapters/tokenless/qoder/.qoder-plugin/plugin.json' \
        --exclude='adapters/tokenless/claude-code/.claude-plugin/plugin.json' \
        --exclude='adapters/tokenless/codex/.codex-plugin/plugin.json' \
        --exclude='adapters/tokenless/qwencode/qwen-extension.json' \
        --exclude='adapters/tokenless/qwenpaw/plugin.json' \
        --exclude='adapters/tokenless/qwenpaw/requirements.txt' \
        . | tar -xf - -C "$pkg_dir"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "tokenless RPM built successfully"
}

# =============================================================================
# agent-memory
# =============================================================================
build_agent_memory() {
    log "=========================================="
    log "Building RPM: agent-memory"
    log "=========================================="

    local spec_in="${MEM_DIR}/agent-memory.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Always clean source-tree vendoring artefacts on exit (success or
    # failure), so a `set -e` mid-build can't leave $MEM_DIR/vendor/
    # and $MEM_DIR/.cargo/ behind to pollute the developer's git tree
    # or confuse subsequent non-vendored cargo builds.
    # shellcheck disable=SC2064  # we want $MEM_DIR expanded now
    trap "rm -rf '${MEM_DIR}/vendor' '${MEM_DIR}/.cargo'" RETURN

    # Version from env, Cargo.toml, then spec fallback
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version' "${MEM_DIR}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        # Hard fail rather than burying a stale fallback that drifts from
        # Cargo.toml. The build must derive its version from the
        # authoritative source (Cargo.toml → spec.in @VERSION@).
        err "Could not derive agent-memory version from VERSION env, Cargo.toml, or ${spec_in}"
        exit 1
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"

    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    # Build the OpenClaw TS plugin first so its dist/ is part of the
    # source archive — the spec's %install copies the prebuilt bundle
    # rather than running npm during rpmbuild (no network in mock).
    log "Step 1/4: Building OpenClaw TS plugin..."
    cd "$MEM_DIR" && make build-openclaw-plugin

    # The source-archive top-level dir must match `%setup -n %{name}-%{version}`
    # in the spec, so the unpacked tree lines up with the CI-produced
    # archive from .github/actions/package-source.
    log "Step 2/4: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    # Single tar pass: copy the whole source tree minus build artefacts.
    # The previous two-pass implementation hard-failed under `set -e`
    # because the first pass referenced an `adapters/` directory that
    # only existed in agent-sec-core. Now agent-memory ships its own
    # adapters/ (the OpenClaw plugin built above) so a single pass
    # captures it via the default include.
    tar -cf - -C "$MEM_DIR" \
        --exclude='target' \
        --exclude='./dist' \
        --exclude='.git' \
        --exclude='vendor' \
        --exclude='.cargo' \
        --exclude='node_modules' \
        --exclude='.tsbuildinfo' \
        --exclude='tests' \
        . | tar -xf - -C "$pkg_dir"

    # Vendor tarball for --offline cargo build. Must run BEFORE copying
    # .cargo/config.toml into the source tarball so the vendored-sources
    # config (not the original crates-io one) ends up in Source0.
    log "Step 3/4: Creating vendor tarball..."
    cd "$MEM_DIR" && cargo vendor vendor/
    mkdir -p "$MEM_DIR"/.cargo
    printf '[source.crates-io]\nreplace-with = "vendored-sources"\n\n[source.vendored-sources]\ndirectory = "vendor"\n' > "$MEM_DIR"/.cargo/config.toml
    local vendor_tmp
    vendor_tmp=$(mktemp -d)
    cp -R "$MEM_DIR"/vendor "$vendor_tmp"/vendor
    tar czf "${BUILD_DIR}/SOURCES/${pkg_name}-${version}-vendor.tar.gz" -C "$vendor_tmp" vendor
    rm -rf "$vendor_tmp"

    # .cargo/config.toml is now the vendored-sources version; copy it
    # into Source0 so cargo --offline can find the local vendor/ dir.
    # vendor/ itself is in Source1, extracted by %setup -a 1.
    mkdir -p "$pkg_dir"/.cargo
    cp "$MEM_DIR"/.cargo/config.toml "$pkg_dir"/.cargo/

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 4/4: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "agent-memory RPM built successfully"
}

# =============================================================================
# anolisa
# =============================================================================
build_anolisa() {
    log "=========================================="
    log "Building RPM: anolisa"
    log "=========================================="

    log "Step 1/3: Creating source tarball from committed snapshot..."
    local snapshot_tmp
    snapshot_tmp=$(mktemp -d)
    (
        trap 'rm -rf -- "$snapshot_tmp"' EXIT
        local extract_dir="${snapshot_tmp}/source"
        mkdir -p "$extract_dir"

        # Both source archives must share this snapshot so a dirty worktree
        # cannot pair committed sources with dependencies from another revision.
        git -C "$ROOT_DIR" archive --format=tar HEAD:src/anolisa |
            tar -xf - -C "$extract_dir"

        local spec_in="${extract_dir}/anolisa.spec.in"
        if [ ! -f "$spec_in" ]; then
            err "Spec template not found in committed snapshot: $spec_in"
            return 1
        fi

        local version="${VERSION:-}"
        if [ -z "$version" ]; then
            version=$(grep -m1 '^version = ' "${extract_dir}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
        fi
        if [ -z "$version" ]; then
            err "Cannot determine anolisa version from committed Cargo.toml. Set VERSION to override it."
            return 1
        fi

        local pkg_name
        pkg_name=$(parse_spec_name "$spec_in")
        local pkg_dir="${snapshot_tmp}/${pkg_name}"
        mv "$extract_dir" "$pkg_dir"
        spec_in="${pkg_dir}/anolisa.spec.in"

        local tarball_name="${pkg_name}-${version}.tar.gz"
        local vendor_tarball_name="${pkg_name}-${version}-vendor.tar.gz"
        local spec_file
        spec_file=$(process_spec_template "$spec_in" "$version")

        tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$snapshot_tmp" "$pkg_name"

        log "Step 2/3: Creating vendor tarball ${vendor_tarball_name}..."
        local vendor_tmp
        vendor_tmp=$(mktemp -d)
        (
            trap 'rm -rf -- "$vendor_tmp"' EXIT
            (
                cd "$pkg_dir"
                cargo vendor --locked "${vendor_tmp}/vendor" >/dev/null
            )
            tar -czf "${BUILD_DIR}/SOURCES/${vendor_tarball_name}" -C "$vendor_tmp" vendor
        )

        log "Step 3/3: Running rpmbuild..."
        "$RPMBUILD" -ba --nodeps \
            --define "_topdir ${BUILD_DIR}" \
            "$spec_file"

        ok "anolisa RPM built successfully"
    )
}

# =============================================================================
# skillfs
# =============================================================================
build_skillfs() {
    log "=========================================="
    log "Building RPM: skillfs"
    log "=========================================="

    local spec_in="${SKILLFS_DIR}/skillfs.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version = ' "${SKILLFS_DIR}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        err "Cannot determine skillfs version. Set VERSION env or ensure Cargo.toml/spec exists."
        return 1
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"
    local vendor_tarball_name="${pkg_name}-${version}-vendor.tar.gz"
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    log "Step 1/3: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    tar -cf - -C "$SKILLFS_DIR" \
        --exclude='target' \
        --exclude='vendor' \
        --exclude='.cargo' \
        . | tar -xf - -C "$pkg_dir"

    if [ -L "${pkg_dir}/LICENSE" ] && [ -f "${ROOT_DIR}/LICENSE" ]; then
        rm -f "${pkg_dir}/LICENSE"
        cp -p "${ROOT_DIR}/LICENSE" "${pkg_dir}/LICENSE"
    fi

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 2/3: Creating vendor tarball ${vendor_tarball_name}..."
    local vendor_tmp
    vendor_tmp=$(mktemp -d)
    mkdir -p "${vendor_tmp}/.cargo"
    (
        cd "$SKILLFS_DIR"
        cargo vendor --locked "${vendor_tmp}/vendor" >/dev/null
    )
    cat > "${vendor_tmp}/.cargo/config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF
    tar -czf "${BUILD_DIR}/SOURCES/${vendor_tarball_name}" -C "$vendor_tmp" vendor .cargo
    rm -rf "$vendor_tmp"

    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "skillfs RPM built successfully"
}

# =============================================================================
# ktuner
# =============================================================================
build_ktuner() {
    log "=========================================="
    log "Building RPM: ktuner"
    log "=========================================="

    local spec_in="${KTUNER_DIR}/ktuner.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version = ' "${KTUNER_DIR}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        err "Cannot determine ktuner version. Set VERSION env or ensure Cargo.toml/spec exists."
        return 1
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local tarball_name="${pkg_name}-${version}.tar.gz"
    local vendor_tarball_name="${pkg_name}-${version}-vendor.tar.gz"
    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    log "Step 1/3: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    tar -cf - -C "$KTUNER_DIR" \
        --exclude='target' \
        --exclude='vendor' \
        --exclude='.cargo' \
        . | tar -xf - -C "$pkg_dir"

    if [ -L "${pkg_dir}/LICENSE" ] && [ -f "${ROOT_DIR}/LICENSE" ]; then
        rm -f "${pkg_dir}/LICENSE"
        cp -p "${ROOT_DIR}/LICENSE" "${pkg_dir}/LICENSE"
    fi

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 2/3: Creating vendor tarball ${vendor_tarball_name}..."
    local vendor_tmp
    vendor_tmp=$(mktemp -d)
    mkdir -p "${vendor_tmp}/.cargo"
    (
        cd "$KTUNER_DIR"
        cargo vendor --locked "${vendor_tmp}/vendor" >/dev/null
    )
    cat > "${vendor_tmp}/.cargo/config.toml" <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF
    tar -czf "${BUILD_DIR}/SOURCES/${vendor_tarball_name}" -C "$vendor_tmp" vendor .cargo
    rm -rf "$vendor_tmp"

    log "Step 3/3: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "ktuner RPM built successfully"
}

# =============================================================================
# cosh-ng
# =============================================================================
build_cosh_ng() {
    log "=========================================="
    log "Building RPM: cosh-ng"
    log "=========================================="

    local spec_in="${COSH_DIR}/cosh-ng.spec.in"
    if [ ! -f "$spec_in" ]; then
        err "Spec template not found: $spec_in"
        return 1
    fi

    # Version from env, Cargo.toml workspace, then spec fallback
    local version="${VERSION:-}"
    if [ -z "$version" ]; then
        version=$(grep -m1 '^version' "${COSH_DIR}/Cargo.toml" | sed 's/version = "\(.*\)"/\1/' 2>/dev/null || true)
    fi
    if [ -z "$version" ]; then
        version=$(grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+' "$spec_in" | head -1)
    fi
    if [ -z "$version" ]; then
        err "Could not derive cosh-ng version from VERSION env, Cargo.toml, or ${spec_in}"
        exit 1
    fi

    local pkg_name
    pkg_name=$(parse_spec_name "$spec_in")
    local component_name
    component_name=$(awk '
        $0 == "[component]" { in_component = 1; next }
        in_component && /^\[/ { exit }
        in_component && /^name = / {
            value = $0
            sub(/^name = "/, "", value)
            sub(/"$/, "", value)
            print value
            exit
        }
    ' "${COSH_DIR}/component.toml")
    if [ "$component_name" != "$pkg_name" ]; then
        err "cosh-ng identity mismatch: RPM name '${pkg_name}', component name '${component_name}'"
        return 1
    fi
    if ! grep -Fqx "Provides:       anolisa-component(${component_name})" "$spec_in"; then
        err "cosh-ng spec must provide anolisa-component(${component_name})"
        return 1
    fi
    local manifest_path="%{_datadir}/anolisa/components/${component_name}/component.toml"
    if ! grep -Fq "install -m 0644 component.toml %{buildroot}${manifest_path}" "$spec_in" \
        || ! grep -Fqx "$manifest_path" "$spec_in"; then
        err "cosh-ng spec must install and own ${manifest_path}"
        return 1
    fi
    local gateway_unit="%{_unitdir}/cosh-gateway@.service"
    if ! grep -Fq "packaging/systemd/cosh-gateway@.service.in" "$spec_in" \
        || ! grep -Fq "%{buildroot}${gateway_unit}" "$spec_in" \
        || ! grep -Fqx "$gateway_unit" "$spec_in"; then
        err "cosh-ng spec must render, install, and own ${gateway_unit}"
        return 1
    fi
    if grep -Fq "packaging/systemd/cosh-gateway-acp@.service.in" "$spec_in" \
        || grep -Fqx "%{_unitdir}/cosh-gateway-acp@.service" "$spec_in"; then
        err "cosh-ng spec must not ship the retired split ACP Gateway unit"
        return 1
    fi
    local tarball_name="${pkg_name}-${version}.tar.gz"

    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    # Single Source0 tarball: spec %build is plain `cargo build --workspace --release`
    # (online), %prep is `%setup -q` which expects top-level dir `%{name}-%{version}`.
    log "Step 1/2: Creating source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"

    # cosh-ng ships component.toml at repo root — must be in the tarball because
    # spec %install does `install -m 0644 component.toml ...`.
    tar -cf - -C "$COSH_DIR" \
        --exclude='target' \
        --exclude='.git' \
        --exclude='node_modules' \
        . | tar -xf - -C "$pkg_dir"

    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 2/2: Running rpmbuild..."
    "$RPMBUILD" -ba --nodeps \
        --define "_topdir ${BUILD_DIR}" \
        "$spec_file"

    ok "cosh-ng RPM built successfully"
}

# =============================================================================
# sandbox: shared helpers
# =============================================================================

# Map architecture to gVisor release directory naming
gvisor_arch() {
    case "$HOST_ARCH" in
        x86_64|amd64)  echo "x86_64" ;;
        aarch64|arm64) echo "aarch64" ;;
        *) err "Unsupported arch: $HOST_ARCH"; return 1 ;;
    esac
}

# Run rpmbuild with optional --define dist override (alinux4/alinux3 etc.)
rpmbuild_with_dist() {
    local spec_file="$1"
    local extra_defines=()
    # Pass spec_release_suffix through if set (used by cmdoutput-fix.patch path).
    if [ -n "${SPEC_RELEASE_SUFFIX:-}" ]; then
        extra_defines+=( --define "spec_release_suffix ${SPEC_RELEASE_SUFFIX}" )
    fi
    if [ -n "$DIST_TAG" ]; then
        "$RPMBUILD" -ba --nodeps \
            --define "_topdir ${BUILD_DIR}" \
            --define "dist ${DIST_TAG}" \
            "${extra_defines[@]}" \
            "$spec_file"
    else
        "$RPMBUILD" -ba --nodeps \
            --define "_topdir ${BUILD_DIR}" \
            "${extra_defines[@]}" \
            "$spec_file"
    fi
}

# Download upstream gvisor binary + sha512 with retry. Idempotent.
# Args: <name> <out_dir>   where <name> ∈ runsc | containerd-shim-runsc-v1
fetch_gvisor_binary() {
    local bin_name="$1"
    local out_dir="$2"
    local arch
    arch="$(gvisor_arch)" || return 1

    install -d -m 0755 "$out_dir"

    # Local-binary override hook (cmdoutput-fix.patch RPM build path).
    # When SHIM_LOCAL_BINARY is set and we are fetching the shim, copy the
    # locally-built patched ELF instead of pulling from upstream. This is the
    # only injection point needed because the spec is a rebrand-only wrapper:
    # %install just `install -p -m 0755 <bin> /usr/bin/<bin>`.
    if [ "$bin_name" = "containerd-shim-runsc-v1" ] && [ -n "${SHIM_LOCAL_BINARY:-}" ]; then
        if [ ! -f "$SHIM_LOCAL_BINARY" ]; then
            err "SHIM_LOCAL_BINARY=$SHIM_LOCAL_BINARY does not exist"; return 1
        fi
        if ! file "$SHIM_LOCAL_BINARY" 2>/dev/null | grep -q 'ELF.*executable'; then
            warn "SHIM_LOCAL_BINARY=$SHIM_LOCAL_BINARY is not an ELF executable"
        fi
        log "Using local patched binary: $SHIM_LOCAL_BINARY (overrides upstream fetch)"
        install -p -m 0755 "$SHIM_LOCAL_BINARY" "${out_dir}/${bin_name}"
        ( cd "$out_dir" && sha512sum "$bin_name" > "${bin_name}.sha512" )
        ok "local binary staged: ${bin_name} ($(du -h "${out_dir}/${bin_name}" | cut -f1))"
        return 0
    fi

    local url="${GVISOR_BASE_URL}/${GVISOR_RELEASE}/${arch}/${bin_name}"
    local sha_url="${url}.sha512"

    log "Fetching ${bin_name} from ${url}"
    curl -fL --retry 3 --retry-delay 2 -o "${out_dir}/${bin_name}" "$url" || {
        err "Failed to download ${bin_name} from upstream"; return 1; }
    curl -fL --retry 3 --retry-delay 2 -o "${out_dir}/${bin_name}.sha512" "$sha_url" || {
        warn "sha512 sidecar not available for ${bin_name}; skipping integrity check"; }

    if [ -f "${out_dir}/${bin_name}.sha512" ]; then
        ( cd "$out_dir" && sha512sum -c "${bin_name}.sha512" ) \
            || { err "sha512 mismatch for ${bin_name}"; return 1; }
        ok "sha512 verified: ${bin_name}"
    fi
    chmod 0755 "${out_dir}/${bin_name}"
}

# Generic packager for sandbox specs that wrap an upstream binary.
# Args: <pkg_name> <bin_name>
_build_sandbox_upstream() {
    local pkg_name="$1"
    local bin_name="$2"
    local spec_in="${SANDBOX_PKG_DIR}/${pkg_name}.spec.in"
    [ -f "$spec_in" ] || { err "Spec template not found: $spec_in"; return 1; }

    local version="${VERSION:-$GVISOR_RELEASE_VERSION}"
    local tarball_name="${pkg_name}-${version}.tar.gz"

    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    log "Step 1/3: Fetching upstream binary (${bin_name})..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"
    fetch_gvisor_binary "$bin_name" "$pkg_dir" || { rm -rf "$tmp_dir"; return 1; }

    # LICENSE / README placeholders (so spec %doc works under --strict)
    cat > "${pkg_dir}/LICENSE" <<'EOF'
Copyright 2018 The gVisor Authors. Licensed under Apache-2.0.
Full text: https://www.apache.org/licenses/LICENSE-2.0.txt
EOF
    cat > "${pkg_dir}/README.md" <<EOF
# ${pkg_name} ${version}

Repackaged upstream gVisor binary — see
https://gvisor.dev/docs/architecture_guide/ and
ANOLISA-design/docs/anolisa/osbase/sandbox/sandbox-rpm-packaging.md
EOF

    log "Step 2/3: Creating source tarball ${tarball_name}..."
    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 3/3: Running rpmbuild..."
    rpmbuild_with_dist "$spec_file"
    ok "${pkg_name} RPM built successfully"
}

# Generic packager for sandbox specs whose source is just a placeholder shim
# (atelet, ateom-gvisor). Spec generates the binary at %install time.
# Args: <pkg_name> <default_version>
_build_sandbox_placeholder() {
    local pkg_name="$1"
    local default_version="$2"
    local spec_in="${SANDBOX_PKG_DIR}/${pkg_name}.spec.in"
    [ -f "$spec_in" ] || { err "Spec template not found: $spec_in"; return 1; }

    local version="${VERSION:-$default_version}"
    local tarball_name="${pkg_name}-${version}.tar.gz"

    local spec_file
    spec_file=$(process_spec_template "$spec_in" "$version")

    log "Step 1/2: Creating placeholder source tarball ${tarball_name}..."
    local tmp_dir
    tmp_dir=$(mktemp -d)
    local pkg_dir="${tmp_dir}/${pkg_name}-${version}"
    mkdir -p "$pkg_dir"
    cat > "${pkg_dir}/LICENSE" <<'EOF'
Copyright 2026 The ANOLISA Authors. Licensed under Apache-2.0.
Full text: https://www.apache.org/licenses/LICENSE-2.0.txt
EOF
    cat > "${pkg_dir}/README.md" <<EOF
# ${pkg_name} ${version}

Placeholder ANOLISA Substrate component. Real implementation pending —
see ANOLISA-design/docs/anolisa/osbase/sandbox/sandbox-rpm-packaging.md
EOF
    tar -czf "${BUILD_DIR}/SOURCES/${tarball_name}" -C "$tmp_dir" "${pkg_name}-${version}"
    rm -rf "$tmp_dir"

    log "Step 2/2: Running rpmbuild..."
    rpmbuild_with_dist "$spec_file"
    ok "${pkg_name} RPM built successfully"
}

build_gvisor_runsc() {
    log "=========================================="
    log "Building RPM: gvisor-runsc (release-${GVISOR_RELEASE_VERSION}, arch=$(gvisor_arch))"
    log "=========================================="
    _build_sandbox_upstream "gvisor-runsc" "runsc"
}

build_containerd_shim_runsc_v1() {
    log "=========================================="
    if [ -n "${SHIM_LOCAL_BINARY:-}" ]; then
        : "${SPEC_RELEASE_SUFFIX:=.cmdoutput.fix1}"
        export SPEC_RELEASE_SUFFIX
        log "Building RPM: containerd-shim-runsc-v1 (release-${GVISOR_RELEASE_VERSION}${SPEC_RELEASE_SUFFIX}) [PATCHED via SHIM_LOCAL_BINARY]"
    else
        log "Building RPM: containerd-shim-runsc-v1 (release-${GVISOR_RELEASE_VERSION})"
    fi
    log "=========================================="
    _build_sandbox_upstream "containerd-shim-runsc-v1" "containerd-shim-runsc-v1"
}

build_atelet() {
    log "=========================================="
    log "Building RPM: atelet (placeholder)"
    log "=========================================="
    _build_sandbox_placeholder "atelet" "0.1.0"
}

build_ateom_gvisor() {
    log "=========================================="
    log "Building RPM: ateom-gvisor (placeholder)"
    log "=========================================="
    _build_sandbox_placeholder "ateom-gvisor" "0.1.0"
}

build_sandbox_all() {
    build_gvisor_runsc
    build_containerd_shim_runsc_v1
    build_atelet
    build_ateom_gvisor
    # Repo metadata is the final stitch that makes the 4 RPMs consumable via
    # `dnf install -y gvisor-runsc ...`. We attempt to generate it, but DO NOT
    # fail the build if createrepo_c is missing — operators can run
    # `bash scripts/rpm-build.sh sandbox-repo` later (or the SRE pipeline will
    # do it on the internal yum host). See sandbox-rpm-packaging.md §8.5.
    if command -v createrepo_c >/dev/null 2>&1 || command -v createrepo >/dev/null 2>&1; then
        build_sandbox_repo
    else
        log "createrepo_c not installed; skipping repodata generation."
        log "Install with: dnf install -y createrepo_c"
        log "Then run: bash scripts/rpm-build.sh sandbox-repo"
    fi
}

# Generate yum/dnf repo metadata for the sandbox RPM set.
# Writes repodata/ in BOTH locations:
#   1. ${BUILD_DIR}/RPMS/<arch>/   (the local rpmbuild tree)
#   2. The dist staging tree if SANDBOX_DIST_DIR is set, e.g.
#      SANDBOX_DIST_DIR=$REPO/dist/sandbox/alinux4 with subdirs RPMS/ SRPMS/
build_sandbox_repo() {
    log "=========================================="
    log "Generating sandbox yum/dnf repo metadata"
    log "=========================================="
    local createrepo
    if command -v createrepo_c >/dev/null 2>&1; then
        createrepo="createrepo_c"
    elif command -v createrepo >/dev/null 2>&1; then
        createrepo="createrepo"
    else
        err "createrepo_c (or createrepo) is required. Install: dnf install -y createrepo_c"
        return 1
    fi

    local arch_dir="${BUILD_DIR}/RPMS/$(gvisor_arch)"
    if [ -d "$arch_dir" ]; then
        log "createrepo on ${arch_dir}"
        "$createrepo" --update "$arch_dir"
    fi

    if [ -n "${SANDBOX_DIST_DIR:-}" ] && [ -d "$SANDBOX_DIST_DIR" ]; then
        for sub in RPMS SRPMS; do
            if [ -d "${SANDBOX_DIST_DIR}/${sub}" ]; then
                log "createrepo on ${SANDBOX_DIST_DIR}/${sub}"
                "$createrepo" --update "${SANDBOX_DIST_DIR}/${sub}"
            fi
        done
    fi

    log "Repo metadata ready. Sample dnf repo file:"
    cat <<'REPO_EOF'
[anolisa-sandbox]
name=ANOLISA Sandbox RPMs (gvisor-runsc, containerd-shim-runsc-v1, atelet, ateom-gvisor)
baseurl=file://<absolute-path-to-RPMS-dir>
enabled=1
gpgcheck=0
REPO_EOF
}

# =============================================================================
# Main
# =============================================================================
usage() {
    echo "Usage: $0 <package|all>"
    echo ""
    echo "Packages:"
    echo "  copilot-shell             Build copilot-shell RPM"
    echo "  agent-sec-core            Build agent-sec-core RPM"
    echo "  agent-sec-core-v2         Build agent-sec-core RPM with the V2 Rust CLI/daemon"
    echo "  os-skills                 Build os-skills RPM"
    echo "  agentsight                Build agentsight RPM"
    echo "  tokenless                 Build tokenless RPM"
    echo "  agent-memory              Build agent-memory RPM"
    echo "  skillfs                   Build skillfs RPM"
    echo "  ktuner                    Build ktuner RPM"
    echo "  anolisa                   Build anolisa RPM"
    echo "  cosh-ng                   Build cosh-ng RPM"
    echo "  gvisor-runsc              Build gvisor-runsc RPM (sandbox)"
    echo "  containerd-shim-runsc-v1  Build containerd-shim-runsc-v1 RPM (sandbox)"
    echo "  atelet                    Build atelet RPM (sandbox, placeholder)"
    echo "  ateom-gvisor              Build ateom-gvisor RPM (sandbox, placeholder)"
    echo "  sandbox-all               Build all 4 sandbox RPMs"
    echo "  sandbox-repo              Generate yum/dnf repo metadata (createrepo_c) for sandbox RPMs"
    echo "  all                       Build all primary RPM packages"
    echo ""
    echo "Environment variables:"
    echo "  VERSION                   Override version for .spec.in templates"
    echo "  RPMBUILD                  Path to rpmbuild binary (default: rpmbuild)"
    echo "  DIST_TAG                  Override %{dist} (e.g. .alinux4, .alinux3)"
    echo "  GVISOR_RELEASE            gVisor upstream release date (default: 20260601)"
    echo "  GVISOR_BASE_URL           gVisor mirror base URL (overridable for offline)"
    echo "  TARGET_ARCH               Force arch (x86_64|aarch64; default: uname -m)"
    echo ""
    echo "Output: scripts/rpmbuild/RPMS/"
}

if [ $# -lt 1 ]; then
    usage
    exit 1
fi

TARGET="$1"

# Pre-flight: check rpmbuild is available
if ! command -v "$RPMBUILD" &>/dev/null; then
    err "rpmbuild not found. Install with: yum install rpm-build (or brew install rpm on macOS)"
    exit 1
fi

setup_rpmbuild

case "$TARGET" in
    copilot-shell)
        build_copilot_shell
        ;;
    agent-sec-core)
        build_agent_sec_core
        ;;
    agent-sec-core-v2)
        build_agent_sec_core_v2
        ;;
    os-skills)
        build_agentic_os_skills
        ;;
    agentsight)
        build_agentsight
        ;;
    tokenless)
        build_tokenless
        ;;
    agent-memory)
        build_agent_memory
        ;;
    skillfs)
        build_skillfs
        ;;
    ktuner)
        build_ktuner
        ;;
    anolisa)
        build_anolisa
        ;;
    cosh-ng)
        build_cosh_ng
        ;;
    gvisor-runsc)
        build_gvisor_runsc
        ;;
    containerd-shim-runsc-v1)
        build_containerd_shim_runsc_v1
        ;;
    atelet)
        build_atelet
        ;;
    ateom-gvisor)
        build_ateom_gvisor
        ;;
    sandbox-all)
        build_sandbox_all
        ;;
    sandbox-repo)
        build_sandbox_repo
        ;;
    all)
        build_copilot_shell
        build_agent_sec_core
        build_agentic_os_skills
        build_agentsight
        build_tokenless
        build_agent_memory
        build_skillfs
        build_ktuner
        build_anolisa
        build_cosh_ng
        ;;
    *)
        err "Unknown package: $TARGET"
        usage
        exit 1
        ;;
esac

# Print results
echo ""
log "=========================================="
log "RPM build output:"
log "=========================================="
find "${BUILD_DIR}/RPMS" "${BUILD_DIR}/SRPMS" -name "*.rpm" -type f 2>/dev/null | while read -r rpm; do
    echo "  $(basename "$rpm")  ($(du -h "$rpm" | cut -f1))"
done
echo ""
log "Output directory: ${BUILD_DIR}/RPMS/"
