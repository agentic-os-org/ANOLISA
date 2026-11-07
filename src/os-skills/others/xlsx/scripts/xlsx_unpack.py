#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""
xlsx_unpack.py — Unpack an xlsx file into a working directory for XML editing.

Usage:
    python3 xlsx_unpack.py <input.xlsx> <output_dir> [--force]

What it does:
1. Unzips the xlsx (which is a ZIP archive)
2. Pretty-prints all XML and .rels files for readability
3. Prints a summary of key files to edit

Safety:
- An existing non-empty output directory is only replaced when it looks like a
  previous unpack work directory (it contains the .xlsx-unpacked marker) or
  when --force is passed. Anything else is refused, not deleted.
"""

import argparse
import sys
import zipfile
import os
import shutil
import xml.dom.minidom

# Written into every directory this tool creates, so a later run can tell its
# own work directory (safe to replace on re-unpack) from an unrelated one.
MARKER_NAME = ".xlsx-unpacked"


def pretty_print_xml(content: bytes):
    """Pretty-print XML bytes.

    Returns the pretty-printed text, or None when the bytes are not
    well-formed XML — the caller must then leave the member untouched.
    """
    try:
        dom = xml.dom.minidom.parseString(content)
        pretty = dom.toprettyxml(indent="  ", encoding="utf-8").decode("utf-8")
        # Remove the extra blank lines toprettyxml adds
        lines = [line for line in pretty.splitlines() if line.strip()]
        return "\n".join(lines) + "\n"
    except Exception:
        return None


def prepare_output_dir(output_dir: str, force: bool) -> None:
    """Make output_dir an empty directory, refusing to delete foreign content.

    A fresh or empty directory is used as-is. A directory containing the
    unpack marker is our own previous work directory — replacing it is the
    documented re-unpack path. Anything else non-empty is an accident
    waiting to happen (an editing session, a typo like `.` or a wrong
    absolute path) and is refused unless --force is passed.
    """
    if not os.path.exists(output_dir):
        os.makedirs(output_dir)
        return
    if not os.path.isdir(output_dir):
        print(f"ERROR: Output path exists and is not a directory: {output_dir}",
              file=sys.stderr)
        sys.exit(1)
    entries = os.listdir(output_dir)
    if not entries:
        return
    if force or MARKER_NAME in entries:
        shutil.rmtree(output_dir)
        os.makedirs(output_dir)
        return
    print(
        f"ERROR: output directory '{output_dir}' exists and is not an unpack "
        f"work directory (no {MARKER_NAME} marker). Refusing to delete it. "
        "Use an empty directory, pass --force, or remove it manually.",
        file=sys.stderr,
    )
    sys.exit(1)


def unpack(xlsx_path: str, output_dir: str, force: bool = False) -> None:
    if not os.path.isfile(xlsx_path):
        print(f"ERROR: File not found: {xlsx_path}", file=sys.stderr)
        sys.exit(1)

    if not xlsx_path.lower().endswith((".xlsx", ".xlsm")):
        print(f"WARNING: '{xlsx_path}' does not have an .xlsx/.xlsm extension", file=sys.stderr)

    prepare_output_dir(output_dir, force)

    try:
        with zipfile.ZipFile(xlsx_path, "r") as z:
            # Validate member paths to prevent zip-slip (path traversal) attacks
            for member in z.namelist():
                member_path = os.path.realpath(os.path.join(output_dir, member))
                if not member_path.startswith(os.path.realpath(output_dir) + os.sep) and member_path != os.path.realpath(output_dir):
                    print(f"ERROR: Zip entry '{member}' would escape target directory (path traversal blocked)", file=sys.stderr)
                    shutil.rmtree(output_dir, ignore_errors=True)
                    sys.exit(1)
            z.extractall(output_dir)
    except zipfile.BadZipFile:
        shutil.rmtree(output_dir, ignore_errors=True)
        print(f"ERROR: '{xlsx_path}' is not a valid ZIP/xlsx file", file=sys.stderr)
        sys.exit(1)

    # Pretty-print XML and .rels files
    xml_count = 0
    for dirpath, _, filenames in os.walk(output_dir):
        for fname in filenames:
            if fname.endswith(".xml") or fname.endswith(".rels"):
                fpath = os.path.join(dirpath, fname)
                with open(fpath, "rb") as f:
                    raw = f.read()
                pretty = pretty_print_xml(raw)
                if pretty is None:
                    # Not well-formed XML: rewriting would corrupt bytes we
                    # cannot represent (errors="replace" turns them into
                    # U+FFFD). Leave the member exactly as extracted.
                    rel = os.path.relpath(fpath, output_dir)
                    print(f"WARNING: {rel}: not well-formed XML — left untouched",
                          file=sys.stderr)
                    continue
                with open(fpath, "w", encoding="utf-8") as f:
                    f.write(pretty)
                xml_count += 1

    # Mark the directory as our work product so re-unpack can replace it.
    with open(os.path.join(output_dir, MARKER_NAME), "w", encoding="utf-8") as f:
        f.write(os.path.basename(xlsx_path) + "\n")

    print(f"Unpacked '{xlsx_path}' → '{output_dir}'")
    print(f"Pretty-printed {xml_count} XML/rels files\n")

    # Print key files grouped by category
    categories = {
        "Package root": ["[Content_Types].xml", "_rels/.rels"],
        "Workbook": ["xl/workbook.xml", "xl/_rels/workbook.xml.rels"],
        "Styles & Strings": ["xl/styles.xml", "xl/sharedStrings.xml"],
        "Worksheets": [],
    }

    all_files = []
    for dirpath, _, filenames in os.walk(output_dir):
        for fname in filenames:
            rel = os.path.relpath(os.path.join(dirpath, fname), output_dir)
            all_files.append(rel)

    # Collect worksheets
    for rel in sorted(all_files):
        if rel.startswith("xl/worksheets/") and rel.endswith(".xml"):
            categories["Worksheets"].append(rel)

    print("Key files to inspect/edit:")
    for category, files in categories.items():
        if not files:
            continue
        print(f"\n  [{category}]")
        for f in files:
            full = os.path.join(output_dir, f)
            if os.path.isfile(full):
                size = os.path.getsize(full)
                print(f"    {f}  ({size:,} bytes)")
            else:
                print(f"    {f}  (not found)")

    # Warn about high-risk files present
    risky = {
        "xl/vbaProject.bin": "VBA macros — DO NOT modify",
        "xl/pivotTables": "Pivot tables — update source ranges carefully if shifting rows",
        "xl/charts": "Charts — update data ranges if shifting rows",
    }
    print("\n  [High-risk content detected:]")
    found_any = False
    for path, warning in risky.items():
        full = os.path.join(output_dir, path)
        if os.path.exists(full):
            print(f"    ⚠️  {path} — {warning}")
            found_any = True
    if not found_any:
        print("    ✓ None (safe to edit)")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(
        description="Unpack an xlsx file into a working directory for XML editing.")
    parser.add_argument("input", help="input .xlsx/.xlsm file")
    parser.add_argument("output_dir", help="working directory to extract into")
    parser.add_argument(
        "--force", action="store_true",
        help="replace output_dir even when it is not a previous unpack "
             "work directory (destructive)")
    args = parser.parse_args()
    unpack(args.input, args.output_dir, force=args.force)
