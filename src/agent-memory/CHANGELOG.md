# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.8] - 2026-09-20

### Added

- **agent-memory**: Falls back to a private per-user runtime or temporary directory when the configured session directory is unusable, unprivileged installations can use session logs and `mem_promote` without creating a root-owned directory; fallback paths are checked for ownership, permissions, and symlink redirection (#3266)

### Changed

- **agent-memory**: **Breaking change:** renames the OpenClaw tools to `anolisa_memory_search` and `anolisa_memory_get`, agents can access ANOLISA memories alongside OpenClaw's own memory tools without name conflicts; update prompts and tool allowlists, restart the gateway, and start a new conversation. Internal MCP names and stored memories are unchanged (#3347)
- **agent-memory**: Declares all four plugin tools in OpenClaw's `coding` profile, agents retain access to the plugin tools under that profile without disabling `memory-core`; custom policies still need explicit tool entries, and disable markers from intermediate installers require the manual recovery described in the [user guide](../../docs/user-guide/en/token-saving/agent-memory.md) (#3347)

### Fixed

- **agent-memory**: Accepts `sessionId` and `sessionDir` in the OpenClaw plugin schema and enforces the server's 128-byte UTF-8 identifier limit at plugin startup, users can pin session scratch across respawns and receive configuration errors before an invalid identifier changes their namespace (#3213)
- **agent-memory**: Ranks stronger BM25 matches first and aligns keyword scores with the higher-is-better convention, search, hybrid retrieval, and automatic recall prioritize the most relevant matching memories (#3296)
- **agent-memory**: Rejects the unsupported OpenClaw `expert` profile with guidance to use `basic` or `advanced` and stops the previous client before validating a reload, operators see actionable configuration errors without leaving the old memory process holding storage locks (#3238)
- **agent-memory**: Restores the required files in `make dist` source archives, source-package and RPM builders receive the example target, configuration, and adapter assets needed to build and install (#3231)

## [0.2.7] - 2026-09-09

### Fixed

- **agent-memory**: Negotiates OpenClaw installer flags from host help and grants declared capabilities by default, operators can install on hosts that require consent or withhold it with `AGENT_MEMORY_ACCEPT_CAPABILITIES=0`; deprecated no-op unsafe-install flags are omitted and failures distinguish consent rejection from unwritable destinations (#3149)
- **agent-memory**: Labels corpus hits as `agent-memory` and supplies read handles for `memory_get corpus=all`, agents can retrieve matching stored memories even when their paths overlap OpenClaw workspace memory and receive accurate `fromLine` / `lineCount` for windowed reads (#3177)
- **agent-memory**: Validates plugin configuration before locating the binary, operators see invalid `userId` / `sessionId` errors even when `agent-memory` is not installed (#3155)
- **agent-memory**: Declares its RPM component identity, ANOLISA can recognize the installed package as the `agent-memory` component (#2560)
- **agent-memory**: Declares Node.js >= 20 and npm as source-build dependencies in the unified build workflow, user-mode setup provisions the tools needed to bundle the OpenClaw adapter and system-mode preflight reports missing tools before building (#3187)

## [0.2.6] - 2026-07-30

### Fixed

- **agent-memory**: Updated to v0.2.6, retries short-token LIKE searches with OR matching when strict matching finds nothing and ranks stronger keyword matches first, agents can recall relevant memories from stopword-heavy prompts instead of receiving an empty result (#2040)

## [0.2.5] - 2026-07-27

### Fixed

- **agent-memory**: Updated to v0.2.5, derives focused recall queries from long English and CJK prompts and combines their results, agents can recall relevant memories from verbose prompts without silently dropping topics (#1574)

## 0.2.4

- fix(memory): auto-recall returns empty results after observe — synchronously reindex after memory_observe so before_prompt_build hook finds new content (#1520)
- fix(memory): install.sh sets allowConversationAccess for hooks (#1521)

## 0.2.3

- fix(memory): normalize OpenClaw content blocks from array of content blocks `[{type:"text", text:"..."}]` to string before trigger matching and hashing, so auto-capture actually fires instead of coercing to `"[object Object]"`
- fix(memory): add BM25 OR fallback — when implicit-AND FTS5 query returns 0 rows and there are multiple tokens, retry with `'\"token1\" OR \"token2\" OR ...'` so partial matches still surface instead of silent failures
- fix(memory): sanitize audit_log by replacing `format!("{:.120}", query)` with `format!("bm25:len={}", query.len())` to prevent user query content from leaking into log paths


## 0.2.2

- fix memory_observe hint sanitization so YAML-escaped hints round-trip through the hand-rolled frontmatter reader (which does not interpret YAML escapes): replace `yaml_escape_hint()` with `sanitize_hint()` that only substitutes newlines and ASCII control chars with spaces; add 8 unit tests plus a real-parser round-trip test covering Windows paths with backslashes
- add `max_hint_bytes` (default 512) to `MemoryConfig` with `MEMORY_MAX_HINT_BYTES` env override; thread `&MemoryConfig` through `memory_observe`, the `MemoryService` facade, and the MCP server
- fix `make install INSTALL_PROFILE=user PREFIX=$HOME/.local` failing with Permission denied at install-adapter-resources: honor `INSTALL_PROFILE` and derive `DATADIR`/`SHARE_DIR` from `$(PREFIX)` so all writable paths follow the profile (system mode unchanged); aligns with the tokenless/ws-ckpt install contract
- add `safe_fs` security-boundary unit tests (path escape, symlink traversal, sandbox root violations) plus formatting/import-order fixes exposed by `cargo fmt --all --check`

## 0.2.1

- fix vector/hybrid search panic and empty index when an embedding provider is configured: the index worker ran on a std::thread with no tokio Handle so embeddings were never produced, and memory_search mode=vector|hybrid called Handle::block_on from a worker thread; the runtime handle is now captured at spawn and threaded through to the worker, and the search path uses block_in_place
- fix memory_get_context leaking .git internals (e.g. .git/logs/HEAD) into agent context by extending the reserved-path filter to cover .git/ via a shared is_under_git predicate in safe_fs
- fix full_scan (startup and inotify-overflow recovery) only building the BM25 index and never dense embeddings, so preexisting files were invisible to vector search until modified; a paths_without_vec query plus a backfill pass now embeds them, centralised in an embed_sync helper shared with flush
- fix memory_search returning zero hits for short CJK query terms (< 3 chars, e.g. "花名"/"小云"): the trigram tokenizer emits no tokens for terms shorter than 3 characters, so such queries now fall back to a `body LIKE '%term%'` substring scan that preserves recall, agent-scope filtering, and cold/superseded exclusion
- resolve embedding dimensions from the first real response instead of hardcoding 1536 (DashScope text-embedding-v3 is 1024): dimensionality is stored in an AtomicUsize seeded with the estimate and overwritten on first embed
- add anolisa-cli adapter contract via .anolisa/component.toml so the CLI adapter manager can discover the openclaw plugin bundle through the [[adapters]] TOML schema

## 0.2.0

- add prompt-injection safety module (looksLikePromptInjection + escapeMemoryForPrompt) mirrored between Rust core and TS adapter
- add secret detection and PII redaction to the safety module
- add auto-recall before_prompt_build hook injecting relevant memories each turn
- add auto-capture agent_end hook with trigger filtering, SHA256 dedup and injection rejection
- add dense-vector semantic search via pluggable EmbeddingProvider (OpenAI /v1/embeddings, Ollama /api/embed)
- add files_vec table (schema v2) for per-file dense embeddings alongside FTS5 BM25
- add hybrid search with reciprocal rank fusion (RRF, k=60) of BM25 + vector scores
- add memory_search mode parameter (bm25/vector/hybrid) with graceful fallback to BM25
- add per-agent memory isolation via [memory].agent_scope (shared/isolated/filter), schema v5
- add memory sovereignty tools (memory_about/forget/auto_created/consent) with consent.toml preferences
- add 4-type closed memory classification (user/feedback/project/reference) to memory_observe
- add mem_export and mem_import for cross-agent memory migration (AMA archive format)
- add memory_summary tool for memory overview and source tracking
- add memory_session_context tool
- add memory_sessions and memory_timeline session history query tools
- add MEMORY.md index file and mem_index_refresh tool
- add user profile synthesis (Dreaming V3 mem_dream)
- add memory consolidation: auto-extract L1 atomic facts from session audit logs on shutdown
- add episodic memory extraction from coherent tool-call chains
- add cross-session task persistence and incremental consolidation
- add consolidation quality filters (mutual exclusion, non-derivable, date normalization)
- add time-decay ranking (exp(-λ×age_days)) applied to BM25/vector/hybrid scores
- add cold archival of old never-accessed files with mem_compact tool
- add conflict detection via BM25 similarity before writing new facts
- add category subdirectories (facts/<category>/) with memory_search category filter
- add token tracking (tokens field in AuditEntry)
- add mem_consolidate tool for manual consolidation trigger
- add corpus supplement registration for memory_search corpus=all
- add EmbeddingConfig (None/OpenAI/Ollama) with TOML parsing and env overrides
- extend memory_search signature with optional mode and category parameters
- cap memory_search query at 1024 characters to prevent FTS5 resource exhaustion
- truncate embedding error response bodies to 200 chars to prevent API key leakage
- distinguish CJK vs ASCII token estimation in ConsolidatedFact
- hold FactWriter JSONL file handle under mutex to prevent line interleaving
- derive BM25Store mount root from db path with canonicalize + starts_with traversal guard
- compute Episode duration from entry timestamps instead of chain length
- propagate session_id to extracted episodic facts
- return fact count from consolidate() for mem_consolidate reporting
- fix effectiveMode in search response to reflect actual mode used
- fix embedding API empty-response handling to return zero vector of correct dimensionality

## 0.1.0

- introduce filesystem memory MCP server for AI agents (Linux only) with 21 tools over stdio JSON-RPC 2.0 in three tiers (Tier A file ops, Tier B BM25 search, Tier C governance)
- add per-namespace mount under ~/.anolisa/memory/<ns>/ with optional user-namespace + private tmpfs isolation (auto/userland/userns strategies)
- enforce path sandbox via openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS) on every Tier A file open
- add SQLite FTS5 BM25 background index with transactional upsert, schema migrations, trigram CJK tokenizer and inotify-driven debounced flush
- add optional git versioning with auto-commit serialized under a per-handle mutex
- add tar.gz snapshots with strict id whitelist, atomic rename swap on restore and rollback entries under .anolisa/trash/
- add optional cgroup v2 memory.max self-limit applied before the tokio runtime starts
- add JSONL audit log (O_NOFOLLOW | O_CLOEXEC, Mutex<File>) with optional systemd-journald fan-out
- enforce profile gating (basic/advanced/expert) at both tools/list and tools/call with deny_unknown_fields on config structs
- add per-session scratch and log under /run/anolisa/sessions/<sid>/ (0700) with tmpfiles.d snippet
- add systemd user template anolisa-memory@.service with hardening (ProtectKernelTunables/Modules/Logs, SystemCallFilter, MemoryDenyWriteExecute, RestrictNamespaces, RestrictAddressFamilies=AF_UNIX)
- add RPM packaging with offline vendor tarball and single statically-linked binary (bundled SQLite + vendored libgit2)
- add OpenClaw plugin memory-anolisa with install/detect/uninstall lifecycle and 4 memory contract tools routed to the MCP server as a stdio child
- add single-source version sync from Cargo.toml into manifest/package/openclaw/mcp JSON and the bundle
- add mcp-harness example and 140 automated tests across 12 integration suites
