#!/bin/bash
# Requires enabled POSIX special readonly/exit; builtin-table rewriting is unsupported.

case $- in
    *i*)
        case ${BASH_VERSION-} in
            [5-9].*|[1-9][0123456789].*)
                if [[ ${OSTYPE-} == linux* && $- != *[arxveuTE]* && :$SHELLOPTS: != *:posix:* &&
                      ! ${BASH_EXECUTION_STRING+x} && -t 0 && -t 1 && -t 2 &&
                      ${COSH_BASH_DISABLE-} != 1 && ${COSH_INTERNAL_SHELL-} != 1 &&
                      -z ${COSH_SESSION_ID-} && ${COSH_SHELL_ISOLATED-} != 1 &&
                      ! ${BASH_ENV+x} && ! ${ENV+x} && ! ${BASH_XTRACEFD+x} &&
                      ! ${GLIBC_TUNABLES+x} ]] &&
                   {
                       if ( if POSIXLY_CORRECT=1 && [[ :$SHELLOPTS: == *:posix:* ]]; then
                           if \readonly -f builtin 2>/dev/null; then \exit 0
                           else case $? in 1) \exit 2 ;; *) \exit 1 ;; esac; fi
                         else ((0)); fi ) 2>/dev/null; then ((0))
                       else case $? in 2) ((1)) ;; *) ((0)) ;; esac; fi
                   } &&
                   \builtin shopt -q login_shell && ! \builtin shopt -q extdebug &&
                   ! \builtin compgen -A variable LD_ >/dev/null &&
                   ! \builtin compgen -A variable __cosh_bash_profile_ >/dev/null; then
                    __cosh_bash_profile_seen=1
                    for __cosh_bash_profile_step in 1; do
                        if __cosh_bash_profile_decl=$(\builtin declare -p __cosh_bash_spent_us 2>/dev/null); then
                            case $__cosh_bash_profile_decl in
                                'declare -- '*) ;;
                                *) \builtin break ;;
                            esac
                        fi
                        __cosh_bash_profile_spent=${__cosh_bash_spent_us-0}
                        [[ -n $__cosh_bash_profile_spent && ${#__cosh_bash_profile_spent} -le 18 &&
                           $__cosh_bash_profile_spent != *[!0123456789]* ]] || \builtin break
                        __cosh_bash_profile_spent=$((10#$__cosh_bash_profile_spent))
                        [[ $__cosh_bash_profile_spent -lt 500000 ]] || \builtin break
                        IFS= \builtin read -r -n 128 __cosh_bash_profile_raw 2>/dev/null </proc/uptime || {
                            __cosh_bash_spent_us=500000
                            \builtin break
                        }
                        __cosh_bash_profile_up=${__cosh_bash_profile_raw%% *}
                        [[ ${#__cosh_bash_profile_raw} -lt 128 && $__cosh_bash_profile_raw == *' '* &&
                           $__cosh_bash_profile_up == *.[0123456789][0123456789] &&
                           $__cosh_bash_profile_up != .* && ${#__cosh_bash_profile_up} -le 15 &&
                           ${__cosh_bash_profile_up//[0123456789]/} == . ]] || {
                            __cosh_bash_spent_us=500000
                            \builtin break
                        }
                        __cosh_bash_profile_start=$((10#${__cosh_bash_profile_up/./} * 10000))
                        __cosh_bash_profile_deadline=$((__cosh_bash_profile_start + 450000 - __cosh_bash_profile_spent))
                        if [[ $__cosh_bash_profile_deadline -gt $((__cosh_bash_profile_start + 350000)) ]]; then
                            __cosh_bash_profile_deadline=$((__cosh_bash_profile_start + 350000))
                        fi
                        [[ -z $(\builtin trap -p DEBUG RETURN) &&
                           -x /usr/bin/timeout && -x /usr/bin/printf &&
                           -x /usr/libexec/anolisa/cosh-ng/cosh-shell ]] || \builtin break
                        ( [[ ${LC_ALL-} == C || ${LC_ALL-} == POSIX ]] || LC_ALL=C ) 2>/dev/null || \builtin break
                        IFS= \builtin read -r -n 128 __cosh_bash_profile_raw 2>/dev/null </proc/uptime || \builtin break
                        __cosh_bash_profile_up=${__cosh_bash_profile_raw%% *}
                        [[ ${#__cosh_bash_profile_raw} -lt 128 && $__cosh_bash_profile_raw == *' '* &&
                           $__cosh_bash_profile_up == *.[0123456789][0123456789] &&
                           $__cosh_bash_profile_up != .* && ${#__cosh_bash_profile_up} -le 15 &&
                           ${__cosh_bash_profile_up//[0123456789]/} == . ]] || \builtin break
                        __cosh_bash_profile_now=$((10#${__cosh_bash_profile_up/./} * 10000))
                        __cosh_bash_profile_helper_deadline=$((__cosh_bash_profile_deadline - 20000))
                        __cosh_bash_profile_ms=$(((__cosh_bash_profile_helper_deadline - __cosh_bash_profile_now) / 1000))
                        [[ $__cosh_bash_profile_now -ge $__cosh_bash_profile_start &&
                           $__cosh_bash_profile_ms -gt 0 && $__cosh_bash_profile_ms -le 350 ]] || \builtin break
                        \builtin printf -v __cosh_bash_profile_timeout '0.%03d' "$__cosh_bash_profile_ms"
                        if ! __cosh_bash_profile_text=$(
                            {
                            /usr/bin/timeout --kill-after=0.005 "$__cosh_bash_profile_timeout" \
                                /usr/libexec/anolisa/cosh-ng/cosh-shell health banner \
                                --budget-ms "$__cosh_bash_profile_ms" \
                                --bash-deadline-boottime-us "$__cosh_bash_profile_helper_deadline" </dev/null |
                                (
                                    [[ ${LC_ALL-} == C || ${LC_ALL-} == POSIX ]] || LC_ALL=C
                                    __cosh_bash_profile_buffer=
                                    if IFS= \builtin read -r -d '' -n 8193 -t "$__cosh_bash_profile_timeout" __cosh_bash_profile_buffer; then
                                        \builtin exit 1
                                    else
                                        __cosh_bash_profile_status=$?
                                    fi
                                    [[ $__cosh_bash_profile_status == 1 && ${#__cosh_bash_profile_buffer} -le 8192 ]] || \builtin exit 1
                                    \builtin printf '%s.' "$__cosh_bash_profile_buffer"
                                ) 2>/dev/null
                            [[ ${PIPESTATUS[0]} == 0 && ${PIPESTATUS[1]} == 0 ]]
                            } 2>/dev/null
                        ); then
                            \builtin break
                        fi
                        __cosh_bash_profile_text=${__cosh_bash_profile_text%.}
                        [[ -n $__cosh_bash_profile_text ]] || \builtin break
                        IFS= \builtin read -r -n 128 __cosh_bash_profile_raw 2>/dev/null </proc/uptime || \builtin break
                        __cosh_bash_profile_up=${__cosh_bash_profile_raw%% *}
                        [[ ${#__cosh_bash_profile_raw} -lt 128 && $__cosh_bash_profile_raw == *' '* &&
                           $__cosh_bash_profile_up == *.[0123456789][0123456789] &&
                           $__cosh_bash_profile_up != .* && ${#__cosh_bash_profile_up} -le 15 &&
                           ${__cosh_bash_profile_up//[0123456789]/} == . ]] || \builtin break
                        __cosh_bash_profile_now=$((10#${__cosh_bash_profile_up/./} * 10000))
                        __cosh_bash_profile_ms=$(((__cosh_bash_profile_deadline - __cosh_bash_profile_now - 10000) / 1000))
                        [[ $__cosh_bash_profile_now -ge $__cosh_bash_profile_start &&
                           $__cosh_bash_profile_ms -gt 0 && $__cosh_bash_profile_ms -le 350 ]] || \builtin break
                        \builtin printf -v __cosh_bash_profile_timeout '0.%03d' "$__cosh_bash_profile_ms"
                        /usr/bin/timeout --kill-after=0.005 "$__cosh_bash_profile_timeout" \
                            /usr/bin/printf '%s' "$__cosh_bash_profile_text" </dev/null 2>/dev/null || \builtin :
                    done
                    if [[ ${__cosh_bash_profile_start+x} ]]; then
                        if IFS= \builtin read -r -n 128 __cosh_bash_profile_raw 2>/dev/null </proc/uptime; then
                            __cosh_bash_profile_up=${__cosh_bash_profile_raw%% *}
                            if [[ ${#__cosh_bash_profile_raw} -lt 128 && $__cosh_bash_profile_raw == *' '* &&
                                  $__cosh_bash_profile_up == *.[0123456789][0123456789] &&
                                  $__cosh_bash_profile_up != .* && ${#__cosh_bash_profile_up} -le 15 &&
                                  ${__cosh_bash_profile_up//[0123456789]/} == . ]]; then
                                __cosh_bash_profile_now=$((10#${__cosh_bash_profile_up/./} * 10000))
                                if [[ $__cosh_bash_profile_now -ge $__cosh_bash_profile_start ]]; then
                                    # /proc reports floor centiseconds; charge the upper bound.
                                    __cosh_bash_spent_us=$((__cosh_bash_profile_spent + __cosh_bash_profile_now + 10000 - __cosh_bash_profile_start))
                                else
                                    __cosh_bash_spent_us=500000
                                fi
                            else
                                __cosh_bash_spent_us=500000
                            fi
                        else
                            __cosh_bash_spent_us=500000
                        fi
                    fi
                    \builtin unset __cosh_bash_profile_step __cosh_bash_profile_decl \
                        __cosh_bash_profile_spent __cosh_bash_profile_raw __cosh_bash_profile_up \
                        __cosh_bash_profile_start __cosh_bash_profile_deadline __cosh_bash_profile_now \
                        __cosh_bash_profile_helper_deadline __cosh_bash_profile_ms \
                        __cosh_bash_profile_timeout __cosh_bash_profile_text 2>/dev/null || \builtin :
                fi
                ;;
        esac
        ;;
esac
