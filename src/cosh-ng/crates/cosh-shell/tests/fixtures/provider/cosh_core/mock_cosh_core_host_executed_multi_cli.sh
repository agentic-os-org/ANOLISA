#!/bin/bash
read -r line
echo '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
echo '{"type":"system","subtype":"init","model":"mock-cosh-core","session_id":"mock-cosh-core-host-executed-multi"}'
read -r line
echo '{"type":"control_request","request_id":"cosh-core-req-001","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"call_cosh_core_001"}}'
read -r line
if ! echo "$line" | grep -q '"behavior":"host_executed_shell"' || ! echo "$line" | grep -q 'df -h'; then
    echo '{"type":"result","subtype":"error","session_id":"mock-cosh-core-host-executed-multi","is_error":true,"result":"missing first host_executed_shell result"}'
    exit 1
fi
echo '{"type":"assistant","session_id":"mock-cosh-core-host-executed-multi","message":{"content":[{"type":"text","text":"First host executed result received."}]}}'
echo '{"type":"control_request","request_id":"cosh-core-req-002","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"du -sh ."},"tool_use_id":"call_cosh_core_002"}}'
read -r line
if ! echo "$line" | grep -q '"behavior":"host_executed_shell"' || ! echo "$line" | grep -q 'du -sh .'; then
    echo '{"type":"result","subtype":"error","session_id":"mock-cosh-core-host-executed-multi","is_error":true,"result":"missing second host_executed_shell result"}'
    exit 1
fi
echo '{"type":"assistant","session_id":"mock-cosh-core-host-executed-multi","message":{"content":[{"type":"text","text":"Second host executed result received."}]}}'
echo '{"type":"result","subtype":"success","session_id":"mock-cosh-core-host-executed-multi","is_error":false,"result":"cosh-core multi host executed completed"}'
