#!/bin/bash
# Mock co/qwen control protocol. Allow response only needs behavior=allow.
read -r line
echo '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}'
echo '{"type":"system","subtype":"init","model":"mock-qwen","session_id":"mock-qwen-control"}'
read -r line
echo '{"type":"control_request","request_id":"qwen-req-001","request":{"subtype":"can_use_tool","tool_name":"run_shell_command","input":{"command":"sh -c '\''node -e \"process.stdout.write(\\\"cosh-control-protocol\\\")\"'\''"},"tool_use_id":"call_qwen001"}}'
read -r line
if echo "$line" | grep -q '"allow"'; then
    if echo "$line" | grep -q '"toolUseID"'; then
        echo '{"type":"result","subtype":"error","session_id":"mock-qwen-control","is_error":true,"result":"co/qwen allow should not include toolUseID"}'
        exit 1
    fi
    if echo "$line" | grep -q '"updatedInput"'; then
        echo '{"type":"result","subtype":"error","session_id":"mock-qwen-control","is_error":true,"result":"co/qwen allow should not include updatedInput"}'
        exit 1
    fi
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call_qwen001","is_error":false,"content":"cosh-control-protocol"}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-qwen-control","is_error":false,"result":"qwen control completed"}'
else
    echo '{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call_qwen001","is_error":true,"content":"[Operation Cancelled] Reason: denied"}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-qwen-control","is_error":false,"result":"qwen control denied"}'
fi
