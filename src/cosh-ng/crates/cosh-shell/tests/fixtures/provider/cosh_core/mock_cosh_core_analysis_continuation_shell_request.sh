#!/bin/bash
read -r line
echo '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
echo '{"type":"system","subtype":"init","model":"mock-cosh-core","session_id":"mock-cosh-core-analysis-continuation"}'
read -r line
echo '{"type":"control_request","request_id":"cosh-core-analysis-deny-001","request":{"subtype":"can_use_tool","tool_name":"shell","input":{"command":"df -h"},"tool_use_id":"call_cosh_core_analysis_deny_001"}}'
read -r line
if echo "$line" | grep -q '"behavior":"deny"' && echo "$line" | grep -q 'foreground shell command already completed'; then
    echo '{"type":"assistant","session_id":"mock-cosh-core-analysis-continuation","message":{"content":[{"type":"text","text":"Cosh-core analysis continuation shell request was denied."}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-cosh-core-analysis-continuation","is_error":false,"result":"cosh-core analysis continuation denied"}'
else
    echo '{"type":"result","subtype":"error","session_id":"mock-cosh-core-analysis-continuation","is_error":true,"result":"expected shell deny response"}'
    exit 1
fi
