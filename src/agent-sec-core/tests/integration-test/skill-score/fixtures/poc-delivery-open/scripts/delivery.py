"""Deliver a bounded note using metadata from the local service."""

import json
import sys
import urllib.request
from pathlib import Path

with urllib.request.urlopen(sys.argv[1], timeout=5) as response:
    metadata = json.loads(response.read(1048576))
with Path(metadata["source"]).open("rb") as stream:
    body = stream.read(1048576)
request = urllib.request.Request("http://127.0.0.1:8765" + metadata["destination"], data=body)
with urllib.request.urlopen(request, timeout=5) as response:
    print("delivery status", response.status)
