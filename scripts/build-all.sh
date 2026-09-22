#!/usr/bin/env bash
# ──────────────────────────────────────────────────────────────────
# build-all.sh  –  ANOLISA unified build script
#
# Usage:
#   ./scripts/build-all.sh                                    # install deps + build + install (default)
#   ./scripts/build-all.sh --no-install                       # install deps + build, skip installation
#   ./scripts/build-all.sh --ignore-deps                      # build + install, skip dependency setup and verification
#   ./scripts/build-all.sh --deps-only                        # install deps only
#   ./scripts/build-all.sh --component cosh                   # deps + build + install copilot-shell only
#   ./scripts/build-all.sh --uninstall                        # uninstall all components
#   ./scripts/build-all.sh --uninstall --component cosh       # uninstall copilot-shell only
#   ./scripts/build-all.sh --help
#
# Components (build order):
#   cosh     copilot-shell      (Node.js / TypeScript)
#   skills   os-skills          (Markdown skill definitions, no compilation)
#   sec-core agent-sec-core     (Security CLI + sandbox + hooks)
#   tokenless tokenless         (Rust compression library, cross-platform)
#   ws-ckpt  ws-ckpt           (Rust workspace checkpoint daemon)
#   memory   agent-memory       (Rust MCP filesystem memory server, Linux only)
#   cosh-ng  cosh-ng            (Rust Agent-OS CLI, core, and interactive shell)
#   sight    agentsight         (eBPF / Rust, Linux only, NOT built by default)
# ──────────────────────────────────────────────────────────────────
set -euo pipefail

# ─── colors ───

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

# ─── paths ───

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# ─── defaults ───

INSTALL_DEPS=true
DEPS_ONLY=false
DO_INSTALL=true
DO_UNINSTALL=false
DRY_RUN=false
INTERACTIVE=false
NON_INTERACTIVE=false
INSTALL_MODE="user"
COMPONENTS=()

SYSTEM_PREFIX="/usr"
SYSTEM_BIN_DIR="/usr/local/bin"
NPM_REGISTRY="${NPM_REGISTRY:-https://registry.npmmirror.com}"
INSTALL_PREFIX="$HOME/.local"
INSTALL_BIN_DIR="$INSTALL_PREFIX/bin"
USER_BIN_DIR="$INSTALL_PREFIX/bin"
USER_LIB_DIR="$INSTALL_PREFIX/lib"
USER_LIBEXEC_DIR="$INSTALL_PREFIX/libexec"
USER_SHARE_DIR="$INSTALL_PREFIX/share"
USER_DOC_DIR="$INSTALL_PREFIX/share/doc"

USER_COSH_DIR="$HOME/.copilot-shell"
USER_COSH_EXTENSIONS_DIR="$USER_COSH_DIR/extensions"
USER_COSH_SKILLS_DIR="$USER_COSH_DIR/skills"
INSTALL_EXTENSIONS_DIR="$USER_COSH_EXTENSIONS_DIR"

# sec-core install paths are loaded from src/agent-sec-core/Makefile after
# INSTALL_PROFILE is resolved, so build-all does not duplicate its defaults.
SEC_CORE_BIN_DIR=""
SEC_CORE_LIB_DIR=""
SEC_CORE_RUST_TOOLCHAIN="1.93.0"
RUNTIME_SYSTEM_PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

# ─── output / staging ───

OUTPUT_DIR="$PROJECT_ROOT/target"
LOG_FILE="$OUTPUT_DIR/build.log"

if [[ ! -t 1 || -n "${NO_COLOR:-}" ]]; then
    RED=''
    GREEN=''
    YELLOW=''
    BLUE=''
    CYAN=''
    BOLD=''
    DIM=''
    NC=''
fi

# ─── helpers ───

info()  { echo -e "${BLUE}[info]${NC}  $*"; }
ok()    { echo -e "${GREEN}[ok]${NC}    $*"; }
warn()  { echo -e "${YELLOW}[warn]${NC}  $*"; }
err()   { echo -e "${RED}[error]${NC} $*"; }
step()  { echo -e "\n${CYAN}${BOLD}==> $*${NC}"; }

cmd_exists() { command -v "$1" &>/dev/null; }

perl_module_exists() {
    local module="$1"
    cmd_exists perl && perl -M"$module" -e1 &>/dev/null
}

shell_args() {
    printf '%q ' "$@"
}

# Extract first semver (X.Y.Z) from a string.
# Examples: "rustc 1.91.0 (abc 2024)" -> "1.91.0", "v22.21.1" -> "22.21.1"
extract_ver() {
    echo "$1" | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1
}

# ver_gte "1.91.0" "1.80.0" -> true (actual >= required)
ver_gte() {
    printf '%s\n%s' "$2" "$1" | sort -V -C
}

die() { err "$@"; exit 1; }

as_root() {
    if [[ "$(id -u)" -eq 0 ]]; then
        "$@"
    else
        sudo "$@"
    fi
}

run_cmd() {
    if $DRY_RUN; then
        echo "DRY-RUN: $*"
    else
        "$@"
    fi
}

component_target_dir() {
    echo "$OUTPUT_DIR/$1"
}

component_install_root() {
    echo "$(component_target_dir "$1")/install-root"
}

stage_component_make_install() {
    local component="$1" dir="$2"; shift 2
    local stage_root
    stage_root="$(component_target_dir "$component")"

    [[ -d "$dir" ]] || die "Directory not found: $dir"

    if $DRY_RUN; then
        echo "DRY-RUN: rm -rf $stage_root"
        echo "DRY-RUN: mkdir -p $stage_root"
        echo "DRY-RUN: (cd $dir && make install DESTDIR=$stage_root INSTALL_PROFILE=system PREFIX= BINDIR=/bin $*)"
        return 0
    fi

    rm -rf "$stage_root"
    mkdir -p "$stage_root"

    cd "$dir"
    run_logged "stage ${component} -> target/${component}" \
        make install DESTDIR="$stage_root" INSTALL_PROFILE=system \
            PREFIX="" BINDIR="/bin" "$@"
}

system_staged_install() {
    local component="$1" stage_root="$2"
    [[ -d "$stage_root" ]] || die "Staged install root not found: $stage_root"

    if $DRY_RUN; then
        echo "DRY-RUN: cp -a $stage_root/. /"
    else
        info "Installing ${component} from ${stage_root} to / ..."
        as_root cp -a "$stage_root/." /
    fi
}

run_component_make_install() {
    local component="$1" dir="$2"; shift 2
    [[ -d "$dir" ]] || die "Directory not found: $dir"

    if $DRY_RUN; then
        if [[ "$INSTALL_MODE" == "system" ]]; then
            local stage_root
            stage_root="$(component_install_root "$component")"
            echo "DRY-RUN: rm -rf $stage_root"
            echo "DRY-RUN: mkdir -p $stage_root"
            echo "DRY-RUN: (cd $dir && make install DESTDIR=$stage_root INSTALL_PROFILE=system PREFIX=$SYSTEM_PREFIX BINDIR=$SYSTEM_BIN_DIR SERVICE_BINDIR=$SYSTEM_BIN_DIR $*)"
            echo "DRY-RUN: cp -a $stage_root/. /"
        else
            echo "DRY-RUN: (cd $dir && make install INSTALL_PROFILE=user PREFIX=$INSTALL_PREFIX $*)"
        fi
        return 0
    fi

    cd "$dir"

    if [[ "$INSTALL_MODE" == "system" ]]; then
        local stage_root
        stage_root="$(component_install_root "$component")"
        rm -rf "$stage_root"
        mkdir -p "$stage_root"
        run_logged "stage system install ${component} -> target/${component}/install-root" \
            make install DESTDIR="$stage_root" INSTALL_PROFILE=system \
                PREFIX="$SYSTEM_PREFIX" BINDIR="$SYSTEM_BIN_DIR" \
                SERVICE_BINDIR="$SYSTEM_BIN_DIR" "$@"
        system_staged_install "$component" "$stage_root"
    else
        run_logged "make install (${component})" \
            make install INSTALL_PROFILE=user PREFIX="$INSTALL_PREFIX" "$@"
    fi
}

run_component_make_uninstall() {
    local component="$1" dir="$2"; shift 2
    [[ -d "$dir" ]] || die "Directory not found: $dir"

    if $DRY_RUN; then
        if [[ "$INSTALL_MODE" == "system" ]]; then
            echo "DRY-RUN: (cd $dir && sudo make uninstall INSTALL_PROFILE=system PREFIX=$SYSTEM_PREFIX BINDIR=$SYSTEM_BIN_DIR SERVICE_BINDIR=$SYSTEM_BIN_DIR $*)"
        else
            echo "DRY-RUN: (cd $dir && make uninstall INSTALL_PROFILE=user PREFIX=$INSTALL_PREFIX $*)"
        fi
        return 0
    fi

    cd "$dir"

    if [[ "$INSTALL_MODE" == "system" ]]; then
        run_logged "make uninstall (${component})" \
            as_root make uninstall INSTALL_PROFILE=system \
                PREFIX="$SYSTEM_PREFIX" BINDIR="$SYSTEM_BIN_DIR" \
                SERVICE_BINDIR="$SYSTEM_BIN_DIR" "$@"
    else
        run_logged "make uninstall (${component})" \
            make uninstall INSTALL_PROFILE=user PREFIX="$INSTALL_PREFIX" "$@"
    fi
}

sec_core_cmd() {
    if [[ "$INSTALL_MODE" == "system" ]]; then
        as_root "$@"
    else
        "$@"
    fi
}

copy_tree() {
    local src="$1" dst="$2"
    [[ -d "$src" ]] || die "Directory not found: $src"
    if $DRY_RUN; then
        echo "DRY-RUN: copy tree $src -> $dst"
        return 0
    fi
    mkdir -p "$dst"
    cp -rp "$src/." "$dst/"
}

copy_file() {
    local src="$1" dst="$2" mode="${3:-0644}"
    [[ -f "$src" ]] || die "File not found: $src"
    if $DRY_RUN; then
        echo "DRY-RUN: install -p -m $mode $src $dst"
        return 0
    fi
    mkdir -p "$(dirname "$dst")"
    install -p -m "$mode" "$src" "$dst"
}

stage_skill_dirs() {
    local src_root="$1" dst_root="$2" skill_dir skill_name
    [[ -d "$src_root" ]] || die "Directory not found: $src_root"
    if $DRY_RUN; then
        echo "DRY-RUN: stage flattened skills from $src_root -> $dst_root"
        return 0
    fi
    mkdir -p "$dst_root"
    while IFS= read -r skill_file; do
        skill_dir="$(dirname "$skill_file")"
        skill_name="$(basename "$skill_dir")"
        mkdir -p "$dst_root/$skill_name"
        cp -rp "$skill_dir/." "$dst_root/$skill_name/"
    done < <(find "$src_root" -name "SKILL.md" -type f | sort)
}

install_skill_dirs_flat() {
    local src_root="$1" dst_root="$2" skill_dir skill_name
    [[ -d "$src_root" ]] || die "Directory not found: $src_root"
    if $DRY_RUN; then
        echo "DRY-RUN: install flattened skills from $src_root -> $dst_root"
        return 0
    fi
    sec_core_cmd install -d -m 0755 "$dst_root"
    while IFS= read -r skill_file; do
        skill_dir="$(dirname "$skill_file")"
        skill_name="$(basename "$skill_dir")"
        sec_core_cmd rm -rf "$dst_root/$skill_name"
        sec_core_cmd install -d -m 0755 "$dst_root/$skill_name"
        sec_core_cmd cp -rp "$skill_dir/." "$dst_root/$skill_name/"
    done < <(find "$src_root" -name "SKILL.md" -type f | sort)
}

remove_skill_dirs_flat() {
    local src_root="$1" dst_root="$2" skill_dir skill_name
    [[ -d "$src_root" ]] || return 0
    if $DRY_RUN; then
        echo "DRY-RUN: remove flattened skills from $dst_root using $src_root"
        return 0
    fi
    while IFS= read -r skill_file; do
        skill_dir="$(dirname "$skill_file")"
        skill_name="$(basename "$skill_dir")"
        sec_core_cmd rm -rf "$dst_root/$skill_name"
    done < <(find "$src_root" -name "SKILL.md" -type f | sort)
}

# Run a command, redirect all output (stdout+stderr) to LOG_FILE.
# Shows an animated spinner on the same line while the command runs,
# then replaces it with ok / FAILED.
run_logged() {
    local desc="$1"; shift

    if $DRY_RUN; then
        echo "DRY-RUN: $desc: $*"
        return 0
    fi

    mkdir -p "$(dirname "$LOG_FILE")"
    "$@" >> "$LOG_FILE" 2>&1 &
    local pid=$!

    local spin='-\|/' i=0
    while kill -0 "$pid" 2>/dev/null; do
        printf "\r    ${DIM}%-52s${NC}  ${CYAN}%s${NC}" "$desc" "${spin:$((i % 4)):1}"
        i=$((i + 1))
        sleep 0.1
    done

    local rc=0
    wait "$pid" || rc=$?
    if [[ $rc -eq 0 ]]; then
        printf "\r    ${DIM}%-52s${NC}  ${GREEN}ok${NC}\n" "$desc"
    else
        printf "\r    ${DIM}%-52s${NC}  ${RED}FAILED${NC}\n" "$desc"
        warn "Failed: $*"
        info "Full output: $LOG_FILE"
        return $rc
    fi
}

run_logged_timeout() {
    local seconds="$1"; shift
    local desc="$1"; shift

    if cmd_exists timeout; then
        run_logged "$desc" timeout "$seconds" "$@"
    else
        run_logged "$desc" "$@"
    fi
}

makefile_var() {
    local dir="$1" profile="$2" var="$3"
    make -s -C "$dir" INSTALL_PROFILE="$profile" VAR="$var" -f - print-var <<'MAKE_EOF'
include Makefile
print-var:
	@printf '%s\n' "$($(VAR))"
MAKE_EOF
}

load_sec_core_make_paths() {
    local dir="$PROJECT_ROOT/src/agent-sec-core"
    [[ -f "$dir/Makefile" ]] || return 0

    SEC_CORE_BIN_DIR="$(makefile_var "$dir" "$INSTALL_MODE" BINDIR)" || \
        die "Failed to read BINDIR from sec-core Makefile"
    SEC_CORE_LIB_DIR="$(makefile_var "$dir" "$INSTALL_MODE" LIBDIR)" || \
        die "Failed to read LIBDIR from sec-core Makefile"
}

ensure_user_mode() {
    case "$INSTALL_MODE" in
        user)
            INSTALL_PREFIX="$HOME/.local"
            INSTALL_BIN_DIR="$INSTALL_PREFIX/bin"
            ;;
        system)
            INSTALL_PREFIX="$SYSTEM_PREFIX"
            INSTALL_BIN_DIR="$SYSTEM_BIN_DIR"
            ;;
        *)
            die "Invalid install mode: $INSTALL_MODE"
            ;;
    esac

    USER_BIN_DIR="$INSTALL_PREFIX/bin"
    USER_LIB_DIR="$INSTALL_PREFIX/lib"
    USER_LIBEXEC_DIR="$INSTALL_PREFIX/libexec"
    USER_SHARE_DIR="$INSTALL_PREFIX/share"
    USER_DOC_DIR="$INSTALL_PREFIX/share/doc"

    USER_COSH_DIR="$HOME/.copilot-shell"
    USER_COSH_EXTENSIONS_DIR="$USER_COSH_DIR/extensions"
    USER_COSH_SKILLS_DIR="$USER_COSH_DIR/skills"
    INSTALL_EXTENSIONS_DIR="$USER_COSH_EXTENSIONS_DIR"
    [[ "$INSTALL_MODE" == "system" ]] && INSTALL_EXTENSIONS_DIR="/usr/share/anolisa/extensions"

    load_sec_core_make_paths
}

