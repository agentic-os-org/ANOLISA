#!/usr/bin/env bash
# Build the architecture-independent Source1 payload for the V2 RPM.
set -euo pipefail

source_root=${1:?"usage: package-v2-rpm-vendor.sh <source-root> <vendor-staging>"}
vendor_staging=${2:?"usage: package-v2-rpm-vendor.sh <source-root> <vendor-staging>"}

for workspace in v2 linux-sandbox; do
    workspace_dir="$source_root/$workspace"
    if [[ ! -f "$workspace_dir/Cargo.toml" || ! -f "$workspace_dir/Cargo.lock" ]]; then
        echo "ERROR: missing locked V2 Cargo workspace: $workspace_dir" >&2
        exit 1
    fi

done

plugin_dir="$source_root/openclaw-plugin"
if [[ ! -f "$plugin_dir/package-lock.json" ]]; then
    echo "ERROR: missing locked OpenClaw npm project: $plugin_dir" >&2
    exit 1
fi

rm -rf "$vendor_staging"
mkdir -p "$vendor_staging"

for workspace in v2 linux-sandbox; do
    workspace_dir="$source_root/$workspace"
    (
        cd "$workspace_dir"
        rm -rf vendor .cargo
        mkdir .cargo
        cargo vendor --locked > .cargo/config.toml
    )
    mkdir -p "$vendor_staging/$workspace"
    mv "$workspace_dir/vendor" "$workspace_dir/.cargo" "$vendor_staging/$workspace/"
done

npm_cache="$vendor_staging/openclaw-plugin/.npm-cache"
mkdir -p "$npm_cache"
(
    cd "$plugin_dir"
    rm -rf node_modules
    # The plugin build only needs JavaScript/TypeScript inputs. Omitting optional
    # native packages keeps Source1 architecture-independent.
    npm ci --include=dev --omit=optional --ignore-scripts --cache "$npm_cache" --no-audit --no-fund
    rm -rf node_modules
    npm ci --offline --include=dev --omit=optional --ignore-scripts --cache "$npm_cache" --no-audit --no-fund
    npm cache verify --cache "$npm_cache"
    rm -rf "$npm_cache/_logs" node_modules
)
