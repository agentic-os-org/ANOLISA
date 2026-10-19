#!/usr/bin/env python3
"""PDF text extractor (PyMuPDF)."""
import argparse, json, os, sys

TRUNCATION_MARKER = "...[truncated]"

def _import_legacy_fitz():
    """Import the legacy ``fitz`` alias without polluting stdout.

    Releases that still ship only the ``fitz`` name print an import advisory.
    This script writes its report to stdout, so an advisory printed during the
    import would prefix every report and make ``--format json`` unparseable.
    """
    saved = sys.stdout
    try:
        sys.stdout = sys.stderr
        import fitz
    finally:
        sys.stdout = saved
    return fitz

def _install():
    try:
        import pymupdf; return pymupdf
    except ImportError:
        pass
    try: return _import_legacy_fitz()
    except ImportError:
        import subprocess; subprocess.check_call([sys.executable,"-m","pip","install","-q","PyMuPDF"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        import pymupdf; return pymupdf

def _pages(spec, total):
    ps = set()
    for p in spec.split(","):
        p = p.strip()
        if "-" in p:
            a, b = p.split("-",1); [ps.add(i) for i in range(max(0,int(a)-1), min(total,int(b)))]
        else:
            i = int(p)-1
            if 0 <= i < total: ps.add(i)
    return sorted(ps)

def _page_tables(page):
    out = []
    for t in page.find_tables().tables:
        rows = [[("" if c is None else str(c)).strip() for c in row]
                for row in t.extract()]
        out.append({"bbox": [float(v) for v in t.bbox], "rows": rows})
    return out

def _cap_text(pages, limit):
    """Cap the total extracted page text at ``limit`` chars, earlier pages first.

    The budget counts extracted text only -- never the rendered structure -- so
    a JSON document stays parseable at every cap. The page where the budget
    runs out keeps its leading characters plus ``TRUNCATION_MARKER``; later
    pages keep their number and report empty text. Mutates ``pages`` in place
    and returns True when any text was dropped.
    """
    if limit <= 0:
        return False
    remaining, capped = limit, False
    for entry in pages:
        text = entry.get("text","")
        if remaining <= 0:
            if text:
                entry["text"] = ""
                capped = True
        elif len(text) <= remaining:
            remaining -= len(text)
        else:
            entry["text"] = text[:remaining] + TRUNCATION_MARKER
            remaining = 0
            capped = True
    return capped

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-f","--file",required=True)
    ap.add_argument("-p","--pages",default=None)
    ap.add_argument("-d","--metadata",action="store_true")
    ap.add_argument("-t","--tables",action="store_true",
                    help="report per-page table bboxes and cell rows (JSON output only)")
    ap.add_argument("--format",default="text",choices=["text","json"])
    ap.add_argument("-m","--max-length",type=int,default=0,
                    help="cap the extracted page text at N characters in total, earlier pages "
                         "first. The cap counts extracted text only (not JSON keys, page headers "
                         "or --tables payloads), so --format json stays parseable. The cut page "
                         f"ends with {TRUNCATION_MARKER!r}, later pages report empty text, and "
                         'JSON output gains "truncated": true')
    a = ap.parse_args()
    if a.tables and a.format != "json":
        ap.error("--tables requires --format json")

    fitz = _install()
    if not os.path.exists(a.file):
        print(f"ERROR: {a.file} not found",file=sys.stderr); sys.exit(1)
    doc = fitz.open(a.file)
    n = len(doc)
    idx = _pages(a.pages, n) if a.pages else list(range(n))

    meta = {}
    if a.metadata and doc.metadata:
        meta = {k:v for k,v in doc.metadata.items() if v}

    pages = []
    for i in idx:
        t = doc[i].get_text("text").strip()
        if not t:
            blocks = doc[i].get_text("blocks")
            t = "\n".join(b[4] for b in sorted(blocks,key=lambda b:(b[1],b[0])) if b[-1]==0).strip()
        entry = {"page":i+1,"text":t}
        if a.tables:
            entry["tables"] = _page_tables(doc[i])
        pages.append(entry)
    doc.close()

    truncated = _cap_text(pages, a.max_length)

    if a.format == "json":
        out = {"total_pages":n,"pages":pages}
        if meta: out["metadata"] = meta
        if truncated: out["truncated"] = True
        r = json.dumps(out,ensure_ascii=False,indent=2)
    else:
        parts = []
        if meta:
            parts.append("=== Metadata ===")
            parts.extend(f"  {k}: {v}" for k,v in meta.items())
            parts.append(f"  total_pages: {n}\n")
        for p in pages:
            parts.append(f"--- Page {p['page']} ---")
            parts.append(p["text"]); parts.append("")
        r = "\n".join(parts)
    print(r)

if __name__ == "__main__":
    main()
