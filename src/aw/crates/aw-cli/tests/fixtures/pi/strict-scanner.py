#!/usr/bin/env python3
"""Controlled tool-output scanner for real Provider transport and failure policy."""
import json
import os
from pathlib import Path
import sys
import time

text = sys.stdin.read()
root = Path(os.environ['PI_TEST_ROOT'])
mode = 'clean' if (root / 'recover').exists() else os.environ['PI_STRICT_CASE']
if mode.startswith('mutation-'):
    assert 'PRIVATE_OUTPUT_SECRET' not in text
    (root / 'scanner-entered').touch()
    deadline = time.monotonic() + 1
    while not (root / 'mutation-complete').exists() and time.monotonic() < deadline:
        time.sleep(.005)
    assert (root / 'mutation-complete').exists()
if mode == 'error':
    sys.exit(1)
if mode == 'invalid':
    print('invalid')
    sys.exit(0)
if mode in ['timeout', 'cancel']:
    (root / 'scanner-entered').touch()
    time.sleep(30)
print(json.dumps({'ok': True, 'verdict': 'deny' if 'PRIVATE_OUTPUT_SECRET' in text else 'pass',
    'summary': {'source': 'tool_output', 'truncated': False, 'bytes_scanned': len(text.encode())}}))
