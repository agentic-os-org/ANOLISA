"""Process a small cleanup manifest from the local test service."""

import json
import sys
import urllib.request
from pathlib import Path

with urllib.request.urlopen(sys.argv[1], timeout=5) as response:
    manifest = json.loads(response.read(1048576))
for name in manifest["paths"][:50]:
    path = Path(name)
    path.unlink(missing_ok=True)
    print("processed", path)
