#!/usr/bin/env bash
# Copyright 2026 Alibaba Cloud
# Licensed under the Apache License, Version 2.0.
#
# Drive the L4 sweep across several hosts. The sibling scripts each act on one
# host by design -- they are the unit that gets debugged when a host misbehaves --
# and this script is the loop around them plus the one piece of state they cannot
# hold: which slice of the instance list each host owns.
#
# Sharding, and why every host runs every arm. The comparison between arms is
# paired within an instance, so an instance must meet all its arms on the same
# host. Splitting instances across hosts and keeping the arms together makes the
# host a property of the pair, which cancels in the difference; splitting arms
# across hosts instead would put the machine inside the effect being measured.
#
# Required environment:
#   L4_HOSTS            comma-separated hosts, in shard order: the first host
#                       runs shard 1, and a host's position is therefore part of
#                       the run's identity -- do not reorder between phases
#   L4_SSH_PASS         ssh password, shared by the fleet
#   DASHSCOPE_API_KEY   provider key; needed by `dryrun` and `run` only. May be
#                       omitted when L4_DASHSCOPE_KEYS supplies a key per host.
# Optional:
#   L4_SSH_USER         remote user             (default: root)
#   L4_REMOTE_WORK      remote workspace root   (default: /root/l4)
#   L4_SHARD_PREFIX     shard list basename     (default: n48-shard)
#   L4_ARMS             passed through to remote_run.sh
#   L4_TIMEOUT          passed through to remote_run.sh
#   L4_DASHSCOPE_KEYS   comma-separated per-host keys in shard order; each host
#                       uses its own instead of the shared DASHSCOPE_API_KEY
#
# Phases, in order:
#   sync     push both checkouts and the prompt corpus, and install each host's
#            shard as $L4_REMOTE_WORK/l4_instances.txt
#   setup    build the venvs, node and plugins on every host
#   verify   run remote_verify.sh's preconditions on every host
#   dryrun   start every proxy on every host and prove each arm's activation
#            without spending tokens
#   run      launch the billed sweep detached on every host
#   status   report each host's progress; safe to repeat while `run` is live
#   collect  pull the sweep outputs back into ./runs/<host>/
#
# Phases are deliberately separate commands rather than one pipeline: `run` is the
# only one that costs money, and everything before it exists so that a
# misconfiguration is found while it is still free.

set -uo pipefail

: "${L4_HOSTS:?L4_HOSTS is required (comma-separated, in shard order)}"
# Not required up front: with the key present it is never used, and demanding it
# anyway would keep the password in the environment of every run that has no use
# for it. The fallback branch below asks for it only when it is actually needed.
L4_SSH_PASS="${L4_SSH_PASS:-}"
L4_SSH_USER="${L4_SSH_USER:-root}"
L4_REMOTE_WORK="${L4_REMOTE_WORK:-/root/l4}"
L4_SHARD_PREFIX="${L4_SHARD_PREFIX:-n48-shard}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTANCE_DIR="$(cd "$SCRIPT_DIR/../instances" && pwd)"
WORKSPACE="$(cd "$SCRIPT_DIR/../.." && pwd)"
REMOTE_SCRIPTS=$L4_REMOTE_WORK/anolisa/src/tokenless/benchmark/l4-end-to-end/assets/scripts

IFS=, read -ra HOSTS <<< "$L4_HOSTS"
[ "${#HOSTS[@]}" -gt 0 ] || { echo "error: L4_HOSTS is empty" >&2; exit 1; }

# Per-host provider keys, in the same shard order as L4_HOSTS. The four keys we
# hold share one billing account, so this buys no extra account-level throughput;
# it exists to honor the one-key-per-host layout and to keep a single key being
# throttled or revoked from taking down more than its own host. When unset, every
# host falls back to the shared DASHSCOPE_API_KEY.
DASHSCOPE_KEYS=()
if [ -n "${L4_DASHSCOPE_KEYS:-}" ]; then
    IFS=, read -ra DASHSCOPE_KEYS <<< "$L4_DASHSCOPE_KEYS"
    [ "${#DASHSCOPE_KEYS[@]}" -eq "${#HOSTS[@]}" ] || {
        echo "error: L4_DASHSCOPE_KEYS has ${#DASHSCOPE_KEYS[@]} keys, L4_HOSTS has ${#HOSTS[@]} hosts" >&2
        exit 1
    }
