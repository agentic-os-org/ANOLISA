#!/bin/bash
[[ ${BASH_VERSINFO[0]} == 5 && ( ${BASH_VERSINFO[1]} == 1 || ${BASH_VERSINFO[1]} == 2 ) &&
   $- == *i* && -t 0 && -t 1 && -t 2 ]] || exit 77
__shadow_phase=$COSH_BASH_SHADOW_PHASE
__shadow_target=$COSH_BASH_SHADOW_TARGET
__shadow_variant=$COSH_BASH_SHADOW_VARIANT
__shadow_safe=(command builtin)
__shadow_targets=("$__shadow_target")
if [[ $__shadow_target == command ]]; then __shadow_safe=(builtin); fi
if [[ $__shadow_target == double ]]; then __shadow_safe=(); __shadow_targets=(builtin command); fi
"${__shadow_safe[@]}" shopt -q login_shell || exit 77
TERM=xterm-256color
PROMPT_COMMAND=('printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
__shadow_native=${PROMPT_COMMAND[0]}

if [[ $__shadow_phase == registration ]]; then
    __shadow_before=$("${__shadow_safe[@]}" declare -f "${__shadow_targets[@]}")
    __shadow_exports_before=$("${__shadow_safe[@]}" export -pf)
fi
__shadow_source_started=$EPOCHREALTIME
"${__shadow_safe[@]}" source "$COSH_BASH_TEST_ASSETS/cosh-integration.bash"
__shadow_source_status=$? __shadow_source_finished=$EPOCHREALTIME
if [[ $__shadow_phase != registration ]]; then
    [[ ${#PROMPT_COMMAND[@]} == 2 && -z $("${__shadow_safe[@]}" trap -p ERR DEBUG RETURN) ]] || {
        "${__shadow_safe[@]}" printf 'SHADOW_PREREQ_FAILED check=registration phase=%s target=%s variant=%s registered=%s pc_count=%s source_status=%s source_us=%s spent_us=%s\n' \
            "$__shadow_phase" "$__shadow_target" "$__shadow_variant" "${__cosh_bash_registered-unset}" \
            "${#PROMPT_COMMAND[@]}" "$__shadow_source_status" \
            "$((10#${__shadow_source_finished/./} - 10#${__shadow_source_started/./}))" "${__cosh_bash_spent_us-unset}" >&2
        exit 80
    }
    __shadow_slot=
    for __shadow_index in "${!PROMPT_COMMAND[@]}"; do
        if [[ $__shadow_index != 0 ]]; then __shadow_slot=$__shadow_index; fi
    done
    [[ -n $__shadow_slot && ${PROMPT_COMMAND[$__shadow_slot]} == *__cosh_hook_boot* ]] || {
        "${__shadow_safe[@]}" printf 'SHADOW_PREREQ_FAILED check=boot-slot phase=%s target=%s variant=%s registered=%s pc_count=%s source_status=%s source_us=%s spent_us=%s slot=%s\n' \
            "$__shadow_phase" "$__shadow_target" "$__shadow_variant" "${__cosh_bash_registered-unset}" \
            "${#PROMPT_COMMAND[@]}" "$__shadow_source_status" \
            "$((10#${__shadow_source_finished/./} - 10#${__shadow_source_started/./}))" "${__cosh_bash_spent_us-unset}" "$__shadow_slot" >&2
        exit 80
    }
    if [[ $__shadow_phase == noevent ]]; then
        "${__shadow_safe[@]}" eval "${PROMPT_COMMAND[$__shadow_slot]}"
        [[ ${PROMPT_COMMAND[$__shadow_slot]-} == *__cosh_hook_prompt* &&
           -n $("${__shadow_safe[@]}" trap -p ERR) ]] || exit 80
    fi
    __shadow_trap_before=$("${__shadow_safe[@]}" trap -p ERR DEBUG RETURN)
    __shadow_entry=${PROMPT_COMMAND[$__shadow_slot]}
    "${__shadow_safe[@]}" eval "$COSH_BASH_SHADOW_DEFINITION"
    if [[ $__shadow_variant == self ]]; then "${__shadow_safe[@]}" export -f "${__shadow_targets[@]}"; fi
    __shadow_before=$("${__shadow_safe[@]}" declare -f "${__shadow_targets[@]}")
    __shadow_exports_before=$("${__shadow_safe[@]}" export -pf)
    if [[ $__shadow_phase == noevent ]]; then ! "${__shadow_safe[@]}" :; fi
    "${__shadow_safe[@]}" eval "$__shadow_entry"
    __shadow_status=$?
fi
__shadow_after=$("${__shadow_safe[@]}" declare -f "${__shadow_targets[@]}")
[[ $__shadow_after == "$__shadow_before" ]] || exit 101
if [[ -s $COSH_BASH_TEST_WORKDIR/shadow.calls ]]; then
    "${__shadow_safe[@]}" printf '%s\n' SHADOW_WAS_CALLED
    exit 102
fi
__shadow_calls=()
if [[ -r $COSH_BASH_TEST_WORKDIR/hook.calls ]]; then
    "${__shadow_safe[@]}" mapfile -t __shadow_calls <"$COSH_BASH_TEST_WORKDIR/hook.calls"
fi
case $__shadow_phase in registration) __shadow_expected=0 ;; activation) __shadow_expected=1 ;; noevent) __shadow_expected=2 ;; esac
[[ ${#__shadow_calls[@]} == $__shadow_expected ]] || exit 103
[[ $("${__shadow_safe[@]}" export -pf) == "$__shadow_exports_before" ]] || exit 106
[[ ${PROMPT_COMMAND[0]} == "$__shadow_native" ]] || exit 104
if [[ $__shadow_target == double && $__shadow_phase != registration ]]; then
    [[ $__cosh_hook_active == -1 && $("${__shadow_safe[@]}" trap -p ERR DEBUG RETURN) == "$__shadow_trap_before" ]] || exit 107
    for __shadow_index in "${!PROMPT_COMMAND[@]}"; do
        [[ $__shadow_index == 0 || -z ${PROMPT_COMMAND[$__shadow_index]} ]] || exit 104
    done
else
    [[ ${#PROMPT_COMMAND[@]} == 1 && -z $("${__shadow_safe[@]}" trap -p ERR DEBUG RETURN) ]] || exit 104
fi
[[ $__shadow_phase != noevent || $__shadow_status == 1 ]] || exit 105
"${__shadow_safe[@]}" printf '%s\n' SHADOW_RELINQUISHED
