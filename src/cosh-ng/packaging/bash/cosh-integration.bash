#!/bin/bash
# Requires the default enabled POSIX special builtins; builtin-table rewriting is unsupported.
case $- in
*i*)
if [[ ${OSTYPE-} == linux* && ${BASH_VERSINFO[0]} == 5 && ( ${BASH_VERSINFO[1]} == 1 || ${BASH_VERSINFO[1]} == 2 ) &&
   $- != *[arxveuTE]* && ! ${BASH_EXECUTION_STRING+x} && -t 0 && -t 1 && -t 2 &&
   ${COSH_BASH_DISABLE-} != 1 && ${COSH_INTERNAL_SHELL-} != 1 && -z ${COSH_SESSION_ID-} &&
   ${COSH_SHELL_ISOLATED-} != 1 && ! ${BASH_ENV+x} && ! ${ENV+x} && ! ${BASH_XTRACEFD+x} &&
   ! ${GLIBC_TUNABLES+x} && ! ${XDG_CONFIG_HOME+x} && ! ${BASH_COMPLETION_COMPAT_DIR+x} &&
   ! ${BASH_COMPLETION_USER_FILE+x} && ! ${BASH_COMPLETION_DEBUG+x} ]]; then
if ( if POSIXLY_CORRECT=1 && [[ :$SHELLOPTS: == *:posix:* ]]; then
    for __cosh_probe_name in builtin local return command type readonly; do
        if \readonly -f "$__cosh_probe_name" 2>/dev/null; then \exit 0
        else case $? in 1) ;; *) \exit 1 ;; esac; fi
    done
    \exit 2
  else ((0)); fi ) 2>/dev/null; then ((1))
