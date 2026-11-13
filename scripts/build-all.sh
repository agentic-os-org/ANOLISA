#!/usr/bin/env bash
# Quick start (Linux, Bash 4.4+; run from a checkout as your normal build user):
#   ./scripts/build-all.sh                         # prepare, build, install system files
#   ./scripts/build-all.sh --all --no-install      # prepare and compile the complete stack
#   ./scripts/build-all.sh --component tokenless --no-install
#   ./scripts/build-all.sh --all --user --supported-only  # install the user-compatible subset
#   ./scripts/build-all.sh --component tokenless --user --install-system-deps
#   ./scripts/build-all.sh --all --deps-only       # prepare build tools without compiling
#   ./scripts/build-all.sh --all --check-deps      # inspect requirements without changes
#   ./scripts/build-all.sh --all --prefix /usr --destdir "$PWD/target/pkgroot"
#
# Default stack: cosh-ng, skillfs, ws-ckpt, tokenless, agentsight,
# agent-sec-core V2 and aw. Matching providers/<component>/ packages are added
# automatically; the provider package name comes from its Cargo.toml.
# AW builds retain native sec-core and Tokenless extensions when cosh-ng is selected.
# System files use PREFIX=/usr/local and AWPRJ=anolisa. sudo is used only for system
# packages and direct system installation; DESTDIR staging needs no sudo.
# User mode never installs system packages unless --install-system-deps is set.
# Installation copies files without activating services or registering adapters.
# Only system python3 is needed for build helpers; no uv, virtualenv, or V1 CLI.
# Existing component build directories and dependency caches are preserved.
set -euo pipefail

# Bash 4.4 permits empty array expansion under nounset during dependency planning.
if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then
    printf 'build-all.sh requires Bash 4.4 or newer.\n' >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DEFAULT_COMPONENTS=(cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw)
ALL_COMPONENTS=("${DEFAULT_COMPONENTS[@]}" agent-memory os-skills)
COMPONENTS=()
INSTALL_DEPS=true
INSTALL_SYSTEM_DEPS=auto
SUPPORTED_ONLY=false
DEPS_ONLY=false
CHECK_DEPS_ONLY=false
DO_INSTALL=true
DRY_RUN=false
INTERACTIVE=false
NON_INTERACTIVE=false
INSTALL_MODE=system
PREFIX=""
DESTDIR=""
BUILD_JOBS=""
OUTPUT_DIR="${BUILD_OUTPUT_DIR:-$PROJECT_ROOT/target/build-all}"
LOG_FILE="$OUTPUT_DIR/build.log"
RUNTIME_SYSTEM_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
AWPRJ="${AWPRJ:-anolisa}"
export AWPRJ

info() { printf '[info] %s\n' "$*"; }
ok() { printf '[ok] %s\n' "$*"; }
warn() { printf '[warn] %s\n' "$*" >&2; }
err() { printf '[error] %s\n' "$*" >&2; }
step() { printf '\n==> %s\n' "$*"; }
die() { err "$@"; exit 1; }
cmd_exists() { command -v "$1" >/dev/null 2>&1; }
extract_ver() { grep -oE '[0-9]+\.[0-9]+\.[0-9]+' <<< "$1" | head -1; }
ver_gte() { printf '%s\n%s\n' "$2" "$1" | sort -V -C; }
shell_args() {
    local argument separator=""
    for argument in "$@"; do
        printf '%s%q' "$separator" "$argument"
        separator=' '
    done
}
run_cmd() {
    if $DRY_RUN; then
        printf 'DRY-RUN: '; shell_args "$@"; printf '\n'
    else "$@"; fi
}
as_root() {
    if [[ "$INSTALL_MODE" == user && "$INSTALL_SYSTEM_DEPS" != true ]]; then
        die 'User mode does not allow privileged commands; --install-system-deps explicitly allows native build package installation'
    fi
    if $DRY_RUN; then
        printf 'DRY-RUN: '; shell_args sudo "$@"; printf '\n'
    elif [[ "$(id -u)" == 0 ]]; then "$@"
    else
        cmd_exists sudo || die 'sudo is required for system package or file installation'
        sudo "$@"
    fi
}

