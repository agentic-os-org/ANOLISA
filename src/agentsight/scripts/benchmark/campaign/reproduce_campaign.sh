#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCHMARK_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
AGENTSIGHT_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"
REPO_ROOT="$(git -C "$AGENTSIGHT_DIR" rev-parse --show-toplevel)"

BASELINE_REF=""
OPTIMIZED_REF=""
MODE="quick"
RESULTS=""
CAMPAIGN_TEMPLATE="$SCRIPT_DIR/campaign.example.json"
DEFAULT_DATABASE="/var/log/sysak/.agentsight/genai_events.db"
DATABASE="$DEFAULT_DATABASE"
RUST_TOOLCHAIN="${AGENTSIGHT_RUST_TOOLCHAIN:-1.89.0}"
BUILD_JOBS="${AGENTSIGHT_BUILD_JOBS:-2}"
RESUME=0
ALLOW_IDENTICAL_VERSIONS=0
COMPARISON_MODE="ab_comparison"
SUDO_KEEPALIVE_PID=""
BUILD_ROOT=""
WORKTREES=()

stop_sudo_keepalive() {
    if [[ -n "$SUDO_KEEPALIVE_PID" ]]; then
        pkill -TERM -P "$SUDO_KEEPALIVE_PID" >/dev/null 2>&1 || true
        kill "$SUDO_KEEPALIVE_PID" >/dev/null 2>&1 || true
        wait "$SUDO_KEEPALIVE_PID" 2>/dev/null || true
        SUDO_KEEPALIVE_PID=""
    fi
}

cleanup_builds() {
    local worktree
    for worktree in "${WORKTREES[@]}"; do
        git -C "$REPO_ROOT" worktree remove --force "$worktree" >/dev/null 2>&1 || true
    done
    WORKTREES=()
    if [[ -n "$BUILD_ROOT" && -d "$BUILD_ROOT" ]]; then
        rm -rf -- "$BUILD_ROOT"
    fi
    BUILD_ROOT=""
}

cleanup() {
    stop_sudo_keepalive
    cleanup_builds
}

authorize_sudo() {
    if [[ "$(id -u)" -eq 0 ]]; then
        return
    fi
    if ! command -v sudo >/dev/null 2>&1; then
        echo "sudo is required to load AgentSight eBPF probes" >&2
        exit 2
    fi
    echo "[SUDO] AgentSight eBPF tracing requires root; authenticate before the build starts"
    if ! sudo -v; then
        echo "sudo authentication failed; no build or campaign was started" >&2
        exit 2
    fi
    # Cargo builds may outlive the sudo timestamp. Refresh it only until the
    # root campaign process starts; the multi-hour runner itself stays root.
    (
        while sleep 60; do
            sudo -n -v >/dev/null 2>&1 || true
        done
    ) >/dev/null 2>&1 &
    SUDO_KEEPALIVE_PID=$!
}

