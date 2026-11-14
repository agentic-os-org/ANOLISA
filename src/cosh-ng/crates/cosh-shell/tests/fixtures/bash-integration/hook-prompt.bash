#!/bin/bash
# retirement keeps the native prompt entry and every recorded sentinel except
# event_command, which retreat clears
__disabled_state() {
    [[ $__cosh_hook_active == -1 && ! ${PROMPT_COMMAND[$__cosh_hook_slot]+x} ]] || exit 91
    [[ ${#PROMPT_COMMAND[@]} == 1 && ${PROMPT_COMMAND[0]} == "$__native_before" ]] || exit 92
    [[ -z ${__cosh_hook_event_command-} && $__cosh_hook_event_status == 77 &&
       $__cosh_hook_event_cwd == sentinel_cwd && $__cosh_hook_event_time == sentinel_time &&
       $__cosh_hook_event_pipe_count == 77 && $__cosh_hook_event_subshell == 77 &&
       $__cosh_hook_event_functions == 77 && $__cosh_hook_event_sources == 77 &&
       $__cosh_hook_event_flags == sentinel_flags ]] || exit 93
}
case $1 in
setup)
    TERM=xterm-256color
    PROMPT_COMMAND=('printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
    builtin source "$COSH_BASH_TEST_ASSETS/cosh-integration.bash"
    [[ ${__cosh_bash_registered-} == 1 && $__cosh_hook_active == 0 ]] || exit 80
    if [[ $COSH_PROMPT_PHASE == registered ]]; then
        builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-prompt.bash" configure
    fi
    ;;
retreat)
    [[ $__cosh_hook_active == 1 &&
       ${PROMPT_COMMAND[$__cosh_hook_slot]-} == "$__cosh_hook_entry" ]] || exit 83
    __native_before=${PROMPT_COMMAND[0]}
    case $COSH_PROMPT_MODE in
        overwrite) PROMPT_COMMAND[$__cosh_hook_slot]='builtin printf USER_PROMPT_MARKER' ;;
        delete) builtin unset "PROMPT_COMMAND[$__cosh_hook_slot]" ;;
        readonly)
            PROMPT_COMMAND=('builtin printf USER_PROMPT_MARKER')
            builtin readonly -a PROMPT_COMMAND
            ;;
        disabled) builtin : >"$__cosh_hook_runtime/disabled" ;;
    esac
    __cosh_hook_event_command=sentinel_command
    __cosh_hook_event_status=77
    __cosh_hook_event_cwd=sentinel_cwd
    __cosh_hook_event_time=sentinel_time
    __cosh_hook_event_pipe_count=77
    __cosh_hook_event_subshell=77
    __cosh_hook_event_functions=77
    __cosh_hook_event_sources=77
    __cosh_hook_event_flags=sentinel_flags
    __event_before=$(builtin declare -p ${!__cosh_hook_event_@})
    __prompt_before=$(builtin declare -p PROMPT_COMMAND)
    __state_before="$__cosh_hook_active:$__cosh_hook_trusted:$__cosh_hook_slot:$__cosh_hook_entry"
    ;;
check)
    __event_after=$(builtin declare -p ${!__cosh_hook_event_@})
    if [[ $COSH_PROMPT_MODE == disabled ]]; then
        __disabled_state
        [[ -z $(builtin trap -p ERR) ]] || exit 89
    else
        [[ $__event_after == "$__event_before" ]] || {
            builtin printf 'EVENT_CHANGED:%s\n' "$__event_after"
            exit 84
        }
        [[ $(builtin declare -p PROMPT_COMMAND) == "$__prompt_before" &&
           "$__cosh_hook_active:$__cosh_hook_trusted:$__cosh_hook_slot:$__cosh_hook_entry" == "$__state_before" ]] || exit 85
    fi
    builtin trap 'builtin printf "USER_ERR:%s:%s\n" "$?" "$BASH_COMMAND" >>"$COSH_BASH_TEST_WORKDIR/user.err"' ERR
    __user_err_before=$(builtin trap -p ERR)
    ;;
configure)
    case $COSH_PROMPT_PHASE in registered) __expected_phase=0 ;; active) __expected_phase=1 ;; esac
    [[ $__cosh_hook_active == "$__expected_phase" &&
       ${PROMPT_COMMAND[$__cosh_hook_slot]-} == "$__cosh_hook_entry" ]] || exit 81
    __user_code=$COSH_PROMPT_CODE
    __user_native() { builtin return "$__user_code"; }
    __user_slot=$__cosh_hook_slot
    PROMPT_COMMAND[0]=__user_native
    builtin trap 'builtin printf "ERR:%s:%s\n" "$?" "$BASH_COMMAND" >>"$COSH_BASH_TEST_WORKDIR/user.err"' ERR
    __after_slot=$((__user_slot + 1))
    while [[ ${PROMPT_COMMAND[$__after_slot]+x} ]]; do __after_slot=$((__after_slot + 1)); done
    PROMPT_COMMAND[$__after_slot]='builtin printf "AFTER:%s:%s:%s\n" "$__round" "$?" "${PIPESTATUS[*]}" >>"$COSH_BASH_TEST_WORKDIR/user.pc"'
    if [[ $COSH_PROMPT_MODE == baseline ]]; then builtin unset "PROMPT_COMMAND[$__user_slot]"; fi
    ;;
finish)
    if [[ $COSH_PROMPT_CODE == retreat ]]; then
        if [[ $COSH_PROMPT_MODE == disabled ]]; then
            __disabled_state
            [[ $(builtin trap -p ERR) == "$__user_err_before" ]] || exit 86
        else
            [[ $(builtin declare -p PROMPT_COMMAND) == "$__prompt_before" &&
               "$__cosh_hook_active:$__cosh_hook_trusted:$__cosh_hook_slot:$__cosh_hook_entry" == "$__state_before" &&
               $(builtin trap -p ERR) == "$__user_err_before" &&
               $(builtin declare -p ${!__cosh_hook_event_@}) == "$__event_before" ]] || exit 86
        fi
        builtin mapfile -t __records <"$COSH_BASH_TEST_WORKDIR/user.err"
        [[ ${#__records[@]} == 1 && ${__records[0]} == 'USER_ERR:5:( exit 5 )' ]] || exit 87
        builtin mapfile -t __records <"$COSH_BASH_TEST_WORKDIR/hook.calls"
        [[ ${#__records[@]} == 2 ]] || exit 88
        builtin printf 'HOOK_RETREAT_OK:%s\n' "$COSH_PROMPT_MODE"
        exit 0
    fi
    PROMPT_COMMAND=()
    builtin trap - ERR
    builtin mapfile -t __records <"$COSH_BASH_TEST_WORKDIR/user.err"
    builtin printf 'OBS_ERR:%s\n' "${__records[@]}"
    builtin mapfile -t __records <"$COSH_BASH_TEST_WORKDIR/user.pc"
    builtin printf 'OBS_PC:%s\n' "${__records[@]}"
    builtin mapfile -t __records <"$COSH_BASH_TEST_WORKDIR/hook.calls"
    case $COSH_PROMPT_PHASE in registered) __expected_calls=1 ;; active) __expected_calls=2 ;; esac
    [[ ${#__records[@]} == "$__expected_calls" ]] || exit 82
    builtin printf 'OBS_HELPER:%s\n' "${#__records[@]}"
    exit 0
    ;;
esac
