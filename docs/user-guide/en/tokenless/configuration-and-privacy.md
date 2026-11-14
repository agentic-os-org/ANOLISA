# Tokenless Configuration and Data Privacy

[中文版](../../zh/tokenless/configuration-and-privacy.md)

Tokenless enables compression, local statistics, and SLS metrics by default. Because local statistics and Stash may contain complete tool output or truncated original payloads, review these defaults before processing source code, credentials, or production logs.

## Configuration precedence

In the normal path, each toggle uses:

```text
Environment variable > ~/.tokenless/config.json > default
```

An empty environment variable is treated as unset. For Boolean environment variables, `1`, `true`, and `yes` are true, case-insensitively; any other non-empty value is false. Prefer explicit `true` or `false` values for readability.

There is one current implementation exception: when both `TOKENLESS_STATS_ENABLED` and `TOKENLESS_SLS_ENABLED` are non-empty, the config file is skipped completely. In that branch, compression uses `TOKENLESS_COMPRESSION_ENABLED` when set and otherwise defaults to `true`. If you export both recording variables, export the compression variable explicitly as well.

## Configuration file

Configuration path:

```text
~/.tokenless/config.json
```

Complete example:

```json
{
  "stats_enabled": true,
  "sls_enabled": true,
  "compression_enabled": true
}
```

A missing, unreadable, or invalid JSON file is silently replaced by the all-`true` defaults in memory. Validate a manually edited file with:

```bash
jq . ~/.tokenless/config.json
```

| Field | Default | Actual behavior |
|-------|---------|-----------------|
| `stats_enabled` | `true` | Writes complete before/after text and metrics to local SQLite |
| `sls_enabled` | `true` | Appends a metrics-only record when the target JSONL file exists |
| `compression_enabled` | `true` | Returns compressed output when true; false runs dry-run and returns the original |

When Tokenless writes the configuration, it restricts the mode to `0600`. Confirm the mode after creating it manually:

```bash
chmod 600 ~/.tokenless/config.json
```

The `stats` subcommands change only `stats_enabled`:

```bash
tokenless stats status
tokenless stats enable
tokenless stats disable
```

An environment override still wins after these commands. For example, `TOKENLESS_STATS_ENABLED=0 tokenless stats enable` saves `true` to the file, but recording remains disabled for processes that keep the environment override.

## Optional Git Diff context cropping

Git Diff cropping is enabled by default. Set `TOKENLESS_DIFF_COMPRESSION_ENABLED=0`
in the environment inherited by Tokenless or its host agent to disable it. Unset or empty
values retain the enabled default; `1`, `true`, and `yes` enable it case-insensitively,
and other non-empty values disable it. This option is independent of the general compression switch
and is not a `config.json` field. With cropping enabled,
`TOKENLESS_COMPRESSION_ENABLED=0` measures candidates but returns the original.

Rust callers use `RuntimeConfig.diff_compression_enabled`; Python callers use
`TokenlessConfig(diff_compression_enabled=False)` or the native `TokenlessRuntime`
keyword of the same name to disable it. Both default to true. `TokenlessConfig` inherits
the environment default, including in QwenPaw and AgentScope; explicit arguments take
precedence. The direct native Runtime uses its explicit constructor configuration.

The compressor handles complete ordinary Git diffs received as successful command
output. It requires a text replacement slot, an available Stash, and a supported
recovery method. All additions, deletions, metadata, and up to two available context
lines around changes are retained. Within each hunk, it may retain extra context
when splitting would add more header overhead. It only adopts output when both
characters decrease and the heuristic token estimate saves at least 16 tokens,
including the notice and recovery instruction. This estimate uses no runtime
tokenizer and does not guarantee a reduction for every model tokenizer.

