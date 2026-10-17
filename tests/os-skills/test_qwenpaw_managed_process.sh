#!/usr/bin/env bash
set -euo pipefail

source "$(dirname "$0")/../../src/os-skills/ai/install-qwenpaw/scripts/managed_process.sh"
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
pid_file="$workdir/qwenpaw.pid"

ps() {
  [[ "$2" == 1234 ]] || return 1
  printf '%s\n' "$process_command"
}

kill() {
  [[ "$1" == 1234 ]]
  printf '%s\n' "$1" >> "$workdir/killed"
}

sleep() { :; }

# No record, invalid record, and a reused PID must not be signaled.
stop_managed_qwenpaw "$pid_file"
printf '%s\n' '1234;5678' > "$pid_file"
stop_managed_qwenpaw "$pid_file"
printf '%s\n' 1234 > "$pid_file"
process_command='/usr/bin/unrelated-service --label qwenpaw'
stop_managed_qwenpaw "$pid_file"
process_command='bash -c qwenpaw app --port 8088'
stop_managed_qwenpaw "$pid_file"
[[ ! -e "$workdir/killed" ]]

process_command='/opt/qwenpaw/bin/qwenpaw app --port 8088'
stop_managed_qwenpaw "$pid_file"
[[ $(cat "$workdir/killed") == 1234 ]]

echo 'QwenPaw managed-process checks passed'
