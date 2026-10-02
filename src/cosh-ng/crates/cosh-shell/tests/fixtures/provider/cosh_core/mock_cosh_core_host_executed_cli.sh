#!/bin/bash
read -r line
echo '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
echo '{"type":"system","subtype":"init","model":"mock-cosh-core","session_id":"mock-cosh-core-host-executed"}'
read -r line
echo '{"type":"control_request","request_id":"cosh-core-req-001","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"call_cosh_core_001"}}'
read -r line
if echo "$line" | grep -q '"behavior":"host_executed_shell"' && echo "$line" | grep -q '"llmContent":"ShellCommandCompleted evidence'; then
    echo '{"type":"assistant","session_id":"mock-cosh-core-host-executed","message":{"content":[{"type":"text","text":"Host executed result received."}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-cosh-core-host-executed","is_error":false,"result":"cosh-core host executed completed"}'
else
    echo '{"type":"result","subtype":"error","session_id":"mock-cosh-core-host-executed","is_error":true,"result":"missing host_executed_shell result"}'
    exit 1
fi
