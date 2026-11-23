#!/bin/bash
case $1 in
setup)
    TERM=xterm-256color
    PATH="$COSH_BASH_TEST_WORKDIR/bin:/usr/bin:/bin"
    PROMPT_COMMAND=('printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
    __chld_native=${PROMPT_COMMAND[0]}
    builtin source "$COSH_BASH_TEST_ASSETS/cosh-integration.bash"
    [[ ${__cosh_bash_registered-} == 1 && $__cosh_hook_active == 0 ]] || exit 80
    __chld_slot=$__cosh_hook_slot
    if [[ $COSH_CHLD_PHASE == registered ]]; then
        builtin source "$COSH_BASH_TEST_ASSETS/tests/hook-chld.bash" configure
    fi
    ;;
configure)
    case $COSH_CHLD_PHASE in registered) __chld_expected=0 ;; active) __chld_expected=1 ;; esac
    [[ $__cosh_hook_active == "$__chld_expected" &&
       ${PROMPT_COMMAND[$__chld_slot]-} == "$__cosh_hook_entry" ]] || exit 81
    if [[ $COSH_CHLD_MODE == baseline ]]; then
        builtin unset "PROMPT_COMMAND[$__chld_slot]"
        builtin trap - ERR
    fi
    builtin printf '%s\n' BEGIN >"$COSH_BASH_TEST_WORKDIR/chld.calls"
    builtin trap 'builtin printf "CHLD:%s:%s\n" "$?" "$BASH_COMMAND" >>"$COSH_BASH_TEST_WORKDIR/chld.calls"' CHLD
    builtin trap -p CHLD >"$COSH_BASH_TEST_WORKDIR/chld.before"
    ;;
discovered)
    __chld_first_retired=1
    [[ ! ${PROMPT_COMMAND[$__chld_slot]+x} ]] || __chld_first_retired=0
    builtin trap -p ERR >"$COSH_BASH_TEST_WORKDIR/chld.err.discovered"
    [[ ! -s $COSH_BASH_TEST_WORKDIR/chld.err.discovered ]] || __chld_first_retired=0
    builtin printf '%s\n' DISCOVERED >>"$COSH_BASH_TEST_WORKDIR/chld.calls"
    ;;
after-idle)
    builtin printf '%s\n' AFTER_IDLE >>"$COSH_BASH_TEST_WORKDIR/chld.calls"
    ;;
finish)
    builtin trap -p CHLD >"$COSH_BASH_TEST_WORKDIR/chld.after"
    builtin trap -p ERR >"$COSH_BASH_TEST_WORKDIR/chld.err.after"
    __chld_final_retired=1 __chld_native_ok=1
    [[ ! ${PROMPT_COMMAND[$__chld_slot]+x} && ! -s $COSH_BASH_TEST_WORKDIR/chld.err.after ]] || __chld_final_retired=0
    [[ ${PROMPT_COMMAND[0]} == "$__chld_native" ]] || __chld_native_ok=0
    builtin printf 'CHLD_STATE:%s:%s:%s\n' "$__chld_first_retired" "$__chld_final_retired" "$__chld_native_ok"
    exit 0
    ;;
esac
