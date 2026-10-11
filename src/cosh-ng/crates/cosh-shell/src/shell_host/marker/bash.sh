
# Owner: shell_host. bash.rs joins this startup/transport prefix with
# bash_prompt.sh before emitting it into interactive Bash. Preserve the leading
# empty line and marker golden tests when editing either fragment.
if [[ -n "${COSH_OSC_MARKER_LOADED:-}" ]]; then
  return 0 2>/dev/null || exit 0
fi
COSH_OSC_MARKER_LOADED=1
if [[ $- != *i* ]]; then
  return 0 2>/dev/null || exit 0
fi
readonly _COSH_MARKER_TOKEN="$COSH_MARKER_TOKEN"
unset COSH_MARKER_TOKEN 2>/dev/null || true
export COSH_SESSION_ID="${COSH_SESSION_ID:-cosh-osc-$$}"
export COSH_POC_PS1="${COSH_POC_PS1:-cosh-osc$ }"
_COSH_INITIAL_COMMAND_NOT_FOUND_HANDLE="$(declare -f command_not_found_handle 2>/dev/null || true)"
if [[ -z "${COSH_SHELL_ISOLATED:-}" ]]; then
  if [[ "${COSH_LOGIN_SHELL:-}" == "1" ]]; then
    [[ -f /etc/profile ]] && source /etc/profile
    if [[ -f ~/.bash_profile ]]; then source ~/.bash_profile
    elif [[ -f ~/.bash_login ]]; then source ~/.bash_login
    elif [[ -f ~/.profile ]]; then source ~/.profile
    fi
  else
    [[ -f ~/.bashrc ]] && source ~/.bashrc
  fi
fi
trap -p DEBUG > "${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.user-debug-trap" 2>/dev/null || true
trap - DEBUG
_COSH_AI_ENABLED="$_COSH_SESSION_AI_ENABLED"
readonly _COSH_AI_ENABLED
_cosh_assistance_enabled() {
  local status restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  [[ -n "${COSH_ASSISTANCE_STATE_FILE:-}"
     && -f "$COSH_ASSISTANCE_STATE_FILE" ]]
  status=$?
  (( restore_xtrace == 1 )) && set -x
  return "$status"
}
_cosh_ai_enabled() {
  [[ "${_COSH_AI_ENABLED:-1}" == 1 ]] && _cosh_assistance_enabled
}
_cosh_load_native_bash_history_if_empty() {
  if [[ -n "${COSH_SHELL_ISOLATED:-}" ]]; then
    return 0
  fi
  if [[ -z "${HISTFILE:-}" || ! -r "$HISTFILE" ]]; then
    return 0
  fi
  if [[ -n "$(builtin history 1 2>/dev/null)" ]]; then
    return 0
  fi
  builtin history -r "$HISTFILE" 2>/dev/null || true
}
if [[ -z "${COSH_SHELL_ISOLATED:-}" ]]; then
  : # native mode: keep user PS1, HISTFILE, etc.
else
  export PS1="$COSH_POC_PS1"
  set -o history
  export HISTFILE="${COSH_HISTFILE:-/dev/null}"
  export HISTSIZE=1000
  export HISTFILESIZE=1000
  export HISTCONTROL=
  export HISTIGNORE=
  export HISTTIMEFORMAT=
fi
_cosh_load_native_bash_history_if_empty
case ":${HISTCONTROL:-}:" in
  *:ignorespace:*|*:ignoreboth:*) ;;
  *) HISTCONTROL="${HISTCONTROL:+$HISTCONTROL:}ignorespace" ;;
esac
_COSH_USER_HISTORY_ENABLED=0
[[ -o history ]] && _COSH_USER_HISTORY_ENABLED=1
set +o history
_COSH_AT_PROMPT=0
_COSH_ATTEMPT_GENERATION=0
_COSH_ATTEMPT_ACTIVE=0
_COSH_ATTEMPT_INPUT=
_COSH_ATTEMPT_TOKEN=
_COSH_ATTEMPT_TOKEN_FINGERPRINT=
_COSH_ATTEMPT_SENSITIVE=0
_COSH_ATTEMPT_UNSAFE=0
_COSH_ATTEMPT_EXPANSION_DRIFT=0
_COSH_ATTEMPT_SUBSHELL=
unset _COSH_PENDING_RECOVERY_INPUT _COSH_PENDING_RECOVERY_CLASSIFICATION \
  _COSH_PENDING_RECOVERY_HISTORY_INPUT 2>/dev/null || true
rm -f -- "${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.recovery-history-confirmed" \
  2>/dev/null || true
