#!/usr/bin/env bash
# Sample daemon resources during a real-CLI health-advisory workload.
set -Eeuo pipefail

HELPER="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/health-advisory-metrics.py"

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
	cat <<'EOF'
Run an end-to-end ws-ckpt CLI workload against one already-running daemon.
Use dedicated Btrfs workspaces; this script never creates or removes baseline snapshots.

Required environment:
  WS_ROOT      Directory containing ws-u1 through ws-u50.
  DAEMON_PID   PID of the daemon to sample.

Optional environment:
  WS_CKPT_BIN            CLI executable (default: ws-ckpt)
  RUN_LABEL              A/B case label (default: unspecified)
  OUT_DIR                New or empty output directory (default: ./health-advisory-<label>-<UTC>)
  CLIENTS                Concurrent workspaces (default: 50)
  ITERATIONS             Cycles per client (default: 100)
  EXPECTED_SNAPSHOTS     Initial snapshots per workspace (default: 500)
  SAMPLE_INTERVAL_SECS   Resource sampling interval (default: 1)
  DRAIN_SECONDS          Continue sampling after the load (default: 30)

The measured window starts immediately before worker launch and ends after all workers
finish. Snapshot validation, the initial two-second settling delay, and the drain period
are outside the business latency/throughput window. Each cycle runs checkpoint, rollback,
and delete through the CLI. Per-operation latency percentiles use nearest-rank. Any
incomplete, interrupted, unresolved, or post-verification-failed run is marked invalid;
partial measurements remain in the output directory for diagnosis.

Write OUT_DIR to a dedicated, fast local filesystem. Run all A/B cases with the same
CLI binary, workspaces, output storage class, and logging setup. The throughput number
measures this end-to-end CLI workload, including coordinator and durable receipt work;
it is not a daemon-only rate. Finalization may remove snapshots whose receipts confirm
ownership; pending and unknown mutation receipts stay in place for manual reconciliation.

Run once with the parent daemon and once with the candidate daemon, keeping the CLI
binary, workspaces, output storage class, logging setup, and workload values identical.
EOF
	exit 0
fi

: "${WS_ROOT:?Set WS_ROOT to the dedicated workspace directory}"
: "${DAEMON_PID:?Set DAEMON_PID to the running daemon PID}"
WS_CKPT_BIN="${WS_CKPT_BIN:-ws-ckpt}"
RUN_LABEL="${RUN_LABEL:-unspecified}"
CLIENTS="${CLIENTS:-50}"
ITERATIONS="${ITERATIONS:-100}"
EXPECTED_SNAPSHOTS="${EXPECTED_SNAPSHOTS:-500}"
SAMPLE_INTERVAL_SECS="${SAMPLE_INTERVAL_SECS:-1}"
DRAIN_SECONDS="${DRAIN_SECONDS:-30}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT_DIR="${OUT_DIR:-$PWD/health-advisory-${RUN_LABEL}-${STAMP}}"

for value in "$CLIENTS" "$ITERATIONS" "$EXPECTED_SNAPSHOTS" "$SAMPLE_INTERVAL_SECS" "$DRAIN_SECONDS"; do
	[[ "$value" =~ ^[1-9][0-9]*$ ]] || { echo "Expected positive integer: $value" >&2; exit 2; }
done
daemon_alive() {
	[[ -r "/proc/$DAEMON_PID/status" ]] || return 1
	[[ "$(awk '/^State:/ {print $2}' "/proc/$DAEMON_PID/status" 2>/dev/null)" != Z ]]
}
[[ "$DAEMON_PID" =~ ^[1-9][0-9]*$ ]] && daemon_alive || {
	echo "DAEMON_PID=$DAEMON_PID is not a readable live process" >&2; exit 2;
}
command -v "$WS_CKPT_BIN" >/dev/null || { echo "CLI not found: $WS_CKPT_BIN" >&2; exit 2; }
CLI_RESOLVED="$(command -v "$WS_CKPT_BIN")"
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }
command -v setsid >/dev/null || { echo "setsid is required to stop workers safely" >&2; exit 2; }
[[ -r "$HELPER" ]] || { echo "Metrics helper is not readable: $HELPER" >&2; exit 2; }
[[ -d "$WS_ROOT" ]] || { echo "WS_ROOT is not a directory: $WS_ROOT" >&2; exit 2; }

