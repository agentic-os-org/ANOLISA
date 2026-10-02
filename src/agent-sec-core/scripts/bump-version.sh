#!/bin/bash
# =============================================================================
# bump-version.sh
#
# One-shot version bump script for agent-sec-core project.
# Updates version strings across all configuration and source files.
#
# Usage:
#   ./bump-version.sh <new-version>
#   ./bump-version.sh 0.4.0
#
# What it updates:
#   1. agent-sec-cli/pyproject.toml          (project.version)
#   2. agent-sec-cli/Cargo.toml              (package.version)
#   3. agent-sec-cli/src/agent_sec_cli/__init__.py  (__version__)
#   4. agent-sec-cli/src/agent_sec_cli/cli.py       (fallback version)
#   5. openclaw-plugin/package.json          ("version" field)
#   6. openclaw-plugin/openclaw.plugin.json  ("version" field)
#   7. cosh-extension/cosh-extension.json    ("version" field)
#   8. hermes-plugin/src/plugin.yaml        (version field)
#   9. .anolisa/component.toml              (component.version)
#  10. codex-plugin/hooks-plugin/.codex-plugin/plugin.json ("version" field)
#  11. qoder-plugin/.qoder-plugin/plugin.json ("version" field)
#  12. qwen-code-extension/qwen-extension.json ("version" field)
#  13. v2/Cargo.toml                         (workspace.package.version)
#  14. Lock files: Cargo.lock, v2/Cargo.lock, uv.lock, package-lock.json (auto-regenerated)
#
# Manual update required (not automated):
#   - agent-sec-core.spec.in  (%changelog entry)
#   - CHANGELOG.md            (new version header with actual notes)
#   - openclaw-plugin/README.md (example version references)
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

