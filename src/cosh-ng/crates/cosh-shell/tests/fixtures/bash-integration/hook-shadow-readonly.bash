#!/bin/bash
__readonly_case=$1 __readonly_assets=$2 __readonly_work=$3
TERM=xterm-256color
PROMPT_COMMAND=('printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
builtin source "$__readonly_assets/cosh-integration.bash"
[[ ${#PROMPT_COMMAND[@]} == 2 ]] || exit 80
for __readonly_index in "${!PROMPT_COMMAND[@]}"; do
    if [[ $__readonly_index != 0 ]]; then __readonly_slot=$__readonly_index; fi
done
if [[ $__readonly_case == hook_readonly_active || $__readonly_case == hook_readonly_owned_active ]]; then
    builtin eval "${PROMPT_COMMAND[$__readonly_slot]}"
    [[ $__cosh_hook_active == 1 ]] || exit 80
fi
__readonly_entry=${PROMPT_COMMAND[$__readonly_slot]}
if [[ $__readonly_case != hook_readonly_owned_active ]]; then
 builtin trap 'builtin printf "%s\n" user-err >>"$__readonly_work/user.err"' ERR
fi
__readonly_trap=$(builtin trap -p ERR DEBUG RETURN)
builtin readonly -a PROMPT_COMMAND
__readonly_before=$(builtin declare -p PROMPT_COMMAND)
if [[ $__readonly_case == hook_readonly_owned_active ]]; then (exit 7); else builtin :; fi
builtin eval "$__readonly_entry"
__readonly_first=$?
builtin :
builtin eval "$__readonly_entry"
__readonly_second=$?
[[ ! -s $__readonly_work/user.err ]] || exit 111
case $__readonly_case in hook_readonly_owned_active) __readonly_expected_status=7 ;; *) __readonly_expected_status=0 ;; esac
[[ $__readonly_first == "$__readonly_expected_status" && $__readonly_second == 0 ]] || exit 112
[[ $(builtin declare -p PROMPT_COMMAND) == "$__readonly_before" && $__cosh_hook_active == -1 ]] || exit 113
if [[ $__readonly_case == hook_readonly_owned_active ]]; then
 [[ -z $(builtin trap -p ERR DEBUG RETURN) ]] || exit 115
 (exit 7)
 builtin eval "$__readonly_entry"
 __readonly_failed_status=$?
 [[ $__readonly_failed_status == 7 && -z ${__cosh_hook_event_command-} &&
    $(builtin declare -p PROMPT_COMMAND) == "$__readonly_before" ]] || exit 116
else
 [[ $(builtin trap -p ERR DEBUG RETURN) == "$__readonly_trap" ]] || exit 113
fi
builtin mapfile -t __readonly_calls <"$__readonly_work/hook.calls"
case $__readonly_case in hook_readonly_registered) __readonly_expected=1 ;; hook_readonly_active|hook_readonly_owned_active) __readonly_expected=2 ;; esac
[[ ${#__readonly_calls[@]} == $__readonly_expected ]] || exit 114
builtin printf '%s\n' READONLY_PROMPT_INERT
