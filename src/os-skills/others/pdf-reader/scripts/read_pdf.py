#!/usr/bin/env python3
"""PDF text extractor (PyMuPDF)."""
import argparse, json, os, sys

def _install():
    try:
        import pymupdf; return pymupdf
    except ImportError:
        pass
    try: import fitz; return fitz
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

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-f","--file",required=True)
    ap.add_argument("-p","--pages",default=None)
    ap.add_argument("-d","--metadata",action="store_true")
    ap.add_argument("-t","--tables",action="store_true",
                    help="report per-page table bboxes and cell rows (JSON output only)")
    ap.add_argument("--format",default="text",choices=["text","json"])
    ap.add_argument("-m","--max-length",type=int,default=0)
    a = ap.parse_args()
    if a.tables and a.format != "json":
        ap.error("--tables requires --format json")

    fitz = _install()
    if not os.path.exists(a.file):
        print(f"ERROR: {a.file} not found",file=sys.stderr); sys.exit(1)
    try:
        doc = fitz.open(a.file)
    except Exception as e:
        print(f"ERROR: cannot open {a.file}: {e}",file=sys.stderr); sys.exit(1)
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

    if a.format == "json":
        out = {"total_pages":n,"pages":pages}
        if meta: out["metadata"] = meta
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

    if a.max_length > 0 and len(r) > a.max_length:
        r = r[:a.max_length] + "\n...[truncated]"
    print(r)

if __name__ == "__main__":
    main()
