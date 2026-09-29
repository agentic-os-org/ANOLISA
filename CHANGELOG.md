# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.4] - 2026-09-10

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.12.0 |
| agentsight | 0.12.1 |
| tokenless | 0.8.1 |
| agent-memory | 0.2.7 |
| os-skills | 0.6.3 |
| anolisa | 0.3.11 |
| skillfs | 0.4.2 |
| ws-ckpt | 0.4.5 |
| cosh-ng | 0.24.1 |

> **Note:** copilot-shell, os-skills, skillfs, and ws-ckpt are unchanged
> since v1.3; they are listed to show the complete stack composition.

### Highlights

- **cosh-ng**: Updated to v0.24.1, routes natural-language and path-like input to the Agent in Enhanced Assisted Zsh while preserving ordinary Shell behavior and the user's terminal customizations, users get a more natural Zsh experience without rebuilding their existing setup (#3004, #3156)
- **agent-sec-core**: Updated to v0.12.0, applies configured policy checks consistently to legacy cosh and cosh-ng Skill calls while reducing false positives, users get consistent Skill enforcement and fewer incorrect security findings across both shells (#2871, #2928)
- **agentsight**: Updated to v0.12.1, adds collection without eBPF, native DashScope support, Kubernetes packaging, and more resilient long-running capture, operators can retain accurate observability in restricted and production environments (#2954, #2976, #3135, #3147)
- **tokenless**: Updated to v0.8.1, adds QwenPaw support and reversible compression for large JSON payloads, build logs, and CSV/TSV data, agents can reduce more kinds of context and retrieve omitted content precisely when needed (#3047, #3052, #3067, #3075, #3089)
- **anolisa**: Updated to v0.3.11, adds QwenPaw adapter management and ktuner discovery when the configured repository provides it while improving install, recovery, and audit behavior, administrators get more trustworthy lifecycle status and component-index previews without requiring root (#2084, #3075, #3127, #3141, #3142, #3158)
- **agent-memory**: Updated to v0.2.7, improves OpenClaw installation, corpus retrieval, configuration validation, and source-build setup, agents can share persistent memory with more reliable setup and recall (#2560, #3149, #3155, #3177, #3187)

### Updated

- **cosh-ng**: Updated to v0.24.1, improves prompt routing, terminal redraws, history recall, custom Tab behavior, login-profile PATH loading, credential redaction, and Hook trust visibility while removing the system OpenSSL requirement, users can keep their existing shell configuration and receive reliable Agent assistance without losing commands, output, or sensitive-history protection (#2967, #2983, #2996, #2999, #3004, #3030, #3050, #3058, #3156)
- **agent-sec-core**: Updated to v0.12.0, applies the same Skill policies across legacy cosh and cosh-ng, reduces scanner false positives, reports every integrity outcome through `skill-ledger` and `check`, and speeds up container health checks, users get consistent enforcement, clearer verification results, and faster readiness checks (#2707, #2871, #2879, #2928)
- **agentsight**: Updated to v0.12.1, adds collection without eBPF, native DashScope and Kubernetes deployment support, Agent resource monitoring, and cosh-ng traffic capture while improving attribution accuracy, tool-result retention, enforcement synchronization, Token accounting, stream completion, duplicate suppression, startup recovery, and automatic capture recovery, operators can trust timelines and usage totals while keeping monitoring and protection running through long-lived workloads and transient failures (#2916, #2954, #2976, #2979, #3005, #3011, #3016, #3081, #3087, #3135, #3147, #3191)
- **tokenless**: Updated to v0.8.1, adds QwenPaw integration, reversible reduction for large JSON collections, build logs, and CSV/TSV output, omitted-content retrieval through existing Shell tools, and trace correlation while fixing installation and compatibility issues across supported hosts, agents can use less context across more tool results and recover the exact omitted data when needed (#2249, #3009, #3047, #3052, #3067, #3068, #3075, #3085, #3089, #3094)
- **anolisa**: Updated to v0.3.11, adds QwenPaw adapter lifecycle management and ktuner discovery from configured repositories, permits declared config edits without false corruption reports, records hook-modified files correctly, avoids false pending operations after package conflicts, enables component-index previews without root, and improves cleanup, logs, dry-run, and audit guidance, administrators can inspect and recover installations with state that better reflects the actual system (#2084, #2618, #2922, #2926, #2994, #3075, #3118, #3127, #3141, #3142, #3158)
- **agent-memory**: Updated to v0.2.7, negotiates OpenClaw installation capabilities, distinguishes permission and configuration failures, returns readable and accurately windowed corpus results, validates configuration before binary discovery, and prepares Node.js and npm dependencies for source builds, agents and operators get more reliable installation, diagnosis, and memory recall (#2560, #3149, #3155, #3177, #3187)

### Compatibility

- **Tokenless Protocol v2**: `tokenless compress` now accepts only `before_model`, `pre_tool`, `post_tool`, and `retrieve` lifecycle requests; Protocol v1 and `tokenless mcp serve` have been removed. Upgrade Tokenless Core, CLI, SDKs, and adapters together and declare recovery capability explicitly to avoid protocol or retrieval mismatches (#2978, #3068).
- **Tokenless Rust APIs**: Replace `TokenlessRuntime::compress` with the Runtime lifecycle methods. Direct response-compression callers must migrate from the removed `tokenless-pipeline` crate and `tokenless_schema::ResponseCompressor` to the Runtime or `tokenless-compressors` APIs (#2974, #2978).
- **Tokenless Python and AgentScope APIs**: Replace the removed `ModelRequest`, `ToolCall`, `ToolResult`, and `ToolResponseCompressor` types with typed `before_model`, `pre_tool`, `post_tool`, and `retrieve` lifecycle requests. AgentScope integrations must add `ToolContract` metadata for custom tools and expose one static retrieval tool (#2986, #3029).
- **Tokenless retrieval configuration**: Custom `retrieve_tool_name` values must satisfy the tool-name rules. Lifecycle schema compression now requires an authorized static retrieval tool and an available Stash; without both, hooks preserve the original schemas. Recovering omitted content also requires a reachable CLI or static tool and a host that can replace the current tool result, including Claude Code 2.1.121 or newer. Existing `<<tokenless:HASH>>` markers remain readable (#2978, #2995, #3029, #3052, #3068).
- **Tokenless adapter configuration**: OpenClaw now uses `post_tool_enabled` for PostTool optimization; its former response, TOON, skip-tool, and shell-tool policy settings have been removed and no longer control compression. Remove those obsolete settings and configure `post_tool_enabled` when upgrading. DeepSeek Harness users must also remove adapter-specific thresholds and tool lists and use the shared compression policy. OpenClaw remains limited to lossless transcript updates without same-turn replacement or retrieval (#3009, #3036).
- **Tokenless statistics fields**: Dashboards and integrations must replace `seam` and `compressor_chain` with `content_origin`, `applied_operations`, and `recoverability`. The legacy SQLite columns remain for compatibility, but new records no longer populate them, so queries that keep using the old fields can return empty or incorrect reports after upgrading (#2978).
- **AgentSight timestamps**: Kernel event timestamps are now calibrated to host wall-clock time instead of container uptime. If NTP or an administrator steps the host wall clock while kernel events are queued, affected historical wall-clock timestamps cannot be reconstructed; persisted records are unaffected. Mark each wall-clock adjustment when analyzing timelines across it (#3128).
- **agent-sec-core Python dependency**: Python deployments and source builds now require `cryptography` 50.0.1 or newer. Refresh the environment from the exported requirements before upgrading custom installations (#3015).
- **agent-memory source builds**: Building the OpenClaw adapter from source now requires Node.js 20 or newer and npm. The unified user-mode build provisions them automatically; system-mode builds report missing tools during preflight (#3187).
- **anolisa RPM previews**: On DNF 4 systems, RPM install `--dry-run` requires `sudo` and may refresh repository metadata while checking solver conflicts. The check now runs before recovery journals are created (#3158).
- **anolisa editable configs and state format**: Allowing content edits to raw-installed `type = "config"` files does not change update or uninstall semantics, so back up customized configs before either operation. The new `kind = "config"` state records are unreadable by older CLI versions; before downgrading, also back up state and convert affected owned-file entries to `kind = "file"` to restore content-digest validation (#3142).

## [1.3] - 2026-08-31

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.11.1 |
| agentsight | 0.11.2 |
| tokenless | 0.7.14 |
| agent-memory | 0.2.6 |
| os-skills | 0.6.3 |
| anolisa | 0.3.8 |
| skillfs | 0.4.2 |
| ws-ckpt | 0.4.5 |
| cosh-ng | 0.22.2 |

> **Note:** copilot-shell and agent-memory are unchanged since v1.1; they are
> listed to show the complete stack composition.
>
> **Note:** agent-sec-core follows a release-branch flow, so `main` still
> shows 0.11.0; the shipped 1.3 stack uses the `sec-core/v0.11.1` tag, and
> the entries below describe behavior on that tag rather than on `main`.

### Highlights

- **cosh-ng**: Updated to v0.22.2, added native shell integration with `Shift+Tab` Shell-only switching and card prefixes that mark output ownership, users can run a hook-free shell while still seeing which subsystem produced each line (#2759, #2832)
- **agent-sec-core**: Updated to v0.11.1, rebuilt the prompt scanner in Rust with updatable rule packs and an optional deep-analysis backend, and narrowed the invisible-character rule, users get faster prompt scanning with far fewer legitimate emoji and multilingual prompts flagged as critical injections (#2409, #2531, #2699, #2900)
- **agentsight**: Updated to v0.11.2, restores model-traffic capture on its own when it goes stale and now recognizes Bun-built Claude Code, users keep continuous observability without restarting the collector (#2782, #2792)
- **tokenless**: Updated to v0.7.14, added the unified `tokenless compress` entry point plus net-savings and Retrieve attribution in `stats summary`, adapters make at most one subprocess call and users can read estimated net token savings (#2844, #2885)
- **ws-ckpt**: Updated to v0.4.5, added k8s sidecar deployment (#2034, #2965) and a guarded checkpoint protocol with identity-fenced snapshots, users can checkpoint containerized workspaces and verify checkpoint state after a crash
- **skillfs**: Updated to v0.4.2, added Kubernetes sidecar deployment and optional mutual HMAC-SHA256 authentication for control and notify sockets, non-privileged workloads can consume a FUSE skill view across container namespaces (#2057, #2449)

### Updated

- **cosh-ng**: Updated to v0.22.2, added a local gateway control plane exposed through `cosh agent task|doctor|run`, bounded transcript memory and a 32 MB `run_command` output cap, sub-millisecond interactive echo, automatic discovery of system extensions outside the package-managed root, and `/hooks enable|disable` layer disambiguation, and fixed terminal display and input routing (stray marker lines appearing after approved commands and slash commands, batch-pasted slash input, Han prompts containing paths, slash history recall, terminal left in raw mode after interrupts), security and audit gaps (hook-blocked commands running anyway in trust mode, approval batch races, malformed hook output silently passing tool calls through, fabricated exit codes for interrupted `precmd` markers), and packaging issues (RPM uninstall leaving a dangling login shell, gateway startup on systemd 255, `dnf --dry-run` false failures, missed awk `system()` calls in code scanning), users get a native shell with visible output ownership, bounded memory, and an auditable approval path (#2125, #2400, #2402, #2405, #2529, #2599, #2603, #2605, #2622, #2655, #2667, #2682, #2709, #2843, #2880, #2909, #2914, #2917, #2918, #2938, #2943, #2949, #2955, #2968)
- **agent-sec-core**: Updated to v0.11.1, added SkillFS HMAC peer authentication, an `agent-sec-cli capabilities` subcommand, and explicit `CHECKED`/`PASSED`/`FAILED` counters for `verify`, and stopped read-only system Skills from failing batch scans, placeholder `set-policy`/`rotate-keys` from reporting success, the daemon health check from over-reporting readiness, and non-loopback model service URLs from being accepted, users can audit Skills in cross-container deployments and trust CLI verification results (#2356, #2493, #2875, #2876, #2892, #2893, #2906)
- **agentsight**: Updated to v0.11.2, added historical agent activity views, semantic session search, a bilingual dashboard, LLM latency metrics, and store size limits, and fixed model-traffic capture that did not recover on its own, missing restart after the collector was killed for memory use, unbounded memory during event bursts, and interruption breakdowns that did not sum to the total, users keep long-running observability with bounded storage and self-healing capture (#2578, #2612, #2644, #2733, #2792, #2796, #2817, #2925)
- **tokenless**: Updated to v0.7.14, added the `anolisa-tokenless` Python wheel with framework-neutral lifecycles, AgentScope and DeepSeek Harness integrations, Gemini `functionDeclarations` schema compression, and a configurable array tail window, and fixed Codex double compression and inconsistent small-payload TOON handling, agents on more frameworks save tokens and can restore truncated payloads through the runnable command embedded in the marker (#2433, #2507, #2581, #2627, #2663, #2866, #2869, #2885)
- **anolisa**: Updated to v0.3.8, added verified prebuilt CLI archives for Linux x64/arm64 and macOS arm64, a native DSH adapter driver, container-runtime telemetry, and schema v2 target-based availability, and fixed raw installs expanding `${VAR}` in rendered content, `--quiet` adapter output, `--dry-run` forget and restart previews, and systemd template instances left running after uninstall, users can install a standalone CLI per platform and preview operations without side effects (#2533, #2580, #2603, #2642, #2752, #2762, #2774, #2883, #2903)
- **os-skills**: Updated to v0.6.3, added the `anolisa-component(os-skills)` RPM capability, users can run `anolisa upgrade` for OS Skills even when the repository component index is unavailable (#2576)
- **ws-ckpt**: Updated to v0.4.5, added k8s sidecar deployment with a bilingual guide (#2034, #2965) and a guarded checkpoint protocol, and fixed a memory leak that eventually exhausted the daemon (#2554), loop-device checkpoint latency under concurrent IO (up to 5x lower) (#2523), orphaned images and loop devices after a failed bootstrap plus silent startup exits (#1956), `config --global` writes the daemon never loaded (#2813), and intermittent bootstrap failure when all loop devices are in use (#2965), users can checkpoint in containers with lower latency and actionable startup diagnostics
- **skillfs**: Updated to v0.4.2, added Kubernetes sidecar deployment, mutual HMAC-SHA256 socket authentication, an optional Alibaba Cloud Linux 4 sidecar image, and bounded backoff for startup reconciliation against a late notify daemon, and fixed categorized Skills not being found on flat normal-mode mounts, non-privileged workloads can consume an authenticated Skill view that converges automatically after a daemon restart (#2057, #2449, #2777, #2787, #2790, #2901)

## [1.2] - 2026-08-14

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.10.1 |
| agentsight | 0.10.1 |
| tokenless | 0.7.6 |
| agent-memory | 0.2.6 |
| os-skills | 0.6.2 |
| anolisa | 0.2.19 |
| skillfs | 0.4.0 |
| ws-ckpt | 0.4.2 |
| cosh-ng | 0.16.1 |

> **Note:** copilot-shell, agent-memory, skillfs, and ws-ckpt are unchanged
> since v1.1; they are listed to show the complete stack composition.

### Highlights

- **cosh-ng**: Updated to v0.16.1, consolidated one-shot agent requests into `/agent` and converged the cosh-core and cosh-shell runtime paths with explicit protocol negotiation, users get one command for single agent requests and identical behavior from either runtime entry point (#2403, #2441)
- **agent-sec-core**: Updated to v0.10.1, unified hook policy controls so code scanning, prompt scanning, and observability are independently environment-gated across agent integrations, users can enable each protection per deployment without editing hook scripts (#2141, #2199, #2239)
- **agentsight**: Updated to v0.10.1, corrected turn boundaries and session continuity across cosh restarts, reclassified pause events as normal completions rather than interruptions (#2320), and added Codex trajectory conversion plus a dashboard that follows the browser locale, users get accurate cross-runtime trajectories in their own language
- **tokenless**: Updated to v0.7.6, added the OpenCode adapter and moved the Qoder adapter to native plugin and hook conventions, agents on both runtimes get command rewriting plus schema and response compression applied in place of the original tool output
- **anolisa**: Updated to v0.2.19, added package-family backend mapping for raw installs, adapter change notices after updates, and 2 GiB integrity degradation, administrators can install on minimal RPM/DEB hosts without large components being reported as damaged (#2018, #2271, #2314)

### Updated

- **cosh-ng**: Updated to v0.16.1, added a raw packaging interface with cross-target build validation and portable macOS launchers, and fixed clock-skew input stalls, lenient streaming response decoding, sensitive file writes, raw-mode leaks on exit, predictable temporary paths, first-match-only slash hints, and CJK line wrapping, users get reproducible archives and a shell that wraps East Asian text correctly and leaves no terminal state behind (#2176, #2209, #2211, #2357, #2361, #2410, #2411, #2446)
- **agent-sec-core**: Updated to v0.10.1, added OpenClaw code-scanner block mode, wider prompt-scan inbound field coverage, read-only Skill analysis, raw Skill directories in ledger checks, manifest authentication before Skill package loading, and session/run filters for events queries, users can block risky code, inspect unpackaged Skills, and query security events by session (#2044, #2132, #2185, #2201, #2242, #2277)
- **agentsight**: Updated to v0.10.1, added Codex trajectory conversion to ATIF, process attribution on captured model traffic with ids resolved in the observer namespace (#2360), and dashboard localization, and fixed turns closing early when a tool call ended, pause events misclassified as interruptions (#2320), truncated streaming responses, QwenCode trace accuracy, sessions lost across cosh restarts, and unmapped cosh session temporary file writes (#2080), users get accurate cross-runtime trajectories in their browser locale
- **tokenless**: Updated to v0.7.6, allowed `TOKENLESS_DATA_DIR` to point outside the user home, hard-disabled Tool Ready pre-call checks and blocking, and fixed duplicate JSON Schema stashing, dry-run settings overridden by environment variables, and `retrieve` appending a trailing newline, agents recover stashed content in one retrieval and are no longer blocked by incorrect readiness results (#2380, #2386, #2396, #2399, #2425, #2434, #2487)
- **anolisa**: Updated to v0.2.19, added adapter change notices after `anolisa update`, Qoder native plugin lifecycle support, Codex hook trust persistence, `OPENCLAW_STATE_DIR` handling, and the standard JSON envelope for legacy commands, and migrated telemetry to `SLS_PROJECT_PREFIX`, users can manage adapters across frameworks and parse every JSON surface the same way (#2018, #2221, #2260, #2281, #2319, #2337)
- **os-skills**: Updated to v0.6.2, added the `ktuner` skill for deterministic kernel diagnosis, tuning, and rollback, removed legacy OpenClaw and Hermes adapter scripts, and documented authenticated Skill Ledger recovery, users get rule-based tuning advice they can apply and roll back in one step (#1172, #1278, #2185)

## [1.1] - 2026-08-08

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.8.0 |
| agent-sec-core | 0.9.0 |
| agentsight | 0.9.1 |
| tokenless | 0.7.3 |
| agent-memory | 0.2.6 |
| os-skills | 0.6.1 |
| anolisa | 0.2.15 |
| skillfs | 0.4.0 |
| ws-ckpt | 0.4.2 |
| cosh-ng | 0.14.0 |

> **Note:** os-skills remains at v0.6.1; it did not change in this release and
> is listed to show the complete stack composition.

### Highlights

- **cosh-ng**: Updated to v0.14.0, added resumable workspace sessions, MCP management, runtime introspection, and DashScope prompt caching, agents can recover long-running work and extend capabilities while reducing repeated prompt cost (#1546, #1592, #1778, #1949, #2046)
- **agentsight**: Updated to v0.9.1, added optimization and trajectory analysis together with case containment, system audit, and ActPlane risk enforcement, users can diagnose agent quality and cost while investigating and containing risky behavior (#1728, #1789, #2051)
- **agent-sec-core**: Updated to v0.9.0, expanded prompt, PII, code, and observability hooks across Qoder CLI, Qwen Code, and Codex, users can apply consistent security policies across supported agent runtimes (#1473, #1480, #1495, #1501, #1529, #1535)
- **tokenless**: Updated to v0.7.3, added reversible compression with MCP retrieval plus Cosh-NG response and command compression, agents can reduce model context while recovering truncated payloads on demand (#1285, #1376, #1669)
- **anolisa**: Updated to v0.2.15, added exact-version RPM and raw installs, file-metadata repair, and interactive progress, administrators can select published versions and recover installation drift with visible operation phases (#1700, #1740, #1987, #2036)

### Updated

- **copilot-shell**: Updated to v2.8.0, added the consent-gated `/ktuner` command, exported `COSH_SESSION_ID`, and reused compatible cosh-ng authentication during switching, users can tune hosts, correlate subprocess activity, and move between shells with less setup (#1279, #1491, #1951)
- **agent-sec-core**: Updated to v0.9.0, added Qoder CLI and Qwen Code hook coverage, Codex PII and observability hooks, custom PII rules, and Chinese prompt-injection detection, users receive broader protection across prompts, tool calls, skills, and agent output (#1473, #1495, #1501, #1522, #1554)
- **agentsight**: Updated to v0.9.1, added ATIF v1.7 trajectory analysis, accuracy/performance/cost workspaces, case containment, system audit, and risk dashboards, users can trace multi-agent behavior and act on optimization or security findings (#1728, #1789, #1828, #2051)
- **tokenless**: Updated to v0.7.3, added stash-backed reversible compression, an MCP retrieval server, Cosh-NG compression, and macOS/Qwencode adapter support, agents can save tokens across more runtimes without permanently losing compressed content (#1285, #1376, #1669, #1894, #1964)
- **agent-memory**: Updated to v0.2.6, added synchronous indexing plus focused-query and OR-ranked recall fallbacks, agents can retrieve newly captured memories from verbose or stopword-heavy prompts (#1520, #1574, #2047)
- **anolisa**: Updated to v0.2.15, added exact-version RPM and raw installs, telemetry controls, macOS arm64 npm delivery, file-metadata repair, and phase-based progress, users can select published versions across Linux and macOS, control reporting, and repair Linux installation drift (#1619, #1700, #1740, #1962, #1987, #2036)
- **skillfs**: Updated to v0.4.0, added Hermes nested-skill compatibility, configurable read-time transforms, authenticated live-source resolution, and hardened permission boundaries, agents can consume adapted skill views while source mutations remain safely controlled (#1146, #1484, #1517)
- **ws-ckpt**: Updated to v0.4.2, added telemetry gating and automatic recovery of orphaned pre-init backups, users can recover workspaces after interrupted initialization without stale backup state (#1509, #1601)
- **cosh-ng**: Updated to v0.14.0, added session recovery, MCP tools, slash-command introspection, and prompt-cache observability, agents can resume complex work, extend capabilities, and diagnose cache savings (#1530, #1546, #1592, #1778, #1949, #2046, #2075)

## [1.0] - 2026-07-06

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.6.1 |
| agent-sec-core | 0.7.0 |
| agentsight | 0.7.1 |
| tokenless | 0.6.1 |
| agent-memory | 0.2.1 |
| os-skills | 0.6.1 |
| anolisa | 0.1.20 |
| skillfs | 0.3.2 |
| ws-ckpt | 0.4.1 |
| cosh-ng | 0.11.0 |

### Highlights

- **anolisa**: Updated to v0.1.20, delivered unified CLI gateway with full component lifecycle and adapter orchestration, users can install/update/diagnose all components with a single command
- **cosh-ng**: Updated to v0.11.0, completed Core/Shell separation and AI-augmented terminal, Agent can execute structured OS operations deterministically across distros
- **agent-memory**: Updated to v0.2.1, added user data sovereignty and 4-type memory classification, users can query/forget/control auto-captured memories
- **tokenless**: Updated to v0.6.1, added compression toggle with A/B testing and QwenCode adapter, users can quantify Token savings per strategy without affecting task execution

### New Components

- **anolisa**: First release v0.1.16, built unified CLI gateway managing component install/update/uninstall with dual-backend (RPM + Raw), users can deploy the entire ANOLISA stack with `anolisa install --all`
- **cosh-ng**: First release v0.11.0, implemented deterministic Agent-OS interface with 5-crate workspace, Agent can execute cross-distro structured system operations via stable API
- **skillfs**: First release v0.3.2, built FUSE virtual filesystem for agent skills with view-based SKILL.md exposure, Agent can discover and load skills from a mounted directory

### Updated

- **agent-memory**: Updated to v0.2.1, added sovereignty tools (about/forget/consent), AMA export/import, 4-type classification, and incremental consolidation resilient to SIGKILL, users can control memory retention and migrate memories across agents
- **tokenless**: Updated to v0.6.1, added compression on/off toggle with dry-run mode, SLS JSONL telemetry default-on, and QwenCode adapter, developers can A/B test compression strategies and monitor Token savings in SLS dashboard
- **agentsight**: Updated to v0.7.1, added Token saving visualization (strategy pie chart + line-level diff), security dashboard, and container/K8s full support, users can visually assess which optimization saves the most Tokens
- **copilot-shell**: Updated to v2.6.1, added `/model` dialog for multi-provider switching and SLS session telemetry (32-field JSONL), users can freely switch LLM providers without losing configuration
- **agent-sec-core**: Updated to v0.7.0, added Skill Ledger integrity chain with GPG signing workflow and Prompt Scanner, users can audit skill security status and get confirmation prompts before risky operations
- **os-skills**: Updated to v0.6.1, added ANOLISA Guide knowledge skill (13 official docs) and OpenClaw pre-check with bootstrap, Agent can reference accurate product documentation in responses
- **ws-ckpt**: Updated to v0.4.1, added auto-cleanup scheduling and TOML config hot-reload, users can set retention policies that take effect without restarting the daemon

### Changed

- Documentation governance established via `specs/documentation-standard.md`
- Bilingual naming convention unified to `_zh.md` (migrated from legacy `_CN.md`)

## [0.6] - 2026-06-12

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.4.1 |
| agent-sec-core | 0.5.0 |
| agentsight | 0.5.0 |
| tokenless | 0.4.1 |
| agent-memory | 0.1.0 |
| os-skills | 0.5.0 |
| cosh-ng | 0.1.0 (MVP) |

### Highlights

- **agent-memory**: First release v0.1.0, delivered sandboxed filesystem MCP memory server, Agent can persistently store and retrieve context across sessions via BM25 search
- **tokenless**: Updated to v0.4.1, added Hermes Agent plugin and Tool Ready 4-stage pre-check, Agent environments are automatically validated before tool execution to avoid wasted retries
- **agentsight**: Updated to v0.5.0, added Skill-level Token metrics and Hermes support, users can pinpoint which Skills consume the most Tokens

### New Components

- **agent-memory**: First release v0.1.0, built 19-tool MCP server with namespace isolation and BM25 background index, Agent can read/write/search persistent memory in a sandboxed filesystem
- **cosh-ng**: First release (MVP), completed production-ready functionality for deterministic OS operations, Agent can execute structured commands with predictable output format

### Updated

- **tokenless**: Updated to v0.4.1, added Hermes adapter runner and Tool Ready mechanism (4-stage env pre-check as cosh extension), Agent tool calls are pre-validated reducing Token waste from environment failures
- **agentsight**: Updated to v0.5.0, added Skill-dimension Token/call metrics and Hermes matcher with SSL support, users can see per-Skill Token breakdown in the dashboard
- **agent-sec-core**: Updated to v0.5.0, added PIIChecker (output PII detection + desensitization) and Skill Scanner (text/code scan + lifecycle trigger), Agent output containing sensitive information is automatically intercepted
- **copilot-shell**: Updated to v2.4.1, added cross-session auto memory extraction and hook reason visibility in UI, users can see exactly why a security hook blocked an operation

## [0.5] - 2026-05-28

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.4.0 |
| agent-sec-core | 0.4.0 |
| agentsight | 0.4.0 |
| tokenless | 0.4.0 |
| os-skills | 0.4.0 |

### Highlights

- **tokenless**: Updated to v0.4.0, added Hermes plugin and Tool Ready environment mechanism, Agent tool execution failures due to missing dependencies are prevented before Token consumption
- **agent-sec-core**: Updated to v0.4.0, delivered PIIChecker and Skill Scanner first version, Agent output is scanned for sensitive information leakage

### Updated

- **tokenless**: Updated to v0.4.0, developed Hermes Agent plugin with Tool Ready 4-stage env pre-check and history compression, Agent runtime dependencies are auto-verified before execution
- **agent-sec-core**: Updated to v0.4.0, added PIIChecker for output PII detection and Skill Scanner baseline capabilities, users are protected from unintentional sensitive data exposure
- **agentsight**: Updated to v0.4.0, added Skill-level metrics display, users can view Token consumption grouped by Skill
- **os-skills**: Updated to v0.4.0, added Nightly automated test coverage, skill quality is continuously validated

## [0.4] - 2026-05-13

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.3.0 |
| agent-sec-core | 0.4.1 |
| agentsight | 0.4.0 |
| tokenless | 0.3.0 |
| os-skills | 0.3.0 |
| ws-ckpt | 0.2.0 |

### Highlights

- **agent-sec-core**: Updated to v0.4.1, established Skill security full lifecycle with Prompt Scanner ask policy, users receive confirmation prompts before Agent executes risky instructions
- **tokenless**: Updated to v0.3.0, built 4-suite Benchmark comparison baselines, developers can quantify Token savings across different Skill/OS environments
- **ws-ckpt**: Updated to v0.2.0, expanded snapshot management commands, users can auto-clean historical snapshots by count or age policy

### Updated

- **agent-sec-core**: Updated to v0.4.1, integrated Prompt Scanner into cosh hook and OpenClaw plugin with ask strategy, users get interactive confirmation before dangerous operations
- **tokenless**: Updated to v0.3.0, built batch-concurrent Benchmark platform with comparison reports, developers can one-click benchmark and compare Token savings across configurations
- **agentsight**: Updated to v0.4.0, optimized resident process memory footprint, 2C2G small-spec instances can run observability stably
- **copilot-shell**: Updated to v2.3.0, adapted SWEBench evaluation framework, developers can execute code-fix tasks and verify pass rates via cosh
- **ws-ckpt**: Updated to v0.2.0, enriched snapshot CRUD capabilities, users can manage workspace checkpoints with flexible retention policies

## [0.3] - 2026-04-30

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.2.1 |
| agent-sec-core | 0.3.0 |
| agentsight | 0.3.1 |
| tokenless | 0.2.0 |
| os-skills | 0.3.0 |
| ws-ckpt | 0.1.0 |

### Highlights

- **tokenless**: Updated to v0.2.0, delivered command rewriting and TOON context compression, CLI output Token consumption reduced by 60–90%
- **agentsight**: Updated to v0.3.1, added Token saving Dashboard and Agent anomaly diagnostics, users can visualize savings and detect Agent interruptions
- **agent-sec-core**: Updated to v0.3.0, added Skill Ledger integrity tracking and Prompt Scanner, every Skill's signature chain is auditable end-to-end

### New Components

- **ws-ckpt**: First release v0.1.0, built btrfs-based workspace checkpoint daemon, Agent can create sub-millisecond snapshots and instantly rollback filesystem state

### Updated

- **tokenless**: Updated to v0.2.0, added command rewriting via RTK and TOON context compression, Agent CLI interactions consume 60–90% fewer Tokens
- **agentsight**: Updated to v0.3.1, added Token saving Dashboard (session/time-range stats) and Agent interrupt detection with drain mechanism, users can monitor savings trends and get alerted on Agent failures
- **agent-sec-core**: Updated to v0.3.0, added Skill Ledger full lifecycle (check/certify/bypass/status/audit) and Prompt Scanner with jailbreak detection, users can track and enforce Skill integrity policies
- **copilot-shell**: Updated to v2.2.1, added extension architecture (command extension + system Hook + instant activation), Skill marketplace integration, and session export (Markdown/HTML/JSON), users can extend cosh capabilities via plugins and export conversation history
- **os-skills**: Updated to v0.3.0, added Skill marketplace listing, Hermes install skill, and utility skills (xlsx/pdf-reader/image-gen/humanizer), users can discover and install skills from a marketplace

## [0.2] - 2026-04-15

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.0.4 |
| agent-sec-core | 0.2.0 |
| agentsight | 0.2.2 |
| os-skills | 0.2.2 |
| tokenless | 0.1.0 |

### Updated

- **agentsight**: Updated to v0.2.2, added Token consumption observability with precise Tokenizer counting, users can view per-message Token breakdown in real time
- **copilot-shell**: Updated to v2.0.4, added independent auth (STS/ECS RAM Role) and Skill marketplace browsing, users can authenticate without AK/SK and discover available skills
- **os-skills**: Updated to v0.2.2, added SysAdmin skills (Linux IO/network/load diagnostics), Agent can independently diagnose common OS performance issues
- **tokenless**: First release v0.1.0, built Skills-level benchmark test cases, developers can compare Token consumption across different Skills quantitatively

## [0.1] - 2026-03-30

### Component Versions

| Component | Version |
|-----------|--------|
| copilot-shell | 2.0.1 |
| agent-sec-core | 0.1 |
| agentsight | 0.1 |
| os-skills | 0.1 |

### New Components

- **copilot-shell**: First release v2.0.1, built AI-powered terminal assistant with Tab completion, /bash mode, sudo support, and hook security, users get an AI-native CLI experience on first login
- **agent-sec-core**: First release v0.1, delivered Skill signature verification, security sandbox, and system hardening, Agent operations run in a controlled least-privilege environment
- **agentsight**: First release v0.1, built eBPF-based zero-intrusion observability probe, users can monitor LLM API calls and Token consumption without modifying Agent code
- **os-skills**: First release v0.1, curated system administration, SysOM, DevOps, and cloud skills, Agent can autonomously perform common OS operations

### Security

- Skill full-link encryption with digital signatures
- Hardware-level security sandbox for risk isolation
- Identity authentication and integrity verification for Skill calls

---

For detailed changelogs of individual components, see:

**User Entrypoint**
- [copilot-shell](deprecated/copilot-shell/CHANGELOG.md)
- [cosh-ng](src/cosh-ng/CHANGELOG.md)
- [anolisa](distribution/anolisa/CHANGELOG.md)
- [os-skills](src/os-skills/CHANGELOG.md)

**Token Saving**
- [tokenless](src/tokenless/CHANGELOG.md)

**Runtime**
- [agent-memory](src/agent-memory/CHANGELOG.md)
- [skillfs](src/skillfs/CHANGELOG.md)
- [ws-ckpt](src/ws-ckpt/CHANGELOG.md)

**Agent Observability**
- [agentsight](src/agentsight/CHANGELOG.md)

**Agent Security**
- [agent-sec-core](src/agent-sec-core/CHANGELOG.md)