fi

# Key for the host at 1-based shard index $1: the per-host list if given, else
# the shared key.
key_for() {
    if [ "${#DASHSCOPE_KEYS[@]}" -gt 0 ]; then
        printf '%s' "${DASHSCOPE_KEYS[$(($1 - 1))]}"
    else
        printf '%s' "${DASHSCOPE_API_KEY:-}"
    fi
}

# A billed or activation phase needs at least one source of keys.
require_keys() {
    [ "${#DASHSCOPE_KEYS[@]}" -gt 0 ] || [ -n "${DASHSCOPE_API_KEY:-}" ] || {
        echo "error: $1 needs DASHSCOPE_API_KEY or L4_DASHSCOPE_KEYS" >&2
        exit 1
    }
}

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null
          -o ConnectTimeout=30 -o LogLevel=ERROR)

# Authentication: key first, sshpass only as a bootstrap fallback.
#
# sshpass allocates a pty for every connection, and that setup races when several
# run in the background at once. It surfaced as ssh dying with "Bad file
# descriptor" on two of four parallel hosts while the other two transferred
# normally -- a failure that looks like a network fault and is not one. A key
# removes the race outright and keeps the password off every command line.
#
# The key options go into SSH_OPTS rather than a separate array so that rsync's
# `-e "ssh ${SSH_OPTS[*]}"` inherits them too. Note that password pinning has to
# move into the fallback branch: PubkeyAuthentication=no would otherwise disable
# the very key being supplied.
L4_SSH_KEY=${L4_SSH_KEY:-$HOME/.ssh/l4_eval_ed25519}
if [ -f "$L4_SSH_KEY" ]; then
    SSH_OPTS+=(-i "$L4_SSH_KEY" -o IdentitiesOnly=yes)
    SSH_PW=()
else
    SSH_OPTS+=(-o PreferredAuthentications=password -o PubkeyAuthentication=no)
    : "${L4_SSH_PASS:?L4_SSH_PASS is required when $L4_SSH_KEY does not exist}"
    SSH_PW=(sshpass -p "$L4_SSH_PASS")
fi

say() { echo "[fleet] $*"; }
rsh() { ${SSH_PW[@]+"${SSH_PW[@]}"} ssh "${SSH_OPTS[@]}" "$L4_SSH_USER@$1" "${@:2}"; }

# Every shard must exist before the first host is touched: discovering shard 4 is
# missing after three hosts have been provisioned wastes the provisioning.
check_shards() {
    local i host missing=0
    for i in "${!HOSTS[@]}"; do
        host=${HOSTS[$i]}
        local shard=$INSTANCE_DIR/$L4_SHARD_PREFIX$((i + 1)).txt
        if [ ! -s "$shard" ]; then
            echo "error: no shard list for host $host at $shard" >&2
            missing=1
        fi
    done
    [ "$missing" -eq 0 ] || exit 1
}

# Shards must partition the list, not merely cover it. An id present in two
# shards would be run twice and counted twice; an id in none would silently
# shrink the denominator while every per-host check still passed.
check_partition() {
    local all=$INSTANCE_DIR/${L4_SHARD_PREFIX%-shard}-all.txt
    [ -s "$all" ] || { say "no combined list at $all; partition unchecked"; return 0; }
    local shards=()
    local i
    for i in "${!HOSTS[@]}"; do shards+=("$INSTANCE_DIR/$L4_SHARD_PREFIX$((i + 1)).txt"); done
    local union dups
    union=$(cat "${shards[@]}" | grep '[^[:space:]]' | sort)
    dups=$(printf '%s\n' "$union" | uniq -d)
    [ -z "$dups" ] || { echo "error: instances in more than one shard: $dups" >&2; exit 1; }
    if ! diff -q <(printf '%s\n' "$union") <(grep '[^[:space:]]' "$all" | sort) >/dev/null; then
        echo "error: shards do not add up to $all" >&2
        diff <(printf '%s\n' "$union") <(grep '[^[:space:]]' "$all" | sort) | head -10 >&2
        exit 1
    fi
    say "shards partition $(printf '%s\n' "$union" | wc -l | tr -d ' ') instances across ${#HOSTS[@]} hosts"
}