# -----------------------------------------------------------------------------
# Validate arguments
# -----------------------------------------------------------------------------
if [[ $# -ne 1 ]]; then
    echo "Usage: $0 <new-version>"
    echo "Example: $0 0.4.0"
    exit 1
fi

NEW_VERSION="$1"

# Validate semver format (x.y.z)
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    err "Invalid version format: '$NEW_VERSION'. Expected semver (e.g., 0.4.0)"
    exit 1
fi

# Resolve project root (parent of scripts/)
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

log "Project root: $PROJECT_ROOT"
log "New version:  $NEW_VERSION"
echo ""

# -----------------------------------------------------------------------------
# Detect current version from pyproject.toml (single source of truth)
# -----------------------------------------------------------------------------
PYPROJECT="$PROJECT_ROOT/agent-sec-cli/pyproject.toml"
if [[ ! -f "$PYPROJECT" ]]; then
    err "pyproject.toml not found at: $PYPROJECT"
    exit 1
fi

OLD_VERSION="$(grep -m1 '^version' "$PYPROJECT" | sed 's/.*"\(.*\)"/\1/')"
if [[ -z "$OLD_VERSION" ]]; then
    err "Cannot detect current version from pyproject.toml"
    exit 1
fi

if [[ "$OLD_VERSION" == "$NEW_VERSION" ]]; then
    warn "Current version is already $NEW_VERSION, nothing to do."
    exit 0
fi

log "Current version: $OLD_VERSION"
log "Bumping: $OLD_VERSION → $NEW_VERSION"
echo ""

# -----------------------------------------------------------------------------
# Helper: replace version in a file and report result
# -----------------------------------------------------------------------------
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

# -----------------------------------------------------------------------------
# 1. pyproject.toml
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/agent-sec-cli/pyproject.toml" \
    "^version = \"$OLD_VERSION\"" \
    "version = \"$NEW_VERSION\"" \
    "agent-sec-cli/pyproject.toml"

# -----------------------------------------------------------------------------
# 2. Cargo.toml
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/agent-sec-cli/Cargo.toml" \
    "^version = \"$OLD_VERSION\"" \
    "version = \"$NEW_VERSION\"" \
    "agent-sec-cli/Cargo.toml"

# -----------------------------------------------------------------------------
# 3. __init__.py
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/agent-sec-cli/src/agent_sec_cli/__init__.py" \
    "__version__ = \"$OLD_VERSION\"" \
    "__version__ = \"$NEW_VERSION\"" \
    "agent-sec-cli/src/agent_sec_cli/__init__.py"

# -----------------------------------------------------------------------------
# 4. cli.py (fallback version)
# -----------------------------------------------------------------------------
CLI_PY="$PROJECT_ROOT/agent-sec-cli/src/agent_sec_cli/cli.py"
if [[ -f "$CLI_PY" ]]; then
    if grep -q "\"$OLD_VERSION\"" "$CLI_PY"; then
        replace_in_file "s|\"$OLD_VERSION\"|\"$NEW_VERSION\"|" "$CLI_PY"
        ok "agent-sec-cli/src/agent_sec_cli/cli.py (fallback)"
    else
        warn "Old version not found in cli.py fallback"
    fi
fi

# -----------------------------------------------------------------------------
# 5. openclaw-plugin/package.json
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/openclaw-plugin/package.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "openclaw-plugin/package.json"

# -----------------------------------------------------------------------------
# 6. openclaw-plugin/openclaw.plugin.json
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/openclaw-plugin/openclaw.plugin.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "openclaw-plugin/openclaw.plugin.json"

# -----------------------------------------------------------------------------
# 7. cosh-extension/cosh-extension.json
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/cosh-extension/cosh-extension.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "cosh-extension/cosh-extension.json"

# -----------------------------------------------------------------------------
# 8. hermes-plugin/src/plugin.yaml
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/hermes-plugin/src/plugin.yaml" \
    "^version: $OLD_VERSION" \
    "version: $NEW_VERSION" \
    "hermes-plugin/src/plugin.yaml"

# -----------------------------------------------------------------------------
# 9. .anolisa/component.toml
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/.anolisa/component.toml" \
    "^version = \"$OLD_VERSION\"" \
    "version = \"$NEW_VERSION\"" \
    ".anolisa/component.toml"

# -----------------------------------------------------------------------------
# 10. codex-plugin/hooks-plugin/.codex-plugin/plugin.json
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/codex-plugin/hooks-plugin/.codex-plugin/plugin.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "codex-plugin/hooks-plugin/.codex-plugin/plugin.json"

# -----------------------------------------------------------------------------
# 11. qoder-plugin/.qoder-plugin/plugin.json
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/qoder-plugin/.qoder-plugin/plugin.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "qoder-plugin/.qoder-plugin/plugin.json"

# -----------------------------------------------------------------------------
# 12. qwen-code-extension/qwen-extension.json
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/qwen-code-extension/qwen-extension.json" \
    "\"version\": \"$OLD_VERSION\"" \
    "\"version\": \"$NEW_VERSION\"" \
    "qwen-code-extension/qwen-extension.json"

# -----------------------------------------------------------------------------
# 13. v2/Cargo.toml
#
# The V2 Rust workspace owns its own version source; it is kept in lockstep with
# the V1 one so a single bump produces one coherent release across both trees.
# -----------------------------------------------------------------------------
bump_file "$PROJECT_ROOT/v2/Cargo.toml" \
    "^version = \"$OLD_VERSION\"" \
    "version = \"$NEW_VERSION\"" \
    "v2/Cargo.toml"

# -----------------------------------------------------------------------------
# 14. Regenerate lock files
# -----------------------------------------------------------------------------
log "Regenerating lock files..."

# Cargo.lock
if command -v cargo &>/dev/null; then
    (
        cd "$PROJECT_ROOT/agent-sec-cli"
        if ! cargo update --workspace 2>/dev/null; then
            cargo generate-lockfile 2>/dev/null
        fi
    )
    ok "agent-sec-cli/Cargo.lock"
    (
        cd "$PROJECT_ROOT/v2"
        if ! cargo update --workspace 2>/dev/null; then
            cargo generate-lockfile 2>/dev/null
        fi
    )
    ok "v2/Cargo.lock"
else
    warn "cargo not found, skipping Cargo.lock update"
fi

# uv.lock
if command -v uv &>/dev/null; then
    (cd "$PROJECT_ROOT/agent-sec-cli" && uv lock 2>/dev/null)
    ok "agent-sec-cli/uv.lock"
else
    warn "uv not found, skipping uv.lock update"
fi

# package-lock.json
if command -v npm &>/dev/null; then
    (cd "$PROJECT_ROOT/openclaw-plugin" && npm install --package-lock-only --ignore-scripts 2>/dev/null)
    ok "openclaw-plugin/package-lock.json"
else
    warn "npm not found, skipping package-lock.json update"
fi

# -----------------------------------------------------------------------------
# NOTE: The following files need MANUAL update (not automated):
#   - agent-sec-core.spec.in (%changelog entry)
#   - CHANGELOG.md (new version section with actual change notes)
#   - openclaw-plugin/README.md (example version references)
# -----------------------------------------------------------------------------

# -----------------------------------------------------------------------------
# Summary
# -----------------------------------------------------------------------------
echo ""
log "=========================================="
log " Version bump complete: $OLD_VERSION → $NEW_VERSION"
log "=========================================="
echo ""
warn "Manual updates still needed:"
echo "  - agent-sec-core.spec.in   (%changelog entry)"
echo "  - CHANGELOG.md             (add actual change notes)"
echo "  - openclaw-plugin/README.md (example version references)"
echo ""
warn "Review CHANGELOG.md and spec.in %changelog entries before committing."
