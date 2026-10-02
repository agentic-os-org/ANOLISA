# Changelog

## Unreleased

### Fixes
- Let `agentsight-enforcer` recover by itself when an upgrade leaves an
  enforcement layout the new binary cannot reuse: the stale layout is now
  detected and replaced automatically, so file-delete guard and enforcement
  health no longer stay down until someone removes the old layout by hand.
  (#3445)

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
