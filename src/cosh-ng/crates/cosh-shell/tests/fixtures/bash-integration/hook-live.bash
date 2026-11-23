case ${1-setup} in
 before-first)
  case $COSH_BASH_TEST_CASE in hook_live_candidate|hook_live_*_candidate)
   [[ ${__cosh_hook_active-} == 1 && ${PROMPT_COMMAND[$__live_slot]-} == "$__live_entry" && -n $(builtin trap -p ERR) ]] || exit 80 ;;
  esac
  if [[ $COSH_BASH_TEST_CASE == hook_live_fault_timeout_candidate ]]; then [[ -x /bin/sleep ]] || exit 80; fi
  if [[ $COSH_BASH_TEST_CASE == hook_live_fault_missing_candidate ]]; then
   __live_helper='/usr/libexec/anolisa/cosh-ng/cosh-shell'
   [[ $__live_helper == "$COSH_BASH_TEST_ASSETS/"* && -x $__live_helper ]] || exit 80
   /bin/rm -- "$__live_helper" || exit 80
   [[ ! -e $__live_helper ]] || exit 80
   builtin printf '%s\n' HOOK_HELPER_REMOVED
  fi
  builtin return 0
  ;;
 after-first)
  __live_first_ok=1
  if [[ $COSH_BASH_TEST_CASE == hook_live_skip_candidate ]]; then
   [[ ${PROMPT_COMMAND[$__live_slot]-} == "$__live_entry" && -n $(builtin trap -p ERR) ]] || __live_first_ok=0
  elif [[ $COSH_BASH_TEST_CASE == hook_live_candidate || $COSH_BASH_TEST_CASE == hook_live_fault_*_candidate ]]; then
   [[ ! ${PROMPT_COMMAND[$__live_slot]+x} && -z $(builtin trap -p ERR) ]] || __live_first_ok=0
  fi
  [[ ${PROMPT_COMMAND[0]} == "$__live_native" ]] || __live_first_ok=0
  builtin printf 'HOOK_FIRST_STATE:%s\n' "$__live_first_ok"
  builtin return 0
  ;;
 after-final)
  __live_final_ok=1
  case $COSH_BASH_TEST_CASE in hook_live_candidate|hook_live_*_candidate)
   [[ ! ${PROMPT_COMMAND[$__live_slot]+x} && -z $(builtin trap -p ERR) ]] || __live_final_ok=0 ;;
  esac
  [[ ${PROMPT_COMMAND[0]} == "$__live_native" ]] || __live_final_ok=0
  builtin printf 'HOOK_FINAL_STATE:%s\n' "$__live_final_ok"
  PROMPT_COMMAND[0]='builtin : live-user-native'
  builtin trap 'builtin printf "%s\n" user-owned >>"$COSH_BASH_TEST_WORKDIR/live-user.err"' ERR
  builtin trap -p ERR >"$COSH_BASH_TEST_WORKDIR/live-user-trap.before"
  builtin return 0
  ;;
 finish)
  [[ $__live_first_ok == 1 && $__live_final_ok == 1 ]] || exit 94
  builtin trap -p ERR >"$COSH_BASH_TEST_WORKDIR/live-user-trap.after"
  [[ ${PROMPT_COMMAND[0]} == 'builtin : live-user-native' &&
     $(<"$COSH_BASH_TEST_WORKDIR/live-user-trap.before") == "$(<"$COSH_BASH_TEST_WORKDIR/live-user-trap.after")" &&
     $(<"$COSH_BASH_TEST_WORKDIR/live-user.err") == user-owned ]] || exit 95
  builtin printf '%s\n' HOOK_LIFECYCLE_OK
  builtin return 0
  ;;
esac
TERM=xterm PS1='FIXTURE> ' PS2='SECONDARY> '
PATH="$COSH_BASH_TEST_WORKDIR/bin:/usr/bin:/bin"
PROMPT_COMMAND=('printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/\~}"')
__live_native=${PROMPT_COMMAND[0]}
case $COSH_BASH_TEST_CASE in hook_live_candidate|hook_live_*_candidate)
 builtin source "$COSH_BASH_TEST_ASSETS/cosh-integration.bash"
 [[ ${__cosh_bash_registered-} == 1 ]] || exit 80
 __live_slot=$__cosh_hook_slot
 __live_entry=${PROMPT_COMMAND[$__live_slot]}
 ;; esac