system_service_dir() {
    if [[ -d /usr/lib/systemd/system || "$INSTALL_MODE" == "system" ]]; then
        echo "/usr/lib/systemd/system"
    else
        echo "/etc/systemd/system"
    fi
}

systemd_is_available() {
    cmd_exists systemctl && [[ -d /run/systemd/system ]]
}

refresh_systemd_service() {
    local service="$1"

    [[ "$INSTALL_MODE" == "system" ]] || return 0
    if $DRY_RUN; then
        echo "DRY-RUN: systemctl daemon-reload"
        echo "DRY-RUN: systemctl enable $service"
        echo "DRY-RUN: systemctl restart $service"
        return 0
    fi

    if ! systemd_is_available; then
        warn "systemd is not active; installed ${service} but skipped enable/restart"
        return 0
    fi

    as_root systemctl daemon-reload || warn "systemctl daemon-reload failed"
    as_root systemctl enable "$service" || warn "systemctl enable $service failed"
    as_root systemctl restart "$service" || warn "systemctl restart $service failed"
}

stop_systemd_service() {
    local service="$1"

    [[ "$INSTALL_MODE" == "system" ]] || return 0
    if $DRY_RUN; then
        echo "DRY-RUN: systemctl stop $service"
        echo "DRY-RUN: systemctl disable $service"
        echo "DRY-RUN: systemctl daemon-reload"
        return 0
    fi

    if ! systemd_is_available; then
        return 0
    fi

    as_root systemctl stop "$service" 2>/dev/null || true
    as_root systemctl disable "$service" 2>/dev/null || true
    as_root systemctl daemon-reload || warn "systemctl daemon-reload failed"
}

stop_systemd_service_for_install() {
    local service="$1"

    [[ "$INSTALL_MODE" == "system" ]] || return 0
    if $DRY_RUN; then
        echo "DRY-RUN: systemctl stop $service"
        return 0
    fi

    if ! systemd_is_available; then
        return 0
    fi

    as_root systemctl stop "$service" 2>/dev/null || true
}

# ─── distro detection ───

DISTRO_ID=""        # alinux, ubuntu, fedora, centos, anolis, etc.
DISTRO_VER=""       # 4, 24.04, 9, etc.
DISTRO_VER_MAJOR="" # 4, 24, 9, etc.
PKG_BASE=""         # rpm | deb
PKG_INSTALL=""

detect_distro() {
    [[ -f /etc/os-release ]] || die "Cannot detect distro (no /etc/os-release). Linux only."
    # shellcheck source=/dev/null
    source /etc/os-release
    DISTRO_ID="${ID:-}"
    DISTRO_VER="${VERSION_ID:-}"
    DISTRO_VER_MAJOR="${DISTRO_VER%%.*}"
    local id_like="${ID_LIKE:-}"

    if [[ "$DISTRO_ID" =~ ^(fedora|rhel|centos|anolis|alinux)$ ]] || [[ "$id_like" =~ (fedora|rhel) ]]; then
        PKG_BASE="rpm"
        if cmd_exists dnf; then PKG_INSTALL="dnf install -y"
        elif cmd_exists yum; then PKG_INSTALL="yum install -y"
        else die "Neither dnf nor yum found"; fi
    elif [[ "$DISTRO_ID" =~ ^(debian|ubuntu)$ ]] || [[ "$id_like" =~ debian ]]; then
        PKG_BASE="deb"
        PKG_INSTALL="apt-get install -y"
    else
        die "Unsupported distro: ${PRETTY_NAME:-$DISTRO_ID}. Supported: Fedora/RHEL/CentOS/Anolis/Alinux, Debian/Ubuntu."
    fi

    ok "Distro: ${PRETTY_NAME:-$DISTRO_ID} (${PKG_BASE}, id=${DISTRO_ID}, ver=${DISTRO_VER})"
}

# ─── component helpers ───

# Default components (cosh-ng and sight are excluded because they are optional;
# use --component to include either explicitly).
DEFAULT_COMPONENTS=(cosh skills sec-core tokenless ws-ckpt memory)
ALL_COMPONENTS=(cosh skills sec-core tokenless ws-ckpt memory cosh-ng sight)

