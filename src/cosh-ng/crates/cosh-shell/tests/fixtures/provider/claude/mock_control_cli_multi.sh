#!/bin/bash
# Mock CLI for multi-tool control protocol integration tests
read -r line
echo '{"type":"system","subtype":"init","model":"mock-model","session_id":"mock-session-002"}'
read -r line
echo '{"type":"assistant","session_id":"mock-session-002","message":{"content":[{"type":"text","text":"Working on it..."}]}}'

# First tool request
echo '{"type":"control_request","request_id":"mock-req-A","request":{"subtype":"can_use_tool","tool_name":"Read","input":{"file_path":"/tmp/test.txt"},"description":"Read file","tool_use_id":"toolu_mockA"}}'
read -r line
if echo "$line" | grep -q '"allow"' && ! echo "$line" | grep -q '"updatedInput":{"file_path":"/tmp/test.txt"}'; then
    echo '{"type":"result","subtype":"error","session_id":"mock-session-002","is_error":true,"result":"missing first updatedInput in allow response"}'
    exit 1
fi

# Second tool request
echo '{"type":"control_request","request_id":"mock-req-B","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls -la"},"description":"List files","tool_use_id":"toolu_mockB"}}'
read -r line
if echo "$line" | grep -q '"allow"' && ! echo "$line" | grep -q '"updatedInput":{"command":"ls -la"}'; then
    echo '{"type":"result","subtype":"error","session_id":"mock-session-002","is_error":true,"result":"missing second updatedInput in allow response"}'
    exit 1
fi

echo '{"type":"assistant","session_id":"mock-session-002","message":{"content":[{"type":"text","text":"Working on it... All done."}]}}'
echo '{"type":"result","subtype":"success","session_id":"mock-session-002","is_error":false,"result":"All done."}'
