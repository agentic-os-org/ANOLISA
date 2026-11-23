# Shell variables cannot represent NUL; escape every representable C0 byte and DEL.
_cosh_json_escape_var() {
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
  _COSH_JSON_ESCAPE_RESULT="$value"
}
_cosh_json_escape() {
  _cosh_json_escape_var "$1"
  printf '%s' "$_COSH_JSON_ESCAPE_RESULT"
}
_cosh_memory_overage() {
  local dir="${_COSH_CGROUP_DIR-}"
  if [[ -z "$dir" ]]; then
    local line=""
    IFS= read -r line < /proc/self/cgroup 2>/dev/null || true
    local rel="${line#0::}"
    if [[ "$rel" == /* && "$rel" != "/" && -r "/sys/fs/cgroup${rel}/memory.high" ]]; then
      dir="/sys/fs/cgroup${rel}"
    else
      dir="none"
    fi
    typeset -g _COSH_CGROUP_DIR="$dir"
  fi
  [[ "$dir" != none ]] || return 1
  local current="" high=""
  IFS= read -r current < "$dir/memory.current" 2>/dev/null || return 1
  IFS= read -r high < "$dir/memory.high" 2>/dev/null || return 1
  [[ "$high" != max ]] || return 1
  (( current > high ))
}
_cosh_emit_throttled_precmd() {
  local exit_status="${1:-0}"
  _cosh_json_escape_var "${_COSH_MARKER_TOKEN}"
  local token="$_COSH_JSON_ESCAPE_RESULT"
  _cosh_json_escape_var "$PWD"
  local cwd="$_COSH_JSON_ESCAPE_RESULT"
  printf '\033]1337;COSH;{"e":"p","t":"%s","c":"%s","pc":"%s","s":%s}\a' \
    "$token" "$cwd" "$cwd" "$exit_status"
}