The emitted operation is `diff_reduction` and recoverability is `retrievable`, not
`lossless`: unmodified context is omitted from the visible output. Follow the
emitted shell or tool instruction to retrieve the received original while it is in
Stash. Recovery requires an additional tool call. File-content results and RTK-owned
output bypass this compressor. Core releases RTK ownership only after complete receipts
confirm unchanged bytes; fallback still requires the host's declared recovery capability.
Common Hooks, Hermes and OpenClaw declare no recovery for RTK results, so their fallback
cannot use Diff reduction. Enabling this option does not change RTK command rewriting.
Unsupported or incomplete diffs pass through. Special file
sections such as renames and binary summaries retain their received bytes; encoded
binary patches pass through in full. Tokenless does not open host-persisted output
files to complete truncated diffs or change the host's truncation limit.

Local compression and original recovery have been verified on finite samples.
Stable whole-Agent token savings have not been established, so this feature remains
opt-in.

## HTML page rendering

HTML page rendering is enabled by default. Set `TOKENLESS_HTML_EXTRACTION_ENABLED=0`
in the environment inherited by Tokenless or its host agent to disable it. When unset
or empty it stays enabled; `1`, `true`, and `yes` also enable it (case-insensitively),
and any other value disables it. This option is independent of the general compression
switch and is not a `config.json` field. With rendering enabled,
`TOKENLESS_COMPRESSION_ENABLED=0` measures candidates but returns the original.

The SDKs and native bindings do not read this environment variable. To turn rendering
off there, Rust callers set `RuntimeConfig.html_extraction_enabled` to `false`, and
Python callers pass `TokenlessConfig(html_extraction_enabled=False)` or the native
`TokenlessRuntime` keyword of the same name.

The compressor handles complete HTML documents (starting with `<!doctype html` or
`<html>`) received as successful command output or API responses, for example a page
fetched with `curl` or returned by an MCP tool. It requires a text replacement slot,
an available Stash, and a supported recovery method. Text after the closing `</html>`
tag, such as a status code or the output of a following command, is kept after the
view. The page is rendered as a Markdown subset: headings, paragraphs, lists, tables,
fenced code with its language, links with targets, image alt text, quotes, and
admonitions. MathML formulas render as their TeX annotation or `alttext` between `$`
signs; cells spanning rows or columns leave empty cells in the slots they cover, rows
are not padded to the widest row, and a table's first header and footer groups are
placed where a browser draws them, at the top and bottom; ordered lists keep the
numbers a browser shows, following `start`, `reversed` and an item's `value`;
paragraph lines that start like a Markdown heading, list item, quote, rule or code
fence are escaped. The content root is `<main>`, an element with `role=main`, or the
only outermost `<article>` (articles nested inside it, such as comments, do not
count); otherwise the whole body.
Scripts, styles, `noscript`, templates, SVG, iframes, comments, `nav`, `aside`,
page-level `header`/`footer`, elements with navigation, banner, contentinfo, or
complementary roles, form controls (`button`, `input`, `select`, `textarea`,
`datalist`, `progress`, `meter`), media embeds (`audio`, `video`, `canvas`, `object`,
`embed`, `map`), and `dialog` are removed; `menu` renders as a list, and `label`,
`legend`, and `fieldset` stay because content tabs keep their titles there. The first line of the view names
the root, the number of nodes omitted outside it, and every removal count by
category. Pages whose
rendered body is shorter than 64 characters, such as application shells, pass through,
and so do pages whose markup nests deeper than 512 elements: HTML parsing time grows
quadratically with nesting depth, so such pages are not parsed at all.
It only adopts output when both characters decrease and the heuristic token estimate
saves at least 16 tokens, including the notice and recovery instruction.

The emitted operation is `html_extraction` and recoverability is `retrievable`, not
`lossless`: markup and the removed elements are not in the visible output. Follow the
emitted shell or tool instruction to retrieve the received original while it is in
Stash. Content that a page loads through scripts is not visible in the view. Content
origin is classified by the adapters: file read tool results pass through, and shell
tool results carry the command line, from which Core reports commands that only print
local files (`cat`, `head`, `tail`, `nl`, `less`, `more`, `bat`, and `sed -n` with a
print-only script, optionally after `cd … &&`) as `file_read`. JSON, CSV, build logs
and diffs in such output still compress, but a printed HTML page is source the agent
may edit and stays verbatim. A read combined with a pipe, redirection or another
command, and a page returned by an MCP file tool, is still rendered like a fetched page
and must be retrieved to see its source.

