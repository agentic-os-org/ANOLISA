# Changelog

## 0.15.0

### Features
- Discover Gemini CLI JSONL chats under `~/.gemini/tmp/<project>/chats/` in the trajectory collector and local viewer, including sub-agent transcripts and project names; the viewer also opens legacy whole-file chats. Sub-agent composite IDs do not link back to the parent row.
- Add `--stats-only` to the standalone `analyze` CLI and batch-analysis wrapper for local `perf`/`cost` statistics without API credentials or LLM requests; `accuracy --stats-only` is rejected, and omitting the option retains model-backed analysis.
- Expose the probe-independent `PipelineCore` Rust library API for parsing, correlating and analyzing events without attaching probes, and re-export `HttpContext` as `agentsight::analyzer::HttpContext` so library callers can name the HTTP data carried by `AnalysisResult::HttpContext`; existing command syntax and capture behavior are preserved.

### Fixes
- Bound the Tokenless savings numerator by the request window when timestamps are available, excluding older optimizations when the Dashboard range narrows. Legacy local `stats.db` tables without `timestamp` retain all matching records, so their savings can still include out-of-window optimizations.
- Count Tokenless records with a missing, null or upper-case mode as applied savings; only dry-run records are excluded.
- Keep Anthropic server-tool and MCP tool calls, including their arguments, when a stream is reconstructed after the connection drains.
- Treat a tool result that opens with a JVM or SQL exception as a tool failure in conversation grading and interruption detection, matching the grounding classifier.
- Preserve non-streaming Anthropic, Responses, Gemini and legacy-completion answers and tool calls in token breakdowns instead of reporting zero output.
- Preserve Gemini prompts, answers, tools and finish reasons when building calls from parsed messages without raw HTTP bodies.
- Render the DeepSeek Harness `agentsight_status` tool result through text content blocks so the host and model receive the validated status.
- Detect SSL symbols in Python executables and shared libpython images for the existing TLS capture path; retain retryable image reads and startup error causes.
- Reject fault attempts that were not fully delivered instead of reporting a passing benchmark campaign. (#8890)
- Recover interrupted benchmark campaigns by validating resume checkpoints, archiving unusable results and atomically replacing checkpoints. (#8857)
- Reject invalid benchmark time controls before network/file operations and prevent PID reuse from corrupting process-survival and resource evidence. (#8889, #8856)
- Preserve literal capture-database paths containing URI punctuation and valid JSONL records around damaged lines, with warnings for discarded records.
- Treat missing or invalid recovery observations as gaps, reject invalid final drop counters, and report unproven recovery gates instead of claiming an improvement.
- Read modern h2load request-latency summaries correctly, preserving mean, median, p95 and p99 without borrowing fields from adjacent rows.

### Performance
- Compute benchmark latency percentiles from one sorted snapshot and release unused fallback samples to reduce long-run work and memory use. (#8888)

### Docs
- Flatten the user manual layout.
- Move the integrated ANOLISA container image sources from `docker/` to `distribution/docker/` and update the AgentSight deployment guide's entrypoint path; CLI syntax, image tags and container behavior are unchanged.

## 0.14.3

Combined release notes for the 0.14.2 and 0.14.3 release work. Previously shipped fixes recorded in 0.14.2 are identified separately below.

### Features
- Add read-only Hermes `state.db` trajectory collection and Claude SDK session discovery, including tool-result matching by call ID. (#8713)
- Add Gemini CLI chat-file conversion to ATIF, including legacy JSON files and replay of conversation corrections; automatic discovery of `~/.gemini` chat files is not included. (#8801)
- Add type, state and agent filters to interruption queries where applicable, agent-scoped summary totals, and JSON output for interruption resolution.
- Add batch ATIF export without analysis and configurable parallel batch analysis.
- Add `token --db`, `summary --tokenless-db`, and per-agent metrics in JSON for offline queries and scripts.
- Add remote Tokenless statistics, refresh savings before queries, and count only savings from applied transformations.
- Add daily skill-hotness reporting and RSS/CPU benchmark metrics.

### Performance
- Reduce capture overhead and database work, stream batch JSONL conversion, and reuse benchmark metric snapshots.
- Add cooperative CPU pacing through `runtime_limits.cpu_budget_percent` (default 25% of one logical CPU; `0` disables pacing) and increase the default connection capacity from 32 to 128.

### Fixes
- Resolve gateway prefixes, vendor names and dated model aliases to usable tokenizers, including Gemini 2.5/latest/preview, GLM 4.5, Qwen 3.6, Kimi K2, GPT-5, and OpenAI audio/realtime families.
- Preserve response text, reasoning, refusals, server-tool blocks and stop reasons across Anthropic, Gemini, Responses, OpenAI-compatible, GLM, Codex and DashScope providers, including incomplete, filtered and non-streaming responses.
- Count streamed, cached and replayed token usage without duplication; retain complete token observations and fall back to raw-token counting when template rendering fails.
- Keep parallel tool calls and results associated by choice and call ID, preserve partial tool inputs, and report tool outcomes and first-output timing accurately.
- Preserve pending SysOM metadata and session attribution; retain main sessions, reasoning-only steps and parallel tool results during batch conversion, and isolate individual analysis failures.
- Keep slow TLS handshakes from blocking new connections, isolate cancelled HTTP/2 streams, preserve final headers after interim HTTP responses, and report HTTP/2 first-token latency.
- Include custom `serve --db` files in the storage budget and support BOM-prefixed storage, optimization and Tokenless configs.
- Require credentials for storage settings and LLM-spending endpoints, use the Agent Security V2 socket, handle IPv4-mapped rule addresses, and bound Agent Security response lines.
- Keep collected trajectories inspectable without GenAI records, refresh audit filters and trajectory savings, preserve UTF-8 previews, and retire stale model labels when content changes.
- Improve session search, preference counting and causal evidence matching; distinguish handled tool failures from unresolved failures and require continuous evidence for recovery claims.
- Record idle-agent OOM interruptions without duplicate kill reports, preserve truncated-turn state, and handle malformed counters, missing CPU measurements and failed reads explicitly.
- Reuse the global Prometheus recorder and initialize metrics before workers; skip Python executables during static-SSL probing.

### Previously shipped fixes recorded in 0.14.2
- Preserve calls with no response body when an agent exits, and keep recap-only sessions inspectable with correct analyzer-row classification and per-session aggregates. These fixes were already present in 0.14.1 and are included here for the combined release summary, not as new 0.14.3 behavior. (#7271)

### Upgrade notes
- Review CPU pacing and connection limits before rollout; explicit existing connection limits are preserved, and CPU pacing is a cooperative target rather than a hard quota.
- Scripts changing storage settings or invoking LLM-spending endpoints must supply credentials. Trajectory collection remains opt-in; Hermes collection requires access to its database and WAL files.

## 0.14.2

### Fixes
- Record previously shipped fixes for calls with no response body at agent exit and recap-only session inspection, analyzer-row classification and per-session aggregates; the code was already included in 0.14.1. (#7271)

## 0.14.1

### Features
- Accept `--db` in the query commands. (#7478)
- Content-probe unknown executables for static BoringSSL, with negative caching of misses.
- Support explicit prebuilt targets and build prebuilt agentsight artifacts in CI.

### Fixes
- Map OpenAI, Claude, Qwen3, InternLM, Baichuan, phi-3-small and yi-9b-chat model names to real, loadable tokenizers.
- Parse Gemini streaming responses, honor final Anthropic usage, fold whole-message chat SSE snapshots, and parse CR-ended SSE lines. (#7329, #7193, #7398)
- Aggregate HTTP/2 responses that finish before the request, cut chunked terminators only at line boundaries, and honor previously inert HTTP config options. (#6562)
- Keep unindexed and nameless tool calls apart, match late results by tool call, and retain mixed tool results. (#7385)
- Exempt trusted sinks by address, let exec wildcard basenames match any command, and map every legacy rule verb to an enforcement mode.
- Preserve numeric JSON keys, null payload members, evidence numbers, and full cost signatures; keep every skill in reports and tolerate null agent names in CSV export. (#7451, #7032, #7403, #7413)
- Treat CJK punctuation and pipes as literal delimiters, and keep health reporting working with a pre-epoch clock. (#7488, #7479, #7239)
- Merge case-variant agents and models across filters, token breakdowns and model series. (#7456, #7184)
- Isolate SQLite maintenance, serialize opt-store migration, and warn when purge stalls within the size limit.
- Bound payload memory lifetimes and the protection scan directory. (#6781)
- Harden audit windows, ATIF token totals and session ids, containment snapshots, and grounding evidence matching. (#7326, #7467)
- Fix dashboard pagination, ranking refresh, cached-only step metrics, idle gaps, and timestamp precision. (#7183, #7093, #7091, #7089, #7095)
- Let recap-only sessions materialize, retain captured session children, and split analyzer call kinds.
- Gate server-only modules behind the server feature.

## 0.14.0

### Features
- Unify SQLite storage under one configurable total cap (default 2200 MB); the largest databases are trimmed first and the cap is re-read without a restart.

### Fixes
- Preserve and correctly count streamed content, reasoning, tool calls, response IDs, finish reasons, and token usage across Anthropic, Gemini, Codex, and Responses providers.
- Make agent discovery, trajectory filtering, reuse labels, semantic search, dashboard metrics, savings, and interruption aggregates more accurate and robust.
- Harden enforcement, credential discovery, IMDSv2 access, BPF polling, process restart, OOM detection, and audit/event validation paths.
- Make SQLite maintenance, storage budgeting, UTF-8 handling, SSE/HPACK parsing, and time handling resilient to malformed or edge-case input.
- Support legacy kernel enforcement, wrapped inode guard labels, legacy function-call tool requests, and additional process/file reference formats.

### Tests
- Expand provider, SLS, trajectory, dashboard filter, storage-budget, and enforcement regression coverage.

## 0.13.1

### Features
- Add JSON output and graceful CLI errors.

### Fixes
- Resolve namespace PIDs to host PIDs with an init-namespace self-check. (#3041)
- Honor the RUST_LOG regex in the main logger. (#3186)
- Add inode guard support for file-delete-guard on 5.10/6.6 kernels.
- Drop the trace_id alias in the traces list. (#3261)
- Fix a stale file descriptor in the enforcer ringbuf consumer after mmap advance.
- Harden credential discovery.
- Pin the drain trigger to membarrier.
- Add a token-plan provider preset.
- Recover empty tool_call names from request tool definitions.
- Log semantic-search degradation causes. (#3443)
- Harden the SQLite lifecycle.
- Map qwen3.8-max to its tokenizer. (#3409)
- Restore `agentsight-enforcer` automatically when an upgrade leaves a stale enforcement layout, so file-delete guard and enforcement health do not require manual cleanup. (#3445)
- Keep an application's process tree under policy when its binding is recreated. (#3370)
- Reject unsupported event filters on audit cases instead of silently ignoring them.

## 0.13.0

### Features
- Add `GET /api/preferences` and `GET /api/preferences/export` to surface the
  working habits inferred from recent conversations (language, plan-first
  workflow, test expectations, correction patterns, tool leanings), with
  `/api/preferences/turns` exposing the user turns an analysis was based on.
  Analysis runs per request over `genai_events` on Linux or collected
  trajectories elsewhere, so there is no derived state to migrate or go stale.
- Add `GET /api/trajectories/steps` to find ATIF steps by derived category
  (`user_input`, `system`, `agent_message`, `thinking`, `tool_call`,
  `tool_result`), returning each hit with its neighbouring steps. Categories are
  multi-label because a single agent step can carry a message, reasoning, tool
  calls and observations at once.
- Label trajectories for reuse triage, let a person settle a trajectory's label,
  add the reuse-label review page, and gate search to serve labelled trajectory
  history.
- Add an inode guard for file-delete-guard on 5.10/6.6 kernels, with domain
  isolation, violation events, and startup cleanup.

### Fixes
- Initialize logging in the macOS `trace` path so trajectory collection failures
  surface instead of being silently dropped.
- Add a token-plan provider preset.
- Fix a stale fd in the enforcer ringbuf consumer after an mmap advance.
- Drop the trace_id alias in the traces list. (#3261)
- Honor the RUST_LOG regex in the main logger. (#3186)
- Resolve namespace PID to host PID with an init-ns self-check. (#3041)
- Add the conversation_id column before its index. (#3378)

## 0.12.1

### Breaking
- Align kernel event timestamps by calibrating BPF monotonic clocks against realtime instead of container uptime. Historical wall-clock steps for events queued at the time cannot be reconstructed; already persisted records are unchanged.

### Features
- Capture rustls plaintext for cosh-ng and attach the cosh plaintext tap by name. (#3191)

### Fixes
- Stop counting cached tokens twice in token totals. (#3081)
- Stream the static SSL pattern scan. (#3087)
- Shorten the SSL uprobe re-attach TTL to 30s.
- Assemble fragmented HTTP responses.
- Complete HTTP/2 streams when SSE ends, and record streaming calls that signal via request headers instead of a body flag. (#3147)
- Merge duplicate records when a long-running call completes after an idle snapshot.
- Retry startup database cleanup when the database is locked, and recognise structured oom-kill records during startup recovery. (#3135)
- Match the OpenClaw gateway launched with the --max-old-space-size V8 flag.
- Add agentsight-enforcer logging.

## 0.12.0

### Features
- Add `--no-ebpf` trajectory-only trace mode for environments where eBPF is unavailable.
- Support the DashScope/Bailian native protocol. (#2954)
- Add Kubernetes DaemonSet packaging. (#2976)
- Monitor agent resource usage. (#3005)
- Ground causal attribution in deterministic, re-checkable evidence. (#3011)
- Add an LSM-only agent file-delete guard profile and report its capability. (#2979)
- Add a vendored ABI guard, one-click audit protection, and Dashboard linkage. (#2775)

### Fixes
- Avoid cgroup OOM during database purge by skipping VACUUM. (#2916)
- Merge Anthropic SSE token usage across events. (#2920)
- Warn when the data directory is on overlay storage. (#2977)
- Resolve PID lookups through a configurable procfs root.
- Keep conversation counts scoped by session. (#3014)
- Preserve tool results for causal attribution.
- Synchronize ActPlane policy deltas and prevent fail-open enforcement. (#2979, #3016)
- Reject oversized enforcement file paths instead of truncating them.
- Extract container IDs from systemd cgroup driver layouts. (#2997)
- Recognize modern OOM-kill dmesg format. (#3001)
- Resolve the PID namespace before seeding BPF `cap_task` state. (#3022)
- Pin Dockerfile RPM package selection to `TARGETARCH`. (#3025)

### Docs
- Document `--no-ebpf` trace mode.

## 0.11.2

### Features
- Show historical agent activity. (#2817)

### Fixes
- Re-attach stale SSL uprobes after kernel-side consumer deregistration so capture self-heals without a restart. (#2792)
- Restart agentsight after an OOM kill. (#2925)
- Bound probe event channel by bytes to cap memory under event bursts. (#2925)
- Align CLI and dashboard with docs and de-duplicate the guide sidebar. (#2899)
- Trim oversized db by row fraction. (#2872)
- Trim interruption db to size limit. (#2823)
- Keep binding API state consistent. (#2794)

### Docs
- Expand the AgentSight user guide into task pages. (#2825)

## 0.11.1

### Fixes
- Keep null-session interruptions in breakdowns so per-session and per-conversation counts always sum to the total. (#2796)
- Add Bun SSL_do_handshake prologue pattern so Claude Code (Bun builds >= 2.1.113) LLM traffic is captured. (#2782)

## 0.11.0

### Changes
- Version bump for RPM packaging; no functional changes since 0.10.3.

## 0.10.3

### Features
- Add semantic search to the sessions list. (#2733)
- Add dsh plugin for DeepSeek Harness.
- Group agents by type in discovery.
- Add filtered_count to agent-health.
- Add /api/docs route inventory.
- Show LLM latency metrics in agent health cards. (#2586)

### Fixes
- Drop hardcoded minus on savings. (#2754)
- Classify analyzer calls as internal. (#2755)
- Return savings rates as fractions. (#2732)
- Match tool args in loop detection. (#2719)
- Probe companion components for dashboard capabilities. (#2696)
- Scope graceful reap to worker agents.
- Count no_port gateways as healthy in dashboard. (#2694)
- Print deadloop hint on fresh install.
- Reload config on SIGHUP.
- Fill provider and session_id in audit events.
- Demote decompress fallback to debug level.
- Exclude sftp-server from agent discovery.
- Correct copilot cmdline rule typo.

## 0.10.2

### Features
- Enforce size limits on sqlite stores. (#2644)
- Make dashboard UI bilingual (en/zh). (#2612)
- Add raw packaging for immutable release artifacts. (#2550)
- Add LLM latency metrics API. (#2578)

### Fixes
- Match latency agents case-insensitively. (#2590)
- Address latency metrics CI follow-ups. (#2583)

## 0.10.1

### Features
- Bootstrap dashboard i18n with preferred browser locale matching.
- Add agentscope framework to agent discovery rules.
- Streamline component onboarding docs and clarify service startup.

### Fixes
- Treat pause_turn as a normal SSE finish. (#2320)
- Keep tool_call name across SSE continuation deltas.
- Report pids in the observer namespace. (#2360)
- Validate enforcer RPM payload, preserve RPM staging targets, and suppress repeated enforcer errors.
- Wrap extractor errors in JSON envelope and classify store errors by variant. (#2417, #2418)
- Dedupe unavailable state and refine dashboard metadata sync.

## 0.10.0

### Features
- Add case containment lifecycle with policy handoff, no-gap replacement, dedicated API, and dashboard views.
- Add system audit protocol, event storage, dashboard, and extract audit service into a dedicated crate.
- Add ActPlane risk enforcement with enforcer service, hardened file controls, secure lifecycle, and risk enforcement dashboard.
- Convert Codex rollout JSONL to ATIF and cover Codex routing and tool calls with tests.
- Report process metadata on raw HTTPS events and carry it in the generic envelope.

### Fixes
- Keep turn open on tool-call stop and add user_message_count to turn.id bucket key.
- Stream-decode fragmented zstd SSE responses.
- Improve QwenCode trace data accuracy and add QwenCode allow rules to agentsight config.
- Keep session across cosh restarts.
- Map cosh session temporary file writes. (#2080)

## 0.9.1

### Features
- Rework optimization dashboard views and add detour-based cost waste analysis.
- Move agent health and interruptions to a dedicated dashboard page.

### Fixes
- Extract cache tokens for OpenAI (`prompt_tokens_details.cached_tokens`) and DashScope (top-level `cached_tokens`) responses, and extract real user query from cosh-ng adapter prompt template.
- Capture Claude Code launched via absolute path or node wrapper.
- Skip `agent_crash` detection on clean exit and flush deferred GenAI events on agent exit.
- Skip message parse for non-LLM paths.
- Encrypt optimization config API key.
- Show known agent rules.

### CI
- Pin eBPF build to clang 15 and use dedicated runner pools.

## 0.9.0

### Features
- Add optimization analysis workspace, APIs, persistent analysis history, and dashboard pages for accuracy, performance, and cost reviews.
- Add Qoder trajectory collection, ATIF v1.7 export, batch analysis tooling, and subagent trajectory navigation with topology-style views.
- Add command-line discovery rules for CoshNG and normalize LLM event attribution with command-line context.
- Add six new interruption types and fallback capture for unparsable LLM HTTPS traffic.

### Fixes
- Fix Anthropic SSE parsing, system prompt injection, and cache token accounting.
- Fix ATIF batch output to use the shared ATIF v1.7 schema and drop stale v1.6 paths.
- Fix optimization and trajectory collection edge cases, including stale conversation anchors and syscall tracepoint probe attach.
- Fix dashboard empty states, error banner wording, auth loopback handling, and session navigation behavior.
- Make raw HTTPS FFI output opt-in and skip duplicate SSE message parsing for OpenAI and Anthropic streams.

### Changed
- Group optimization dimension analyses under per-target run roots and represent parallel LLM calls as ATIF subagent trajectories.
- Slim and gate default SLS output so trace content is not uploaded unless explicitly enabled.

## 0.8.1

### Fixes
- Replace `lock().unwrap()` with poison-safe `unwrap_or_else` for mutex recovery.
- Correct SSL library attribution from aws-lc/BoringSSL to OpenSSL 3.x.
- Add Claude process name to BoringSSL classification.
- Preserve user config on schema migration instead of overwriting.
- Don't auto-overwrite invalid JSON configs; record process pid not thread tid in ns pid helper.
- Downgrade high-frequency event logs from debug to trace to reduce noise.

### Tests
- Expand unit tests for handlers, interruption store, and token store.
- Add poison-recovery tests for mutex `unwrap_or_else` changes.

## 0.8.0

### Features
- Add dashboard token-based authentication with file-only auth config.
- Add LAN/public IP address display and Chinese output in dashboard CLI.
- Add ECS security group guide and metadata integration to dashboard.
- Add conversation grader API and dashboard controls.
- Add `COSH_SESSION_ID` export for per-run session correlation.
- Auto-upgrade stale configs via `schema_version`.

### Fixes
- Fix Codex SSL capture and SSE token extraction.
- Fix false interruption signals.
- Persist idle streams and tool results to avoid snapshot loss.
- Detect SSE stream errors explicitly.
- Restrict `/health`, `/metrics`, and server auth to localhost/file-only config.
- Remove `hf-hub` git fork from default build dependency.
- Fix IMDSv2 token fetching, probe deadlines, and ECS metadata deduplication.
- Fix RPM build to copy `agentsight.json` into source tarball.
- Address clippy `single_match`, nested if-let, and architecture boundary issues.

### Tests
- Add dashboard mock HTTP and unit tests for coverage gate.
- Add `build_output` and `public_address` tests.
- Mark probe tests as `#[ignore]` for CI ECS runners.

## 0.7.1

### Fixes
- Improve severity labels and agent sidebar UX.
- Show all verdicts in the summary command.
- Sync component.toml version with package version.

## 0.7.0

### Features
- Add Codex CLI adaptation with three-tier SSL probe attach (symbol table → byte pattern → offset table) and cross-chunk SSE continuation buffer.
- Add security observability dashboard and server proxy for agent threat visibility.
- Add memory optimization with bounded event buffers, feature flags (`features.*`) and configurable runtime limits (`runtime_limits.*`).
- Add `container_id` to `AgentsightLLMData` for container-level attribution.
- Derive `session_id` from process environment variables and request metadata instead of message content.
- Add `call_kind` classification (chat / completion / embedding / tool_use) to GenAI semantic events.
- Add `--exclude` filter to `agentsight audit` CLI for noise reduction, and show non-streaming LLM calls in audit output.
- Add unified `agentsight summary` command for one-shot status overview.
- Enhance token savings page with baseline comparison, strategy breakdown, line-level diff highlighting and optimization tips.
- Upload skill metrics via SLS Logtail exporter.
- Improve agent health UX: role badges (P1/P2), TTL-based cleanup, process-ancestry grouping, and Session ID help tooltip.
- Filter client processes from health API to reduce dashboard noise.
- Add anolisa component contract for RPM lifecycle integration.

### Fixes
- Fix sslsniff BPF verifier rejection on kernel 5.15 and add BPF load tests.
- Fix traced_processes BPF map leak causing uprobe attach failure after long runtime.
- Prevent duplicate uprobe `Link`s by retaining inodes in `traced_files` on detach.
- Decode compressed (zstd/brotli) SSE streams so Claude Code and similar agents are fully captured.
- Harden compressed SSE decode against partial chunk boundaries.
- Extract token usage from non-streaming and HTTP/2 responses.
- Fix namespace PID usage in udpdns and tcpsniff probes.
- Strip `/proc/{pid}/root` prefix for uprobe attach in containerized environments.
- Implement tiered SSL and tcpsniff ring buffer reservations to reduce dropped events.
- Clamp before mask in filewrite/udpdns BPF probes; cap stdout payload to `MAX-1`.
- Change cgroup gate to OR semantics and add `trace_cgroup` FFI interface.
- Tighten SSE truncation detection and write pending row for deferred GenAI calls.
- Respect dynamic sysom path in SLS exporter mode selection; replace removed `sysom_logtail_path` with `logtail_path` filter.
- Validate ring buffer size is power-of-two at startup.
- Wire feature flags and runtime limits to actual runtime code paths.

### Refactoring
- Split `genai/builder.rs` into 4 focused modules and `genai.rs` into 5 submodules.
- Bundle shared BPF maps into `SharedMaps` for reduced duplication.
- Extract background threads module with stop-signal support.
- Replace remaining `unwrap()` calls with `if-let` / `?` patterns.

### CI & Quality
- Add fmt, clippy, unit test coverage, and architecture boundary check CI gates.
- Add `clippy.toml` + `cargo-deny` for lint and supply-chain auditing.
- Add architecture boundary check script (`check-arch-boundary.py`).
- Add scoped AGENTS.md for FFI, unified orchestrator, and storage modules.
- Define Footprint Ladder for code surface growth control.
- Add `agentsight-code-review` and `pr-body` develop-skills.

## 0.6.1

- Add real-time agent_crash detection in trace mode.
- Add OOM crash detection.
- Add cgroup-level event filtering with v1/v2 compatibility.
- Support QwenCode skill discovery via per-user home scanning.
- Support SLS Logtail activation reversible via dynamic path.
- Support bridging ilogtail `SLS_LOG_PATH` into config via token-collector switch.
- Default `traceEnabled` to false to drop conversation content from SLS by default.
- Drop `gen_ai.system_instructions` from SLS uploads when `traceEnabled=false`.
- Refactor session_id and conversation_id derivation from response_id instead of message content.
- Fix CJK deadloop detection, `kill()` error check, and SIGKILL escalation.
- Fix SQLite read/write contention via VACUUM optimization.
- Fix rpm-build.sh agentsight build failures.
- Fix allow log path re-init on repeated new+start.

## 0.6.0

- Add deadloop detection and auto-kill mechanism for runaway agent processes.
- Add retry storm detection and `/metrics` interruption counters.
- Add BPF-layer HTTP protocol filter and wildcard capture (`*`) for unknown IP/port targets.
- Add client-side hybrid encryption for sensitive message fields.
- Add `traceEnabled` configuration toggle with SLS upload layer enforcement.
- Add HTTP domain rules resolved to tcpsniff BPF map via DNS.
- Add default DashScope HTTPS rule and `anolisa_release` module.
- Add FFI interface for `tcp_targets` and `input_delta` config.
- Add CO-RE compatibility to UDP DNS probe for kernel 6.0+.
- Support runtime SLS logtail path via config hot-reload.
- Expand interruption types and add logtail export.
- Restructure config to `https`/`http` rules.
- Refactor query `stats.db` by `tool_use_id` and unify savings display.
- Refactor load encryption public key from `agentsight.json`.
- Fix decode HPACK Huffman headers.
- Fix BoringSSL probe attachment, FFI event delivery, and chunked-body panic.
- Fix preserve initial SSE chunk in event-stream responses.
- Fix `c_char` / BPF comm portability (i8 vs u8).
- Remove dead code and deprecated APIs.

## 0.5.0

- Add Claude Code support including SSL probe attach for BoringSSL, Anthropic SSE thinking/tool_use content blocks, and `message.id`-based session correlation.
- Add tcpsniff probe for plain HTTP traffic capture with configurable IP/port filtering (disabled by default with empty `tcp_targets`).
- Add User-Agent based agent detection with `comm` fallback for simplified agent matching.
- Add UDP DNS probe for agent discovery (replacing TLS SNI probe) with QNAME parsing moved to userspace.
- Add TLS SNI probe module and refactor discovery to config-driven rules.
- Add connection scanner for pre-established LLM API connections.
- Add `tools` field to `AgentsightLLMData` FFI struct, passed through as raw JSON.
- Add container PID namespace support in BPF traced process filtering and event emission.
- Add agent matching rules and reduce BPF ring buffer to 32MB.
- Add `uid` field to SLS logs with `OnceLock` cache and startup validation.
- Support profile-based installs.
- Fix `duration_ns` calculation in LLM data.
- Fix SSL probe cleanup of stale inodes on process exit.
- Fix BPF verifier `-E2BIG` issues by removing nested `#pragma unroll` in `udpdns.bpf.c` and masking `payload_len` on older kernels.
- Fix skill extraction for Hermes agent architecture.
- Fix Node.js `process.title` change handling in OpenClaw matcher.

## 0.4.0

- Add HTTP/1.1 request body reassembly for fragmented SSL writes.
- Add skill metrics analysis with cosh filesystem-based discovery.
- Add SLS upload and Logtail file exporter for GenAI events.
- Add hermes agent matcher for LLM process discovery.
- Detect uv Python static OpenSSL in SSL sniffer.
- Remove AK/SK-based SLS direct upload, keep Logtail file export.

## 0.3.1

- Fix simplify agent_crash detection and fix multi-process dedup. (#411)
- Fix use SqliteConfig for audit CLI db path. (#399)
- Fix hide Cosh from agent health UI and remove keepalive support. (#401)
- Fix API endpoint table in AGENTS.md. (#397)

## 0.3.0

- Add interruption detection system with drain mechanism and dashboard integration. (#315)
- Add token savings page and API endpoint for optimization visualization. (#310)
- Add compounded token savings and request count tracking. (#320)
- Add C FFI API with cbindgen header generation. (#306)
- Add filewatch and filewrite eBPF probes for file access monitoring. (#308, #309)
- Support SysOM AK/SK GenAI capture for cosh. (#305)
- Use LLM API response_id as trace_id and add conversation_id field. (#304)
- Resolve session_id from agent's own session via ResponseSessionMapper. (#303)
- Fix interruption CLI and align conversation_id naming. (#318)
- Fix cosh session_id recognition by supporting snake_case response_id. (#307)
- Fix wrong tool call id in token savings compounding. (#316, #317)
- Fix standardize call_id, add tool_call_ids column. (#319)
- Fix session_id and response_id mapping in genai builder and storage. (#321)
- Fix token savings display in conversation list. (#322)
- Fix cache agent name by pid for dead process resolution. (#358)
- Fix remove custom db path and use default paths. (#359)
- Support nightly docker image build in CI. (#302)

## 0.2.2

- Support starting backend-server for dashboard with AgentSight service.
- Fix dashboard frontend dynamic width for multiple display-size.

## 0.2.1

- Add `/usr/lib/copilot-shell` path to CoshMatcher for agent discovery. (#190)
- Add 200MB size limit for `genai_events.db` to prevent unbounded growth. (#211)
- Remove `/api/stats` endpoint returning incorrect data. (#197)
- Extract audit from HttpRecord and filter non-LLM calls. (#196)
- Always show comparison data when `--compare` flag is used in token queries. (#194)
- Fix incorrect `discover` command in README documentation. (#191)
- Remove breakdown command and keep token consumption commented. (#193)
- Replace deprecated `MemoryLimit` with `MemoryMax` in systemd service file. (#181)

## 0.2.0

- AgentSight Dashboard web UI with real-time monitoring interface. (#74)
- Agent health monitoring with offline alerting and hung process dashboard restart. (#158)
- One-click navigation from dashboard to ATIF trace analysis page. (#116)
- /metrics endpoint to expose standard Prometheus-format data. (#134)
- Support for HTTP 2.0 protocol. (#147)
- Support to build RPM package. (#166)
