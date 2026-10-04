#!/bin/sh
# install.sh — Register the Tokenless bundle for DeepSeek Harness (dsh) profiles.
#
# Unlike the OpenCode/Qoder/Codex adapters, the dsh adapter
# (src/tokenless/adapters/tokenless/dsh/) is a native npm bundle
# (@anolisa/dsh-tokenless: dist/index.js + cordis.patch.yml) with no bundled
# scripts/install.sh — registration goes exclusively through the anolisa CLI.
# Facts mirrored from the repo:
#   - `anolisa adapter enable tokenless dsh --profile <p>` requires at least
#     one --profile, repeatable; each enable treats the supplied profiles as
#     the complete desired set (omitted ones lose the bundle)
#     (src/tokenless/README.md, docs/.../framework-integration.md#deepseek-harness-native-processing)
#   - Node.js >= 22 is required (adapter package.json.in engines.node)
#   - profile names must match `dsh --profile <profile>`
#   - verification via `anolisa adapter status tokenless`
#
# Usage:
#   sh scripts/install.sh --profile <name> [--profile <name> ...] [--skip-tokenless]
#
# Options:
#   --skip-tokenless  Run the environment preflight only; skip registration.

set -eu

PROFILES=""
SKIP_TOKENLESS=0

usage() {
    echo "usage: $0 --profile <name> [--profile <name> ...] [--skip-tokenless]" >&2
}

while [ $# -gt 0 ]; do
    case "$1" in
        --profile)
            if [ $# -lt 2 ] || [ -z "$2" ]; then
                echo "install-dsh: --profile requires a non-empty value" >&2
                exit 2
            fi
            PROFILES="$PROFILES $2"
            shift 2
            ;;
        --skip-tokenless) SKIP_TOKENLESS=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *)
            echo "install-dsh: unknown argument: $1" >&2
            usage
            exit 2
            ;;
    esac
done

info() { echo "[install-dsh] $*"; }
err()  { echo "[install-dsh] ERROR: $*" >&2; }

# --- Preflight: required commands -------------------------------------------
for cmd in dsh anolisa tokenless node; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        err "$cmd is required (dsh CLI / registration / compression core / bundle runtime)."
        case "$cmd" in
            dsh)      err "The repo documents no dsh installer; install DeepSeek Harness from its release channel." ;;
            anolisa)  err "Install Tokenless first (see the install-tokenless skill)." ;;
            tokenless) err "Install the Tokenless CLI, then re-run this script." ;;
        esac
        exit 1
    fi
done

# --- Node.js >= 22 (adapter engines.node) -----------------------------------
node_major="$(node --version 2>/dev/null | sed 's/^v//' | cut -d. -f1)"
case "$node_major" in
    ''|*[!0-9]*)
        err "cannot determine Node.js version (node --version output unexpected)."
        exit 1
        ;;
esac
if [ "$node_major" -lt 22 ]; then
    err "Node.js >= 22 is required (found: $(node --version))."
    exit 1
fi
info "preflight ok: dsh, anolisa, tokenless, node $(node --version)."

# --- Collect profiles --------------------------------------------------------
# Strip the leading space built up during argument parsing.
PROFILES="${PROFILES# }"
if [ "$SKIP_TOKENLESS" = 1 ]; then
    info "Skipping Tokenless registration (--skip-tokenless)."
    info "Enable later with: anolisa adapter enable tokenless dsh --profile <name>"
    info "Environment preflight only; nothing was registered."
    exit 0
fi

if [ -z "$PROFILES" ]; then
    err "at least one --profile is required (names must match 'dsh --profile <name>')."
    err "Pass every profile that should keep Tokenless in this single command."
    exit 2
fi

# --- Register: one enable command carrying the complete profile set ----------
ENABLE_ARGS=""
# shellcheck disable=SC2086 # Word-splitting of PROFILES is intended.
for profile in $PROFILES; do
    ENABLE_ARGS="$ENABLE_ARGS --profile $profile"
done
info "Enabling the Tokenless dsh adapter for profiles:$PROFILES"
# shellcheck disable=SC2086 # One --profile flag per word is intended.
anolisa adapter enable tokenless dsh $ENABLE_ARGS

# --- Verify -----------------------------------------------------------------
if ! anolisa adapter status tokenless >/dev/null 2>&1; then
    err "anolisa adapter status tokenless failed after enable."
    # shellcheck disable=SC2086 # One --profile flag per word is intended.
    err "Retry: anolisa --verbose adapter enable tokenless dsh$ENABLE_ARGS"
    exit 1
fi
info "Tokenless dsh bundle registered for:$PROFILES"
info "Restart each profile (dsh --profile <name>), run a compressible tool, then check 'tokenless stats list'."
