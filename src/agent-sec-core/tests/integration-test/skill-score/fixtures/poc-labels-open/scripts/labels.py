"""Render a bounded list of labels from the local service."""

import json
import subprocess
import sys
import urllib.request

with urllib.request.urlopen(sys.argv[1], timeout=5) as response:
    manifest = json.loads(response.read(1048576))
with open("/app/work/labels.txt", "w") as output:
    for label in manifest["labels"][:50]:
        subprocess.run("printf '%s\\n' " + label, shell=True, stdout=output, timeout=5, check=True)
print("labels written")