active_components() {
    if [[ ${#COMPONENTS[@]} -eq 0 ]]; then
        printf '%s\n' "${DEFAULT_COMPONENTS[@]}"
    else
        printf '%s\n' "${COMPONENTS[@]}"
    fi
}

join_by() {
    local sep="$1"; shift
    local first=true item
    for item in "$@"; do
        if $first; then
            printf '%s' "$item"
            first=false
        else
            printf '%s%s' "$sep" "$item"
        fi
    done
}

selected_components_text() {
    local items=()
    while IFS= read -r item; do
        items+=("$item")
    done < <(active_components)
    join_by ", " "${items[@]}"
}

is_valid_component() {
    local c="$1" v
    for v in "${ALL_COMPONENTS[@]}"; do
        [[ "$v" == "$c" ]] && return 0
    done
    return 1
}

want_component() {
    local c="$1"
    if [[ ${#COMPONENTS[@]} -eq 0 ]]; then
        local d
        for d in "${DEFAULT_COMPONENTS[@]}"; do
            if [[ "$d" == "$c" ]]; then return 0; fi
        done
        return 1
    fi
    local x
    for x in "${COMPONENTS[@]}"; do
        if [[ "$x" == "$c" ]]; then return 0; fi
    done
    return 1
}

# ─── dependency installation ───

# Query the highest version of a package available in the configured system repositories.
# Prints semver string (e.g. "20.18.0") or nothing if the package is not found.
query_repo_ver() {
    local pkg="$1"
    if [[ "$PKG_BASE" == "rpm" ]]; then
        # dnf list output example: "nodejs.x86_64    1:20.18.0-1.alnx4    appstream"
        local raw
        raw=$(dnf list "$pkg" 2>/dev/null | grep -E "^${pkg}\." | tail -1)
        [[ -z "$raw" ]] && raw=$(yum list "$pkg" 2>/dev/null | grep -E "^${pkg}\." | tail -1)
        if [[ -n "$raw" ]]; then
            local nvr
            nvr=$(echo "$raw" | awk '{print $2}')
            nvr="${nvr#*:}"   # strip epoch (e.g. "1:20.18.0-1" → "20.18.0-1")
            extract_ver "$nvr"
            return
        fi
    elif [[ "$PKG_BASE" == "deb" ]]; then
        # apt-cache policy output: "  Candidate: 18.19.0+dfsg-6ubuntu5"
        local candidate
        candidate=$(apt-cache policy "$pkg" 2>/dev/null | sed -n 's/.*Candidate: *//p')
        if [[ -n "$candidate" && "$candidate" != "(none)" ]]; then
            extract_ver "$candidate"
            return
        fi
    fi
}

node_version_satisfies_on_path() {
    local command_path="$1" required="$2" version
    version="$(PATH="$command_path" node -v 2>/dev/null)" || return 1
    version="$(extract_ver "$version" || true)"
    [[ -n "$version" ]] && ver_gte "$version" "$required"
}

install_node() {
    step "Node.js (for copilot-shell, agent-sec-core, agentsight, agent-memory)"
    local REQUIRED="20.0.0"
    local NVM_INSTALL_MAJOR="24"

    local node_pkg="nodejs" npm_pkg="npm"

    _node_ver_ok() {
        local command_path="$PATH"
        [[ "$INSTALL_MODE" == "system" ]] && command_path="$RUNTIME_SYSTEM_PATH"
        node_version_satisfies_on_path "$command_path" "$REQUIRED"
    }

    _source_nvm() {
        export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
        # shellcheck source=/dev/null
        if [[ -s "$NVM_DIR/nvm.sh" ]]; then source "$NVM_DIR/nvm.sh"; fi
    }

    _node_command_path() {
        if [[ "$INSTALL_MODE" == "system" ]]; then
            echo "$RUNTIME_SYSTEM_PATH"
        else
            echo "$PATH"
        fi
    }

    _npm_ok() {
        PATH="$(_node_command_path)" npm -v &>/dev/null
    }

    # Several distributions ship nodejs without npm, while the plugin builds
    # (`build-openclaw-plugin`, copilot-shell, agentsight) call npm directly.
    # A node-only toolchain therefore is not a ready Node.js setup.
    #
    # `nvm install-latest-npm` is *not* a way to recover a missing npm: it
    # starts by running `npm --version` and aborts with "Unable to obtain npm
    # version" when that fails (see nvm_install_latest_npm in nvm.sh), so it
    # needs the very npm that is gone. `nvm install <version>` does not help
    # either: for a version that is already installed it exits 1 with
    # "<version> is already installed." before unpacking anything. An
    # nvm-managed Node keeps npm in <prefix>/lib/node_modules/npm behind the
    # <prefix>/bin/npm and <prefix>/bin/npx symlinks, so restore exactly that
    # from the registry tarball instead. It needs only curl and tar (both
    # already required by this script) plus a writable prefix, never sudo, and
    # leaves the existing Node install untouched.
    #
    # The npm release each Node.js release shipped with is recorded in the
    # dist index, so prefer that: it is by construction inside the engine range
    # of the node binary next to it, whereas the registry `latest` tag lands
    # npm 12 on Node 20, which npm itself reports as unsupported.
    _bundled_npm_version() {
        local mirror="${1%/}" node_version="$2"
        curl -fsSL --connect-timeout 10 --max-time 60 "$mirror/index.json" \
            | tr '{' '\n' | grep -F "\"version\":\"$node_version\"" \
            | head -1 | grep -o '"npm":"[0-9][0-9.]*"' | head -1 \
            | sed 's/.*"\([0-9][0-9.]*\)"/\1/' || true
    }

    _latest_npm_version() {
        local registry="$1" version=""
        version="$(curl -fsSL --connect-timeout 10 --max-time 30 \
                "$registry/-/package/npm/dist-tags" \
            | grep -o '"latest"[[:space:]]*:[[:space:]]*"[0-9][^"]*"' \
            | head -1 | sed 's/.*"\([0-9][^"]*\)"$/\1/' || true)"
        if [[ -z "$version" ]]; then
            version="$(curl -fsSL --connect-timeout 10 --max-time 30 \
                    "$registry/npm/latest" \
                | grep -o '"version"[[:space:]]*:[[:space:]]*"[0-9][^"]*"' \
                | head -1 | sed 's/.*"\([0-9][^"]*\)"$/\1/' || true)"
        fi
        echo "${version:-}"
    }

    _restore_nvm_npm() {
        local node_bin="$1"
        local prefix modules npm_dir registry node_version version archive
        prefix="$(cd "$(dirname "$node_bin")/.." && pwd)" || return 1
        modules="$prefix/lib/node_modules"
        npm_dir="$modules/npm"
        registry="${npm_config_registry:-$NPM_REGISTRY}"
        registry="${registry%/}"
        node_version="${prefix##*/}"

        info "Restoring npm for the nvm-managed Node.js in $prefix ..."
        version=""
        if [[ "$node_version" == v[0-9]* ]]; then
            version="$(_bundled_npm_version \
                "${NVM_NODEJS_ORG_MIRROR:-https://nodejs.org/dist}" "$node_version")"
            if [[ -n "$version" ]]; then
                info "Node.js $node_version shipped with npm $version"
            fi
        fi
        if [[ -z "$version" ]]; then
            version="$(_latest_npm_version "$registry")"
            if [[ -n "$version" ]]; then
                warn "Could not read the Node.js dist index; using the registry's latest npm $version, which may not support Node.js $node_version"
            fi
        fi
        if [[ -z "$version" ]]; then
            warn "Could not resolve an npm version for Node.js $node_version"
            return 1
        fi

        archive="$(mktemp "${TMPDIR:-/tmp}/anolisa-npm-XXXXXX.tgz")" || return 1
        if ! curl -fsSL --connect-timeout 10 --max-time 180 \
                -o "$archive" "$registry/npm/-/npm-$version.tgz"; then
            warn "Could not download npm $version from $registry"
            rm -f "$archive"
            return 1
        fi

        # Strip the archive's package/ prefix without touching sibling global packages.
        rm -rf "$npm_dir"
        if ! mkdir -p "$npm_dir" || ! tar -xzf "$archive" --strip-components=1 -C "$npm_dir"; then
            warn "Could not unpack npm $version into $npm_dir"
            rm -f "$archive"
            return 1
        fi
        rm -f "$archive"
        chmod +x "$npm_dir/bin/npm-cli.js" "$npm_dir/bin/npx-cli.js" 2>/dev/null || true
        ln -sfn "../lib/node_modules/npm/bin/npm-cli.js" "$prefix/bin/npm"
        ln -sfn "../lib/node_modules/npm/bin/npx-cli.js" "$prefix/bin/npx"

        if _npm_ok; then
            ok "npm $(PATH="$(_node_command_path)" npm -v) restored for the nvm-managed Node.js"
            return 0
        fi
        warn "Unpacked npm $version into $npm_dir but it still does not run"
        return 1
    }

    _ensure_npm() {
        if _npm_ok; then
            return 0
        fi
        if [[ "$INSTALL_MODE" == "system" ]]; then
            # System installs never mutate package state: the operator owns
            # language runtimes. Components whose build actually calls npm
            # (agent-memory) declare it as a source-build dependency record,
            # where the preflight already turns a missing npm into a manual
            # blocker, so this stays a warning here.
            warn "npm was not found in $RUNTIME_SYSTEM_PATH; the plugin builds invoke npm"
            return 0
        fi
        warn "Node.js is available but npm is missing; the plugin builds invoke npm"
        local node_bin
        node_bin="$(PATH="$(_node_command_path)" command -v node 2>/dev/null || true)"
        if [[ -n "$node_bin" && "$node_bin" == "${NVM_DIR:-$HOME/.nvm}"/* ]]; then
            _restore_nvm_npm "$node_bin" && return 0
            warn "Could not restore npm inside ${NVM_DIR:-$HOME/.nvm}; falling back to the $npm_pkg package"
        fi
        info "Installing $npm_pkg via $PKG_BASE ..."
        if [[ "$PKG_BASE" == "deb" ]]; then sudo apt-get update -y 2>/dev/null || true; fi
        # shellcheck disable=SC2086
        sudo $PKG_INSTALL $npm_pkg || true
        if _npm_ok; then
            ok "npm $(PATH="$(_node_command_path)" npm -v) installed"
            return 0
        fi
        die "Failed to install npm; the plugin builds run npm install and npm run build. Install the '$npm_pkg' package, or reinstall the Node.js version your nvm manages, then retry"
    }

    _configure_npm_mirror

    if _node_ver_ok; then
        local command_path="$PATH"
        [[ "$INSTALL_MODE" == "system" ]] && command_path="$RUNTIME_SYSTEM_PATH"
        ok "Node.js $(PATH="$command_path" node -v) already installed, skipping"
        _ensure_npm
        return 0
    fi

    # System installs keep language runtimes outside automatic package
    # mutation. The aggregate preflight normally reports this first; retain a
    # defensive check in case the runtime disappears between phases.
    if [[ "$INSTALL_MODE" == "system" ]]; then
        die "System Node.js >= $REQUIRED is required in $RUNTIME_SYSTEM_PATH; install it manually and retry"
    fi

    local repo_ver
    repo_ver=$(query_repo_ver "$node_pkg")
    if [[ -n "$repo_ver" ]] && ver_gte "$repo_ver" "$REQUIRED"; then
        info "Repository provides $node_pkg $repo_ver (>= $REQUIRED), installing via $PKG_BASE ..."
        if [[ "$PKG_BASE" == "deb" ]]; then sudo apt-get update -y 2>/dev/null || true; fi
        sudo $PKG_INSTALL $node_pkg $npm_pkg 2>/dev/null || true
        if _node_ver_ok; then
            local command_path="$PATH"
            [[ "$INSTALL_MODE" == "system" ]] && command_path="$RUNTIME_SYSTEM_PATH"
            ok "Node.js $(PATH="$command_path" node -v) installed via package manager"
            _ensure_npm
            return 0
        fi
        warn "Package manager install did not satisfy version requirement"
    else
        info "Repository $node_pkg${repo_ver:+ $repo_ver} does not meet >= $REQUIRED"
    fi

    info "Installing Node.js via nvm ..."

    if [[ "${SHELL}" == */zsh ]]; then touch "$HOME/.zshrc"; else touch "$HOME/.bashrc"; fi

    if ! cmd_exists nvm; then _source_nvm; fi

    if ! cmd_exists nvm; then
        info "Installing nvm ..."
        local NVM_VERSION="v0.40.3"
        export NVM_DIR="${NVM_DIR:-$HOME/.nvm}"
        # Disable interactive git prompts so clone fails fast instead of hanging
        export GIT_TERMINAL_PROMPT=0
        export GIT_ASKPASS=/bin/true
        local _nvm_script

        # Probe GitHub reachability (the official install.sh internally runs
        # `git clone github.com`, which hangs indefinitely when GitHub is
        # blocked — so we only try it when GitHub is actually reachable).
        local _github_ok=false
        if curl -sSf --connect-timeout 5 --max-time 10 \
                -o /dev/null https://github.com 2>/dev/null; then
            _github_ok=true
        fi

        if $_github_ok; then
            _nvm_script=$(mktemp /tmp/nvm-install-XXXXXX.sh)
            curl -fsSL --connect-timeout 10 --max-time 30 \
                "https://raw.githubusercontent.com/nvm-sh/nvm/${NVM_VERSION}/install.sh" \
                -o "$_nvm_script" 2>/dev/null || true
            [[ -s "$_nvm_script" ]] && bash "$_nvm_script" 2>/dev/null || true
            rm -f "$_nvm_script"
            _source_nvm
        else
            info "GitHub not reachable, skipping official installer"
        fi

        if ! cmd_exists nvm; then
            warn "Cloning nvm from Gitee mirror ..."
            if [[ -d "$NVM_DIR" && ! -s "$NVM_DIR/nvm.sh" ]]; then
                rm -rf "$NVM_DIR"
            fi
            if [[ ! -d "$NVM_DIR" ]]; then
                git clone --depth=1 --branch "$NVM_VERSION" \
                    https://gitee.com/mirrors/nvm.git "$NVM_DIR" 2>/dev/null \
                    || git clone https://gitee.com/mirrors/nvm.git "$NVM_DIR" 2>/dev/null || true
                if [[ -d "$NVM_DIR/.git" ]]; then
                    (cd "$NVM_DIR" && \
                        git checkout "$NVM_VERSION" 2>/dev/null \
                        || git checkout "$(git describe --abbrev=0 --tags --match "v[0-9]*" 2>/dev/null)" 2>/dev/null \
                        || true)
                fi
            fi
            local _rc="$HOME/.bashrc"
            [[ "${SHELL}" == */zsh ]] && _rc="$HOME/.zshrc"
            if [[ -s "$NVM_DIR/nvm.sh" ]] && ! grep -q 'NVM_DIR' "$_rc" 2>/dev/null; then
                {
                    echo ''
                    echo 'export NVM_DIR="$HOME/.nvm"'
                    echo '[ -s "$NVM_DIR/nvm.sh" ] && \. "$NVM_DIR/nvm.sh"'
                    echo '[ -s "$NVM_DIR/bash_completion" ] && \. "$NVM_DIR/bash_completion"'
                } >> "$_rc"
            fi
            _source_nvm
        fi
    fi
    cmd_exists nvm || die "Failed to install nvm"

    nvm install "$NVM_INSTALL_MAJOR" || \
        die "nvm install $NVM_INSTALL_MAJOR failed; check network or mirror settings"

    _configure_npm_mirror

    if _node_ver_ok; then
        _ensure_npm
        ok "Node.js $(node -v), npm $(npm -v)"
        info "nvm was sourced for this session; open a new terminal (or run: source ~/.bashrc) to persist"
    else
        die "Failed to install Node.js >= $REQUIRED"
    fi
}

install_build_tools() {
    step "Build tools (make, g++)"

    local missing=()
    if ! cmd_exists make; then missing+=("make"); fi
    if ! cmd_exists patch; then missing+=("patch"); fi

    if [[ "$PKG_BASE" == "rpm" ]]; then
        if ! cmd_exists g++; then missing+=("gcc-c++"); fi
    else
        if ! cmd_exists g++; then missing+=("g++"); fi
    fi

    if [[ ${#missing[@]} -eq 0 ]]; then
        ok "Build tools already installed, skipping"
        return 0
    fi

    info "Installing: ${missing[*]}"
    # shellcheck disable=SC2086
    sudo $PKG_INSTALL "${missing[@]}"
    ok "Build tools installed"
}

install_rust() {
    step "Rust (for agent-sec-core, cosh-ng, agentsight, tokenless, ws-ckpt, agent-memory)"
    local REQUIRED="1.91.0"

    local rust_pkg="rust" cargo_pkg="cargo"
    if [[ "$PKG_BASE" == "deb" ]]; then rust_pkg="rustc"; fi

    _source_cargo() {
        # shellcheck source=/dev/null
        if [[ -f "$HOME/.cargo/env" ]]; then source "$HOME/.cargo/env"; fi
    }

    _rust_ver_ok() {
        cmd_exists rustc && cmd_exists cargo || return 1
        local v
        v=$(extract_ver "$(rustc --version 2>/dev/null)" || echo "")
        [[ -n "$v" ]] && ver_gte "$v" "$REQUIRED"
    }

    _source_cargo
    # Capture overrides before automatic mirror selection exports its defaults.
    local rustup_server_override=false
    if [[ -n "${RUSTUP_DIST_SERVER:-}" || -n "${RUSTUP_UPDATE_ROOT:-}" ]]; then
        rustup_server_override=true
    fi
    _configure_cargo_mirror

    if _rust_ver_ok; then
        ok "Rust $(extract_ver "$(rustc --version)") already installed, skipping"
        return 0
    fi

    # If rustc exists but too old and rustup is available, try updating first.
    # Use a stable-channel mirror only for this command; the global
    # RUSTUP_DIST_SERVER remains selected for sec-core's pinned Rust toolchain.
    if cmd_exists rustup; then
        info "Updating via rustup ..."
        local stable_picked="" stable_dist stable_update_root
        if ! $rustup_server_override; then
            stable_picked=$(_pick_rustup_stable_mirror 2>/dev/null || echo "")
        fi
        if [[ -n "$stable_picked" ]]; then
            stable_dist="${stable_picked%%|*}"
            stable_update_root="${stable_picked##*|}"
            info "Rust stable channel mirror: ${stable_dist}"
            RUSTUP_DIST_SERVER="$stable_dist" \
                RUSTUP_UPDATE_ROOT="$stable_update_root" \
                rustup update stable || warn "rustup update stable failed; continuing with other Rust install methods"
        else
            rustup update stable || warn "rustup update stable failed; continuing with other Rust install methods"
        fi
        _source_cargo
        if _rust_ver_ok; then
            ok "Rust updated to $(extract_ver "$(rustc --version)") via rustup"
            return 0
        fi
    fi

    local repo_ver=""
    repo_ver=$(query_repo_ver "$rust_pkg")

    # DEB repos may ship versioned packages (rustc-1.XX) — pick the best one
    if [[ "$PKG_BASE" == "deb" ]]; then
        if [[ -z "$repo_ver" ]] || ! ver_gte "$repo_ver" "$REQUIRED"; then
            local best_pkg="" best_ver="" p pv
            while IFS= read -r p; do
                [[ -z "$p" ]] && continue
                pv=$(query_repo_ver "$p")
                [[ -z "$pv" ]] && continue
                if ver_gte "$pv" "$REQUIRED"; then
                    if [[ -z "$best_ver" ]] || ver_gte "$pv" "$best_ver"; then
                        best_pkg="$p"; best_ver="$pv"
                    fi
                fi
            done < <(apt-cache search '^rustc-[0-9]' 2>/dev/null | awk '{print $1}' | sort -V)
            if [[ -n "$best_pkg" ]]; then
                rust_pkg="$best_pkg"
                cargo_pkg="${best_pkg/rustc/cargo}"
                repo_ver="$best_ver"
            fi
        fi
    fi

    if [[ -n "$repo_ver" ]] && ver_gte "$repo_ver" "$REQUIRED"; then
        info "Repository provides $rust_pkg $repo_ver (>= $REQUIRED), installing via $PKG_BASE ..."
        sudo $PKG_INSTALL "$rust_pkg" "$cargo_pkg" gcc make || true

        # For versioned DEB packages (e.g. rustc-1.91), set up alternatives
        if [[ "$PKG_BASE" == "deb" && "$rust_pkg" != "rustc" ]]; then
            local suffix="${rust_pkg#rustc-}"
            if cmd_exists update-alternatives; then
                sudo update-alternatives --install /usr/bin/cargo cargo "/usr/bin/cargo-${suffix}" 100 2>/dev/null || true
            fi
        fi

        if _rust_ver_ok; then
            ok "Rust $(extract_ver "$(rustc --version)") installed via package manager"
            info "Note: agent-sec-core pins Rust ${SEC_CORE_RUST_TOOLCHAIN} via rust-toolchain.toml; rustup will auto-download if needed"
            return 0
        fi
        warn "Package manager install did not satisfy version requirement"
    else
        info "Repository ${rust_pkg}${repo_ver:+ $repo_ver} does not meet >= $REQUIRED"
    fi

    info "Installing Rust via rustup ..."
    sudo $PKG_INSTALL gcc make 2>/dev/null || true

    # Multi-level mirror fallback: official → Aliyun internal → Aliyun public → rsproxy.cn
    local _rustup_script
    _rustup_script=$(mktemp /tmp/rustup-init-XXXXXX.sh)
    curl --proto '=https' --tlsv1.2 -sSf --connect-timeout 15 --max-time 120 \
        https://sh.rustup.rs \
        -o "$_rustup_script" 2>/dev/null || true
    [[ -s "$_rustup_script" ]] && sh "$_rustup_script" -y 2>/dev/null || true
    rm -f "$_rustup_script"
    _source_cargo
    if ! cmd_exists rustc; then
        warn "rustup.rs unreachable, trying China mirrors ..."
        _rustup_script=$(mktemp /tmp/rustup-init-XXXXXX.sh)
        curl -sSf --connect-timeout 15 --max-time 60 \
            http://mirrors.cloud.aliyuncs.com/repo/rust/rustup-init.sh \
            -o "$_rustup_script" 2>/dev/null || true
        [[ -s "$_rustup_script" ]] && sh "$_rustup_script" -y 2>/dev/null || true
        rm -f "$_rustup_script"
        _source_cargo
    fi
    if ! cmd_exists rustc; then
        _rustup_script=$(mktemp /tmp/rustup-init-XXXXXX.sh)
        curl --proto '=https' --tlsv1.2 -sSf --connect-timeout 15 --max-time 120 \
            https://mirrors.aliyun.com/repo/rust/rustup-init.sh \
            -o "$_rustup_script" 2>/dev/null || true
        [[ -s "$_rustup_script" ]] && sh "$_rustup_script" -y 2>/dev/null || true
        rm -f "$_rustup_script"
        _source_cargo
    fi
    if ! cmd_exists rustc; then
        _rustup_script=$(mktemp /tmp/rustup-init-XXXXXX.sh)
        curl --proto '=https' --tlsv1.2 -sSf --connect-timeout 15 --max-time 120 \
            https://rsproxy.cn/rustup-init.sh \
            -o "$_rustup_script" 2>/dev/null || true
        [[ -s "$_rustup_script" ]] && sh "$_rustup_script" -y 2>/dev/null || true
        rm -f "$_rustup_script"
        _source_cargo
    fi

    if _rust_ver_ok; then
        ok "Rust $(extract_ver "$(rustc --version)"), cargo $(extract_ver "$(cargo --version)")"
    else
        die "Failed to install Rust >= $REQUIRED"
    fi
}

_configure_npm_mirror() {
    if [[ -z "${NVM_NODEJS_ORG_MIRROR:-}" ]]; then
        export NVM_NODEJS_ORG_MIRROR="https://npmmirror.com/mirrors/node/"
    fi
    export npm_config_registry="${npm_config_registry:-$NPM_REGISTRY}"
    export npm_config_replace_registry_host="${npm_config_replace_registry_host:-always}"

    if ! cmd_exists npm; then return 0; fi
    local current
    current=$(npm config get registry 2>/dev/null || echo "")
    if [[ "$current" == "$NPM_REGISTRY" || "$current" == "$NPM_REGISTRY/" ]]; then return 0; fi
    if [[ -n "$current" && "$current" != "https://registry.npmjs.org/" ]]; then
        info "Using npm registry for this build: $current"
        return 0
    fi
    npm config set registry "$NPM_REGISTRY"
    ok "npm registry mirror configured: $NPM_REGISTRY"
}

# Probe candidate rustup dist mirrors and pick the first reachable one.
# Returns the chosen base URL via stdout, or empty string on failure.
_rustup_host_triple() {
    if cmd_exists rustc; then
        rustc -vV 2>/dev/null | awk '/^host:/ { print $2; exit }'
        return 0
    fi

    case "$(uname -m 2>/dev/null || echo unknown)" in
        x86_64|amd64) echo "x86_64-unknown-linux-gnu" ;;
        aarch64|arm64) echo "aarch64-unknown-linux-gnu" ;;
        *) echo "x86_64-unknown-linux-gnu" ;;
    esac
}

_rustup_probe_path() {
    local host
    host="$(_rustup_host_triple)"
    echo "dist/rust-${SEC_CORE_RUST_TOOLCHAIN}-${host}.tar.gz.sha256"
}

_rustup_channel_path() {
    echo "dist/channel-rust-${SEC_CORE_RUST_TOOLCHAIN}.toml"
}

_rustup_dist_has_toolchain() {
    local base="$1"
    local toolchain_path="$2"
    local channel_path
    channel_path="$(_rustup_channel_path)"

    curl -sSfL --connect-timeout 3 --max-time 6 -o /dev/null \
        "$base/$channel_path" 2>/dev/null || return 1
    curl -sSfL --connect-timeout 3 --max-time 6 -o /dev/null \
        "$base/$toolchain_path" 2>/dev/null
}

_pick_rustup_mirror() {
    local candidates=(
        "https://rsproxy.cn|https://rsproxy.cn/rustup"
        "https://mirror.sjtu.edu.cn/rust-static|https://mirror.sjtu.edu.cn/rust-static/rustup"
        "https://mirrors.ustc.edu.cn/rust-static|https://mirrors.ustc.edu.cn/rust-static/rustup"
        "https://static.rust-lang.org|https://static.rust-lang.org/rustup"
    )
    # Probe both the versioned channel manifest and a real toolchain tarball
    # checksum. Some mirrors expose only one of them while rustup needs both.
    local probe_path
    probe_path="$(_rustup_probe_path)"
    local entry base
    for entry in "${candidates[@]}"; do
        base="${entry%%|*}"
        if _rustup_dist_has_toolchain "$base" "$probe_path"; then
            echo "$entry"
            return 0
        fi
    done
    return 1
}

_rustup_stable_dist_available() {
    local base="$1"

    curl -sSfL --connect-timeout 3 --max-time 6 -o /dev/null \
        "$base/dist/channel-rust-stable.toml.sha256" 2>/dev/null
}

_pick_rustup_stable_mirror() {
    local candidates=(
        "https://mirrors.tuna.tsinghua.edu.cn/rustup|https://mirrors.tuna.tsinghua.edu.cn/rustup/rustup"
        "https://rsproxy.cn|https://rsproxy.cn/rustup"
        "https://mirrors.ustc.edu.cn/rust-static|https://mirrors.ustc.edu.cn/rust-static/rustup"
        "https://mirror.sjtu.edu.cn/rust-static|https://mirror.sjtu.edu.cn/rust-static/rustup"
        "https://static.rust-lang.org|https://static.rust-lang.org/rustup"
    )
    local entry base
    for entry in "${candidates[@]}"; do
        base="${entry%%|*}"
        if _rustup_stable_dist_available "$base"; then
            echo "$entry"
            return 0
        fi
    done
    return 1
}

_configure_cargo_mirror() {
    local _aliyun_internal=false
    if curl -sSf --connect-timeout 3 http://mirrors.cloud.aliyuncs.com/ &>/dev/null; then
        _aliyun_internal=true
    fi

    # Ensures rustup downloads from a reachable mirror (e.g. when
    # rust-toolchain.toml triggers an auto-install of a pinned version).
    # This is CRITICAL: when cargo build encounters rust-toolchain.toml,
    # rustup silently downloads the pinned toolchain (7+ components, ~300MB)
    # from the configured dist server — defaulting to static.rust-lang.org,
    # which is effectively unreachable from China and causes long hangs.
    local picked dist update_root
    if [[ -n "${RUSTUP_DIST_SERVER:-}" || -n "${RUSTUP_UPDATE_ROOT:-}" ]]; then
        # Let rustup use the configured endpoints and report download failures.
        info "Using configured rustup servers"
    else
        picked=$(_pick_rustup_mirror 2>/dev/null || echo "")
        if [[ -n "$picked" ]]; then
            dist="${picked%%|*}"
            update_root="${picked##*|}"
            export RUSTUP_DIST_SERVER="$dist"
            export RUSTUP_UPDATE_ROOT="$update_root"
            info "RUSTUP_DIST_SERVER=${RUSTUP_DIST_SERVER}"
        else
            # No mirror reachable — fall back to rsproxy.cn and let rustup surface any error
            export RUSTUP_DIST_SERVER="https://rsproxy.cn"
            export RUSTUP_UPDATE_ROOT="https://rsproxy.cn/rustup"
            warn "No rustup mirror reachable; falling back to ${RUSTUP_DIST_SERVER}"
        fi
    fi

    local cargo_home="${CARGO_HOME:-$HOME/.cargo}"
    local cargo_config="$cargo_home/config.toml"
    local cargo_config_legacy="$cargo_home/config"
    # Skip if user already has a custom registry configured
    if [[ -f "$cargo_config" ]] && grep -q '\[source\.' "$cargo_config" 2>/dev/null; then
        info "Existing cargo registry config found, skipping crates.io mirror setup"
        return 0
    fi
    if [[ -f "$cargo_config_legacy" ]] && grep -q '\[source\.' "$cargo_config_legacy" 2>/dev/null; then
        info "Existing cargo registry config found, skipping crates.io mirror setup"
        return 0
    fi

    local mirror_url
    if $_aliyun_internal; then
        mirror_url="sparse+http://mirrors.cloud.aliyuncs.com/crates.io-index/"
        info "Using Aliyun internal crates.io mirror"
    else
        mirror_url="sparse+https://mirrors.aliyun.com/crates.io-index/"
        info "Using Aliyun public crates.io mirror"
    fi

    mkdir -p "$cargo_home"
    if ! grep -q '\[source\.crates-io\]' "$cargo_config" 2>/dev/null; then
        cat >> "$cargo_config" <<EOF

[source.crates-io]
replace-with = 'aliyun'
[source.aliyun]
registry = "$mirror_url"
EOF
    fi
    ok "crates.io mirror configured in $cargo_config"
}

_configure_git_mirror() {
    # Configure a reachable GitHub mirror for git operations when
    # github.com is blocked (e.g. ECS instances in China).
    # IMPORTANT: we write to --global (not --local) so that git-clone processes
    # (e.g. justfile setup-rtk) also inherit the insteadOf rule.
    local repo_dir="${1:-.}"

    if curl -sSf --connect-timeout 3 --max-time 6 -o /dev/null https://github.com 2>/dev/null; then
        return 0
    fi

    local existing
    existing=$(git config --global --get-regexp 'url\..*insteadOf' 2>/dev/null | grep -i github | head -1 || true)
    if [[ -n "$existing" ]]; then
        info "Git insteadOf already configured: $existing"
        return 0
    fi

    info "GitHub unreachable, probing mirrors ..."
    local mirror_base mirror_full
    local candidates=(
        "https://gh-proxy.com"
        "https://ghps.cc"
        "https://mirror.ghproxy.com"
        "https://ghproxy.com"
        "https://gitclone.com"
    )
    local c
    for c in "${candidates[@]}"; do
        if curl -sSf --connect-timeout 3 --max-time 6 -o /dev/null "$c/" 2>/dev/null; then
            mirror_base="${c}/"
            mirror_full="${c}/https://github.com/"
            break
        fi
    done

    if [[ -z "${mirror_base:-}" ]]; then
        warn "All GitHub mirrors unreachable; submodule clone may fail"
        return 0
    fi

    git config --global "url.${mirror_full}.insteadOf" "https://github.com/"
    ok "Git mirror (global): $mirror_base"
}

_configure_uv_mirror() {
    # Configure mirrors for uv (and pip3 as fallback).
    # uv respects these env vars and ~/.config/uv/uv.toml.
    local aliyun_pypi="https://mirrors.aliyun.com/pypi/simple/"
    local official_python_install_mirror="https://github.com/astral-sh/python-build-standalone/releases/download"
    local legacy_python_install_mirror="https://mirror.nju.edu.cn/github-release/astral-sh/python-build-standalone"
    local python_install_mirror="${UV_PYTHON_INSTALL_MIRROR:-$official_python_install_mirror}"

    export UV_INDEX_URL="$aliyun_pypi"
    export UV_DEFAULT_INDEX="$aliyun_pypi"
    export UV_PYTHON_INSTALL_MIRROR="$python_install_mirror"
    export PIP_INDEX_URL="$aliyun_pypi"

    # python-install-mirror toml field requires uv >= 0.5.2; skip for older versions
    local _uv_supports_pim=true
    if cmd_exists uv; then
        local _uv_ver
        _uv_ver=$(extract_ver "$(uv --version 2>/dev/null)")
        if [[ -n "$_uv_ver" ]] && ! ver_gte "$_uv_ver" "0.5.2"; then
            _uv_supports_pim=false
        fi
    fi

    local uv_cfg="$HOME/.config/uv/uv.toml"
    if [[ -f "$uv_cfg" ]] && \
            grep -Fq 'managed by build-all.sh' "$uv_cfg" 2>/dev/null && \
            grep -Fqx "python-install-mirror = \"$legacy_python_install_mirror\"" \
                "$uv_cfg" 2>/dev/null; then
        local tmp_cfg old_setting new_setting
        tmp_cfg=$(mktemp)
        old_setting="python-install-mirror = \"$legacy_python_install_mirror\""
        new_setting="python-install-mirror = \"$python_install_mirror\""
        if awk -v old="$old_setting" -v new="$new_setting" \
                '{ print ($0 == old ? new : $0) }' "$uv_cfg" > "$tmp_cfg"; then
            mv "$tmp_cfg" "$uv_cfg"
            ok "uv Python install mirror migrated: $python_install_mirror"
        else
            rm -f "$tmp_cfg"
            return 1
        fi
    fi

    if [[ ! -f "$uv_cfg" ]]; then
        mkdir -p "$(dirname "$uv_cfg")"
        if [[ "$_uv_supports_pim" == "true" ]]; then
            cat > "$uv_cfg" <<EOF
# uv configuration — managed by build-all.sh
python-install-mirror = "$python_install_mirror"

[[index]]
url = "https://mirrors.aliyun.com/pypi/simple/"
default = true
EOF
            ok "uv PyPI mirror configured: $aliyun_pypi"
            ok "uv Python install mirror configured: $python_install_mirror"
        else
            cat > "$uv_cfg" <<EOF
# uv configuration — managed by build-all.sh

[[index]]
url = "https://mirrors.aliyun.com/pypi/simple/"
default = true
EOF
            ok "uv PyPI mirror configured: $aliyun_pypi"
            info "uv < 0.5.2: python-install-mirror skipped in toml (env var still active)"
        fi
        return 0
    fi

    if [[ "$_uv_supports_pim" == "true" ]]; then
        if ! grep -Eq '^[[:space:]]*python-install-mirror[[:space:]]*=' "$uv_cfg" 2>/dev/null; then
            local tmp_cfg
            tmp_cfg=$(mktemp)
            {
                echo "python-install-mirror = \"$python_install_mirror\""
                echo ""
                cat "$uv_cfg"
            } > "$tmp_cfg" && mv "$tmp_cfg" "$uv_cfg"
            ok "uv Python install mirror configured: $python_install_mirror"
        fi
    else
        : # uv < 0.5.2 does not support python-install-mirror in toml; env var is sufficient
    fi

    if ! grep -q 'mirrors.aliyun.com/pypi/simple/' "$uv_cfg" 2>/dev/null; then
        cat >> "$uv_cfg" <<'EOF'

[[index]]
url = "https://mirrors.aliyun.com/pypi/simple/"
default = true
EOF
        ok "uv PyPI mirror configured: $aliyun_pypi"
    fi
}

install_uv() {
    step "uv (Python package manager, for agent-sec-core)"

    if cmd_exists uv; then
        ok "uv $(extract_ver "$(uv --version 2>/dev/null)") already installed, skipping"
        return 0
    fi

    if cmd_exists pip3; then
        info "Trying: pip3 install uv ..."
        pip3 install uv 2>/dev/null || true
        if cmd_exists uv; then
            ok "uv $(extract_ver "$(uv --version 2>/dev/null)") installed via pip3"
            return 0
        fi
    fi

    if ! cmd_exists pipx; then
        info "Trying to install pipx via package manager ..."
        sudo $PKG_INSTALL pipx 2>/dev/null || true
    fi
    if cmd_exists pipx; then
        info "Trying: pipx install uv ..."
        pipx ensurepath 2>/dev/null || true
        export PATH="$HOME/.local/bin:$PATH"
        pipx install uv 2>/dev/null || true
        if cmd_exists uv; then
            ok "uv $(extract_ver "$(uv --version 2>/dev/null)") installed via pipx"
            return 0
        fi
    fi

    info "Installing uv via upstream installer ..."
    local _uv_script
    _uv_script=$(mktemp /tmp/uv-install-XXXXXX.sh)
    curl -LsSf --connect-timeout 15 --max-time 60 \
        https://astral.sh/uv/install.sh \
        -o "$_uv_script" 2>/dev/null || true
    [[ -s "$_uv_script" ]] && sh "$_uv_script" 2>/dev/null || true
    rm -f "$_uv_script"
    if [[ -f "$HOME/.local/bin/env" ]]; then
        # shellcheck source=/dev/null
        source "$HOME/.local/bin/env"
    fi
    export PATH="$HOME/.local/bin:$PATH"
    if ! cmd_exists uv; then
        warn "astral.sh unreachable, trying GitHub mirror ..."
        _uv_script=$(mktemp /tmp/uv-install-XXXXXX.sh)
        curl -LsSf --connect-timeout 15 --max-time 60 \
            https://github.com/astral-sh/uv/releases/latest/download/uv-installer.sh \
            -o "$_uv_script" 2>/dev/null || true
        [[ -s "$_uv_script" ]] && sh "$_uv_script" 2>/dev/null || true
        rm -f "$_uv_script"
        if [[ -f "$HOME/.local/bin/env" ]]; then
            # shellcheck source=/dev/null
            source "$HOME/.local/bin/env"
        fi
    fi

    if cmd_exists uv; then
        ok "uv $(extract_ver "$(uv --version 2>/dev/null)")"
    else
        die "Failed to install uv"
    fi
}

check_ebpf_deps() {
    step "eBPF dependencies (for agentsight)"

    info "AgentSight requires clang, llvm, and libbpf headers from your system package manager."

    local missing=()

    if ! cmd_exists clang; then missing+=("clang"); fi
    if ! cmd_exists llvm-config && ! cmd_exists llvm-config-*; then missing+=("llvm"); fi

    if [[ "$PKG_BASE" == "rpm" ]]; then
        local pkgs=("libbpf-devel" "libbpf-static" "elfutils-libelf-devel" "zlib-devel" "openssl-devel" "perl" "perl-core" "pkg-config")
        local pkg
        for pkg in "${pkgs[@]}"; do
            if ! rpm -q "$pkg" &>/dev/null; then
                missing+=("$pkg")
            fi
        done
        if ! perl_module_exists "IPC::Cmd"; then
            missing+=("perl(IPC::Cmd)")
        fi
        if ! perl_module_exists "FindBin"; then
            missing+=("perl(FindBin)")
        fi

        if [[ ${#missing[@]} -eq 0 ]]; then
            ok "All eBPF packages present"
        else
            warn "Missing eBPF packages: ${missing[*]}"
            info "Install with: ${BOLD}sudo dnf install -y $(shell_args "${missing[@]}")${NC}"

            if $INSTALL_DEPS; then
                info "Installing missing eBPF packages ..."
                # shellcheck disable=SC2086
                sudo $PKG_INSTALL "${missing[@]}"
                ok "eBPF packages installed"
            fi
        fi

    elif [[ "$PKG_BASE" == "deb" ]]; then
        local pkgs=("libbpf-dev" "libelf-dev" "zlib1g-dev" "libssl-dev" "perl")
        local kver
        kver=$(uname -r 2>/dev/null || echo "")
        if [[ -n "$kver" ]]; then
            pkgs+=("linux-headers-${kver}")
        fi
        local pkg
        for pkg in "${pkgs[@]}"; do
            if ! dpkg -s "$pkg" &>/dev/null 2>&1; then
                missing+=("$pkg")
            fi
        done

        if [[ ${#missing[@]} -eq 0 ]]; then
            ok "All eBPF packages present"
        else
            warn "Missing eBPF packages: ${missing[*]}"
            info "Install with: ${BOLD}sudo apt-get install -y ${missing[*]}${NC}"

            if $INSTALL_DEPS; then
                info "Updating package index ..."
                sudo apt-get update -y
                info "Installing missing eBPF packages ..."
                sudo $PKG_INSTALL "${missing[@]}"
                ok "eBPF packages installed"
            fi
        fi
    fi

    if [[ -f /sys/kernel/btf/vmlinux ]]; then
        ok "Kernel BTF support available"
    else
        warn "Kernel BTF not found (/sys/kernel/btf/vmlinux). agentsight requires CONFIG_DEBUG_INFO_BTF=y"
    fi
}

# ─── top-level dep installer ───

install_just() {
    step "just (command runner, for tokenless rtk setup)"

    if cmd_exists just; then
        ok "just already installed, skipping"
        return 0
    fi

    # just may have been installed alongside rustup; source cargo env first
    # shellcheck source=/dev/null
    [[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"

    if cmd_exists cargo; then
        info "Installing just via cargo install ..."
        cargo install just 2>/dev/null || true
        if cmd_exists just; then
            ok "just installed via cargo install"
            return 0
        fi
    fi

    warn "'just' is required for tokenless build (rtk clone+patch). Install manually: cargo install just"
}

do_install_deps() {
    if $DRY_RUN; then
        step "Dependency plan"
        echo "DRY-RUN: detect Linux distribution and package manager"
        if $DO_INSTALL || $DEPS_ONLY; then
            if [[ "$INSTALL_MODE" == "system" ]]; then
                echo "DRY-RUN: preflight all selected component runtime dependencies"
            else
                echo "DRY-RUN: preflight platform capabilities before user dependency setup"
            fi
        fi
        if want_component cosh || want_component sec-core || want_component sight || want_component memory; then
            echo "DRY-RUN: check/install Node.js and npm if needed"
        fi
        if want_component cosh || want_component sec-core || want_component cosh-ng || want_component sight; then
            echo "DRY-RUN: check/install build tools if needed"
        fi
        if want_component sec-core || want_component cosh-ng || want_component sight || want_component tokenless || want_component ws-ckpt || want_component memory; then
            echo "DRY-RUN: check/install Rust toolchain if needed"
        fi
        if want_component tokenless; then
            echo "DRY-RUN: check/install just if needed"
        fi
        if want_component sec-core; then
            echo "DRY-RUN: check/install uv and configure Python mirrors if needed"
        fi
        if want_component sight; then
            echo "DRY-RUN: check agentsight eBPF dependencies"
        fi
        if { $DO_INSTALL || $DEPS_ONLY; } && [[ "$INSTALL_MODE" != "system" ]]; then
            echo "DRY-RUN: verify all runtime dependencies after user dependency setup"
        fi
        ok "Dependency setup plan generated"
        return 0
    fi

    step "Detecting system"
    detect_distro

    if $DO_INSTALL || $DEPS_ONLY; then
        if [[ "$INSTALL_MODE" == "system" ]]; then
            preflight_runtime_dependencies || return 1
        else
            preflight_runtime_dependencies platform-only || return 1
        fi
    fi

    if want_component cosh || want_component sec-core || want_component sight || want_component memory; then
        install_node
    fi

    if want_component cosh || want_component sec-core || want_component cosh-ng || want_component sight; then
        install_build_tools
    fi

    if want_component sec-core || want_component cosh-ng || want_component sight || want_component tokenless || want_component ws-ckpt || want_component memory; then
        install_rust
    fi

    if want_component tokenless; then
        install_just
    fi

    if want_component sec-core; then
        _configure_uv_mirror
        install_uv
    fi

    if want_component sight; then
        check_ebpf_deps
    fi

    if { $DO_INSTALL || $DEPS_ONLY; } && [[ "$INSTALL_MODE" != "system" ]]; then
        preflight_runtime_dependencies || return 1
    fi

    echo ""
    ok "Dependency setup complete"
}

# ─── build functions ───

build_cosh() {
    step "Building copilot-shell"
    local dir="$PROJECT_ROOT/deprecated/copilot-shell"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    run_logged "npm install (deps)" make deps
    run_logged "esbuild + bundle" make build

    if $DRY_RUN; then
        stage_component_make_install "copilot-shell" "$dir"
        ok "copilot-shell build plan generated"
        return 0
    fi

    if [[ -f dist/cli.js ]]; then
        stage_component_make_install "copilot-shell" "$dir"
        ok "copilot-shell built successfully"
    else
        warn "Expected artifact dist/cli.js not found"
    fi
}

build_skills() {
    step "Preparing os-skills"
    local dir="$PROJECT_ROOT/src/os-skills"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    local count=0
    count=$(find . -name "SKILL.md" 2>/dev/null | wc -l)
    count=$((count + 0)) # trim whitespace

    info "Found ${count} skill definitions (install step will deploy by mode)"

    stage_component_make_install "os-skills" "$dir"

    if $DRY_RUN; then
        ok "os-skills stage plan generated for $(component_target_dir os-skills)"
        return 0
    fi

    ok "os-skills staged to $(component_target_dir os-skills)"
}

build_sec_core() {
    step "Building agent-sec-core"
    local dir="$PROJECT_ROOT/src/agent-sec-core"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    local component_root build_dir
    component_root="$(component_target_dir sec-core)"
    build_dir="$component_root/build"

    if $DRY_RUN; then
        echo "DRY-RUN: rm -rf $component_root"
        echo "DRY-RUN: mkdir -p $component_root"
        echo "DRY-RUN: (cd $dir && make build-all BUILD_DIR=$build_dir)"
        ok "agent-sec-core build plan generated"
        return 0
    fi

    rm -rf "$component_root"
    mkdir -p "$component_root"

    info "make build-all (sandbox + CLI + sec-core assets) ..."
    run_logged_timeout "${AGENT_SEC_BUILD_TIMEOUT:-1200}" \
        "make build-all (agent-sec-core)" \
        make build-all BUILD_DIR="$build_dir"

    if [[ -d "$build_dir/share" ]]; then
        rm -rf "$component_root/share"
        cp -a "$build_dir/share" "$component_root/share"
    fi

    local bin="$build_dir/linux-sandbox"
    if [[ -f "$bin" ]]; then
        ok "agent-sec-core built successfully"
    else
        warn "Expected artifact $bin not found"
    fi
}

build_cosh_ng() {
    step "Building cosh-ng"
    local dir="$PROJECT_ROOT/src/cosh-ng"
    local cosh_ng_toolchain="${COSH_NG_RUST_TOOLCHAIN:-1.88.0}"
    [[ -d "$dir" ]] || die "Directory not found: $dir"

    local component_root bin source
    local -a cargo_command=(cargo)
    component_root="$(component_target_dir cosh-ng)"

    if $DRY_RUN; then
        echo "DRY-RUN: rm -rf $component_root"
        echo "DRY-RUN: ensure Rust $cosh_ng_toolchain via rustup, or validate the PATH toolchain"
        echo "DRY-RUN: CARGO_NET_GIT_FETCH_WITH_CLI=true rustup run $cosh_ng_toolchain cargo build --manifest-path $dir/Cargo.toml --workspace --release"
        for bin in cosh-cli cosh-core cosh-gateway cosh-shell; do
            echo "DRY-RUN: install $dir/target/release/$bin -> $component_root/bin/$bin"
        done
        ok "cosh-ng build plan generated"
        return 0
    fi

    if cmd_exists rustup; then
        if ! rustup toolchain list | grep -Eq "^${cosh_ng_toolchain}(-|[[:space:]])"; then
            info "Installing cosh-ng Rust toolchain ${cosh_ng_toolchain} ..."
            rustup toolchain install "$cosh_ng_toolchain" --profile minimal
        fi
        cargo_command=(rustup run "$cosh_ng_toolchain" cargo)
    else
        local installed_rust
        installed_rust=$(extract_ver "$(rustc --version 2>/dev/null)" || echo "")
        [[ -n "$installed_rust" ]] && ver_gte "$installed_rust" "$cosh_ng_toolchain" \
            || die "cosh-ng requires Rust >= ${cosh_ng_toolchain}; rustup is unavailable"
        info "rustup unavailable; using validated Rust ${installed_rust} from PATH"
    fi

    rm -rf "$component_root"
    mkdir -p "$component_root/bin"

    # Run from the repository root so Cargo honors user registry policy without
    # discovering a component-local source replacement from parent traversal.
    cd "$PROJECT_ROOT"
    run_logged_timeout "${COSH_NG_BUILD_TIMEOUT:-1200}" \
        "cargo build (cosh-ng workspace)" \
        env CARGO_NET_GIT_FETCH_WITH_CLI=true \
        "${cargo_command[@]}" build \
            --manifest-path "$dir/Cargo.toml" --workspace --release

    for bin in cosh-cli cosh-core cosh-gateway cosh-shell; do
        source="$dir/target/release/$bin"
        copy_file "$source" "$component_root/bin/$bin" 0755
    done

    ok "cosh-ng built successfully"
}

build_sight() {
    step "Building agentsight"
    local dir="$PROJECT_ROOT/src/agentsight"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    if [[ -f Makefile ]] && grep -q 'build' Makefile; then
        stage_component_make_install "agentsight" "$dir" \
            SERVICE_BINDIR="$SYSTEM_BIN_DIR" SETCAP=0 \
            NPM_REGISTRY="$NPM_REGISTRY" NPM_REPLACE_REGISTRY_HOST=always
        if $DRY_RUN; then
            ok "agentsight build plan generated"
            return 0
        fi
    else
        run_logged "cargo build (agentsight)" cargo build --release
        if $DRY_RUN; then
            echo "DRY-RUN: copy target/release/agentsight -> $(component_target_dir agentsight)/bin/agentsight"
            ok "agentsight build plan generated"
            return 0
        fi
        copy_file target/release/agentsight "$(component_target_dir agentsight)/bin/agentsight" 0755
    fi

    local bin="target/release/agentsight"
    if [[ -f "$bin" || -f "$(component_target_dir agentsight)/bin/agentsight" ]]; then
        ok "agentsight built successfully"
    else
        warn "Expected artifact $bin not found"
    fi
}

build_tokenless() {
    step "Building tokenless"
    local dir="$PROJECT_ROOT/src/tokenless"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    # rtk setup is handled by Makefile build-tokenless target (just setup-rtk),
    # but 'just' must be available before make install runs.
    if ! $DRY_RUN; then
        if ! command -v just &>/dev/null; then
            die "'just' is required for tokenless build (rtk clone+patch orchestration). Install: cargo install just"
        fi
    else
        info "DRY-RUN: just setup-rtk would be called by Makefile build-tokenless"
    fi

    info "make install (tokenless workspace) ..."
    stage_component_make_install "tokenless" "$dir"
    if $DRY_RUN; then
        ok "tokenless build plan generated"
        return 0
    fi

    local component_root bin rtk_bin
    component_root="$(component_target_dir tokenless)"
    bin="$component_root/bin/tokenless"
    rtk_bin="$component_root/libexec/anolisa/tokenless/rtk"
    if [[ -f "$bin" ]] && [[ -f "$rtk_bin" ]]; then
        if [[ ! -d "$component_root/share/anolisa/adapters/tokenless" ]]; then
            warn "tokenless adapter resources staged empty"
        fi
        if [[ ! -d "$component_root/share/anolisa/extensions/tokenless" ]]; then
            warn "tokenless cosh extension staged empty"
        fi
        ok "tokenless and rtk built successfully"
    else
        [[ -f "$bin" ]]     || warn "Expected artifact $bin not found"
        [[ -f "$rtk_bin" ]] || warn "Expected artifact $rtk_bin not found"
    fi
}

build_wsckpt() {
    step "Building ws-ckpt"
    local dir="$PROJECT_ROOT/src/ws-ckpt"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    stage_component_make_install "ws-ckpt" "$dir"
    if $DRY_RUN; then
        ok "ws-ckpt build plan generated"
        return 0
    fi

    local component_root bin
    component_root="$(component_target_dir ws-ckpt)"
    bin="$component_root/bin/ws-ckpt"
    if [[ -f "$bin" ]]; then
        ok "ws-ckpt built successfully"
    else
        warn "Expected artifact $bin not found"
    fi
}

build_agent_memory() {
    step "Building agent-memory"
    local dir="$PROJECT_ROOT/src/agent-memory"
    [[ -d "$dir" ]] || die "Directory not found: $dir"
    cd "$dir"

    # agent-memory needs cmake (for git2's vendored libgit2) and libsystemd
    # headers (for the journald audit fan-out); both are missing from the
    # default toolchain installs above.
    if ! $DRY_RUN; then
        local missing=()
        cmd_exists cmake || missing+=("cmake")
        if [[ "$PKG_BASE" == "rpm" ]] && ! rpm -q systemd-devel &>/dev/null; then
            missing+=("systemd-devel")
        elif [[ "$PKG_BASE" == "deb" ]] && ! dpkg -s libsystemd-dev &>/dev/null 2>&1; then
            missing+=("libsystemd-dev")
        fi
        if [[ ${#missing[@]} -gt 0 ]]; then
            warn "agent-memory native deps missing: ${missing[*]}"
            info "Install with: ${BOLD}sudo $PKG_INSTALL ${missing[*]}${NC}"
        fi
    fi

    stage_component_make_install "agent-memory" "$dir"
    if $DRY_RUN; then
        ok "agent-memory build plan generated"
        return 0
    fi

    local component_root bin
    component_root="$(component_target_dir agent-memory)"
    bin="$component_root/bin/agent-memory"
    if [[ -f "$bin" ]]; then
        ok "agent-memory built successfully"
    else
        warn "Expected artifact $bin not found"
    fi
}

do_build() {
    # shellcheck source=/dev/null
    [[ -f "$HOME/.cargo/env" ]] && source "$HOME/.cargo/env"
    # shellcheck source=/dev/null
    [[ -s "$HOME/.nvm/nvm.sh" ]] && { export NVM_DIR="$HOME/.nvm"; source "$HOME/.nvm/nvm.sh"; }
    export PATH="$HOME/.local/bin:$PATH"

    if $DRY_RUN; then
        if want_component sec-core || want_component cosh-ng || want_component sight || want_component tokenless || want_component ws-ckpt || want_component memory; then
            echo "DRY-RUN: configure cargo mirror for this build"
        fi
        if want_component cosh || want_component sec-core || want_component sight; then
            echo "DRY-RUN: configure npm registry for this build"
        fi
        if want_component sec-core; then
            echo "DRY-RUN: configure uv mirrors for this build"
        fi
        if want_component tokenless; then
            echo "DRY-RUN: configure git mirror for this build"
        fi
        echo "DRY-RUN: rm -rf $OUTPUT_DIR"
        echo "DRY-RUN: mkdir -p $OUTPUT_DIR"
    else
        if want_component sec-core || want_component cosh-ng || want_component sight || want_component tokenless || want_component ws-ckpt || want_component memory; then
            _configure_cargo_mirror
        fi
        if want_component cosh || want_component sec-core || want_component sight; then
            _configure_npm_mirror
        fi
        if want_component sec-core; then
            _configure_uv_mirror
        fi
        if want_component tokenless; then
            _configure_git_mirror "$PROJECT_ROOT"
        fi

        rm -rf "$OUTPUT_DIR"
        mkdir -p "$OUTPUT_DIR"

        : > "$LOG_FILE"
        info "Build log → $LOG_FILE"
    fi

    if want_component cosh;      then build_cosh;         fi
    if want_component skills;    then build_skills;       fi
    if want_component sec-core;  then build_sec_core;     fi
    if want_component cosh-ng;   then build_cosh_ng;      fi
    if want_component tokenless; then build_tokenless;    fi
    if want_component ws-ckpt;   then build_wsckpt;       fi
    if want_component memory;    then build_agent_memory;   fi
    if want_component sight;     then build_sight;        fi
}

# ─── install functions ───

install_cosh() {
    step "Installing copilot-shell"
    local dir="$PROJECT_ROOT/deprecated/copilot-shell"
    run_component_make_install "copilot-shell" "$dir"
    if $DRY_RUN; then
        ok "copilot-shell install plan generated"
    else
        ok "copilot-shell installed to ${INSTALL_BIN_DIR}/{cosh,co,copilot}"
    fi
}

install_skills() {
    step "Installing os-skills"
    local dir="$PROJECT_ROOT/src/os-skills"
    run_component_make_install "os-skills" "$dir"
    local skills_dir="/usr/share/anolisa/skills"
    [[ "$INSTALL_MODE" == "user" ]] && skills_dir="$USER_COSH_SKILLS_DIR"
    if $DRY_RUN; then
        ok "os-skills install plan generated for ${skills_dir}"
    else
        ok "os-skills installed to ${skills_dir}"
    fi
}

# Detection stays non-fatal so every selected component can appear in one
# actionable report before any package-manager mutation.
detect_runtime_package_manager() {
    if [[ "$PKG_BASE" =~ ^(deb|rpm)$ && -n "$PKG_INSTALL" ]]; then
        return 0
    fi

    [[ -r /etc/os-release ]] || return 1
    local ID="" ID_LIKE=""
    # shellcheck source=/dev/null
    source /etc/os-release

    if [[ "${ID:-}" =~ ^(fedora|rhel|centos|anolis|alinux)$ ]] || \
            [[ "${ID_LIKE:-}" =~ (fedora|rhel) ]]; then
        PKG_BASE="rpm"
        if cmd_exists dnf; then
            PKG_INSTALL="dnf install -y"
        elif cmd_exists yum; then
            PKG_INSTALL="yum install -y"
        else
            return 1
        fi
    elif [[ "${ID:-}" =~ ^(debian|ubuntu)$ ]] || [[ "${ID_LIKE:-}" =~ debian ]]; then
        cmd_exists apt-get || return 1
        PKG_BASE="deb"
        PKG_INSTALL="apt-get install -y"
    else
        return 1
    fi
}

runtime_manifest_path() {
    case "$1" in
        cosh)      echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/cosh/component.toml" ;;
        skills)    echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/os-skills/component.toml" ;;
        sec-core)  echo "$PROJECT_ROOT/src/agent-sec-core/.anolisa/component.toml" ;;
        cosh-ng)   echo "$PROJECT_ROOT/src/cosh-ng/.anolisa/component.toml" ;;
        tokenless) echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/tokenless/component.toml" ;;
        ws-ckpt)   echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/ws-ckpt/component.toml" ;;
        memory)    echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/agent-memory/component.toml" ;;
        sight)     echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/agentsight/component.toml" ;;
        *)         return 1 ;;
    esac
}

# Emits component|name|kind|probe|rpm|deb|check|version|min_kernel records.
runtime_dependencies_for_manifest() {
    local component="$1" manifest="$2"
    [[ -r "$manifest" ]] || return 1
    awk -v component="$component" '
        function trim(value) {
            sub(/^[[:space:]]+/, "", value)
            sub(/[[:space:]]+$/, "", value)
            return value
        }
        function quoted_string(value, quote, position, char, escaped, result) {
            value = trim(value)
            quote = substr(value, 1, 1)
            if (quote != "\"" && quote != sprintf("%c", 39)) return ""
            value = substr(value, 2)
            escaped = 0
            result = ""
            for (position = 1; position <= length(value); position++) {
                char = substr(value, position, 1)
                if (quote == "\"" && escaped) {
                    result = result char
                    escaped = 0
                } else if (quote == "\"" && char == "\\") {
                    escaped = 1
                } else if (char == quote) {
                    return result
                } else {
                    result = result char
                }
            }
            return ""
        }
        function assignment_string(line, value) {
            value = line
            sub(/^[^=]*=[[:space:]]*/, "", value)
            return quoted_string(value)
        }
        function inline_string(line, key, pattern, value) {
            pattern = "(^|[,{[:space:]])" key "[[:space:]]*=[[:space:]]*"
            if (!match(line, pattern)) return ""
            value = substr(line, RSTART + RLENGTH)
            return quoted_string(value)
        }
        function emit() {
            if (name != "") {
                print component "|" name "|" kind "|" probe "|" rpm "|" deb \
                    "|" check "|" version "|" min_kernel
            }
        }
        function clear_dep() {
            name = kind = probe = rpm = deb = check = version = min_kernel = ""
        }
        {
            header = trim($0)
            if (header ~ /^\[\[[[:space:]]*component\.dependencies[[:space:]]*\]\]([[:space:]]*#.*)?$/) {
                emit(); clear_dep(); inside = 1; next
            }
            if (inside && header ~ /^\[/) {
                emit(); clear_dep(); inside = 0; next
            }
            if (!inside || $0 !~ /=/) next

            key = $0
            sub(/=.*/, "", key)
            key = trim(key)
            if (key == "name") name = assignment_string($0)
            else if (key == "kind") kind = assignment_string($0)
            else if (key == "probe") probe = assignment_string($0)
            else if (key == "check") check = assignment_string($0)
            else if (key == "version") version = assignment_string($0)
            else if (key == "min_kernel") min_kernel = assignment_string($0)
            else if (key == "packages") {
                rpm = inline_string($0, "rpm")
                deb = inline_string($0, "deb")
            }
        }
        END { emit() }
    ' "$manifest"
}

runtime_dependency_for_source_build() {
    local record="$1" component name
    IFS='|' read -r component name _ <<< "$record"
    if [[ "$component" == "sec-core" && "$name" == "systemd" ]]; then
        # Source installs do not install or manage the packaged systemd unit.
        return 0
    elif [[ "$component" == "sec-core" && "$name" == "nodejs" ]]; then
        # The manifest only probes runtime presence, while the OpenClaw plugin
        # source build requires the same Node version as copilot-shell.
        echo 'sec-core|node|language-runtime|node --version|nodejs|nodejs||>=20|'
    else
        echo "$record"
    fi
}

source_build_runtime_dependencies() {
    local component
    while IFS= read -r component; do
        case "$component" in
            sight)
                echo 'sight|node|language-runtime|node --version|nodejs|nodejs||>=20|'
                ;;
            memory)
                # `make install` bundles the OpenClaw adapter from source
                # (npm install + esbuild), so Node.js is a source-build
                # requirement even though the installed MCP server itself is
                # a Rust binary that never shells out to node.
                echo 'memory|node|language-runtime|node --version|nodejs|nodejs||>=20|'
                # npm is a separate package on several distributions and
                # `build-openclaw-plugin` calls it directly, so a node-only
                # toolchain must not pass the source-build preflight.
                echo 'memory|npm|language-runtime|npm --version|npm|npm|||'
                ;;
        esac
    done < <(runtime_install_components)
}

runtime_install_components() {
    local component
    while IFS= read -r component; do
        # ws-ckpt installs no files in user mode, so its daemon runtime
        # requirements must not block unrelated user-profile installs.
        if [[ "$INSTALL_MODE" == "user" && "$component" == "ws-ckpt" ]]; then
            continue
        fi
        echo "$component"
    done < <(active_components)
}

selected_runtime_dependencies() {
    local component manifest dependencies record
    while IFS= read -r component; do
        manifest="$(runtime_manifest_path "$component")" || return 1
        dependencies="$(runtime_dependencies_for_manifest "$component" "$manifest")" || return 1
        while IFS= read -r record; do
            [[ -n "$record" ]] || continue
            runtime_dependency_for_source_build "$record"
        done <<< "$dependencies"
    done < <(runtime_install_components)
    source_build_runtime_dependencies
}

runtime_command_path() {
    if [[ "$INSTALL_MODE" == "system" ]]; then
        echo "$RUNTIME_SYSTEM_PATH"
    elif [[ -n "${PATH:-}" ]]; then
        # Preserve user-local runtimes while keeping system package tools in
        # sbin discoverable from non-login environments.
        echo "$PATH:$RUNTIME_SYSTEM_PATH"
    else
        echo "$RUNTIME_SYSTEM_PATH"
    fi
}

normalize_runtime_version() {
    local version="$1"
    case "$version" in
        *.*.*) echo "$version" ;;
        *.*)   echo "${version}.0" ;;
        *)     echo "${version}.0.0" ;;
    esac
}

runtime_probe_succeeds() {
    local dependency="$1" probe="$2" version="$3"
    local command_path output required actual
    local -a argv=()
    read -r -a argv <<< "$probe"
    [[ ${#argv[@]} -gt 0 ]] || return 1
    command_path="$(runtime_command_path)"
    output="$(PATH="$command_path" "${argv[@]}" 2>/dev/null)" || {
        # The rpm package is gnupg2, whose binary name differs across distros.
        if [[ "$dependency" == "gnupg" ]]; then
            output="$(PATH="$command_path" gpg2 --version 2>/dev/null)" || return 1
        else
            return 1
        fi
    }
    [[ -z "$version" ]] && return 0
    [[ "$version" == '>='* ]] || return 0
    required="$(normalize_runtime_version "${version#>=}")"
    actual="$(extract_ver "$output" || true)"
    [[ -n "$actual" ]] && ver_gte "$actual" "$required"
}

runtime_package_present() {
    local rpm_package="$1" deb_package="$2"
    case "$PKG_BASE" in
        rpm) [[ -n "$rpm_package" ]] && rpm -q "$rpm_package" &>/dev/null ;;
        deb) [[ -n "$deb_package" ]] && dpkg -s "$deb_package" &>/dev/null ;;
        *)   return 1 ;;
    esac
}

runtime_kernel_satisfies() {
    local minimum="$1" current required
    [[ -z "$minimum" ]] && return 0
    current="$(uname -r 2>/dev/null | grep -oE '^[0-9]+\.[0-9]+(\.[0-9]+)?' || true)"
    [[ -n "$current" ]] || return 1
    current="$(normalize_runtime_version "$current")"
    required="$(normalize_runtime_version "$minimum")"
    ver_gte "$current" "$required"
}

runtime_btrfs_available() {
    local filesystems="${RUNTIME_PROC_FILESYSTEMS:-/proc/filesystems}"
    grep -qw btrfs "$filesystems" 2>/dev/null && return 0
    # Kernel module tools live in sbin on supported distributions. This host
    # capability must not depend on whether the invoking user's PATH includes it.
    PATH="$RUNTIME_SYSTEM_PATH" modprobe -n btrfs &>/dev/null
}

RUNTIME_DEP_DETAIL=""
runtime_dependency_present() {
    local name="$1" kind="$2" probe="$3" rpm_package="$4" deb_package="$5"
    local check="$6" version="$7" min_kernel="$8"
    RUNTIME_DEP_DETAIL=""

    if ! runtime_kernel_satisfies "$min_kernel"; then
        RUNTIME_DEP_DETAIL="requires kernel >= ${min_kernel}"
        return 1
    fi

    case "$kind" in
        system-package)
            if [[ -n "$probe" ]]; then
                runtime_probe_succeeds "$name" "$probe" "" && return 0
                RUNTIME_DEP_DETAIL="requires probe: ${probe}"
            else
                runtime_package_present "$rpm_package" "$deb_package" && return 0
                RUNTIME_DEP_DETAIL="required system package is not installed"
            fi
            ;;
        language-runtime)
            runtime_probe_succeeds "$name" "${probe:-$name --version}" "$version" && return 0
            RUNTIME_DEP_DETAIL="requires ${name}${version:+ ${version}}"
            ;;
        platform-capability)
            case "$check" in
                btf)
                    [[ -f /sys/kernel/btf/vmlinux ]] && return 0
                    RUNTIME_DEP_DETAIL="kernel BTF is unavailable (/sys/kernel/btf/vmlinux)"
                    ;;
                btrfs)
                    runtime_btrfs_available && return 0
                    RUNTIME_DEP_DETAIL="btrfs is neither registered nor loadable"
                    ;;
                *)
                    RUNTIME_DEP_DETAIL="unknown platform check: ${check:-<empty>}"
                    ;;
            esac
            ;;
        *)
            RUNTIME_DEP_DETAIL="unknown dependency kind: ${kind:-<empty>}"
            ;;
    esac
    return 1
}