else case $? in 2)
if \command builtin shopt -q login_shell && ! \command builtin shopt -oq posix && ! \command builtin shopt -q extdebug &&
[[ -z $(\command builtin compgen -A variable LD_) && -z $(\command builtin compgen -A variable __cosh_hook_) &&
   -z $(\command builtin compgen -A function __cosh_hook_) && -z $(\command builtin compgen -A alias __cosh_hook_) &&
   -z $(\command builtin trap -p ERR DEBUG RETURN CHLD) && $(\command builtin declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* &&
   ${#PROMPT_COMMAND[@]} == 1 && ${PROMPT_COMMAND[0]+x} == x &&
   -x /usr/bin/timeout && -x /usr/bin/printf && -x /usr/libexec/anolisa/cosh-ng/cosh-shell ]]; then
case ${TERM-} in xterm*|screen*)
case ${PROMPT_COMMAND[0]} in
    'printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"'|'printf "\033k%s@%s:%s\033\\" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
__cosh_hook_clock() {
    "${__cosh_hook_ops[@]}" local raw up
    IFS= "${__cosh_hook_ops[@]}" read -r -n 128 raw 2>/dev/null </proc/uptime || "${__cosh_hook_ops[@]}" return 1
    up=${raw%% *}
    [[ ${#raw} -lt 128 && $raw == *' '* && $up == *.[0123456789][0123456789] &&
       $up != .* && ${#up} -le 15 && ${up//[0123456789]/} == . ]] || "${__cosh_hook_ops[@]}" return 1
    __cosh_hook_now=$((10#${up/./} * 10000))
}

__cosh_hook_call() {
    "${__cosh_hook_ops[@]}" local operation=$1 phase=$2 budget start available timeout field reader result count=0 bytes=0
    "${__cosh_hook_ops[@]}" local LC_ALL=C
    __cosh_hook_response=()
    __cosh_hook_clock || "${__cosh_hook_ops[@]}" return 1
    start=$__cosh_hook_now
    if [[ $operation == compat ]]; then
        [[ $("${__cosh_hook_ops[@]}" declare -p __cosh_bash_spent_us 2>/dev/null) == 'declare -- '* || ! ${__cosh_bash_spent_us+x} ]] || "${__cosh_hook_ops[@]}" return 1
        available=${__cosh_bash_spent_us-0}
        [[ -n $available && ${#available} -le 6 && $available != *[!0123456789]* ]] || "${__cosh_hook_ops[@]}" return 1
        __cosh_hook_startup_spent=$((10#$available))
        available=$((500000 - __cosh_hook_startup_spent))
        budget=$((100000 - __cosh_hook_used))
        [[ $phase != registration || $budget -le 50000 ]] || budget=50000
        [[ $budget -le $available ]] || budget=$available
        __cosh_hook_final_deadline=$((start + budget))
    else
        budget=40000
        __cosh_hook_final_deadline=$((start + 50000))
    fi
    budget=$((budget / 1000))
    [[ $budget -gt 0 && $budget -le 100 ]] || "${__cosh_hook_ops[@]}" return 1
    __cosh_hook_deadline=$((start + budget * 1000))
    "${__cosh_hook_ops[@]}" printf -v timeout '0.%03d' "$budget"
    __cosh_hook_input=(2 external-bash "$phase" "$__cosh_hook_snapshot" "$__cosh_hook_deadline" "$budget")
    if [[ $operation == advise ]]; then
        __cosh_hook_input+=("$__cosh_hook_event_command" "$__cosh_hook_event_status" "$__cosh_hook_event_cwd" "$__cosh_hook_event_ms" observed_command_fragment external_unshadowed "$__cosh_hook_host" "$__cosh_hook_session" "$__cosh_hook_generation")
    fi
    "${__cosh_hook_ops[@]}" exec {reader}< <(
        "${__cosh_hook_ops[@]}" set -o pipefail
        "${__cosh_hook_ops[@]}" printf '%s\0' "${__cosh_hook_input[@]}" |
            /usr/bin/timeout --kill-after=0.005 "$timeout" /usr/libexec/anolisa/cosh-ng/cosh-shell shell-integration "$operation" 2>/dev/null
    )
    __cosh_hook_reader_pid=$!
    result=0
    while IFS= "${__cosh_hook_ops[@]}" read -r -d '' -n 8193 -t "$timeout" field <&"$reader"; do
        bytes=$((bytes + ${#field} + 1))
        [[ $count -lt 8 && $bytes -le 8192 ]] || { result=1; "${__cosh_hook_ops[@]}" break; }
        __cosh_hook_response+=("$field")
        count=$((count + 1))
    done
    "${__cosh_hook_ops[@]}" exec {reader}<&-
    "${__cosh_hook_ops[@]}" wait "$__cosh_hook_reader_pid" 2>/dev/null || result=1
    __cosh_hook_clock || "${__cosh_hook_ops[@]}" return 1
    if [[ $operation == compat ]]; then
        [[ $__cosh_hook_now -ge $start ]] || "${__cosh_hook_ops[@]}" return 1
        available=$((__cosh_hook_now + 10000 - start))
        __cosh_hook_used=$((__cosh_hook_used + available))
        __cosh_bash_spent_us=$((__cosh_hook_startup_spent + available))
    fi
    [[ $result == 0 && $count == 8 && -z $field && $__cosh_hook_now -lt $__cosh_hook_deadline && ${__cosh_hook_response[0]} == 2 ]]
}

__cosh_hook_guard() {
    case $- in *[arxveuTE]*) "${__cosh_hook_ops[@]}" return 1 ;; esac
    ! "${__cosh_hook_ops[@]}" shopt -oq posix && ! "${__cosh_hook_ops[@]}" shopt -q extdebug || "${__cosh_hook_ops[@]}" return 1
    [[ -t 0 && -t 1 && -t 2 && ${COSH_BASH_DISABLE-} != 1 &&
       ${COSH_INTERNAL_SHELL-} != 1 && -z ${COSH_SESSION_ID-} && ${COSH_SHELL_ISOLATED-} != 1 &&
       ! ${BASH_ENV+x} && ! ${ENV+x} && ! ${BASH_XTRACEFD+x} && ! ${GLIBC_TUNABLES+x} &&
       ! ${XDG_CONFIG_HOME+x} && ! ${BASH_COMPLETION_COMPAT_DIR+x} &&
       ! ${BASH_COMPLETION_USER_FILE+x} && ! ${BASH_COMPLETION_DEBUG+x} &&
       -z $("${__cosh_hook_ops[@]}" compgen -A variable LD_) &&
       ! -e $__cosh_hook_runtime/disabled && -d $__cosh_hook_runtime &&
       ${TERM-} == "$__cosh_hook_term" &&
       $("${__cosh_hook_ops[@]}" set +o; "${__cosh_hook_ops[@]}" shopt -p) == "$__cosh_hook_options" &&
       $("${__cosh_hook_ops[@]}" declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* &&
       ${#PROMPT_COMMAND[@]} == 2 && ${PROMPT_COMMAND[0]-} == "$__cosh_hook_native" &&
       ${PROMPT_COMMAND[$__cosh_hook_slot]-} == "$1" ]]
}

__cosh_hook_retreat() {
    __cosh_hook_active=-1
    __cosh_hook_event_command=
}

__cosh_hook_boot() {
    "${__cosh_hook_ops[@]}" local saved=$1
    __cosh_hook_observed_traps=$2
    if [[ -z $__cosh_hook_observed_traps ]] && __cosh_hook_guard "$__cosh_hook_boot_entry" &&
       __cosh_hook_call compat activation && [[ ${__cosh_hook_response[1]} == SHOW &&
       ${__cosh_hook_response[2]} == compatible && ${#__cosh_hook_response[4]} == 64 &&
       ${__cosh_hook_response[4]} != *[!0123456789abcdef]* &&
       ${__cosh_hook_response[6]} == "$__cosh_hook_runtime" ]]; then
        __cosh_hook_snapshot=${__cosh_hook_response[4]}
        __cosh_hook_generation=${__cosh_hook_response[3]}
        __cosh_hook_active=1
        PROMPT_COMMAND[$__cosh_hook_slot]=$__cosh_hook_prompt_entry
        "${__cosh_hook_ops[@]}" trap "$__cosh_hook_err_action" ERR
        __cosh_hook_err_definition=$("${__cosh_hook_ops[@]}" trap -p ERR)
    else
        __cosh_hook_retreat
    fi
    "${__cosh_hook_ops[@]}" return "$saved"
}

__cosh_hook_prompt() {
    "${__cosh_hook_ops[@]}" local saved=$1 program kind seconds fraction ms timeout text
    __cosh_hook_observed_traps=$2
    if [[ $__cosh_hook_observed_traps != "$__cosh_hook_err_definition" ]] ||
       ! __cosh_hook_guard "$__cosh_hook_prompt_entry"; then
        __cosh_hook_retreat
        "${__cosh_hook_ops[@]}" return "$saved"
    fi
    if [[ -z ${__cosh_hook_event_command-} ]]; then "${__cosh_hook_ops[@]}" return "$saved"; fi
    text=$__cosh_hook_event_command
    __cosh_hook_event_command=
    if [[ $__cosh_hook_event_subshell != 0 || $__cosh_hook_event_functions != 0 ||
          $__cosh_hook_event_sources != 0 || $__cosh_hook_event_pipe_count != 1 ||
          $__cosh_hook_event_flags == *[arxveuTE]* ]]; then "${__cosh_hook_ops[@]}" return "$saved"; fi
    case "$text:$__cosh_hook_event_status" in
        'cargo build:101'|'cargo test:101'|'cargo build '*':101'|'cargo test '*':101') program=cargo ;;
        'pytest:1'|'pytest '*':1') program=pytest ;;
        *) "${__cosh_hook_ops[@]}" return "$saved" ;;
    esac
    [[ $("${__cosh_hook_ops[@]}" type -t "$program" 2>/dev/null) == file ]] || "${__cosh_hook_ops[@]}" return "$saved"
    seconds=${__cosh_hook_event_time%%.*} fraction=${__cosh_hook_event_time#*.}
    [[ $seconds != *[!0123456789]* && -n $seconds && ${#seconds} -le 13 &&
       $fraction != *[!0123456789]* && ${#fraction} == 6 ]] || "${__cosh_hook_ops[@]}" return "$saved"
    __cosh_hook_event_ms=$((10#$seconds * 1000 + 10#${fraction:0:3}))
    __cosh_hook_event_command=$text
    if ! __cosh_hook_call advise activation; then
        __cosh_hook_retreat
    elif [[ ${__cosh_hook_response[1]} == SHOW &&
         ${__cosh_hook_response[2]} == suggest_only && ${__cosh_hook_response[3]} == "$__cosh_hook_generation" &&
         ${__cosh_hook_response[6]} == "$__cosh_hook_runtime" &&
         ! -e $__cosh_hook_runtime/disabled ]]; then
        __cosh_hook_retreat
        text=${__cosh_hook_response[7]}
        __cosh_hook_clock || "${__cosh_hook_ops[@]}" return "$saved"
        ms=$(((__cosh_hook_final_deadline - __cosh_hook_now - 10000) / 1000))
        if [[ -n $text && ${#text} -le 4096 && $ms -gt 0 && $ms -le 50 ]]; then
            "${__cosh_hook_ops[@]}" printf -v timeout '0.%03d' "$ms"
            /usr/bin/timeout --kill-after=0.005 "$timeout" /usr/bin/printf '\n%s\n' "$text" </dev/null 2>/dev/null || "${__cosh_hook_ops[@]}" :
        fi
    elif [[ ${__cosh_hook_response[1]} != SKIP ]]; then
        __cosh_hook_retreat
    fi
    __cosh_hook_event_command=
    "${__cosh_hook_ops[@]}" return "$saved"
}

__cosh_hook_status() { "${__cosh_hook_ops[@]}" return "$1"; }

__cosh_hook_ops=(command builtin)
__cosh_hook_slot=0
while [[ ${PROMPT_COMMAND[$__cosh_hook_slot]+x} ]]; do
    __cosh_hook_slot=$((__cosh_hook_slot + 1))
    [[ $__cosh_hook_slot -lt 8 ]] || "${__cosh_hook_ops[@]}" break
done
__cosh_hook_native=${PROMPT_COMMAND[0]}
__cosh_hook_term=$TERM
__cosh_hook_used=0 __cosh_hook_snapshot= __cosh_hook_active=0
__cosh_hook_event_command=
__cosh_hook_entry='__cosh_hook_saved=$?; __cosh_hook_ops=(); __cosh_hook_trusted=0
if [[ $__cosh_hook_active != -1 ]]; then
    if ( if POSIXLY_CORRECT=1 && [[ :$SHELLOPTS: == *:posix:* ]]; then
    for __cosh_probe_name in builtin local return command type readonly; do
        if \readonly -f "$__cosh_probe_name" 2>/dev/null; then \exit 0
        else case $? in 1) ;; *) \exit 1 ;; esac; fi
    done
    \exit 2
  else ((0)); fi ) 2>/dev/null; then __cosh_hook_gate=0; else __cosh_hook_gate=$?; fi
    case $__cosh_hook_gate in
        2) __cosh_hook_ops=(command builtin) ;;
        0)
            if ( if POSIXLY_CORRECT=1 && [[ :$SHELLOPTS: == *:posix:* ]]; then
                if \readonly -f command 2>/dev/null; then
                    if \readonly -f builtin 2>/dev/null; then \exit 0
                    else case $? in 1) \exit 3 ;; *) \exit 1 ;; esac; fi
                else case $? in 1) \exit 2 ;; *) \exit 1 ;; esac; fi
              else ((0)); fi ) 2>/dev/null; then __cosh_hook_choice=0; else __cosh_hook_choice=$?; fi
            case $__cosh_hook_choice in 2) __cosh_hook_ops=(command builtin) ;; 3) __cosh_hook_ops=(builtin) ;; esac ;;
    esac
    if [[ $__cosh_hook_gate == 2 && ${#__cosh_hook_ops[@]} != 0 && $("${__cosh_hook_ops[@]}" declare -f __cosh_hook_clock __cosh_hook_call __cosh_hook_guard __cosh_hook_retreat __cosh_hook_boot __cosh_hook_prompt __cosh_hook_status) == "$__cosh_hook_definitions" ]]; then
        __cosh_hook_trusted=1
    fi
    if [[ ${PROMPT_COMMAND@a} == *r* ]]; then __cosh_hook_active=-1; fi
    if [[ $__cosh_hook_active != -1 && $__cosh_hook_gate == 2 && $__cosh_hook_trusted == 1 ]]; then
        __cosh_hook_seen_traps=$("${__cosh_hook_ops[@]}" trap -p ERR DEBUG RETURN CHLD)
        case $__cosh_hook_active in
            0) if [[ -z $__cosh_hook_seen_traps ]]; then __cosh_hook_boot "$__cosh_hook_saved" "$__cosh_hook_seen_traps"; else __cosh_hook_active=-1; __cosh_hook_trusted=0; fi ;;
            1) if [[ $__cosh_hook_seen_traps == "$__cosh_hook_err_definition" ]]; then __cosh_hook_prompt "$__cosh_hook_saved" "$__cosh_hook_seen_traps"; else __cosh_hook_active=-1; __cosh_hook_trusted=0; fi ;;
            *) __cosh_hook_active=-1 ;;
        esac
    else __cosh_hook_active=-1; fi
fi
if [[ $__cosh_hook_active == -1 ]]; then
    __cosh_hook_event_command=
    if [[ ${PROMPT_COMMAND@a} != *r* && ${PROMPT_COMMAND[$__cosh_hook_slot]-} == "$__cosh_hook_entry" ]]; then
        if [[ ${#__cosh_hook_ops[@]} != 0 ]]; then "${__cosh_hook_ops[@]}" unset "PROMPT_COMMAND[$__cosh_hook_slot]" 2>/dev/null
        else PROMPT_COMMAND[$__cosh_hook_slot]= 2>/dev/null; fi
    fi
    if [[ ${#__cosh_hook_ops[@]} != 0 && -n ${__cosh_hook_err_definition-} && $("${__cosh_hook_ops[@]}" trap -p ERR) == "$__cosh_hook_err_definition" ]]; then "${__cosh_hook_ops[@]}" trap - ERR; fi
fi
if [[ $__cosh_hook_trusted == 1 ]]; then __cosh_hook_status "$__cosh_hook_saved"
else ( if [[ :$SHELLOPTS: == *:posix:* ]] || { POSIXLY_CORRECT=1 && [[ :$SHELLOPTS: == *:posix:* ]]; }; then \exit "$__cosh_hook_saved"; else ((__cosh_hook_saved == 0)); fi ) 2>/dev/null; fi && [[ 1 ]]'
__cosh_hook_boot_entry=$__cosh_hook_entry
__cosh_hook_prompt_entry=$__cosh_hook_entry
__cosh_hook_err_action='__cosh_hook_err_status=$? __cosh_hook_err_pipe_count=${#PIPESTATUS[@]} 2>/dev/null
case ${__cosh_hook_slot-} in [0-7])
if [[ ${__cosh_hook_active-} == 1 && -n ${__cosh_hook_entry-} && ${PROMPT_COMMAND[$__cosh_hook_slot]-} == "$__cosh_hook_entry" ]]; then
    __cosh_hook_event_status=$__cosh_hook_err_status __cosh_hook_event_command=${BASH_COMMAND:0:1025} __cosh_hook_event_pipe_count=$__cosh_hook_err_pipe_count __cosh_hook_event_subshell=$BASH_SUBSHELL __cosh_hook_event_functions=${#FUNCNAME[@]} __cosh_hook_event_sources=${#BASH_SOURCE[@]} __cosh_hook_event_flags=$- __cosh_hook_event_time=$EPOCHREALTIME __cosh_hook_event_cwd=${PWD:0:4097} 2>/dev/null
fi
;; esac'
__cosh_hook_definitions=$("${__cosh_hook_ops[@]}" declare -f __cosh_hook_clock __cosh_hook_call __cosh_hook_guard __cosh_hook_retreat __cosh_hook_boot __cosh_hook_prompt __cosh_hook_status)
__cosh_hook_options=$("${__cosh_hook_ops[@]}" set +o; "${__cosh_hook_ops[@]}" shopt -p)
if __cosh_hook_call compat registration 2>/dev/null && [[ ${__cosh_hook_response[1]} == SHOW &&
     ${__cosh_hook_response[2]} == compatible && ${#__cosh_hook_response[4]} == 64 &&
     ${__cosh_hook_response[4]} != *[!0123456789abcdef]* &&
     ${#__cosh_hook_response[3]} == 64 && ${__cosh_hook_response[3]} != *[!0123456789abcdef]* &&
     ${__cosh_hook_response[6]} == /* && ${#__cosh_hook_response[6]} -le 4096 ]]; then
    __cosh_hook_snapshot=${__cosh_hook_response[4]}
    __cosh_hook_runtime=${__cosh_hook_response[6]}
    __cosh_hook_generation=${__cosh_hook_response[3]}
    __cosh_hook_host=${HOSTNAME-local}
    __cosh_hook_session=$BASHPID:$EPOCHREALTIME
    PROMPT_COMMAND[$__cosh_hook_slot]=$__cosh_hook_boot_entry
    __cosh_bash_registered=1
else
    \command builtin unset -f __cosh_hook_clock __cosh_hook_call __cosh_hook_guard __cosh_hook_retreat __cosh_hook_boot __cosh_hook_prompt __cosh_hook_status
    for __cosh_hook_cleanup in $(\command builtin compgen -A variable __cosh_hook_); do \command builtin unset "$__cosh_hook_cleanup"; done
    \command builtin unset __cosh_hook_cleanup
fi
;; esac
;; esac
fi
;; esac; fi
fi
;;
esac
