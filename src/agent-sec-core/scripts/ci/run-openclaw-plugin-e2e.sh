#!/usr/bin/env bash
# Run the OpenClaw plugin E2E pilot for one selected OpenClaw version.
#
# GitHub Actions fans this script out across an OpenClaw version matrix. The
# script itself stays single-version so it can be validated locally and reused
# outside GitHub Actions.

set -euo pipefail

# OPENCLAW_VERSION is accepted as the CI input. Capture it, then unset it before
# child processes run so OpenClaw and deploy.sh observe the actual installed
# OpenClaw binary instead of an ambient version override.
OPENCLAW_REQUESTED_VERSION="${OPENCLAW_VERSION:-latest}"
unset OPENCLAW_VERSION
OPENCLAW_EXPECTED_VERSION="${OPENCLAW_EXPECTED_VERSION:-}"
OPENCLAW_MATRIX_LABEL="${OPENCLAW_MATRIX_LABEL:-$OPENCLAW_REQUESTED_VERSION}"
OPENCLAW_BIN="${OPENCLAW_BIN:-}"
OPENCLAW_E2E_DRY_RUN="${OPENCLAW_E2E_DRY_RUN:-0}"
OPENCLAW_E2E_AGENT_SEC_INSTALL_MODE="${OPENCLAW_E2E_AGENT_SEC_INSTALL_MODE:-minimal}"
OPENCLAW_E2E_SKIP_NPM_CI="${OPENCLAW_E2E_SKIP_NPM_CI:-0}"
OPENCLAW_E2E_PLUGIN_PACKAGE="${OPENCLAW_E2E_PLUGIN_PACKAGE:-}"
EXPECT_UNSAFE_INSTALL_FLAG="${EXPECT_UNSAFE_INSTALL_FLAG:-}"
AGENT_SEC_CLI_BIN="${AGENT_SEC_CLI_BIN:-}"
AGENT_SEC_DAEMON_BIN="${AGENT_SEC_DAEMON_BIN:-}"
AGENT_SEC_CLI_WHEEL="${AGENT_SEC_CLI_WHEEL:-}"
PYTHON_VERSION="${PYTHON_VERSION:-3.11.6}"

usage() {
    cat <<'USAGE'
Usage: scripts/ci/run-openclaw-plugin-e2e.sh [options]

Options:
  --dry-run        Print the resolved plan without installing or running E2E.
  --skip-npm-ci    Skip npm ci in openclaw-plugin.
  -h, --help       Show this help.

Environment:
  OPENCLAW_VERSION              OpenClaw npm version to install, or latest.
  OPENCLAW_EXPECTED_VERSION     Exact detected OpenClaw version expected.
  OPENCLAW_MATRIX_LABEL         Stable matrix/artifact label.
  OPENCLAW_BIN                  Existing OpenClaw binary; skips npm install.
  AGENT_SEC_CLI_BIN             Existing agent-sec-cli binary.
  AGENT_SEC_DAEMON_BIN          Existing agent-sec-daemon binary.
  AGENT_SEC_CLI_WHEEL           Wheel artifact to install into .venv.
  OPENCLAW_E2E_PLUGIN_PACKAGE   Prebuilt plugin .tgz; skips npm ci/build/pack.
  OPENCLAW_E2E_AGENT_SEC_INSTALL_MODE
                                minimal (default) installs wheel-declared
                                runtime deps without ML packages; full installs
                                all wheel deps.
  EXPECT_UNSAFE_INSTALL_FLAG    Optional true/false assertion for deploy.sh.
  OPENCLAW_E2E_RESULT_ROOT      Result root; defaults to target/openclaw-e2e/results.
  OPENCLAW_E2E_SKIP_NPM_CI      Set to 1 to skip npm ci.
  OPENCLAW_E2E_NPM_RETRIES      Number of retries for npm install/ci (default: 3).
  OPENCLAW_E2E_DRY_RUN          Set to 1 for dry-run.
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run)
            OPENCLAW_E2E_DRY_RUN=1
            shift
            ;;
        --skip-npm-ci)
            OPENCLAW_E2E_SKIP_NPM_CI=1
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            echo "ERROR: unknown argument: $1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

