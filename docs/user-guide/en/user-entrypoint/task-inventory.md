# Offline Benchmark Task Inventory

[中文版](../../zh/user-entrypoint/task-inventory.md)

The ClawEval task inventory reads local benchmark metadata without launching evaluations, models or services. Use CSV output to review a selection in a spreadsheet or another CSV reader.

## Export tasks

Use Python 3.11 or newer after task files are available under `benchmark/claweval-runner/claw-eval/tasks`:

```bash
cd benchmark/claweval-runner
python scripts/list_tasks.py --format csv > tasks.csv
python scripts/list_tasks.py --prefix T --difficulty hard --format csv > hard-tasks.csv
```

Prefix choices are `T`, `M` and `C`; difficulty choices are `simple`, `easy`, `medium`, `hard` and `expert`. CSV reuses grouped-view selection and retains ascending task-directory order. Task files are read as UTF-8 and remain unchanged.

## CSV reference

The header always contains these six columns in this order:

| Column | Value |
| --- | --- |
| `task_id` | Metadata identifier, which can differ from the directory name |
| `task_name` | Task metadata name |
| `difficulty` | Recorded difficulty, or `unknown` when absent |
| `prefix` | Uppercased first character of the identifier |
| `category` | Category metadata text |
| `tags` | Tag metadata text, such as `[general, review]` |

Missing name/category/tag values remain empty cells. The export retains existing metadata fields; it does not convert tag text into a normalized list.

Output is UTF-8 without a byte-order mark, even on an ASCII console, and uses standard CSV CRLF record endings. Commas, quotes and embedded CR/LF line breaks are quoted so a CSV reader recovers the original field text. Parse it as CSV rather than splitting lines or commas.

Empty roots or selections produce the header and zero data records. CSV never includes grouped headings, totals or “No tasks found” text. Task-directory errors remain on standard error with a nonzero exit.

## Grouped output

```bash
python scripts/list_tasks.py
python scripts/list_tasks.py --format grouped
```

Both commands retain the existing grouped display. `--format` selects one mode and defaults to `grouped`. CSV export adds no dependency and does not execute tasks.

See the [ClawEval runner overview](../../../../benchmark/claweval-runner/README.md) for setup and execution workflows.
