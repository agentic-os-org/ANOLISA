#!/usr/bin/env python3
"""PDF text extractor (PyMuPDF)."""

import argparse, json, os, sys
import tempfile
from pathlib import Path
from typing import Any


def _install():
    try:
        import pymupdf

        return pymupdf
    except ImportError:
        pass
    try:
        import fitz

        return fitz
    except ImportError:
        import subprocess

        subprocess.check_call(
            [sys.executable, "-m", "pip", "install", "-q", "PyMuPDF"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        import pymupdf

        return pymupdf


def _pages(spec, total):
    ps = set()
    for p in spec.split(","):
        p = p.strip()
        if "-" in p:
            a, b = p.split("-", 1)
            [ps.add(i) for i in range(max(0, int(a) - 1), min(total, int(b)))]
        else:
            i = int(p) - 1
            if 0 <= i < total:
                ps.add(i)
    return sorted(ps)


def _page_tables(page):
    out = []
    for t in page.find_tables().tables:
        rows = [[("" if c is None else str(c)).strip() for c in row] for row in t.extract()]
        out.append({"bbox": [float(v) for v in t.bbox], "rows": rows})
    return out


def _document_attachments(document: Any) -> list[dict[str, Any]]:
    return [
        {**document.embfile_info(index), "index": index}
        for index in range(document.embfile_count())
    ]


def _publish_attachment(payload: bytes, output: Path) -> None:
    if output.exists() or output.is_symlink():
        raise FileExistsError(f"Output already exists: {output}")
    with tempfile.TemporaryDirectory(prefix=".pdf-attachment-", dir=output.parent) as directory:
        staged = Path(directory) / "payload.bin"
        with staged.open("xb") as destination:
            destination.write(payload)
            destination.flush()
            os.fsync(destination.fileno())
        # Linking a completed sibling file publishes atomically without replacing a raced path.
        os.link(staged, output)


def _attachment_index(value: str) -> int:
    try:
        index = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(
            "Attachment index must be a non-negative integer"
        ) from error
    if index < 0:
        raise argparse.ArgumentTypeError("Attachment index must be a non-negative integer")
    return index


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-f", "--file", required=True)
    ap.add_argument("-p", "--pages", default=None)
    ap.add_argument("-d", "--metadata", action="store_true")
    ap.add_argument(
        "-t",
        "--tables",
        action="store_true",
        help="report per-page table bboxes and cell rows (JSON output only)",
    )
    ap.add_argument("--format", default="text", choices=["text", "json"])
    ap.add_argument("-m", "--max-length", type=int, default=0)
    ap.add_argument("--attachments", action="store_true", help="Discover embedded files in JSON")
    ap.add_argument(
        "--extract-attachment",
        type=_attachment_index,
        metavar="INDEX",
        help="Extract this zero-based embedded-file index to --output",
    )
    ap.add_argument("--output", metavar="PATH", help="New file for the selected attachment")
    a = ap.parse_args()
    if a.tables and a.format != "json":
        ap.error("--tables requires --format json")
    if (a.attachments or a.extract_attachment is not None) and a.format != "json":
        ap.error("Attachment discovery/extraction requires --format json")
    if a.extract_attachment is not None and a.output is None:
        ap.error("--extract-attachment requires --output")
    if a.output is not None and a.extract_attachment is None:
        ap.error("--output requires --extract-attachment")

    fitz = _install()
    if not os.path.exists(a.file):
        print(f"ERROR: {a.file} not found", file=sys.stderr)
        sys.exit(1)
    doc = fitz.open(a.file)
    n = len(doc)
    idx = _pages(a.pages, n) if a.pages else list(range(n))

    attachments = None
    payload = None
    try:
        if a.attachments or a.extract_attachment is not None:
            attachments = _document_attachments(doc)
        if a.extract_attachment is not None:
            if a.extract_attachment >= len(attachments):
                raise ValueError(f"Attachment index {a.extract_attachment} is not present")
            payload = doc.embfile_get(a.extract_attachment)
    except (ValueError, RuntimeError, OSError) as error:
        doc.close()
        print(f"ERROR: Cannot read PDF attachments: {error}", file=sys.stderr)
        sys.exit(1)

    meta = {}
    if a.metadata and doc.metadata:
        meta = {k: v for k, v in doc.metadata.items() if v}

    pages = []
    for i in idx:
        t = doc[i].get_text("text").strip()
        if not t:
            blocks = doc[i].get_text("blocks")
            t = "\n".join(
                b[4] for b in sorted(blocks, key=lambda b: (b[1], b[0])) if b[-1] == 0
            ).strip()
        entry = {"page": i + 1, "text": t}
        if a.tables:
            entry["tables"] = _page_tables(doc[i])
        pages.append(entry)
    doc.close()

    if a.format == "json":
        out = {"total_pages": n, "pages": pages}
        if meta:
            out["metadata"] = meta
        if attachments is not None:
            out["attachments"] = attachments
        if payload is not None:
            out["extracted_attachment"] = {
                "index": a.extract_attachment,
                "output": a.output,
                "size": len(payload),
            }
        r = json.dumps(out, ensure_ascii=False, indent=2)
    else:
        parts = []
        if meta:
            parts.append("=== Metadata ===")
            parts.extend(f"  {k}: {v}" for k, v in meta.items())
            parts.append(f"  total_pages: {n}\n")
        for p in pages:
            parts.append(f"--- Page {p['page']} ---")
            parts.append(p["text"])
            parts.append("")
        r = "\n".join(parts)

    if a.max_length > 0 and len(r) > a.max_length:
        r = r[: a.max_length] + "\n...[truncated]"
    if payload is not None:
        try:
            _publish_attachment(payload, Path(a.output))
        except OSError as error:
            print(f"ERROR: Cannot publish attachment: {error}", file=sys.stderr)
            sys.exit(1)
    print(r)


if __name__ == "__main__":
    main()
