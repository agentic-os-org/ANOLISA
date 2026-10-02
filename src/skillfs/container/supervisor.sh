#!/usr/bin/env bash
#
# PID 1 supervisor for the SkillFS Kubernetes sidecar.
#
# The mount command is passed as argv. Health and cleanup deliberately remain
# in the existing probe and preflight helpers so the container has one source
# of truth for real-I/O health and residual FUSE handling.

set -uo pipefail

PROBE_BIN="${SKILLFS_SUPERVISOR_PROBE_BIN:-/usr/local/bin/skillfs-mount-probe}"
PREFLIGHT_BIN="${SKILLFS_SUPERVISOR_PREFLIGHT_BIN:-/usr/local/bin/skillfs-preflight}"
PROBE_INTERVAL="${SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS:-2}"
FAILURE_THRESHOLD="${SKILLFS_SUPERVISOR_FAILURE_THRESHOLD:-2}"
STABLE_HEALTHY_PROBES="${SKILLFS_SUPERVISOR_STABLE_HEALTHY_PROBES:-3}"
STARTUP_TIMEOUT="${SKILLFS_SUPERVISOR_STARTUP_TIMEOUT_SECONDS:-30}"
STOP_TIMEOUT="${SKILLFS_SUPERVISOR_STOP_TIMEOUT_SECONDS:-10}"
MAX_FAILED_ATTEMPTS="${SKILLFS_SUPERVISOR_MAX_FAILED_ATTEMPTS:-5}"
BACKOFF_INITIAL="${SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS:-1}"
BACKOFF_MAX="${SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS:-30}"

child_pid=""
sleep_pid=""
terminating=0
termination_signal="TERM"
stable_health=0

log() {
	printf '[skillfs-supervisor] %s\n' "$*" >&2
}

die() {
	log "FAIL: $*"
	exit 64
}

is_positive_decimal() {
	[[ "$1" =~ ^([0-9]+)([.][0-9]+)?$ ]] &&
		awk -v value="$1" 'BEGIN { exit !(value > 0) }'
}