check_host_safety() {
    local config="$1"
    local reserve_for_build="$2"
    local max_results_gb
    local min_free_disk_gb
    local min_available_memory_mb
    local max_agentsight_rss_mb
    local max_k6_vus
    local existing_path="$RESULTS"
    local available_disk_kb
    local required_disk_gb
    local available_memory_kb

    max_results_gb="$(jq -er '(.safety.max_results_gb // 30) |
        select(type == "number" and floor == . and . > 0)' "$config")"
    min_free_disk_gb="$(jq -er '(.safety.min_free_disk_gb // 5) |
        select(type == "number" and floor == . and . > 0)' "$config")"
    min_available_memory_mb="$(jq -er '(.safety.min_available_memory_mb // 2048) |
        select(type == "number" and floor == . and . > 0)' "$config")"
    max_agentsight_rss_mb="$(jq -er '(.safety.max_agentsight_rss_mb // 1536) |
        select(type == "number" and floor == . and . > 0)' "$config")"
    max_k6_vus="$(jq -er '(.safety.max_k6_vus // 256) |
        select(type == "number" and floor == . and . > 0)' "$config")"

    while [[ ! -d "$existing_path" && "$existing_path" != "/" ]]; do
        existing_path="$(dirname "$existing_path")"
    done
    available_disk_kb="$(df -Pk "$existing_path" | awk 'NR == 2 {print $4}')"
    required_disk_gb="$min_free_disk_gb"
    if [[ "$reserve_for_build" -eq 1 ]]; then
        required_disk_gb=$((required_disk_gb + 8))
    fi
    if (( available_disk_kb < required_disk_gb * 1024 * 1024 )); then
        echo "host safety check failed: ${required_disk_gb} GiB free disk is required before starting" >&2
        exit 2
    fi
    available_memory_kb="$(awk '/^MemAvailable:/ {print $2}' /proc/meminfo)"
    if [[ -n "$available_memory_kb" ]] \
        && (( available_memory_kb < min_available_memory_mb * 1024 )); then
        echo "host safety check failed: ${min_available_memory_mb} MiB available memory is required" >&2
        exit 2
    fi
    echo "[SAFETY] results<=${max_results_gb} GiB, free-disk>=${min_free_disk_gb} GiB, available-memory>=${min_available_memory_mb} MiB, AgentSight-RSS<=${max_agentsight_rss_mb} MiB, k6-VUs<=${max_k6_vus}"
    if [[ "$reserve_for_build" -eq 1 ]]; then
        echo "[SAFETY] verified ${required_disk_gb} GiB free-disk headroom for temporary builds"
    fi
}

check_bpf_clock_alignment() {
    python3 - <<'PY'
import sys
import time

with open("/proc/uptime", encoding="ascii") as handle:
    uptime = float(handle.read().split()[0])
skew = abs(uptime - time.monotonic())
if skew > 1.0:
    print(
        "host clock check failed: /proc/uptime and CLOCK_MONOTONIC differ "
        f"by {skew:.1f}s after system suspend; reboot the host and disable "
        "sleep before starting a campaign",
        file=sys.stderr,
    )
    raise SystemExit(2)
print(f"[CLOCK] BPF timestamp domains aligned (skew={skew:.3f}s)")
PY
}

trap cleanup EXIT INT TERM

usage() {
    cat <<'EOF'
usage: reproduce_campaign.sh [options]

Build and run a reproducible AgentSight baseline/optimized campaign.

Required for a new run:
  --baseline-ref REF       baseline Git commit or ref
  --optimized-ref REF      optimized Git commit or ref

Options:
  --mode quick|formal      campaign size (default: quick)
  --results DIR            unified artifact directory
  --campaign-template FILE campaign settings and frozen thresholds
  --db FILE                AgentSight path inside isolation (must remain the default)
  --rust-toolchain VERSION Rust toolchain used for both builds (default: 1.89.0)
  --build-jobs N           maximum concurrent Cargo build jobs (default: 2)
  --allow-identical-versions
                           run a formal A/A calibration with identical versions
  --resume                 resume an existing prepared result directory
  -h, --help               show this help

Example:
  ./scripts/benchmark/campaign/reproduce_campaign.sh \
    --baseline-ref origin/main \
    --optimized-ref HEAD \
    --mode quick
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --baseline-ref) BASELINE_REF="$2"; shift 2 ;;
        --optimized-ref) OPTIMIZED_REF="$2"; shift 2 ;;
        --mode) MODE="$2"; shift 2 ;;
        --results) RESULTS="$2"; shift 2 ;;
        --campaign-template) CAMPAIGN_TEMPLATE="$2"; shift 2 ;;
        --db) DATABASE="$2"; shift 2 ;;
        --rust-toolchain) RUST_TOOLCHAIN="$2"; shift 2 ;;
        --build-jobs) BUILD_JOBS="$2"; shift 2 ;;
        --allow-identical-versions)
            ALLOW_IDENTICAL_VERSIONS=1
            COMPARISON_MODE="aa_calibration"
            shift
            ;;
        --resume) RESUME=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ "$MODE" != "quick" && "$MODE" != "formal" ]]; then
    echo "--mode must be quick or formal" >&2
    exit 2
