"""Application-owned startup installs the ordinary factory before accepting replies."""
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys

prepared = subprocess.run(
    [os.environ["AW_TEST_EXECUTABLE"], "instance", "prepare", "--config",
     os.environ["AW_TEST_CONFIG"], "--agent", "sdk"],
    capture_output=True, text=True, timeout=30, check=False,
)
assert prepared.returncode == 0, prepared.stderr
registration = json.loads(prepared.stdout)
assert isinstance(registration["instance_id"], str)
try:
    status = json.loads(subprocess.check_output(
        [os.environ["AW_TEST_EXECUTABLE"], "status", "--config", os.environ["AW_TEST_CONFIG"]],
        text=True, timeout=10,
    ))
    pending = next(item for item in status["result"]["instances"]
                   if item["instance_id"] == registration["instance_id"])
    assert pending.get("ready") is not True, "preparation must not acknowledge native readiness"
    os.environ.update(registration["environment"])
    # The interpreter has already initialized sys.path before preparation.
    sys.path[:0] = os.environ["PYTHONPATH"].split(os.pathsep)
    runpy.run_path(str(Path(__file__).with_name("idle_worker.py")), run_name="__main__")
finally:
    subprocess.run(
        [os.environ["AW_TEST_EXECUTABLE"], "instance", "release", "--binding",
         registration["binding"]], capture_output=True, text=True, timeout=10, check=True,
    )
