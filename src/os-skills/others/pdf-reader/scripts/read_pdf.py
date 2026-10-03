#!/usr/bin/env python3
"""PDF text extractor (PyMuPDF)."""
import argparse, json, os, sys

def _install():
    """Return the PyMuPDF module, installing it on first use.

    Prefer the modern `pymupdf` import name: importing through the legacy
    `fitz` alias makes PyMuPDF >= 1.24 print a deprecation warning on
    stdout, which corrupts --format json output.
    """
    try:
        import pymupdf
        return pymupdf
    except ImportError:
        pass
    try:
        import fitz
        return fitz
    except ImportError:
        import subprocess; subprocess.check_call([sys.executable,"-m","pip","install","-q","PyMuPDF"],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        try:
            import pymupdf
            return pymupdf
        except ImportError:
            import fitz
            return fitz

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

def _cap_pages(pages, limit):
    """Cap the extracted page text to `limit` characters, earlier pages first.

    Returns (pages, truncated). The first page that crosses the limit is
    cut mid-text and marked with an ellipsis; later pages are dropped.
    The marker itself is not counted against the limit. A limit <= 0
    disables capping.
    """
    if limit <= 0:
        return pages, False
    out = []
    used = 0
    truncated = False
    for p in pages:
        text = p["text"]
        room = limit - used
        if len(text) <= room:
            out.append(p)
            used += len(text)
            continue
        if room > 0:
            out.append({"page": p["page"], "text": text[:room] + "\n...[truncated]"})
        truncated = True
        break
    return out, truncated

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("-f","--file",required=True)
    ap.add_argument("-p","--pages",default=None)
    ap.add_argument("-d","--metadata",action="store_true")
    ap.add_argument("--format",default="text",choices=["text","json"])
    ap.add_argument("-m","--max-length",type=int,default=0,
                    help="Cap output at N characters. With --format json the cap applies to the extracted page text (earlier pages first; the ellipsis marker and JSON structure overhead are not counted) and a top-level \"truncated\": true flag is added when output was cut. With text format the final text is capped at N characters plus the marker.")
    a = ap.parse_args()

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
        pages.append({"page":i+1,"text":t})
    doc.close()

    if a.format == "json":
        pages, truncated = _cap_pages(pages, a.max_length)
        out = {"total_pages":n,"pages":pages}
        if truncated:
            out["truncated"] = True
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