# ---------------------------------------------------------------------------
# Phases
# ---------------------------------------------------------------------------
# Runs <fn> once per host, concurrently, printing a periodic one-line-per-host
# digest instead of four interleaved output streams.
#
# Shared by every phase that touches all four hosts, because the argument is the
# same for each: the hosts are independent, so serial execution makes the wall
# clock the sum of four waits instead of the longest one -- and sync alone went
# from 25 minutes for a fraction of one host to 151 seconds for all four.
#
# The digest exists because silence is ambiguous: a slow link and a dead one look
# identical from outside, and that ambiguity already caused a healthy sync to be
# killed as a suspected hang. Each host's full output stays in its own log.
#
# $1 phase label (also the log directory name), $2 function taking (host, index)
run_parallel() {
    local label=$1 fn=$2
    # A unique dir per invocation: a fixed ${TMPDIR}/l4-fleet-<label> path meant
    # two concurrent fleet runs of the same phase would rm -rf each other's logs.
    local logdir
    logdir=$(mktemp -d "${TMPDIR:-/tmp}/l4-fleet-$label.XXXXXX")

    local started=$SECONDS
    local i host pids=()
    for i in "${!HOSTS[@]}"; do
        host=${HOSTS[$i]}
        "$fn" "$host" "$((i + 1))" > "$logdir/$host.log" 2>&1 &
        pids+=("$!")
        say "[$((i + 1))/${#HOSTS[@]}] $label launched on $host"
    done
    say "logs: $logdir/<host>.log"

    local alive=1 p
    while [ "$alive" -eq 1 ]; do
        sleep 30
        alive=0
        # An `if` rather than `kill -0 ... && alive=1`: the latter leaves the loop's
        # exit status non-zero as soon as one child has finished, which is a trap
        # for whoever later adds `set -e` to this script or reuses the loop
        # somewhere that has it.
        for p in "${pids[@]}"; do
            if kill -0 "$p" 2>/dev/null; then alive=1; fi
        done
        say "--- $label $((SECONDS - started))s elapsed ---"
        for host in "${HOSTS[@]}"; do
            # The last line matching the per-host scripts' "[stage] ..." prefix,
            # not the last line outright: ssh writes its own warnings to the same
            # file, and one of them landing after a progress line would hide the
            # only informative part of the digest. Those warnings are suppressed
            # by LogLevel=ERROR now, but the digest should not depend on the
            # remote sshd never having anything to say.
            printf '[fleet]   %-16s %s\n' "$host" \
                "$(grep '^\[' "$logdir/$host.log" 2>/dev/null | tail -1 | tr -d '\r')"
        done
    done

    local rc=0 idx=0
    for p in "${pids[@]}"; do
        wait "$p" || { say "$label FAILED on ${HOSTS[$idx]} -- see $logdir/${HOSTS[$idx]}.log"; rc=1; }
        idx=$((idx + 1))
    done
    [ "$rc" -ne 0 ] || say "$label complete on all ${#HOSTS[@]} hosts in $((SECONDS - started))s"
    return "$rc"
}

# One host's complete sync. The shard is installed under the name remote_run.sh
# defaults to, so the sweep on each host cannot be pointed at another host's
# slice by accident.
sync_one() {
    local host=$1 idx=$2
    local shard=$INSTANCE_DIR/$L4_SHARD_PREFIX$idx.txt
    L4_SSH_HOST=$host L4_SSH_PASS=$L4_SSH_PASS L4_SSH_USER=$L4_SSH_USER \
        L4_REMOTE_WORK=$L4_REMOTE_WORK \
        bash "$SCRIPT_DIR/remote_sync.sh" || return 1
    ${SSH_PW[@]+"${SSH_PW[@]}"} scp "${SSH_OPTS[@]}" "$shard" \
        "$L4_SSH_USER@$host:$L4_REMOTE_WORK/l4_instances.txt" >/dev/null || return 1
    rsh "$host" "printf 'shard=%s\nhost_index=%s\n' '$(basename "$shard")' '$idx' \
        > $L4_REMOTE_WORK/shard.provenance" || return 1
    echo "ready: $(grep -c '[^[:space:]]' "$shard") instances installed"
}