fi
if [[ "$ALLOW_IDENTICAL_VERSIONS" -eq 1 && "$MODE" != "formal" ]]; then
    echo "--allow-identical-versions requires --mode formal" >&2
    exit 2
fi
if [[ ! "$BUILD_JOBS" =~ ^[1-9][0-9]*$ ]]; then
    echo "--build-jobs must be a positive integer" >&2
    exit 2
fi
for tool in gzip jq mount pkill python3 unshare; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "required campaign tool is missing: $tool" >&2
        exit 2
    fi
done
if [[ -z "$RESULTS" ]]; then
    RESULTS="$BENCHMARK_DIR/results/campaign-$MODE-$(date -u +%Y%m%dT%H%M%SZ)"
fi
RESULTS="$(realpath -m "$RESULTS")"
DATABASE="$(realpath -m "$DATABASE")"
CAMPAIGN_TEMPLATE="$(realpath -m "$CAMPAIGN_TEMPLATE")"
if [[ "$DATABASE" != "$DEFAULT_DATABASE" ]]; then
    echo "--db must remain $DEFAULT_DATABASE; isolated data is stored under --results" >&2
    exit 2
fi
if [[ "$RESULTS" == "/" || "$RESULTS" == "$REPO_ROOT" || "$RESULTS" == "$AGENTSIGHT_DIR" ]]; then
    echo "refusing unsafe results directory: $RESULTS" >&2
    exit 2
fi
echo "[RESULTS] $RESULTS"

CAMPAIGN_FILE="$RESULTS/inputs/campaign.json"
if [[ "$RESUME" -eq 1 ]]; then
    if [[ ! -f "$CAMPAIGN_FILE" ]]; then
        echo "cannot resume; prepared campaign is missing: $CAMPAIGN_FILE" >&2
        exit 2
    fi
    if jq -e '.comparison_mode == "aa_calibration"' "$CAMPAIGN_FILE" >/dev/null; then
        if [[ "$MODE" != "formal" ]]; then
            echo "an A/A calibration can only resume with --mode formal" >&2
            exit 2
        fi
        ALLOW_IDENTICAL_VERSIONS=1
        COMPARISON_MODE="aa_calibration"
    fi
    check_bpf_clock_alignment
    check_host_safety "$CAMPAIGN_FILE" 0
    authorize_sudo
