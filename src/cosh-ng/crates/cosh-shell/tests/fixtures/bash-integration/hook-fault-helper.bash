#!/bin/bash
if [[ $2 != advise ]]; then
 builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-helper.bash" "$@"
 builtin exit "$?"
fi
builtin printf '%s\n' "$*" >>"$COSH_BASH_TEST_WORKDIR/hook.calls"
__fault_fields=()
while IFS= builtin read -r -d '' __fault_field; do __fault_fields+=("$__fault_field"); done
[[ $1 == shell-integration && ${#__fault_fields[@]} == 15 && ${__fault_fields[0]} == 2 ]] || exit 70
__fault_mode=${COSH_BASH_TEST_CASE#hook_live_fault_}
__fault_mode=${__fault_mode%_candidate}
builtin printf 'FAULT:%s\n' "$__fault_mode" >>"$COSH_BASH_TEST_WORKDIR/hook.advise"
__fault_version=2 __fault_disposition=SHOW __fault_exit=0
__fault_generation=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
__fault_runtime=$COSH_BASH_TEST_WORKDIR/runtime
case $__fault_mode in
 exit) __fault_exit=73 ;;
 empty) builtin exit 0 ;;
 tail) ;;
 version) __fault_version=9 __fault_disposition=SKIP ;;
 disposition) __fault_disposition=UNKNOWN ;;
 generation) __fault_generation=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb ;;
 runtime) __fault_runtime+=/different ;;
 timeout) /bin/sleep 0.2 ;;
 closed) builtin : >"$__fault_runtime/disabled" ;;
 printf) ;;
 *) builtin exit 70 ;;
esac
builtin printf '%s\0' "$__fault_version" "$__fault_disposition" suggest_only "$__fault_generation" '' '' "$__fault_runtime" FRAGMENT_INVITATION
if [[ $__fault_mode == tail ]]; then builtin printf x; fi
builtin exit "$__fault_exit"
