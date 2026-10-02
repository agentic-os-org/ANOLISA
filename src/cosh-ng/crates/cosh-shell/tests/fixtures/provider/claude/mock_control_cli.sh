#!/bin/bash
# Mock CLI for control protocol integration tests
# Speaks the same stdin/stdout JSON protocol as claude/co

# Read initialize
read -r line

# Emit init system message
echo '{"type":"system","subtype":"init","model":"mock-model","session_id":"mock-session-001"}'

# Read user message
read -r line

# Emit assistant text (snapshot with cumulative content)
echo '{"type":"assistant","session_id":"mock-session-001","message":{"content":[{"type":"text","text":"Analyzing your request..."}]}}'

# Emit can_use_tool control request
echo '{"type":"control_request","request_id":"mock-req-001","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo hello"},"description":"Run echo","tool_use_id":"toolu_mock001"}}'

# Read allow/deny response
read -r line

# Check response behavior
if echo "$line" | grep -q '"allow"'; then
    if ! echo "$line" | grep -q '"updatedInput":{"command":"echo hello"}'; then
        echo '{"type":"result","subtype":"error","session_id":"mock-session-001","is_error":true,"result":"missing updatedInput in allow response"}'
        exit 1
    fi
    # Emit updated assistant text with tool result
    echo '{"type":"assistant","session_id":"mock-session-001","message":{"content":[{"type":"text","text":"Analyzing your request... The command executed successfully."}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-session-001","is_error":false,"result":"The command executed successfully."}'
else
    echo '{"type":"assistant","session_id":"mock-session-001","message":{"content":[{"type":"text","text":"Analyzing your request... Operation was denied by user."}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-session-001","is_error":false,"result":"Operation was denied by user."}'
fi
