#!/bin/bash
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
