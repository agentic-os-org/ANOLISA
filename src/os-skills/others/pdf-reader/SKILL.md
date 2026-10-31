---
name: pdf-reader
version: 1.0.0
description: "Extract text from PDF files. Use when reading, parsing, or analyzing PDFs."
metadata:
  requires:
    bins: ["python3"]
---

# PDF Reader

Run `scripts/read_pdf.py` relative to this skill's directory.

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f <pdf_path> [options]
```

Options: `-p "1-5,7"` page range, `--format json` structured output, `--metadata` include doc info, `-t/--tables` per-page table bboxes and cell rows (JSON only), `-m 8000` max chars, `--sort` extract text in spatial reading order instead of PDF content-object order (requires PyMuPDF >= 1.19.1).

Tables: `--tables --format json` adds a `tables` array to each selected page: `{"bbox": [x0,y0,x1,y1], "rows": [[cell, ...], ...]}` from PyMuPDF table detection; pages without ruled tables report `"tables": []`. Plain page text and the default schemas are unchanged; the flag is rejected for text output.

`--outline` includes the document-wide bookmark outline in `--format json` output as `[level, title, page]` entries (1-based pages, `-1` when a bookmark has no page destination); JSON only.

Setup: `pip install PyMuPDF`