collect_missing_runtime_dependencies() {
    local -n missing_ref="$1"
    local filter="${2:-all}"
    local dependencies component name kind probe rpm_package deb_package check version min_kernel
    missing_ref=()
    dependencies="$(selected_runtime_dependencies)" || {
        err "Failed to load runtime dependency manifests"
        return 1
    }
    while IFS='|' read -r component name kind probe rpm_package deb_package check version min_kernel; do
        [[ -n "$name" ]] || continue
        if [[ "$filter" == "platform-only" && "$kind" != "platform-capability" ]]; then
            continue
        fi
        if ! runtime_dependency_present \
                "$name" "$kind" "$probe" "$rpm_package" "$deb_package" \
                "$check" "$version" "$min_kernel"; then
            missing_ref+=("${component}|${name}|${kind}|${probe}|${rpm_package}|${deb_package}|${check}|${version}|${min_kernel}|${RUNTIME_DEP_DETAIL}")
        fi
    done <<< "$dependencies"
}

runtime_missing_packages() {
    local -n missing_ref="$1" packages_ref="$2"
    local record component name kind probe rpm_package deb_package check version min_kernel detail package
    local -A seen=()
    packages_ref=()
    for record in "${missing_ref[@]}"; do
        IFS='|' read -r component name kind probe rpm_package deb_package check version min_kernel detail <<< "$record"
        # Selecting a language-runtime distribution is a user decision; only
        # native system packages participate in the automatic transaction.
        [[ "$kind" == "system-package" ]] || continue
        if [[ "$PKG_BASE" == "rpm" ]]; then package="$rpm_package"; else package="$deb_package"; fi
        [[ -n "$package" && -z "${seen[$package]:-}" ]] || continue
        seen[$package]=1
        packages_ref+=("$package")
    done
}