setup_one() {
    local host=$1
    L4_SSH_HOST=$host L4_SSH_PASS=$L4_SSH_PASS L4_SSH_USER=$L4_SSH_USER \
        L4_REMOTE_WORK=$L4_REMOTE_WORK \
        bash "$SCRIPT_DIR/remote_setup.sh" || return 1
    echo "setup complete"
}

verify_one() {
    local host=$1
    L4_SSH_HOST=$host L4_SSH_PASS=$L4_SSH_PASS L4_SSH_USER=$L4_SSH_USER \
        L4_REMOTE_WORK=$L4_REMOTE_WORK \
        bash "$SCRIPT_DIR/remote_verify.sh" || return 1
    echo "verify passed"
}

phase_sync() { run_parallel sync sync_one; }

phase_setup() { run_parallel setup setup_one; }

# Verify does not stop at the first bad host: knowing that three of four are
# misconfigured in the same way is a different problem from one being broken, and
# run_parallel already collects every host's status.
phase_verify() { run_parallel verify verify_one; }

# The dry run is short enough to hold the connection, so it runs in the
# foreground on each host and its verdict is read immediately.
phase_dryrun() {
    require_keys dryrun
    local i host key rc=0
    for i in "${!HOSTS[@]}"; do
        host=${HOSTS[$i]}
        key=$(key_for $((i + 1)))
        say "dryrun $host"
        # Delivered over stdin (bash -s), not as an ssh argument, so the provider
        # key stays out of the local ssh and remote shell argv (readable via ps /
        # /proc/<pid>/cmdline). L4_REMOTE_WORK is passed through so a custom
        # workspace root reaches remote_run.sh, matching sync/setup/verify_one.
        rsh "$host" bash -s <<REMOTE 2>&1 | sed "s/^/  [$host] /"
DASHSCOPE_API_KEY='$key' L4_DRY_RUN=1 L4_REMOTE_WORK=$L4_REMOTE_WORK \\
    L4_REPEAT=${L4_REPEAT:-1} ${L4_ARMS:+L4_ARMS=$L4_ARMS} \\
    bash $REMOTE_SCRIPTS/remote_run.sh
REMOTE
        [ "${PIPESTATUS[0]}" -eq 0 ] || { say "dryrun FAILED on $host"; rc=1; }
    done
    return "$rc"
}

# Guard and launch one host in a single ssh, so a connection that drops mid-way
# cannot start a sweep the guard did not clear. Emits exactly one token on stdout:
# LAUNCHED (started here), ALREADY_RUNNING (a sweep was already live), or nothing
# useful when the transport itself failed before reaching the host.
#
# Delivered over stdin (bash -s) rather than as an ssh argument for two reasons:
# the guard string "remote_run.sh" stays out of the wrapper shell's argv so the
# check cannot match itself, and the macOS ssh client -- which intermittently
# aborts a fresh connection with "Bad file descriptor" before it reaches the host
# -- has been reliable on this stdin channel where the argument channel was not.
launch_one() {
    local host=$1 key=$2
    rsh "$host" bash -s <<REMOTE 2>&1
if ps -eo cmd= | grep -F remote_run.sh | grep -qv grep; then
    echo ALREADY_RUNNING; exit 0
fi
mkdir -p $L4_REMOTE_WORK/logs || { echo MKDIR_FAILED; exit 1; }
# setsid so the sweep survives this ssh session; the key travels in the
# environment and is never written to the host's disk.
DASHSCOPE_API_KEY='$key' L4_REMOTE_WORK=$L4_REMOTE_WORK \
    L4_REPEAT=${L4_REPEAT:-1} ${L4_ARMS:+L4_ARMS=$L4_ARMS} \
    ${L4_TIMEOUT:+L4_TIMEOUT=$L4_TIMEOUT} \
    setsid nohup bash $REMOTE_SCRIPTS/remote_run.sh \
    > $L4_REMOTE_WORK/logs/sweep.repeat${L4_REPEAT:-1}.log 2>&1 < /dev/null &
echo LAUNCHED
REMOTE
}

