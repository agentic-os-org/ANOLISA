#!/bin/bash

builtin printf '%s\n' "$@" >>"${COSH_BASH_TEST_WORKDIR:?}/helper.args"
if [[ $# != 6 || $1 != health || $2 != banner || $3 != --budget-ms ||
      -z $4 || $4 == *[!0-9]* || ${#4} -gt 3 || $4 -lt 1 || $4 -gt 350 ||
      $5 != --bash-deadline-boottime-us || -z $6 || $6 == *[!0-9]* || ${#6} -gt 18 ]]; then
    exit 64
fi
IFS=' ' builtin read -r __fixture_uptime __fixture_idle </proc/uptime || exit 65
__fixture_now_us=$((10#${__fixture_uptime/./} * 10000))
__fixture_deadline_us=$((10#$6))
__fixture_budget_ms=$((10#$4))
[[ $__fixture_deadline_us -gt $__fixture_now_us &&
   $((__fixture_deadline_us - __fixture_now_us)) -le $((__fixture_budget_ms * 1000)) ]] || exit 66
case ${COSH_BASH_TEST_MODE-} in
    failure)
        builtin printf '%s\n' 'fixture helper diagnostic' >&2
        exit 2
        ;;
    oversize)
        builtin printf '%09000d' 0
        exit 0
        ;;
    slow)
        builtin printf '%s\n' "$$" >"${COSH_BASH_TEST_WORKDIR:?}/helper.pid"
        /usr/bin/sleep 1
        builtin printf '%s\n' 'fixture late summary'
        exit 0
        ;;
esac
builtin printf '%s\n' 'fixture resource summary'
