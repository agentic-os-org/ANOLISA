#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""ST_Xstring text values shared by the unpacked-workbook editing helpers."""

import re
import xml.etree.ElementTree as ET

_ESCAPE = re.compile(r"_x([0-9A-Fa-f]{4})_")


def encode_xstring(text: str) -> str:
    """Protect literal escapes and replace characters disallowed in XML text."""
    protected = re.sub(r"_(?=x[0-9A-Fa-f]{4}_)", "_x005F_", text)
    return "".join(
        (
            f"_x{ord(character):04X}_"
            if (
                (ord(character) < 0x20 and character not in "\t\n\r")
                or 0xD800 <= ord(character) <= 0xDFFF
                or character in "\ufffe\uffff"
            )
            else character
        )
        for character in protected
    )


def decode_xstring(text: str) -> str:
    """Decode one escape layer, preserving protected literal patterns."""
    decoded = _ESCAPE.sub(lambda match: chr(int(match.group(1), 16)), text)
    # Existing producers may encode non-BMP characters as UTF-16 code-unit pairs.
    return decoded.encode("utf-16-le", "surrogatepass").decode("utf-16-le", "surrogatepass")


def write_shared_strings(tree: ET.ElementTree, path: str) -> None:
    """Serialize shared text in UTF-8 without normalizing CR or dropping blank lines."""
    content = ET.tostring(tree.getroot(), encoding="utf-8", xml_declaration=True)
    with open(path, "wb") as stream:
        stream.write(content.replace(b"\r", b"&#13;"))
