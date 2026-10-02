#!/bin/bash
# Asserts the persistent cosh-core initialize handshake carries both the
# protocol version and the control capability pair (#2156), then completes a
# trivial turn.
read -r init
if echo "$init" | grep -q '"subtype":"initialize"' \
  && echo "$init" | grep -q '"protocol_version"' \
  && echo "$init" | grep -q '"can_handle_can_use_tool":true' \
  && echo "$init" | grep -q '"can_handle_host_executed_shell":true'; then
  echo '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
  echo '{"type":"system","subtype":"init","model":"mock-cosh-core","session_id":"mock-cosh-core-init-caps"}'
else
  echo '{"type":"result","subtype":"error","session_id":"mock-cosh-core-init-caps","is_error":true,"result":"initialize missing protocol_version or capabilities"}'
  exit 1
fi
read -r line
echo '{"type":"assistant","session_id":"mock-cosh-core-init-caps","message":{"content":[{"type":"text","text":"init handshake accepted"}]}}'
echo '{"type":"result","subtype":"success","session_id":"mock-cosh-core-init-caps","is_error":false,"result":"init ok"}'
