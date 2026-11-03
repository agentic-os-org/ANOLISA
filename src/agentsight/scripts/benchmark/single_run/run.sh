#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PYTHON_BIN="${PYTHON:-python3}"
BENCHMARK_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="${OUTPUT_DIR:-$BENCHMARK_DIR/results/$(date -u +%Y%m%dT%H%M%SZ)}"
HOST="127.0.0.1"
PORT="8443"
PROTOCOL="sse"
QPS="100"
DURATION="900"
PAYLOAD_KB="4"
CHUNKS="10"
CHUNK_BYTES="64"
CHUNK_DELAY_MS="0"
AGENTS=""
TOKENS_PER_SECOND=""
TOKENS_PER_RESPONSE=1000
STARTUP_DELAY=5
RATE_TOLERANCE=0.05
EXPECTED_TOTAL_TOKENS=20
AGENTSIGHT_PID=""
PREPARE_CAPTURE=0
METRICS_FILE=""
DB=""
FFI_EVENTS=""
KEEP_ARTIFACTS=0
USERSPACE=0
EXTERNAL_SERVER=0
FAULT_COUNT=0
FAULT_OVERSIZED_BYTES=$((9 * 1024 * 1024))
VALIDATION_WAIT_SECONDS=30
RESULTS_ROOT=""
MAX_RESULTS_GB=30
MIN_FREE_DISK_GB=5
MIN_AVAILABLE_MEMORY_MB=2048
MAX_AGENTSIGHT_RSS_MB=1536
MAX_K6_VUS=256
REQUEST_LOG_DIR=""
RUN_ID="${BENCHMARK_RUN_ID:-$(date -u +%Y%m%dT%H%M%S)-$$-$(date +%N)}"

usage() {
    echo "usage: $0 [--userspace] [--external-server] [--protocol sse|json|h2] [--host HOST] [--port PORT] [--qps N] [--duration SEC] [--payload-kb N] [--chunks N] [--chunk-bytes N] [--chunk-delay-ms N] [--agentsight-pid PID] [--metrics-file PATH] [--db PATH] [--request-log-dir DIR] [--validation-wait-seconds SEC] [--fault-count N] [--fault-oversized-bytes N] [--output-dir DIR] [--results-root DIR] [--max-results-gb N] [--min-free-disk-gb N] [--min-available-memory-mb N] [--max-agentsight-rss-mb N] [--max-k6-vus N] [--keep-artifacts]"
    echo "agent load: --agents N --tokens-per-second RATE [--tokens-per-response N] [--startup-delay SEC] [--rate-tolerance FRACTION]; defaults: 32 agents, 100 output tokens/s, 1000 tokens/response, 5s discovery delay, 5% rate tolerance"
    echo "capture source: --db PATH or --ffi-events PATH (mutually exclusive)"
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --userspace) USERSPACE=1; shift ;;
        --external-server) EXTERNAL_SERVER=1; shift ;;
        --protocol) PROTOCOL="$2"; shift 2 ;;
        --host) HOST="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        --qps) QPS="$2"; shift 2 ;;
        --duration) DURATION="$2"; shift 2 ;;
        --payload-kb) PAYLOAD_KB="$2"; shift 2 ;;
        --chunks) CHUNKS="$2"; shift 2 ;;
        --chunk-bytes) CHUNK_BYTES="$2"; shift 2 ;;
        --chunk-delay-ms) CHUNK_DELAY_MS="$2"; shift 2 ;;
        --agents) AGENTS="$2"; shift 2 ;;
        --tokens-per-second) TOKENS_PER_SECOND="$2"; shift 2 ;;
        --tokens-per-response) TOKENS_PER_RESPONSE="$2"; shift 2 ;;
        --startup-delay) STARTUP_DELAY="$2"; shift 2 ;;
        --rate-tolerance) RATE_TOLERANCE="$2"; shift 2 ;;
        --agentsight-pid) AGENTSIGHT_PID="$2"; shift 2 ;;
        --prepare-capture) PREPARE_CAPTURE=1; shift ;;
        --metrics-file) METRICS_FILE="$2"; shift 2 ;;
        --db) DB="$2"; shift 2 ;;
        --ffi-events) FFI_EVENTS="$2"; shift 2 ;;
        --request-log-dir) REQUEST_LOG_DIR="$2"; shift 2 ;;
        --validation-wait-seconds) VALIDATION_WAIT_SECONDS="$2"; shift 2 ;;
        --fault-count) FAULT_COUNT="$2"; shift 2 ;;
        --fault-oversized-bytes) FAULT_OVERSIZED_BYTES="$2"; shift 2 ;;
        --output-dir) OUTPUT_DIR="$2"; shift 2 ;;
        --results-root) RESULTS_ROOT="$2"; shift 2 ;;
        --max-results-gb) MAX_RESULTS_GB="$2"; shift 2 ;;
        --min-free-disk-gb) MIN_FREE_DISK_GB="$2"; shift 2 ;;
        --min-available-memory-mb) MIN_AVAILABLE_MEMORY_MB="$2"; shift 2 ;;
        --max-agentsight-rss-mb) MAX_AGENTSIGHT_RSS_MB="$2"; shift 2 ;;
        --max-k6-vus) MAX_K6_VUS="$2"; shift 2 ;;
        --keep-artifacts) KEEP_ARTIFACTS=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [[ "$USERSPACE" -eq 1 ]]; then
    cd "$SCRIPT_DIR/../../.."
    cargo test --test pipeline --test memory_storage -- --nocapture
    exit $?