Local rendering and original recovery have been verified on finite samples.

### HTML inside JSON values

The same switch also covers HTML carried inside JSON results, such as an API record
whose `content` or `body` field holds rich text. Field names play no part: a string
value is considered when it is at least 256 characters long, starts with a supported
tag, a comment, or `<!doctype html`, and ends with `>`. Results from printed local
files (`file_read`) keep their HTML verbatim, as above. A text replacement slot is not
required, because the result stays JSON. An available Stash and a supported recovery
method are required.

Only the value changes. Field names, other values, JSON types, exact number literals,
and the position of the value in its record are kept. The new value starts with a line
such as `[HTML rendered as Markdown; removed 1 style. If needed, run in shell: tokenless
retrieve <hash>]`, followed by the Markdown. The instruction retrieves that one value
exactly as received. Serialized JSON inside a string is never parsed or turned into an
object.

Unlike a page, a value is rendered whole and nothing visible is removed. Only style
sheets and comments are removed and counted in the first line, and cosmetic styling
is not printed. Line breaks from `br` and block elements are kept, also inside code.
Button labels stay as separate text, images without alt text and links without text
keep their targets, and a link or image tooltip (`title`) becomes its Markdown title.
A value passes through unchanged when the view could change what a reader sees or
lose a fact. Styling cannot be printed, so a value is rewritten only when no text
the view would show is hidden or illegible on screen:

- Form fields and their state (`input`, `select`, `textarea`, `form`, `label`).
- Scripts, templates, media, frames, SVG, MathML, and elements outside HTML.
- Page chrome (`nav`, `header`, `footer`, `aside`, or a navigation, banner,
  contentinfo, or complementary role).
- `del`, `ins`, `s`, `sub`, `sup`, `q`, `time`, `data`, `bdo`, `base`, and `link`.
- Styles in a `style` attribute or a `<style>` sheet of the value that do more than
  change colors, fonts, alignment, borders, sizes, or spacing. This covers
  strike-through, raised or lowered text, hidden or generated content, list markers,
  preserved line breaks, symbol fonts such as Wingdings, negative or very wide letter
  or word spacing, and properties, values, or functions such as `calc()` that the
  check cannot read. Selectors are not matched, so a `<style>` rule must pass for any
  element it could reach. The `hidden` attribute also passes through.
- Text that its styling makes hard or impossible to see. Colors, sizes, and line
  heights are followed through the value's inline styles, presentational attributes
  such as `bgcolor` or a `font` element's `color`, its `<style>` sheets and column
  backgrounds, and a browser's defaults for links, marks, and buttons. Any text with a
  contrast ratio below 1.5 against a color it may sit on (white on white, or text the
  color of the page) passes the value through, as does text over a background image or
  gradient, smaller than 6 pixels, or with lines closer than half its font size. So do
  a fixed width or height that text could overflow onto other content, and an inline
  element's vertical padding or border taller than 8 pixels, or a text decoration with
  a set thickness, either of which can paint over neighboring lines. The page behind
  the value is taken to be a browser's default: white, with black 16-pixel text.
  Stylesheets outside the value, such as those of the application that displays it,
  its dark mode, or fonts it loads, are unknown and not considered. Margins or padding
  may move text right or down, where a page scrolls to it; whether the displaying
  application clips it there is also unknown.
- Attributes the view cannot show, such as `data-*` values, `aria-*` labels and
  states, a tooltip on elements other than links and images, `abbr` on table
  headers, a quote's `cite`, a download file name, or a button's value or state.
  Text inside an `aria-hidden` element also passes through, because it may be an
  icon ligature.
- Metadata the view would drop: a `meta` other than a character set, content type,
  compatibility mode, viewport, or mail formatting hint (so a description or a
  refresh passes through), and a `title` outside the head or after the first.
