#!/bin/bash
if read -r -t 0.2 _line; then
  echo '{"type":"result","subtype":"error","session_id":"mock-qwen-stream","is_error":true,"result":"stdin was not closed"}'
  exit 1
fi

prompt="${*: -1}"
if [[ -z "$prompt" ]]; then
  echo '{"type":"result","subtype":"error","session_id":"mock-qwen-stream","is_error":true,"result":"missing prompt argv"}'
  exit 1
fi

echo '{"type":"system","subtype":"init","model":"mock-qwen","session_id":"mock-qwen-stream"}'
echo '{"type":"result","subtype":"success","session_id":"mock-qwen-stream","is_error":false,"result":"qwen stream completed"}'
