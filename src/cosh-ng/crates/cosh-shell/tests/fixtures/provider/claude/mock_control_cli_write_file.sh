#!/bin/bash
# Mock qwen/co control protocol write_file request. It fails if allow clears args.
read -r line
echo '{"type":"system","subtype":"init","model":"mock-model","session_id":"mock-session-write"}'
read -r line
echo '{"type":"assistant","session_id":"mock-session-write","message":{"content":[{"type":"text","text":"Preparing a file write..."}]}}'
echo '{"type":"control_request","request_id":"mock-req-write","request":{"subtype":"can_use_tool","tool_name":"write_file","input":{"file_path":"/tmp/cosh-write.html","content":"<html>ok</html>"},"description":"Write file","tool_use_id":"toolu_write001"}}'
read -r line
if echo "$line" | grep -q '"allow"'; then
    if ! echo "$line" | grep -q '"updatedInput":{"file_path":"/tmp/cosh-write.html","content":"<html>ok</html>"}'; then
        echo '{"type":"result","subtype":"error","session_id":"mock-session-write","is_error":true,"result":"missing updatedInput in write_file allow response"}'
        exit 1
    fi
    if echo "$line" | grep -q '"updatedInput":{}'; then
        echo '{"type":"result","subtype":"error","session_id":"mock-session-write","is_error":true,"result":"write_file args were cleared by updatedInput"}'
        exit 1
    fi
    echo '{"type":"assistant","session_id":"mock-session-write","message":{"content":[{"type":"text","text":"Preparing a file write... File written successfully."}]}}'
    echo '{"type":"result","subtype":"success","session_id":"mock-session-write","is_error":false,"result":"File written successfully."}'
else
    echo '{"type":"result","subtype":"error","session_id":"mock-session-write","is_error":true,"result":"write_file was denied"}'
    exit 1
fi