- Content a browser shows only after the reader acts: the body of a closed
  `details`. A `details` without a `summary`, or grouped by `name`, also passes
  through.
- Ordered lists whose numbers the view could not reproduce: reversed or lettered
  lists, a `start` other than a plain non-negative number, items with an explicit
  `value`, empty items, or content between items.
- Tables nested in a cell, cells containing line breaks, or a row of headers anywhere
  but the top, such as a second header row or a section label, since a Markdown table
  has one header row.
- `javascript:` or `data:` targets, any text or target that would not appear in the
  view in the order a browser draws it, or text a browser shows apart that the view
  would run together.

Each rewritten value must save at least 16 estimated tokens in its JSON-escaped form,
including its first line, and the rewritten document must be smaller than the compact
JSON or TOON result without rewrites. That comparison happens before the 15% lossless
threshold ends the search, so compact formatting no longer hides the markup inside
fields. When the result is also reduced (records omitted or strings bounded), a value
whose view exceeds the string limit is truncated instead. Work is bounded per result:
values up to 256 KiB, nested at most 32 levels deep, at most 128 parsed values and
2 MiB of parsed HTML. The legibility check compares each pair of text and background
colors once, and a value that needs more than 65,536 distinct comparisons passes
through. A value repeated in several records is rendered and stashed once.

The response reports `html_extraction` with content type `json` and recoverability
`retrievable`. Statistics therefore tell embedded fragments (`json`) from rendered
pages (`html`). These rules have been verified with synthetic fragments and documents;
they are guards, not proof that every fragment keeps its meaning for a model.

## Optional mixed-content region compression

Mixed-content region compression is disabled by default. Set
`TOKENLESS_MIXED_CONTENT_ENABLED=1` in the environment inherited by Tokenless or its
host agent to enable it; unset the variable or set it to `0` to disable it. `1`,
`true`, and `yes` enable it (case-insensitively). This option is independent of the
general compression switch and is not a `config.json` field. With it enabled,
`TOKENLESS_COMPRESSION_ENABLED=0` measures candidates but returns the original.

Python `TokenlessConfig` reads this environment variable by default; an explicit
`mixed_content_enabled` argument takes precedence. Rust and direct native binding
callers set `RuntimeConfig.mixed_content_enabled` to `true` or pass the native
`TokenlessRuntime(mixed_content_enabled=True)` keyword.

The route handles tool outputs that are neither a whole structured document nor
handled by another domain. It recognizes explicitly bounded typed regions — fenced
code blocks labeled `json`, `csv`, or `tsv` (indented at most three spaces, closed by
a fence of the same marker at least as long), and bare JSON documents that start with
`{` or `[` within the first three columns and balance their brackets — and compresses
each region with the same compressor and policy as the standalone route. Prose,
logs, and separators around regions stay byte-for-byte; a region the compressor
rejects stays verbatim; a fenced JSON region that selects TOON has its fence label
rewritten to `toon` (only the info word is replaced; the remaining attributes keep
their text and order); an unclosed fence or an unbalanced document protects its
remainder. Regions are recognized only inside the same 64 KiB / 200-line detection
window the other domains use; a region opening past it is preserved verbatim. Regions
run only when the output is neither from Grep nor a recognized shell `grep`/`rg`
command and the host can replace output with text. Search output stays excluded even
when search path sharing is disabled. The whole candidate still passes the pipeline's
character and token arbitration. A candidate with no changed region, no tentative
Stash write, and no Stash error passes through; an unchanged candidate carrying
tentative writes reports `no_savings` and rolls them back. Any region's Stash write
failure rolls back all tentative writes from the request, including successful writes
from other regions, before the runtime reports the storage error.

Local region compression and reconstruction have been verified on finite samples.

## Environment variables

### Common user variables

