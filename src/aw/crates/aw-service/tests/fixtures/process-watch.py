"""Change procfs access after the parent has retained process descriptors."""

import ctypes
import signal
import sys

signal.alarm(20)
print("ready", flush=True)
assert sys.stdin.readline() == "change\n"
assert ctypes.CDLL(None).prctl(4, 0, 0, 0, 0) == 0  # PR_SET_DUMPABLE
print("changed", flush=True)
sys.stdin.readline()
