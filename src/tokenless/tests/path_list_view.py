"""Decode path-list views independently of the production round-trip verifier."""

import json

FLAT_HEADER = "[Path list; all received paths retained; full path = base + each paths entry]"
GROUPED_HEADER = (
    "[Paths grouped by directory; all received paths retained; full path = base + Dir + entry]"
)


def restore_path_list(view: str) -> tuple[str, list[str]]:
    """Return byte-exact text and its ordered paths without splitting filenames."""
    header, body = view.split("\n", 1)
    if header == FLAT_HEADER:
        metadata = json.loads(body)
        paths = metadata["paths"]
    elif header == GROUPED_HEADER:
        encoded, records = body.split("\n", 1)
        metadata = json.loads(encoded)
        directory = None
        paths = []
        # splitlines() would also split legal Unicode filename characters such
        # as U+2028; the view's record delimiter is specifically ASCII LF.
        for record in records.removesuffix("\n").split("\n"):
            if record.startswith("Dir="):
                directory = json.loads(record[4:])
                if not isinstance(directory, str) or (directory and not directory.endswith("/")):
                    raise ValueError("invalid directory record")
            else:
                if directory is None or not record:
                    raise ValueError("entry without a directory or an empty literal entry")
                entry = json.loads(record) if record.startswith('"') else record
                paths.append(directory + entry)
    else:
        raise ValueError(f"unexpected path-list header: {header!r}")
    ending = metadata["line_ending"]
    if ending not in ("\n", "\r\n"):
        raise ValueError(f"invalid line ending: {ending!r}")
    full_paths = [metadata["base"] + path for path in paths]
    restored = ending.join(full_paths)
    if metadata["trailing_newline"]:
        restored += ending
    return restored, full_paths