fi

case "$PROTOCOL" in sse|json|h2) ;; *) echo "protocol must be sse, json, or h2" >&2; exit 2 ;; esac
if [[ -n "$AGENTS" || -n "$TOKENS_PER_SECOND" ]]; then
    [[ "$PROTOCOL" == "sse" ]] || { echo "agent load requires --protocol sse" >&2; exit 2; }
    AGENTS="${AGENTS:-32}"
    TOKENS_PER_SECOND="${TOKENS_PER_SECOND:-100}"
    "$PYTHON_BIN" "$SCRIPT_DIR/load/agents.py" --check-only --agents "$AGENTS" \
        --tokens-per-second "$TOKENS_PER_SECOND" --tokens-per-response "$TOKENS_PER_RESPONSE" \
        --duration "$DURATION" --payload-kb "$PAYLOAD_KB" --startup-delay "$STARTUP_DELAY" \
        --rate-tolerance "$RATE_TOLERANCE" --output-dir "$OUTPUT_DIR/agents" --run-id "$RUN_ID"
    EXPECTED_TOTAL_TOKENS="$("$PYTHON_BIN" -c 'import sys; print(12 + int(sys.argv[1]))' "$TOKENS_PER_RESPONSE")"
    QPS="$("$PYTHON_BIN" -c 'import sys; print(int(sys.argv[1]) * float(sys.argv[2]) / int(sys.argv[3]))' "$AGENTS" "$TOKENS_PER_SECOND" "$TOKENS_PER_RESPONSE")"
fi
if [[ -n "$DB" && -n "$FFI_EVENTS" ]]; then
    echo "--db and --ffi-events are mutually exclusive" >&2
    exit 2
fi
if [[ "$PREPARE_CAPTURE" -eq 1 && ( -z "$AGENTS" || ( -z "$DB" && -z "$FFI_EVENTS" ) || -n "$AGENTSIGHT_PID" ) ]]; then
    echo "capture preparation requires Agent mode, --db or --ffi-events, and a deferred collector PID" >&2
    exit 2
fi
if [[ ! "$RUN_ID" =~ ^[A-Za-z0-9_-]+$ ]]; then
    echo "BENCHMARK_RUN_ID may contain only letters, digits, underscores, and hyphens" >&2
    exit 2
