# Changelog

## 0.13.0

**V2 Policy & Daemon Runtime**

- Added V2 daemon Policy, Scope, and Binding CRUD APIs with root-only authorization. (#3062)
- Added V2 CLI commands for Policy, Scope, and Binding management through the daemon. (#3097)
- Added AgentSight policy adapters, client integration, and binding reconciliation. (#3103)
- Added bounded concurrent reconciliation workers so one binding failure cannot block the daemon. (#3194)

**V2 Security Operations**

- Added the V2 daemon-backed Bash and Python regex Code Scanner with compatible structured results. (#3203)
- Added the V2 `capabilities` command with V1-compatible environment capability reporting. (#3243)
- Added V1-shaped JSONL and SQLite persistence for accepted V2 code-scan events. (#3246)
- Added shared scan lifecycle recording and allowlisted telemetry sinks for V2 security operations. (#3300)

**Skill Ledger Runtime**

- Skipped unmanaged read-only raw user Skills during default batch initialization and scanning. (#3183)
- Unified Skill Ledger CLI and configuration inputs around supported names, initialization, and policies. (#3253)

**Build & Packaging**

- Delivered the V2 daemon as a hardened systemd system service. (#3217)

**Skills**

- Added the bundled `pii-checker` Skill for PII and credential scans with optional redaction. (#3241)

**Testing & CI**

- Added installed V2 RPM end-to-end coverage for CLI, daemon, plugins, and service contracts. (#3162)

**Documentation**

- Added AARM registry and OWASP Agentic Top 10 security-control mappings. (#3227)

**Maintenance**

- Flattened the V2 crate layout and updated internal references. (#3319)

## 0.12.0

**cosh & Cosh-NG Hook Integration**

- Normalized legacy cosh and Cosh-NG Skill invocation payloads in the cosh Skill Ledger hook so Cosh-NG invocations follow the configured policy instead of bypassing the check. (#2871)

**Skill Ledger Runtime**

- Tightened six Skill scanner detector patterns to remove deterministic false positives and bumped the static scanner identifier to `cisco-static-only-0.1.1`. (#2928)
- Completed the `skill-ledger` and `check` help output with all eight integrity statuses and their production conditions. (#2707)

**Daemon Runtime**

- Added a POSIX shell `daemon.health` probe that avoids Python cold start for container health checks. (#2879)

**Rust v2 Framework**

- Added the v2 foundation and policy type crates as the base for the Rust security core migration. (#3007)
- Added the v2 daemon service framework with UDS server, request dispatch, and shutdown handling. (#3040)
- Added the v2 PAP service with Policy, Scope, and Binding CRUD over a repository abstraction. (#3027)

**Build & Packaging**

- Raised the `cryptography` runtime floor to 50.0.1 and refreshed the lock and exported requirements. (#3015)

**Documentation**

- Added design docs for the current daemon protocols and the v2 framework requirements. (#2881)

## 0.11.1

**Prompt Scanner**

- Restricted the invisible-character injection rule so emoji ZWJ sequences, Persian ZWNJ, and leading BOM no longer score as critical injections. (#2900)
- Refused non-loopback model service base URLs derived from the environment in both the Rust and Python model service paths. (#2893)
- Reported `daemon.health` prompt_scan state as untracked instead of unconditionally ready now that scanning runs in-process. (#2892)
- Dropped the dead prompt model preload environment flag and the obsolete scan-prompt protocol document. (#2886)

**Skill Ledger Runtime**

- Skipped host-backed read-only system Skills in batch `scan --all` and default `init` instead of failing them. (#2906)
- Rejected the placeholder `set-policy` and `rotate-keys` commands so they no longer report false success. (#2876)

**Asset Verification**

- Reported explicit `CHECKED` / `PASSED` / `FAILED` counters and a distinct zero-candidate outcome for `agent-sec-cli verify`. (#2875)

**Security Events & CLI**

- Enforced owner-only permissions on files created by the CLI. (#2873)

**Documentation**

- Documented the five hidden `agent-sec-cli` integration commands and their contract caveats. (#2887)
- Documented the rule-only L1 detection boundary and how to read `pass` together with degraded fields. (#2895)

## 0.11.0

**Prompt Scanner**

- Rewrote prompt scanner core in Rust for native performance. (#2409)
- Added ATR (Agent Threat Rules) rule packs and reduced compile time. (#2531)
- Added Warden-Gen L2 backend option for prompt scanning. (#2699)
- Routed Rust native extension logs to Python logging framework. (#2640)
- Corrected warmup documentation to describe model availability checks and silenced per-invocation scan mode logging in the Codex, cosh, and Qwen Code prompt scanner hooks. (#2745)

**Hermes Hook Integration**

- Dropped Hermes synthetic warning injection; PII and Skill Ledger policies now use native observe/block boundaries. (#2712)

**PII Checker**

- Refined PII detection to reduce JWT and Email false positives and aligned user notices across all host adapters. (#2443)

**Skill Ledger Runtime**

- Added SkillFS HMAC peer authentication for cross-container Skill Ledger deployments. (#2493)

**Security Events & CLI**

- Added `agent-sec-cli capabilities` subcommand for environment-based hook configuration inspection. (#2356)
- Set observability hook timeout to 10s and CLI default timeout to 5s. (#2346)
- Added security observability skill for structured event and session report queries. (#2245)

**Documentation**

- Corrected and restructured user docs to align with 0.10.x shipped behavior. (#2456)

## 0.10.1

**OpenClaw & cosh Hook Integrations**

- Piped prompt text through stdin instead of command-line arguments in the OpenClaw and cosh prompt scanner hooks. (#2445)

**Security Events & CLI**

- Validated events query parameters in the CLI and the daemon security query handlers. (#2451)

## 0.10.0

**Agent Hook Policy Controls**

- Added code scanner enable flags for agent hooks. (#2001)
- Unified hook policy controls across agent integrations. (#2141)
- Added an observability hook environment toggle. (#2199)
- Restored scanner mode environment variable names. (#2212)
- Aligned code scanner hook flags across supported agent integrations. (#2229)
- Added environment-based prompt scanner gating. (#2239)

**OpenClaw Hook Integration**

- Added block mode support for the OpenClaw code scanner hook. (#2242)

**Prompt Scanner**

- Widened prompt scan inbound text field coverage. (#2277)

**Skill Ledger Runtime**

- Added read-only skill analysis. (#2044)
- Included raw skill directories in skill ledger checks. (#2201)
- Authenticated manifests before loading skill package contents. (#2185)

**Security Events & CLI**

- Added session and run filters to agent-sec-cli events queries. (#2132)

**Raw Packaging**

- Added component-owned raw package build targets and archive validation. (#2133)
- Updated raw hooks to use the bundled Python launcher. (#2255)

## 0.9.0

**Qoder CLI & Qwen Code Hook Capability Expansion**

- Added Qwen plugin and observability hooks. (#1473)
- Added Qoder CLI hook framework support. (#1480)
- Added Qoder prompt injection scanner hook integration. (#1529)
- Added Qwen prompt scanner hook integration. (#1538)
- Added code scanner hook integration for Qoder CLI and Qwen Code. (#1535)
- Added Qoder CLI Skill PreToolUse skill ledger checks for user and project skills. (#1552)
- Added Qwen PII hooks. (#1559)
- Added Qwen skill ledger hook integration. (#1561)
- Added Qoder CLI observability hook integration. (#1580)
- Unified Qwen Code hook trace context handling. (#1738)

**Codex & OpenClaw Hook Integrations**

- Added observability capability in the Codex plugin. (#1495)
- Added Codex PreToolUse PII checker hook integration. (#1501)
- Showed OpenClaw policy hints in hook responses. (#1525)

**Scanner & Policy Engine**

- Skipped prompt model downloads when the model cache already exists. (#1467)
- Added custom PII regex rules. (#1522)
- Satisfied L1-L3 telemetry requirements. (#1527)
- Added Chinese prompt-injection and jailbreak rules covering instruction override, authority escalation, encoding evasion, and role-play framing. (#1554)
- Unified model resources into handles to make prompt scanning resource lifecycle more consistent. (#1553)
- Made prompt scan mode configurable through environment variables. (#1620)
- Hardened audit, PII, and notify hook behavior. (#1649)

**Skill Ledger Runtime**

- Isolated the skill ledger worker for more reliable hook execution. (#1492)
- Resolved canonical skill roots for skill ledger checks. (#1558)
- Exposed skill ledger verdicts to hook callers. (#1577)

**Build & Packaging**

- Moved prompt-scan benchmarks to a standalone repository to keep the CLI package lean. (#1557)
- Rejected stale CLI wheels during packaging and runtime validation. (#1651)
- Replaced and restarted the daemon service when upgrading RPM packages. (#1681)

**Testing & CI**

- Fixed OpenClaw E2E test dependencies by excluding ML packages. (#1631)
- Fixed skill-ledger E2E test execution on macOS. (#1643)

**Documentation**

- Centralized user guides and added documentation lint CI. (#1586)

## 0.8.0

**Build & Packaging**

- Installed the Codex plugin during source builds so source deployments include the same Codex integration as packaged installs. (#1302)
- Updated source build scripts for the sec-core install flow. (#1348)
- Fixed system-mode source builds by placing uv-managed Python under the shared sec-core library directory and adding a system install smoke check. (#1400)

**Code Scanner**

- Added sensitive file path rules for common agent credentials to block API key exposure. (#1401)

**OpenClaw Plugin**

- Hardened OpenClaw deploy compatibility handling and covered deployment edge cases with unit tests. (#1358)
- Added an OpenClaw plugin cross-version E2E matrix that validates packaged plugin loading, Gateway flows, policy behavior, and observability across supported OpenClaw hosts. (#1372)

**Documentation**

- Added bilingual agent-sec-core user guide documentation and documentation maintenance rules. (#1311)
- Documented OpenClaw plugin deployment, compatibility, and upgrade guidance. (#1370)

## 0.7.1

**Prompt Scanner**

- Degraded scan-prompt to fast mode when model unready; rewrote DENY to WARN and enriched degraded reason with diagnostics. (#1258)

**Skill Ledger**

- Clarified skill ledger fallback warnings and sanitized finding summaries. (#1240)
- Tamed ledger reconcile noise and typed live-root skip errors. (#1232)

**Security Observability**

- Added observability mapping for new pii_scan at before_tool_call & after_tool_call. (#1229)

## 0.7.0

**Codex Plugin — Full security integration for OpenAI Codex**

- Added codex-plugin with code scanning, prompt scanning, skill ledger, and PII checking hooks. (#1074)
- Supported packaging codex-plugin into RPM. (#1138)
- Fixed codex-plugin paths in Makefile and CI for correct RPM install verification. (#1165)

**Code Scanner**

- Added code-scanner LLM mode for AI-assisted security analysis. (#1108)
- Added code-scanner static rules for expanded coverage. (#1033)

**Prompt Scanner**

- Added L4 multi-turn intent detection with ollama model service. (#1060)
- Routed prompt scan to daemon and added prompt model preload for reduced latency. (#786)
- Controlled prompt scan call to use daemon by env variable. (#933)

**Skill Ledger — Activation daemon and policy engine**

- Added Skill Ledger activation daemon for background integrity monitoring. (#857)
- Added runtime activation resolver for skill trust decisions. (#826)
- Added skill ledger activation policy for configurable enforcement. (#944)
- Updated skill ledger activation and event contracts. (#983)
- Updated Skill Ledger hook defaults and reconcile notify behavior. (#1086)
- Aligned skill ledger hooks across all agent platforms. (#1135)
- Resolved Skill Ledger FUSE and unmanaged roots handling. (#1141)
- Fail-open unsupported Hermes skill ledger scenarios. (#1155)

**Daemon & Telemetry**

- Added daemon service with systemd integration and RPM build support. (#1090)
- Exposed SQL query endpoint at daemon for observability queries. (#1042)
- Enhanced daemon logging including requests and jobs. (#871)
- Added telemetry schema definition and SLS JSONL writer. (#977, #1008)
- Passed agent_name to telemetry data for multi-agent identification. (#1032)
- Added logging system for structured agent-sec-cli output. (#651)
- Added security daemon socket fallback under `/run/user/<uid>` for user-scoped deployments. (#1129)

**PII Scanner**

- Extended PII scanning coverage with additional pattern detectors. (#925)

**Security Observability**

- Added session report command for post-session security summaries. (#703)

**Sandbox**

- Converged sandbox trigger rules for consistent enforcement. (#979)

**Adapter & Build**

- Added ANOLISA CLI component.toml for adapter manifest integration. (#1067)
- Added systemd-rpm-macros as RPM build dependency. (#1156)

## 0.6.0

**Self-Protection — Tamper-resistance for agent-sec-core itself**

- Added self-protect code-scan rules that block disabling/uninstalling agent-sec plugins on OpenClaw and Hermes. (#692)
- Optimized self-protect rules in code-scan to eliminate false positives on prefix-matched plugin names and cover Hermes uninstall/rm patterns. (#710)

**Prompt Scanner**

- Unified prompt-scan warning format across cosh-extension, hermes-plugin, and openclaw-plugin with structured fields (threat type, risk level, interception stage, model confidence). (#709)

**Agent-Sec-CLI**

- Added daemon process for agent-sec-cli to amortize startup latency across hook invocations. (#677)

**Adapter & Manifest**

- Added standalone ANOLISA adapter entry `anolisa-for-openclaw` to package sec-core OpenClaw adapter scripts and drive install/detect/uninstall via the adapter manifest. (#549)
- Added Hermes adapter runner: refactored the OpenClaw entry into a target-agnostic `anolisa-adapter-runner` and added `anolisa-for-hermes` wrapper, with per-agent adapter directory layout under sec-core. (#617)
- Centralized sec-core adapter manifest parsing across adapter scripts and moved the manifest under the cli package. (#617)

**OpenClaw Integration**

- Normalized OpenClaw state directory handling: use `OPENCLAW_STATE_DIR` for adapter filesystem state, unset `OPENCLAW_HOME` when invoking the OpenClaw CLI, and aligned plugin install/list/uninstall handling. (#641)

## 0.5.0

**PII Scanner — Personal information leak detection**

- Added PIIChecker scan CLI with text/file input, regex/validator-based detection, redaction, and security middleware integration. (#525)
- Added PIIChecker hooks for cosh and OpenClaw with stdin-based input passing. (#539)
- Added Hermes PII checker hook. (#556)
- Fixed scan-pii module mode detection via subprocess. (#540)

**Security Observability — Agent run metrics & posture insights**

- Added security observability schema, metrics definition, and CLI with jsonl writer for agent runs. (#488)
- Added openclaw plugin for security observability. (#515)
- Added cosh hook for security observability. (#528)
- Persisted observability records to sqldb with CLI review command. (#544)
- Added observability plugin for hermes. (#553)
- Correlated security events with observability events and supported batch query. (#578)
- Respected trace-id filter in count queries. (#595)

**Hermes Plugin — AI Agent integration framework**

- Added hermes-plugin framework with abstract hook class and code scan capability. (#536)
- Added Hermes prompt-scan capability. (#579)
- Added Hermes PII checker hook. (#556)
- Added Hermes skill ledger hook. (#565)
- Added observability plugin for hermes. (#553)
- Supported correlation context in hermes agent plugin. (#590)
- Added hermes plugin install for rpmbuild and build from scratch. (#577)
- Stabilized Hermes skill-ledger warning delivery for non-pass skill checks. (#600)

**Correlation & Tracing Context**

- Unified caller tracing context across CLI, OpenClaw, and cosh with `--trace-context` JSON and SQLite schema v2. (#569)
- Supported correlation context in hermes agent plugin. (#590)
- Correlated security events with observability events. (#578)

**Skill Ledger**

- Integrated code-scanner with skill-ledger for unified security assessment. (#505)
- Updated skill ledger security interactions. (#529)
- Made openclaw skill ledger approval configurable. (#575)
- Added Hermes skill ledger hook. (#565)
- Refined skill ledger scan workflow and aligned documentation. (#529)
- Included skill-ledger e2e in install flows. (#573)
- Fixed skill-ledger hook scope limitation. (#497)
- Fixed managed skill dirs for discovery. (#510)
- Expanded home paths for skill-ledger. (#596)
- Hardened skill ledger recovery and key UX. (#575)

**Code Scanner**

- Added code-scan requireApproval config for openclaw. (#560)
- Added OpenClaw enableBlock hook policies. (#586)

**Security Middleware & Event System**

- Fixed TOCTOU race condition at sqldb read path. (#546)
- Made SQLAlchemy lazy import for non-DB subcommands. (#581)
- Lowered frequency for SQL maintenance operations. (#546)

**Prompt Scanner**

- Added Hermes prompt-scan capability via hermes plugin. (#579)
- Fixed warmup detection from error-string matching to file-based check. (#500)
- Fixed prompt text passing via stdin instead of argv. (#579)

**Toolchain & CI**

- Added build-all support with local space install for sec-core. (#527)
- Added hermes plugin install for rpmbuild and from-scratch build. (#577)
- Included skill-ledger e2e in install flows. (#573)
- Added adapter manifest for capability discovery. (#577)

## 0.4.0

**Prompt Scanner**

- Prompt scanner hook now asks user on missing model instead of fail-open. (#463)
- Added prompt injection detection benchmark dataset and evaluation toolkit. (#464)

**Security Middleware & Event System**

- Refactored security_events SQLite storage to SQLAlchemy ORM with multi-table extensibility and typed repositories. (#459)

**Skill Ledger**

- Fixed sign-skill auto-register config (exact awk match) and parse openclaw stdout unconditionally. (#445)
- Unified XDG paths under `agent-sec/skill-ledger` vendor namespace. (#445)
- Unified single-skill verify into structured result for consistent output. (#445)
- Converted integration tests from subprocess to Typer CliRunner. (#445)

**OpenClaw Integration**

- Registered plugin at openclaw gateway explicitly to support Gateway startup planning. (#446)

**Refactoring**

- Removed deprecated agent-sec-core skill directory; aligned README and spec with agent-sec-cli workflow. (#454)

**Toolchain & CI**

- Added coverage report for sec-core CI. (#431)
- Enabled rpmbuild and e2e test CI for main branch. (#432)

## 0.3.0

**Prompt Scanner — Multi-layer prompt injection & jailbreak detection**

- Added prompt injection/jailbreak detection scanner architecture with L1 rule engine (YAML-based) and L2 ML classifier (Prompt Guard 2). (#253)
- Integrated prompt scanner into cosh hook and openclaw plugin with security middleware lifecycle. (#261, #294)
- Added `list-scanners` command, improved CLI help, and made `--scanner-version` optional. (#284)
- Added prompt scan summary and backend tests. (#294)
- Added prompt-scanner skill definition. (#256)
- Added model warmup, audit logging, and comprehensive documentation. (#253)
- Stabilized batch scanning and verdict logic with thread-safe model loading. (#253)
- Unified prompt scanner response to use "ask" instead of "block". (#341)
- Added prompt-scanner e2e test suite and Makefile target. (#352)

**Code Scanner — Static code security analysis**

- Added code scanner component with rule-based detection for obfuscation, permission abuse, and more. (#234)
- Integrated code scanner into cosh hook (with ask decision support) and openclaw plugin adapter. (#234)
- Added code scanner CLI entry, error codes, and unit tests. (#234)
- Fixed code scan bugs and added e2e test. (#342)

**Skill Ledger — Skill integrity tracking and signing**

- Added skill-ledger CLI with middleware integration for skill integrity verification. (#252)
- Added skill-ledger skill definition. (#266)
- Added skill-ledger cosh hook for PreToolUse and openclaw-plugin capability. (#292, #281)
- Improved skill-ledger CLI and cleaned up imports. (#284)
- Restructured skill-ledger config defaults and documentation. (#296)
- Aligned skill-ledger tool name and added path validation. (#317)
- Reworked skill-ledger status, output, and check signing. (#335)
- Skill-ledger hook hardening, e2e suite, and posture integration. (#339)
- **Known limitation:** skill directory resolution assumes dir name matches SKILL.md `name` field; see #381.

**Security Middleware & Event System**

- Added security middleware framework with unified CLI entry point and metrics integration. (#121, #220)
- Added sqldb writer & reader with query command at CLI interface for security event persistence. (#254)
- Fixed cross-process event loss in SecurityEventWriter. (#226)
- Applied corruption whitelist to stop false-positive DB rebuilds. (#338)
- Added e2e test and fixed bugs revealed during testing. (#330)

**Linux Sandbox**

- Added sandbox guard and failure handler hooks. (#362)

**OpenClaw Integration**

- Added hook plugin for openclaw with integrated security scanning capabilities. (#242)
- Added jq requires for openclaw hook package. (#370)

**Cosh Extension Integration**

- Integrated with new cosh extension API and added builtin commands. (#302)

**Performance**

- Lazy-load ML dependencies to speed up non-ML subcommands. (#318)

**Toolchain & CI**

- Migrated Python toolchain to uv package manager and pinned Python 3.11.6. (#227)
- Added sec-core RPM build CI and adapted nightly build pipeline. (#295)
- Initialized code format check CI with python-code-pretty. (#229)
- Added e2e test in RPM build CI. (#369)

**Bug Fixes**

- Preserved seharden wrapper defaults. (#236)
- Removed dynamic import at middleware router. (#277)
- Improved missing loongshield guidance. (#289)
- Fixed build errors. (#288)
- Removed openclaw hook examples and fixed documentation. (#282)

## 0.2.0

- Added Hardened skill signing pipeline and added `.skill-meta` layout. (#129)
- Added `Cargo.lock` to version control. (#149)
- Added `make install-sandbox` target. (#68)
- Fixed bubblewrap version compatibility for `--argv0` option. (#112)
- Changed Refactor SKILL.md to executable protocol and align sub-skills. (#130)