log() {
    printf '[openclaw-e2e] %s\n' "$*"
}

die() {
    printf 'ERROR: %s\n' "$*" >&2
    exit 1
}

run() {
    log "+ $*"
    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        return 0
    fi
    "$@"
}

run_with_retries() {
    local max_retries="${1:-3}"
    shift
    local i
    for ((i = 1; i <= max_retries; i++)); do
        if run "$@"; then
            return 0
        fi
        if [[ "$i" -lt "$max_retries" ]]; then
            log "$* failed, retrying ($i/$max_retries) in 5s..."
            sleep 5
        fi
    done
    return 1
}

require_command() {
    local name="$1"

    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        return 0
    fi
    command -v "$name" >/dev/null 2>&1 || die "$name is required"
}

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/../.." && pwd)"
plugin_root="$repo_root/openclaw-plugin"
venv_dir="${AGENT_SEC_CLI_VENV:-$repo_root/.venv}"
openclaw_label="$(printf '%s' "$OPENCLAW_MATRIX_LABEL" | tr -c 'A-Za-z0-9_.-' '-')"
result_root="${OPENCLAW_E2E_RESULT_ROOT:-$repo_root/target/openclaw-e2e/results}"
run_id="${OPENCLAW_E2E_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
result_dir="${OPENCLAW_E2E_RESULT_DIR:-$result_root/$openclaw_label/$run_id}"
tmp_root="${OPENCLAW_E2E_TMP_ROOT:-${TMPDIR:-/tmp}}"
pilot_workdir="${OPENCLAW_E2E_WORKDIR:-$tmp_root/agentsec-openclaw-e2e-$openclaw_label-$run_id}"
artifact_workdir="$result_dir/workdir"
tools_root="${OPENCLAW_E2E_TOOLS_ROOT:-$repo_root/target/openclaw-e2e/tools}"
npm_cache="${NPM_CONFIG_CACHE:-${npm_config_cache:-}}"
plugin_package=""
# Wheels no longer declare torch/transformers/modelscope, so there is nothing
# to exclude from the resolved runtime-dep set. Kept as an empty array so the
# resolve_wheel_runtime_deps call site keeps working if exclusions are needed again.
agent_sec_cli_e2e_excluded_deps=()

sync_pilot_workdir() {
    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" || ! -d "$pilot_workdir" ]]; then
        return 0
    fi
    if [[ "$pilot_workdir" == "$artifact_workdir" ]]; then
        return 0
    fi
    rm -rf "$artifact_workdir"
    mkdir -p "$result_dir"
    cp -R "$pilot_workdir" "$artifact_workdir"
}

trap 'sync_pilot_workdir || true' EXIT

run_openclaw() {
    local bin="$1"
    shift

    if [[ "$bin" == *.mjs ]]; then
        node "$bin" "$@"
        return
    fi
    "$bin" "$@"
}

extract_version() {
    grep -Eo '[0-9]{4}\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?' | head -n 1 || true
}

find_latest_wheel() {
    local wheel

    wheel="$(find "$repo_root/target/wheels" -maxdepth 1 -type f -name 'agent_sec_cli-*.whl' -print 2>/dev/null | sort | tail -n 1 || true)"
    if [[ -z "$wheel" ]]; then
        wheel="$(find "$repo_root/target/wheels" -maxdepth 1 -type f -name 'agent-sec-cli-*.whl' -print 2>/dev/null | sort | tail -n 1 || true)"
    fi
    printf '%s\n' "$wheel"
}