| Variable | Purpose | Constraint |
|----------|---------|------------|
| `TOKENLESS_STATS_ENABLED` | Override local statistics | Does not affect SLS or Stash |
| `TOKENLESS_SLS_ENABLED` | Override SLS metrics | Does not affect local statistics |
| `TOKENLESS_COMPRESSION_ENABLED` | Override active compression | False is dry-run, not a full stop |
| `TOKENLESS_DATA_DIR` | Directory containing `stats.db`, `stash.db` and `rtk-outcomes.db` | Any accessible absolute directory except filesystem root; no parent traversal |
| `TOKENLESS_STATS_DB` | Override the statistics database | Must be under the real user home or selected data directory |
| `TOKENLESS_STASH_DB` | Override the Stash database | Must be under the real user home or selected data directory |
| `TOKENLESS_SLS_PATH` | Override the SLS JSONL path | Must be under `/var/log/` or `/tmp/` |
| `TOKENLESS_DIFF_COMPRESSION_ENABLED` | Enable Git Diff context cropping | On by default; `0` disables; explicit SDK options take precedence |
| `TOKENLESS_HTML_EXTRACTION_ENABLED` | Switch HTML page rendering, including HTML inside JSON values | On by default; `1`, `true`, or `yes` enables, any other non-empty value disables; does not override SDK options |
| `TOKENLESS_MIXED_CONTENT_ENABLED` | Enable mixed-content region compression | Off by default; `1`, `true`, or `yes` enables; does not override SDK options |
| `TOKENLESS_SEARCH_PATH_SHARING_ENABLED` | Switch search-path sharing for numbered API search records, recognized native shell searches, and pure path lists from commands or APIs; every received path and byte is retained | On by default; `1`, `true`, or `yes` enables, any other non-empty value disables; does not override SDK options |

### Adapter and diagnostic variables

| Variable | Purpose |
|----------|---------|
| `TOKENLESS_AGENT_ID` | Agent identifier injected by an adapter |
| `TOKENLESS_SESSION_ID` | Session identifier injected by an adapter |
| `TOKENLESS_TOOL_USE_ID` | Tool-call identifier injected by an adapter |
| `TOKENLESS_TRACEPARENT` | W3C trace context override stamped onto SLS records |
| `TRACEPARENT` | Standard W3C trace context variable, injected by the launching host or adapter |
| `TOKENLESS_TOOL_READY_SPEC` | Override the Tool Ready dependency specification |
| `TOKENLESS_ENV_FIX_SCRIPT` | Override the environment repair script |
| `TOKENLESS_PACKAGE_MANAGER` | Override package-manager detection, mainly for tests |

Tool Ready is hard-disabled in this build. Its specification and repair-script overrides are retained for the dormant legacy implementation but have no runtime effect. They are subject to trusted-path validation and are not recommended for normal users.

`TOKENLESS_TRACEPARENT` and the standard `TRACEPARENT` carry a W3C trace context for SLS records. Injecting one is the launcher's job: OpenTelemetry propagates W3C context through in-process carriers and does not export the active span as a process variable, so a host or adapter that wants correlation has to set one of these two variables in the environment it spawns Tokenless with. Tokenless only reads them. The override is read first, and an empty or unparsable override falls back to the standard variable so one typo cannot drop correlation for a whole session. Both are optional: when neither carries a usable context, records keep the previous shape and are written uncorrelated. The identity is stamped only onto the SLS JSONL — the local statistics database does not store it.

Database path priority is:

- Stats: `TOKENLESS_STATS_DB` > `DSH_TOKENLESS_STATS_DB` (only while `DSH_SHELL=1`) > `TOKENLESS_DATA_DIR/stats.db` > `DSH_TOKENLESS_DATA_DIR/stats.db` (only while `DSH_SHELL=1`) > `~/.tokenless/stats.db`
- Stash: `--stash-db` > `TOKENLESS_STASH_DB` > `DSH_TOKENLESS_STASH_DB` (only while `DSH_SHELL=1`) > `TOKENLESS_DATA_DIR/stash.db` > `DSH_TOKENLESS_DATA_DIR/stash.db` (only while `DSH_SHELL=1`) > `~/.tokenless/stash.db`
- RTK receipts: `TOKENLESS_DATA_DIR/rtk-outcomes.db` > `DSH_TOKENLESS_DATA_DIR/rtk-outcomes.db` (only while `DSH_SHELL=1`) > `~/.tokenless/rtk-outcomes.db`

