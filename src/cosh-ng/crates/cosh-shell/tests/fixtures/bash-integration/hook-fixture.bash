#!/bin/bash
__fixture_case=$1 __fixture_assets=$2 __fixture_work=$3
__fixture_registration_diagnostic() {
 builtin printf 'HOOK_PREREQ_FAILED check=%s phase=%s prompt_count=%s registered=%s traps=%s source_us=%s spent_us=%s\n' \
  "${1:-registration}" "${2:-registration}" "${#PROMPT_COMMAND[@]}" "${__cosh_bash_registered-unset}" \
  "$(builtin trap -p ERR DEBUG RETURN)" \
  "$((10#${__fixture_source_finished/./} - 10#${__fixture_source_started/./}))" \
  "${__cosh_bash_spent_us-unset}" >&2
}
TERM=xterm
PROMPT_COMMAND=('printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
__fixture_source_started=$EPOCHREALTIME
builtin source "$__fixture_assets/cosh-integration.bash"
__fixture_source_finished=$EPOCHREALTIME
if [[ $__fixture_case == hook_bad_tail ]]; then
 [[ ${#PROMPT_COMMAND[@]} == 1 && -z $(builtin trap -p ERR DEBUG RETURN) ]] || exit 97
 builtin printf '%s\n' HOOK_BAD_TAIL_OK
 exit
fi
if [[ $__fixture_case == hook_registration_timeout ]]; then
 [[ ${#PROMPT_COMMAND[@]} == 1 && -z $(builtin trap -p ERR DEBUG RETURN) &&
    -z $(builtin declare -F __cosh_hook_boot __cosh_hook_call) &&
    ! ${__cosh_bash_registered+x} ]] || { __fixture_registration_diagnostic; exit 110; }
 __fixture_registration_diagnostic
 builtin printf '%s\n' HOOK_QUIET_RETREAT_OK
 exit
fi
[[ ${#PROMPT_COMMAND[@]} == 2 && -z $(builtin trap -p ERR DEBUG RETURN) ]] || { __fixture_registration_diagnostic; exit 91; }
__fixture_slot=
for __fixture_index in "${!PROMPT_COMMAND[@]}"; do
    if [[ $__fixture_index != 0 ]]; then __fixture_slot=$__fixture_index; fi
done
[[ -n $__fixture_slot && ${PROMPT_COMMAND[$__fixture_slot]} == *boot* ]] || { __fixture_registration_diagnostic; exit 92; }
if [[ $__fixture_case != hook_environment_registered_* ]]; then
 builtin eval "${PROMPT_COMMAND[$__fixture_slot]}"
 if [[ -z $(builtin trap -p ERR) || ${PROMPT_COMMAND[$__fixture_slot]} != *prompt* ]]; then
  __fixture_registration_diagnostic registration activation
  builtin printf 'HOOK_ACTIVATION_FAILED active=%s trusted=%s slot=%s traps=%s\n' \
   "${__cosh_hook_active-unset}" "${__cosh_hook_trusted-unset}" "$__fixture_slot" "$(builtin trap -p ERR)" >&2
  exit 93
 fi
fi
__fixture_owned=${PROMPT_COMMAND[$__fixture_slot]}
if [[ $__fixture_case == hook_environment_* ]]; then
 case $__fixture_case in
  hook_environment_registered_*) __fixture_variable=${__fixture_case#hook_environment_registered_}; __fixture_expected=1 ;;
  hook_environment_active_*) __fixture_variable=${__fixture_case#hook_environment_active_}; __fixture_expected=2 ;;
  *) exit 77 ;;
 esac
 __fixture_native=${PROMPT_COMMAND[0]}
 __fixture_value=
 case $__fixture_variable in
  LD_SENTINEL|GLIBC_TUNABLES|BASH_ENV|ENV|XDG_CONFIG_HOME|BASH_COMPLETION_COMPAT_DIR|BASH_COMPLETION_USER_FILE|BASH_COMPLETION_DEBUG) ;;
  BASH_XTRACEFD)
   builtin exec {__fixture_trace_fd}>"$__fixture_work/user-trace.fd"
   __fixture_value=$__fixture_trace_fd
   builtin printf '%s\n' fd-before >&"$__fixture_trace_fd" || exit 78
   ;;
  COSH_INTERNAL_SHELL|COSH_SHELL_ISOLATED) __fixture_value=1 ;;
  COSH_SESSION_ID) __fixture_value=user-session ;;
  *) exit 77 ;;
 esac
 builtin printf -v "$__fixture_variable" '%s' "$__fixture_value"
 case $__fixture_variable in LD_SENTINEL|BASH_ENV) builtin export "$__fixture_variable" ;; esac
 __fixture_variable_before=$(builtin declare -p "$__fixture_variable")
 builtin :
 builtin eval "$__fixture_owned"
 __fixture_status=$?
 builtin mapfile -t __fixture_calls <"$__fixture_work/hook.calls"
 [[ ${#__fixture_calls[@]} == $__fixture_expected ]] || exit 101
 [[ ! ${PROMPT_COMMAND[$__fixture_slot]+x} && $__cosh_hook_active == -1 ]] || exit 102
 [[ ${PROMPT_COMMAND[0]} == "$__fixture_native" && -z $(builtin trap -p ERR DEBUG RETURN) ]] || exit 103
 [[ $__fixture_status == 0 && $(builtin declare -p "$__fixture_variable") == "$__fixture_variable_before" ]] || exit 104
 PROMPT_COMMAND[0]='builtin : environment-user-native'
 builtin trap 'builtin printf "%s\n" user-err >>"$__fixture_work/user.err"' ERR
 builtin trap -p ERR DEBUG RETURN >"$__fixture_work/user-traps.before"
 builtin :
 builtin eval "$__fixture_owned"
 __fixture_status=$?
 builtin trap -p ERR DEBUG RETURN >"$__fixture_work/user-traps.after"
 [[ $__fixture_status == 0 && ${PROMPT_COMMAND[0]} == 'builtin : environment-user-native' &&
    ! ${PROMPT_COMMAND[$__fixture_slot]+x} && $__cosh_hook_active == -1 &&
    $(<"$__fixture_work/user-traps.before") == "$(<"$__fixture_work/user-traps.after")" &&
    ! -e $__fixture_work/user.err &&
    $(builtin declare -p "$__fixture_variable") == "$__fixture_variable_before" ]] || exit 105
 if [[ $__fixture_variable == BASH_XTRACEFD ]]; then
  builtin printf '%s\n' fd-after >&"$__fixture_trace_fd" || exit 106
  [[ $(<"$__fixture_work/user-trace.fd") == $'fd-before\nfd-after' ]] || exit 106
 else
  builtin unset "$__fixture_variable"
  builtin :
  builtin eval "$__fixture_owned"
  [[ $? == 0 && $__cosh_hook_active == -1 && ! ${PROMPT_COMMAND[$__fixture_slot]+x} ]] || exit 107
 fi
 builtin trap -p ERR DEBUG RETURN >"$__fixture_work/user-traps.after"
 [[ ${PROMPT_COMMAND[0]} == 'builtin : environment-user-native' &&
    $(<"$__fixture_work/user-traps.before") == "$(<"$__fixture_work/user-traps.after")" &&
    ! -e $__fixture_work/user.err ]] || exit 108
 builtin mapfile -t __fixture_calls <"$__fixture_work/hook.calls"
 [[ ${#__fixture_calls[@]} == $__fixture_expected ]] || exit 109
 builtin printf '%s\n' HOOK_ENVIRONMENT_OK
 exit 0
fi
case $__fixture_case in
 hook_option_change) builtin shopt -s nullglob ;;
 hook_trap_change) builtin trap ': user-owned' ERR ;;
 hook_function_change) __cosh_hook_clock() { return 1; } ;;
 hook_native_change) PROMPT_COMMAND[0]='builtin : user-native' ;;
 hook_guide_off) builtin : >"$__fixture_work/runtime/disabled" ;;
 *) PROMPT_COMMAND+=(__fixture_user_callback) ;;
esac
builtin eval "$__fixture_owned"
[[ ! ${PROMPT_COMMAND[$__fixture_slot]+x} ]] || exit 94
if [[ $__fixture_case == hook_trap_change ]]; then
 [[ $(builtin trap -p ERR) == *user-owned* ]] || exit 95
else
 [[ -z $(builtin trap -p ERR) ]] || exit 95
fi
case $__fixture_case in
 hook_option_change) builtin shopt -q nullglob || exit 98 ;;
 hook_native_change) [[ ${PROMPT_COMMAND[0]} == 'builtin : user-native' ]] || exit 98 ;;
 hook_registration) [[ ${#PROMPT_COMMAND[@]} == 2 && ${PROMPT_COMMAND[-1]} == __fixture_user_callback ]] || exit 98 ;;
esac
builtin mapfile -t __fixture_calls <"$__fixture_work/hook.calls"
[[ ${#__fixture_calls[@]} == 2 ]] || exit 96
builtin printf '%s\n' HOOK_COMPONENT_OK
