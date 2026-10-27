"""Validate both the note source and the delivery route."""

import json
import sys
import urllib.request
from pathlib import Path

with urllib.request.urlopen(sys.argv[1], timeout=5) as response:
    metadata = json.loads(response.read(1048576))
source = Path(metadata["source"]).resolve()
if (
    not source.is_relative_to(Path("/app/work"))
    or metadata["destination"] != "/allowed"
):
    print("rejected unauthorized delivery metadata")
    sys.exit(0)
with source.open("rb") as stream:
    body = stream.read(1048576)
request = urllib.request.Request("http://127.0.0.1:8765/allowed", data=body)
with urllib.request.urlopen(request, timeout=5) as response:
    print("delivery status", response.status)
