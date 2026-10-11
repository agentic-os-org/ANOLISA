_cosh_begin_attempt() {
  local input="$1"
  local top_token="$2"
  local expansion_drift="${3:-0}"
  local generation="${4:-}"
  local utf8_status
  if [[ "$generation" =~ ^[0-9]+$ ]]; then
    _COSH_ATTEMPT_GENERATION="$generation"
  else
    _COSH_ATTEMPT_GENERATION=$((_COSH_ATTEMPT_GENERATION + 1))
  fi
  _COSH_ATTEMPT_ACTIVE=1
  _COSH_ATTEMPT_WRAPPER_ID="$_COSH_WRAPPER_ID"
  _COSH_ATTEMPT_SENSITIVE=0
  _COSH_ATTEMPT_UNSAFE=0
  _COSH_ATTEMPT_EXPANSION_DRIFT="$expansion_drift"
  _COSH_ATTEMPT_SUBSHELL="${BASH_SUBSHELL:-0}"
  _COSH_ATTEMPT_INPUT=
  _COSH_ATTEMPT_TOKEN=
  _COSH_ATTEMPT_TOKEN_FINGERPRINT=
  if _cosh_command_has_secret "$input"; then
    _COSH_ATTEMPT_SENSITIVE=1
  fi
  _cosh_utf8_han_status "$input"
  utf8_status=$?
  if (( utf8_status == 2 )); then
    _COSH_ATTEMPT_UNSAFE=1
    _COSH_ATTEMPT_TOKEN_FINGERPRINT="$(_cosh_token_fingerprint "$top_token")" || _COSH_ATTEMPT_ACTIVE=0
    return 0
  fi
  _COSH_ATTEMPT_INPUT="$input"
  _COSH_ATTEMPT_TOKEN="$top_token"
}
_cosh_token_fingerprint() {
  local result
  result="$(printf '%s\n' "$1" | command cksum 2>/dev/null)" || return 1
  printf '%s' "${result%% *}"
}
_cosh_delegate_bash_command_not_found() {
  if [[ "${_COSH_IN_USER_COMMAND_NOT_FOUND:-0}" == 1 ]]; then
    printf 'bash: %s: command not found\n' "$1" >&2
    return 127
  fi
  if [[ "${_COSH_HAS_USER_COMMAND_NOT_FOUND:-0}" == 1 ]]; then
    _COSH_IN_USER_COMMAND_NOT_FOUND=1
    _cosh_user_command_not_found_handle "$@"
    local status=$?
    local final_xtrace=0
    if [[ $- == *x* ]]; then
      final_xtrace=1
      set +x
    fi
    _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE="$final_xtrace"
    _COSH_IN_USER_COMMAND_NOT_FOUND=0
    return "$status"
  fi
  printf 'bash: %s: command not found\n' "$1" >&2
  return 127
}
_cosh_user_handler_definition="$(declare -f command_not_found_handle 2>/dev/null || true)"
if [[ -n "$_cosh_user_handler_definition"
   && "$_cosh_user_handler_definition" != "$_COSH_INITIAL_COMMAND_NOT_FOUND_HANDLE" ]]; then
  _cosh_user_handler_definition="${_cosh_user_handler_definition/command_not_found_handle/_cosh_user_command_not_found_handle}"
  # Resume at the start of user code so the private wrapper invocation cannot
  # expose dynamic handler arguments before the user's own trace begins.
  _cosh_user_handler_xtrace_prefix=$'{ \n  if [[ "${_COSH_COMMAND_NOT_FOUND_XTRACE:-0}" == 1 ]]; then set -x; fi\n'
  _cosh_user_handler_definition="${_cosh_user_handler_definition/\{ /$_cosh_user_handler_xtrace_prefix}"
  eval "$_cosh_user_handler_definition"
  _COSH_HAS_USER_COMMAND_NOT_FOUND=1
else
  _COSH_HAS_USER_COMMAND_NOT_FOUND=0
fi
unset _cosh_user_handler_definition _cosh_user_handler_xtrace_prefix \
  _COSH_INITIAL_COMMAND_NOT_FOUND_HANDLE
