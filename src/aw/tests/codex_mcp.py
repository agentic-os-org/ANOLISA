"""Disposable stdio MCP server for the Codex result-replacement fixture."""

import json
import sys


for line in sys.stdin:
    request = json.loads(line)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
                  "serverInfo": {"name": "fixture", "version": "1.0"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "echo", "description": "Return fixture data",
                             "inputSchema": {"type": "object", "properties": {
                                 "value": {"type": "string"}}, "required": ["value"]}}]}
    elif method == "tools/call":
        if sys.argv[1:] == ["media"]:
            result = {"content": [{"type": "image", "data": "c2VjcmV0",
                                   "mimeType": "image/png"}], "isError": False}
        elif sys.argv[1:] == ["large"]:
            result = {"content": [{"type": "text", "text": "AW_MCP_ORIGINAL" * 150000}],
                      "isError": False}
        else:
            result = {"content": [{"type": "text", "text": "AW_MCP_ORIGINAL"}],
                      "structuredContent": {"private": "AW_MCP_ORIGINAL"}, "isError": False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}) + "\n")
    sys.stdout.flush()