runtime_missing_has_blocker() {
    local record kind
    for record in "$@"; do
        IFS='|' read -r _ _ kind _ <<< "$record"
        # Resolve manual runtimes and host capabilities before mutating packages.
        [[ "$kind" == "platform-capability" || "$kind" == "language-runtime" ]] && return 0
    done
    return 1
}

runtime_retry_command() {
    local retry=("$PROJECT_ROOT/scripts/build-all.sh") component
    while IFS= read -r component; do
        retry+=("--component" "$component")
    done < <(active_components)
    [[ "$INSTALL_MODE" == "system" ]] && retry+=("--system")
    $INSTALL_DEPS || retry+=("--ignore-deps")
    $DEPS_ONLY && retry+=("--deps-only")
    shell_args "${retry[@]}"
}

report_missing_runtime_dependencies() {
    local phase="$1" package_manager_known="$2"; shift 2
    local missing=("$@") packages=() record component name kind probe rpm_package deb_package
    local check version min_kernel detail manual_runtime=false

    if [[ "$phase" == "after-install" ]]; then
        err "Runtime dependencies are still missing after package installation; no components were installed:"
    else
        err "Missing runtime dependencies; no component files were installed:"
    fi
    for record in "${missing[@]}"; do
        IFS='|' read -r component name kind probe rpm_package deb_package check version min_kernel detail <<< "$record"
        echo "  ${component}: ${name} [${kind}]${detail:+ - ${detail}}"
        [[ "$kind" == "language-runtime" ]] && manual_runtime=true
    done

    runtime_missing_packages missing packages
    if [[ ${#packages[@]} -gt 0 ]]; then
        echo ""
        if [[ "$package_manager_known" == "true" ]]; then
            local install_command=() privilege_prefix="sudo "
            read -r -a install_command <<< "$PKG_INSTALL"
            [[ "$(id -u)" -eq 0 ]] && privilege_prefix=""
            info "Install them with:"
            echo "  ${privilege_prefix}$(shell_args "${install_command[@]}" "${packages[@]}")"
        else
            err "Cannot determine a supported deb/rpm package manager; install the listed dependencies manually."
        fi
    fi

    if $manual_runtime; then
        echo ""
        info "Install these language runtimes manually:"
        for record in "${missing[@]}"; do
            IFS='|' read -r component name kind probe rpm_package deb_package check version min_kernel detail <<< "$record"
            [[ "$kind" == "language-runtime" ]] || continue
            if [[ "$INSTALL_MODE" == "system" ]]; then
                echo "  ${component}: ${name} ${version} in $RUNTIME_SYSTEM_PATH"
            else
                echo "  ${component}: ${name} ${version} in PATH"
            fi
        done
    fi

    echo ""
    info "Then retry:"
    echo "  $(runtime_retry_command)"
    return 1
}

preflight_runtime_dependencies() {
    local filter="${1:-all}"
    if [[ "$filter" == "platform-only" ]]; then
        step "Runtime platform capability preflight"
    else
        step "Runtime dependency preflight"
    fi

    local missing=()
    local package_manager_known=true
    detect_runtime_package_manager || package_manager_known=false
    collect_missing_runtime_dependencies missing "$filter" || return 1
    if [[ ${#missing[@]} -eq 0 ]]; then
        if [[ "$filter" == "platform-only" ]]; then
            ok "Selected component platform capabilities are available"
        else
            ok "Selected component runtime dependencies are available"
        fi
        return 0
    fi

    # User installs and --ignore-deps never mutate system package state.
    if [[ "$INSTALL_MODE" != "system" ]] || ! $INSTALL_DEPS; then
        report_missing_runtime_dependencies "preflight" "$package_manager_known" "${missing[@]}"
        return 1
    fi

    if ! $package_manager_known || runtime_missing_has_blocker "${missing[@]}"; then
        report_missing_runtime_dependencies "preflight" "$package_manager_known" "${missing[@]}"
        return 1
    fi

    local packages=() install_command=()
    runtime_missing_packages missing packages
    read -r -a install_command <<< "$PKG_INSTALL"
    if [[ ${#install_command[@]} -eq 0 || ${#packages[@]} -eq 0 ]]; then
        report_missing_runtime_dependencies "preflight" false "${missing[@]}"
        return 1
    fi

    if [[ "$PKG_BASE" == "deb" ]]; then
        info "Refreshing APT package indexes ..."
        if ! as_root apt-get update; then
            err "Failed to refresh APT package indexes; no runtime packages were installed."
            return 1
        fi
    fi

    info "Installing runtime dependencies: ${packages[*]}"
    if ! as_root "${install_command[@]}" "${packages[@]}"; then
        warn "The package manager returned an error; re-checking every runtime dependency."
    fi

    missing=()
    collect_missing_runtime_dependencies missing "$filter" || return 1
    if [[ ${#missing[@]} -gt 0 ]]; then
        report_missing_runtime_dependencies "after-install" true "${missing[@]}"
        return 1
    fi

    ok "Runtime dependencies installed and verified"
}

install_sec_core() {
    step "Installing agent-sec-core"

    local staged build_dir
    staged="$(component_target_dir sec-core)"
    build_dir="$staged/build"

    local dir="$PROJECT_ROOT/src/agent-sec-core"
    [[ -d "$dir" ]] || die "Directory not found: $dir"

    if $DRY_RUN; then
        if [[ "$INSTALL_MODE" == "system" ]]; then
            echo "DRY-RUN: sudo env PATH=\$PATH UV_PYTHON_INSTALL_MIRROR=\${UV_PYTHON_INSTALL_MIRROR:-} make -C $dir install BUILD_DIR=$build_dir INSTALL_PROFILE=system"
        else
            echo "DRY-RUN: make -C $dir install BUILD_DIR=$build_dir INSTALL_PROFILE=user"
        fi
        ok "agent-sec-core install plan generated for $SEC_CORE_BIN_DIR and $SEC_CORE_LIB_DIR"
        return 0
    fi

    [[ -d "$build_dir" ]] || die "Build directory not found: $build_dir"
    [[ -f "$build_dir/linux-sandbox" ]] || die "Built linux-sandbox not found: $build_dir/linux-sandbox"
    [[ -d "$build_dir/cosh-extension" ]] || die "Built cosh extension not found: $build_dir/cosh-extension"
    [[ -d "$build_dir/openclaw-plugin" ]] || die "Built OpenClaw plugin not found: $build_dir/openclaw-plugin"
    [[ -d "$build_dir/hermes-plugin" ]] || die "Built hermes-plugin not found: $build_dir/hermes-plugin"
    [[ -d "$build_dir/skills" ]] || die "Built sec-core skills not found: $build_dir/skills"
    find "$build_dir/wheels" -maxdepth 1 -name 'agent_sec_cli-*.whl' -type f | grep -q . || \
        die "Built agent-sec-cli wheel not found under $build_dir/wheels"
    cmd_exists uv || die "uv not found; install dependencies first or run without --ignore-deps"

    _configure_uv_mirror

    if [[ "$INSTALL_MODE" == "system" ]]; then
        run_logged "make install (agent-sec-core)" \
            as_root env PATH="$PATH" \
                UV_PYTHON_INSTALL_MIRROR="${UV_PYTHON_INSTALL_MIRROR:-}" \
                make -C "$dir" install \
                BUILD_DIR="$build_dir" INSTALL_PROFILE=system
    else
        run_logged "make install (agent-sec-core)" \
            make -C "$dir" install \
                BUILD_DIR="$build_dir" INSTALL_PROFILE=user
    fi

    ok "agent-sec-core installed to $SEC_CORE_BIN_DIR and $SEC_CORE_LIB_DIR"
    if [[ "$INSTALL_MODE" != "system" ]]; then
        info "Make sure $SEC_CORE_BIN_DIR is in PATH before starting integrations."
    fi
}

install_cosh_ng() {
    step "Installing cosh-ng"
    local staged bin target
    staged="$(component_target_dir cosh-ng)/bin"

    if $DRY_RUN; then
        for bin in cosh-cli cosh-core cosh-gateway cosh-shell; do
            echo "DRY-RUN: install -p -m 0755 $staged/$bin $INSTALL_BIN_DIR/$bin"
        done
        ok "cosh-ng install plan generated for ${INSTALL_BIN_DIR}/"
        return 0
    fi

    for bin in cosh-cli cosh-core cosh-gateway cosh-shell; do
        [[ -f "$staged/$bin" ]] || die "Built cosh-ng binary not found: $staged/$bin"
        target="$INSTALL_BIN_DIR/$bin"
        if [[ "$INSTALL_MODE" == "system" ]]; then
            as_root install -d -m 0755 "$INSTALL_BIN_DIR"
            as_root install -p -m 0755 "$staged/$bin" "$target"
        else
            install -d -m 0755 "$INSTALL_BIN_DIR"
            install -p -m 0755 "$staged/$bin" "$target"
        fi
    done

    ok "cosh-ng installed to ${INSTALL_BIN_DIR}/{cosh-cli,cosh-core,cosh-gateway,cosh-shell}"
    info "Start cosh-ng with: ${INSTALL_BIN_DIR}/cosh-shell"
    info "The existing cosh launcher was not changed."
}

install_sight() {
    step "Installing agentsight"
    local dir="$PROJECT_ROOT/src/agentsight"
    local setcap_arg="SETCAP=0"
    if [[ "$INSTALL_MODE" == "system" ]]; then
        setcap_arg="SETCAP=0"
        stop_systemd_service_for_install agentsight.service
        stop_systemd_service_for_install agentsight-enforcer.service
    fi
    run_component_make_install "agentsight" "$dir" "$setcap_arg"
    if [[ "$INSTALL_MODE" == "system" ]]; then
        if cmd_exists setcap; then
            run_cmd as_root setcap cap_bpf,cap_perfmon=ep "$INSTALL_BIN_DIR/agentsight" || \
                warn "setcap failed; agentsight trace may need sudo"
        else
            warn "setcap not found; agentsight trace may need sudo"
        fi
        refresh_systemd_service agentsight-enforcer.service
        refresh_systemd_service agentsight.service
    else
        warn "agentsight user install skips systemd/setcap; trace/audit may need sudo or manual setcap."
    fi
    if $DRY_RUN; then
        ok "agentsight install plan generated for ${INSTALL_BIN_DIR}/agentsight"
    else
        ok "agentsight installed to ${INSTALL_BIN_DIR}/agentsight"
    fi
}

install_tokenless() {
    step "Installing tokenless"
    local dir="$PROJECT_ROOT/src/tokenless"
    run_component_make_install "tokenless" "$dir"
    if $DRY_RUN; then
        ok "tokenless install plan generated for ${INSTALL_BIN_DIR}/"
    else
        ok "tokenless installed to ${INSTALL_BIN_DIR}/"
    fi
}

install_wsckpt() {
    step "Installing ws-ckpt"
    local dir="$PROJECT_ROOT/src/ws-ckpt"
    if [[ "$INSTALL_MODE" == "system" ]]; then
        stop_systemd_service_for_install ws-ckpt.service
    fi
    run_component_make_install "ws-ckpt" "$dir"
    if [[ "$INSTALL_MODE" == "system" ]]; then
        refresh_systemd_service ws-ckpt.service
    else
        info "Skipping ws-ckpt systemd service in user mode; use --system for service management."
    fi
    if $DRY_RUN; then
        ok "ws-ckpt install plan generated for ${INSTALL_BIN_DIR}/"
    else
        ok "ws-ckpt installed to ${INSTALL_BIN_DIR}/"
    fi
}

install_agent_memory() {
    step "Installing agent-memory"
    local dir="$PROJECT_ROOT/src/agent-memory"
    run_component_make_install "agent-memory" "$dir"
    # agent-memory ships a per-user systemd template
    # (anolisa-memory@.service); intentionally NOT enabled by default so
    # users can opt-in with `systemctl --user enable anolisa-memory@$USER`.
    if $DRY_RUN; then
        ok "agent-memory install plan generated for ${INSTALL_BIN_DIR}/"
    else
        ok "agent-memory installed to ${INSTALL_BIN_DIR}/"
    fi
}

do_install() {
    step "Installing components (mode=${INSTALL_MODE})"
    if $DRY_RUN; then
        if $INSTALL_DEPS; then
            echo "DRY-RUN: preflight selected component runtime dependencies before install (host probes skipped)"
        else
            echo "DRY-RUN: skip runtime dependency verification (--ignore-deps)"
        fi
    elif $INSTALL_DEPS; then
        preflight_runtime_dependencies || return 1
    else
        warn "Skipping runtime dependency verification (--ignore-deps)"
    fi
    if want_component cosh;      then install_cosh;         fi
    if want_component skills;    then install_skills;       fi
    if want_component sec-core;  then install_sec_core;     fi
    if want_component cosh-ng;   then install_cosh_ng;      fi
    if want_component tokenless; then install_tokenless;    fi
    if want_component ws-ckpt;   then install_wsckpt;       fi
    if want_component memory;    then install_agent_memory; fi
    if want_component sight;     then install_sight;        fi
}

# ─── uninstall functions ───

uninstall_cosh() {
    step "Uninstalling copilot-shell"
    local dir="$PROJECT_ROOT/deprecated/copilot-shell"
    run_component_make_uninstall "copilot-shell" "$dir" || true
    if $DRY_RUN; then
        ok "copilot-shell uninstall plan generated"
    else
        ok "copilot-shell uninstalled"
    fi
}

uninstall_skills() {
    step "Uninstalling os-skills"
    local dir="$PROJECT_ROOT/src/os-skills"
    run_component_make_uninstall "os-skills" "$dir" || true
    if $DRY_RUN; then
        ok "os-skills uninstall plan generated"
    else
        ok "os-skills uninstalled"
    fi
}

uninstall_sec_core() {
    step "Uninstalling agent-sec-core"
    local dir="$PROJECT_ROOT/src/agent-sec-core"
    [[ -d "$dir" ]] || die "Directory not found: $dir"

    if $DRY_RUN; then
        if [[ "$INSTALL_MODE" == "system" ]]; then
            echo "DRY-RUN: sudo make -C $dir uninstall INSTALL_PROFILE=system"
        else
            echo "DRY-RUN: make -C $dir uninstall INSTALL_PROFILE=user"
        fi
        ok "agent-sec-core uninstall plan generated (mode=${INSTALL_MODE})"
        return 0
    fi

    if [[ "$INSTALL_MODE" == "system" ]]; then
        run_logged "make uninstall (agent-sec-core)" \
            as_root make -C "$dir" uninstall INSTALL_PROFILE=system || true
    else
        run_logged "make uninstall (agent-sec-core)" \
            make -C "$dir" uninstall INSTALL_PROFILE=user || true
    fi
    ok "agent-sec-core install removed (mode=${INSTALL_MODE})"
}

uninstall_cosh_ng() {
    step "Uninstalling cosh-ng"
    local bin

    stop_systemd_service 'cosh-gateway@*.service'
    stop_systemd_service 'cosh-gateway-acp@*.service'

    for bin in cosh-cli cosh-core cosh-gateway cosh-shell; do
        if $DRY_RUN; then
            echo "DRY-RUN: rm -f $INSTALL_BIN_DIR/$bin"
        elif [[ "$INSTALL_MODE" == "system" ]]; then
            as_root rm -f "$INSTALL_BIN_DIR/$bin"
        else
            rm -f "$INSTALL_BIN_DIR/$bin"
        fi
    done

    if $DRY_RUN; then
        ok "cosh-ng uninstall plan generated"
    else
        ok "cosh-ng uninstalled; the existing cosh launcher was not changed"
    fi
}

uninstall_sight() {
    step "Uninstalling agentsight"
    stop_systemd_service agentsight.service
    stop_systemd_service agentsight-enforcer.service
    local dir="$PROJECT_ROOT/src/agentsight"
    run_component_make_uninstall "agentsight" "$dir" || true
    if $DRY_RUN; then
        ok "agentsight uninstall plan generated"
    else
        ok "agentsight uninstalled"
    fi
}

uninstall_tokenless() {
    step "Uninstalling tokenless"
    local dir="$PROJECT_ROOT/src/tokenless"
    run_component_make_uninstall "tokenless" "$dir" || true
    if $DRY_RUN; then
        ok "tokenless and rtk uninstall plan generated"
    else
        ok "tokenless and rtk uninstalled"
    fi
}

uninstall_wsckpt() {
    step "Uninstalling ws-ckpt"
    stop_systemd_service ws-ckpt.service
    local dir="$PROJECT_ROOT/src/ws-ckpt"
    run_component_make_uninstall "ws-ckpt" "$dir" || true
    if $DRY_RUN; then
        ok "ws-ckpt uninstall plan generated"
    else
        ok "ws-ckpt uninstalled"
    fi
}

uninstall_agent_memory() {
    step "Uninstalling agent-memory"
    local dir="$PROJECT_ROOT/src/agent-memory"
    run_component_make_uninstall "agent-memory" "$dir" || true
    if $DRY_RUN; then
        ok "agent-memory uninstall plan generated"
    else
        ok "agent-memory uninstalled"
    fi
}

do_uninstall() {
    step "Uninstalling components"
    if want_component cosh;      then uninstall_cosh;         fi
    if want_component skills;    then uninstall_skills;       fi
    if want_component sec-core;  then uninstall_sec_core;     fi
    if want_component cosh-ng;   then uninstall_cosh_ng;      fi
    if want_component tokenless; then uninstall_tokenless;    fi
    if want_component ws-ckpt;   then uninstall_wsckpt;       fi
    if want_component memory;    then uninstall_agent_memory; fi
    if want_component sight;     then uninstall_sight;        fi

    if [[ -d "$INSTALL_EXTENSIONS_DIR" ]] && [[ -z "$(ls -A "$INSTALL_EXTENSIONS_DIR" 2>/dev/null)" ]]; then
        if $DRY_RUN; then
            echo "DRY-RUN: remove empty $INSTALL_EXTENSIONS_DIR"
        elif [[ "$INSTALL_MODE" == "system" ]]; then
            as_root rm -rf "$INSTALL_EXTENSIONS_DIR"
        else
            rm -rf "$INSTALL_EXTENSIONS_DIR"
        fi
        if $DRY_RUN; then
            info "Empty $INSTALL_EXTENSIONS_DIR would be removed"
        else
            info "Removed empty $INSTALL_EXTENSIONS_DIR"
        fi
    fi
}

print_output_summary() {
    step "Output"

    if $DRY_RUN; then
        info "Dry-run mode: target/ is not changed."
        return 0
    fi

    if [[ ! -d "$OUTPUT_DIR" ]]; then
        warn "No target/ directory found"
        return 0
    fi

    local total
    total=$(find "$OUTPUT_DIR" -type f 2>/dev/null | wc -l | tr -d ' ')
    info "$total files staged → $OUTPUT_DIR"

    local component_dir component_files
    for component_dir in "$OUTPUT_DIR"/*; do
        [[ -d "$component_dir" ]] || continue
        component_files=$(find "$component_dir" -type f 2>/dev/null | wc -l | tr -d ' ')
        info "  $(basename "$component_dir"): ${component_files} files → $component_dir"
    done
}

prompt_choice() {
    # `read -p` writes the prompt to stderr, so command substitution
    # ($(prompt_choice ...)) only captures the printf'd answer below.
    local prompt="$1" default="$2" answer
    read -r -p "$prompt [$default]: " answer
    printf '%s' "${answer:-$default}"
}

run_interactive_wizard() {
    [[ -t 0 ]] || die "--interactive requires a TTY. Use --non-interactive for automation."

    echo -e "${BOLD}ANOLISA interactive setup${NC}"
    echo "Choose the build flow. Press Enter to accept defaults."
    echo ""

    local choice comps confirm
    echo "1) Build and install"
    echo "2) Build only"
    echo "3) Install dependencies only"
    echo "4) Uninstall"
    choice="$(prompt_choice "Action" "1")"
    case "$choice" in
        1)
            DO_INSTALL=true
            DEPS_ONLY=false
            DO_UNINSTALL=false
            ;;
        2)
            DO_INSTALL=false
            DEPS_ONLY=false
            DO_UNINSTALL=false
            ;;
        3)
            DO_INSTALL=false
            DEPS_ONLY=true
            INSTALL_DEPS=true
            DO_UNINSTALL=false
            ;;
        4)
            DO_UNINSTALL=true
            ;;
        *) die "Invalid action choice: $choice" ;;
    esac

    echo ""
    echo "1) User install (~/.local, ~/.copilot-shell)"
    echo "2) System install (/usr/local/bin, /usr/share/anolisa)"
    choice="$(prompt_choice "Install mode" "$([[ "$INSTALL_MODE" == "system" ]] && echo 2 || echo 1)")"
    case "$choice" in
        1) INSTALL_MODE="user" ;;
        2) INSTALL_MODE="system" ;;
        *) die "Invalid install mode choice: $choice" ;;
    esac

    echo ""
    echo "1) Default components: $(join_by ", " "${DEFAULT_COMPONENTS[@]}")"
    echo "2) All components: $(join_by ", " "${ALL_COMPONENTS[@]}")"
    echo "3) Custom list"
    choice="$(prompt_choice "Components" "$([[ ${#COMPONENTS[@]} -gt 0 ]] && echo 3 || echo 1)")"
    case "$choice" in
        1) COMPONENTS=("${DEFAULT_COMPONENTS[@]}") ;;
        2) COMPONENTS=("${ALL_COMPONENTS[@]}") ;;
        3)
            comps="$(prompt_choice "Comma-separated components" "$(selected_components_text)")"
            COMPONENTS=()
            comps="${comps//,/ }"
            for comp in $comps; do
                if is_valid_component "$comp"; then
                    COMPONENTS+=("$comp")
                else
                    die "Unknown component: $comp"
                fi
            done
            [[ ${#COMPONENTS[@]} -gt 0 ]] || die "No components selected"
            ;;
        *) die "Invalid component choice: $choice" ;;
    esac

    if ! $DO_UNINSTALL && ! $DEPS_ONLY; then
        echo ""
        choice="$(prompt_choice "Install/check dependencies" "$($INSTALL_DEPS && echo y || echo n)")"
        case "$choice" in
            y|Y|yes|YES) INSTALL_DEPS=true ;;
            n|N|no|NO) INSTALL_DEPS=false ;;
            *) die "Invalid dependency choice: $choice" ;;
        esac
    fi

    echo ""
    ensure_user_mode
    step "Selected flow"
    if $DO_UNINSTALL; then
        info "Action: uninstall"
    elif $DEPS_ONLY; then
        info "Action: dependencies only"
    elif $DO_INSTALL; then
        info "Action: build and install"
    else
        info "Action: build only"
    fi
    info "Mode: ${INSTALL_MODE}"
    info "Components: $(selected_components_text)"
    info "Dependencies: $($INSTALL_DEPS && echo enabled || echo skipped)"
    info "Install: $($DO_INSTALL && echo enabled || echo skipped)"
    echo ""
    confirm="$(prompt_choice "Continue" "y")"
    case "$confirm" in
        y|Y|yes|YES) ;;
        *) ok "Cancelled"; exit 0 ;;
    esac
}

# ─── usage ───

usage() {
    cat <<EOF
$(echo -e "${BOLD}ANOLISA Build Script${NC}")

$(echo -e "${BOLD}Usage:${NC}")
  $0 [OPTIONS]

$(echo -e "${BOLD}Options:${NC}")
    --no-install            Skip installing built components
    --install-mode <mode>   Install mode: user or system (default: user)
    --usr, --system         Use system install mode
    --ignore-deps           Skip dependency setup and runtime verification (pre-provisioned hosts only)
    --deps-only             Install dependencies only, do not build
    --uninstall             Remove installed files (skips build; combine with --component to target one)
    --dry-run               Print actions without changing files or systemd state
    --interactive           Open a guided terminal flow before running
    --non-interactive       Explicit no-prompt mode; same as default, useful in CI to assert intent
    --all                   Include optional components such as cosh-ng and sight
    --component <name>      Build/uninstall specific component (can be repeated).
                                                    Valid names: cosh, skills, sec-core, cosh-ng, memory, sight, tokenless, ws-ckpt
                                                    Default (no --component): cosh, skills, sec-core, memory, tokenless, ws-ckpt
                                                    (cosh-ng and sight are optional; use --all or --component)
    -h, --help              Show this help

$(echo -e "${BOLD}Examples:${NC}")
    $0                                             # Install deps + build + install to user paths
    $0 --interactive                               # Guided terminal flow
    $0 --non-interactive                           # Explicit automation mode (same as default)
    $0 --install-mode user                         # Explicit user install mode
    $0 --no-install                                # Install deps + build (skip installation)
    $0 --ignore-deps                               # Build + install (pre-provisioned hosts only)
    $0 --deps-only                                 # Install deps only
    $0 --all                                       # Build + install default and optional components
    $0 --component cosh                            # Install deps + build + install copilot-shell
    $0 --component cosh-ng                         # Build + install cosh-ng without replacing cosh
    $0 --system --component cosh-ng                # Install cosh-ng binaries to /usr/local/bin
    $0 --no-install                                # Build target/ staging only
    $0 --component sec-core                          # Build + install sec-core to user paths
    $0 --system --component sec-core                 # Build + install sec-core to FHS system paths
    $0 --ignore-deps --component sec-core            # Build + install sec-core without dependency setup or verification
    $0 --uninstall                                 # Uninstall all default components
    $0 --uninstall --component cosh                # Uninstall copilot-shell only
    $0 --uninstall --component tokenless --component ws-ckpt
                                                     # Uninstall tokenless and ws-ckpt

$(echo -e "${BOLD}Components:${NC}")
  cosh     copilot-shell      Node.js / TypeScript AI terminal assistant       [default]
  skills   os-skills          Markdown skill definitions                         [default]
  sec-core agent-sec-core     Security CLI + sandbox + hooks                    [default]
  tokenless tokenless         Rust token compression library (cross-platform)   [default]
  ws-ckpt  ws-ckpt           Rust workspace checkpoint daemon                   [default]
  memory   agent-memory      Rust MCP filesystem memory server                   [default]
  cosh-ng  cosh-ng           Rust Agent-OS CLI, core, and interactive shell      [optional]
  sight    agentsight         eBPF observability/audit agent (Linux only)        [optional]

$(echo -e "${BOLD}What this script does:${NC}")
  1. Detects installed toolchains and queries system repositories for available versions
  2. Installs via system package manager (dnf/yum/apt) when repository versions meet requirements
  3. Falls back to upstream installers (nvm, rustup, uv) when system packages don't suffice
  4. Builds default components in order: cosh -> skills -> sec-core -> tokenless -> ws-ckpt -> memory
     (cosh-ng and sight are optional — add --all or --component to include them)
  5. Installs components to the selected profile layout
         - prefix: ${INSTALL_PREFIX}
         - binaries: ${INSTALL_BIN_DIR}
         - cosh extensions: ${INSTALL_EXTENSIONS_DIR}
         - docs (component-native): ${USER_DOC_DIR}
  6. Reports artifact locations at the end

$(echo -e "${BOLD}Note:${NC}")
  For agentsight eBPF probes, clang and libbpf headers must be installed via your
  system package manager. The script will detect and warn if they are missing.
EOF
    exit 0
}

# ─── argument parsing ───

parse_args() {
    while [[ $# -gt 0 ]]; do
        case "$1" in
            --no-install)
                DO_INSTALL=false
                shift
                ;;
            --ignore-deps)
                INSTALL_DEPS=false
                shift
                ;;
            --install-mode)
                [[ -n "${2:-}" ]] || die "--install-mode requires a value: user|system"
                case "$2" in
                    user|system) INSTALL_MODE="$2" ;;
                    *) die "Invalid --install-mode: $2. Valid: user, system" ;;
                esac
                shift 2
                ;;
            --usr|--system)
                INSTALL_MODE="system"
                shift
                ;;
            --dry-run)
                DRY_RUN=true
                shift
                ;;
            --interactive)
                INTERACTIVE=true
                shift
                ;;
            --non-interactive)
                NON_INTERACTIVE=true
                shift
                ;;
            --deps-only)
                DEPS_ONLY=true
                INSTALL_DEPS=true
                shift
                ;;
            --all)
                COMPONENTS=("${ALL_COMPONENTS[@]}")
                shift
                ;;
            --component)
                [[ -n "${2:-}" ]] || die "--component requires a value ($(join_by ", " "${ALL_COMPONENTS[@]}"))"
                if is_valid_component "$2"; then
                    COMPONENTS+=("$2")
                else
                    die "Unknown component: $2. Valid: $(join_by ", " "${ALL_COMPONENTS[@]}")"
                fi
                shift 2
                ;;
            --uninstall)
                DO_UNINSTALL=true
                shift
                ;;
            -h|--help)
                usage
                ;;
            *)
                die "Unknown option: $1. Use --help for usage."
                ;;
        esac
    done

    if $DEPS_ONLY; then
        INSTALL_DEPS=true
    fi
    if $INTERACTIVE && $NON_INTERACTIVE; then
        die "--interactive and --non-interactive cannot be used together"
    fi
}

# ─── main ───

main() {
    parse_args "$@"
    if $INTERACTIVE; then
        run_interactive_wizard
    else
        ensure_user_mode
    fi

    echo -e "${BOLD}ANOLISA Build Script${NC}"
    echo -e "${DIM}Project root: ${PROJECT_ROOT}${NC}"
    echo -e "${DIM}Mode: ${INSTALL_MODE}${NC}"

    if $DO_UNINSTALL; then
        do_uninstall
        echo ""
        ok "Done"
        exit 0
    fi

    if $INSTALL_DEPS; then
        do_install_deps
    fi

    if $DEPS_ONLY; then
        echo ""
        info "Deps-only mode, skipping build."
        exit 0
    fi

    do_build
    print_output_summary

    if $DO_INSTALL; then
        do_install
    fi

    echo ""
    ok "Done"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
    main "$@"
fi