fi
for value in "$MAX_RESULTS_GB" "$MIN_FREE_DISK_GB" "$MIN_AVAILABLE_MEMORY_MB" \
    "$MAX_AGENTSIGHT_RSS_MB" "$MAX_K6_VUS"; do
    if [[ ! "$value" =~ ^[1-9][0-9]*$ ]]; then
        echo "safety limits must be positive integers" >&2
        exit 2
    fi
done
if [[ -n "$AGENTS" && -e "$OUTPUT_DIR/agents" ]]; then
    echo "agent output exists; choose a new --output-dir: $OUTPUT_DIR" >&2
    exit 2
fi
mkdir -p "$OUTPUT_DIR"
# The collector writes this marker only on a violation, so a leftover file from
# a previous run would fail a healthy rerun; clear it before the load starts.
rm -f "$OUTPUT_DIR/safety-stop.json"
RESULTS_ROOT="${RESULTS_ROOT:-$OUTPUT_DIR}"
if [[ ! -d "$RESULTS_ROOT" ]]; then
    echo "results root does not exist: $RESULTS_ROOT" >&2
    exit 2
fi
printf '%s\n' "$RUN_ID" >"$OUTPUT_DIR/run-id.txt"
REQUEST_LOG=""
if [[ "$PROTOCOL" != "h2" ]]; then
    if [[ -z "$REQUEST_LOG_DIR" && "$EXTERNAL_SERVER" -eq 0 ]]; then
        REQUEST_LOG_DIR="$OUTPUT_DIR/request-logs"
    fi
    if [[ -n "$REQUEST_LOG_DIR" ]]; then
        mkdir -p "$REQUEST_LOG_DIR"
        REQUEST_LOG="$REQUEST_LOG_DIR/$RUN_ID.jsonl"
        : >"$REQUEST_LOG"
    elif [[ -n "$DB" ]]; then
        echo "--db with --external-server requires --request-log-dir" >&2
        exit 2
    fi
fi
TMP_DIR="$(mktemp -d)"
cleanup() {
    [[ -n "${LOAD_PID:-}" ]] && kill "$LOAD_PID" 2>/dev/null || true
    [[ -n "${COMPRESSOR_PID:-}" ]] && kill "$COMPRESSOR_PID" 2>/dev/null || true
    [[ -n "${COLLECTOR_PID:-}" ]] && kill "$COLLECTOR_PID" 2>/dev/null || true
    [[ -n "${FAULT_PID:-}" ]] && kill "$FAULT_PID" 2>/dev/null || true
    [[ -n "${VALIDATOR_PID:-}" ]] && kill "$VALIDATOR_PID" 2>/dev/null || true
    [[ -n "${SERVER_PID:-}" ]] && kill "$SERVER_PID" 2>/dev/null || true
    if [[ "$KEEP_ARTIFACTS" -eq 0 ]]; then rm -rf "$TMP_DIR"; fi
}
trap cleanup EXIT INT TERM

if [[ "$EXTERNAL_SERVER" -eq 0 ]]; then
    openssl req -x509 -newkey rsa:2048 -nodes -days 1 \
        -subj "/CN=localhost" -keyout "$TMP_DIR/server.key" -out "$TMP_DIR/server.crt" \
        >/dev/null 2>&1
    SERVER_ARGS=(--host "$HOST" --port "$PORT" --cert "$TMP_DIR/server.crt" --key "$TMP_DIR/server.key" --chunks "$CHUNKS" --chunk-bytes "$CHUNK_BYTES" --chunk-delay-ms "$CHUNK_DELAY_MS")
    [[ -n "$AGENTS" ]] && SERVER_ARGS+=(--tokens-per-second "$TOKENS_PER_SECOND" --output-tokens "$TOKENS_PER_RESPONSE")
    [[ "$PROTOCOL" == "json" ]] && SERVER_ARGS+=(--json)
    [[ "$PROTOCOL" == "h2" ]] && SERVER_ARGS+=(--http2)
    [[ -n "$REQUEST_LOG_DIR" ]] && SERVER_ARGS+=(--request-log-dir "$REQUEST_LOG_DIR")
    "$PYTHON_BIN" "$SCRIPT_DIR/mock_llm_server.py" "${SERVER_ARGS[@]}" >"$OUTPUT_DIR/mock-server.log" 2>&1 &
    SERVER_PID=$!
