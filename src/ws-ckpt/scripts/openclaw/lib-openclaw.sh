#!/bin/bash

OPENCLAW_MIN_VERSION="2026.2.13"
OPENCLAW_CONDITIONAL_CONFIG_VERSION="2026.9.1"

help_lists_flag() {
    local help_text="$1"
    local flag="$2"

    grep -Eq "(^|[^[:alnum:]_.-])${flag}([^[:alnum:]_.-]|$)" <<<"$help_text"
}

is_unset_config_path() {
    local output="$1"

    [[ "$output" == *"valid but unset"* ]] || [[ "$output" == *"Config path not found"* ]]
}

root_config_uses_include() {
    local config_path="$1"

    [ -f "$config_path" ] && grep -Fq '$include' -- "$config_path"
}

openclaw_version_from_line() {
    local line="$1"
    local -a tokens
    local version_token=""
    local trailing_start=0
    local label

    read -r -a tokens <<<"$line"
    case "${#tokens[@]}" in
        0) return 1 ;;
        1) version_token="${tokens[0]}" ;;
        *)
            label="${tokens[0]%:}"
            if [[ ! "$label" =~ ^[Oo][Pp][Ee][Nn][Cc][Ll][Aa][Ww]$ ]]; then
                return 1
            fi
            if [ "${tokens[1],,}" = "cli" ] && [ "${tokens[2]:-}" != "" ] \
                && [ "${tokens[2],,}" = "version" ] && [ "${tokens[3]:-}" != "" ]; then
                version_token="${tokens[3]}"
                trailing_start=4
            elif [ "${tokens[1],,}" = "version" ] && [ "${tokens[2]:-}" != "" ]; then
                version_token="${tokens[2]}"
                trailing_start=3
            else
                version_token="${tokens[1]}"
                trailing_start=2
            fi
            if [ "${#tokens[@]}" -gt "$trailing_start" ]; then
                if [[ "${tokens[$trailing_start]}" != \(* ]] \
                    || [[ "${tokens[${#tokens[@]}-1]}" != *\) ]]; then
                    return 1
                fi
            fi
            ;;
    esac

    version_token="${version_token#[vV]}"
    if [[ ! "$version_token" =~ ^[0-9]{4}\.[0-9]+\.[0-9]+(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
        return 1
    fi
    printf '%s\n' "$version_token"
}

parse_openclaw_version_output() {
    local output="$1"
    local line candidate
    local found=""
    local count=0

    while IFS= read -r line || [ -n "$line" ]; do
        if candidate="$(openclaw_version_from_line "$line")"; then
            found="$candidate"
            count=$((count + 1))
        fi
    done <<<"$output"

    [ "$count" -eq 1 ] || return 1
    printf '%s\n' "$found"
}

openclaw_version_ge_release() {
    local version="$1"
    local required="$2"
    local without_build="${version%%+*}"
    local core="${without_build%%-*}"
    local required_core="${required%%[-+]*}"
    local suffix=""
    local year month day required_year required_month required_day

    if [[ "$without_build" == *-* ]]; then
        suffix="${without_build#*-}"
    fi
    IFS=. read -r year month day <<<"$core"
    IFS=. read -r required_year required_month required_day <<<"$required_core"
    year=$((10#$year))
    month=$((10#$month))
    day=$((10#$day))
    required_year=$((10#$required_year))
    required_month=$((10#$required_month))
    required_day=$((10#$required_day))

    if (( year != required_year )); then
        (( year > required_year ))
        return
    fi
    if (( month != required_month )); then
        (( month > required_month ))
        return
    fi
    if (( day != required_day )); then
        (( day > required_day ))
        return
    fi
    [ -z "$suffix" ] || [[ "$suffix" =~ ^[0-9]+(\.[0-9]+)*$ ]]
}

classify_openclaw_version() {
    local version="$1"

    if ! openclaw_version_ge_release "$version" "$OPENCLAW_MIN_VERSION"; then
        printf '%s\n' "unsupported"
    elif openclaw_version_ge_release "$version" "$OPENCLAW_CONDITIONAL_CONFIG_VERSION"; then
        printf '%s\n' "conditional"
    else
        printf '%s\n' "plain-json"
    fi
}

prepare_openclaw_config_set() {
    local openclaw_bin="$1"
    local state_dir="$2"
    local config_path="$3"
    local config_field="$4"
    local config_value="$5"
    local current_state="$6"
    local current_value="$7"
    local config_set_help

    OPENCLAW_CONFIG_SET_ARGS=(config set "$config_field" "$config_value")
    OPENCLAW_CONFIG_WRITE_ERROR=""

    if ! config_set_help="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$state_dir" \
        "$openclaw_bin" config set --help 2>/dev/null)"; then
        OPENCLAW_CONFIG_WRITE_ERROR="could not determine OpenClaw config write capabilities"
        return 1
    fi

    case "$OPENCLAW_COMPAT_MODE" in
        conditional)
            if ! help_lists_flag "$config_set_help" "--expect-current-absent" \
                || ! help_lists_flag "$config_set_help" "--expect-current-json"; then
                OPENCLAW_CONFIG_WRITE_ERROR="OpenClaw $OPENCLAW_VERSION lacks the conditional config flags required for a safe config update"
                return 1
            fi
            if [ "$current_state" = "absent" ]; then
                OPENCLAW_CONFIG_SET_ARGS+=(--expect-current-absent)
            else
                OPENCLAW_CONFIG_SET_ARGS+=(--expect-current-json "$current_value")
            fi
            ;;
        plain-json)
            if ! help_lists_flag "$config_set_help" "--json"; then
                OPENCLAW_CONFIG_WRITE_ERROR="OpenClaw $OPENCLAW_VERSION lacks the JSON config mode required for a safe config update"
                return 1
            fi
            if root_config_uses_include "$config_path"; then
                OPENCLAW_CONFIG_WRITE_ERROR="$config_path uses \$include, but OpenClaw $OPENCLAW_VERSION cannot update it safely"
                return 1
            fi
            OPENCLAW_CONFIG_SET_ARGS+=(--json)
            ;;
        *)
            OPENCLAW_CONFIG_WRITE_ERROR="OpenClaw $OPENCLAW_VERSION has no supported config write mode"
            return 1
            ;;
    esac
}

probe_openclaw_compat() {
    local openclaw_bin="$1"
    local state_dir="$2"
    local output rc

    OPENCLAW_VERSION=""
    OPENCLAW_COMPAT_MODE="unverifiable"
    OPENCLAW_COMPAT_ERROR=""

    output="$(env -u OPENCLAW_HOME OPENCLAW_STATE_DIR="$state_dir" \
        "$openclaw_bin" --version 2>&1)" && rc=0 || rc=$?
    if [ "$rc" -ne 0 ]; then
        OPENCLAW_COMPAT_ERROR="version probe exited with status $rc"
        return 1
    fi
    if ! OPENCLAW_VERSION="$(parse_openclaw_version_output "$output")"; then
        OPENCLAW_COMPAT_ERROR="could not parse an unambiguous version from: ${output//$'\n'/ }"
        return 1
    fi
    OPENCLAW_COMPAT_MODE="$(classify_openclaw_version "$OPENCLAW_VERSION")"
    if [ "$OPENCLAW_COMPAT_MODE" = "unsupported" ]; then
        OPENCLAW_COMPAT_ERROR="OpenClaw $OPENCLAW_VERSION is older than the minimum supported version $OPENCLAW_MIN_VERSION"
        return 2
    fi
}
