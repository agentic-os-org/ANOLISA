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

For a protected PDF, supply its known password with `--password`, or use
`--password-env NAME` to read it from an environment variable. These options are
mutually exclusive. Missing or incorrect passwords produce a nonzero exit and
an error on stderr before document text is extracted.

```bash
python3 SKILL_DIR/scripts/read_pdf.py -f protected.pdf --password-env PDF_PASSWORD -p 1-3
```

Set `PDF_PASSWORD` in the invoking environment before running this example.

Setup: `pip install PyMuPDF`
