#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Preserve namespace declarations used by OOXML compatibility attributes."""

import xml.etree.ElementTree as ET

XML_NAMESPACE = "http://www.w3.org/XML/1998/namespace"


def parse_xml(path: str) -> ET.ElementTree:
    """Keep xmlns declarations on their original elements, including rebindings."""
    declarations = []
    parser = ET.iterparse(path, events=("start-ns", "start"))
    for event, value in parser:
        if event == "start-ns":
            prefix, namespace = value
            declarations.append((f"xmlns:{prefix}" if prefix else "xmlns", namespace))
        else:
            value.attrib.update(declarations)
            declarations.clear()
    return ET.ElementTree(parser.root)


def write_xml(tree: ET.ElementTree, path: str) -> None:
    """Serialize expanded names without changing prefixes stored in XML values.

    ElementTree otherwise discards declarations referenced only by attribute
    values such as mc:Ignorable or mc:Choice/@Requires. Retained declarations
    must not collide with the prefixes ElementTree generates for element and
    attribute names, including when a document rebinds a prefix in a subtree.
    """
    bindings = {}
    namespaces = set()
    for element in tree.iter():
        for name, namespace in element.attrib.items():
            if name.startswith("xmlns:"):
                bindings.setdefault(name.partition(":")[2], set()).add(namespace)
        for name in (element.tag, *element.attrib):
            if name.startswith("{"):
                namespaces.add(name[1:].partition("}")[0])

    index = 0
    generated = {}
    for namespace in sorted(namespaces - {XML_NAMESPACE}):
        while f"xlsx{index}" in bindings and bindings[f"xlsx{index}"] != {namespace}:
            index += 1
        prefix = f"xlsx{index}"
        ET.register_namespace(prefix, namespace)
        generated[f"xmlns:{prefix}"] = namespace
        index += 1

    # Reuse our aliases on subsequent edits without duplicating the root
    # declarations that ElementTree emits itself. Keep scoped declarations.
    root = tree.getroot()
    emitted = {
        name: root.attrib.pop(name)
        for name, namespace in generated.items()
        if root.get(name) == namespace
    }
    try:
        tree.write(path, encoding="unicode", xml_declaration=False)
    finally:
        root.attrib.update(emitted)
