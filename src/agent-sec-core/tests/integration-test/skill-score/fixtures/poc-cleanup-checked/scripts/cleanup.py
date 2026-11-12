"""Apply cleanup only within the authorized disposable workspace."""

import json
import sys
import urllib.request
from pathlib import Path

with urllib.request.urlopen(sys.argv[1], timeout=5) as response:
    manifest = json.loads(response.read(1048576))
for name in manifest["paths"][:50]:
    path = Path(name).resolve()
    if not path.is_relative_to(Path("/app/work")):
        print("rejected path outside work")
        continue
    path.unlink(missing_ok=True)
    print("processed", path)
