#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""
xlsx_pack.py — Pack a working directory back into a valid xlsx file.

Usage:
    python3 xlsx_pack.py <source_dir> <output.xlsx>

Requirements:
    - source_dir must contain [Content_Types].xml at its root
    - All XML files are re-validated for well-formedness before packing

The resulting xlsx is a valid ZIP archive with correct OOXML structure.
"""

import sys
import os
import zipfile
import xml.etree.ElementTree as ET


def validate_xml_files(source_dir: str) -> list[str]:
    """Return list of XML files that fail to parse."""
    bad = []
    for dirpath, _, filenames in os.walk(source_dir):
        for fname in filenames:
            if fname.endswith(".xml") or fname.endswith(".rels"):
                fpath = os.path.join(dirpath, fname)
                try:
                    ET.parse(fpath)
                except ET.ParseError as e:
                    rel = os.path.relpath(fpath, source_dir)
                    bad.append(f"{rel}: {e}")
    return bad


def pack(source_dir: str, xlsx_path: str) -> None:
    if not os.path.isdir(source_dir):
        print(f"ERROR: Directory not found: {source_dir}", file=sys.stderr)
        sys.exit(1)

    content_types = os.path.join(source_dir, "[Content_Types].xml")
    if not os.path.isfile(content_types):
        print(
            f"ERROR: Missing [Content_Types].xml in {source_dir}\n"
            "  This file is required at the root of every valid xlsx package.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Validate XML well-formedness before packing
    print("Validating XML files...")
    bad_files = validate_xml_files(source_dir)
    if bad_files:
        print("ERROR: The following files have XML parse errors:", file=sys.stderr)
        for b in bad_files:
            print(f"  {b}", file=sys.stderr)
        print(
            "\nFix all XML errors before packing. "
            "A malformed xlsx cannot be opened by Excel or LibreOffice.",
            file=sys.stderr,
        )
        sys.exit(1)

    # Refuse to pack our own output into itself: when the target lives inside
    # the source directory, the walk below would pick up the half-written
    # archive and store a truncated or self-referential member.
    out_abs = os.path.abspath(xlsx_path)
    src_abs = os.path.abspath(source_dir)
    if out_abs == src_abs or out_abs.startswith(src_abs + os.sep):
        print(
            "ERROR: output file must be outside the source directory "
            f"(got '{xlsx_path}' inside '{source_dir}')",
            file=sys.stderr,
        )
        sys.exit(1)

    print("✓ All XML files are well-formed")

    # Count files to pack
    file_count = sum(len(files) for _, _, files in os.walk(source_dir))

    with zipfile.ZipFile(xlsx_path, "w", compression=zipfile.ZIP_DEFLATED) as z:
        for dirpath, _, filenames in os.walk(source_dir):
            for fname in filenames:
                fpath = os.path.join(dirpath, fname)
                # Zip member names are OPC part names: always forward slashes,
                # never os.sep (a backslash member name is unresolvable in
                # Excel and LibreOffice on every platform).
                arcname = os.path.relpath(fpath, source_dir).replace(os.sep, "/")
                z.write(fpath, arcname)

    # The forward-slash invariant, asserted where a regression would show up.
    with zipfile.ZipFile(xlsx_path, "r") as z:
        members = z.namelist()
        assert not any("\\" in name for name in members), \
            "packed archive contains backslash member names"

    size = os.path.getsize(xlsx_path)
    print(f"Packed {file_count} files → '{xlsx_path}' ({size:,} bytes)")
    print("\nNext step: run formula_check.py to validate formulas:")
    print(f"  python3 formula_check.py {xlsx_path}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        print("Usage: xlsx_pack.py <source_dir> <output.xlsx>")
        sys.exit(1)
    pack(sys.argv[1], sys.argv[2])