normalize_component() {
    local component="$1" known
    case "$1" in
        sec-core) component=agent-sec-core ;;
        sight) component=agentsight ;;
        memory) component=agent-memory ;;
        skills) component=os-skills ;;
        cosh) die 'cosh is deprecated; select cosh-ng instead' ;;
    esac
    for known in "${ALL_COMPONENTS[@]}"; do
        if [[ "$known" == "$component" ]]; then printf '%s\n' "$component"; return; fi
    done
    case "$component" in
        aw-provider-*)
            if provider_directory "$component" >/dev/null; then printf '%s\n' "$component"; return; fi ;;
    esac
    die "Unknown component: $component. Use --help for supported components."
}
active_components() {
    if (( ${#COMPONENTS[@]} )); then printf '%s\n' "${COMPONENTS[@]}"
    else printf '%s\n' "${DEFAULT_COMPONENTS[@]}"; fi
}
select_components() {
    local component normalized
    local -A seen=()
    local -a selected=()
    if (( ${#COMPONENTS[@]} == 0 )); then COMPONENTS=("${DEFAULT_COMPONENTS[@]}"); fi
    for component in "${COMPONENTS[@]}"; do
        normalized="$(normalize_component "$component")" || return 1
        if [[ -z "${seen[$normalized]:-}" ]]; then
            selected+=("$normalized"); seen[$normalized]=1
        fi
    done
    COMPONENTS=("${selected[@]}")
}

expand_companion_providers() {
    local component provider
    local -A seen=()
    local -a expanded=("${COMPONENTS[@]}")
    for component in "${COMPONENTS[@]}"; do seen[$component]=1; done
    for component in "${COMPONENTS[@]}"; do
        provider="$(companion_provider "$component")" || return 1
        if [[ -n "$provider" && -z "${seen[$provider]:-}" ]]; then
            expanded+=("$provider"); seen[$provider]=1
        fi
    done
    COMPONENTS=("${expanded[@]}")
}

component_supports_user_install() {
    case "$1" in skillfs|ws-ckpt|agentsight|agent-sec-core) return 1 ;; esac
}

select_user_components() {
    if [[ "$INSTALL_MODE" != user ]]; then
        if $SUPPORTED_ONLY; then die '--supported-only requires --user'; fi
        return 0
    fi
    local component
    local -a supported=() unsupported=()
    for component in "${COMPONENTS[@]}"; do
        if component_supports_user_install "$component"; then supported+=("$component")
        else unsupported+=("$component"); fi
    done
    if $SUPPORTED_ONLY; then
        if (( ${#unsupported[@]} )); then
            warn "Skipped components without user installation support: ${unsupported[*]}"
        fi
        (( ${#supported[@]} )) || die 'None of the selected components support user installation'
        COMPONENTS=("${supported[@]}")
    elif { $DO_INSTALL || [[ -n "$DESTDIR" ]]; } && ! $DEPS_ONLY && ! $CHECK_DEPS_ONLY && (( ${#unsupported[@]} )); then
        err "Components without user installation support: ${unsupported[*]}"
        die 'Select --system, use --no-install to compile the full selection, or add --supported-only to select the user-compatible subset'
    fi
}

# shellcheck source=lib/build-env.sh
source "$SCRIPT_DIR/lib/build-env.sh"
# shellcheck source=lib/build-runtime.sh
source "$SCRIPT_DIR/lib/build-runtime.sh"

usage() {
    cat <<'EOF'
ANOLISA native Linux build and file installation

Usage: ./scripts/build-all.sh [OPTIONS]
  --all                  Select default components and their matching providers
  --component NAME       Select one component; repeat to select several
  --no-install           Prepare dependencies and build without installing files
  --install              Prepare, build and install files (default)
  --deps-only            Prepare build dependencies without building or installing
  --check-deps           Check build dependencies without changing the host
  --ignore-deps          Skip build dependency setup and runtime verification
  --install-system-deps  Allow installing native build packages in user mode
  --destdir PATH         Build and stage files under an absolute package root
  --prefix PATH          Target prefix (system: /usr/local; user: ~/.local)
  --system               System installation profile (default)
  --user                 User profile, for components that support it
  --supported-only       With --user, select only user-installable components
  --install-mode MODE    Explicit user or system profile
  --jobs N               Maximum Cargo build jobs per component
  --dry-run              Print the plan without downloads or filesystem changes
  --interactive          Choose the action and components in a terminal
  --non-interactive      Explicit automation mode (no prompts, also the default)
  -h, --help             Show this help

Default components:
  cosh-ng skillfs ws-ckpt tokenless agentsight agent-sec-core aw
Matching providers/<component>/ packages are added after component selection.
Their aw-provider-* target names are read from Cargo.toml, without invoking Cargo.
Explicit --component aw-provider-NAME selection remains available on its own.
Selecting aw skips component plugin bundles, retaining core, skills and providers.
With aw and cosh-ng selected, selected sec-core and Tokenless backends retain their Cosh extensions.
Additional components: agent-memory, os-skills
Aliases: sec-core=agent-sec-core, sight=agentsight,
         memory=agent-memory, skills=os-skills

Download source environment variables (HTTPS):
  NODE_DIST_MIRROR        Node distribution base (default: https://nodejs.org/dist)
  RUSTUP_INIT_SCRIPT_URL  rustup bootstrap URL (default: https://sh.rustup.rs)

The build uses agent-sec-core V2 only. System python3 supports build helpers;
uv, a fixed Python runtime and the V1 Python CLI are not prepared.
Each component owns compilation and dependency downloads. Native installation
copies prebuilt files; service activation and adapter registration are separate.
DESTDIR stages files even with --no-install and skips host runtime preflight.
--ignore-deps assumes a provisioned host; component builds can still download
Cargo/npm dependencies and pinned third-party sources.
User mode never installs system packages by default; missing native packages
are reported before toolchain downloads. --install-system-deps explicitly
permits package installation without changing the user file layout.
--supported-only prints skipped components and filters the selection before
preparing dependencies. It also applies to build-only and dependency checks.
Legacy --uninstall is rejected before any operation. Removal of existing files,
services and persistent data requires the component-specific removal procedure.
EOF
}
parse_args() {
    while (( $# )); do
        case "$1" in
            --all) COMPONENTS+=("${DEFAULT_COMPONENTS[@]}"); shift ;;
            --component)
                [[ -n "${2:-}" && "$2" != --* ]] || die '--component requires a name'
                COMPONENTS+=("$2"); shift 2 ;;
            --component=*) COMPONENTS+=("${1#*=}"); shift ;;
            --no-install) DO_INSTALL=false; shift ;;
            --install) DO_INSTALL=true; shift ;;
            --ignore-deps) INSTALL_DEPS=false; shift ;;
            --install-system-deps) INSTALL_SYSTEM_DEPS=true; shift ;;
            --supported-only) SUPPORTED_ONLY=true; shift ;;
            --deps-only) DEPS_ONLY=true; shift ;;
            --check-deps) CHECK_DEPS_ONLY=true; shift ;;
            --dry-run) DRY_RUN=true; shift ;;
            --system) INSTALL_MODE=system; shift ;;
            --usr) die '--usr is no longer supported; use --system for installation scope and --prefix /usr for package paths' ;;
            --user) INSTALL_MODE=user; shift ;;
            --install-mode|--prefix|--destdir|--jobs)
                [[ -n "${2:-}" && "$2" != --* ]] || die "$1 requires a value"
                case "$1" in
                    --install-mode) INSTALL_MODE="$2" ;;
                    --prefix) PREFIX="$2" ;;
                    --destdir) DESTDIR="$2" ;;
                    --jobs) BUILD_JOBS="$2" ;;
                esac
                shift 2 ;;
            --interactive) INTERACTIVE=true; shift ;;
            --non-interactive) NON_INTERACTIVE=true; shift ;;
            --uninstall) die 'Unified --uninstall is no longer supported; use the component-specific removal procedure for its installation layout and persistent data' ;;
            -h|--help) usage; exit 0 ;;
            *) die "Unknown option: $1. Use --help for usage." ;;
        esac
    done
    if $INTERACTIVE && $NON_INTERACTIVE; then die '--interactive and --non-interactive cannot be combined'; fi
    if $DEPS_ONLY && $CHECK_DEPS_ONLY; then die '--deps-only and --check-deps cannot be combined'; fi
    if { $DEPS_ONLY || $CHECK_DEPS_ONLY; } && ! $INSTALL_DEPS; then
        die '--ignore-deps cannot be combined with --deps-only or --check-deps'
    fi
}
validate_path() {
    local label="$1" path="$2"
    [[ "$path" == /* ]] || die "$label must be an absolute path"
    case "/$path/" in */../*|*/./*) die "$label must not contain dot segments" ;; esac
    [[ "$path" != *$'\n'* && "$path" != *'$'* && "$path" != *'#'* ]] || die "Invalid $label"
}
validate_options() {
    [[ "$(uname -s)" == Linux ]] || die 'The complete native stack requires Linux'
    case "$INSTALL_MODE" in user|system) ;; *) die 'Invalid install mode; use user or system' ;; esac
    select_user_components
    expand_companion_providers
    if [[ -z "$PREFIX" ]]; then
        if [[ "$INSTALL_MODE" == system ]]; then PREFIX=/usr/local; else PREFIX="$HOME/.local"; fi
    fi
    validate_path PREFIX "$PREFIX"
    if [[ -n "$DESTDIR" ]]; then
        validate_path DESTDIR "$DESTDIR"
        [[ -n "${DESTDIR//\//}" ]] || die 'DESTDIR must be a package root other than /'
        [[ ! -L "$DESTDIR" ]] || die 'DESTDIR must not be a symlink'
    fi
    if [[ -n "$BUILD_JOBS" ]]; then
        [[ "$BUILD_JOBS" =~ ^[1-9][0-9]*$ ]] || die '--jobs requires a positive integer'
        export CARGO_BUILD_JOBS="$BUILD_JOBS"
    fi
    [[ "$AWPRJ" =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]*$ ]] || die 'AWPRJ must be a single namespace segment'
    local component
    for component in "${COMPONENTS[@]}"; do
        [[ -d "$(component_dir "$component")" ]] || die "Missing component directory: $(component_dir "$component")"
    done
}
run_interactive_wizard() {
    [[ -t 0 ]] || die '--interactive requires a terminal; use explicit options for automation'
    local action selection component
    printf 'Action: 1) build and install  2) build only  3) prepare dependencies  4) check dependencies\n'
    read -r -p 'Choose [1]: ' action
    case "${action:-1}" in
        1) DO_INSTALL=true ;; 2) DO_INSTALL=false ;;
        3) DEPS_ONLY=true ;; 4) CHECK_DEPS_ONLY=true ;; *) die 'Invalid action' ;;
    esac
    read -r -p 'Components (space-separated, Enter for the complete stack): ' selection
    if [[ -n "$selection" ]]; then
        COMPONENTS=()
        for component in ${selection//,/ }; do COMPONENTS+=("$component"); done
    fi
}
run_component_make() {
    local component="$1" phase="$2" target="$2" directory
    directory="$(component_dir "$component")"
    if skip_component_plugins "$component"; then
        case "$component:$phase" in
            agentsight:build) target=build-all ;;
            *:build) target=build-core ;;
            agent-sec-core:install|ws-ckpt:install) target=install-core-and-skills ;;
            *:install) target=install-core ;;
        esac
        # AW has no cosh-ng adapter. Keep the extensions for its native hooks.
        if component_selected cosh-ng; then
            case "$component:$phase" in
                agent-sec-core:build|agent-sec-core:install|tokenless:install) target+=-with-cosh ;;
            esac
        fi
    fi
    local -a command=(make --no-print-directory -C "$directory" "$target"
        "INSTALL_PROFILE=$INSTALL_MODE" "PREFIX=$PREFIX" "AWPRJ=$AWPRJ")
    case "$component" in
        agent-sec-core)
            if [[ -n "${BUILD_DIR:-}" ]]; then
                # Both phases use the same -C directory; keep relative staging paths.
                # Pass the output selection through sudo's environment filter.
                command+=("BUILD_DIR=$BUILD_DIR")
            fi
            ;;
        aw|aw-provider-*)
            if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
                # sudo filters the environment; retain the native artifact path.
                local cargo_target_dir="$CARGO_TARGET_DIR"
                [[ "$cargo_target_dir" == /* ]] || cargo_target_dir="$directory/$cargo_target_dir"
                command+=("CARGO_TARGET_DIR=$cargo_target_dir")
            fi
            ;;
    esac
    if [[ "$phase" != build ]]; then command+=("DESTDIR=$DESTDIR"); fi
    if [[ "$phase" == build ]]; then
        # Prefer directory pins over the fallback without changing rustup defaults.
        if [[ -n "${BUILD_FALLBACK_TOOLCHAIN:-}" ]]; then
            (
                export ANOLISA_BUILD_TOOLCHAIN="$BUILD_FALLBACK_TOOLCHAIN"
                export PATH="$SCRIPT_DIR/lib/build-toolchain:$PATH"
                run_cmd "${command[@]}"
            )
        else
            run_cmd "${command[@]}"
        fi
    elif [[ "$INSTALL_MODE" == system && -z "$DESTDIR" ]]; then as_root "${command[@]}"
    else run_cmd "${command[@]}"; fi
}
do_build() {
    step 'Build selected components'
    if [[ -n "${BUILD_FALLBACK_TOOLCHAIN:-}" ]]; then
        info "Rust fallback: $BUILD_FALLBACK_TOOLCHAIN (component pins take priority)"
    fi
    if ! $DRY_RUN; then mkdir -p "$OUTPUT_DIR"; : > "$LOG_FILE"; fi
    local component
    for component in "${COMPONENTS[@]}"; do
        info "Building $component"
        if $DRY_RUN; then run_component_make "$component" build
        else run_component_make "$component" build 2>&1 | tee -a "$LOG_FILE"; fi
    done
}
do_install() {
    # The build host need not provide the target root's runtime/kernel capabilities.
    if [[ -z "$DESTDIR" ]] && $INSTALL_DEPS; then preflight_runtime_dependencies; fi
    local component
    for component in "${COMPONENTS[@]}"; do
        info "Installing $component${DESTDIR:+ into $DESTDIR}"
        run_component_make "$component" install
    done
    info 'Service activation and adapter registration are separate component operations.'
}
print_output_summary() {
    if [[ -n "$DESTDIR" ]]; then info "Package root: $DESTDIR"
    else info 'Build artifacts remain in each component target directory.'; fi
    if ! $DRY_RUN; then info "Build log: $LOG_FILE"; fi
}
main() {
    parse_args "$@"
    if $INTERACTIVE; then run_interactive_wizard; fi
    select_components
    validate_options
    info "Components: ${COMPONENTS[*]}"
    info "Layout: $INSTALL_MODE, PREFIX=$PREFIX, AWPRJ=$AWPRJ${DESTDIR:+, DESTDIR=$DESTDIR}"
    if $CHECK_DEPS_ONLY; then check_build_environment; return; fi
    if $INSTALL_DEPS; then prepare_build_environment; fi
    if $DEPS_ONLY; then ok 'Build environment prepared'; return; fi
    do_build
    if $DO_INSTALL || [[ -n "$DESTDIR" ]]; then do_install; fi
    print_output_summary
    ok 'Done'
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then main "$@"; fi
