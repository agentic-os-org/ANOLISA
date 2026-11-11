#!/usr/bin/env bash

if [[ $# != 3 || $2 != /* || $3 != /* ]]; then
    builtin printf '%s\n' 'usage: fixture.bash CASE ASSET_DIR WORK_DIR' >&2
    exit 64
fi

__fixture_case=$1
__fixture_assets=$2
__fixture_work=$3

__fixture_fail() {
    command builtin printf '%s\n' "$1" >&2
    command builtin exit 1
}

case $__fixture_case in
    noninteractive)
        [[ -r $__fixture_assets/cosh-profile.sh && -r $__fixture_assets/cosh-integration.bash ]] ||
            __fixture_fail 'shell assets are missing'
        HOME=$__fixture_work/untouched-home
        XDG_RUNTIME_DIR=$__fixture_work/untouched-runtime
        PATH=$__fixture_work/empty-path
        __fixture_calls=0
        cosh() { __fixture_calls=$((__fixture_calls + 1)); return 99; }
        timeout() { __fixture_calls=$((__fixture_calls + 1)); return 99; }
        return() { __fixture_calls=$((__fixture_calls + 1)); builtin return 99; }
        PROMPT_COMMAND=('__fixture_native' '__fixture_user')
        trap ':' ERR
        __fixture_trap_before=$(builtin trap -p ERR)
        builtin source "$__fixture_assets/cosh-profile.sh" >"$__fixture_work/profile.out" 2>"$__fixture_work/profile.err"
        __fixture_status=$?
        [[ $__fixture_status == 0 ]] || __fixture_fail "profile source returned $__fixture_status"
        builtin source "$__fixture_assets/cosh-integration.bash" >"$__fixture_work/module.out" 2>"$__fixture_work/module.err"
        __fixture_status=$?
        [[ $__fixture_status == 0 ]] || __fixture_fail "module source returned $__fixture_status"
        builtin source "$__fixture_assets/bashrc-source.bash" >"$__fixture_work/wrapper.out" 2>"$__fixture_work/wrapper.err"
        __fixture_status=$?
        [[ $__fixture_status == 0 ]] || __fixture_fail "wrapper source returned $__fixture_status"
        [[ ! -s $__fixture_work/profile.out && ! -s $__fixture_work/profile.err &&
           ! -s $__fixture_work/module.out && ! -s $__fixture_work/module.err &&
           ! -s $__fixture_work/wrapper.out && ! -s $__fixture_work/wrapper.err ]] ||
            __fixture_fail 'noninteractive assets produced output'
        [[ $__fixture_calls == 0 ]] || __fixture_fail 'noninteractive assets called a helper'
        [[ ! -e $HOME && ! -e $XDG_RUNTIME_DIR ]] || __fixture_fail 'noninteractive assets touched state'
        [[ ${#PROMPT_COMMAND[@]} == 2 && ${PROMPT_COMMAND[0]} == __fixture_native &&
           ${PROMPT_COMMAND[1]} == __fixture_user ]] || __fixture_fail 'noninteractive assets changed callbacks'
        [[ $(builtin trap -p ERR) == "$__fixture_trap_before" ]] || __fixture_fail 'noninteractive assets changed traps'
        [[ ! ${__cosh_bash_registered+x} ]] || __fixture_fail 'noninteractive assets registered state'
        if builtin declare -F __cosh_bash_prompt >/dev/null; then
            __fixture_fail 'noninteractive assets defined a prompt handler'
        fi
        ;;
    entry_profile_restricted|entry_profile_posix|entry_wrapper_restricted|entry_wrapper_posix)
        [[ $- == *i* && -t 0 && -t 1 && -t 2 ]] ||
            __fixture_fail 'entry mode fixture requires an interactive PTY'
        command builtin shopt -q login_shell || __fixture_fail 'entry mode fixture requires a login shell'
        TERM=xterm
        PROMPT_COMMAND=('__fixture_native' '__fixture_user')
        __fixture_native() { :; }
        __fixture_user() { :; }
        command builtin trap ': user-owned' ERR
        command builtin cd "$__fixture_assets" || __fixture_fail 'entry mode asset directory is missing'
        PATH=$__fixture_assets:/usr/bin:/bin
        case $__fixture_case in
            *_restricted)
                command builtin set -r
                [[ $- == *r* ]] || exit 80
                ;;
            *_posix)
                command builtin set -o posix
                command builtin shopt -oq posix || exit 80
                ;;
        esac
        __fixture_pc_before=$(command builtin declare -p PROMPT_COMMAND)
        __fixture_traps_before=$(command builtin trap -p ERR DEBUG RETURN)
        __fixture_options_before=$(command builtin set +o; command builtin shopt -p)
        __fixture_functions_before=$(command builtin declare -f __fixture_native __fixture_user)
        [[ -n $__fixture_traps_before ]] || { command builtin printf '%s\n' 'ENTRY_MODE_INVALID_TRAP_CAPTURE'; exit 80; }
        case $__fixture_case in
            entry_profile_*) __fixture_entry=cosh-profile.sh ;;
            entry_wrapper_*) __fixture_entry=bashrc-source.bash ;;
        esac
        command builtin source "$__fixture_entry"
        __fixture_status=$?
        [[ $(command builtin declare -p PROMPT_COMMAND) == "$__fixture_pc_before" &&
           $(command builtin trap -p ERR DEBUG RETURN) == "$__fixture_traps_before" &&
           $(command builtin set +o; command builtin shopt -p) == "$__fixture_options_before" &&
           $(command builtin declare -f __fixture_native __fixture_user) == "$__fixture_functions_before" ]] ||
            { command builtin printf '%s\n' 'ENTRY_MODE_CHANGED_USER_STATE'; exit 81; }
        [[ $__fixture_status == 0 && ! -e $__fixture_work/helper.args && ! -e $__fixture_work/hook.calls ]] ||
            { command builtin printf '%s\n' 'ENTRY_MODE_CALLED_HELPER_OR_FAILED'; exit 82; }
        command builtin printf '%s\n' ENTRY_MODE_RELINQUISHED
        ;;
    entry_profile_builtin_shadow_plain_enabled|entry_profile_builtin_shadow_self_enabled|entry_profile_builtin_shadow_plain_disabled|entry_profile_builtin_shadow_self_disabled|entry_wrapper_builtin_shadow_plain_enabled|entry_wrapper_builtin_shadow_self_enabled|entry_wrapper_builtin_shadow_plain_disabled|entry_wrapper_builtin_shadow_self_disabled)
        [[ $- == *i* && -t 0 && -t 1 && -t 2 && ${BASH_VERSINFO[0]} -ge 5 ]] ||
            __fixture_fail 'entry shadow fixture requires interactive Bash 5 with a real PTY'
        command builtin shopt -q login_shell || __fixture_fail 'entry shadow fixture requires a login shell'
        TERM=xterm
        PROMPT_COMMAND=('__fixture_native' '__fixture_user')
        __fixture_native() { :; }
        __fixture_user() { :; }
        command builtin trap ': user-owned' ERR
        case $__fixture_case in *_disabled) COSH_BASH_DISABLE=1 ;; esac
        case $__fixture_case in
            entry_profile_*) __fixture_entry=$__fixture_assets/cosh-profile.sh ;;
            entry_wrapper_*) __fixture_entry=$__fixture_assets/bashrc-source.bash ;;
        esac
        [[ -r $__fixture_entry ]] || __fixture_fail 'entry shadow asset is missing'
        case $__fixture_case in
            *_self_*)
                builtin() {
                    command builtin printf 'builtin:%s\n' "${1-}" >>"$COSH_BASH_TEST_WORKDIR/entry-shadow.calls"
                    command builtin export -nf builtin
                    command builtin "$@"
                }
                ;;
            *)
                builtin() {
                    command builtin printf 'builtin:%s\n' "${1-}" >>"$COSH_BASH_TEST_WORKDIR/entry-shadow.calls"
                    command builtin "$@"
                }
                ;;
        esac
        command builtin export -f builtin
        __fixture_pc_before=$(command builtin declare -p PROMPT_COMMAND)
        command builtin trap -p ERR DEBUG RETURN >"$__fixture_work/entry-traps.before"
        [[ -s $__fixture_work/entry-traps.before ]] || __fixture_fail 'entry shadow trap capture is empty'
        __fixture_traps_before=$(<"$__fixture_work/entry-traps.before")
        __fixture_options_before=$(command builtin set +o; command builtin shopt -p)
        __fixture_function_before=$(command builtin declare -f builtin)
        __fixture_exports_before=$(command builtin export -pf)
        command builtin source "$__fixture_entry"
        __fixture_status=$?
        command builtin trap -p ERR DEBUG RETURN >"$__fixture_work/entry-traps.after"
        __fixture_calls=()
        if [[ -r $__fixture_work/entry-shadow.calls ]]; then
            command builtin mapfile -t __fixture_calls <"$__fixture_work/entry-shadow.calls"
        fi
        [[ ${#__fixture_calls[@]} == 0 ]] || __fixture_fail "entry called user builtin ${#__fixture_calls[@]} times"
        [[ $__fixture_status == 0 ]] || __fixture_fail 'entry shadow source did not fail open'
        [[ ! -e $__fixture_work/helper.args && ! -e $__fixture_work/hook.calls ]] ||
            __fixture_fail 'entry shadow started a helper'
        [[ $(command builtin declare -p PROMPT_COMMAND) == "$__fixture_pc_before" &&
           $(<"$__fixture_work/entry-traps.after") == "$__fixture_traps_before" &&
           $(command builtin set +o; command builtin shopt -p) == "$__fixture_options_before" &&
           $(command builtin declare -f builtin) == "$__fixture_function_before" &&
           $(command builtin export -pf) == "$__fixture_exports_before" ]] ||
            __fixture_fail 'entry shadow changed callbacks, traps, options or function attributes'
        command builtin printf '%s\n' ENTRY_SHADOW_RELINQUISHED
        ;;
    banner|missing_module|user_conflict|budget_exhausted|admin_disabled|internal_shell|dynamic_loader|explicit_command|nonlogin|helper_failure|helper_oversize|helper_slow)
        [[ $- == *i* && -t 0 && -t 1 && ${BASH_VERSINFO[0]} -ge 5 ]] ||
            __fixture_fail 'fixture requires interactive Bash 5 with a real PTY'
        if [[ $__fixture_case == nonlogin ]]; then
            if builtin shopt -q login_shell; then __fixture_fail 'fixture requires a nonlogin shell'; fi
        else
            builtin shopt -q login_shell || __fixture_fail 'fixture requires a login shell'
        fi
        if [[ $__fixture_case == explicit_command && ! ${BASH_EXECUTION_STRING+x} ]]; then
            __fixture_fail 'fixture requires an explicit execution string'
        fi
        PROMPT_COMMAND=('__fixture_native' '__fixture_user')
        trap ':' ERR
        __fixture_trap_before=$(builtin trap -p ERR)
        __fixture_native() { :; }
        __fixture_user() { :; }
        case $__fixture_case in
            budget_exhausted) __cosh_bash_spent_us=500000 ;;
            admin_disabled) COSH_BASH_DISABLE=1 ;;
            internal_shell) COSH_INTERNAL_SHELL=1 ;;
            dynamic_loader) LD_PRELOAD= ;;
            helper_failure) export COSH_BASH_TEST_MODE=failure ;;
            helper_oversize) export COSH_BASH_TEST_MODE=oversize ;;
            helper_slow) export COSH_BASH_TEST_MODE=slow ;;
        esac
        if [[ -r $__fixture_assets/cosh-integration.bash ]]; then
            builtin source "$__fixture_assets/cosh-integration.bash" ||
                __fixture_fail 'module source did not fail open'
        fi
        builtin source "$__fixture_assets/cosh-profile.sh" ||
            __fixture_fail 'profile source did not fail open'
        builtin source "$__fixture_assets/cosh-profile.sh" ||
            __fixture_fail 'repeated profile source did not fail open'
        if [[ $__fixture_case == helper_slow ]]; then
            [[ -r $__fixture_work/helper.pid ]] || __fixture_fail 'slow helper did not start'
            IFS= builtin read -r __fixture_helper_pid <"$__fixture_work/helper.pid"
            if builtin kill -0 "$__fixture_helper_pid" 2>/dev/null; then
                __fixture_fail 'timed out helper is still alive'
            fi
        fi
        [[ ${#PROMPT_COMMAND[@]} == 2 && ${PROMPT_COMMAND[0]} == __fixture_native &&
           ${PROMPT_COMMAND[1]} == __fixture_user ]] || __fixture_fail 'assets changed user callbacks'
        [[ $(builtin trap -p ERR) == "$__fixture_trap_before" ]] ||
            __fixture_fail 'assets changed user trap'
        [[ ! ${__cosh_bash_registered+x} ]] || __fixture_fail 'conflicting Shell state installed hooks'
        case $__fixture_case in
            budget_exhausted|admin_disabled|internal_shell|dynamic_loader|explicit_command|nonlogin)
                [[ ! -e $__fixture_work/helper.args ]] || __fixture_fail 'excluded entry started a helper'
                ;;
            *)
                [[ -r $__fixture_work/helper.args ]] || __fixture_fail 'independent banner was not invoked'
                builtin mapfile -t __fixture_args <"$__fixture_work/helper.args"
                [[ ${#__fixture_args[@]} == 6 && ${__fixture_args[0]} == health &&
                   ${__fixture_args[1]} == banner && ${__fixture_args[2]} == --budget-ms &&
                   ${__fixture_args[3]} -ge 1 && ${__fixture_args[3]} -le 350 &&
                   ${__fixture_args[4]} == --bash-deadline-boottime-us &&
                   ${__fixture_args[5]} != *[!0-9]* ]] ||
                    __fixture_fail 'banner invocation was duplicated or exceeded its budget'
                ;;
        esac
        ;;
    *)
        builtin printf '%s\n' 'unknown shell fixture case' >&2
        exit 64
        ;;
esac
