#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""
shared_strings_builder.py — Generate a valid sharedStrings.xml from a list of strings.

Usage (strings as command-line arguments):
    python3 shared_strings_builder.py "Revenue" "Cost" "Gross Profit" > sharedStrings.xml

Usage (strings from a file, one per line):
    python3 shared_strings_builder.py --file strings.txt > sharedStrings.xml

Usage (print index table instead of XML, for reference):
    python3 shared_strings_builder.py --index "Revenue" "Cost" "Gross Profit"
    python3 shared_strings_builder.py --index --file strings.txt

Output format:
    Valid xl/sharedStrings.xml written to stdout.
    Redirect to the correct path:
        python3 shared_strings_builder.py "A" "B" > /tmp/xlsx_work/xl/sharedStrings.xml

Notes:
    - Strings are de-duplicated: identical strings appear only once in the table.
      De-duplication compares the ORIGINAL strings, before any escaping, so a
      literal escape token like "_x0041_" stays distinct from the corresponding
      control character.
    - The 'count' attribute equals the number of unique strings (appropriate for new files
      where each string is used in exactly one cell). If a string appears in multiple cells,
      manually increment 'count' by the number of extra references.
    - Special characters (&, <, >) are automatically XML-escaped.
    - Text is additionally encoded with the SpreadsheetML ST_Xstring rules
      (MS-OI29500): literal "_xHHHH_" sequences are protected as "_x005F_HHHH_",
      control characters that are illegal in XML 1.0 (including carriage
      returns, which XML line-end normalization would otherwise turn into
      line feeds) are encoded as "_xHHHH_", and the noncharacters U+FFFE /
      U+FFFF are encoded as "_xFFFE_" / "_xFFFF_". Excel decodes these on
      read; ordinary Unicode, tabs and line feeds pass through untouched.
    - Leading/trailing spaces are preserved with xml:space="preserve".
"""

import sys
import html
import re
import argparse


HEADER = '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
SST_NS = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"

# ST_Xstring escape notation (MS-OI29500, "SpreadsheetML shared string
# parsing"): Excel decodes "_xHHHH_" sequences in shared-string text, so
# literal look-alikes must be escaped and characters that are illegal in
# XML 1.0 (or that XML line-end normalization would corrupt) must be
# encoded. The literal-escape pass MUST run before the control-character
# pass, otherwise the escapes produced for control characters would
# themselves be escaped a second time.
ST_XSTRING_LITERAL = re.compile(r"(_x[0-9a-fA-F]{4}_)")
ST_XSTRING_CONTROL = re.compile(r"[\x00-\x08\x0b-\x1f]")
ST_XSTRING_NONCHARACTERS = (("￾", "_xFFFE_"), ("￿", "_xFFFF_"))


def st_xstring_escape(s: str) -> str:
    """Encode a string with the SpreadsheetML ST_Xstring rules.

    - A literal "_xHHHH_" sequence becomes "_x005F_HHHH_".
    - Characters in [\x00-\x08, \x0b-\x1f] (including "\r") become
      "_xHHHH_" with uppercase hexadecimal digits.
    - The noncharacters U+FFFE and U+FFFF become "_xFFFE_" / "_xFFFF_".
    - Tabs, line feeds and ordinary Unicode are left untouched.
    """
    s = ST_XSTRING_LITERAL.sub(r"_x005F\1", s)
    s = ST_XSTRING_CONTROL.sub(lambda m: f"_x{ord(m.group(0)):04X}_", s)
    for char, escape in ST_XSTRING_NONCHARACTERS:
        s = s.replace(char, escape)
    return s


def escape_text(s: str) -> tuple[str, bool]:
    """
    Return (escaped_text, needs_preserve).
    needs_preserve is True if the string has leading or trailing whitespace.
    """
    escaped = html.escape(st_xstring_escape(s), quote=False)
    needs_preserve = s != s.strip()
    return escaped, needs_preserve


def build_xml(strings: list[str]) -> str:
    """Build sharedStrings.xml content from a list of unique strings."""
    n = len(strings)
    lines = [
        HEADER,
        f'<sst xmlns="{SST_NS}" count="{n}" uniqueCount="{n}">',
    ]
    for i, s in enumerate(strings):
        escaped, preserve = escape_text(s)
        if preserve:
            lines.append(f'  <si><t xml:space="preserve">{escaped}</t></si>'
                         f'  <!-- index {i} -->')
        else:
            lines.append(f'  <si><t>{escaped}</t></si>  <!-- index {i} -->')
    lines.append("</sst>")
    return "\n".join(lines) + "\n"


def build_index_table(strings: list[str]) -> str:
    """Return a human-readable index table (for agent reference, not written to file)."""
    lines = [
        f"{'Index':<6}  String",
        "-" * 50,
    ]
    for i, s in enumerate(strings):
        lines.append(f"{i:<6}  {s!r}")
    lines.append("")
    lines.append(
        f"Total: {len(strings)} unique strings. "
        "Use these indices in <c t=\"s\"><v>N</v></c> cells."
    )
    return "\n".join(lines) + "\n"


def deduplicate(strings: list[str]) -> list[str]:
    """Remove duplicates while preserving first-occurrence order."""
    seen: set[str] = set()
    result: list[str] = []
    for s in strings:
        if s not in seen:
            seen.add(s)
            result.append(s)
    return result


def load_from_file(path: str) -> list[str]:
    """Read one string per non-empty line from a file."""
    with open(path, encoding="utf-8") as f:
        return [line.rstrip("\n") for line in f if line.strip()]


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Generate xl/sharedStrings.xml from a list of strings.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument(
        "strings",
        nargs="*",
        metavar="STRING",
        help="String values to include in the shared string table.",
    )
    parser.add_argument(
        "--file",
        "-f",
        metavar="PATH",
        help="Read strings from a file (one string per line) instead of arguments.",
    )
    parser.add_argument(
        "--index",
        action="store_true",
        help="Print a human-readable index table instead of XML output.",
    )
    args = parser.parse_args()

    if args.file:
        try:
            raw = load_from_file(args.file)
        except FileNotFoundError:
            print(f"ERROR: File not found: {args.file}", file=sys.stderr)
            sys.exit(1)
        except OSError as e:
            print(f"ERROR: Cannot read file: {e}", file=sys.stderr)
            sys.exit(1)
    else:
        raw = list(args.strings)

    if not raw:
        print(
            "ERROR: No strings provided.\n"
            "Usage: shared_strings_builder.py \"String1\" \"String2\" ...\n"
            "   or: shared_strings_builder.py --file strings.txt",
            file=sys.stderr,
        )
        sys.exit(1)

    strings = deduplicate(raw)

    if len(strings) < len(raw):
        removed = len(raw) - len(strings)
        print(
            f"Note: {removed} duplicate(s) removed. "
            f"{len(strings)} unique strings in table.",
            file=sys.stderr,
        )

    if args.index:
        print(build_index_table(strings))
    else:
        print(build_xml(strings), end="")


if __name__ == "__main__":
    main()