fi
for _ in $(seq 1 50); do
    curl -ksf "https://$HOST:$PORT/healthz" >/dev/null 2>&1 && break
    sleep 0.1
done
curl -ksf "https://$HOST:$PORT/healthz" >/dev/null

if [[ "$FAULT_COUNT" -gt 0 ]]; then
    FAULT_ARGS=(--host "$HOST" --port "$PORT" --count "$FAULT_COUNT" \
        --oversized-bytes "$FAULT_OVERSIZED_BYTES" --output "$OUTPUT_DIR/fault-results.json")
    [[ -n "$AGENTSIGHT_PID" ]] && FAULT_ARGS+=(--watch-pid "$AGENTSIGHT_PID")
    (
        sleep 1
        exec "$PYTHON_BIN" "$SCRIPT_DIR/fault_injector.py" "${FAULT_ARGS[@]}"
    ) &
    FAULT_PID=$!
fi

LOAD_STATUS=0
COMPRESSION_STATUS=0
VALIDATION_STATUS=0
VALIDATION_SINCE_NS="$(date +%s%N)"
VALIDATION_MARKER="$TMP_DIR/load-complete"
LOAD_RESULTS=""
LOAD_LOG=""
if [[ "$PROTOCOL" != "h2" ]]; then
    LOAD_RESULTS="$OUTPUT_DIR/k6.jsonl.gz"
    LOAD_LOG="$OUTPUT_DIR/k6.log"
    if [[ -n "$AGENTS" ]]; then
        LOAD_RESULTS="$OUTPUT_DIR/agents.jsonl.gz"
        LOAD_LOG="$OUTPUT_DIR/agents.log"
    fi
fi
start_validator() {
    [[ "$PROTOCOL" != "h2" && ( -n "$DB" || -n "$FFI_EVENTS" ) ]] || return 0
    local source_args=(--db "$DB")
    [[ -n "$FFI_EVENTS" ]] && source_args=(--ffi-events "$FFI_EVENTS")
    "$PYTHON_BIN" "$SCRIPT_DIR/validate_results.py" \
        --load-results "$LOAD_RESULTS" "${source_args[@]}" \
        --prefix "bench-$RUN_ID-" --since-ns "$VALIDATION_SINCE_NS" \
        --expected-total-tokens "$EXPECTED_TOTAL_TOKENS" --wait-seconds "$VALIDATION_WAIT_SECONDS" \
        --completion-marker "$VALIDATION_MARKER" \
        --request-log "$REQUEST_LOG" \
        --output "$OUTPUT_DIR/report.json" \
        >"$OUTPUT_DIR/validation.log" 2>&1 &
    VALIDATOR_PID=$!
}
[[ "$PREPARE_CAPTURE" -eq 1 ]] || start_validator
if [[ -n "$AGENTS" ]]; then
    AGENT_CAPTURE_ARGS=()
    if [[ -n "$DB" || -n "$FFI_EVENTS" ]]; then
        AGENT_CAPTURE_ARGS=(--capture-marker "$VALIDATION_MARKER" \
            --capture-timeout "$VALIDATION_WAIT_SECONDS")
    fi
    [[ "$PREPARE_CAPTURE" -eq 1 ]] && AGENT_CAPTURE_ARGS+=(--prepare-capture)
    "$PYTHON_BIN" "$SCRIPT_DIR/load/agents.py" --agents "$AGENTS" \
        --tokens-per-second "$TOKENS_PER_SECOND" --tokens-per-response "$TOKENS_PER_RESPONSE" \
        --duration "$DURATION" --payload-kb "$PAYLOAD_KB" --startup-delay "$STARTUP_DELAY" \
        --rate-tolerance "$RATE_TOLERANCE" --base-url "https://$HOST:$PORT" \
        --run-id "$RUN_ID" --output-dir "$OUTPUT_DIR/agents" \
        "${AGENT_CAPTURE_ARGS[@]}" >"$LOAD_LOG" 2>&1 &
    LOAD_PID=$!
