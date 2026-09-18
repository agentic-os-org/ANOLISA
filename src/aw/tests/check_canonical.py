#!/usr/bin/env python3
"""Check valid AW JSON encoding vectors independently in Python and JavaScript.

This is a vector test, not a production wire decoder. Rejection of duplicate
keys, floats, excessive depth and unsafe integers is tested by Rust.
"""

import hashlib
import json
import subprocess
from pathlib import Path

vectors = Path(__file__).parent / "fixtures" / "canonical-vectors.json"
loaded = json.loads(vectors.read_text(encoding="utf-8"))
if not isinstance(loaded, list) or not loaded:
    raise ValueError("canonical vectors must be a nonempty list")
for vector in loaded:
    encoded = json.dumps(
        vector["input"], sort_keys=True, ensure_ascii=False, separators=(",", ":")
    ).encode("utf-8")
    if encoded.decode("utf-8") != vector["canonical"]:
        raise ValueError("Python canonical encoding differs from the vector")
    if hashlib.sha256(encoded).hexdigest() != vector["digest"]:
        raise ValueError("Python canonical digest differs from the vector")

subprocess.run(
    [
        "node",
        "--input-type=module",
        "-e",
        r"""
import { readFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import assert from 'node:assert/strict';
// Assemble object members directly: JS object enumeration reorders numeric keys.
function encode(value) {
  if (Array.isArray(value)) return '[' + value.map(encode).join(',') + ']';
  if (value !== null && typeof value === 'object') {
    return '{' + Object.keys(value).sort().map(
      key => JSON.stringify(key) + ':' + encode(value[key])
    ).join(',') + '}';
  }
  return JSON.stringify(value);
}
for (const vector of JSON.parse(readFileSync(process.argv[1], 'utf8'))) {
  const encoded = encode(vector.input);
  assert.equal(encoded, vector.canonical);
  assert.equal(createHash('sha256').update(encoded, 'utf8').digest('hex'), vector.digest);
}
""",
        str(vectors),
    ],
    check=True,
    timeout=15,
)
print("AW JSON vectors agree in Python and JavaScript")