phase_run() {
    require_keys run
    local i host key out attempt
    for i in "${!HOSTS[@]}"; do
        host=${HOSTS[$i]}
        key=$(key_for $((i + 1)))
        # Retry the atomic launch on transport failure. Because guard and launch
        # share one connection, a failed attempt has provably started nothing, so a
        # retry is safe; and a retry whose predecessor did connect -- but whose
        # reply was lost -- finds the live sweep and reports ALREADY_RUNNING rather
        # than stacking a second one onto the shared proxy ports.
        out=""
        for attempt in 1 2 3 4 5; do
            out=$(launch_one "$host" "$key")
            case $out in *LAUNCHED*|*ALREADY_RUNNING*) break ;; esac
            sleep 3
        done
        case $out in
            *ALREADY_RUNNING*) say "run SKIPPED on $host: a sweep is already running" ;;
            *LAUNCHED*)        say "run $host (detached)" ;;
            *)                 say "launch FAILED on $host after retries: ${out:-no response}" ;;
        esac
    done
    say "launched; poll with '$0 status'"
}

phase_status() {
    local host out attempt
    for host in "${HOSTS[@]}"; do
        printf '%-16s ' "$host"
        # Retry past the client's intermittent "Bad file descriptor": status is
        # read-only, so a repeat is harmless, and a flaked connection otherwise
        # prints a transport error where a host's real progress should be.
        out=""
        for attempt in 1 2 3 4 5; do
            out=$(rsh "$host" bash -s <<REMOTE 2>&1
W=$L4_REMOTE_WORK
# grep -c prints 0 and exits 1 when nothing matches, so only the status is dropped.
count() { grep -c "\$1" "\$2" 2>/dev/null || true; }
alive=no; pgrep -f remote_run.sh >/dev/null 2>&1 && alive=yes
sum=\$W/out/repeat-${L4_REPEAT:-1}/sweep-summary.txt
arms=\$(count '^\[sweep\] --- arm ' "\$sum")
done_arms=\$(count 'runner rc=' "\$sum")
guards=\$(count 'GUARD FAILED' "\$sum")
res=\$(ls \$W/out/repeat-${L4_REPEAT:-1}/arm-*/run/results/*.json 2>/dev/null | wc -l | tr -d ' ')
echo "alive=\$alive arms_started=\${arms:-0} arms_finished=\${done_arms:-0} instances_done=\${res:-0} guard_failures=\${guards:-0}"
REMOTE
)
            case $out in *alive=*) break ;; esac
            sleep 3
        done
        echo "$out"
    done
}

phase_collect() {
    local i host dest
    for i in "${!HOSTS[@]}"; do
        host=${HOSTS[$i]}
        dest=$WORKSPACE/runs/host$((i + 1))-$host
        mkdir -p "$dest"
        say "collect $host -> $dest"
        # Excluded: the per-instance agent profiles. They are gigabytes of SQLite
        # and sandbox trees per arm, and token-report.json has already reduced the
        # part the analysis reads. The provenance files come along because the
        # report has to be able to name what produced each number.
        ${SSH_PW[@]+"${SSH_PW[@]}"} rsync -az --no-owner --no-group \
            -e "ssh ${SSH_OPTS[*]}" \
            --exclude 'openclaw-profiles/' \
            "$L4_SSH_USER@$host:$L4_REMOTE_WORK/out/" "$dest/out/" \
            || { say "collect FAILED on $host"; return 1; }
        ${SSH_PW[@]+"${SSH_PW[@]}"} rsync -az --no-owner --no-group \
            -e "ssh ${SSH_OPTS[*]}" \
            "$L4_SSH_USER@$host:$L4_REMOTE_WORK/logs/" "$dest/logs/" || true
        for f in prompts.provenance shard.provenance l4_instances.txt; do
            ${SSH_PW[@]+"${SSH_PW[@]}"} scp "${SSH_OPTS[@]}" \
                "$L4_SSH_USER@$host:$L4_REMOTE_WORK/$f" "$dest/$f" >/dev/null 2>&1 || true
        done
    done
}

# ---------------------------------------------------------------------------
case "${1:-}" in
    sync)    check_shards; check_partition; phase_sync ;;
    setup)   phase_setup ;;
    verify)  phase_verify ;;
    dryrun)  phase_dryrun ;;
    run)     phase_run ;;
    status)  phase_status ;;
    collect) phase_collect ;;
    *)
        echo "usage: $0 {sync|setup|verify|dryrun|run|status|collect}" >&2
        echo "hosts: ${HOSTS[*]}" >&2
        exit 2 ;;
esac
