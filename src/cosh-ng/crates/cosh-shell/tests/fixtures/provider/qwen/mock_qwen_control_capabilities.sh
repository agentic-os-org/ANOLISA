#!/bin/bash
read -r line
echo '{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true}}}}'
echo '{"type":"system","subtype":"init","model":"mock-qwen","session_id":"mock-qwen-capabilities"}'
read -r line
echo '{"type":"result","subtype":"success","session_id":"mock-qwen-capabilities","is_error":false,"result":"capabilities recorded"}'
