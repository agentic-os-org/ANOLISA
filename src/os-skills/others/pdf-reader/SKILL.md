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

Options: `-p "1-5,7"` page range, `--format json` structured output, `--metadata` include doc info, `-t/--tables` per-page table bboxes and cell rows (JSON only), `-m 8000` cap on the extracted text.

Tables: `--tables --format json` adds a `tables` array to each selected page: `{"bbox": [x0,y0,x1,y1], "rows": [[cell, ...], ...]}` from PyMuPDF table detection; pages without ruled tables report `"tables": []`. Plain page text and the default schemas are unchanged; the flag is rejected for text output.

Truncation: `-m N` caps the extracted page **text** at N characters in total, earlier pages first. It never slices the rendered document, so `--format json -m N` stays parseable at any cap; the budget does not cover JSON keys, page headers or `--tables` payloads. The page where the budget runs out ends with `...[truncated]`, later pages keep their number and report empty text, and JSON output gains a top-level `"truncated": true` (omitted when nothing was cut).

Setup: `pip install PyMuPDF`
