"""Preserve markup-compatibility namespace bindings across ElementTree writes.

ElementTree emits a declaration only for a namespace bound to an element or
attribute *name*. A prefix that appears only inside an attribute value — as
`mc:Ignorable` and `mc:Choice/@Requires` do — loses its declaration, so the
written part references a prefix it no longer defines and fails markup
compatibility validation.

Capture the declarations before parsing and put back the ones serialization
drops. Prefixes that must survive are then known from the document itself
instead of a hardcoded list that cannot cover namespaces a workbook introduces.
"""

from __future__ import annotations

import re

import xml.etree.ElementTree as ET

_DECLARATION_RE = re.compile(r'xmlns:([A-Za-z_][\w.-]*)="([^"]*)"')
_RESERVED_PREFIX_RE = re.compile(r"ns\d+$")


def capture(xml_text: str) -> dict[str, str]:
    """Returns every prefix declaration in one part, outermost binding first."""
    declarations: dict[str, str] = {}
    for prefix, uri in _DECLARATION_RE.findall(xml_text):
        declarations.setdefault(prefix, uri)
    return declarations


def capture_file(path: str) -> dict[str, str]:
    """Returns the prefix declarations of an on-disk part."""
    with open(path, "r", encoding="utf-8") as handle:
        return capture(handle.read())


def register(declarations: dict[str, str]) -> None:
    """Teaches ElementTree the original prefixes so it does not invent `ns0`."""
    for prefix, uri in declarations.items():
        # `ns0` and friends are ElementTree's own generated names; registering
        # them back is what a later parse would do anyway.
        if _RESERVED_PREFIX_RE.match(prefix):
            continue
        try:
            ET.register_namespace(prefix, uri)
        except ValueError:
            # xml and xmlns are reserved and cannot be re-registered.
            continue


def restore(text: str, declarations: dict[str, str]) -> str:
    """Re-adds the declarations `text` references but no longer declares.

    Declarations are added to the root start tag. A prefix declared on an
    ancestor keeps the meaning it had in a narrower scope, so widening the scope
    preserves the binding every reference relied on.
    """
    missing = {
        prefix: uri
        for prefix, uri in declarations.items()
        if f"xmlns:{prefix}=" not in text
    }
    if not missing:
        return text
    tag_end = _root_start_tag_end(text)
    if tag_end is None:
        return text
    additions = "".join(f' xmlns:{prefix}="{uri}"' for prefix, uri in missing.items())
    return text[:tag_end] + additions + text[tag_end:]


def _root_start_tag_end(text: str) -> int | None:
    """Returns the index of the `>` closing the root start tag."""
    index = 0
    while True:
        index = text.find("<", index)
        if index < 0:
            return None
        if text.startswith(("<?", "<!"), index):
            close = text.find(">", index)
            if close < 0:
                return None
            index = close + 1
            continue
        quote = ""
        for position in range(index + 1, len(text)):
            character = text[position]
            if quote:
                if character == quote:
                    quote = ""
            elif character in "\"'":
                quote = character
            elif character == ">":
                return position
        return None
