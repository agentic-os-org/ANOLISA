#!/usr/bin/env bash
# A provider's source directory identifies its backend; its package names the target.

provider_package_name() {
    local directory="$1"
    [[ -f "$directory/Makefile" && -f "$directory/Cargo.toml" ]] || return 0
    awk '
        /^[[:space:]]*\[/ {
            package_section = ($0 ~ /^[[:space:]]*\[package\][[:space:]]*(#.*)?$/)
            next
        }
        package_section && /^[[:space:]]*name[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*/, "", value)
            quote = substr(value, 1, 1)
            if (quote != "\"" && quote != sprintf("%c", 39)) exit
            value = substr(value, 2)
            end = index(value, quote)
            if (end) print substr(value, 1, end - 1)
            exit
        }
    ' "$directory/Cargo.toml"
}

companion_provider() {
    local component="$1" name
    [[ "$component" != aw-provider-* ]] || return 0
    name="$(provider_package_name "$PROJECT_ROOT/providers/$component")" || return 1
    if [[ "$name" =~ ^aw-provider-[a-z0-9][a-z0-9-]*$ ]]; then printf '%s\n' "$name"; fi
}

provider_directory() {
    local provider="$1" directory name found=""
    for directory in "$PROJECT_ROOT"/providers/*; do
        [[ -d "$directory" ]] || continue
        name="$(provider_package_name "$directory")" || return 1
        [[ "$name" == "$provider" ]] || continue
        [[ -z "$found" ]] || { err "Duplicate provider package $provider: $found and $directory"; return 1; }
        found="$directory"
    done
    [[ -n "$found" ]] || return 1
    printf '%s\n' "$found"
}

component_dir() {
    case "$1" in
        aw-provider-*) provider_directory "$1" ;;
        *) printf '%s/src/%s\n' "$PROJECT_ROOT" "$1" ;;
    esac
}

component_selected() {
    local selected
    for selected in "${COMPONENTS[@]}"; do
        if [[ "$selected" == "$1" ]]; then return 0; fi
    done
    return 1
}

skip_component_plugins() {
    case "$1" in
        agent-memory|agent-sec-core|agentsight|tokenless|ws-ckpt) component_selected aw ;;
        *) return 1 ;;
    esac
}