RTK receipts are independent of the statistics switch. They store the original and exact executed commands,
agent/session/tool identifiers, execution generation and output-change flags, not tool-output
text. PostTool consumes them once only when its command matches the registered execution. Unconsumed receipts expire after one day and
are deleted on a subsequent rewrite. An unusable receipt directory disables this fallback;
PostTool retains RTK ownership and returns the original output.

`TOKENLESS_DATA_DIR` is an explicit directory-level relocation and may point outside the real user home, including to a managed service directory under `/var/lib`. Both the CLI and bundled RTK writer reject filesystem root, relative paths, parent traversal, and existing non-directory targets. Without a valid higher-priority file override, an invalid explicit data directory disables that operation's SQLite state instead of silently falling back to home.

An empty value is treated as unset. `TOKENLESS_DATA_DIR` may name a directory that does not exist yet; Tokenless canonicalizes its nearest existing ancestor before creating it. File-level overrides are accepted only beneath the canonical real home or selected data directory, and existing database symlinks are rejected. `TOKENLESS_DATA_DIR` does not relocate `~/.tokenless/config.json` or the SLS JSONL output.

DeepSeek Harness is an exception to the default database location because its
sandbox removes inherited `TOKENLESS_*` variables and may not expose the home
directory. Its adapter uses `.tokenless` in the session workspace unless
`TOKENLESS_DATA_DIR` is set, and publishes managed shell aliases for that
directory plus the inherited `TOKENLESS_STATS_DB` and `TOKENLESS_STASH_DB`
values, under the `DSH_TOKENLESS_*` names below. The default
workspace directory contains a `.gitignore` with `*`, so complete tool text,
Stash payloads, and SQLite sidecars are not staged by `git add -A`. Custom paths
are not modified; make them accessible to the DSH shell sandbox and exclude
them from source control or backups as required by your data policy.

The DSH sandbox sets a sentinel variable, and the adapter publishes
overrides for the managed shell environment within the sandbox:

| Variable | Purpose | Constraint |
|----------|---------|------------|
| `DSH_SHELL` | Sentinel set by the DeepSeek Harness sandbox; the `DSH_TOKENLESS_*` overrides below take effect only while its value is `1` | Set by the DSH host, not published by the adapter |
| `DSH_TOKENLESS_DATA_DIR` | Override the data directory the managed shell environment uses | Only takes effect while `DSH_SHELL=1`; any accessible absolute directory except filesystem root; no parent traversal |
| `DSH_TOKENLESS_STATS_DB` | Override the statistics database the managed shell environment uses | Only takes effect while `DSH_SHELL=1`; must be under the real user home or selected data directory |
| `DSH_TOKENLESS_STASH_DB` | Override the Stash database the managed shell environment uses | Only takes effect while `DSH_SHELL=1`; must be under the real user home or selected data directory |

The DSH sandbox strips inherited `TOKENLESS_*` variables, so the adapter
publishes the `DSH_TOKENLESS_*` overrides into the managed shell
environment; the Tokenless CLI reads them on every invocation, and only
while `DSH_SHELL=1`. `DSH_SHELL` itself comes from the DSH host and is not
published by the adapter.

## Local and external data

| Data | Default path | Default content | Retention | Stop new data |
|------|--------------|-----------------|-----------|---------------|
| Local statistics | `~/.tokenless/stats.db` | Complete before/after text, identifiers, and metrics | No automatic TTL; retained until cleared | `tokenless stats disable` |
| Stash | `~/.tokenless/stash.db` | Original strings, dropped middle segments of truncated arrays, complete object record arrays reduced to a sampled subset, deep subtrees, schema descriptions removed by truncation, build/log gaps, the complete original of rendered HTML pages, and each HTML value rendered inside JSON | One-hour TTL and 10,000 live entries; expired rows are purged lazily | CLI: `--no-stash`; agent: disable the adapter |
| Configuration | `~/.tokenless/config.json` | Three Boolean toggles | Persistent | Not applicable |
| SLS JSONL | `/var/log/anolisa/sls/ops/tokenless.jsonl` | Metrics and identifiers, no compressed source text | Managed by SLS/Logtail infrastructure | `TOKENLESS_SLS_ENABLED=0` or config false |