_COSH_WRAPPER_ID="${COSH_SESSION_ID}:${_COSH_MARKER_TOKEN}"
_cosh_apply_internal_recovery() {
  if [[ -z "${COSH_RECOVERY_REQUEST_FILE:-}" || ! -f "$COSH_RECOVERY_REQUEST_FILE" ]]; then
    return 0
  fi
  rm -f -- "$COSH_RECOVERY_REQUEST_FILE" 2>/dev/null || true
  stty echo icanon isig iexten opost 2>/dev/null || true
}
# Shell variables cannot represent NUL; escape every representable C0 byte and DEL.
_cosh_json_escape() {
  local value="$1"
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//$'\001'/\\u0001}; value=${value//$'\002'/\\u0002}
  value=${value//$'\003'/\\u0003}
  value=${value//$'\004'/\\u0004}
  value=${value//$'\005'/\\u0005}
  value=${value//$'\006'/\\u0006}
  value=${value//$'\a'/\\u0007}
  value=${value//$'\b'/\\b}
  value=${value//$'\n'/\\n}
  value=${value//$'\v'/\\u000b}
  value=${value//$'\f'/\\f}
  value=${value//$'\r'/\\r}
  value=${value//$'\016'/\\u000e}
  value=${value//$'\017'/\\u000f}
  value=${value//$'\020'/\\u0010}
  value=${value//$'\021'/\\u0011}
  value=${value//$'\022'/\\u0012}
  value=${value//$'\023'/\\u0013}
  value=${value//$'\024'/\\u0014}
  value=${value//$'\025'/\\u0015}
  value=${value//$'\026'/\\u0016}
  value=${value//$'\027'/\\u0017}
  value=${value//$'\030'/\\u0018}
  value=${value//$'\031'/\\u0019}
  value=${value//$'\032'/\\u001a}
  value=${value//$'\e'/\\u001b}
  value=${value//$'\034'/\\u001c}
  value=${value//$'\035'/\\u001d}
  value=${value//$'\036'/\\u001e}
  value=${value//$'\037'/\\u001f}
  value=${value//$'\t'/\\t}
  value=${value//$'\177'/\\u007f}
  printf '%s' "$value"
}
_cosh_native_history_file_path() {
  if [[ -n "${COSH_SHELL_ISOLATED:-}" || -z "${HISTFILE:-}" ]]; then
    return 1
  fi
  local history_file="$HISTFILE"
  case "$history_file" in
    /*) ;;
    '~') history_file="$HOME" ;;
    '~/'*) history_file="$HOME/${history_file#\~/}" ;;
    *) history_file="$PWD/$history_file" ;;
  esac
  if [[ "$history_file" != /* ]]; then
    return 1
  fi
  if printf '%s' "$history_file" | LC_ALL=C grep -q '[[:cntrl:]]'; then
    return 1
  fi
  printf '%s' "$history_file"
}
_cosh_emit_native_history_file_marker() {
  local history_file="$1"
  local restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  printf '\033]1337;COSH;{"event":"history_file","token":"%s","session_id":"%s","history_file":"%s"}\a' \
    "$(_cosh_json_escape "$_COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$(_cosh_json_escape "$history_file")"
  local marker_status=$?
  (( restore_xtrace == 1 )) && set -x
  return "$marker_status"
}
_cosh_maybe_emit_native_history_file_marker() {
  local history_file last_history_file=""
  history_file="$(_cosh_native_history_file_path)" || return 0
  local state_file="${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.history-file"
  IFS= read -r last_history_file < "$state_file" 2>/dev/null || true
  if [[ "$history_file" == "$last_history_file" ]]; then
    return 0
  fi
  if _cosh_emit_native_history_file_marker "$history_file"; then
    printf '%s\n' "$history_file" > "$state_file" 2>/dev/null || true
  fi
}
_cosh_maybe_emit_native_history_file_marker
_cosh_native_history_file_fragment() {
  local history_file last_history_file=""
  history_file="$(_cosh_native_history_file_path)" || return 0
  local state_file="${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.history-file"
  IFS= read -r last_history_file < "$state_file" 2>/dev/null || true
  if [[ "$history_file" == "$last_history_file" ]]; then
    return 0
  fi
  printf '%s\n' "$history_file" > "$state_file" 2>/dev/null || true
  printf ',\"h\":\"%s\"' "$(_cosh_json_escape "$history_file")"
}
_cosh_now_ms() {
  date +%s000
}
_cosh_history_entry() {
  local saved_fmt="${HISTTIMEFORMAT-}"
  HISTTIMEFORMAT=
  local entry
  entry="$(builtin history 1 2>/dev/null)"
  HISTTIMEFORMAT="$saved_fmt"
  printf '%s' "$entry"
}
_cosh_history_no() {
  printf '%s' "$1" | sed -E 's/^[[:space:]]*([0-9]+).*/\1/'
}
_cosh_history_command_from_entry() {
  local saved_fmt="${HISTTIMEFORMAT-}"
  HISTTIMEFORMAT=
  local entry
  entry="$(builtin history 1 2>/dev/null)"
  HISTTIMEFORMAT="$saved_fmt"
  printf '%s' "$entry" | sed -E 's/^[[:space:]]*[0-9]+[[:space:]]*//'
}
_cosh_last_preexec_history_no() {
  local history_no="${_COSH_LAST_PREEXEC_HISTORY_NO:-}"
  local state_file="${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.preexec-history-no"
  if [[ -f "$state_file" ]]; then
    IFS= read -r history_no < "$state_file" 2>/dev/null || true
  fi
  [[ "$history_no" =~ ^[0-9]+$ ]] && printf '%s' "$history_no"
}
_cosh_remember_preexec_history_no() {
  local history_no="$1"
  local state_file="${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.preexec-history-no"
  local state_dir="${state_file%/*}"
  [[ "$history_no" =~ ^[0-9]+$ ]] || return 0
  _COSH_LAST_PREEXEC_HISTORY_NO="$history_no"
  # The relay may remove its private directory before Bash processes `exit`.
  # Avoid letting that teardown race surface as user-visible shell output.
  [[ "$state_dir" != "$state_file" && -d "$state_dir" ]] || return 0
  printf '%s\n' "$history_no" > "$state_file" 2>/dev/null || true
}
_cosh_command_has_secret() {
  local jwt_pattern='(^|[^A-Za-z0-9_])eyJ[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}\.[A-Za-z0-9_-]{5,}([^A-Za-z0-9_]|$)'
  if [[ "$1" =~ $jwt_pattern ]]; then
    return 0
  fi
  local lower
  lower="$(printf '%s' "$1" | LC_ALL=C tr '[:upper:]' '[:lower:]')"
  case "$lower" in
    *"-----begin "*"private key-----"*)
      return 0
      ;;
  esac
  # ASCII boundaries allow credentials next to CJK text without matching inside
  # identifiers such as npm_package_version or shf_test.
  local bearer_pattern='(^|[^a-z0-9_])bearer[[:space:]]+[a-z0-9._~+/=-]+([^a-z0-9_]|$)'
  local url_password_pattern='[a-z][a-z0-9+.-]*://[^/[:space:]:@]*:[^@/[:space:]]+@'
  local github_pattern='(^|[^a-z0-9_])(gh[pousr]_[a-z0-9_]{20,}|github_pat_[a-z0-9_]{20,})([^a-z0-9_]|$)'
  # A five-character sk- suffix needs a digit so short keys stay private
  # without classifying the sk-hynix product name as a credential.
  local short_sk_pattern='([0-9][a-z0-9_-]{4}|[a-z0-9_-][0-9][a-z0-9_-]{3}|[a-z0-9_-]{2}[0-9][a-z0-9_-]{2}|[a-z0-9_-]{3}[0-9][a-z0-9_-]|[a-z0-9_-]{4}[0-9])'
  local opaque_pattern="(^|[^a-z0-9_])(sk-([a-z0-9_-]{6,}|${short_sk_pattern})|sk_(live|test)_[a-z0-9]{10,}|glpat-[a-z0-9_-]{10,}|npm_[a-z0-9]{20,}|hf_[a-z0-9]{20,}|aiza[a-z0-9_-]{20,}|xox[a-z]-[a-z0-9-]{10,})([^a-z0-9_]|$)"
  local alibaba_pattern='(^|[^a-z0-9_])ltai[a-z0-9]{12,32}([^a-z0-9_]|$)'
  local aws_pattern='(^|[^a-z0-9_])(akia|asia)[a-z0-9]{16}([^a-z0-9_]|$)'
  if [[ "$lower" =~ $bearer_pattern
     || "$lower" =~ $url_password_pattern
     || "$lower" =~ $github_pattern
     || "$lower" =~ $opaque_pattern
     || "$lower" =~ $alibaba_pattern
     || "$lower" =~ $aws_pattern ]]; then
    return 0
  fi

  # Shell gating protects native history; Rust separately redacts persisted evidence.
  local canonical_assignment_key
  canonical_assignment_key='(alibaba[_-]?cloud[_-]?access[_-]?key[_-]?id|'
  canonical_assignment_key+='aws[_-]?access[_-]?key[_-]?id|access[_-]?key[_-]?id|'
  canonical_assignment_key+='aws[_-]?secret[_-]?access[_-]?key|access[_-]?key[_-]?secret|'
  canonical_assignment_key+='dashscope[_-]?api[_-]?key|openai[_-]?api[_-]?key|'
  canonical_assignment_key+='client[_-]?secret|security[_-]?token|refresh[_-]?token|'
  canonical_assignment_key+='access[_-]?token|github[_-]?token|id[_-]?token|'
  canonical_assignment_key+='password|passphrase|passwd|api[_-]?key|apikey|token|secret|'
  canonical_assignment_key+='authorization)'
  local assignment_value="['\"]?[^[:space:]]"
  # Match canonical suffixes across complete shell assignment identifiers.
  local canonical_assignment_pattern="(^|[^a-z0-9_-])['\"]?([a-z_][a-z0-9_]*)?${canonical_assignment_key}['\"]?[[:space:]]*(=|:)[[:space:]]*${assignment_value}"
  local canonical_flag_pattern="(^|[^a-z0-9_-])--${canonical_assignment_key}([[:space:]]+|=)[[:space:]]*${assignment_value}"
  local cookie_key='(cookie|set[_-]?cookie)'
  local cookie_flag_pattern="(^|[^a-z0-9_-])--${cookie_key}([[:space:]]+|=)[[:space:]]*[^[:space:]]"
  local cookie_assignment_pattern="(^|[^a-z0-9_-])['\"]?${cookie_key}['\"]?[[:space:]]*=[[:space:]]*[^[:space:]]"
  local cookie_json_pattern="(^|[^a-z0-9_-])['\"]${cookie_key}['\"][[:space:]]*:[[:space:]]*['\"][^'\"]+['\"]"
  local cookie_header_pattern="(^|[[:space:]'\"])${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  local attached_cookie_header_pattern="(^|[[:space:]])(-[[:alnum:]#]*h|--header=)${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  local dynamic_cookie_header_pattern='(\$|`)[^;&|]*'
  dynamic_cookie_header_pattern+="${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  # Operators inside a closed substitution do not terminate its outer word.
  local dynamic_expansion_cookie_header_pattern='(\$\{.*\}|\$\(.*\)|`[^`]*`)[^[:space:];&|]*'
  dynamic_expansion_cookie_header_pattern+="${cookie_key}[[:space:]]*:[[:space:]]*[^=[:space:];,]+[[:space:]]*=[^[:space:]]*"
  # Quote removal and backslash escapes can construct a static Cookie prefix
  # even when the raw command does not contain a contiguous `Cookie:` token.
  local static_cookie_input="${lower//\\/}"
  static_cookie_input="${static_cookie_input//\'/}"
  static_cookie_input="${static_cookie_input//\"/}"
  if [[ "$lower" =~ $canonical_assignment_pattern
     || "$lower" =~ $canonical_flag_pattern
     || "$lower" =~ $cookie_flag_pattern
     || "$lower" =~ $cookie_assignment_pattern
     || "$lower" =~ $cookie_json_pattern
     || "$static_cookie_input" =~ $cookie_header_pattern
     || "$static_cookie_input" =~ $attached_cookie_header_pattern
     || "$static_cookie_input" =~ $dynamic_cookie_header_pattern
     || "$static_cookie_input" =~ $dynamic_expansion_cookie_header_pattern ]]; then
    return 0
  fi
  return 1
}
_cosh_scrub_sensitive_native_history() {
  [[ -o history ]] || return 0
  local history_entry history_no command
  history_entry="$(_cosh_history_entry)"
  history_no="$(_cosh_history_no "$history_entry")"
  command="$(_cosh_history_command_from_entry "$history_entry")"
  [[ -n "$history_no" && -n "$command" ]] || return 0
  if _cosh_command_has_secret "$command"; then
    builtin history -d "$history_no" 2>/dev/null || true
  fi
}
_cosh_emit_marker() {
  local event="$1"
  local command="$2"
  local exit_status="$3"
  local path_trusted="${4:-false}"
  local restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  local timestamp
  timestamp="$(_cosh_now_ms)"
  # Optional handoff-claim fragment (#2142): only approved-handoff preexec
  # lines carry a token, every other marker stays byte-identical.
  local handoff_fragment=""
  if [[ -n "${_COSH_HANDOFF_TOKEN:-}" ]]; then
    handoff_fragment=",\"handoff\":\"$(_cosh_json_escape "$_COSH_HANDOFF_TOKEN")\""
  fi
  printf '\033]1337;COSH;{"event":"%s","token":"%s","session_id":"%s","timestamp_ms":%s,"cwd":"%s","command":"%s","status":%s,"path":"%s","path_trusted":%s,"generation":%s%s}\a' \
    "$(_cosh_json_escape "$event")" \
    "$(_cosh_json_escape "$_COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$timestamp" \
    "$(_cosh_json_escape "$PWD")" \
    "$(_cosh_json_escape "$command")" \
    "$exit_status" \
    "$(_cosh_json_escape "$PATH")" \
    "$path_trusted" \
    "${_COSH_ATTEMPT_GENERATION:-0}" \
    "$handoff_fragment"
  local marker_status=$?
  (( restore_xtrace == 1 )) && set -x
  return "$marker_status"
}
_cosh_emit_boundary_marker() {
  local exit_status="$1"
  local restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  local handoff_fragment=""
  local history_fragment=""
  local physical_cwd=""
  if [[ -n "${_COSH_HANDOFF_TOKEN:-}" ]]; then
    handoff_fragment=",\"x\":\"$(_cosh_json_escape "$_COSH_HANDOFF_TOKEN")\""
  fi
  history_fragment="$(_cosh_native_history_file_fragment)"
  # Keep path suffix newlines distinct from pwd's one record separator.
  if physical_cwd="$(builtin pwd -P 2>/dev/null && printf x)"; then
    physical_cwd="${physical_cwd%x}"
    physical_cwd="${physical_cwd%$'\n'}"
  else
    physical_cwd=""
  fi
  # The authenticated compact event is equivalent to precmd + prompt_ready;
  # omitted time and generation default in the parser. It is written directly
  # to the controlling terminal and never participates in Readline's PS1 width.
  printf '\033]1337;COSH;{"e":"p","t":"%s","c":"%s","pc":"%s","s":%s%s%s}\a' \
    "$(_cosh_json_escape "$_COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$PWD")" \
    "$(_cosh_json_escape "$physical_cwd")" \
    "$exit_status" \
    "$history_fragment" \
    "$handoff_fragment"
  local marker_status=$?
  (( restore_xtrace == 1 )) && set -x
  return "$marker_status"
}
_cosh_emit_intercept_marker() {
  local input="$1"
  local reason="$2"
  local top_level_missing="${3:-false}"
  local sensitive="${4:-false}"
  local restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  local timestamp
  timestamp="$(_cosh_now_ms)"
  printf '\033]1337;COSH;{"event":"intercept","token":"%s","session_id":"%s","timestamp_ms":%s,"cwd":"%s","command":"%s","reason":"%s","status":0,"generation":%s,"top_level_missing":%s,"sensitive":%s}\a' \
    "$(_cosh_json_escape "$_COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$timestamp" \
    "$(_cosh_json_escape "$PWD")" \
    "$(_cosh_json_escape "$input")" \
    "$(_cosh_json_escape "$reason")" \
    "${_COSH_ATTEMPT_GENERATION:-0}" \
    "$top_level_missing" \
    "$sensitive"
  local marker_status=$?
  (( restore_xtrace == 1 )) && set -x
  return "$marker_status"
}
_cosh_emit_top_level_missing_marker() {
  local intent="$1"
  local sensitive="${2:-false}"
  local unsafe="${3:-false}"
  local restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  local timestamp
  timestamp="$(_cosh_now_ms)"
  printf '\033]1337;COSH;{"event":"top_level_missing","token":"%s","session_id":"%s","timestamp_ms":%s,"cwd":"%s","generation":%s,"proven":true,"intent":"%s","sensitive":%s,"unsafe":%s}\a' \
    "$(_cosh_json_escape "$_COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$timestamp" \
    "$(_cosh_json_escape "$PWD")" \
    "${_COSH_ATTEMPT_GENERATION:-0}" \
    "$(_cosh_json_escape "$intent")" \
    "$sensitive" \
    "$unsafe"
  local marker_status=$?
  (( restore_xtrace == 1 )) && set -x
  return "$marker_status"
}
_cosh_should_intercept_unknown() {
  local command="$1"
  _cosh_assistance_enabled || return 1
  if _cosh_is_slash_control_candidate "$command"; then
    printf '%s' "slash"
    return 0
  fi
  if [[ "$command" == "??" || "$command" == "??"* ]]; then
    printf '%s' "agent_marker"
    return 0
  fi
  return 1
}
_cosh_is_slash_control_candidate() {
  local command="$1"
  case "$command" in
    /about|/agent|/allow|/answer|/approval-mode|/approve|/audit|/auth|/cancel|/clear|/config|/copy|/debug|/deny|/details|/explain|/extensions|/health|/help|/hooks|/mcp|/mode|/new|/recommendations|/resume|/select|/send-to-shell|/session|/shell|/skills|/stats|/status|/task)
      return 0
      ;;
  esac
  return 1
}
_cosh_guard_slash_submission() {
  local restore_xtrace=0
  if [[ $- == *x* ]]; then
    restore_xtrace=1
    set +x
  fi
  local input="$READLINE_LINE"
  if [[ "${1:-false}" == true && "$input" == ' '* ]]; then
    input="${input# }"
  fi
  local classification_input="$input"
  while [[ "$classification_input" == ' '* || "$classification_input" == $'\t'* ]]; do
    classification_input="${classification_input#?}"
  done
  if [[ -n "$classification_input"
     && ( "${1:-false}" == true || "$classification_input" != "$input" ) ]]; then
    _COSH_PENDING_RECOVERY_INPUT="$input"
    _COSH_PENDING_RECOVERY_CLASSIFICATION="$classification_input"
  fi
  if [[ "${1:-false}" == true ]]; then
    unset _COSH_PENDING_RECOVERY_HISTORY_INPUT 2>/dev/null || true
    rm -f -- "${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.recovery-history-confirmed" \
      2>/dev/null || true
    if [[ "${2:-false}" == true
       && -n "$classification_input" && "$classification_input" == "$input" ]]; then
      if ! _cosh_command_has_secret "$input"; then
        _COSH_PENDING_RECOVERY_HISTORY_INPUT="$input"
      fi
    fi
  fi
  local first_word="${classification_input%%[[:space:]]*}"
  if ! _cosh_assistance_enabled || ! _cosh_is_slash_control_candidate "$first_word"; then
    (( restore_xtrace == 1 )) && set -x
    return 0
  fi

  local sensitive=false
  if _cosh_command_has_secret "$input"; then
    sensitive=true
  fi
  _COSH_PENDING_SLASH_INPUT="$input"
  _COSH_PENDING_SLASH_SENSITIVE="$sensitive"
  # Arm one bounded guard-display window; marker failure remains visual-only.
  printf '\033]1337;COSH;{"e":"slash_guard","t":"%s"}\a' \
    "$_COSH_MARKER_TOKEN" > /dev/tty || true
  # The static no-op makes one replaceable native-history entry.
  READLINE_LINE='case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac'
  READLINE_POINT=${#READLINE_LINE}
  (( restore_xtrace == 1 )) && set -x
  return 0
}
_cosh_guard_private_slash_submission() {
  _cosh_guard_slash_submission true
}
_cosh_guard_recoverable_history_submission() {
  _cosh_guard_slash_submission true true
}
_cosh_commit_pending_slash() {
  if [[ -z "${_COSH_PENDING_SLASH_INPUT+x}" ]]; then
    return 0
  fi
  local input="$_COSH_PENDING_SLASH_INPUT"
  local sensitive="${_COSH_PENDING_SLASH_SENSITIVE:-false}"
  local history_entry history_no history_command
  unset _COSH_PENDING_SLASH_INPUT _COSH_PENDING_SLASH_SENSITIVE
  history_entry="$(_cosh_history_entry)"
  history_no="$(_cosh_history_no "$history_entry")"
  history_command="$(_cosh_history_command_from_entry "$history_entry")"
  if [[ "$history_command" == 'case $- in *x*) builtin set +x; builtin true __cosh_slash_guard__; builtin set -x ;; *) : ;; esac' ]]; then
    builtin history -d "$history_no" 2>/dev/null || true
  fi
  if [[ "$sensitive" != true && -o history ]]; then
    builtin history -s "$input" 2>/dev/null || true
    history_entry="$(_cosh_history_entry)"
    history_no="$(_cosh_history_no "$history_entry")"
    _cosh_remember_preexec_history_no "$history_no"
  fi
  if [[ "$history_no" =~ ^[0-9]+$ ]]; then
    _COSH_ATTEMPT_GENERATION="$history_no"
  else
    _COSH_ATTEMPT_GENERATION=$((_COSH_ATTEMPT_GENERATION + 1))
  fi
  _cosh_emit_intercept_marker "$input" "slash" false "$sensitive" > /dev/tty
}
_cosh_commit_pending_recovery_history() {
  local input="${_COSH_PENDING_RECOVERY_HISTORY_INPUT-}"
  local state_file="${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.recovery-history-confirmed"
  local confirmed=0
  [[ -f "$state_file" ]] && confirmed=1
  rm -f -- "$state_file" 2>/dev/null || true
  unset _COSH_PENDING_RECOVERY_HISTORY_INPUT 2>/dev/null || true
  [[ "$confirmed" == 1 && -n "$input" && -o history ]] || return 0
  case "$input" in
    ' '*|$'\t'*) return 0 ;;
  esac
  _cosh_command_has_secret "$input" && return 0

  local history_entry history_no history_command
  history_entry="$(_cosh_history_entry)"
  history_no="$(_cosh_history_no "$history_entry")"
  history_command="$(_cosh_history_command_from_entry "$history_entry")"
  if [[ "$history_command" != "$input" ]]; then
    builtin history -s "$input" 2>/dev/null || return 0
    history_entry="$(_cosh_history_entry)"
    history_no="$(_cosh_history_no "$history_entry")"
  fi
  _cosh_remember_preexec_history_no "$history_no"
}
_cosh_confirm_pending_recovery_history() {
  local input="$1"
  [[ -n "${_COSH_PENDING_RECOVERY_HISTORY_INPUT+x}"
     && "$_COSH_PENDING_RECOVERY_HISTORY_INPUT" == "$input" ]] || return 0
  local state_file="${COSH_RECOVERY_REQUEST_FILE:-/tmp/cosh-recovery}.recovery-history-confirmed"
  local state_dir="${state_file%/*}"
  [[ "$state_dir" != "$state_file" && -d "$state_dir" ]] || return 0
  printf '1\n' > "$state_file" 2>/dev/null || true
}
_COSH_HANDOFF_PREFIX='COSH_SHELL_HANDOFF_BYPASS=1 '
# Transport-only prefix for agent handoffs whose implicit pagers are disabled.
# Must stay byte-identical to NON_INTERACTIVE_PAGER_PREFIX in
# src/types/shell_handoff.rs, or the original command text would leak into
# markers, history and evidence.
_COSH_HANDOFF_PAGER_PREFIX='PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER=cat '
_COSH_BOUNDED_HANDOFF_COMMAND='_COSH_HANDOFF_DEBUG_TRAP="$(trap -p DEBUG 2>/dev/null)"; _COSH_HANDOFF_RETURN_TRAP="$(trap -p RETURN 2>/dev/null)"; _COSH_HANDOFF_ERR_TRAP="$(trap -p ERR 2>/dev/null)"; trap - DEBUG RETURN ERR 2>/dev/null; _cosh_prepare_staged_handoff && eval -- "$(<"$COSH_HANDOFF_REQUEST_FILE")"; _COSH_HANDOFF_STATUS=$?; eval "unset _COSH_HANDOFF_STATUS _COSH_HANDOFF_DEBUG_TRAP _COSH_HANDOFF_RETURN_TRAP _COSH_HANDOFF_ERR_TRAP ${_COSH_HANDOFF_RETURN_TRAP:+; ${_COSH_HANDOFF_RETURN_TRAP}} ${_COSH_HANDOFF_ERR_TRAP:+; ${_COSH_HANDOFF_ERR_TRAP}} ${_COSH_HANDOFF_DEBUG_TRAP:+; ${_COSH_HANDOFF_DEBUG_TRAP}}; (exit ${_COSH_HANDOFF_STATUS})"'
# Only the bypass prefix marks a transport line: handoff_pty_bytes always emits
# it first, so a line that merely starts with the pager assignments is an
# ordinary user command and must keep its full text.
_cosh_is_handoff_wrapper() {
  case "$1" in
    "$_COSH_HANDOFF_PREFIX"*)
      return 0
      ;;
  esac
  return 1
}
_cosh_unwrap_handoff_command() {
  local command="${1#$_COSH_HANDOFF_PREFIX}"
  printf '%s' "${command#$_COSH_HANDOFF_PAGER_PREFIX}"
}
_cosh_is_pending_handoff_command() {
  local command="$1"
  if [[ -z "${COSH_HANDOFF_REQUEST_FILE:-}" || ! -f "$COSH_HANDOFF_REQUEST_FILE" ]]; then
    return 1
  fi
  [[ "$(cat -- "$COSH_HANDOFF_REQUEST_FILE" 2>/dev/null)" == "$command" ]]
}
_cosh_clear_handoff_request() {
  if [[ -n "${COSH_HANDOFF_REQUEST_FILE:-}" && -f "$COSH_HANDOFF_REQUEST_FILE" ]]; then
    rm -f -- "$COSH_HANDOFF_REQUEST_FILE" 2>/dev/null || true
  fi
  if [[ -n "${COSH_HANDOFF_REQUEST_FILE:-}"
     && -f "${COSH_HANDOFF_REQUEST_FILE}.no-pager" ]]; then
    rm -f -- "${COSH_HANDOFF_REQUEST_FILE}.no-pager" 2>/dev/null || true
  fi
  if [[ -n "${COSH_HANDOFF_REQUEST_FILE:-}"
     && -f "${COSH_HANDOFF_REQUEST_FILE}.token" ]]; then
    rm -f -- "${COSH_HANDOFF_REQUEST_FILE}.token" 2>/dev/null || true
  fi
}
# One-time claim token for the approved handoff (#2142). Staged by the Rust
# transport next to the request file; carried back on the preexec/precmd
# markers so the parser can claim the command block even when the reported
# command text is redacted. Missing sidecar leaves the token empty, which
# keeps the marker JSON byte-identical to the pre-token format.
_cosh_load_handoff_token() {
  _COSH_HANDOFF_TOKEN=""
  if [[ -n "${COSH_HANDOFF_REQUEST_FILE:-}"
     && -f "${COSH_HANDOFF_REQUEST_FILE}.token" ]]; then
    _COSH_HANDOFF_TOKEN="$(cat -- "${COSH_HANDOFF_REQUEST_FILE}.token" 2>/dev/null)" || _COSH_HANDOFF_TOKEN=""
  fi
}
# Implicit-pager policy for one approved handoff. The sidecar file is written by
# the Rust transport before the command reaches the shell; the variable set must
# stay identical to NON_INTERACTIVE_PAGER_PREFIX in src/types/shell_handoff.rs.
# Scope is a single command: preexec applies it, precmd restores it, so the
# user's own commands keep their own pager configuration.
# Classifies both value visibility and readonly state. An exported readonly
# pager cannot be assigned, but its export attribute can be removed long enough
# to keep the inherited value out of the handoff command's environment.
_cosh_pager_var_state() {
  local name="$1" dump
  if [[ -z "${!name+x}" ]]; then
    printf unset
    return 0
  fi
  # One subshell per variable, and only on approved-handoff lines: the handoff
  # branch of the preexec marker already forks for _cosh_unwrap_handoff_command.
  dump="$(declare -p "$name" 2>/dev/null)"
  case "$dump" in
    "declare -"*r*" $name="*)
      case "$dump" in
        "declare -"*x*" $name="*)
          printf readonly_export
          ;;
        *)
          printf readonly_shell
          ;;
      esac
      ;;
    "declare -"*x*" $name="*)
      printf export
      ;;
    *)
      printf shell
      ;;
  esac
}
_cosh_apply_handoff_pager_policy() {
  if [[ -z "${COSH_HANDOFF_REQUEST_FILE:-}"
     || ! -f "${COSH_HANDOFF_REQUEST_FILE}.no-pager" ]]; then
    return 0
  fi
  local name state
  for name in PAGER GIT_PAGER MANPAGER SYSTEMD_PAGER; do
    state="$(_cosh_pager_var_state "$name")"
    printf -v "_COSH_${name}_STATE" '%s' "$state"
    printf -v "_COSH_${name}_SAVED" '%s' "${!name-}"
    case "$state" in
      readonly_export)
        export -n "$name"
        ;;
      readonly_shell)
        ;;
      *)
        export "$name=cat"
        ;;
    esac
  done
  _COSH_HANDOFF_PAGER_APPLIED=1
  return 0
}
# Undoes an injection only while it is still exactly what cosh left behind: an
# exported scalar holding `cat`. A handoff command that changed the value
# (export PAGER=less), removed it (unset GIT_PAGER) or only dropped the export
# attribute (export -n PAGER) keeps its own result, because reverting it would
# report success while silently discarding the effect.
_cosh_restore_one_pager_var() {
  local name="$1"
  local state_var="_COSH_${name}_STATE" saved_var="_COSH_${name}_SAVED"
  case "${!state_var-unset}" in
    readonly_export)
      if [[ "${!name-}" == "${!saved_var-}"
         && "$(_cosh_pager_var_state "$name")" == readonly_shell ]]; then
        export "$name"
      fi
      return 0
      ;;
    readonly_shell)
      return 0
      ;;
  esac
  if [[ "${!name-}" != cat
     || "$(_cosh_pager_var_state "$name")" != export ]]; then
    return 0
  fi
  unset "$name"
  case "${!state_var-unset}" in
    shell)
      printf -v "$name" '%s' "${!saved_var-}"
      ;;
    export)
      printf -v "$name" '%s' "${!saved_var-}"
      export "$name"
      ;;
  esac
  return 0
}
_cosh_restore_handoff_pager_policy() {
  if [[ "${_COSH_HANDOFF_PAGER_APPLIED:-0}" != 1 ]]; then
    return 0
  fi
  unset _COSH_HANDOFF_PAGER_APPLIED 2>/dev/null || true
  local name
  for name in PAGER GIT_PAGER MANPAGER SYSTEMD_PAGER; do
    _cosh_restore_one_pager_var "$name"
    unset "_COSH_${name}_STATE" "_COSH_${name}_SAVED" 2>/dev/null || true
  done
  return 0
}
_cosh_replace_handoff_history() {
  if [[ -z "${_COSH_HANDOFF_HISTORY_NO:-}" || -z "${_COSH_HANDOFF_HISTORY_COMMAND+x}" ]]; then
    return 0
  fi
  builtin history -d "$_COSH_HANDOFF_HISTORY_NO" 2>/dev/null || true
  builtin history -s "$_COSH_HANDOFF_HISTORY_COMMAND" 2>/dev/null || true
  unset _COSH_HANDOFF_HISTORY_NO _COSH_HANDOFF_HISTORY_COMMAND 2>/dev/null || true
}
_cosh_prepare_staged_handoff() {
  trap - DEBUG RETURN ERR 2>/dev/null || true
  local command history_entry history_no history_command display_command
  if [[ -z "${COSH_HANDOFF_REQUEST_FILE:-}" || ! -r "$COSH_HANDOFF_REQUEST_FILE" ]]; then
    return 127
  fi
  command="$(cat -- "$COSH_HANDOFF_REQUEST_FILE" 2>/dev/null)" || return 127
  [[ -n "$command" ]] || return 127

  _COSH_HANDOFF_TOKEN=""
  _COSH_HANDOFF_ACTIVE=1
  _cosh_load_handoff_token
  _cosh_apply_handoff_pager_policy
  history_entry="$(_cosh_history_entry)"
  history_no="$(_cosh_history_no "$history_entry")"
  history_command="$(_cosh_history_command_from_entry "$history_entry")"
  if [[ "$history_command" == "$_COSH_BOUNDED_HANDOFF_COMMAND" ]]; then
    builtin history -d "$history_no" 2>/dev/null || true
  fi
  display_command="$command"
  if _cosh_command_has_secret "$command"; then
    display_command="<redacted sensitive command>"
  elif [[ -o history ]]; then
    builtin history -s "$command" 2>/dev/null || true
    history_entry="$(_cosh_history_entry)"
    _cosh_remember_preexec_history_no "$(_cosh_history_no "$history_entry")"
  fi
  _COSH_ATTEMPT_GENERATION=$((_COSH_ATTEMPT_GENERATION + 1))
  _cosh_emit_marker "preexec" "$display_command" 0 false
  return 0
}
