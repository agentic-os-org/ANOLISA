#!/usr/bin/env python3
"""Shared restorer for both path-list view formats.

The search-path-sharing compressor emits two lossless views: the compact-JSON
common-prefix view and the directory-grouped view. The suites that drive real
`tokenless compress` traffic use this helper so a fixture flipping from one
view to the other fails on round-trip data, not on a format assumption.
"""

import json

PREFIX_HEADER = (
    "[Path list; all received paths retained; full path = base + each paths entry]"
)
GROUPED_HEADER_PREFIX = "[Paths grouped by directory"


def restore(view: str) -> str:
    """Reconstruct the original byte-for-byte output from either view."""
    header, _, body = view.partition("\n")
    if header == PREFIX_HEADER:
        data = json.loads(body)
        output = data["line_ending"].join(
            data["base"] + path for path in data["paths"]
        )
        if data["trailing_newline"]:
            output += data["line_ending"]
        return output
    if header.startswith(GROUPED_HEADER_PREFIX):
        if "CRLF; final newline" in header:
            ending, trailing = "\r\n", True
        elif "CRLF; no final newline" in header:
            ending, trailing = "\r\n", False
        elif "no final newline" in header:
            ending, trailing = "\n", False
        else:
            ending, trailing = "\n", True
        lines = body.split("\n")
        # The body may end with a trailing LF (the header records whether the
        # *input* ended with a newline; the view itself always ends its last
        # entry with one). A bare empty line is never an entry — empty
        # entries are JSON-quoted — so drop a final empty split segment.
        if lines and lines[-1] == "":
            lines.pop()
        base = json.loads(lines[0][len("Base="):])
        current_dir = ""
        parts = []
        for line in lines[1:]:
            if line.startswith("Dir="):
                current_dir = json.loads(line[len("Dir="):])
                continue
            entry = json.loads(line) if line.startswith('"') else line
            parts.append(base + current_dir + entry)
        output = ending.join(parts)
        if trailing:
            output += ending
        return output
    raise AssertionError(f"not a path-list view: {view[:120]!r}")