elif [[ "$PROTOCOL" == "h2" ]]; then
    printf '{"request_id":"bench-%s-h2","model":"test-model","stream":true,"messages":[{"role":"user","content":"benchmark"}]}' "$RUN_ID" >"$TMP_DIR/payload.json"
    LOAD_LOG="$OUTPUT_DIR/h2load.txt"
    BASE_URL="https://$HOST:$PORT" QPS="$QPS" DURATION="$DURATION" \
        PAYLOAD_FILE="$TMP_DIR/payload.json" \
        "$SCRIPT_DIR/load/h2load.sh" >"$LOAD_LOG" &
    LOAD_PID=$!
else
    K6_PIPE="$TMP_DIR/k6-json.pipe"
    mkfifo "$K6_PIPE"
    gzip -1 <"$K6_PIPE" >"$LOAD_RESULTS" &
    COMPRESSOR_PID=$!
    BASE_URL="https://$HOST:$PORT" QPS="$QPS" DURATION="${DURATION}s" \
        PAYLOAD_KB="$PAYLOAD_KB" RUN_ID="$RUN_ID" \
        ENDPOINT="/v1/chat/completions" BENCHMARK_MAX_VUS="$MAX_K6_VUS" \
        k6 run --quiet --no-color --summary-mode=disabled --out json=- \
        "$SCRIPT_DIR/load/k6.js" >"$K6_PIPE" 2>"$LOAD_LOG" &
    LOAD_PID=$!
fi

if [[ "$PREPARE_CAPTURE" -eq 1 ]]; then
    while [[ ! -f "$OUTPUT_DIR/capture-ready.json" ]]; do
        if ! kill -0 "$LOAD_PID" 2>/dev/null; then
            echo "clients failed during capture preparation; see $LOAD_LOG" >&2
            exit 1
        fi
        sleep 0.05
    done
    AGENTSIGHT_PID="$("$PYTHON_BIN" -c 'import json,sys; print(json.load(open(sys.argv[1]))["pid"])' "$OUTPUT_DIR/tracer-ready.json")"
    start_validator
fi

if [[ -n "$AGENTSIGHT_PID" ]]; then
    COLLECTOR_ARGS=(--pid "$AGENTSIGHT_PID" --input-qps "$QPS" --interval 1 \
        --duration 0 --output "$OUTPUT_DIR/metrics.csv" \
        --load-pid "$LOAD_PID" --results-root "$RESULTS_ROOT" \
        --safety-output "$OUTPUT_DIR/safety-stop.json" \
        --max-results-gb "$MAX_RESULTS_GB" \
        --min-free-disk-gb "$MIN_FREE_DISK_GB" \
        --min-available-memory-mb "$MIN_AVAILABLE_MEMORY_MB" \
        --max-agentsight-rss-mb "$MAX_AGENTSIGHT_RSS_MB")
    [[ -n "$METRICS_FILE" ]] && COLLECTOR_ARGS+=(--metrics-file "$METRICS_FILE")
    [[ -n "$AGENTS" ]] && COLLECTOR_ARGS+=(--duration 0 --start-file "$OUTPUT_DIR/agents/start.json")
    "$PYTHON_BIN" "$SCRIPT_DIR/collect_metrics.py" "${COLLECTOR_ARGS[@]}" &
    COLLECTOR_PID=$!
fi
if [[ "$PREPARE_CAPTURE" -eq 1 ]]; then
    : >"$OUTPUT_DIR/load-release.json"
