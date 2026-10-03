# Changelog

All notable changes to the cosh-ng project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.26.0] — 2026-09-23

### Added
- Login sessions started by the Enhanced Bash host now carry a real login identity on Bash 4+: `shopt -q login_shell` reports yes, `$0` is `-bash`, and `/etc/profile` plus `~/.bash_profile` are read, matching a native `bash -l` session; set `shell.login_identity = false` to opt out, and the previous behavior is restored automatically on Bash 3.2 or when the capability probe fails (#3358)
- cosh-ng can now be installed on Intel Macs: the raw backend ships an x86_64 macOS archive with a minimum of macOS 11.0, and the public installer recognizes Intel Macs instead of failing or falling back to the ARM package (#3448, #3449)
- `cosh-cli checkpoint recover` can recover an interrupted initialization even when the original workspace path no longer exists, reporting the notices through `meta.warning` (#3356)

### Changed
- Diagnostic logging now defaults to the `info` level, so `~/.copilot-shell/logs/cosh-{shell,core}.log.<date>` records shell/core startup, core spawn, and error choke-points that previously surfaced only in the UI; explicit `COSH_LOG`, `RUST_LOG`, or `[logging] level` settings still win, and `debug: true` raises the effective level to `debug` (#3331)

### Fixed
- rtk-wrapped readonly commands whose only redirection suppresses stderr to `/dev/null` (e.g. `find /tmp -name '*x*' 2>/dev/null` wrapped as `env TOKENLESS_* /usr/bin/rtk …`) are auto-approved again in auto mode: the wrapper is peeled for classification while the wrapped form still executes, keeping RTK output compression active, and any deviation falls back to the approval card (#3436)
- When a Task created with `checkpoint = on` in an empty workspace fails against an older daemon, the Task now states why no snapshot was taken and offers clear next steps (upgrade ws-ckpt, add a file and resubmit, or select `checkpoint = off`) instead of failing without explanation (#3437)
- After a successful Task snapshot switch, new Core and Codex Tasks use the rebuilt workspace without a Gateway restart; if the workspace refresh cannot be established, new Runtime launches fail closed instead of using a stale view (#3438)

## [0.25.0] — 2026-09-20

### Added
- Managed Tasks are now end-to-end: submit persistent Core or Codex Tasks from the guided `/task` form, choose a checkpoint policy, reconnect to inspect progress, and safely preview, diff, and switch Task-owned snapshots through a Gateway-backed recovery flow (#2911)
- Slash command discovery inside `/agent`: typing `/` in the Composer offers prefix-filtered candidates with browsing and Tab completion, and submitted commands run through the normal handlers with confirmation cards (#3224)
- First-time provider setup is now automated: a conflict-free default name is derived from the template type and existing configs, and ECS RAM Role credentials are detected and submitted without manual confirmation through a bounded, cancellable probe (#3298)
- Readonly commands whose only redirection suppresses stderr to `/dev/null` (e.g. `find /tmp -name '*x*' 2>/dev/null`) no longer require approval in auto mode (#3208)

### Changed
- Declared `[[hooks.PreToolUse]]` config hooks now run by default without requiring `hooks.enabled`; explicitly disabled hooks warn and are removed before extension registration, and empty hook output still blocks the tool call by default (#3330)
- Enhanced sessions no longer print the ownership status line (`◇`/`◌`) above the prompt by default, keeping the shell's native prompt appearance; set `shell.status_symbols = true` or `COSH_SHELL_STATUS_SYMBOLS=1` to opt back in — when enabled, the symbol is published on its own line so narrow-terminal editing no longer loses CJK characters or misplaces the cursor (#3345, #3254)

### Fixed
- Lexical path-traversal spellings that resolve into `/proc`, `/dev`, or `/sys` are now blocked by the readonly safety check, closing a bypass where variants like `/../proc/version` slipped through (#2708)
- Quoted `/dev/null` redirection targets such as `2>"/dev/null"` are now classified as null redirections instead of redirection-write, so the approval card shows the correct reason (#2710)
- SysOM endpoint resolution now prefers a reachable in-VPC proxy, so ECS instances without public egress can reach SysOM; explicit overrides via `COSH_SYSOM_ENDPOINT` or the `sysom_endpoint` config keep the system proxy (#3188)
- Medium-risk tool requests whose names merely contain `HOOK:` as a substring are no longer misclassified as Hook requests, so the approval card offers the standard actions instead of the Hook action set (#3248)
- Valid token-plan credentials can again be saved through `/auth` when the gateway answers HTTP 400 on the model-retrieve route, while list-only providers still reject unverifiable credentials (#3276)
- zh-CN `/help` panel: the Registry group title is localized and the `/agent` summary wording corrected; en-US output is unchanged (#3363)
- RPM install/erase scriptlets now manage the `/etc/shells` entry safely: swapping between `cosh-ng` and `copilot-shell` no longer drops the login-shell registration the replacement still needs, updates are atomic and preserve file metadata, and install-time failures are reported instead of silent (#3367)

## [0.24.1] — 2026-09-09

### Fixed
- LLM token recording via agentsight broke silently when the Rust compiler version changed after the TLS library switch: cosh-core now exports a named probe symbol that agentsight attaches to by name, so capture stays stable across compiler upgrades (#3156)

## [0.24.0] — 2026-09-08

### Added
- In Enhanced Assisted Zsh sessions, slash-bearing, space-prefixed, and Han-leading path prompts are now routed to the Agent instead of falling through to the Zsh line editor, while ordinary shell input returns to Zsh unchanged once routing has been decided (#3004)
- The Tab key is handed back to the Zsh line editor before later typed input is processed, so custom Tab widgets keep working; retained input is replayed automatically without requiring another keypress, and user traps, key bindings, history, and prompt status are preserved (#3004)
- Default Bash Cosh sessions now include PATH additions from login profiles, so commands installed there are available in a new terminal; set `COSH_SHELL_BOOTSTRAP_PATH=0` to disable this startup check (#3050)

### Fixed
- Cookie headers assembled through braced shell parameter expansions (such as `${parameter#pattern}`) are now recognized as sensitive data and kept out of native shell history; ambiguous nested syntax is redacted as a whole command (#3004)
- Zsh aliases and functions whose names contain slashes no longer get mistaken for missing filesystem paths and routed to the Agent; they continue to run in the shell (#3058)

## [0.23.0] — 2026-09-02

### Added
- The `/hooks` slash panel now lists individual shell hooks with their source and trust state, matching how agent hooks are displayed (#2999)

### Changed
- cosh-ng no longer depends on the system OpenSSL runtime: HTTPS now runs on a pure-Rust TLS stack that still trusts the host OS certificate store, so Linux installs no longer require the openssl-libs / openssl1.1 system packages and source builds succeed on macOS without OpenSSL headers (#3030)

### Fixed
- Secret redaction now respects word boundaries: ordinary text such as `sk-hynix` or `npm_package_version` stays visible in local evidence, while short credentials next to CJK text and structured Cookie/Bearer/JWT forms are consistently kept out of native shell history and redacted from journals, no matter which shell or entry path produced the output (#2996)

## [0.22.3] — 2026-08-31

### Fixed
- A damaged or unterminated terminal control sequence no longer hides subsequent terminal output or blocks command-boundary detection: oversized fragments are discarded and parsing resumes at the next valid marker (#2967)
- Installing the prebuilt cosh-ng package now works on newer distributions such as Ubuntu 24.04: the portable artifact bundles its own OpenSSL instead of requiring the OpenSSL 1.1 runtime that newer systems no longer ship (#2982)
- Natural-language prompts that are deliberately kept out of Bash history are now recalled and re-submitted with their exact original text instead of falling through to the shell or disappearing from history recall; leftover fragments from wrapped hint lines no longer linger on the next prompt (#2983)
- An unknown command entered right after a successful one still shows its failure insight: the command-not-found event is now attributed to the current input instead of being dropped (#2983)
- Prompt lines keep exactly one assisted marker after redraws caused by agent interactions, cancellations, or terminal width changes, instead of duplicating or dropping it (#2983)

## [0.22.2] — 2026-08-28

### Fixed
- Approved Bash tool commands handed off to the native shell now display the approved command itself instead of the internal wrapper line, including on Bash 4.4 terminals where fragmented redraws previously leaked that wrapper; execution, approval, history, and exit status are unchanged (#2949)
- Slash commands entered directly, recalled with Up-arrow, or typed on narrow terminals now appear exactly once in the terminal before their result panel again, while the internal guard line stays absent from terminal output, history, and logs; sensitive slash input is no longer expanded into Bash xtrace output (#2955)
- Natural-language prompts containing file paths that are routed to the Agent remain visible as a request notice even when the model's first response event is a tool call, so the original request no longer disappears from the screen; sensitive input is redacted as a whole field (#2953)
- An external interrupt signal sent to the shell process no longer leaves the parent terminal in raw mode: terminal settings are restored while the signal exit status is still reported to callers (#2968)

## [0.22.1] — 2026-08-28

### Fixed
- The internal `builtin true __cosh_slash_guard__` sentinel line no longer appears in the terminal after an exact slash command in native Bash, including with verbose echo (`set -v`) enabled; slash routing, command execution, and history recall behave as before (#2943)
- Input that arrives pasted or piped in one batch together with a running command is no longer misrouted: slash-looking lines inside such a batch are delivered to the shell byte-for-byte instead of being intercepted as slash commands, keeping scripted pipelines intact (#2938)
- Command tracking and working-directory reporting keep working in directories whose names contain control characters, instead of dropping shell events (#2938)
- Headless one-shot runs with a prompt argument no longer hang for hours when an approval request goes unanswered: the request times out after 30 seconds and the run exits with a clear timeout error and a non-zero exit code (#2941)

## [0.22.0] — 2026-08-27

### Added
- Enhanced Assisted mode no longer relies on a global Bash DEBUG trap or forced tracing options; user-defined traps, shell options, and prompt hooks are preserved, and slash-command handoffs keep normal interactive shell semantics (#2832)
- System extensions installed outside the package-managed root (e.g. under `/usr/local/share/anolisa/extensions`) are now discovered automatically, so raw-installed sec-core and tokenless extensions load without a separate enable step; conflicting duplicate system installs stay disabled unless an exact source selection is persisted (#2909)

### Fixed
- With xtrace enabled, cosh's internal hook activity no longer appears in the trace, so dynamic marker, working-directory, and environment data are not exposed; user commands and user-defined hooks keep producing trace output as before (#2914)
- Slash-bearing Han prompts such as `打开./missing/SKILL.md` are routed to the Agent again instead of failing as a shell path, and the Agent now receives the correct prompt-time working directory, including after `cd` (#2918)
- Slash commands entered in the interactive shell are now persisted in Bash history so Up-arrow recall retrieves them correctly instead of an older command (#2917)

## [0.21.1] — 2026-08-26

### Fixed
- Registry slash commands such as `/skills list`, `/hooks`, and `/extensions` now discover project-level resources when no Agent turn has run yet and the process working directory differs from the shell cwd, e.g. after `cd` or daemon-style startup (#2686)
- Shell commands that produce very large output no longer exhaust memory: output is capped at 32 MB, oversized commands are stopped automatically, and the Agent sees a truncation notice with the start of the output instead of a bare error; when a hook's output exceeds the cap, the affected command stays blocked (safe side) instead of the hook's decision being silently ignored (#2880)
- Gateway tasks now start correctly on hosts running systemd 255, where the packaged service previously failed at startup because one of its security hardening settings is incompatible with that systemd version; workspace isolation is unchanged: task files stay confined to the workspace, host files remain read-only, and daemon state stays private to the service (#2843)

## [0.21.0] — 2026-08-25

### Added
- Add Native shell integration (`shell.integration = "native"`) for a hook-free shell and Enhanced Shell-only mode toggled with `Shift+Tab`, while keeping Enhanced Assisted as the default with visible prompt markers; slash commands, tool calls, permissions, and system notices now use distinct `/`, `*`, `!`, and `·` card prefixes so output ownership is visible at a glance (#2759)

## [0.20.0] — 2026-08-24

### Added
- Report anonymous operational telemetry directly to SLS when cosh-ng runs without the anolisa unified uploader; no user prompts, code, or conversation content is collected; opt out at any time by creating `~/.copilot-shell/telemetry_disabled` (per-user) or `/etc/anolisa/.telemetry_disabled` (system-wide) (#2715)
- Bind the local Gateway task profile to a closed `task-only-v1` manifest so mismatched Core/Gateway pairs are rejected before task input or side effects (#2728)

### Fixed
- Restore `/usr/bin/cosh raw <adapter>` to launch the TUI instead of failing with exit 127 after the transparency classifier misrouted the `raw` subcommand to bash (#2743)
- Route Han-leading natural-language prompts containing simple parameter references such as `$HOME` to the Agent, while keeping executable shell structures native so an intercept cannot alter adjacent command execution (#2746)
- Preserve the DEBUG trap across prompts on bash 4.2.x so command audit markers keep emitting after the first command (#2757)

## [0.19.0] — 2026-08-21

### Added
- Add a local gateway control plane with durable Task scheduling, runtime leases, and crash recovery, exposed through `cosh agent task ...` commands (#2603)
- Add direct ACP entrypoints `cosh agent doctor` and `cosh agent run` to verify and drive locally installed ACP adapters from outside the interactive Shell, with a once-only permission flow that prompts only on the local controlling terminal and records redacted evidence (#2603)

### Fixed
- Detect `system()` calls in awk programs even when whitespace or line continuations appear before the opening parenthesis (#2655)
- Map missing precmd status markers to -1 instead of fabricating exit 0, preserving ledger integrity for interrupted or forged markers (#2709)

## [0.18.0] — 2026-08-20

### Added
- Cap in-memory transcript growth in long-running interactive sessions by keeping bounded working windows while spooling full terminal output to session files (#2682)
- Enable MCP tools that require long-running task augmentation, returning actionable errors when tasks fail or time out (#2645)

### Changed
- **BREAKING**: `/usr/bin/cosh` now forwards non-TUI invocations directly to the configured shell; `cosh --version`, `cosh --help`, and `cosh <script>` outside an interactive terminal behave like the shell rather than opening the cosh TUI (#2625)

### Fixed
- In trust approval mode under the cosh-core driver, hook-blocked shell commands can no longer execute through the staging grace window (#2125)
- Approval decisions are now resolved correctly when an external component has already logged the initial request (#2402)
- Approving a hook-rewritten shell command no longer fails with "could not route this approval" when another request in the same batch was refused (#2667)
- Audit logs now include a paired cancellation resolution when a pending turn extension is superseded, instead of leaving an orphaned request entry (#2695)

## [0.17.2] — 2026-08-19

### Fixed
- Cap `run_command` output at 32 MB to prevent out-of-memory kills from runaway commands such as `dd if=/dev/zero` or `yes`; when stdout or stderr exceeds the limit, the process group is killed and an `OutputTooLarge` error is returned instead of growing unbounded (#2405)
- Make RPM `%post` /etc/shells registration idempotent by normalizing content and probing before appending, and add a `%preun` scriptlet that fails closed when users still reference `/usr/bin/cosh` as their login shell, preventing dangling shells after `rpm -e cosh-ng` (#2599)

## [0.17.1] — 2026-08-18

### Fixed
- Reduce interactive echo latency from p50 20 ms to sub-millisecond and eliminate high-frequency idle CPU polling in long-running sessions by making the raw relay and SIGINT paths event-driven (#2622)
- Keep allowed commands executing when best-effort audit storage writes fail (#2631)
- Allow approved pipelines whose single-quoted arguments contain quoted newlines, such as multi-line jq or awk scripts (#2638)

## [0.17.0] — 2026-08-17

### Added
- Interactive disambiguation panel for `/hooks enable|disable <id>` when the hook id exists in both shell and agent layers, letting users choose to toggle the shell hook, the agent hook, or both (#2400)

### Changed
- Skip redundant history processing during idle polling so long-running interactive sessions no longer repeat history-sized work while idle (#2546)

### Fixed
- Reject hook output with an explicit `"decision": null` as invalid hook output: the tool call is blocked by default, or passes through with a recorded `hook_failure` notice when the hook sets `fail_open = true`; emitting `{}` remains valid pass-through (#2529)
- Report `cosh pkg install/remove --dry-run` success on dnf-based systems instead of returning a backend error for installable or removable packages (#2605)

## [0.16.1] — 2026-08-14

### Fixed
- Improve CJK text wrapping so East Asian characters fill the full terminal line width and punctuation stays attached to adjacent text (#2446)
- Validate credentials, endpoints, and models before persisting `/auth` changes, keeping the auth panel open on failure (#2458)
- Treat empty successful output from extension hooks as valid instead of fail-closed (#2506)

## [0.16.0] — 2026-08-13

### Added
- Raw packaging interface for deterministic cosh-ng archives with cross-target build validation and portable macOS launchers (#2411)

### Changed
- Converge cosh-core and cosh-shell runtime paths, unify Claude and Qwen provider drivers, and add explicit shell/core protocol v1 negotiation with legacy fallback (#2403)
- Remove deprecated `/draft` slash command alias in favor of `/agent` (#2441)

### Fixed
- Use monotonic clock for input-wait timing to avoid clock-skew stalls (#2176)
- Fix panel-family hint card rendering (#2196)
- Decode SSE events per spec and fail loud on malformed input (#2209)
- Confine and guard sensitive file writes across core (#2211, #2378)
- Pass raw prompt text through core and shell without normalization (#2256)
- Fix review marker activation on Enter key (#2274)
- Narrow interactive-cancel detection and pair ledger finish after correlated intercept (#2352, #2353)
- Send raw-mode disable to stdout on drop to prevent terminal state leaks (#2357)
- Harden hooks across core and shell (#2359)
- Avoid predictable temporary file paths (#2361)
- Enforce byte cap in line truncation branch (#2370)
- List all slash hint matches instead of first-only (#2410)
- Map missing failed exit code to -1 sentinel for consistent error reporting (#2412)
- Expose runtime context in core for consistent state access (#2428)

## [0.15.0] — 2026-08-09

### Fixed
- Restore hint cursor after inline hints in shell (#2172)
- Support ID_LIKE fallback for OS distribution detection (#2200)
- List hook commands in `/help` output (#2208)
- Skip audit logs for service lifecycle actions (#2213)
- Support macOS-specific file reads in core (#2220)
- Serialize handoff state to prevent race conditions (#2226)
- Normalize apt search glob patterns (#2227)
- Recover context budget after compaction in core (#2244)
- Deduplicate hook notices in shell (#2259)

## [0.14.0] — 2026-08-04

### Added
- `/status`, `/about`, and `/stats` slash commands for runtime introspection (#1778)
- `/mcp` slash command for MCP server management (#1949)
- `/session list --all` to enumerate sessions across workspaces (#2139)
- DashScope prompt cache support to reduce token cost (#2046)
- `cached_tokens` observability for cache-hit diagnostics (#2075)
- Dynamic `max_tokens` by model in the OpenAI provider (#2165)
- Advertise `roots` capability in MCP client initialization (#2007)
- Hook tools and environment support in core (#1894)
- Surface tool-argument status and cap retries across core and shell (#1925)
- Auto-execute fully readonly compound commands in shell (#1959)
- Extend capped runs across core and shell (#2035)
- Improved auth menu across core and shell (#2062)
- Bound agent handoff input-waits in shell (#2168)

### Changed
- Terminal-agnostic multi-line prompt entry in shell (#1947)

### Fixed
- Shell handoff and hooks: preserve LLM input, drop stale handoff text, resume handoff fallback within a provider session, redact handoff evidence, close secret-redacted handoffs via one-time claim token, converge Han NL input ownership, route sensitive NL to agent, and run project hooks for send-to-shell (#1955, #2010, #2055, #2074, #2130, #2137, #2151, #2154)
- Approval lifecycle: guarantee terminal state with lifecycle ledger and last-resort timeout, rearm auth input, surface sandbox-bypass approval in trust mode, and reject zero idle timeout (#1934, #1939, #1968, #2116)
- Auth flows: step `/auth` back on ESC, list `/auth` in `/help`, and hint `/auth` on noauth startup (#1891, #1906, #2166)
- Slash command and prompt input: prevent slash echo duplication on intercept, let Up recall slash commands, intercept slash-bearing NL prompts, support soft newline in NL prompts, keep card submit type-ahead, normalize CSI-u backspace, and key ghost ownership by route (#1868, #1899, #1911, #1922, #1942, #1993, #2167)
- Shell rendering: stop extdebug leak into prompt hooks, highlight code-block syntax, reply in user language, compact skills list, and disable implicit pagers (#1849, #1904, #1910, #1921, #1998)
- Command risk and safety: assess all compound-command segments for risk, gate irrecoverable system-control commands, and classify interpreter risk (#1905, #2081, #2119)
- File IO hardening: reject placeholder writes, make file writes atomic, bound and confine read tools, restore blocking before drop-write, and treat fd-dup redirect as non-write (#1918, #2069, #2120, #2121, #2124, #2127)
- Shell recovery and drift: prevent recovery storms and use zsh preexec `$3` for drift (#2072, #2073)
- Audit logging: show hook context (#2082)
- Core runtime: preserve tool arguments, align compaction, show real session prompt, fail closed on emit error, and make truncation UTF-8-safe (#1844, #1847, #2003, #2005, #2118)
- Types, wire, and packaging: fix wire errors, drop cross-workspace dev-dep, align RPM identity, align bundle health checks, and add base-dir hint in skill tool (#1514, #1933, #1937, #1984, #2140)

## [0.13.0] — 2026-07-26

### Added
- Interactive session recovery via `/session`, `/resume`, and the `--resume` launch option (#1546, #1592)
- Workspace-scoped session persistence with schema versioning and legacy-session migration (#1546, #1592)
- MCP tool support for extensible agent capabilities (#1530)
- Contextual shell insight interactions (#1537)
- Secret redaction across core and shell layers (#1555)
- Diagnostic bundle export via `cosh doctor` (#1576, #1597)
- Extension platform in core and shell (#1583)
- Personalized prompt recommendations (#1606)
- Session compaction to manage persistence growth (#1668)
- PostToolUse response replacement and hook adaptation (#1669)
- Gated startup suggestions (#1671)
- Audit logging across core and shell (#1679)
- Improved session UX with slash command refinements and sysom `/auth` shortcut (#1726, #1813)
- Route unresolved natural-language input to Agent (#1742)
- Cancel active agent runs with ESC (#1761)
- Turn-scope batch approval consent (#1825)

### Changed
- Revert persisted credential encryption (#1748)
- Align task scope documentation (#1445)
- Stabilize core and shell test gates (#1699)
- Speed up raw CLI tests and fake stream pacing (#1797)
- Track cosh-shell test inventory at 2469 (#1827)

### Fixed
- Session command parsing, signal exits, slash argument handling, and prompt ghost ESC (#1632, #1634, #1636, #1663, #1724, #1843)
- Agent question interaction, suggestion controls, recommendation scopes, and tab redraw (#1725, #1741, #1749, #1758, #1821)
- Approval card layout, blocked-title alignment, and empty enter handling (#1786, #1788, #1838)
- Auth and trust hardening: validate providers, harden auth, preserve trust blocks, encrypt credentials, and expand paths (#1627, #1673, #1701, #1722, #1777, #1784, #1791, #1809, #1816, #1841)
- Audit logging fixes: kill trees, split compound commands, redact secrets, scope claims, and preserve export path (#1611, #1613, #1635, #1765, #1772, #1840, #1842)
- Core runtime stability: JSONL validation, tool selection errors, streamed state, ai-like tables, revision clock, free-text clearing, layout gates, and SysOM terminator (#1599, #1661, #1689, #1730, #1731, #1799, #1800, #1803, #1839)
- Platform and CLI correctness: honor dry-run, handle skipped checkpoints, allow search patterns, fix package dry-run/search results, respect cargo config, sync Bash HISTFILE, validate workspace paths, route --help to stdout, add skill arg checks, warn on redacted writes, skip DEBUG trap, restore utility tools, stop BASHOPTS extdebug leak, handle null redirection, keep provider handoffs alive, update DashScope URL, raw action relay watchdog, restore startup health row, hide receipt audit ref, and update test inventory baseline (#1426, #1440, #1633, #1637, #1642, #1646, #1672, #1675, #1676, #1710, #1719, #1733, #1783, #1787, #1790, #1795, #1808, #1812, #1818, #1820, #1845)

## [0.12.0] — 2026-07-12

### Added
- Move authentication ownership into cosh-core with isolated config layers (f028ad90)

### Changed
- Consolidate logging under a unified runtime module (db86b3dd)
- Migrate component docs into user-guide/developer-guide and add cosh-ng docs (317d3f26, adf63ac2)
- Rename `*_CN.md` docs to `*_zh.md` and fix cross-references (82f8dab4)

### Fixed
- Honor svc dry-run across platform and cli (4c593050)
- Preserve manual aliyun fallback and legacy STS auth (924dd76b, ee1dd179)
- Protect auth provider edits; prioritize aliyun auth option (f0c97efa, 904655fb)
- Bound host-executed shell preview (a6da7301)
- Route noninteractive cosh launcher calls; support raw command passthrough (ecb56739, 1490eb3c)
- Bind startup prompt and agent request context (14d6336f, 25d9d28f)
- Own prompt boundary in shell (#1310)
- Avoid UTF-8 split in loop detection (ef7f5147)
- Remove provider-visible skill hints; guide diagnostic skill use (a6024873, 7f695178)
- Drop redundant format borrow; satisfy clippy diagnostics (5e14a686, 063217f1)
- Stabilize CI, raw-cli, PTY, and service tests (d573796d, 3563a5ab, 0fb34ea6, fc28da5f, ad138d45, 65421d25, 5b17b892, 3e43ba6e, 707bc3c0)

## [0.11.0] — 2026-06-28

### Added
- Aliyun authentication provider with ECS auto-detection, STS credentials, and QR code flow
- SysOM Aliyun provider with ACS3 signing for LLM API access
- Per-turn SLS JSONL logging for observability
- SysOM request source identification headers
- Structured tracing logging system across all crates
- Sandbox bypass approval flow on PostToolUseFailure hook events
- Startup health scan for environment diagnostics
- Extension/hook/skill enable/disable commands (`/extensions`, `/hooks`, `/skills`)
- Unified component state management module
- Dedicated HOOK approval panel with simplified UI
- UserPromptSubmit hook with Ask approval enforcement
- Shell evidence read admission control in cosh-core
- Tool activity rendering in cosh-shell
- `cosh-switch` hint in startup banner for toggling between cosh-ng and copilot-shell

### Changed
- **BREAKING**: Rename CLI binary from `cosh` to `cosh-cli`; remove dispatch_core
- RPM spec: install cosh-cli binary, `/usr/bin/cosh` launcher, cosh-switch script, `Conflicts: copilot-shell`
- Replace eprintln with structured tracing macros
- Unify hook decision aggregation with fold_decision
- Update workspace repository URL to github.com/alibaba/anolisa

### Fixed
- Auth ECS flow and panel overlap on phase transitions
- Auth QR code rendered without ANSI escape codes
- Use SysomProvider for aliyun after auth success
- Align hook input fields and AfterModel/wrap_tool_response with copilot-shell protocol
- tool_result dedup guard and visibility
- Restore prompt before shell handoff
- Suppress duplicate evidence reads
- Reduce failed-command auto analysis noise
- Skill existence check and harden approval matching
- Resolve clippy warnings across cosh-core and cosh-shell

## [0.10.0] — 2026-06-23

### Added
- Shell evidence protocol for capturing and replaying command execution context
- Shell evidence control tool in cosh-core for evidence lifecycle management
- Hook protocol aligned with copilot-shell for zero-change extension support
- Per-hook decision propagation through notification protocol
- Hook warnings rendered with per-hook decision color-coding in cosh-shell

### Changed
- Share agent error display text across cosh-shell modules

### Tests
- Cover shell evidence raw CLI flows

## [0.9.0] — 2026-06-22

### Added
- Hook notifications integrated into approval panel with ⚠ warning display
- Hook ask decisions enforce user approval even in Trust/Auto modes
- Extended hook system with tool_use_id association and new event types
- Registry protocol for /extensions /skills /hooks slash commands

### Fixed
- Show Registry group in /help output

### Changed
- Remove dead skill management code

## [0.8.0] — 2026-06-18

### Changed
- Rename `cosh-tui` crate and binary to `cosh-core` across the entire workspace
- Update adapter system: `CoshTuiAdapter` → `CoshCoreAdapter`, `AdapterKind::CoshTui` → `CoshCore`
- Update environment variable `COSH_TUI_PATH` → `COSH_CORE_PATH`
- Update RPM spec, documentation, and all test fixtures

### Fixed
- Neutralize agent status text in streaming cards
- Align streaming card widths in cosh-shell

## [0.7.0] — 2026-06-17

### Added
- Extension discovery and loading module with `cosh-extension.json` manifest support
- Extension hooks integrated into startup lifecycle
- Skill module with multi-level loading (built-in, user, project) and hot-reload
- SkillManager integrated into tool registry and startup
- Available skills injected into system prompt for LLM discovery

### Fixed
- Infinite loop in `expand_env_vars` when environment variable is undefined
- Warn on unsupported extension hook events (`PostToolUseFailure`, `BeforeModel`, `AfterModel`) instead of silently discarding
- Align extension hooks format with copilot-shell nested group structure
- Remove unused `args` parameter from skill tool schema to avoid misleading LLM
- Show question free text answers in cosh-shell
- Harden foreground shell handoffs
- Share copilot shell config path and keep legacy config fallback

### Changed
- Normalize cosh-shell config keys
- Standardize cosh-shell code and test organization
- Move user state under copilot shell scope

## [0.6.0] — 2026-06-16

### Added
- P0 hook system with 5 lifecycle events (`on_session_start`, `on_turn_start`, `on_turn_end`, `on_tool_call`, `on_session_end`) in cosh-tui
- Shell approval classification and hook origin tracking in cosh-shell
- Migrate current cosh shell into monorepo workspace

### Fixed
- Address approval review findings in cosh-shell
- Harden shell evidence continuation to prevent dropped context
- Normalize tool call streaming protocol in cosh-tui
- Fix passthrough for subcommands in cosh-shell

## [0.5.0] — 2026-06-15

### Added
- CoshTuiAdapter persistent process mode (spawn once, reuse across agent runs, auto-restart on death)
- `ask_user` round-trip through control protocol (agent can ask inline questions routed to TUI)

### Changed
- Split cosh-tui main into cli/headless/interactive modules
- Rename binary from cosh-tui-core back to cosh-tui

## [0.4.1] — 2026-06-15

### Added
- settings.json → config.toml auto-migration with AES-256-GCM encrypted API key decryption
- JSONL protocol and tool approval integration tests

### Fixed
- Prepend precmd in PROMPT_COMMAND to capture real exit code (Alibaba Cloud Linux /etc/bashrc issue)

## [0.4.0] — 2026-06-15

### Added
- JSONL wire protocol (InputMessage / OutputMessage) for cosh-shell ↔ cosh-tui communication
- Provider abstraction with OpenAI-compatible streaming (DashScope, OpenAI, DeepSeek, Generic profiles)
- Tool execution framework with 7 built-in tools (shell, read_file, write_file, edit, grep, todo, skill) and approval control
- Context window management, message truncation, loop detection, conversation compression
- Lifecycle hooks framework
- CoshCore agent loop engine
- TOML-based multi-provider config with environment variable expansion

### Changed
- **BREAKING**: Binary interface from ratatui interactive TUI to JSONL stdin/stdout backend
- **BREAKING**: Config format from settings.json to config.toml
- Rewrite session store with single-file JSON persistence

### Removed
- Legacy ratatui-based TUI code (app, commands, llm, logger, theme, tools, ui modules)

## [0.3.0] — 2026-06-15

### Added
- **cosh-shell crate** — PTY-based AI-augmented shell host with OSC marker protocol
- Claude, Qwen, Fake AI adapters with streaming support
- Inline rendering engine (approval, question, recommendation, activity panels)
- Governance layer with approval modes
- Terminal recovery via signal handlers (SIGTERM/SIGHUP/SIGQUIT) and panic hook
- Exit code classification with 8 categories (Smart/Auto/Manual analysis modes)
- Tool display engine with per-tool-type parsing and ANSI color categories
- Hook engine with built-in hooks (FailedCommandHook, TestFailureHook) and skill routing
- External hook loading from ~/.config/cosh/hooks/ with subprocess execution
- Native shell compatibility (rcfile loading, PS1, history, login shell detection)
- Context window with sliding window (max commands, max age, token budget)
- Prompt intent optimization (do → Bash tool, know → prose)
- Natural language intercept with visual feedback
- InputClassifier conservative mode for native mode
- Analysis throttle (30s cooldown, max 3 consecutive)
- Consultation card rendering with keyboard capture
- Control protocol for tool approval round-trips
- Startup banner with gradient ASCII art logo
- `/mode` and `/hooks` slash commands
- Architecture documentation

### Fixed
- Native mode input rendering with powerlevel10k dual-line prompts
- Slash/NL intercept via buffered-then-judge strategy in native mode
- Zsh preexec intercept for command_not_found
- CandidateRedraw line clearing for CJK input and backspace
- Suppress cosh-osc$ prompt leak in native mode
- Tool display label matching in bash tool executor
- Wide character placeholder cell handling in buffer extraction

### Changed
- Unified workspace version (0.3.0) for all crates (cosh-types, cosh-platform, cosh-cli, cosh-shell, cosh-tui)

## [0.2.0] - 2026-05-16

Hardening + audit-subsystem release. Workspace versions bumped to `0.2.0` together with the release profile and lockfile commit.

### Added

- **`audit` subsystem** with PEP/PDP/log split: `cosh audit check` / `cosh audit log` for command-safety gating and per-session retrieval.
- **Workspace release profile** (`opt-level = 3`, `lto = true`, `strip = true`, `codegen-units = 1`), committed `Cargo.lock`, workspace-level dependency pinning, and native CA cert support.
- **Command timeouts, input validation, and panic-safe JSON output** across `cosh-cli` and `cosh-platform` so a panic still emits a `CoshResponse` envelope on stderr instead of an empty exit.
- **`forbid(unsafe_code)`** on `cosh-cli` / `cosh-platform`, plus `svc list --state` filter validation against an allow-list.
- **`pkg search` cross-references installed status** so results show which matches are already installed.
- **`ResponseMeta.warning`** field for non-fatal warnings; `audit` responses are explicitly marked as stub via this field.
- **LLM tool surface expansion** in cosh-tui: pkg / svc / checkpoint wrapper tools, plus `svc enable` / `svc disable --dry-run`.
- **Timeouts + exponential-backoff retries** on LLM and external command tools in cosh-tui; 60 s shell-tool timeout.

### Changed

- TUI `/help` aligned with the full command set; title bar version and markdown prefix stripping corrected.
- Clippy warnings resolved across the workspace; dead-code allowances dropped; test code aligned with production lint level.
- Build warnings eliminated and version detection improved across cosh-tui / cosh-platform.

### Fixed

- **Shell safety check tokenized** to close tab / newline / redirect / chain bypasses; substring matching on raw command strings replaced with whitespace (incl. `\t` / `\n` / `\r`) tokenization and metacharacter rejection (`;` `|` `&` `>` `<` `$` `` ` `` `(` `)` `{` `}`) — `is_safe_command` in `crates/cosh-tui/src/tools/shell.rs`.
- Forbidden tool calls are now blocked even under Yolo approval mode.
- `cosh-cli` wrapper tool output is bounded so a chatty subcommand cannot blow the LLM context window.
- Tool-call IDs synthesized via a process-wide counter to guarantee uniqueness across the agentic loop.
- `settings.json` and session files written atomically with `0600` permissions.
- Runtime bounds enforced for the agentic loop, history, config, and tool messages; scrollback bounded with UTF-8-safe truncation.
- Panic hook installed in the TUI; history navigation recovered after panic.
- ws-ckpt IPC response size bounded to 64 MiB.
- Nonexistent systemd services detected via `LoadState=not-found` instead of misclassifying them as "inactive".

### Security

- Audit-stub `recoverable` / `hint` semantics surface clearly to agents via the standard `CoshError` envelope.
- Atomic-rename + `0600` perms on credential-bearing files.

## [0.1.0] - 2026-05-10

Initial public-shaped release after renaming the workspace from `agos-core` to `cosh-ng` and adding the interactive TUI crate.

### Added

- **4-crate workspace**: `cosh-types`, `cosh-platform`, `cosh-cli`, `cosh-tui` with strict dependency direction `cosh-cli` / `cosh-tui` → `cosh-platform` → `cosh-types`.
- **`cosh` CLI binary** with dual-mode dispatch: `cosh` (no args) execs into `cosh-tui`, `cosh <subsystem> <action>` returns structured JSON.
- **Cross-distro `pkg` subsystem**: `install` / `remove` / `search` / `list` routed across `dnf` / `apt-get` (`apt-cache` for search) / `zypper` based on `Distro::detect()` reading `/etc/os-release`.
- **`svc` subsystem** over `systemctl`: `status` / `start` / `stop` / `restart` / `enable` / `disable` / `list`, with uptime and corrected column mapping in `list`.
- **`checkpoint` subsystem** talking to the `ws-ckpt` daemon over Unix-socket IPC; bincode wire format with 4-byte LE length prefix and explicit protocol versioning + error handling. Commands: `init` / `create` / `list` / `restore` / `recover` / `delete` / `diff` / `cleanup` / `status`.
- **`cosh-tui`** interactive TUI on `ratatui` + `crossterm`: slash-command system with auto-complete, session management, theming, custom border set, echo-on-submit.
- **Agentic loop with cosh-cli wrapper tools** in cosh-tui, bringing pkg / svc / checkpoint tooling to the LLM (initially shipped as `cosh-tui v0.4.0`).
- **LLM chat integration** with config-driven providers and UI surfacing.
- **Unified `settings.json` V2 config** consolidating prior scattered config files.
- **AES-256-GCM decryption** for encrypted credentials.
- **macOS detection + Homebrew backend** in `cosh-platform`, with unit tests.
- **Unified JSON envelope** `CoshResponse<T>` with `ok` / `data` / `error` / `meta`, classified `CoshError` carrying `recoverable` and `hint` for agent retry decisions.
- **Integration tests** for `pkg` and `checkpoint` CLI commands.

### Changed

- Workspace renamed from `agos-core` (with `agos-types` / `agos-platform` / `agos-cli`) to `cosh-ng` (with `cosh-*` crates); `agos-cli` and `agos-platform` removed in the same commit.
- `cosh-tui` checkpoint tooling adapted to the new daemon protocol.

### Fixed

- `cosh-cli` stdout validated as JSON before forwarding to the LLM, preventing parser confusion on malformed bytes.

## [pre-0.1.0] - 2026-05-03 → 2026-05-08

Pre-rename `agos-core` foundation.

### Added

- Initial 2-crate workspace `agos-types` + `agos-platform`.
- `agos-cli` cross-distro CLI prototype with `pkg`, `svc`, `checkpoint`, `audit` command shapes.
- MVP v2 CLI Gateway architecture document and bilingual (English / Chinese) usage guide.
