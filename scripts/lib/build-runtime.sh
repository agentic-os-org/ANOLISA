#!/usr/bin/env bash
# Runtime checks for direct host installs. Pure builds and DESTDIR staging do not
# require the build host to provide the destination machine's runtime capabilities.

PKG_BASE="${PKG_BASE:-}"
PKG_INSTALL="${PKG_INSTALL:-}"

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
        agent-sec-core|sec-core)
            echo "$PROJECT_ROOT/src/agent-sec-core/packaging/anolisa/component.toml" ;;
        cosh-ng|agentsight|skillfs|ws-ckpt)
            echo "$PROJECT_ROOT/src/$1/packaging/anolisa/raw/component.toml" ;;
        sight)
            echo "$PROJECT_ROOT/src/agentsight/packaging/anolisa/raw/component.toml" ;;
        tokenless)
            echo "$PROJECT_ROOT/src/tokenless/packaging/anolisa/component.toml.in" ;;
        agent-memory|memory)
            echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/agent-memory/component.toml" ;;
        os-skills|skills)
            echo "$PROJECT_ROOT/distribution/anolisa/manifests/components/os-skills/component.toml" ;;
        # Native AW installations use explicit transport paths. Backend runtime
        # requirements belong to the separately selected backend components.
        aw|aw-provider-*) return 0 ;;
        *) return 1 ;;
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
    if [[ "$component" == "sec-core" || "$component" == "agent-sec-core" ]] && \
            [[ "$name" == "nodejs" ]]; then
        # File installation stages optional adapter resources without activating
        # them. Their build-time Node requirement is checked by build-env.sh.
        return 0
    fi
    echo "$record"
}

source_build_runtime_dependencies() {
    # Compiler and package-manager requirements are resolved before building,
    # independently of whether the resulting files are installed on this host.
    return 0
}

runtime_install_components() {
    active_components
}

selected_runtime_dependencies() {
    local component manifest dependencies record
    while IFS= read -r component; do
        manifest="$(runtime_manifest_path "$component")" || return 1
        [[ -n "$manifest" ]] || continue
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
    # A retry must keep the original installation and privilege boundaries.
    retry+=("--$INSTALL_MODE")
    [[ -z "${PREFIX:-}" ]] || retry+=("--prefix" "$PREFIX")
    ${SUPPORTED_ONLY:-false} && retry+=("--supported-only")
    [[ "${INSTALL_SYSTEM_DEPS:-auto}" == true ]] && retry+=("--install-system-deps")
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
    if ${DRY_RUN:-false}; then
        info "DRY-RUN: check selected component runtime dependencies before host installation"
        return 0
    fi
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