fi

if [[ -n "$AGENTS" && -n "${VALIDATOR_PID:-}" ]]; then
    # Workers remain alive after publishing their load results until the
    # validator consumes queued TLS events, including the final usage/DONE.
    while kill -0 "$LOAD_PID" 2>/dev/null && kill -0 "$VALIDATOR_PID" 2>/dev/null; do
        [[ -f "$VALIDATION_MARKER" ]] && break
        sleep 0.1
    done
else
    wait "$LOAD_PID" || LOAD_STATUS=$?
    LOAD_PID=""
fi
if [[ -n "${COMPRESSOR_PID:-}" ]]; then
    wait "$COMPRESSOR_PID" || COMPRESSION_STATUS=$?
    COMPRESSOR_PID=""
fi
if [[ -n "${VALIDATOR_PID:-}" ]]; then
    : >"$VALIDATION_MARKER"
    wait "$VALIDATOR_PID" || VALIDATION_STATUS=$?
    VALIDATOR_PID=""
    : >"$VALIDATION_MARKER.release"
    cat "$OUTPUT_DIR/validation.log"
fi
if [[ -n "$LOAD_PID" ]]; then
    wait "$LOAD_PID" || LOAD_STATUS=$?
    LOAD_PID=""
fi
COLLECTOR_STATUS=0
FAULT_STATUS=0
if [[ -n "${COLLECTOR_PID:-}" ]] && kill -0 "$COLLECTOR_PID" 2>/dev/null; then
    kill -TERM "$COLLECTOR_PID" 2>/dev/null || true
fi
if [[ -n "${COLLECTOR_PID:-}" ]]; then
    wait "$COLLECTOR_PID" || COLLECTOR_STATUS=$?
    COLLECTOR_PID=""
fi
if [[ -n "${FAULT_PID:-}" ]]; then
    wait "$FAULT_PID" || FAULT_STATUS=$?
    FAULT_PID=""
fi
if [[ -f "$OUTPUT_DIR/safety-stop.json" ]]; then
    echo "benchmark safety guard stopped the load:" >&2
    cat "$OUTPUT_DIR/safety-stop.json" >&2
    LOAD_STATUS=75
fi
REPORT_ARGS=(--output "$OUTPUT_DIR/benchmark-report.md" --summary-output "$OUTPUT_DIR/run-summary.json" --protocol "$PROTOCOL" --qps "$QPS" --duration "$DURATION")
[[ -n "$LOAD_RESULTS" ]] && REPORT_ARGS+=(--load-results "$LOAD_RESULTS")
[[ -n "$LOAD_LOG" ]] && REPORT_ARGS+=(--load-log "$LOAD_LOG")
[[ -f "$OUTPUT_DIR/metrics.csv" ]] && REPORT_ARGS+=(--metrics "$OUTPUT_DIR/metrics.csv")
[[ -f "$OUTPUT_DIR/report.json" ]] && REPORT_ARGS+=(--validation-report "$OUTPUT_DIR/report.json")
[[ -n "$AGENTS" && -f "$OUTPUT_DIR/agent-summary.json" ]] && REPORT_ARGS+=(--agent-summary "$OUTPUT_DIR/agent-summary.json")
"$PYTHON_BIN" "$SCRIPT_DIR/render_report.py" "${REPORT_ARGS[@]}"
echo "benchmark artifacts: $OUTPUT_DIR"
[[ "$LOAD_STATUS" -eq 0 ]] || exit "$LOAD_STATUS"
[[ "$COMPRESSION_STATUS" -eq 0 ]] || exit "$COMPRESSION_STATUS"
[[ "$COLLECTOR_STATUS" -eq 0 ]] || exit "$COLLECTOR_STATUS"
[[ "$FAULT_STATUS" -eq 0 ]] || exit "$FAULT_STATUS"
[[ "${VALIDATION_STATUS:-0}" -eq 0 ]] || exit "$VALIDATION_STATUS"
