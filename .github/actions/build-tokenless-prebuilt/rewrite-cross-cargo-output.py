#!/usr/bin/env python3
"""Map Cross container paths in Cargo JSON messages back to host paths."""

from __future__ import annotations

import os
import sys


def main() -> int:
    if len(sys.argv) != 2:
        raise SystemExit("usage: rewrite-cross-cargo-output.py PROJECT_ROOT")

    project_root = os.fsencode(sys.argv[1])
    if not os.path.isabs(project_root):
        raise SystemExit("PROJECT_ROOT must be absolute")

    replacements = (
        (b"path+file:///project", b"path+file://" + project_root),
        (b'"/project', b'"' + project_root),
        (b'"/target', b'"' + project_root + b"/target"),
    )
    for line in sys.stdin.buffer:
        for source, destination in replacements:
            line = line.replace(source, destination)
        sys.stdout.buffer.write(line)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
