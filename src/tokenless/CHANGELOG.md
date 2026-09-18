# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to Tokenless will be documented in this file.

Releases from 0.7.2 onward follow
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.8.2] - 2026-09-15

### Added

- Supported search listings, including Claude Code native `Grep` results without context lines, now share consecutive file paths while retaining every received match, line number, and line ending. This lossless optimization is enabled by default and requires a text replacement slot; disable it with `TOKENLESS_SEARCH_PATH_SHARING_ENABLED=0` or SDK `search_path_sharing_enabled=False` ([#3173](https://github.com/alibaba/anolisa/pull/3173)).

### Changed

- Bundled RTK is upgraded to 0.49.0, with conservative pipeline rewriting and unchanged `sudo` commands. Direct `rtk grep` users must use `--max-len` and `--max` for RTK display limits: `-l` and `-m` now retain native grep meanings, and file-type filtering moves to `rtk rg -t` ([#3273](https://github.com/alibaba/anolisa/pull/3273)).
- Supported RTK filters now provide `rtk recall HASH` hints for retained failure or truncated output. Recovery storage is scoped to the host OS user, with limits and expiry; sessions sharing that user can access the same store. RTK recall and Tokenless Stash retrieval remain separate ([#3273](https://github.com/alibaba/anolisa/pull/3273)).

### Fixed

- Cosh-NG and copilot-shell now correctly attribute failures carried inside JSON-encoded shell results under Protocol v2, preserving the original failed output and sending error details for diagnosis ([#2238](https://github.com/alibaba/anolisa/pull/2238)).
- Bundled RTK preserves pytest startup and fallback diagnostics, including when recovery storage is disabled, and keeps the no-tests summary alongside stderr ([#3273](https://github.com/alibaba/anolisa/pull/3273)).

## [0.8.1] - 2026-09-09

### Added

- CSV/TSV tool output can now preserve all cells through quoting and line-ending compaction, or reduce larger tables to boundary rows, diagnostic rows, and a representative sample when recovery is available. Reduced views identify omitted rows and provide byte-exact original retrieval; complete enumeration or calculations require the original table. File reads and hosts without a text replacement slot pass through unchanged ([#3089](https://github.com/alibaba/anolisa/pull/3089)).
- SLS records can now carry `tokenless.trace_id` and `tokenless.span_id` from `TOKENLESS_TRACEPARENT` or `TRACEPARENT`, so observability backends can correlate token savings with a host trace. The launching host or adapter must inject the context; without a usable value, records remain uncorrelated, and local `stats.db` data is unchanged ([#3094](https://github.com/alibaba/anolisa/pull/3094)).

### Fixed

- Claude Code detection now briefly re-lists a staged plugin missing from the first successful registry scan, avoiding false “not installed” results immediately after installation ([#3085](https://github.com/alibaba/anolisa/pull/3085)).
- Hermes now skips trusted shared hook modules whose APIs or call signatures are incompatible with the adapter, tries later compatible candidates, and reports rejected candidates when none work. This prevents stale installations or cached modules from breaking lifecycle hooks ([#2249](https://github.com/alibaba/anolisa/pull/2249)).
- OpenClaw installation now accepts declared capabilities when the host supports that option and sends the unsafe-install flag only when the host advertises it as effective. Hosts marking it as a no-op receive no bypass flag, and rejected installs point operators to `security.installPolicy` ([#3126](https://github.com/alibaba/anolisa/pull/3126), [#3152](https://github.com/alibaba/anolisa/pull/3152)).

## [0.8.0] - 2026-09-06

### Added

- Large JSON record arrays can now retain representative records, errors, unusual shapes, and numeric outliers under a 32-record base budget, with the complete collection available through Stash. Compact JSON and TOON candidates that save at least 15% take priority over record reduction ([#3047](https://github.com/alibaba/anolisa/pull/3047)).
- Recognized Cargo, pytest, npm/Jest, Go, Make/C, and generic build/test logs can now drop repeated routine progress while preserving diagnostics, summaries, phases, and stack traces. Supported build/test commands keep their native output for this compression, and omitted intervals remain retrievable when recovery is available ([#3067](https://github.com/alibaba/anolisa/pull/3067)).
- Cosh-NG, Claude Code, Qoder CLI, OpenCode, Hermes, and DeepSeek Harness can now recover omitted content through their existing shell tools. Successful standalone `tokenless retrieve HASH` results bypass further compression; recovery requires a reachable CLI and a host that can replace the current tool result, including Claude Code 2.1.121 or newer ([#3052](https://github.com/alibaba/anolisa/pull/3052)).
- A QwenPaw plugin now provides in-process schema compression, RTK command rewriting, response/TOON compression, and a static `tokenless_retrieve` tool. Installation selects the matching SDK wheel, and tools outside the supported built-in table pass through unchanged ([#3075](https://github.com/alibaba/anolisa/pull/3075)).

### Changed

- **Breaking:** `tokenless compress` now accepts only Protocol v2 `before_model`, `pre_tool`, `post_tool`, and `retrieve` requests; Protocol v1 and `tokenless mcp serve` have been removed. Core, adapters, and custom callers must upgrade together, including the required explicit recovery capability ([#2978](https://github.com/alibaba/anolisa/pull/2978), [#3068](https://github.com/alibaba/anolisa/pull/3068)).
- **Breaking:** Rust callers now use the Runtime lifecycle methods in place of `TokenlessRuntime::compress`. Direct response-compression callers must migrate from the removed `tokenless-pipeline` crate and `tokenless_schema::ResponseCompressor` to the Runtime or `tokenless-compressors` APIs ([#2974](https://github.com/alibaba/anolisa/pull/2974), [#2978](https://github.com/alibaba/anolisa/pull/2978)).
- **Breaking:** The Python SDK now uses typed lifecycle requests with `before_model`, `pre_tool`, `post_tool`, and `retrieve`; the old `ModelRequest`, `ToolCall`, `ToolResult`, and `ToolResponseCompressor` APIs have been removed. AgentScope integrations require `ToolContract` metadata for custom tools and expose one static retrieval tool ([#2986](https://github.com/alibaba/anolisa/pull/2986), [#3029](https://github.com/alibaba/anolisa/pull/3029)).
- Recovery instructions now name the available shell command or configured static tool and use a bare hash, so agents can choose the correct recovery action. Historical `<<tokenless:HASH>>` markers remain readable; custom `retrieve_tool_name` settings must satisfy the tool-name rules ([#3068](https://github.com/alibaba/anolisa/pull/3068)).
- Lifecycle schema compression now requires an authorized static recovery tool and an available Stash. Common BeforeModel hooks without that capability preserve the original schemas ([#2978](https://github.com/alibaba/anolisa/pull/2978), [#2995](https://github.com/alibaba/anolisa/pull/2995), [#3029](https://github.com/alibaba/anolisa/pull/3029)).
- Lifecycle compression now preserves declared file contents and results already optimized by RTK. Common hooks carry per-call optimization state, and hooks without a stable tool-call ID leave command arguments unchanged ([#2974](https://github.com/alibaba/anolisa/pull/2974), [#2978](https://github.com/alibaba/anolisa/pull/2978), [#2995](https://github.com/alibaba/anolisa/pull/2995)).
- **Breaking:** OpenClaw now uses `post_tool_enabled` for PostTool optimization; its former response, TOON, skip-tool, and shell-tool policy settings have been removed. DeepSeek Harness also uses the shared compression policy instead of adapter-specific thresholds and tool lists. OpenClaw remains limited to lossless transcript updates without same-turn replacement or retrieval ([#3009](https://github.com/alibaba/anolisa/pull/3009), [#3036](https://github.com/alibaba/anolisa/pull/3036)).
- Hermes now delegates PreTool rewriting and PostTool result handling to Core, using the shared response and TOON policy instead of adapter-local decisions; older Hermes releases retain block-and-suggest command rewriting ([#3018](https://github.com/alibaba/anolisa/pull/3018)).
- Compression statistics now expose `content_origin`, `applied_operations`, and `recoverability` in place of `seam` and `compressor_chain`; authorized retrieval events also carry agent, session, and tool-call attribution ([#2978](https://github.com/alibaba/anolisa/pull/2978)).

### Fixed

- OpenClaw now reads plugin-local configuration, so explicit enable and disable settings take effect even when global configuration contains conflicting values ([#3009](https://github.com/alibaba/anolisa/pull/3009)).
- npm packages now include executable shared hook dispatchers as regular files, so installed plugins can invoke Tokenless even though `npm pack` omits source symlinks ([#3068](https://github.com/alibaba/anolisa/pull/3068)).

## [0.7.14] - 2026-08-26

### Added

- The protocol-v1 `tokenless compress` command and `TokenlessRuntime::compress` now provide one entry point for schema, response, and TOON compression, so adapters make at most one subprocess call, apply one final size decision, and record only the winning operation ([#2844](https://github.com/alibaba/anolisa/pull/2844)).
- `tokenless stats summary` now excludes dry-run rows from active totals while reporting their count, and adds gross savings, retrieved tokens, net savings, Retrieve hit/miss/error counts, and unrecoverable truncation attribution; JSON output uses schema version 1.1, and the Python `TokenlessStats` API exposes the same typed fields ([#2885](https://github.com/alibaba/anolisa/pull/2885)).

### Changed

- Truncation markers now include a directly runnable `tokenless retrieve` command, retrievals through the CLI, MCP, and embedded Runtime contribute to attribution, and MCP retrieval accepts uppercase hashes like the CLI ([#2885](https://github.com/alibaba/anolisa/pull/2885)).

### Fixed

- The Codex integration no longer appends a second compressed copy after Tool use; it keeps environment diagnostics and RTK source reduction without increasing the Model-visible Prompt ([#2866](https://github.com/alibaba/anolisa/pull/2866)).
- `tokenless compress-toon` and the Runtime/Python SDK now consistently leave valid payloads shorter than 500 characters unchanged, while `--min-toon-chars 0` can force encoding; malformed JSON still fails, and CLI output preserves the input's exact trailing-newline form ([#2869](https://github.com/alibaba/anolisa/pull/2869)).

## [0.7.13] - 2026-08-25

### Added

- Rust callers can now use the `tokenless-protocol` and `tokenless-pipeline` crates for versioned compression requests and responses, bounded content detection, registry routing, staged execution, and fail-open arbitration ([#2783](https://github.com/alibaba/anolisa/pull/2783), [#2788](https://github.com/alibaba/anolisa/pull/2788), [#2799](https://github.com/alibaba/anolisa/pull/2799)).

### Changed

- The CLI `compress-response` command, `TokenlessRuntime::compress_response`, and the Python binding now route record-shaped JSON through the shared pipeline; scalar JSON roots pass through unchanged, while timeouts and rejected candidates return the original content and roll back their Stash writes ([#2816](https://github.com/alibaba/anolisa/pull/2816)).
- Runtime and Python `disposition` values now use the protocol's snake_case forms such as `dry_run` and `no_savings`, and may report `passthrough`, `timeout`, or `error`; cleanup-only savings can now apply under `require_reversible` without a Stash when no truncation occurred ([#2816](https://github.com/alibaba/anolisa/pull/2816)).

## [0.7.12] - 2026-08-22

### Changed

- Response compression now keeps a configurable tail window after the retained array head (8 items by default, controlled by `--array-tail-preserve` and the Runtime API), so final statuses and error details remain inline while Stash stores only the omitted middle segment ([#2433](https://github.com/alibaba/anolisa/pull/2433)).
- The `BeforeModel` schema hook now warns once per session when a payload is malformed or carries no tool declarations, making a skipped hook distinguishable from a successful run that produced no savings; explicitly empty tool arrays still pass through silently ([#2606](https://github.com/alibaba/anolisa/pull/2606)).
- L2 benchmark JSON, Markdown, and semantic-gate findings now identify the missing ground-truth items behind retention failures instead of reporting counts alone ([#2433](https://github.com/alibaba/anolisa/pull/2433)).

### Fixed

- `tokenless stats enable` and `stats disable` now persist only the Stats toggle from the on-disk configuration, so temporary compression and SLS environment overrides are not copied into `config.json` ([#2592](https://github.com/alibaba/anolisa/pull/2592)).
- `tokenless stats summary --compare` now fails when either Session has no records, and `--limit 0` is rejected, preventing typos or empty samples from appearing as successful 0% comparisons ([#2674](https://github.com/alibaba/anolisa/pull/2674)).
- Schema compression now handles complete request objects with a top-level `tools` array, compressing Function Calling entries while preserving non-function tools and fields outside the array ([#2758](https://github.com/alibaba/anolisa/pull/2758)).
- Lossy array truncation markers now survive TOON round trips intact, and extremely large tail-preservation values keep the full array instead of overflowing ([#2433](https://github.com/alibaba/anolisa/pull/2433)).

## [0.7.11] - 2026-08-20

### Fixed

- `tokenless compress-toon` now scores TOON savings with the same CJK-aware character estimator as `stats summary` and the Python/SDK path, so dry-run stderr predicted counts match recorded `before_tokens`/`after_tokens`. JSON parse, oversized input, and TOON encode failures still exit 2 ([#2681](https://github.com/alibaba/anolisa/pull/2681)).

## [0.7.10] - 2026-08-19

### Added

- Gemini-native `functionDeclarations` tool schemas are now compressed in `BeforeModel` integrations such as copilot-shell, including declarations that use `parametersJsonSchema`, while preserving unrelated Gemini Tool fields ([#2663](https://github.com/alibaba/anolisa/pull/2663)).
- The `anolisa-tokenless` Python SDK now exposes typed, read-only status, summary, list, show, diff, and comparison queries through `TokenlessStats`, using the same Runtime data directory and returning stored tool content only for explicit show and diff calls ([#2666](https://github.com/alibaba/anolisa/pull/2666)).

### Changed

- Raw, RPM, npm, and source installations no longer build or ship the unused standalone `toon` executable; TOON encoding remains available through `tokenless compress-toon` and `tokenless decompress-toon`, and upgrades remove only Tokenless-owned legacy artifacts ([#2657](https://github.com/alibaba/anolisa/pull/2657)).

### Fixed

- The AgentScope integration wheel now declares `tqdm`, so clean installations using the supported AgentScope 1.x range continue to import with OpenAI 3.3.0 and later without a manual dependency workaround ([#2665](https://github.com/alibaba/anolisa/pull/2665)).

## [0.7.9] - 2026-08-18

### Added

- The `anolisa-tokenless` Python wheel now exposes framework-neutral `before_model`, `before_tool_call`, `after_tool_call`, and `retrieve` lifecycles, with bundled RTK plus native schema and response compression, TOON, marker-authorized retrieval, and per-call attribution ([#2627](https://github.com/alibaba/anolisa/pull/2627)).

### Changed

- AgentScope 1.0.11 through 1.x and AgentScope 2.0.x integrations now attach the same complete SDK contract, adding schema compression, command rewriting, TOON, environment-error guidance, and per-call attribution to the existing response compression and retrieval support ([#2627](https://github.com/alibaba/anolisa/pull/2627)).

### Fixed

- The Cosh-NG extension's RTK rewrite hook now matches the lowercase `shell` tool name directly, so shell commands are rewritten without depending on host-side tool-name aliases ([#2611](https://github.com/alibaba/anolisa/pull/2611)).

## [0.7.8] - 2026-08-18

### Changed

- TOON encoding is now skipped for payloads under 500 characters; below that threshold the token savings are near-zero while the per-event encode cost stays the same ([#2613](https://github.com/alibaba/anolisa/pull/2613)).

### Fixed

- npm platform packages (`@anolisa/tokenless-*`) no longer declare `tokenless`/`rtk`/`toon` bin entries. The name collision with the root package made npm remove every conflicting `.bin` link during install, leaving installs without a usable `tokenless` executable ([#2613](https://github.com/alibaba/anolisa/pull/2613)).

## [0.7.7] - 2026-08-17

### Added

- A source-built `anolisa-tokenless` ABI3 wheel now provides stateful in-process JSON response compression and marker-based Stash retrieval for CPython 3.11+ without spawning the CLI ([#2501](https://github.com/alibaba/anolisa/pull/2501)).
- AgentScope 1.0.11 through 1.x and AgentScope 2.0.x applications can install a separate same-version integration wheel to compress successful final Tool Responses and expose retrieval only for markers visible to the current Agent ([#2507](https://github.com/alibaba/anolisa/pull/2507), [#2528](https://github.com/alibaba/anolisa/pull/2528), [#2553](https://github.com/alibaba/anolisa/pull/2553)).
- DeepSeek Harness profiles can enable a bundled native Plugin that compresses successful single-block JSON Tool Results while preserving environment-error attribution and fail-open behavior ([#2581](https://github.com/alibaba/anolisa/pull/2581)).

### Changed

- Claude Code Adapter detection now retries transient first-run binary and Plugin-registry initialization failures, reducing false not-ready results immediately after provisioning ([#2519](https://github.com/alibaba/anolisa/pull/2519)).
- The Tokenless RPM now provides the virtual `anolisa-component(tokenless)` capability, allowing ANOLISA to resolve the Package when the repository component index is unavailable ([#2576](https://github.com/alibaba/anolisa/pull/2576)).

### Fixed

- Cosh-NG extension execution now treats the hard-disabled Tool Ready Hook's empty result as a successful no-op instead of failing closed ([#2506](https://github.com/alibaba/anolisa/pull/2506)).
- No-savings response compression now removes only Stash rows created by the discarded candidate, avoiding orphaned rows without deleting entries refreshed by another process ([#2480](https://github.com/alibaba/anolisa/pull/2480)).

## [0.7.6] - 2026-08-13

### Changed

- `TOKENLESS_DATA_DIR` now accepts absolute non-root directories outside the real user home, while invalid explicit directories disable SQLite state instead of silently falling back to home ([#2434](https://github.com/alibaba/anolisa/pull/2434)).
- Tool Ready pre-call checks, repairs, and blocking are now hard-disabled across adapters so incorrect readiness results cannot block valid work; post-tool failure attribution and other Tokenless features remain active ([#2487](https://github.com/alibaba/anolisa/pull/2487)).

### Fixed

- Direct JSON Schema descriptions are now stashed once, so one retrieval returns the original content without a nested marker ([#2399](https://github.com/alibaba/anolisa/pull/2399)).
- Dry-run compression settings from `config.json` now remain effective when statistics and SLS toggles are set through environment variables ([#2380](https://github.com/alibaba/anolisa/pull/2380)).
- `tokenless retrieve` now writes stored payloads byte-for-byte without appending a trailing newline ([#2396](https://github.com/alibaba/anolisa/pull/2396)).
- Stash retrieval now scans past malformed markers to find a later valid key and stays linear on adversarial input ([#2386](https://github.com/alibaba/anolisa/pull/2386)).
- RPM installations now include the shared Codex lifecycle helper required by the adapter install script ([#2425](https://github.com/alibaba/anolisa/pull/2425)).

## [0.7.5] - 2026-08-10

### Added

- OpenCode users can now enable Tokenless through a collision-safe local plugin that shares the existing readiness, rewrite, schema, and response-compression hooks ([1233cfcf](https://github.com/alibaba/anolisa/commit/1233cfcfd863de4bca7819b0a98615c569da2c9a)).

### Changed

- The Qoder adapter now uses native plugin and hook conventions, replacing compressed tool output in place while preserving fail-open behavior ([13817938](https://github.com/alibaba/anolisa/commit/13817938f0a8cf2b8df78d3e59f97302e4fb1947)).

### Fixed

- Rewritten shell commands now use the resolved absolute `rtk` path, so they continue to work in agent environments with a restricted `PATH` ([ae83f7d3](https://github.com/alibaba/anolisa/commit/ae83f7d3ef9c85d5f42e7b1c0fd6884a0ffc4869)).
- Qoder and OpenClaw hooks now preserve agent, session, and tool attribution across rewrite and proxy boundaries ([#2158](https://github.com/alibaba/anolisa/issues/2158), [2f330656](https://github.com/alibaba/anolisa/commit/2f330656fe94fc5936e1ebcaf586d7ebcd7df0d5)).
- Adapter installation now recognizes legacy `/usr/local` layouts, recommends RPM upgrade mode, and removes stale packaged user-manual files during upgrades ([f7ce3878](https://github.com/alibaba/anolisa/commit/f7ce38786cfba318614849a73a7f9acb693ea803), [ec25d516](https://github.com/alibaba/anolisa/commit/ec25d516b8c05ebd8e88a703f57708749c8032ab), [917f151e](https://github.com/alibaba/anolisa/commit/917f151ea4157850b51162668b3e1a441fb04262)).

## [0.7.4] - 2026-07-31

### Added

- Tokenless can now be installed from npm on Linux and macOS x64/arm64, including the `tokenless`, `rtk`, and `toon` binaries plus framework adapters ([#1929](https://github.com/alibaba/anolisa/pull/1929)).
- `tokenless stats diff` now explains estimated savings for records, sessions, and tool uses with text or JSON reports and bounded unified diffs ([#1991](https://github.com/alibaba/anolisa/pull/1991)).
- `TOKENLESS_DATA_DIR` now sets one trusted directory for both statistics and reversible-compression databases while preserving per-database overrides ([#2038](https://github.com/alibaba/anolisa/pull/2038)).

### Fixed

- The Qwencode adapter now declares its delivered `compress-toon` capability, keeping adapter discovery consistent with its compression behavior ([#1945](https://github.com/alibaba/anolisa/pull/1945)).
- Hermes copy installations now resolve shared hook resources from trusted system, XDG, and user data paths with actionable diagnostics when no safe candidate exists ([#2058](https://github.com/alibaba/anolisa/pull/2058)).

## [0.7.3] - 2026-07-28

### Added

- ANOLISA can now install Tokenless on macOS and enable Qwencode as an independent adapter ([#1964](https://github.com/alibaba/anolisa/pull/1964)).

### Changed

- Adapter hooks now discover `tokenless`, `rtk`, and `toon` across user, `/usr/local`, RPM, and legacy installation layouts ([#1957](https://github.com/alibaba/anolisa/pull/1957)).
- Hook launchers now prefer resources from the active installation, preventing mixed versions when multiple Tokenless installations coexist ([#1964](https://github.com/alibaba/anolisa/pull/1964)).

### Fixed

- Tool schema compression now reads the canonical Cosh and Cosh-NG request field, so schemas are compressed instead of silently passing through unchanged ([#1894](https://github.com/alibaba/anolisa/pull/1894)).
- Cosh-NG compression statistics are now attributed to `cosh-ng` when hook environment variables are present ([#1894](https://github.com/alibaba/anolisa/pull/1894)).
- Qoder plugin installation now expands cached hook paths, preventing invalid `/rewrite_hook.py` commands from blocking tool calls; the user manual includes recovery steps for affected upgrades ([#1924](https://github.com/alibaba/anolisa/pull/1924)).
- ANOLISA packages now include the shared hook resources required by Tokenless adapters ([#1964](https://github.com/alibaba/anolisa/pull/1964)).

## [0.7.2] - 2026-07-27

### Added

- Tokenless now compresses Cosh-NG tool responses by replacing the original model-visible content ([#1669](https://github.com/alibaba/anolisa/pull/1669)).
- Tokenless now rewrites supported Cosh-NG shell commands for more compact output ([#1669](https://github.com/alibaba/anolisa/pull/1669)).

### Changed

- Shell environment checks now report only recommended tools referenced by the current command ([#1598](https://github.com/alibaba/anolisa/pull/1598)).
- `tokenless env-check --fix` now installs required dependencies only, leaving optional recommendations untouched ([#1598](https://github.com/alibaba/anolisa/pull/1598)).
- Automatic dependency fixes now fail quickly with actionable authentication, network, or permission messages instead of prompting for sudo ([#1598](https://github.com/alibaba/anolisa/pull/1598)).
- Cosh-NG compression statistics are now recorded under the `cosh-ng` agent ([#1669](https://github.com/alibaba/anolisa/pull/1669)).
- Cosh-NG compression now excludes display-only content from model context ([#1669](https://github.com/alibaba/anolisa/pull/1669)).
- Cosh-NG runs with undetectable versions now keep original tool responses unchanged ([#1669](https://github.com/alibaba/anolisa/pull/1669)).
- Compression now leaves tool results unchanged when the compressed output is not smaller ([#1674](https://github.com/alibaba/anolisa/pull/1674)).
- Tokenless user manuals now live in the central ANOLISA guide instead of the RPM package ([#1586](https://github.com/alibaba/anolisa/pull/1586)).

### Fixed

- Claude Code 2.1.121+ now replaces original tool results with compressed versions, preventing duplicate context ([#1674](https://github.com/alibaba/anolisa/pull/1674), [#1686](https://github.com/alibaba/anolisa/pull/1686)).
- Older or undetectable Claude Code versions now pass tool results through unchanged instead of duplicating compressed context ([#1674](https://github.com/alibaba/anolisa/pull/1674), [#1686](https://github.com/alibaba/anolisa/pull/1686)).
- Claude Code replacements now preserve built-in tool result formats, including empty fields ([#1674](https://github.com/alibaba/anolisa/pull/1674), [#1686](https://github.com/alibaba/anolisa/pull/1686)).
- ANOLISA now recognizes the packaged Tokenless version correctly ([#1587](https://github.com/alibaba/anolisa/pull/1587)).

## 0.7.1

- fix RPM tarball to exclude generated `.anolisa/component.toml`, ensuring rpmbuild always regenerates the adapter contract from the authoritative `.toml.in` template — previously stale checked-in copies shipped outdated contracts missing claude-code, codex, and cosh adapter declarations (closes #1470)
- synchronize adapter contracts: declare every shipped driver (qoder, claude-code, codex, cosh, qwencode) in `component.toml.in` and add CI check (`check-component-contract`) to keep them in sync
- raise test coverage from 75% to 90%: ~170 new unit tests across all four crates covering compression edge cases, stash round-trip, schema migration, SLS writer, and CLI dispatch
- harden test isolation: replace unsafe env-var mutations with RAII `TempDbGuard` / `EnvGuard` to prevent tests from touching real `~/.tokenless` state; enforce `--test-threads=1` in Makefile (Rust 2024 `set_var` is unsafe)


## 0.7.0

- add MCP `tokenless_retrieve` stdio server (`tokenless mcp serve`) so MCP-connected agents can recover truncated payloads on demand — the MCP analogue of the `tokenless retrieve` CLI, closing the stash MCP gap vs Headroom CCR's `headroom_retrieve`
- complete reversible-compression (stash / CCR) coverage across the remaining lossy paths: `ResponseCompressor` string truncation, `ResponseCompressor` depth truncation, and `SchemaCompressor` description truncation are now stash-backed with `<<tokenless:KEY>>` markers; fit-check before stash prevents orphan entries; shared `stash_suffix()` helpers keep marker budget consistent
- add `--no-stash` / `--stash-db` flags to `compress-schema` (mirroring `compress-response`); dry-run (`compression_on=false`) skips the stash so markers never reach the LLM without a retrievable entry
- add lazy TTL purge to `SqliteStore`: expired rows are physically deleted before retrieve lookups so the stash db does not grow unbounded
- add actual-savings-rate display: `StatsSummary::actual_savings_percent(session_total_tokens)`; `format_summary()` / `format_summary_json()` accept optional session total and emit an "Overall Savings vs Total Consumption" section plus new JSON fields (`session_total_tokens`, `actual_savings_tokens`, `actual_savings_percent`) — backward-compatible when absent
- add stash write/size counters to compression stats (`record_compression_stats` extended); retrieve-side hits/misses deferred pending a stats use case
- add qoder framework driver (qodercli install + settings.json merge/prune, `AdapterOps::read_file`, symlink-safe atomic `write_file`); gate qoder to `adapter_type=plugin`; fail closed on forged receipts and require all managed hooks
- raise test coverage from 59% to 75%: 100+ new unit tests plus 18 CLI integration tests across all four crates, test code moved to `src/tests/` via `include!()` for cleaner separation
- add reversible-compression user manual (`docs/stash-reversible-compression.md`) plus README updates: architecture tree entry for `tokenless-ccr`, retrieve subsection documenting hash/marker input and `--no-stash`/`--stash-db`, scenario-mapping rewrite of the "Applicable Scenarios & Expected Effects" chapter
- rename tokenless docs `*_CN.md` to `*_zh.md`, add bidirectional bilingual links, create `README.md` + `README_zh.md`
- address adapter review findings: trust packaged datadir roots for Codex symlink targets, scope Claude Code marketplaces per component and fail closed, reject framework/adapter type mismatches before enable
- silence clippy warnings surfaced by rustc 1.94 stable in existing tests (`field_reassign_with_default` in tokenless-cli, `bool_assert_comparison` and `default_constructed_unit_structs` in tokenless-stats)

## 0.6.1

- bundle tool_categories.json into dist for npm installs
- use node: prefix, eliminate shell subprocess in openclaw plugin

## 0.6.0

- add absolute saved values + schema version to JSON output
- use import.meta.dirname instead of __dirname in openclaw plugin
- add qwencode adapter for Qwen Code extension
- fix rtk pytest 'No tests collected' regression
- add trusted FHS fallback paths for hook_utils import in codex scripts
- add SLS JSONL data collection with config toggle
- add tokenless RPM component contract (publishing metadata)
- add compression toggle with dry-run compare mode (`TOKENLESS_COMPRESSION_ENABLED`, `stats summary --compare`)
- enable SLS recording by default and document usage
- align compression mode serde/db form and dedup config load
- expand RPM component contract (bundle.entry + hermes)
- make SLS writer append-only and skip when log file absent
- prefer tool_call_id over internal tool_use_id for qwencode hooks
- bump vendored rtk to v0.43.0; rework pytest stderr-surfacing patch for the refactored runner; drop grep-fallback-fix (root cause fixed upstream) and preflight-skip-python (reversed upstream)
- sync toon-format to 0.5.0 in Makefile and spec (was stale at 0.4.6)

## 0.5.1

- add --json output to stats summary
- implement unified tool categorization and 3-layer compression strategy
- add rtk grep fallback pattern fix patch
- add rtk pytest error report patch

## 0.5.0

- add Hermes adapter runner
- drop TOON wrapper prefix and slim diagnostic tags
- unify rtk rewrite exit code 3 handling across adapters
- secure shell variable interpolation in env-fix and hooks
- add subprocess returncode checks and extract shared hook utilities
- secure resolveBinaryPath and improve binary cache invalidation
- use mktemp in tests and safe home expansion
- bound SchemaCompressor recursion to prevent stack overflow
- propagate env-fix subprocess failures instead of returning stdout
- anchor home lookup on getpwuid_r and trust-check candidate binaries
- harden env-fix install paths with uid trust check and divert stderr to log
- recover from poisoned mutex in stats recorder instead of failing
- add input size limit and validate db path
- reserve truncation marker length in response compressor
- rename openclaw plugin Name to Tokenless and ID to tokenless
- add qoder CLI adapter
- compress-schema on array input
- warn when compression is skipped
- stats command syntax
- add Claude Code adapter plugin
- error on TTY stdin instead of hang
- add codex adapter plugin
- fix compression pipeline output inflation, truncation and hook timeouts
- harden env-fix, version extraction, file trust, schema, permissions
- address review findings — trailing newline, chmod guard, rate-limited log, comment
- make env attribution reachable for skip-tools entries
- add selective-claw context engine plugin
- address review findings for selective-claw plugin
- remove invalid "2" dependency from selective-claw
- restore indentation in compress_response_hook.py
- harden hook exit-code handling + trust model consistency
- only warn on truly unexpected rtk exit codes
- dedup rewrite_hook, import from hook_utils

## 0.4.1

- fix version_ge 3-segment truncation in env_check.rs (compare all segments)
- add qoder, claude-code, codex adapter plugins and documentation
- sync manifest.json with template to include all six agents
- update README and user manuals for new agent integrations
- add __pycache__ to root .gitignore
- update response-compression.md with all agent integration paths
- derive Makefile version from Cargo.toml, fix spec changelog weekday
- normalize adapter version numbers to 0.4.0
- derive adapter plugin versions from Cargo.toml instead of hardcoding

## 0.4.0

- correct 5 bugs in stats, naming, SQL, paths and permissions
- align FHS paths, restructure adapter dir, remove install.sh
- address code review findings across schema, env-check, hooks, and plugin
- add hermes agent plugin
- security hardening & critical algorithm correctness
- behavioral correctness & logic fixes
- dedup, dead code removal & cosmetic cleanup
- support staged installs
- support Debian/Ubuntu FHS paths and harden binary resolution
- build OpenClaw plugin to dist/index.js

## 0.3.2

- replace spoofable home-dir uid derivation with libc::getuid() syscall for trust chain integrity
- replace subprocess toon -e calls with in-process toon_format::encode_default() library call
- replace rtk/toon git submodules with crates.io deps and inline toon-format source
- hard-fail on rtk stats patch failure in justfile setup-rtk recipe
- unify compress-toon/compress-schema/compress-response error exit codes (all exit 2)
- remove 2>/dev/null || true from Makefile toon install (hard fail on missing binary)
- remove redundant #[source] attribute on thiserror variants that already have #[from]
- deduplicate Python hook FHS path constants into shared hook_utils module
- add libc to workspace dependencies for uid syscall
- add detailed rust >= 1.89 comment in spec.in explaining CI pin rationale

## 0.3.0

- add tool-ready 4-phase environment pre-check with cosh extension integration
- skip compression and stats when no token savings
- pass caller context to rtk stats via .rewrite-context file
- remove redundant cosh extension install/uninstall from install.sh
- convert cosh hooks to extension format per cosh dev guide
- skip zero compression and stats recording
- use isExecutable() and resolved paths in openclaw plugin
- resolve rtk/toon binary paths for RPM-installed plugins
- correct RPM install paths to align with install.sh expectations
- preserve tool result message structure in TOON encoding
- align install paths with FHS
- auto-record stats with real tool_use_id from hook payload
- restructure RPM dirs and remove auto plugin/hook installation

## 0.2.0

- add compression stats with auto-record from real data
- add TOON context compression support
- skip compression for skill and content-retrieval tools

## 0.1.0

- introduce tokenless into ANOLISA (#199)

[0.7.6]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.5...tokenless/v0.7.6
[0.7.5]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.4...tokenless/v0.7.5
[0.7.4]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.3...tokenless/v0.7.4
[0.7.3]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.2...tokenless/v0.7.3
[0.7.2]: https://github.com/alibaba/anolisa/compare/tokenless/v0.7.1...tokenless/v0.7.2
