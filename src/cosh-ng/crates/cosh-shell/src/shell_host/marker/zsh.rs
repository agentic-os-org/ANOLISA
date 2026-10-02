// Owner: shell_host (zsh marker script). Emitted protocol must stay
// byte-identical to the pre-split marker.rs; golden coverage lives in
// osc_tests.rs and tests/shell_host/marker.rs.
pub(in crate::shell_host) fn zsh_marker_script() -> &'static str {
    concat!(
        r#"
if [[ -n "${COSH_OSC_MARKER_LOADED:-}" ]]; then
  return 0 2>/dev/null || exit 0
fi
COSH_OSC_MARKER_LOADED=1
[[ -o interactive ]] || return 0 2>/dev/null || exit 0
export COSH_SESSION_ID="${COSH_SESSION_ID:-cosh-osc-$$}"
export COSH_POC_PS1="${COSH_POC_PS1:-cosh-osc$ }"
_COSH_INITIAL_COMMAND_NOT_FOUND_HANDLER="${functions[command_not_found_handler]-}"
if [[ -z "${COSH_SHELL_ISOLATED:-}" ]]; then
  if [[ -n "${COSH_ZDOTDIR_ORIG:-}" ]]; then
    _cosh_marker_zdotdir="${ZDOTDIR:-}"
    if [[ -n "$_cosh_marker_zdotdir" && "${HISTFILE:-}" == "$_cosh_marker_zdotdir/.zsh_history" ]]; then
      HISTFILE="${COSH_ZDOTDIR_ORIG}/.zsh_history"
    fi
    export ZDOTDIR="${COSH_ZDOTDIR_ORIG}"
    [[ -f "${COSH_ZDOTDIR_ORIG}/.zshenv" ]] && source "${COSH_ZDOTDIR_ORIG}/.zshenv"
    if [[ "${COSH_LOGIN_SHELL:-}" == "1" ]]; then
      [[ -f "${COSH_ZDOTDIR_ORIG}/.zprofile" ]] && source "${COSH_ZDOTDIR_ORIG}/.zprofile"
      [[ -f "${COSH_ZDOTDIR_ORIG}/.zlogin" ]] && source "${COSH_ZDOTDIR_ORIG}/.zlogin"
    fi
    [[ -f "${COSH_ZDOTDIR_ORIG}/.zshrc" ]] && source "${COSH_ZDOTDIR_ORIG}/.zshrc"
    unset _cosh_marker_zdotdir
  else
    [[ -f ~/.zshenv ]] && source ~/.zshenv
    if [[ "${COSH_LOGIN_SHELL:-}" == "1" ]]; then
      [[ -f ~/.zprofile ]] && source ~/.zprofile
      [[ -f ~/.zlogin ]] && source ~/.zlogin
    fi
    [[ -f ~/.zshrc ]] && source ~/.zshrc
  fi
fi
_COSH_AI_ENABLED="$_COSH_SESSION_AI_ENABLED"
readonly _COSH_AI_ENABLED
_cosh_assistance_enabled() {
  setopt localoptions noxtrace
  [[ -n "${COSH_ASSISTANCE_STATE_FILE:-}"
     && -f "$COSH_ASSISTANCE_STATE_FILE" ]]
}
_cosh_ai_enabled() {
  [[ "${_COSH_AI_ENABLED:-1}" == 1 ]] && _cosh_assistance_enabled
}
_cosh_load_native_zsh_history_if_empty() {
  if [[ -n "${COSH_SHELL_ISOLATED:-}" ]]; then
    return 0
  fi
  if [[ -z "${HISTFILE:-}" && -n "${COSH_ZDOTDIR_ORIG:-}" && -r "${COSH_ZDOTDIR_ORIG}/.zsh_history" ]]; then
    HISTFILE="${COSH_ZDOTDIR_ORIG}/.zsh_history"
  fi
  if [[ -z "${HISTFILE:-}" || ! -r "$HISTFILE" ]]; then
    return 0
  fi
  if fc -l 1 >/dev/null 2>&1; then
    return 0
  fi
  fc -R "$HISTFILE" 2>/dev/null || true
}
if [[ -z "${COSH_SHELL_ISOLATED:-}" ]]; then
  : # native mode: keep user PS1/PROMPT, HISTFILE, etc.
else
  export PS1="$COSH_POC_PS1"
  export PROMPT="$COSH_POC_PS1"
  export HISTFILE="${COSH_HISTFILE:-/dev/null}"
  HISTSIZE="${COSH_HISTSIZE:-1000}"
  SAVEHIST=0
fi
_cosh_load_native_zsh_history_if_empty
setopt NO_BEEP 2>/dev/null || true
setopt NO_PROMPT_CR 2>/dev/null || true
setopt NO_PROMPT_SP 2>/dev/null || true
unsetopt NOMATCH 2>/dev/null || true
_COSH_ATTEMPT_GENERATION=0
_COSH_ATTEMPT_ACTIVE=0
_COSH_ATTEMPT_INPUT=
_COSH_ATTEMPT_TOKEN=
_COSH_ATTEMPT_TOKEN_FINGERPRINT=
_COSH_ATTEMPT_SENSITIVE=0
_COSH_ATTEMPT_UNSAFE=0
_COSH_ATTEMPT_EXPANSION_DRIFT=0
_COSH_ATTEMPT_SUBSHELL=
_COSH_WRAPPER_ID="${COSH_SESSION_ID}:${COSH_MARKER_TOKEN}"
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
_cosh_now_ms() {
  date +%s000
}
_cosh_shell_path_names_fragment() { # Line-init can mutate the namespace after this snapshot.
  local -a names suffixes quoted_names quoted_suffixes; local name total=0
  if [[ -n "${widgets[zle-line-init]:-}" ]]; then REPLY=',"shell_path_names":null,"shell_path_suffixes":null'; return; fi
  for name in ${(k)functions} ${(k)aliases} ${(k)galiases}; do
    [[ "$name" == */* ]] && names+=("$name")
  done
  suffixes=("${(k)saliases}"); if (( ${#names} > 128 || ${#suffixes} > 128 )); then REPLY=',"shell_path_names":null,"shell_path_suffixes":null'; return; fi
  for name in "${names[@]}"; do
    if [[ "$name" == *[[:cntrl:]]* || "$name" == *'"'* || "$name" == *\\* ]] || (( (total += ${#name} + 3) > 8192 )); then
      REPLY=',"shell_path_names":null,"shell_path_suffixes":null'; return
    fi
    quoted_names+=("\"$name\"")
  done
  for name in "${suffixes[@]}"; do
    if [[ "$name" == *[[:cntrl:]]* || "$name" == *'"'* || "$name" == *\\* ]] || (( (total += ${#name} + 3) > 8192 )); then
      REPLY=',"shell_path_names":null,"shell_path_suffixes":null'; return
    fi
    quoted_suffixes+=("\"$name\"")
  done
  REPLY=",\"shell_path_names\":[${(j:,:)quoted_names}],\"shell_path_suffixes\":[${(j:,:)quoted_suffixes}]"
}
_cosh_emit_marker() {
  local event="$1"
  local command="$2"
  local exit_status="$3"
  local path_trusted="${4:-false}"
  local timestamp
  timestamp="$(_cosh_now_ms)"
  # Optional handoff-claim fragment (#2142): only approved-handoff preexec
  # lines carry a token, every other marker stays byte-identical.
  local handoff_fragment=""
  local physical_cwd_fragment=""
  local shell_path_names_fragment=""
  local REPLY=""
  if [[ -n "${_COSH_HANDOFF_TOKEN:-}" ]]; then
    handoff_fragment=",\"handoff\":\"$(_cosh_json_escape "$_COSH_HANDOFF_TOKEN")\""
  fi
  if [[ "$event" == "prompt_ready" ]]; then
    local physical_cwd=""
    # Keep path suffix newlines distinct from pwd's one record separator.
    if physical_cwd="$(builtin pwd -P 2>/dev/null && printf x)"; then
      physical_cwd="${physical_cwd%x}"
      physical_cwd="${physical_cwd%$'\n'}"
    else
      physical_cwd=""
    fi
    if [[ "$physical_cwd" == /* ]]; then
      physical_cwd_fragment=",\"physical_cwd\":\"$(_cosh_json_escape "$physical_cwd")\""
    fi
    _cosh_shell_path_names_fragment
    shell_path_names_fragment="$REPLY"
  fi
  printf '\033]1337;COSH;{"event":"%s","token":"%s","session_id":"%s","timestamp_ms":%s,"cwd":"%s","command":"%s","status":%s,"path":"%s","path_trusted":%s,"generation":%s%s%s%s}\a' \
    "$(_cosh_json_escape "$event")" \
    "$(_cosh_json_escape "$COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$timestamp" \
    "$(_cosh_json_escape "$PWD")" \
    "$(_cosh_json_escape "$command")" \
    "$exit_status" \
    "$(_cosh_json_escape "$PATH")" \
    "$path_trusted" \
    "${_COSH_ATTEMPT_GENERATION:-0}" \
    "$physical_cwd_fragment" \
    "$shell_path_names_fragment" \
    "$handoff_fragment"
}
_cosh_emit_intercept_marker() {
  local input="$1"
  local reason="$2"
  local top_level_missing="${3:-false}"
  local sensitive="${4:-false}"
  local timestamp
  timestamp="$(_cosh_now_ms)"
  printf '\033]1337;COSH;{"event":"intercept","token":"%s","session_id":"%s","timestamp_ms":%s,"cwd":"%s","command":"%s","reason":"%s","status":0,"generation":%s,"top_level_missing":%s,"sensitive":%s}\a' \
    "$(_cosh_json_escape "$COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$timestamp" \
    "$(_cosh_json_escape "$PWD")" \
    "$(_cosh_json_escape "$input")" \
    "$(_cosh_json_escape "$reason")" \
    "${_COSH_ATTEMPT_GENERATION:-0}" \
    "$top_level_missing" \
    "$sensitive"
}
_cosh_emit_top_level_missing_marker() {
  local intent="$1"
  local sensitive="${2:-false}"
  local unsafe="${3:-false}"
  local timestamp
  timestamp="$(_cosh_now_ms)"
  printf '\033]1337;COSH;{"event":"top_level_missing","token":"%s","session_id":"%s","timestamp_ms":%s,"cwd":"%s","generation":%s,"proven":true,"intent":"%s","sensitive":%s,"unsafe":%s}\a' \
    "$(_cosh_json_escape "$COSH_MARKER_TOKEN")" \
    "$(_cosh_json_escape "$COSH_SESSION_ID")" \
    "$timestamp" \
    "$(_cosh_json_escape "$PWD")" \
    "${_COSH_ATTEMPT_GENERATION:-0}" \
    "$(_cosh_json_escape "$intent")" \
    "$sensitive" \
    "$unsafe"
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
_COSH_HANDOFF_PREFIX='COSH_SHELL_HANDOFF_BYPASS=1 '
# Transport-only prefix for agent handoffs whose implicit pagers are disabled.
# Must stay byte-identical to NON_INTERACTIVE_PAGER_PREFIX in
# src/types/shell_handoff.rs, or the original command text would leak into
# markers, history and evidence.
_COSH_HANDOFF_PAGER_PREFIX='PAGER=cat GIT_PAGER=cat MANPAGER=cat SYSTEMD_PAGER=cat '
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
  local name="$1"
  local kind="${(Pt)name}"
  if [[ -z "$kind" ]]; then
    printf unset
    return 0
  fi
  if [[ "$kind" == *readonly* ]]; then
    if [[ "$kind" == *export* ]]; then
      printf readonly_export
    else
      printf readonly_shell
    fi
    return 0
  fi
  if [[ "$kind" == *export* ]]; then
    printf export
    return 0
  fi
  printf shell
}
_cosh_apply_handoff_pager_policy() {
  if [[ -z "${COSH_HANDOFF_REQUEST_FILE:-}"
     || ! -f "${COSH_HANDOFF_REQUEST_FILE}.no-pager" ]]; then
    return 0
  fi
  local name state
  for name in PAGER GIT_PAGER MANPAGER SYSTEMD_PAGER; do
    state="$(_cosh_pager_var_state "$name")"
    typeset -g "_COSH_${name}_STATE=$state"
    typeset -g "_COSH_${name}_SAVED=${(P)name}"
    case "$state" in
      readonly_export)
        typeset +x "$name"
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
# attribute (typeset +x PAGER) keeps its own result, because reverting it would
# report success while silently discarding the effect.
_cosh_restore_one_pager_var() {
  local name="$1"
  local state_var="_COSH_${name}_STATE" saved_var="_COSH_${name}_SAVED"
  case "${(P)state_var}" in
    readonly_export)
      if [[ "${(P)name}" == "${(P)saved_var}"
         && "$(_cosh_pager_var_state "$name")" == readonly_shell ]]; then
        export "$name"
      fi
      return 0
      ;;
    readonly_shell)
      return 0
      ;;
  esac
  if [[ "${(P)name}" != cat
     || "$(_cosh_pager_var_state "$name")" != export ]]; then
    return 0
  fi
  unset "$name"
  case "${(P)state_var}" in
    shell)
      typeset -g "$name=${(P)saved_var}"
      ;;
    export)
      typeset -gx "$name=${(P)saved_var}"
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
"#,
        include_str!("zsh_secret_detector.zsh"),
        r#"_cosh_zshaddhistory_marker() {
  setopt localoptions noxtrace
  local command="${1%$'\n'}"
  if _cosh_is_handoff_wrapper "$command"; then
    local history_command="$(_cosh_unwrap_handoff_command "$command")"
    if _cosh_command_has_secret "$history_command"; then
      history_command="<redacted sensitive command>"
    fi
    _COSH_HANDOFF_HISTORY_COMMAND="$history_command"
    return 1
  fi
  if _cosh_command_has_secret "$command"; then
    return 1
  fi
  _cosh_utf8_han_status "$command"
  (( $? == 2 )) && return 1
  return 0
}
_cosh_add_handoff_history() {
  if [[ -z "${_COSH_HANDOFF_HISTORY_COMMAND+x}" ]]; then
    return 0
  fi
  print -sr -- "$_COSH_HANDOFF_HISTORY_COMMAND" 2>/dev/null || true
  unset _COSH_HANDOFF_HISTORY_COMMAND 2>/dev/null || true
}
_cosh_begin_attempt() {
  local input="$1"
  local top_token="$2"
  local expansion_drift="${3:-0}"
  local utf8_status
  _COSH_ATTEMPT_GENERATION=$((_COSH_ATTEMPT_GENERATION + 1))
  _COSH_ATTEMPT_ACTIVE=1
  _COSH_ATTEMPT_WRAPPER_ID="$_COSH_WRAPPER_ID"
  _COSH_ATTEMPT_SENSITIVE=0
  _COSH_ATTEMPT_UNSAFE=0
  _COSH_ATTEMPT_EXPANSION_DRIFT="$expansion_drift"
  _COSH_ATTEMPT_SUBSHELL="${ZSH_SUBSHELL:-0}"
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
_cosh_delegate_zsh_command_not_found() {
  if [[ "${_COSH_IN_USER_COMMAND_NOT_FOUND:-0}" == 1 ]]; then
    printf 'zsh: command not found: %s\n' "$1" >&2
    return 127
  fi
  if [[ "${_COSH_HAS_USER_COMMAND_NOT_FOUND:-0}" == 1 ]]; then
    _COSH_IN_USER_COMMAND_NOT_FOUND=1
    if [[ "${_COSH_COMMAND_NOT_FOUND_XTRACE:-off}" == on ]]; then
      setopt xtrace
    fi
    _cosh_user_command_not_found_handler "$@"
    local result=$?
    local final_xtrace="${options[xtrace]}"
    unsetopt xtrace
    _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE="$final_xtrace"
    _COSH_IN_USER_COMMAND_NOT_FOUND=0
    return "$result"
  fi
  printf 'zsh: command not found: %s\n' "$1" >&2
  return 127
}
_cosh_user_handler_definition="${functions[command_not_found_handler]-}"
if [[ -n "$_cosh_user_handler_definition"
   && "$_cosh_user_handler_definition" != "$_COSH_INITIAL_COMMAND_NOT_FOUND_HANDLER" ]]; then
  functions[_cosh_user_command_not_found_handler]="$_cosh_user_handler_definition"
  _COSH_HAS_USER_COMMAND_NOT_FOUND=1
else
  _COSH_HAS_USER_COMMAND_NOT_FOUND=0
fi
unset _cosh_user_handler_definition _COSH_INITIAL_COMMAND_NOT_FOUND_HANDLER
_cosh_command_not_found_handler() {
  local command="$1"
  shift || true
  local original="${_COSH_ATTEMPT_INPUT:-}"
  if [[ "${_COSH_HANDOFF_ACTIVE:-0}" == 1 ]]; then
    _cosh_delegate_zsh_command_not_found "$command" "$@"
    return $?
  fi
  if [[ "${_COSH_ATTEMPT_ACTIVE:-0}" != 1
     || "${_COSH_ATTEMPT_WRAPPER_ID:-}" != "$_COSH_WRAPPER_ID" ]]; then
    _cosh_delegate_zsh_command_not_found "$command" "$@"
    return $?
  fi
  if (( ${ZSH_SUBSHELL:-0} != ${_COSH_ATTEMPT_SUBSHELL:-0} + 1 )) \
     || (( ${#funcstack[@]} != 2 )) \
     || [[ "${_COSH_ATTEMPT_EXPANSION_DRIFT:-0}" == 1 ]]; then
    _cosh_delegate_zsh_command_not_found "$command" "$@"
    return $?
  fi
  if [[ "${_COSH_ATTEMPT_UNSAFE:-0}" == 1 ]]; then
    local command_fingerprint
    command_fingerprint="$(_cosh_token_fingerprint "$command")"
    if [[ -z "$command_fingerprint"
       || "$command_fingerprint" != "${_COSH_ATTEMPT_TOKEN_FINGERPRINT:-}" ]]; then
      _cosh_delegate_zsh_command_not_found "$command" "$@"
      return $?
    fi
    _COSH_ATTEMPT_ACTIVE=0
    local sensitive=false
    [[ "${_COSH_ATTEMPT_SENSITIVE:-0}" == 1 ]] && sensitive=true
    _cosh_emit_top_level_missing_marker "ambiguous" "$sensitive" true
    _cosh_delegate_zsh_command_not_found "$command" "$@"
    return $?
  fi
  if [[ -z "$original" ]] \
     || ! _cosh_literal_first_word_matches "$original" "${_COSH_ATTEMPT_TOKEN:-}" "$command" \
     || ! _cosh_arguments_have_no_unquoted_expansion "$original"; then
    _cosh_delegate_zsh_command_not_found "$command" "$@"
    return $?
  fi
  if _cosh_is_pending_handoff_command "$original"; then
    _cosh_delegate_zsh_command_not_found "$command" "$@"
    return $?
  fi
  _COSH_ATTEMPT_ACTIVE=0
  local sensitive=false
  [[ "${_COSH_ATTEMPT_SENSITIVE:-0}" == 1 ]] && sensitive=true
  local reason
  if reason="$(_cosh_should_intercept_unknown "$command" "$original" "$(($# + 1))")"; then
    _cosh_emit_intercept_marker "$original" "$reason" false "$sensitive"
    return 0
  fi
  local intent
  intent="$(_cosh_classify_missing "$original" "$command")"
  if [[ "$intent" == "natural_language" ]] && _cosh_ai_enabled; then
    if [[ "${_COSH_HAS_USER_COMMAND_NOT_FOUND:-0}" == 1 ]]; then
      _cosh_emit_top_level_missing_marker "$intent" "$sensitive" false
      _cosh_delegate_zsh_command_not_found "$command" "$@"
      return $?
    fi
    _cosh_emit_intercept_marker "$original" "natural_language" true "$sensitive"
    return 0
  fi
  _cosh_emit_top_level_missing_marker "$intent" "$sensitive" false
  _cosh_delegate_zsh_command_not_found "$command" "$@"
  return $?
}
command_not_found_handler() {
  local _COSH_COMMAND_NOT_FOUND_XTRACE="${options[xtrace]}"
  # zsh restores XTRACE at every function return even without LOCAL_OPTIONS;
  # the observed post-handler state therefore already matches native zsh.
  local _COSH_COMMAND_NOT_FOUND_FINAL_XTRACE="$_COSH_COMMAND_NOT_FOUND_XTRACE"
  unsetopt xtrace
  _cosh_command_not_found_handler "$@"
  local result=$?
  if [[ "$_COSH_COMMAND_NOT_FOUND_FINAL_XTRACE" == on ]]; then
    setopt xtrace
  else
    unsetopt xtrace
  fi
  return "$result"
}
_cosh_preexec_marker() {
  setopt localoptions noxtrace
  local command="$1"
  # zsh passes the abbreviated, size-limited form in $2 and the full text of
  # the command being executed in $3. Long inputs truncate $2, so comparing
  # against it produces a false expansion-drift signal that misroutes long
  # natural-language prompts to the native command-not-found path (#2053).
  # Prefer $3; keep $2/$1 as defensive fallbacks for hosts that omit it.
  local expanded_command="${3:-${2:-$1}}"
  local canonical_command="${(j: :)${(z)command}}"
  local expansion_drift=0
  [[ "$canonical_command" != "$expanded_command" ]] && expansion_drift=1
  _COSH_ATTEMPT_ACTIVE=0
  _COSH_ATTEMPT_SENSITIVE=0
  _COSH_ATTEMPT_UNSAFE=0
  local display_command="$command"
  local path_trusted=false
  if [[ "${preexec_functions[-1]:-}" == "_cosh_preexec_marker" ]]; then
    path_trusted=true
  fi
  if _cosh_is_handoff_wrapper "$command"; then
    display_command="$(_cosh_unwrap_handoff_command "$command")"
    # Handoff treatment (active flag, pager policy, token) applies only
    # when the unwrapped text matches the staged request: a user-typed
    # bypass-prefixed line racing ahead must not steal the claim, and its
    # precmd must not see the active flag and clear the staged sidecars
    # the real handoff line is about to consume (#2142 review).
    if _cosh_is_pending_handoff_command "$display_command"; then
      _COSH_HANDOFF_ACTIVE=1
      _cosh_apply_handoff_pager_policy
      _cosh_load_handoff_token
      _cosh_clear_handoff_request
    fi
    if _cosh_command_has_secret "$display_command"; then
      display_command="<redacted sensitive command>"
    fi
    _COSH_HANDOFF_HISTORY_COMMAND="$display_command"
  elif _cosh_is_pending_handoff_command "$command"; then
    _COSH_HANDOFF_ACTIVE=1
    _cosh_load_handoff_token
    _cosh_apply_handoff_pager_policy
    # Consume-then-clear: the claim is single-shot, and clearing here
    # (not in unrelated branches) is what keeps it alive across
    # command-ahead races.
    _cosh_clear_handoff_request
  else
    # Deliberately no _cosh_clear_handoff_request here: an unrelated
    # command racing ahead of an approved handoff must leave the staged
    # request/token sidecars for the handoff line that follows; the Rust
    # transport owns cleanup for abandoned handoffs (#2142 review).
    unset _COSH_HANDOFF_ACTIVE 2>/dev/null || true
    unset _COSH_HANDOFF_TOKEN 2>/dev/null || true
    unset _COSH_HANDOFF_HISTORY_COMMAND 2>/dev/null || true
    local command_word_source="$command"
    while [[ "$command_word_source" == ' '* || "$command_word_source" == $'\t'* ]]; do
      command_word_source="${command_word_source#?}"
    done
    local first_word="$command_word_source"
    local argc=1
    if [[ "$command_word_source" == *[[:space:]]* ]]; then
      first_word="${command_word_source%%[[:space:]]*}"
      argc=2
    fi
    local reason
    if reason="$(_cosh_should_intercept_unknown "$first_word" "$command" "$argc")"; then
      local intercept_sensitive=false
      _cosh_command_has_secret "$command" && intercept_sensitive=true
      _cosh_emit_intercept_marker "$command" "$reason" false "$intercept_sensitive"
      return 1
    fi
    _cosh_begin_attempt "$command" "$first_word" "$expansion_drift"
  fi
  if [[ "${_COSH_ATTEMPT_SENSITIVE:-0}" == 1
     || "${_COSH_ATTEMPT_UNSAFE:-0}" == 1 ]] \
     || _cosh_command_has_secret "$display_command"; then
    display_command="<redacted sensitive command>"
  fi
  _cosh_emit_marker "preexec" "$display_command" 0 "$path_trusted"
}
_cosh_precmd_marker() {
  local exit_status=$?
  setopt localoptions noxtrace
  setopt NO_PROMPT_CR 2>/dev/null || true
  setopt NO_PROMPT_SP 2>/dev/null || true
  _cosh_apply_internal_recovery
  _cosh_add_handoff_history
  # Only the handoff's own prompt boundary may clear the staged files: an
  # unrelated command finishing while a handoff is still pending must not
  # destroy the request/token sidecars it is about to consume (#2142 review).
  if [[ "${_COSH_HANDOFF_ACTIVE:-0}" == 1 ]]; then
    _cosh_clear_handoff_request
  fi
  _cosh_restore_handoff_pager_policy
  unset _COSH_HANDOFF_ACTIVE 2>/dev/null || true
  _COSH_ATTEMPT_ACTIVE=0
  # The precmd marker still carries the handoff token (#2142): it closes the
  # same command the preexec claimed. Cleared right after so the following
  # prompt_ready and ordinary markers stay token-free.
  _cosh_emit_marker "precmd" "" "$exit_status" false
  unset _COSH_HANDOFF_TOKEN 2>/dev/null || true
  # Only claim prompt readiness while this remains the final precmd hook.
  # Hooks appended later may still emit output or block before zsh paints.
  if [[ "${precmd_functions[-1]:-}" == "_cosh_precmd_marker" ]]; then
    _cosh_emit_marker "prompt_ready" "" "$exit_status" false
  fi
}
for _cosh_sc in about agent allow answer approval-mode approve audit auth cancel clear config copy debug deny details explain extensions health help hooks mcp mode new recommendations resume select send-to-shell session shell skills stats status task; do
  functions[/$_cosh_sc]='_cosh_assistance_enabled || command "$0" "$@"'
done
unset _cosh_sc
autoload -Uz add-zsh-hook
add-zsh-hook zshaddhistory _cosh_zshaddhistory_marker
add-zsh-hook preexec _cosh_preexec_marker
add-zsh-hook precmd _cosh_precmd_marker
"#,
    )
}
