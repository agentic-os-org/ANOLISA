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

Options: `-p "1-5,7"` page range, `--format json` structured output, `--metadata` include doc info, `-t/--tables` per-page table bboxes and cell rows (JSON only), `-m 8000` max chars.

Tables: `--tables --format json` adds a `tables` array to each selected page: `{"bbox": [x0,y0,x1,y1], "rows": [[cell, ...], ...]}` from PyMuPDF table detection; pages without ruled tables report `"tables": []`. Plain page text and the default schemas are unchanged; the flag is rejected for text output.

Setup: `pip install PyMuPDF`

Use `--attachments --format json` to discover document-level embedded files by
zero-based physical index and SDK metadata. Use `--extract-attachment INDEX
--output PATH --format json` to recover a selected payload to an explicit new
file. Duplicate names remain selectable by index; embedded filenames never
choose an output path. Binary/UTF-8/empty bytes and the PDF source are preserved.
Existing paths are refused and publication requires hard-link support; the
output parent must exist. Discovery does not load payloads, extraction loads
only the selected payload in memory, and no attachment or action is executed.