else
    if [[ -z "$BASELINE_REF" || -z "$OPTIMIZED_REF" ]]; then
        echo "--baseline-ref and --optimized-ref are required for a new run" >&2
        exit 2
    fi
    if [[ ! -f "$CAMPAIGN_TEMPLATE" ]]; then
        echo "campaign template does not exist: $CAMPAIGN_TEMPLATE" >&2
        exit 2
    fi
    if [[ -e "$RESULTS" && -n "$(find "$RESULTS" -mindepth 1 -maxdepth 1 -print -quit 2>/dev/null)" ]]; then
        echo "results directory is not empty; use --resume or a new directory: $RESULTS" >&2
        exit 2
    fi

    BASELINE_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify "$BASELINE_REF^{commit}")"
    OPTIMIZED_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify "$OPTIMIZED_REF^{commit}")"
    if [[ "$MODE" == "formal" && "$ALLOW_IDENTICAL_VERSIONS" -eq 0 \
        && "$BASELINE_COMMIT" == "$OPTIMIZED_COMMIT" ]]; then
        echo "formal baseline and optimized refs must resolve to different commits" >&2
        exit 2
    fi

    check_bpf_clock_alignment
    check_host_safety "$CAMPAIGN_TEMPLATE" 1
    authorize_sudo
    mkdir -p "$RESULTS/inputs/baseline" "$RESULTS/inputs/optimized"
    BUILD_ROOT="$(mktemp -d -t agentsight-campaign-build-XXXXXXXX)"

    LIBBPF_PATH="$(pkg-config --variable=libdir libbpf 2>/dev/null || true)"
    LIBBPF_PATH="${LIBBPF_PATH:-/usr/lib64:/usr/lib}"

    build_version() {
        local version="$1"
        local commit="$2"
        local worktree="$BUILD_ROOT/$version"
        local input_dir="$RESULTS/inputs/$version"
        local build_log="$input_dir/build.log"
        local lockfile_updated=0
        WORKTREES+=("$worktree")
        echo "[BUILD] $version commit=$commit"
        git -C "$REPO_ROOT" worktree add --detach "$worktree" "$commit"
        install -m 0644 "$worktree/src/agentsight/Cargo.lock" \
            "$input_dir/Cargo.lock.committed"
        set +e
        (
            cd "$worktree/src/agentsight"
            env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
                LIBBPF_SYS_LIBRARY_PATH="$LIBBPF_PATH" \
                CARGO_TARGET_DIR="$BUILD_ROOT/target" \
                rustup run "$RUST_TOOLCHAIN" cargo build --release --locked \
                    --jobs "$BUILD_JOBS"
        ) 2>&1 | tee "$build_log"
        local locked_status="${PIPESTATUS[0]}"
        set -e
        if [[ "$locked_status" -ne 0 ]]; then
            if ! grep -Fq "needs to be updated but --locked was passed" "$build_log"; then
                return "$locked_status"
            fi
            lockfile_updated=1
            echo "[RETRY] $version has a stale committed Cargo.lock; resolving only in the temporary worktree" \
                | tee -a "$build_log"
            (
                cd "$worktree/src/agentsight"
                env -u DESTDIR -u MAKEFLAGS -u MFLAGS -u MAKEOVERRIDES \
                    LIBBPF_SYS_LIBRARY_PATH="$LIBBPF_PATH" \
                    CARGO_TARGET_DIR="$BUILD_ROOT/target" \
                    rustup run "$RUST_TOOLCHAIN" cargo build --release \
                        --jobs "$BUILD_JOBS"
            ) 2>&1 | tee -a "$build_log"
        fi
        install -m 0755 "$BUILD_ROOT/target/release/agentsight" \
            "$input_dir/agentsight"
        install -m 0644 "$worktree/src/agentsight/agentsight.json" \
            "$input_dir/agentsight.json"
        install -m 0644 "$worktree/src/agentsight/Cargo.lock" \
            "$input_dir/Cargo.lock.resolved"
        git -C "$worktree" diff -- src/agentsight/Cargo.lock \
            >"$input_dir/Cargo.lock.diff"
        jq -n \
            --arg commit "$commit" \
            --argjson lockfile_updated "$lockfile_updated" \
            --arg committed_lock_sha256 "$(sha256sum "$input_dir/Cargo.lock.committed" | cut -d' ' -f1)" \
            --arg resolved_lock_sha256 "$(sha256sum "$input_dir/Cargo.lock.resolved" | cut -d' ' -f1)" \
            '{commit: $commit,
              lockfile_updated: ($lockfile_updated == 1),
              committed_lock_sha256: $committed_lock_sha256,
              resolved_lock_sha256: $resolved_lock_sha256}' \
            >"$input_dir/build-provenance.json"
        git -C "$REPO_ROOT" worktree remove --force "$worktree"
        echo "[OK] $version binary=$input_dir/agentsight"
    }

    build_version baseline "$BASELINE_COMMIT"
    build_version optimized "$OPTIMIZED_COMMIT"

    jq \
        --arg campaign_id "$(basename "$RESULTS")" \
        --arg comparison_mode "$COMPARISON_MODE" \
        --arg baseline_ref "$BASELINE_REF" \
        --arg baseline_commit "$BASELINE_COMMIT" \
        --arg baseline_binary "$RESULTS/inputs/baseline/agentsight" \
        --arg baseline_config "$RESULTS/inputs/baseline/agentsight.json" \
        --arg optimized_ref "$OPTIMIZED_REF" \
        --arg optimized_commit "$OPTIMIZED_COMMIT" \
        --arg optimized_binary "$RESULTS/inputs/optimized/agentsight" \
        --arg optimized_config "$RESULTS/inputs/optimized/agentsight.json" \
        --arg database "$DATABASE" \
        '.campaign_id = $campaign_id
         | .comparison_mode = $comparison_mode
         | .versions.baseline = {
             commit: $baseline_commit,
             source_ref: $baseline_ref,
             binary: $baseline_binary,
             config: $baseline_config,
             db: $database
           }
         | .versions.optimized = {
             commit: $optimized_commit,
             source_ref: $optimized_ref,
             binary: $optimized_binary,
             config: $optimized_config,
             db: $database
           }' \
        "$CAMPAIGN_TEMPLATE" >"$CAMPAIGN_FILE.tmp"
    mv "$CAMPAIGN_FILE.tmp" "$CAMPAIGN_FILE"

    jq -n \
        --arg rust_toolchain "$RUST_TOOLCHAIN" \
        --argjson build_jobs "$BUILD_JOBS" \
        --arg campaign_template "$CAMPAIGN_TEMPLATE" \
        --arg comparison_mode "$COMPARISON_MODE" \
        '{rust_toolchain: $rust_toolchain,
          build_jobs: $build_jobs,
          campaign_template: $campaign_template,
          comparison_mode: $comparison_mode}' \
        >"$RESULTS/inputs/build-settings.json"

    cleanup_builds
