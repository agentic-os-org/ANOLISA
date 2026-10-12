#!/bin/bash
# =============================================================================
# bump-version.sh
#
# One-shot version bump script for agent-sec-core V2 and shared hooks.
#
# Usage:
#   ./bump-version.sh <new-version>
#   ./bump-version.sh 0.4.0
#
# What it updates:
#   1. v2/Cargo.toml                         (workspace.package.version)
#   2. openclaw-plugin/package.json          ("version" field)
#   3. openclaw-plugin/openclaw.plugin.json  ("version" field)
#   4. cosh-extension/cosh-extension.json    ("version" field)
#   5. hermes-plugin/src/plugin.yaml          (version field)
#   6. .anolisa/component.toml                (component.version)
#   7. codex-plugin/hooks-plugin/.codex-plugin/plugin.json ("version" field)
#   8. qoder-plugin/.qoder-plugin/plugin.json ("version" field)
#   9. qwen-code-extension/qwen-extension.json ("version" field)
#  10. v2/Cargo.lock and openclaw-plugin/package-lock.json
#
# Manual update required (not automated):
#   - agent-sec-core.spec.v2.in (%changelog entry)
#   - CHANGELOG.md (new version header with actual notes)
#
# =============================================================================

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log()  { echo -e "${CYAN}[INFO]${NC} $*"; }
ok()   { echo -e "${GREEN}[ OK ]${NC} $*"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $*"; }
err()  { echo -e "${RED}[ERROR]${NC} $*" >&2; }

replace_in_file() {
    local expression="$1"
    local file="$2"

    if sed --version >/dev/null 2>&1; then
        sed -i "$expression" "$file"
    else
        sed -i '' "$expression" "$file"
    fi
}

if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.4.0"
    exit 1
fi

NEW_VERSION="$1"

if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    err "Invalid version format: '$NEW_VERSION'. Expected semver (e.g., 0.4.0)"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
V2_CARGO_TOML="$PROJECT_ROOT/v2/Cargo.toml"

log "Project root: $PROJECT_ROOT"
log "New version:  $NEW_VERSION"
echo ""

if [[ ! -f "$V2_CARGO_TOML" ]]; then
    err "V2 Cargo.toml not found at: $V2_CARGO_TOML"
    exit 1
fi

OLD_VERSION="$(awk '
    $0 == "[workspace.package]" { in_section = 1; next }
    in_section && /^\[/ { exit }
    in_section && /^version = / {
        value = $0
        sub(/^version = "/, "", value)
        sub(/"$/, "", value)
        print value
        exit
    }
' "$V2_CARGO_TOML")"
if [[ -z "$OLD_VERSION" ]]; then
    err "Cannot detect current version from v2/Cargo.toml"
    exit 1
fi

if [[ "$OLD_VERSION" == "$NEW_VERSION" ]]; then
    warn "Current version is already $NEW_VERSION, nothing to do."
    exit 0
fi

log "Current version: $OLD_VERSION"
log "Bumping: $OLD_VERSION → $NEW_VERSION"
echo ""

bump_file() {
    local file="$1"
    local pattern="$2"
    local replacement="$3"
    local label="$4"

    if [[ ! -f "$file" ]]; then
        warn "File not found, skipping: $file"
        return
    fi

    if grep -q "$pattern" "$file"; then
        replace_in_file "s|$pattern|$replacement|" "$file"
        ok "$label"
    else
        warn "Pattern not found in $file: $pattern"
    fi
}

bump_file "$V2_CARGO_TOML" \
    "^version = \"$OLD_VERSION\"" \
    "version = \"$NEW_VERSION\"" \
    "v2/Cargo.toml"

bump_file "$PROJECT_ROOT/openclaw-plugin/package.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "openclaw-plugin/package.json"

bump_file "$PROJECT_ROOT/openclaw-plugin/openclaw.plugin.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "openclaw-plugin/openclaw.plugin.json"

bump_file "$PROJECT_ROOT/cosh-extension/cosh-extension.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "cosh-extension/cosh-extension.json"

bump_file "$PROJECT_ROOT/hermes-plugin/src/plugin.yaml" \
    "^version: $OLD_VERSION" \
    "version: $NEW_VERSION" \
    "hermes-plugin/src/plugin.yaml"

bump_file "$PROJECT_ROOT/.anolisa/component.toml" \
    "^version = \"$OLD_VERSION\"" \
    "version = \"$NEW_VERSION\"" \
    ".anolisa/component.toml"

bump_file "$PROJECT_ROOT/codex-plugin/hooks-plugin/.codex-plugin/plugin.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "codex-plugin/hooks-plugin/.codex-plugin/plugin.json"

bump_file "$PROJECT_ROOT/qoder-plugin/.qoder-plugin/plugin.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "qoder-plugin/.qoder-plugin/plugin.json"

bump_file "$PROJECT_ROOT/qwen-code-extension/qwen-extension.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "qwen-code-extension/qwen-extension.json"

log "Updating lock files..."

V2_CARGO_LOCK="$PROJECT_ROOT/v2/Cargo.lock"
if [[ -f "$V2_CARGO_LOCK" ]]; then
    lock_tmp="$(mktemp "$V2_CARGO_LOCK.XXXXXX")"
    awk -v version="$NEW_VERSION" '
        /^\[\[package\]\]$/ { is_workspace_package = 0 }
        /^name = "asc-/ { is_workspace_package = 1 }
        is_workspace_package && /^version = "/ {
            sub(/"[^"]+"$/, "\"" version "\"")
        }
        { print }
    ' "$V2_CARGO_LOCK" > "$lock_tmp"
    mv "$lock_tmp" "$V2_CARGO_LOCK"
    ok "v2/Cargo.lock"
else
    warn "v2/Cargo.lock not found, skipping"
fi

if command -v npm &>/dev/null; then
    (cd "$PROJECT_ROOT/openclaw-plugin" && npm install --package-lock-only --ignore-scripts 2>/dev/null)
    ok "openclaw-plugin/package-lock.json"
else
    warn "npm not found, skipping openclaw-plugin/package-lock.json update"
fi

echo ""
log "=========================================="
log " Version bump complete: $OLD_VERSION → $NEW_VERSION"
log "=========================================="
echo ""
warn "Manual updates still needed:"
echo "  - agent-sec-core.spec.v2.in (%changelog entry)"
echo "  - CHANGELOG.md             (add actual change notes)"
echo ""
warn "Review CHANGELOG.md and spec.v2.in %changelog entries before committing."
