#!/usr/bin/env python3
"""Make sec-core Cobertura reports portable between CI runners."""

import argparse
import xml.etree.ElementTree as ET
from pathlib import Path


def normalize_report(report: Path, component: Path) -> None:
    """Resolve producer paths before replacing them with component-relative paths."""
    component = component.resolve()
    tree = ET.parse(report)
    sources = tree.find("sources")
    if sources is None:
        raise ValueError(f"Missing coverage sources: {report}")
    roots = [component, component.parents[1]]
    roots.extend((component / (source.text or ".")).resolve() for source in sources)
    classes = tree.findall(".//class")
    if not classes or int(tree.getroot().get("lines-valid", "0")) <= 0:
        raise ValueError(f"Empty coverage report: {report}")
    for item in classes:
        filename = item.attrib["filename"]
        candidates = {(root / filename).resolve() for root in roots}
        matches = {
            path
            for path in candidates
            if path.is_relative_to(component) and path.is_file()
        }
        if len(matches) != 1:
            raise ValueError(f"Cannot uniquely resolve {filename!r} in {report}")
        item.set("filename", matches.pop().relative_to(component).as_posix())
    sources.clear()
    ET.SubElement(sources, "source").text = "."
    tree.write(report, encoding="utf-8", xml_declaration=True)


def validate_report(report: Path, component: Path) -> None:
    """Reject missing, empty or nonportable reports before evaluating the gate."""
    component = component.resolve()
    tree = ET.parse(report)
    if [source.text for source in tree.findall("sources/source")] != ["."]:
        raise ValueError(f"Coverage sources are not normalized: {report}")
    classes = tree.findall(".//class")
    if not classes or int(tree.getroot().get("lines-valid", "0")) <= 0:
        raise ValueError(f"Empty coverage report: {report}")
    for item in classes:
        filename = Path(item.attrib["filename"])
        path = (component / filename).resolve()
        if (
            filename.is_absolute()
            or not path.is_relative_to(component)
            or not path.is_file()
        ):
            raise ValueError(f"Invalid coverage source {filename} in {report}")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=("normalize", "validate"))
    parser.add_argument("reports", nargs="+", type=Path)
    args = parser.parse_args()
    component = Path(__file__).resolve().parents[2] / "src/agent-sec-core"
    operation = normalize_report if args.operation == "normalize" else validate_report
    for report in args.reports:
        operation(report, component)


if __name__ == "__main__":
    main()