fi

RUNNER=(
    "$SCRIPT_DIR/run_campaign.py"
    --campaign "$CAMPAIGN_FILE"
    --results "$RESULTS"
    --mode "$MODE"
    --resume
    --isolated-storage-root "$RESULTS/runtime/storage"
)
if [[ "$ALLOW_IDENTICAL_VERSIONS" -eq 1 ]]; then
    RUNNER+=(--allow-identical-versions)
fi

echo "[RUN] $MODE campaign"
if [[ -x "$AGENTSIGHT_DIR/.venv/bin/python" ]]; then
    RUNNER_PYTHON="$AGENTSIGHT_DIR/.venv/bin/python"
else
    RUNNER_PYTHON="python3"
fi
if command -v systemctl >/dev/null 2>&1 \
    && systemctl is-active --quiet agentsight.service; then
    echo "[STOP] agentsight.service"
    if [[ "$(id -u)" -eq 0 ]]; then
        systemctl stop agentsight.service
    else
        sudo -n systemctl stop agentsight.service
    fi
fi
stop_sudo_keepalive
if [[ "$(id -u)" -ne 0 ]] && ! sudo -n -v; then
    echo "sudo credentials expired before campaign startup; rerun with --resume" >&2
    echo "Campaign artifacts: $RESULTS" >&2
    exit 2
fi
set +e
if [[ "$(id -u)" -eq 0 ]]; then
    unshare --mount --propagation private -- \
        env RUST_LOG="${RUST_LOG:-info}" AGENTSIGHT_CARGO_JOBS="$BUILD_JOBS" \
        "$RUNNER_PYTHON" "${RUNNER[@]}"
    CAMPAIGN_STATUS=$?
else
    sudo -n unshare --mount --propagation private -- \
        env RUST_LOG="${RUST_LOG:-info}" AGENTSIGHT_CARGO_JOBS="$BUILD_JOBS" \
        "$RUNNER_PYTHON" "${RUNNER[@]}"
    CAMPAIGN_STATUS=$?
fi
set -e

echo "Campaign artifacts: $RESULTS"
exit "$CAMPAIGN_STATUS"
