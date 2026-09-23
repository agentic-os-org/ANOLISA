# Changelog

## 2.8.0

- Added COSH_SESSION_ID export for subprocess and MCP correlation. (#1491)
- Fixed hook process tree termination on timeout and cancellation. (#1585)
- Updated docs to centralize user guides and add docs lint CI. (#1586)
- Fixed cosh-switch to reuse compatible cosh-ng authentication. (#1951)

## 2.7.0

- Added `/ktuner` command with consent-gated, opt-in first-run check and trusted-path resolver. (#1279)
- Added loading indicator when selecting an auth provider. (#1389)
- Added COSH_SESSION_ID export for agentsight per-run correlation. (#1383)
- Fixed security intents to map to hook commands. (#1438)
- Fixed cosh-switch post-switch guidance for clarity. (#1424)
- Fixed shell mode to add actionable error guidance for exit code 127. (#1319)
- Updated docs to rename _\_CN.md to _\_zh.md, fix cross-refs, and add missing READMEs. (#1335)

## 2.6.1

- Added npm packaging support for CLI and cosh. (#1307)
- Added multi-hook toggles support. (#1206)
- Fixed tips banner to stay visible after initial login. (#1308)
- Fixed Ctrl+O to prioritize error details over compact mode. (#1202)
- Fixed system-profile BINDIR to follow PREFIX. (#1193)
- Fixed AfterModel hook non-blocking notifications not surfacing. (#1182)
- Updated component docs to migrate into user-guide and developer-guide. (#1295)
- Added copilot-shell zh/en user and developer docs. (#1236)
- Fixed CONTRIBUTING.md file mode from symlink to regular file. (65601f06)

## 2.6.0

- Added multi-hook enable and disable support to `/hooks`. (#1200)
- Added cosh-ng compatibility with cosh-switch. (#1169)
- Added instance_id to SysOM API request params. (#1160)
- Added anolisa component contract. (#1128)
- Added SLS JSONL session telemetry with expanded metrics. (#1057)
- Added authenticated models display in /model dialog. (#1030)
- Added kitty csi-u keys support with sequence timeout management. (#552)
- Added large paste placeholder and fixed placeholder id reset on esc cancel. (#312)
- Fixed SLS log write to skip when file does not exist or is not writable. (#1101)
- Fixed useless sandbox guard and failure handler hooks by removing them. (#979)
- Fixed missing keyboard shortcut hints in footer status bar. (#921)
- Fixed response language and model identity rules. (#920)
- Fixed sub-model refusal detection and byteLength display. (#895)
- Fixed webfetch sub-model output validation to avoid silent refusals. (#895)
- Fixed thinking output to be distinguished from user input with prefix and color. (#894)
- Fixed custom model configuration to be preserved. (#887)
- Fixed partial message finalization on API error. (#801)
- Fixed API error reporting in all output formats, not only text. (#801)
- Improved escape key handling by unifying it in appcontainer. (#437)

## 2.5.0

- Fixed missing esc hint in tool confirmation footer status bar (#732)
- Fixed invisible cursor in provider/auth config inputs (#683)

## 2.4.1

- Fixed HookSystemMessage rendering as info and resolved Content/Thought duplication (#636)
- Fixed prompt ids to remain monotonic after shell remount (#628)

## 2.4.0

- Added DashScope Token Plan provider entry to the OpenAI-compatible auth dialog. (#598)
- Added UserPromptSubmit and PostToolUse hook reason surfacing in the UI. (#545)
- Added run_id field to HookInput for per-run event correlation. (#482)
- Fixed UserPromptSubmit hook decision merging to enforce safety priority over allow. (#597)
- Fixed missing tool_use_id in PreToolUse hook input. (#559)
- Fixed memory hooks lock takeover with atomic rename and async IO. (#550)
- Fixed auto-memory workspace cleanup wiping user-added directories. (#548)
- Fixed auto-memory session hook missing read_file events due to wrong arg key. (#547)
- Fixed run_id ordering by setting it before UserPromptSubmit hook fires. (#537)
- Fixed UserPromptSubmit hook firing on tool-result and Stop continuations. (#534)
- Updated installer to support multiple install profiles. (#541)

## 2.3.0

- **BREAKING** Removed qwen-oauth authentication support. (#455)
- Added auto memory background extraction system. (#465)
- Added full shell command display in hook-ask and exec confirm dialogs. (#452)
- Added esc key to cancel running slash commands. (#290)
- Fixed JavaScript heap out of memory during long sessions. (#462)
- Fixed missing allow decision reason in UI when systemMessage is absent. (#435)
- Improved test coverage with standalone tests for ExecCommandPreview. (#460)
- Updated hook docs to clarify difference between systemMessage and reason. (#436)

## 2.2.1

- Fixed initial chat being blocked during skill/subagent first-load discovery. (#418)
- Fixed missing tool_use_id in PostToolUse hook event payload. (#414)
- Fixed missing skill_context in PreToolUse hook input for resolved skill path. (#409)
- Fixed missing auto-completion for `/statusline` subcommands. (#408)
- Fixed unavailable agents appearing in the key sharing prompt. (#394)
- Fixed bash option not being restored after canceling from the provider screen. (#393)
- Fixed hook systemMessages to be concatenated with a `[name]` prefix for clarity. (#387)

## 2.2.0

- Added `ask` decision support for UserPromptSubmit hook. (#328)
- Added new command for Clawhub CLI. (#313)
- Added interactive Skills TUI Panel with enable/disable support. (#311)
- Added variable substitution and display control for extension TOML commands. (#291)
- Added immediate hook activation on extension install/uninstall. (#283)
- Added `ask` decision support for PreToolUse hooks. (#276)
- Added configurable status bar. (#251)
- Added `/export` command for session history. (#245)
- Fixed API key validation to skip non-Dashscope providers. (#337)
- Fixed PreToolUse ask dialog by unifying it to info type with diff preview. (#345)
- Fixed memory leak in memory management. (#309)
- Fixed extension lifecycle reliability. (#298)
- Fixed hook registry sync on extension enable/disable. (#298)
- Fixed interface crash caused by leftBottomContent of Box nested in Text in Footer. (#293)
- Fixed `/hooks install` command by removing it and adding default help. (#287)
- Fixed extension examples installation and package configuration. (#271)

## 2.1.0

- Added startup bash entry and simplified manual auth dialog. (#217)
- Added async fzf-based tab completion optimization. (#214)
- Fixed OpenAI API key and model validation via /models endpoint on auth. (#243)
- Fixed API key retention when navigating to apiKey field in auth dialog. (#241)
- Fixed node-pty native binary bundling for both linux architectures. (#232)
- Fixed stream redaction by replacing integer offset with committed text reference. (#210)
- Fixed missing fields in hook system. (#188)

## 2.0.4

- Added STS authentication support via ECS RAM role. (#161)
- Added BeforeModel, AfterModel, and BeforeToolSelection hooks. (#154)
- Added sandbox usage summary on session exit. (#137)
- Added Tab-completion for `!` shell mode. (#131)
- Fixed config-dir source unification and prevented ~/.copilot creation on startup. (#171)
- Fixed /bug command crash in headless environment. (#175)
- Fixed undefined metrics.sandbox in StatsDisplay. (#171)
- Supplement /hooks install step to post-installation guide. (#142)
- Supplement hooks documentation (index, reference, writing-hooks). (#142)

## 2.0.3

- Migrated config directory from `~/.copilot` to `~/.copilot-shell`. (#78)
- Added API key detection from configured agents with user approval on bootstrap. (#127)
- Added support for configuring multiple custom model providers. (task#80737766)
- Added global API endpoint support for Dashscope. (#133)
- Added custom skill paths support via `settings.json`. (#128)
- Added support for loading skills from extension directories with `cosh-extension.json` compatibility. (#54)
- Added `/bug` command for submitting bug reports. (#122)
- Added sandbox-guard install command with bypass approval flow. (#125)
- Added secret redaction for model output and tool results. (#100)
- Added extensible feature tip banner for first-launch guidance. (#113)
- Added built-in `/dir cd` command for in-session directory navigation. (#19)
- Added session renaming command. (task#80737766)
- Added nvm-aware Node.js detection in `cosh` wrapper script. (#72)
- Added system-level install via `Makefile` with FHS-compliant directory layout. (#72)
- Fixed 24-item limit on `@` file completion menu. (#92)
- Fixed TUI flicker on Qwen OAuth page in limited-height terminals. (#76)
- Fixed left-arrow key not wrapping from line start to previous line end. (#53)
- Fixed irrelevant info display in `/model` command. (#85)
- Fixed credentials encryption support in `settings.json`. (#90)
- Fixed test failure when running as `root` user. (#29)
- Fixed pre-commit hook working directory for lint-staged. (#90)
- Configured Husky hooks and documented pre-commit setup. (#65)

## 2.0.1

- Renamed OpenAI authentication label to "BaiLian (OpenAI Compatible)" for clarity.
- Fixed login shell stdin drain to prevent unwanted input echo.
- Removed ripgrep unavailable warning message.

## 2.0.0

- Synced upstream `qwen-code` to v0.9.0 and rebranded to **Copilot Shell**.
- Bumped version directly to 2.0.0 (skipping 1.x, which was used by a previous `OS Copilot` release).
- Integrated Skill-OS online remote skill discovery with priority-based fallback (Project > User > Extension > Remote).
- Added `/skills remote` and `/skills cache clear` commands for remote skill management.
- Added `/bash` interactive shell mode
- Added `-c` argument support for inline bash commands.
- Added PTY mode for `sudo` command support.
- Added hooks system with PreToolUse event for intercepting tool calls before execution.
- Added new model provider named Aliyun
- Added nested startup detection warning banner.
- Added system-wide skill path (`/usr/share`) support.
- Removed original Gemini sandbox.
- Fixed skill frontmatter parsing for YAML special characters (`|`, `&`, `>`).
- Fixed login escaped character echo issue in ECS workbench.
- Fixed Linux headless environment browser open failure when auth with Qwen OAuth.
- Fixed Qwen OAuth authentication, replay, and UI rendering issues.
- Fixed exception handling when adding workspace directories.
- Fixed user query start with unix path being misidentified as command.
- Fixed API key display explicitly.
- Fixed Chinese i18n for `/resume` command.
- Improved `?` hint visibility — hidden while user is typing.
- Miscellaneous UI, branding, CI, and build improvements.