resolve_artifact_spec() {
    local label="$1"
    local spec="$2"
    local absolute_spec="$spec"
    local matches=()
    local match

    if [[ "$spec" != /* ]]; then
        absolute_spec="$repo_root/$spec"
    fi
    if [[ -f "$absolute_spec" ]]; then
        printf '%s\n' "$absolute_spec"
        return
    fi

    while IFS= read -r match; do
        matches+=("$match")
    done < <(compgen -G "$absolute_spec" || true)

    if [[ "${#matches[@]}" -eq 1 ]]; then
        printf '%s\n' "${matches[0]}"
        return
    fi
    if [[ "${#matches[@]}" -gt 1 ]]; then
        printf '%s\n' "${matches[@]}" | sort | tail -n 1
        return
    fi

    die "$label not found from spec: $spec"
}

configure_npm() {
    export NPM_CONFIG_AUDIT="${NPM_CONFIG_AUDIT:-false}"
    export NPM_CONFIG_FETCH_RETRIES="${NPM_CONFIG_FETCH_RETRIES:-2}"
    export NPM_CONFIG_FETCH_RETRY_MAXTIMEOUT="${NPM_CONFIG_FETCH_RETRY_MAXTIMEOUT:-10000}"
    export NPM_CONFIG_FETCH_RETRY_MINTIMEOUT="${NPM_CONFIG_FETCH_RETRY_MINTIMEOUT:-1000}"
    export NPM_CONFIG_FETCH_TIMEOUT="${NPM_CONFIG_FETCH_TIMEOUT:-60000}"
    export NPM_CONFIG_FUND="${NPM_CONFIG_FUND:-false}"
    if [[ "$OPENCLAW_REQUESTED_VERSION" == "latest" ]]; then
        export NPM_CONFIG_PREFER_OFFLINE=false
        export NPM_CONFIG_PREFER_ONLINE=true
    else
        export NPM_CONFIG_PREFER_OFFLINE="${NPM_CONFIG_PREFER_OFFLINE:-true}"
    fi

    if [[ -z "$npm_cache" ]]; then
        if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
            npm_cache="<npm default>"
        else
            npm_cache="$(npm config get cache)"
        fi
    fi
}

resolve_plugin_package() {
    [[ -n "$OPENCLAW_E2E_PLUGIN_PACKAGE" ]] || return 0

    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        plugin_package="$OPENCLAW_E2E_PLUGIN_PACKAGE"
    else
        plugin_package="$(resolve_artifact_spec "OpenClaw plugin package" "$OPENCLAW_E2E_PLUGIN_PACKAGE")"
    fi
    log "prebuilt plugin package: $plugin_package"
}

resolve_wheel_runtime_deps() {
    local python_bin="$1"
    local wheel="$2"
    shift 2

    "$python_bin" - "$wheel" "$@" <<'PY'
import re
import sys
import zipfile
from email import policy
from email.parser import BytesParser
from pathlib import Path

requirement_name = re.compile(
    r"^\s*([A-Za-z0-9](?:[A-Za-z0-9._-]*[A-Za-z0-9])?)"
)
extra_marker = re.compile(r"\bextra\b", re.IGNORECASE)


def canonicalize_package_name(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


wheel_path = Path(sys.argv[1])
excluded = {canonicalize_package_name(name) for name in sys.argv[2:]}

try:
    if not wheel_path.is_file():
        raise ValueError(f"wheel does not exist: {wheel_path}")
    with zipfile.ZipFile(wheel_path) as archive:
        metadata_paths = [
            name
            for name in archive.namelist()
            if name.endswith(".dist-info/METADATA")
        ]
        if len(metadata_paths) != 1:
            raise ValueError(
                f"expected one .dist-info/METADATA entry, found {len(metadata_paths)}"
            )
        metadata_content = archive.read(metadata_paths[0])

    metadata = BytesParser(policy=policy.default).parsebytes(metadata_content)
    for requirement_header in metadata.get_all("Requires-Dist", []):
        requirement = str(requirement_header)
        match = requirement_name.match(requirement)
        if match is None:
            raise ValueError(f"invalid Requires-Dist entry: {requirement!r}")

        _, marker_separator, marker = requirement.partition(";")
        if marker_separator and extra_marker.search(marker):
            continue
        if canonicalize_package_name(match.group(1)) in excluded:
            continue
        print(requirement)
except (OSError, KeyError, ValueError, zipfile.BadZipFile) as exc:
    print(f"ERROR: cannot resolve wheel runtime dependencies: {exc}", file=sys.stderr)
    raise SystemExit(1)
PY
}

install_agent_sec_cli_wheel() {
    local wheel="$1"
    local dependency_output
    local runtime_deps=()

    case "$OPENCLAW_E2E_AGENT_SEC_INSTALL_MODE" in
        minimal)
            if ! dependency_output="$(
                resolve_wheel_runtime_deps \
                    "$venv_dir/bin/python" \
                    "$wheel" \
                    "${agent_sec_cli_e2e_excluded_deps[@]}"
            )"; then
                die "failed to resolve runtime dependencies from wheel: $wheel"
            fi
            if [[ -n "$dependency_output" ]]; then
                mapfile -t runtime_deps <<<"$dependency_output"
                log "minimal runtime deps from wheel: ${runtime_deps[*]}"
                run uv pip install --python "$venv_dir/bin/python" "${runtime_deps[@]}"
            fi
            run uv pip install --python "$venv_dir/bin/python" --no-deps "$wheel"
            ;;
        full)
            run uv pip install --python "$venv_dir/bin/python" "$wheel"
            ;;
        *)
            die "unsupported OPENCLAW_E2E_AGENT_SEC_INSTALL_MODE: $OPENCLAW_E2E_AGENT_SEC_INSTALL_MODE"
            ;;
    esac
}

ensure_agent_sec_cli() {
    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        AGENT_SEC_CLI_BIN="${AGENT_SEC_CLI_BIN:-$venv_dir/bin/agent-sec-cli}"
        AGENT_SEC_DAEMON_BIN="${AGENT_SEC_DAEMON_BIN:-$venv_dir/bin/agent-sec-daemon}"
        log "agent-sec-cli binary: $AGENT_SEC_CLI_BIN"
        log "agent-sec-daemon binary: $AGENT_SEC_DAEMON_BIN"
        return
    fi

    if [[ -n "$AGENT_SEC_CLI_BIN" || -n "$AGENT_SEC_DAEMON_BIN" ]]; then
        [[ -n "$AGENT_SEC_CLI_BIN" && -n "$AGENT_SEC_DAEMON_BIN" ]] || die "set both AGENT_SEC_CLI_BIN and AGENT_SEC_DAEMON_BIN"
    elif [[ -x "$venv_dir/bin/agent-sec-cli" && -x "$venv_dir/bin/agent-sec-daemon" ]]; then
        AGENT_SEC_CLI_BIN="$venv_dir/bin/agent-sec-cli"
        AGENT_SEC_DAEMON_BIN="$venv_dir/bin/agent-sec-daemon"
    else
        require_command uv
        run uv venv --python "$PYTHON_VERSION" "$venv_dir"
        if [[ -z "$AGENT_SEC_CLI_WHEEL" ]]; then
            run make build-cli
            AGENT_SEC_CLI_WHEEL="$(find_latest_wheel)"
        else
            AGENT_SEC_CLI_WHEEL="$(resolve_artifact_spec "agent-sec-cli wheel" "$AGENT_SEC_CLI_WHEEL")"
        fi
        [[ -n "$AGENT_SEC_CLI_WHEEL" ]] || die "agent-sec-cli wheel not found"
        install_agent_sec_cli_wheel "$AGENT_SEC_CLI_WHEEL"
        AGENT_SEC_CLI_BIN="$venv_dir/bin/agent-sec-cli"
        AGENT_SEC_DAEMON_BIN="$venv_dir/bin/agent-sec-daemon"
    fi

    if [[ "$OPENCLAW_E2E_DRY_RUN" != "1" ]]; then
        [[ -x "$AGENT_SEC_CLI_BIN" ]] || die "agent-sec-cli binary is not executable: $AGENT_SEC_CLI_BIN"
        [[ -x "$AGENT_SEC_DAEMON_BIN" ]] || die "agent-sec-daemon binary is not executable: $AGENT_SEC_DAEMON_BIN"
        "$AGENT_SEC_CLI_BIN" --version
    fi
}

install_openclaw() {
    if [[ -n "$OPENCLAW_BIN" ]]; then
        return
    fi

    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        OPENCLAW_BIN="$tools_root/openclaw-$openclaw_label/node_modules/.bin/openclaw"
        log "OpenClaw binary: $OPENCLAW_BIN"
        return
    fi

    require_command npm
    local install_dir="$tools_root/openclaw-$openclaw_label"
    run mkdir -p "$install_dir"
    run_with_retries "${OPENCLAW_E2E_NPM_RETRIES:-3}" npm install --prefix "$install_dir" --no-save "openclaw@$OPENCLAW_REQUESTED_VERSION"
    OPENCLAW_BIN="$install_dir/node_modules/.bin/openclaw"
}

verify_openclaw() {
    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        return
    fi

    [[ -n "$OPENCLAW_BIN" ]] || die "OPENCLAW_BIN was not resolved"
    [[ -x "$OPENCLAW_BIN" || "$OPENCLAW_BIN" == *.mjs ]] || die "OpenClaw binary is not executable: $OPENCLAW_BIN"

    local raw_version actual_version
    raw_version="$(run_openclaw "$OPENCLAW_BIN" --version)"
    actual_version="$(printf '%s\n' "$raw_version" | extract_version)"
    [[ -n "$actual_version" ]] || die "unable to parse OpenClaw version from: $raw_version"

    if [[ -n "$OPENCLAW_EXPECTED_VERSION" && "$actual_version" != "$OPENCLAW_EXPECTED_VERSION" ]]; then
        die "OpenClaw version mismatch: expected $OPENCLAW_EXPECTED_VERSION, got $actual_version"
    fi
    log "OpenClaw version: $actual_version ($OPENCLAW_BIN)"
}

install_plugin_dependencies() {
    if [[ -n "$plugin_package" ]]; then
        log "skipping npm ci because a prebuilt plugin package was provided"
        return
    fi
    if [[ "$OPENCLAW_E2E_SKIP_NPM_CI" == "1" ]]; then
        log "skipping npm ci because OPENCLAW_E2E_SKIP_NPM_CI=1"
        return
    fi
    run_with_retries "${OPENCLAW_E2E_NPM_RETRIES:-3}" npm ci --prefix "$plugin_root"
}

run_pilot() {
    local pilot_args=(
        --openclaw-bin "$OPENCLAW_BIN"
        --agent-sec-cli "$AGENT_SEC_CLI_BIN"
        --agent-sec-daemon "$AGENT_SEC_DAEMON_BIN"
        --workdir "$pilot_workdir"
    )
    if [[ -n "$plugin_package" ]]; then
        pilot_args+=(--plugin-package "$plugin_package")
    fi

    run mkdir -p "$result_dir"
    run mkdir -p "$pilot_workdir"
    run chmod 700 "$pilot_workdir"
    run npm run e2e:openclaw --prefix "$plugin_root" -- \
        "${pilot_args[@]}"
}

write_summary() {
    if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
        return
    fi

    local result_file="$pilot_workdir/pilot-result.json"
    local artifact_result_file="$artifact_workdir/pilot-result.json"
    local pilot_passed="false"
    local unsafe_flag_matches="true"
    local actual_unsafe

    [[ -f "$result_file" ]] || die "pilot result not found: $result_file"
    if jq -e '.status == "passed" and (.errors | length == 0)' "$result_file" >/dev/null; then
        pilot_passed="true"
    fi

    if [[ -n "$EXPECT_UNSAFE_INSTALL_FLAG" ]]; then
        actual_unsafe="$(jq -r '.install.usedUnsafeInstallFlag' "$result_file")"
        if [[ "$actual_unsafe" != "$EXPECT_UNSAFE_INSTALL_FLAG" ]]; then
            unsafe_flag_matches="false"
        fi
    fi

    local summary_json="$result_dir/summary.json"
    local summary_md="$result_dir/summary.md"
    jq \
        --arg requested "$OPENCLAW_REQUESTED_VERSION" \
        --arg expected "$OPENCLAW_EXPECTED_VERSION" \
        --arg matrix_label "$OPENCLAW_MATRIX_LABEL" \
        --arg resultFile "$artifact_result_file" \
        '{
          matrixLabel: $matrix_label,
          requestedOpenClawVersion: $requested,
          expectedOpenClawVersion: $expected,
          detectedOpenClawVersion: .versions.openclaw,
          agentSecCliVersion: .versions.agentSecCli,
          status,
          usedUnsafeInstallFlag: .install.usedUnsafeInstallFlag,
          resultFile: $resultFile,
          policyMatrixSkipped: (.policyMatrix.skipped // false),
          policyMatrixSkipReason: (.policyMatrix.reason // null),
          policyConfigApplication: (.policyMatrix.policyConfigApplication // null),
          livePolicyConfig: (.policyMatrix.livePolicyConfig // null),
          policyMatrix: [.policyMatrix.cases[]? | {name, passed, approvalDelivery, assertions}],
          observabilityCompatibility: (.gatewayTrafficProbe.observabilityCompatibility // null),
          observabilityAssertions: .gatewayTrafficProbe.observability.assertions,
          hookProbeImportResolution: (.hookProbe.importResolution // null),
          hookProbeRegisteredHooks: (.hookProbe.registeredHooks // [] | length),
          hookProbeCases: (.hookProbe.cases // [] | length),
          slowestSteps: ([
            .steps[]?
            | select(.durationMs != null)
            | {name, durationMs, exitCode, timedOut}
          ] | sort_by(.durationMs) | reverse | .[:12])
        }' "$result_file" > "$summary_json"

    {
        printf '# OpenClaw Plugin E2E\n\n'
        printf '%s\n' "- Matrix label: \`$OPENCLAW_MATRIX_LABEL\`"
        printf '%s\n' "- Requested OpenClaw: \`$OPENCLAW_REQUESTED_VERSION\`"
        printf '%s\n' "- Result file: \`$result_file\`"
        printf '\n```json\n'
        cat "$summary_json"
        printf '\n```\n'
    } > "$summary_md"
    log "summary: $summary_json"

    if [[ "$pilot_passed" != "true" ]]; then
        die "OpenClaw plugin E2E pilot failed; see $summary_json and $result_file"
    fi
    if [[ "$unsafe_flag_matches" != "true" ]]; then
        die "unsafe install flag mismatch: expected $EXPECT_UNSAFE_INSTALL_FLAG, got $actual_unsafe"
    fi
}

log "matrix label: $OPENCLAW_MATRIX_LABEL"
log "requested OpenClaw: $OPENCLAW_REQUESTED_VERSION"
log "result dir: $result_dir"
log "pilot workdir: $pilot_workdir"

if [[ "$OPENCLAW_E2E_DRY_RUN" == "1" ]]; then
    log "dry-run enabled; no install or E2E command will be executed"
fi

require_command node
require_command jq
require_command npm
configure_npm
log "npm cache: $npm_cache"
log "npm fetch preference: prefer-offline=$NPM_CONFIG_PREFER_OFFLINE prefer-online=${NPM_CONFIG_PREFER_ONLINE:-false}"
resolve_plugin_package
ensure_agent_sec_cli
install_openclaw
verify_openclaw
install_plugin_dependencies
run_pilot
write_summary