### Sensitivity of local statistics

`before_text` and `after_text` in `stats.db` preserve complete content. `tokenless stats show` prints that content, while record-level and tool-use-level `tokenless stats diff` commands can render changed lines from it. They may contain:

- Source code and patches.
- Paths, user names, or environment details from command output.
- Business data returned by an API.
- Access tokens, cookies, or credentials found in logs.

The `tokenless` CLI's SQLite recorder attempts to set `stats.db` to `0600` whenever it opens the database. The bundled RTK statistics patch can create or open the same file directly and does not apply that permission change itself. Do not rely on the process umask; verify the deployed database and sidecars:

```bash
ls -l ~/.tokenless/stats.db*
```

### Sensitivity of Stash

Stash saves the original content removed by truncation, not a summary. For record reduction, the stashed entry is the complete original array before reduction, not only the omitted records. JSON fields are retained regardless of their names or empty values; their names and empty values do not trigger Stash writes. Explicit truncation and record reduction can still write to Stash. The `tokenless` CLI restricts its path to the real user home or selected data directory, but also verify that the database and SQLite sidecar files are not readable by other local users:

```bash
ls -l ~/.tokenless/stash.db*
```

TTL means that `retrieve` no longer returns an entry after one hour. Expired rows are deleted lazily during a later successful stash write or retrieval, so compression-only workloads also reclaim old entries. TTL is not an immediate secure-erasure guarantee for disk data, and deleting rows does not immediately shrink the SQLite file. When more than 10,000 live entries exist, the store evicts entries with the earliest expiry first, so retrieval can fail before one hour under heavy use.

### SLS excludes original text

Tokenless SLS JSONL includes the component, operation, session/tool-use identifiers, the host trace identity when one was propagated, and character/token metrics. It does not include `before_text` or `after_text`. Identifiers can still be organizational runtime metadata and should follow the platform's log policy.

## Guidance for sensitive workloads

JSON compression retains diagnostic fields such as `debug`, `trace`, and `logs`.
The host may send these values to its configured model endpoint. If those fields
contain credentials or internal details, redact them at the tool or host boundary
before model submission. The former name-based deletion was not a reliable
sanitizer; disabling compression or Stash also does not redact outgoing content.

### Compress without recording

```bash
TOKENLESS_STATS_ENABLED=0 \
TOKENLESS_SLS_ENABLED=0 \
  tokenless compress-response --no-stash -f response.json
```

This applies to standalone CLI use. Agent adapters may use Stash by default. If the framework does not provide an appropriate exclusion rule, disable the adapter for sensitive tasks.

### Keep the adapter but do not apply compression

Set this in the environment used to start the agent:

```bash
export TOKENLESS_COMPRESSION_ENABLED=0
```

This is a dry-run and may still write local statistics or SLS. To avoid persistence, also set:

```bash
export TOKENLESS_STATS_ENABLED=0
export TOKENLESS_SLS_ENABLED=0
```

Dry-run does not create Stash entries, but it also does not disable RTK rewriting. Tool Ready is independently hard-disabled. Disable the adapter when all hook behavior must stop.

### Stop Tokenless completely in an agent

```bash
anolisa adapter disable tokenless <framework>
```

Restart the agent afterwards. Setting only `compression_enabled=false` does not stop hook or plugin execution.

## Clear data

Clear local statistics records:

```bash
tokenless stats clear --yes
```

This clears records from the statistics database resolved in the current environment, but it does not remove the database file or SQLite sidecars. Tokenless currently has no Stash clear subcommand. For irreversible local-database removal:

1. Disable every Tokenless adapter.
2. Exit agents and Tokenless processes that may still use the databases.
3. Confirm that statistics history and Stash retrieval are no longer needed.
4. Back up anything that must be retained.
5. Inspect path overrides in the actual environment used to start the agent, service, and Tokenless:

```bash
env | grep -E '^TOKENLESS_(DATA_DIR|STATS_DB|STASH_DB)='
```

The statistics path resolves in this order: `TOKENLESS_STATS_DB`, `DSH_TOKENLESS_STATS_DB` (only while `DSH_SHELL=1`), `TOKENLESS_DATA_DIR/stats.db`, `DSH_TOKENLESS_DATA_DIR/stats.db` (only while `DSH_SHELL=1`), then `~/.tokenless/stats.db`. The Stash path resolves in this order: command-line `--stash-db`, `TOKENLESS_STASH_DB`, `DSH_TOKENLESS_STASH_DB` (only while `DSH_SHELL=1`), `TOKENLESS_DATA_DIR/stash.db`, `DSH_TOKENLESS_DATA_DIR/stash.db` (only while `DSH_SHELL=1`), then `~/.tokenless/stash.db`. Write the final values as verified absolute paths; do not expand untrusted environment values directly into a removal command.

The following command works for default and custom paths. Replace and print both paths first, then confirm that they are the Tokenless databases to remove:

```bash
stats_db='/absolute/path/to/resolved/stats.db'
stash_db='/absolute/path/to/resolved/stash.db'
printf '%s\n' "$stats_db" "$stash_db"
rm -f -- \
  "$stats_db" \
  "$stats_db-wal" \
  "$stats_db-shm" \
  "$stats_db-journal" \
  "$stash_db" \
  "$stash_db-wal" \
  "$stash_db-shm" \
  "$stash_db-journal"
```

This cannot be undone. Do not recursively remove the data directory or `~/.tokenless/` because either location may contain configuration or other files that you want to keep.

## Fine-grained OpenClaw control

The OpenClaw plugin also provides framework-level options:

| Option | Purpose |
|--------|---------|
| `rtk_enabled` | Rewrite supported shell commands through RTK |
| `tool_ready_enabled` | OpenClaw-side Tool Ready registration gate |
| `post_tool_enabled` | Replace supported live tool text before the model request |
| `verbose` | Plugin diagnostic logging |

The OpenClaw plugin does not compress tool schemas or provide content retrieval. Results that cannot
be safely optimized without retrieval pass through unchanged.

RTK, the OpenClaw-side Tool Ready registration gate, and PostTool default to on; verbose logging
defaults to off. The Tool Ready option currently has no operational effect because Tokenless
hard-disables the underlying check. Tokenless automatically decides whether JSON compaction or TOON is
useful and which tool outputs must pass through unchanged. The removed
`response_compression_enabled`, `toon_compression_enabled`, `skip_tools`, and `shell_tools` keys no
longer control the adapter.

These values are managed by OpenClaw plugin configuration, not `~/.tokenless/config.json`. The
adapter requires OpenClaw 2026.6.6 or later and explicit plugin enablement. Its manifest declares
`contracts.agentToolResultMiddleware: ["openclaw"]`. Unsupported or unrecognized versions are
rejected before installation. Missing runtime middleware disables PostTool with a warning rather
than using transcript-only compression. Restart the gateway as instructed after
changing them. On legacy hosts, registering middleware can itself truncate results before
Tokenless receives them; fail-open cannot undo that preprocessing. Supported hosts remain
usable within this limit. See the [OpenClaw compatibility notes](framework-integration.md#openclaw)
for the recommended upstream fix. Set `post_tool_enabled: false` to stop Tokenless from
registering result middleware while leaving RTK independently configurable.

## Related documents

- [Measuring savings](measuring-savings.md)
- [CLI reference](cli-reference.md)
- [Agent integration](framework-integration.md)
- [Troubleshooting](troubleshooting.md)