_cosh_command_not_found_handle() {
  local command="$1"
  shift || true
  if [[ "${_COSH_ATTEMPT_ACTIVE:-0}" != 1 ]]; then
    local history_entry history_no history_command classification_command first_word
    if [[ -n "${_COSH_PENDING_RECOVERY_INPUT+x}" ]]; then
      history_command="$_COSH_PENDING_RECOVERY_INPUT"
      classification_command="${_COSH_PENDING_RECOVERY_CLASSIFICATION:-$history_command}"
      unset _COSH_PENDING_RECOVERY_INPUT _COSH_PENDING_RECOVERY_CLASSIFICATION \
        2>/dev/null || true
    else
      history_entry="$(_cosh_history_entry)"
      history_no="$(_cosh_history_no "$history_entry")"
      history_command="$(_cosh_history_command_from_entry "$history_entry")"
      classification_command="$history_command"
    fi
    first_word="${classification_command%%[[:space:]]*}"
    if [[ -n "$history_command" ]] \
       && ! _cosh_has_leading_alias "$classification_command" \
       && _cosh_literal_first_word_matches "$classification_command" "$first_word" "$command"; then
      _cosh_begin_attempt "$history_command" "$first_word" 0 "$history_no"
    fi
  fi
  local original="${_COSH_ATTEMPT_INPUT:-}"
  local classification_original="$original"
  while [[ "$classification_original" == ' '* || "$classification_original" == $'\t'* ]]; do
    classification_original="${classification_original#?}"
  done
  if [[ "${_COSH_HANDOFF_ACTIVE:-0}" == 1 ]]; then
    _cosh_delegate_bash_command_not_found "$command" "$@"
    return $?
  fi
  if [[ "${_COSH_ATTEMPT_ACTIVE:-0}" != 1
     || "${_COSH_ATTEMPT_WRAPPER_ID:-}" != "$_COSH_WRAPPER_ID" ]]; then
    _cosh_delegate_bash_command_not_found "$command" "$@"
    return $?
  fi
  if [[ "${_COSH_ATTEMPT_SUBSHELL:-}" != "${BASH_SUBSHELL:-0}"
     || "${#FUNCNAME[@]}" != 2
     || "${_COSH_ATTEMPT_EXPANSION_DRIFT:-0}" == 1 ]]; then
    _cosh_delegate_bash_command_not_found "$command" "$@"
    return $?
  fi
  if [[ "${_COSH_ATTEMPT_UNSAFE:-0}" == 1 ]]; then
    local command_fingerprint
    command_fingerprint="$(_cosh_token_fingerprint "$command")"
    if [[ -z "$command_fingerprint"
       || "$command_fingerprint" != "${_COSH_ATTEMPT_TOKEN_FINGERPRINT:-}" ]]; then
      _cosh_delegate_bash_command_not_found "$command" "$@"
      return $?
    fi
    _COSH_ATTEMPT_ACTIVE=0
    local sensitive=false
    [[ "${_COSH_ATTEMPT_SENSITIVE:-0}" == 1 ]] && sensitive=true
    _cosh_emit_top_level_missing_marker "ambiguous" "$sensitive" true
    _cosh_delegate_bash_command_not_found "$command" "$@"
    return $?
  fi
  if [[ -z "$original" ]] \
     || ! _cosh_literal_first_word_matches "$classification_original" "${_COSH_ATTEMPT_TOKEN:-}" "$command" \
     || ! _cosh_arguments_have_no_unquoted_expansion "$classification_original"; then
    _cosh_delegate_bash_command_not_found "$command" "$@"
    return $?
  fi
  if _cosh_is_pending_handoff_command "$original"; then
    _cosh_delegate_bash_command_not_found "$command" "$@"
    return $?
  fi
  _COSH_ATTEMPT_ACTIVE=0
  local sensitive=false
  [[ "${_COSH_ATTEMPT_SENSITIVE:-0}" == 1 ]] && sensitive=true
  local reason
  if reason="$(_cosh_should_intercept_unknown "$command" "$classification_original" "$(($# + 1))")"; then
    _cosh_emit_intercept_marker "$original" "$reason" false "$sensitive"
    return 0
  fi
  local intent
  intent="$(_cosh_classify_missing "$classification_original" "$command")"
  if [[ "$intent" == "natural_language" ]] && _cosh_ai_enabled; then
    _cosh_confirm_pending_recovery_history "$original"
    if [[ "${_COSH_HAS_USER_COMMAND_NOT_FOUND:-0}" == 1 ]]; then
      _cosh_emit_top_level_missing_marker "$intent" "$sensitive" false
      _cosh_delegate_bash_command_not_found "$command" "$@"
      return $?
    fi
    _cosh_emit_intercept_marker "$original" "natural_language" false "$sensitive"
    return 0
  fi
  _cosh_emit_top_level_missing_marker "$intent" "$sensitive" false
  _cosh_delegate_bash_command_not_found "$command" "$@"
  return $?
}
command_not_found_handle() {
  local _COSH_COMMAND_NOT_FOUND_XTRACE=0
  local _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE=0
  if [[ $- == *x* ]]; then
    _COSH_COMMAND_NOT_FOUND_XTRACE=1
    _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE=1
    set +x
  fi
  _cosh_command_not_found_handle "$@"
  local status=$?
  (( _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE == 1 )) && set -x
  return "$status"
}

# Expands the leading command word of a history line following bash alias
# rules and stores the whitespace-compacted result in _COSH_EXPANDED_COMPACT
# (out-parameter form: $(...) would fork a subshell inside the DEBUG trap).
# Leaves _COSH_EXPANDED_COMPACT empty when no alias applies. Builtin-only:
# no subprocess, no fork.
#
# BASH_ALIASES requires bash 4+. On bash 3.x the associative array does not
# exist and ${BASH_ALIASES[$word]} would evaluate the subscript as an
# arithmetic expression (breaking on words like "/help"), so the capability
# is probed once at load time and the helper degrades to the pre-fix guard.
_COSH_HAS_BASH_ALIASES=0
if (( ${BASH_VERSINFO[0]:-0} >= 4 )); then
  _COSH_HAS_BASH_ALIASES=1
fi

_cosh_has_leading_alias() {
  local command="$1"
  local rest="$command"
  local word
  [[ "${_COSH_HAS_BASH_ALIASES:-0}" == 1 ]] || return 1
  while [[ "$rest" =~ ^[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+ ]]; do
    rest="${rest:${#BASH_REMATCH[0]}}"
  done
  word="${rest%%[[:space:]]*}"
  [[ -n "$word" && -n "${BASH_ALIASES[$word]:-}" ]]
}

_cosh_compact_alias_expanded() {
  local command="$1" expanded=0 guard=0 prefix rest word expansion done_prefix=""
  _COSH_EXPANDED_COMPACT=""
  if [[ "${_COSH_HAS_BASH_ALIASES:-0}" != 1 ]]; then
    return 0
  fi
  # Depth cap: deeper alias chains are vanishingly rare in practice; on
  # overflow the compact expansion stays incomplete, the stale-history guard
  # reports a mismatch, and the untracked fallback closes the handoff with
  # degraded evidence instead of deadlocking.
  while (( guard++ < 10 )); do
    prefix=""
    rest="$command"
    # Skip leading NAME=VALUE assignments (covers handoff wrapper prefixes);
    # bash still alias-expands the command word after assignments.
    while [[ "$rest" =~ ^[A-Za-z_][A-Za-z0-9_]*=[^[:space:]]*[[:space:]]+ ]]; do
      prefix+="${BASH_REMATCH[0]}"
      rest="${rest:${#BASH_REMATCH[0]}}"
    done
    word="${rest%%[[:space:]]*}"
    expansion="${BASH_ALIASES[$word]:-}"
    if [[ -z "$expansion" ]]; then
      break
    fi
    expanded=1
    command="${prefix}${expansion}${rest:${#word}}"
    # bash stops recursive expansion when the expansion starts with the
    # word being expanded (e.g. ls='ls --color=auto'); the single-round
    # expansion must still be reported. A trailing blank in the alias
    # value makes bash alias-expand the next word as well, so freeze the
    # settled part into done_prefix and keep expanding after it.
    if [[ "${expansion%%[[:space:]]*}" == "$word" ]]; then
      if [[ "$expansion" =~ [[:space:]]$ ]]; then
        done_prefix+="${prefix}${expansion}"
        command="${rest:${#word}}"
        command="${command#"${command%%[![:space:]]*}"}"
        if [[ -z "$command" ]]; then
          break
        fi
        continue
      fi
      break
    fi
    # The same trailing-blank rule applies when the expansion changed the
    # command word: settle the expansion and continue with the next word.
    if [[ "$expansion" =~ [[:space:]]$ ]]; then
      done_prefix+="${prefix}${expansion}"
      command="${rest:${#word}}"
      command="${command#"${command%%[![:space:]]*}"}"
      if [[ -z "$command" ]]; then
        break
      fi
    fi
  done
  if (( expanded )); then
    command="${done_prefix}${command}"
    _COSH_EXPANDED_COMPACT="${command//[[:space:]]/}"
  fi
}

_cosh_bounded_preexec_marker() {
  local history_entry history_no command first_word argc=1 display_command sensitive=false
  local classification_command pending_recovery=false
  if [[ -n "${_COSH_PENDING_SLASH_INPUT+x}" ]]; then
    printf '\r\033[K' > /dev/tty
    return 0
  fi
  history_entry="$(_cosh_history_entry)"
  history_no="$(_cosh_history_no "$history_entry")"
  command="$(_cosh_history_command_from_entry "$history_entry")"
  classification_command="$command"
  if [[ -n "${_COSH_PENDING_RECOVERY_INPUT+x}" ]]; then
    command="$_COSH_PENDING_RECOVERY_INPUT"
    classification_command="${_COSH_PENDING_RECOVERY_CLASSIFICATION:-$command}"
    pending_recovery=true
  fi
  if [[ -n "${COSH_HANDOFF_REQUEST_FILE:-}" && -f "$COSH_HANDOFF_REQUEST_FILE" ]]; then
    return 0
  fi
  if [[ "$pending_recovery" != true
     && ( -z "$history_no" || -z "$command"
     || "$history_no" == "$(_cosh_last_preexec_history_no)" ) ]]; then
    # PS0 still proves a top-level execution boundary when Bash deliberately
    # omitted the accepted line from history. Keep the ledger complete but do
    # not guess command text from a stale entry or route it to Agent.
    _COSH_ATTEMPT_GENERATION=$((_COSH_ATTEMPT_GENERATION + 1))
    _cosh_emit_marker "preexec" "<redacted untracked command>" 0 false
    return 0
  fi
  if [[ "$pending_recovery" != true ]]; then
    _cosh_remember_preexec_history_no "$history_no"
  fi
  first_word="${classification_command%%[[:space:]]*}"
  [[ "$classification_command" == *[[:space:]]* ]] && argc=2

  # PS0 is observation-only and cannot suppress execution. Missing commands
  # route through command_not_found_handle; ordinary slash controls stay in
  # the Rust input relay. A slash line that reaches PS0 came from Readline
  # history or editing and is deliberately Shell-owned, so it still needs an
  # observed command boundary.
  local reason
  if [[ "${_COSH_HAS_USER_COMMAND_NOT_FOUND:-0}" != 1 ]] \
     && reason="$(_cosh_should_intercept_unknown "$first_word" "$classification_command" "$argc")"; then
    if [[ "$reason" != slash ]]; then
      return 0
    fi
  fi
  if ! builtin type -t -- "$first_word" >/dev/null 2>&1; then
    local intent
    intent="$(_cosh_classify_missing "$classification_command" "$first_word")"
    if [[ "${_COSH_HAS_USER_COMMAND_NOT_FOUND:-0}" != 1 \
       && "$intent" == natural_language ]] && _cosh_ai_enabled; then
      return 0
    fi
  fi

  display_command="$command"
  if _cosh_is_handoff_wrapper "$command"; then
    display_command="$(_cosh_unwrap_handoff_command "$command")"
  fi
  _cosh_command_has_secret "$display_command" && sensitive=true
  if [[ "$sensitive" == true ]]; then
    display_command="<redacted sensitive command>"
  fi
  if [[ "$pending_recovery" == true ]]; then
    _COSH_ATTEMPT_GENERATION=$((_COSH_ATTEMPT_GENERATION + 1))
  else
    _COSH_ATTEMPT_GENERATION="$history_no"
  fi
  _cosh_emit_marker "preexec" "$display_command" 0 false
}
_COSH_STARTUP_ENVIRONMENT_REPORTED=0
_cosh_report_startup_environment_once() {
  if [[ "${COSH_SHELL_BOOTSTRAP_PATH:-1}" == 0 || "${COSH_LOGIN_SHELL:-}" != 1 || "${_COSH_STARTUP_ENVIRONMENT_REPORTED:-0}" == 1 ]]; then
    return 0
  fi
  _COSH_STARTUP_ENVIRONMENT_REPORTED=1
  local startup_path="${PATH-}"
  if (( ${#startup_path} > 8192 )); then
    return 0
  fi
  printf '\033]1337;COSH;{"event":"startup_environment","token":"%s","session_id":"%s","path":"%s"}\a' \
    "$(_cosh_json_escape "$_COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$(_cosh_json_escape "$startup_path")" > /dev/tty 2>/dev/null || true
}
_cosh_bounded_prompt_marker() {
  _cosh_precmd_marker "${1:-$?}"
  return 0
}
_cosh_precmd_marker() {
  local status="${1:-$?}"
  _cosh_apply_internal_recovery
  _cosh_commit_pending_recovery_history
  _cosh_commit_pending_slash
  _cosh_scrub_sensitive_native_history
  _cosh_replace_handoff_history
  # Only the handoff's own prompt boundary may clear the staged files: an
  # unrelated command finishing while a handoff is still pending must not
  # destroy the request/token sidecars it is about to consume (#2142 review).
  if [[ "${_COSH_HANDOFF_ACTIVE:-0}" == 1 ]]; then
    _cosh_clear_handoff_request
  fi
  _cosh_restore_handoff_pager_policy
  unset _COSH_HANDOFF_ACTIVE 2>/dev/null || true
  unset _COSH_PENDING_RECOVERY_INPUT _COSH_PENDING_RECOVERY_CLASSIFICATION \
    _COSH_PENDING_RECOVERY_HISTORY_INPUT 2>/dev/null || true
  _COSH_ATTEMPT_ACTIVE=0
  # The precmd marker still carries the handoff token (#2142): it closes the
  # same command the preexec claimed. Cleared right after so the following
  # prompt_ready and ordinary markers stay token-free.
  _cosh_emit_boundary_marker "$status"
  unset _COSH_HANDOFF_TOKEN 2>/dev/null || true
  _COSH_AT_PROMPT=1
}
{
  if [[ -n "${COSH_SHELL_ISOLATED:-}" ]]; then
    unset PROMPT_COMMAND
    builtin history -c 2>/dev/null || true
  fi
  _COSH_USER_PS0="${PS0-}"
  _cosh_initial_history_entry="$(_cosh_history_entry)"
  _cosh_remember_preexec_history_no "$(_cosh_history_no "$_cosh_initial_history_entry")"
  unset _cosh_initial_history_entry
  PS0='$(set +x; trap - DEBUG RETURN ERR 2>/dev/null; _cosh_bounded_preexec_marker > /dev/tty)'"$_COSH_USER_PS0"
  _COSH_USER_PROMPT_COMMAND=()
  if [[ -z "${COSH_SHELL_ISOLATED:-}" ]]; then
    if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
      _COSH_USER_PROMPT_COMMAND=("${PROMPT_COMMAND[@]}")
    elif [[ -n "${PROMPT_COMMAND+x}" ]]; then
      _COSH_USER_PROMPT_COMMAND=("$PROMPT_COMMAND")
    fi
  fi
  # Keep user prompt hooks at top level so their shell-state changes persist.
  # Scratch state uses Cosh's reserved namespace and is cleared before traps
  # resume. Bash before 5.1 executes only element zero of an array
  # PROMPT_COMMAND, so those versions receive the same sequence as one scalar
  # command.
  PROMPT_COMMAND=('_COSH_PROMPT_STATUS="$?"; _COSH_PROMPT_XTRACE=0; case "$-" in *x*) _COSH_PROMPT_XTRACE=1; set +x;; esac; _COSH_PROMPT_DEBUG_TRAP="$(trap -p DEBUG 2>/dev/null)"; _COSH_PROMPT_RETURN_TRAP="$(trap -p RETURN 2>/dev/null)"; _COSH_PROMPT_ERR_TRAP="$(trap -p ERR 2>/dev/null)"; trap - DEBUG RETURN ERR 2>/dev/null; eval "unset _COSH_PROMPT_DEBUG_TRAP _COSH_PROMPT_RETURN_TRAP _COSH_PROMPT_ERR_TRAP ${_COSH_PROMPT_RETURN_TRAP:+; ${_COSH_PROMPT_RETURN_TRAP}} ${_COSH_PROMPT_ERR_TRAP:+; ${_COSH_PROMPT_ERR_TRAP}} ${_COSH_PROMPT_DEBUG_TRAP:+; ${_COSH_PROMPT_DEBUG_TRAP}}"; if (( _COSH_PROMPT_XTRACE == 1 )); then unset _COSH_PROMPT_XTRACE; set -x; else unset _COSH_PROMPT_XTRACE; fi')
  PROMPT_COMMAND+=("${_COSH_USER_PROMPT_COMMAND[@]}")
  PROMPT_COMMAND+=('_COSH_PROMPT_XTRACE=0; case "$-" in *x*) _COSH_PROMPT_XTRACE=1; set +x;; esac; _COSH_PROMPT_DEBUG_TRAP="$(trap -p DEBUG 2>/dev/null)"; _COSH_PROMPT_RETURN_TRAP="$(trap -p RETURN 2>/dev/null)"; _COSH_PROMPT_ERR_TRAP="$(trap -p ERR 2>/dev/null)"; trap - DEBUG RETURN ERR 2>/dev/null; _cosh_report_startup_environment_once; _cosh_bounded_prompt_marker "$_COSH_PROMPT_STATUS" > /dev/tty; eval "unset _COSH_PROMPT_STATUS _COSH_PROMPT_DEBUG_TRAP _COSH_PROMPT_RETURN_TRAP _COSH_PROMPT_ERR_TRAP ${_COSH_PROMPT_RETURN_TRAP:+; ${_COSH_PROMPT_RETURN_TRAP}} ${_COSH_PROMPT_ERR_TRAP:+; ${_COSH_PROMPT_ERR_TRAP}} ${_COSH_PROMPT_DEBUG_TRAP:+; ${_COSH_PROMPT_DEBUG_TRAP}}"; if (( _COSH_PROMPT_XTRACE == 1 )); then unset _COSH_PROMPT_XTRACE; set -x; else unset _COSH_PROMPT_XTRACE; fi')
  if (( BASH_VERSINFO[0] < 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] < 1) )); then
    _COSH_PROMPT_COMMAND_SCALAR="${PROMPT_COMMAND[0]}"
    if [[ -n "${_COSH_USER_PROMPT_COMMAND[0]-}" ]]; then
      _COSH_PROMPT_COMMAND_SCALAR+=$'\n'"${_COSH_USER_PROMPT_COMMAND[0]}"
    fi
    _COSH_PROMPT_COMMAND_SCALAR+=$'\n'"${PROMPT_COMMAND[${#PROMPT_COMMAND[@]}-1]}"
    unset PROMPT_COMMAND
    PROMPT_COMMAND="$_COSH_PROMPT_COMMAND_SCALAR"
    unset _COSH_PROMPT_COMMAND_SCALAR
  fi
  if [[ "${_COSH_USER_HISTORY_ENABLED:-0}" == 1 ]]; then
    set -o history
  fi
  # The relay inserts this private sequence at a recognized main-prompt
  # accept boundary. The widget inspects and clears Readline's unexpanded
  # buffer so guarded slash controls never reach Bash parsing.
  for _cosh_keymap in emacs-standard emacs-meta vi-insert vi-command; do
    bind -m "$_cosh_keymap" -x '"\e[99~":_cosh_guard_slash_submission' 2>/dev/null || true
    bind -m "$_cosh_keymap" -x '"\e[100~":_cosh_guard_private_slash_submission' 2>/dev/null || true
    bind -m "$_cosh_keymap" -x '"\e[101~":_cosh_guard_recoverable_history_submission' 2>/dev/null || true
  done
  unset _cosh_keymap
  IFS= read -r _COSH_USER_DEBUG_TRAP < "${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.user-debug-trap" || true
  rm -f -- "${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.user-debug-trap" 2>/dev/null || true
  if [[ -n "${_COSH_USER_DEBUG_TRAP:-}" ]]; then
    eval "$_COSH_USER_DEBUG_TRAP"
  fi
}
