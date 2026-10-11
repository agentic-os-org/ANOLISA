#!/bin/sh
# Core stand-in reachable only through the login profile's PATH.
if [ "$1" = "--registry" ]; then
  read -r request
  printf '%s\n' '{"type":"registry_response","request_id":"reg","success":true,"data":{"configured":true}}'
  exit 0
fi
printf '%s\n' "$PATH" > "$HOME/core-path"
command -v cosh-li-profile-tool > "$HOME/core-child" 2>/dev/null || :
read -r init
printf '%s\n' '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
printf '%s\n' '{"type":"system","subtype":"init","session_id":"startup-path","model":"mock","tools":[]}'
read -r line
printf '%s\n' '{"type":"assistant","session_id":"startup-path","message":{"content":[{"type":"text","text":"STARTUP_PATH_CORE_DONE"}]}}'
printf '%s\n' '{"type":"result","subtype":"success","session_id":"startup-path","is_error":false,"result":"done"}'