if [[ -e "$OUT_DIR" ]]; then
	[[ -d "$OUT_DIR" ]] || { echo "OUT_DIR is not a directory: $OUT_DIR" >&2; exit 2; }
	[[ -z "$(find "$OUT_DIR" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
		echo "OUT_DIR must be empty to avoid overwriting prior evidence: $OUT_DIR" >&2; exit 2;
	}
else
	mkdir -p "$OUT_DIR"
fi
mkdir -p "$OUT_DIR/logs" "$OUT_DIR/active" "$OUT_DIR/snapshots-before" "$OUT_DIR/snapshots-after"
OUT_DIR="$(cd "$OUT_DIR" && pwd)"
WS_ROOT="$(cd "$WS_ROOT" && pwd)"
if [[ "$WS_CKPT_BIN" == */* ]]; then
	WS_CKPT_BIN="$(cd "$(dirname "$WS_CKPT_BIN")" && pwd)/$(basename "$WS_CKPT_BIN")"
fi
export WS_ROOT DAEMON_PID WS_CKPT_BIN OUT_DIR CLIENTS ITERATIONS

run_id="$(date -u +%s)-$$"
worker_failures=0
worker_pids=()
worker_waited=()
sampler_pid=''
interrupted=0
run_error=0
before_ok=0
started_ns=0
ended_ns=0
summary_rc=1
snapshot_verify_rc=1
sampler_failed=0
final_rc=0
finalized=0
signal_status=0

daemon_hash=""
cli_hash=""
if [[ -r "$CLI_RESOLVED" ]]; then cli_hash="$(sha256sum "$CLI_RESOLVED" | awk '{print $1}')"; fi
daemon_exe="/proc/$DAEMON_PID/exe"
if [[ -r "$daemon_exe" ]]; then daemon_hash="$(sha256sum "$daemon_exe" 2>/dev/null | awk '{print $1}' || true)"; fi
cpu_model="$(awk -F: '/^model name[[:space:]]*:/ {gsub(/^[[:space:]]+/, "", $2); print $2; exit}' /proc/cpuinfo 2>/dev/null || true)"
{
	printf 'run_label\t%s\ncli\t%s\ncli_resolved\t%s\ncli_sha256\t%s\n' \
		"$RUN_LABEL" "$WS_CKPT_BIN" "$CLI_RESOLVED" "${cli_hash:-unavailable}"
	printf 'workspace_root\t%s\noutput_dir\t%s\ndaemon_pid\t%s\ndaemon_exe_sha256\t%s\n' \
		"$WS_ROOT" "$OUT_DIR" "$DAEMON_PID" "${daemon_hash:-unavailable}"
	printf 'kernel\t%s\ncpu_model\t%s\nclients\t%s\niterations_per_client\t%s\n' \
		"$(uname -srmo)" "${cpu_model:-unavailable}" "$CLIENTS" "$ITERATIONS"
	printf 'expected_snapshots_per_workspace\t%s\nsample_interval_secs\t%s\ndrain_seconds\t%s\n' \
		"$EXPECTED_SNAPSHOTS" "$SAMPLE_INTERVAL_SECS" "$DRAIN_SECONDS"
	printf 'business_window_scope\tworker launch through final worker wait; excludes validation and drain\n'
	printf 'latency_percentile_method\tnearest-rank\nrun_id\t%s\nstarted_utc\t%s\n' "$run_id" "$STAMP"
} >"$OUT_DIR/metadata.tsv"
printf 'timestamp_utc\trss_kib\tthreads\tfd_total\tsocket_fds\tpipe_fds\tanon_inode_fds\tother_fds\n' >"$OUT_DIR/samples.tsv"
printf 'phase\tclient\tsnapshot_count\n' >"$OUT_DIR/snapshot-counts.tsv"
printf 'client\tstatus\tbefore_count\tafter_count\tmissing_ids\textra_ids\n' >"$OUT_DIR/snapshot-diff.tsv"
: >"$OUT_DIR/recovery.tsv"

snapshot_ids() {
	local workspace="$1" output="$2"
	"$WS_CKPT_BIN" list --workspace "$workspace" --format json |
		python3 -c 'import json,sys; values=json.load(sys.stdin); assert isinstance(values,list) and all(isinstance(x,dict) and isinstance(x.get("id"),str) and x["id"] for x in values); ids=sorted(x["id"] for x in values); assert len(ids)==len(set(ids)); sys.stdout.write("\n".join(ids) + ("\n" if ids else ""))' >"$output"
}

capture_before() {
	local i workspace path count
	for ((i=1; i<=CLIENTS; i++)); do
		workspace="$WS_ROOT/ws-u$i"
		[[ -d "$workspace" ]] || { echo "Missing workspace: $workspace" >&2; return 1; }
		path="$OUT_DIR/snapshots-before/client-$i.ids"
		if ! snapshot_ids "$workspace" "$path"; then
			echo "Could not capture baseline snapshot IDs for $workspace" >&2
			return 1
		fi
		count="$(python3 -c 'from pathlib import Path; import sys; print(len(Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()))' "$path")"
		printf 'before\t%s\t%s\n' "$i" "$count" >>"$OUT_DIR/snapshot-counts.tsv"
		[[ "$count" -eq "$EXPECTED_SNAPSHOTS" ]] || {
			echo "before: $workspace has $count snapshots; expected $EXPECTED_SNAPSHOTS" >&2
			return 1
		}
	done
	return 0
}

fd_counts() {
	local sockets=0 pipes=0 anon=0 other=0 fd target
	for fd in "/proc/$DAEMON_PID/fd/"*; do
		[[ -e "$fd" || -L "$fd" ]] || continue
		target="$(readlink "$fd" 2>/dev/null || true)"
		case "$target" in
			socket:*) sockets=$((sockets+1)) ;;
			pipe:*) pipes=$((pipes+1)) ;;
			anon_inode:*) anon=$((anon+1)) ;;
			*) other=$((other+1)) ;;
		esac
	done
	printf '%s\t%s\t%s\t%s' "$sockets" "$pipes" "$anon" "$other"
}

sample_daemon() {
	local timestamp rss threads fds types
	while kill -0 "$DAEMON_PID" 2>/dev/null && daemon_alive; do
		timestamp="$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)"
		rss="$(awk '/^VmRSS:/ {print $2}' "/proc/$DAEMON_PID/status")"
		threads="$(awk '/^Threads:/ {print $2}' "/proc/$DAEMON_PID/status")"
		fds="$(find "/proc/$DAEMON_PID/fd" -mindepth 1 -maxdepth 1 2>/dev/null | wc -l)"
		types="$(fd_counts)"
		printf '%s\t%s\t%s\t%s\t%s\n' "$timestamp" "${rss:-0}" "${threads:-0}" "$fds" "$types" >>"$OUT_DIR/samples.tsv"
		sleep "$SAMPLE_INTERVAL_SECS"
	done
}

stop_workers() {
	local pid index
	for index in "${!worker_pids[@]}"; do
		[[ "${worker_waited[$index]:-0}" == 1 ]] && continue
		pid="${worker_pids[$index]}"
		# A just-launched child may not have entered its new session yet.
		kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
	done
	for index in "${!worker_pids[@]}"; do
		[[ "${worker_waited[$index]:-0}" == 1 ]] && continue
		pid="${worker_pids[$index]}"
		if wait "$pid"; then :; else worker_failures=$((worker_failures+1)); fi
		worker_waited[$index]=1
	done
}

on_signal() {
	interrupted=1
	if [[ "$1" == TERM ]]; then signal_status=143; else signal_status=130; fi
	trap - TERM INT
	printf 'signal\t%s\t%s\n' "$1" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$OUT_DIR/run-events.tsv"
	stop_workers
	exit "$signal_status"
}
trap 'on_signal TERM' TERM
trap 'on_signal INT' INT

capture_after_and_compare() {
	local i workspace before after before_count after_count
	local compare_rc=0
	for ((i=1; i<=CLIENTS; i++)); do
		workspace="$WS_ROOT/ws-u$i"
		before="$OUT_DIR/snapshots-before/client-$i.ids"
		after="$OUT_DIR/snapshots-after/client-$i.ids"
		if ! snapshot_ids "$workspace" "$after"; then
			echo "Could not capture final snapshot IDs for $workspace" >&2
			printf '%s\tverification-failed\t\t\t\t\n' "$i" >>"$OUT_DIR/snapshot-diff.tsv"
			compare_rc=1
			continue
		fi
		before_count="$(python3 -c 'from pathlib import Path; import sys; print(len(Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()))' "$before")"
		after_count="$(python3 -c 'from pathlib import Path; import sys; print(len(Path(sys.argv[1]).read_text(encoding="utf-8").splitlines()))' "$after")"
		printf 'after\t%s\t%s\n' "$i" "$after_count" >>"$OUT_DIR/snapshot-counts.tsv"
		if python3 - "$before" "$after" "$i" "$before_count" "$after_count" "$OUT_DIR/snapshot-diff.tsv" <<'PY'
import csv
from pathlib import Path
import sys
before = set(Path(sys.argv[1]).read_text(encoding="utf-8").splitlines())
after = set(Path(sys.argv[2]).read_text(encoding="utf-8").splitlines())
missing = sorted(before - after)
extra = sorted(after - before)
with Path(sys.argv[6]).open("a", encoding="utf-8", newline="") as stream:
    csv.writer(stream, delimiter="\t", lineterminator="\n").writerow(
        [sys.argv[3], "match" if not missing and not extra else "mismatch", sys.argv[4], sys.argv[5], ",".join(missing), ",".join(extra)]
    )
raise SystemExit(0 if not missing and not extra else 1)
PY
		then :; else compare_rc=1; fi
	done
	return "$compare_rc"
}

finalize() {
	local client sampler_rc=0 sampler_alive=0 resource_rc=0 sample_count=0
	[[ "$finalized" == 1 ]] && return
	finalized=1
	stop_workers
	if [[ -n "$sampler_pid" ]]; then
		if kill -0 "$sampler_pid" 2>/dev/null; then
			sampler_alive=1
			kill "$sampler_pid" 2>/dev/null || true
			if wait "$sampler_pid"; then sampler_rc=0; else sampler_rc=$?; fi
			[[ "$sampler_rc" == 0 || "$sampler_rc" == 143 || "$sampler_rc" == 130 ]] || sampler_failed=1
		else
			sampler_failed=1
		fi
		sampler_pid=''
	fi
	daemon_alive || sampler_failed=1
	if (( started_ns == 0 )); then
		started_ns="$(python3 -c 'import time; print(time.monotonic_ns())')"
	fi
	if (( ended_ns == 0 )); then
		ended_ns="$(python3 -c 'import time; print(time.monotonic_ns())')"
	fi
	for ((client=1; client<=CLIENTS; client++)); do
		if ! python3 "$HELPER" cleanup "$client" >>"$OUT_DIR/recovery.tsv" 2>&1; then
			printf 'client=%s marker=%s\n' "$client" "$OUT_DIR/active/client-$client.snapshot" >>"$OUT_DIR/recovery.tsv"
			final_rc=1
		fi
	done
	if (( before_ok == 1 )); then
		if ! capture_after_and_compare; then
			snapshot_verify_rc=1
			final_rc=1
		else
			snapshot_verify_rc=0
		fi
	fi
	if python3 "$HELPER" summarize \
		--out-dir "$OUT_DIR" --clients "$CLIENTS" --iterations "$ITERATIONS" \
		--started-ns "$started_ns" --ended-ns "$ended_ns" \
		--worker-failures "$worker_failures" --interrupted "$interrupted"; then
		summary_rc=0
	else
		summary_rc=$?
		final_rc=1
	fi
if ! python3 - "$OUT_DIR/samples.tsv" "$OUT_DIR/summary.tsv" "$started_ns" "$ended_ns" "$worker_failures" "$DRAIN_SECONDS" "$sampler_rc" <<'PY'
import csv, sys
samples_path, summary_path, started, ended, failures, drain, sampler_rc = sys.argv[1:]
with open(samples_path, newline="", encoding="utf-8") as stream:
    rows = list(csv.DictReader(stream, delimiter="\t"))
fields = ("rss_kib", "threads", "fd_total", "socket_fds", "pipe_fds", "anon_inode_fds", "other_fds")
with open(summary_path, "w", encoding="utf-8") as stream:
    stream.write(f"measurement_window_ns\t{int(ended) - int(started)}\ndrain_seconds\t{drain}\nworker_failures\t{failures}\nsampler_exit_code\t{sampler_rc}\nsamples\t{len(rows)}\n")
    for field in fields:
        peak = max((int(row[field]) for row in rows), default=0)
        stream.write(f"peak_{field}\t{peak}\n")
PY
	then resource_rc=1; fi
	if (( resource_rc == 0 )); then
		sample_count="$(awk -F '\t' '$1 == "samples" {print $2}' "$OUT_DIR/summary.tsv" | tail -n 1)"
	fi
	[[ "$summary_rc" == 0 && "$snapshot_verify_rc" == 0 && "$worker_failures" == 0 && "$interrupted" == 0 && "$run_error" == 0 && "$sampler_failed" == 0 && "$resource_rc" == 0 && "$sample_count" =~ ^[1-9][0-9]*$ ]] || final_rc=1
	printf 'started_monotonic_ns\t%s\nended_monotonic_ns\t%s\n' "$started_ns" "$ended_ns" >>"$OUT_DIR/metadata.tsv" || final_rc=1
	{
		printf 'metric\tvalue\nrun_status\t%s\nworker_failures\t%s\ninterrupted\t%s\n' \
			"$([[ "$final_rc" == 0 ]] && echo valid || echo invalid)" \
			"$worker_failures" "$interrupted"
		printf 'snapshot_verification\t%s\nsummary_status\t%s\n' \
			"$([[ "$snapshot_verify_rc" == 0 ]] && echo passed || echo failed-or-not-run)" \
			"$([[ "$summary_rc" == 0 ]] && echo valid || echo invalid)"
		printf 'sampler_status\t%s\nsampler_exit_code\t%s\nsamples\t%s\n' \
			"$([[ "$sampler_failed" == 0 && "$resource_rc" == 0 && "$sample_count" =~ ^[1-9][0-9]*$ ]] && echo valid || echo invalid)" \
			"$sampler_rc" "${sample_count:-0}"
		printf 'ended_utc\t%s\nended_monotonic_ns\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$ended_ns"
	} >"$OUT_DIR/run-status.tsv" || final_rc=1
}

trap 'script_status=$?; trap - EXIT; (( script_status == 0 )) || run_error=1; finalize; if (( signal_status != 0 )); then exit "$signal_status"; elif (( final_rc != 0 )); then exit 1; else exit "$script_status"; fi' EXIT

if ! capture_before; then
	run_error=1
	exit 1
fi
before_ok=1
sleep 2

sample_daemon & sampler_pid=$!
started_ns="$(python3 -c 'import time; print(time.monotonic_ns())')"
printf 'started_utc\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >>"$OUT_DIR/metadata.tsv"
for ((i=1; i<=CLIENTS; i++)); do
	setsid python3 "$HELPER" worker "$i" "$run_id" &
	worker_pids+=("$!")
	worker_waited+=(0)
done

for index in "${!worker_pids[@]}"; do
	[[ "${worker_waited[$index]:-0}" == 1 ]] && continue
	pid="${worker_pids[$index]}"
	if wait "$pid"; then :; else
		worker_failures=$((worker_failures+1))
	fi
	worker_waited[$index]=1
done
ended_ns="$(python3 -c 'import time; print(time.monotonic_ns())')"
sleep "$DRAIN_SECONDS" || true
exit 0
