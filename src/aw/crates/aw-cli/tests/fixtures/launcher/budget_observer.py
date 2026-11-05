"""Delay observer initialization while retaining the event's absolute deadline."""

from pathlib import Path
import sys
import time

root = Path(sys.argv[1])
time.sleep(int(sys.argv[2]) / 1000)
(root / "observer-ready").touch()
# Stay alive beyond both the event and provider deadlines. The executor owns
# termination even when startup never reached the readiness marker.
time.sleep(30)