is_positive_integer() {
	[[ "$1" =~ ^[0-9]+$ ]] && ((10#$1 > 0))
}

decimal_is_at_most() {
	awk -v value="$1" -v maximum="$2" 'BEGIN { exit !(value <= maximum) }'
}

validate_helper() {
	local name="$1"
	local path="$2"
	[[ "$path" == /* ]] || die "$name must be an absolute path, got '$path'"
	[[ -x "$path" ]] || die "$name '$path' is missing or not executable"
}

validate_config() {
	(($# > 0)) || die "no mount command supplied"
	validate_helper SKILLFS_SUPERVISOR_PROBE_BIN "$PROBE_BIN"
	validate_helper SKILLFS_SUPERVISOR_PREFLIGHT_BIN "$PREFLIGHT_BIN"
	is_positive_decimal "$PROBE_INTERVAL" ||
		die "SKILLFS_SUPERVISOR_PROBE_INTERVAL_SECONDS must be positive, got '$PROBE_INTERVAL'"
	is_positive_integer "$FAILURE_THRESHOLD" ||
		die "SKILLFS_SUPERVISOR_FAILURE_THRESHOLD must be a positive integer, got '$FAILURE_THRESHOLD'"
	is_positive_integer "$STABLE_HEALTHY_PROBES" ||
		die "SKILLFS_SUPERVISOR_STABLE_HEALTHY_PROBES must be a positive integer, got '$STABLE_HEALTHY_PROBES'"
	is_positive_integer "$STARTUP_TIMEOUT" ||
		die "SKILLFS_SUPERVISOR_STARTUP_TIMEOUT_SECONDS must be a positive integer, got '$STARTUP_TIMEOUT'"
	is_positive_integer "$STOP_TIMEOUT" ||
		die "SKILLFS_SUPERVISOR_STOP_TIMEOUT_SECONDS must be a positive integer, got '$STOP_TIMEOUT'"
	is_positive_integer "$MAX_FAILED_ATTEMPTS" ||
		die "SKILLFS_SUPERVISOR_MAX_FAILED_ATTEMPTS must be a positive integer, got '$MAX_FAILED_ATTEMPTS'"
	is_positive_decimal "$BACKOFF_INITIAL" ||
		die "SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS must be positive, got '$BACKOFF_INITIAL'"
	is_positive_decimal "$BACKOFF_MAX" ||
		die "SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS must be positive, got '$BACKOFF_MAX'"
	decimal_is_at_most "$BACKOFF_INITIAL" "$BACKOFF_MAX" ||
		die "SKILLFS_SUPERVISOR_BACKOFF_INITIAL_SECONDS ($BACKOFF_INITIAL) must not exceed SKILLFS_SUPERVISOR_BACKOFF_MAX_SECONDS ($BACKOFF_MAX)"
}

child_is_running() {
	local running_pid
	[[ -n "$child_pid" ]] || return 1
	# Unlike `kill -0`, Bash's running-jobs view excludes an exited child that
	# has not been waited yet. That distinction prevents zombie workers from
	# consuming the entire stop timeout or masking an unexpected exit.
	while read -r running_pid; do
		[[ "$running_pid" == "$child_pid" ]] && return 0
	done < <(jobs -pr)
	return 1
}

reap_child() {
	local status=0
	if [[ -n "$child_pid" ]]; then
		wait "$child_pid" 2>/dev/null || status=$?
		log "mount worker pid=$child_pid exited with status $status"
		child_pid=""
	fi
	return "$status"
}

forward_signal() {
	termination_signal="$1"
	terminating=1
	log "received SIG$termination_signal; stopping without remount"
	# Avoid child_is_running here: its process substitution changes $! if a
	# signal interrupts worker or sleeper launch before PID assignment.
	if [[ -n "$child_pid" ]]; then
		kill -s "$termination_signal" "$child_pid" 2>/dev/null || true
	fi
	if [[ -n "$sleep_pid" ]]; then
		# Non-interactive background jobs inherit SIGINT as ignored.
		kill -TERM "$sleep_pid" 2>/dev/null || true
	fi
}

trap 'forward_signal TERM' TERM
trap 'forward_signal INT' INT

sleep_interruptibly() {
	local duration="$1"
	local status=0
	((terminating == 0)) || return 1
	sleep "$duration" &
	sleep_pid=$!
	# The trap can run after launch but before the sleeper PID is recorded.
	if ((terminating != 0)); then
		kill -TERM "$sleep_pid" 2>/dev/null || true
	fi
	# `wait` is a Bash builtin, so the signal trap runs immediately instead of
	# being deferred until a long backoff sleep completes. Waiting a second time
	# reaps the sleeper when the first wait was interrupted by that trap.
	wait "$sleep_pid" 2>/dev/null || status=$?
	if ((status > 128)); then
		wait "$sleep_pid" 2>/dev/null || true
	fi
	sleep_pid=""
	((terminating == 0))
}

run_preflight() {
	if [[ "${SKILLFS_SKIP_PREFLIGHT:-0}" == "1" ]]; then
		log "WARNING: SKILLFS_SKIP_PREFLIGHT=1; skipping preflight"
		return 0
	fi
	log "running preflight before mount attempt"
	"$PREFLIGHT_BIN"
}

run_probe() {
	local mode="$1"
	local output
	local status
	if output="$("$PROBE_BIN" "--$mode" 2>&1)"; then
		return 0
	else
		status=$?
	fi
	if [[ -n "$output" ]]; then
		while IFS= read -r line; do
			log "probe diagnostic: $line"
		done <<<"$output"
	fi
	return "$status"
}

start_child() {
	log "starting mount worker: $*"
	"$@" &
	child_pid=$!
	log "mount worker started pid=$child_pid"
}

# Stop and reap the worker within a bounded budget. A TERM already forwarded by
# the signal trap is harmless; repeating it closes the race with child startup.
stop_child() {
	local reason="$1"
	local ticks=$((10#$STOP_TIMEOUT * 10))
	local tick

	if [[ -z "$child_pid" ]]; then
		return 0
	fi
	if child_is_running; then
		log "stopping mount worker pid=$child_pid ($reason)"
		kill -TERM "$child_pid" 2>/dev/null || true
		for ((tick = 0; tick < ticks; tick++)); do
			child_is_running || break
			sleep 0.1 || true
		done
	fi
	if child_is_running; then
		log "mount worker pid=$child_pid exceeded ${STOP_TIMEOUT}s; sending SIGKILL"
		kill -KILL "$child_pid" 2>/dev/null || true
	fi
	reap_child || true
}

cleanup_after_stop() {
	if [[ "${SKILLFS_SKIP_PREFLIGHT:-0}" == "1" ]]; then
		return 0
	fi
	log "running preflight to clear any residual FUSE mount"
	if ! "$PREFLIGHT_BIN" --cleanup-only; then
		log "WARNING: post-stop preflight failed; residual mount may require kubelet recovery"
		return 1
	fi
}

# Return 0 only after real I/O succeeds. Probe exit 2 (not mounted) is expected
# while the first FUSE session is still coming up and consumes the startup
# timeout rather than the runtime failure threshold.
wait_until_healthy() {
	local started_at=$SECONDS
	local probe_status

	while ((terminating == 0)); do
		if ! child_is_running; then
			reap_child || true
			cleanup_after_stop || true
			return 1
		fi
		if run_probe startup; then
			log "mount worker pid=$child_pid passed initial real-I/O health check"
			return 0
		else
			probe_status=$?
			log "startup probe failed with status $probe_status; waiting for mount readiness"
		fi
		if ((SECONDS - started_at >= 10#$STARTUP_TIMEOUT)); then
			log "mount worker pid=$child_pid did not become healthy within ${STARTUP_TIMEOUT}s"
			stop_child "startup timeout"
			cleanup_after_stop || true
			return 1
		fi
		sleep_interruptibly "$PROBE_INTERVAL" || return 2
	done
	return 2
}

# Monitor a previously healthy mount. Returns only when it must be remounted or
# the supervisor is terminating.
monitor_healthy_child() {
	local failures=0
	local healthy_probes=0
	local probe_status
	stable_health=0

	while ((terminating == 0)); do
		sleep_interruptibly "$PROBE_INTERVAL" || return 2
		if ! child_is_running; then
			reap_child || true
			log "healthy mount worker exited unexpectedly; scheduling remount"
			cleanup_after_stop || true
			return 1
		fi
		if run_probe liveness; then
			if ((failures > 0)); then
				log "mount health recovered after $failures failed probe(s)"
			fi
			failures=0
			if ((stable_health == 0)); then
				((healthy_probes += 1))
				if ((healthy_probes >= 10#$STABLE_HEALTHY_PROBES)); then
					stable_health=1
					log "mount remained healthy for $healthy_probes consecutive liveness probes; recovery failure budget reset"
				fi
			fi
			continue
		else
			probe_status=$?
		fi
		((failures += 1))
		healthy_probes=0
		log "liveness probe failed with status $probe_status ($failures/$FAILURE_THRESHOLD)"
		if ((failures >= 10#$FAILURE_THRESHOLD)); then
			log "mount declared unhealthy after $failures consecutive probe failures"
			stop_child "unhealthy mount"
			cleanup_after_stop || true
			return 1
		fi
	done
	return 2
}

shutdown() {
	trap - TERM INT
	stop_child "supervisor shutdown"
	cleanup_after_stop || true
	log "shutdown complete after SIG$termination_signal"
	exit 0
}

main() {
	validate_config "$@"
	local failed_attempts=0
	local backoff="$BACKOFF_INITIAL"
	local startup_status

	log "supervision enabled: interval=${PROBE_INTERVAL}s threshold=$FAILURE_THRESHOLD stable_probes=$STABLE_HEALTHY_PROBES startup_timeout=${STARTUP_TIMEOUT}s max_failed_attempts=$MAX_FAILED_ATTEMPTS"
	while ((terminating == 0)); do
		if run_preflight; then
			# A termination signal may arrive while preflight is running. Do not
			# launch a fresh FUSE worker after the signal has been observed.
			((terminating == 0)) || break
			start_child "$@"
			wait_until_healthy
			startup_status=$?
			if ((startup_status == 0)); then
				monitor_healthy_child
				startup_status=$?
				if ((startup_status == 2)); then
					break
				fi
				if ((stable_health == 1)); then
					failed_attempts=0
					backoff="$BACKOFF_INITIAL"
				fi
			fi
			if ((startup_status == 2)); then
				break
			fi
		else
			log "preflight failed with status $?"
		fi

		((failed_attempts += 1))
		if ((failed_attempts >= 10#$MAX_FAILED_ATTEMPTS)); then
			log "FAIL: exhausted $failed_attempts consecutive failed mount attempts"
			exit 1
		fi
		log "mount or recovery cycle failed ($failed_attempts/$MAX_FAILED_ATTEMPTS); retrying in ${backoff}s"
		sleep_interruptibly "$backoff" || break
		backoff="$(awk -v current="$backoff" -v maximum="$BACKOFF_MAX" 'BEGIN { value = current * 2; if (value > maximum) value = maximum; print value }')"
	done
	shutdown
}

main "$@"
