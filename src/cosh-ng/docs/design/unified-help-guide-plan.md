# Unified help and guide execution plan

[简体中文](unified-help-guide-plan_zh.md) · [Design](unified-help-guide.md)

Worktree: isolated `cosh-unified-guide/anolisa`; branch `feat/cosh-unified-guide`.
Baseline: `1300cfcd50938af9323c0c5df4faaaf00ff6be60`. Do not implement in the original main checkout.
This is an executable staged checklist, not a claim that the complete feature exists.

## Execution and evidence

For each stage: failing test → minimal implementation → focused pass → code/interaction review → user docs.
Use cosh-lab doctor and test plan before tests. macOS runs only small exact filters.
Use owner unit tests, logic for public API behavior, raw_cli for processes, protocol for protocol and shell_host for terminals.
No dependencies, real credentials, host account mutations or changes to original untracked files.
Later work is authorized by the approved design; resolve owner-contract conflicts rather than bypassing them.

## P0: Design and baseline

- [x] Record three-entry ownership, five settings, independent onboarding, interaction, state and safety boundaries.
- [x] Create an isolated worktree through the native tool and establish a feature branch.
- [x] Run doctor; host contention means no performance conclusions.
- [x] Capture root-help baseline and new-behavior failing tests with logs.

Deliver bilingual design/plan and red/green evidence. The prototype remains a visual reference.

## P1: Useful root help (first implementation slice)

Files: runtime/invocation.rs, new runtime/help.rs, runtime/mod.rs, main.rs,
i18n/message_id/help.rs with trailing registration, i18n/{en,zh}/help.rs, tests/raw_cli/startup.rs,
and corresponding bilingual README/user-guide.
Raise only the i18n macro recursion limit in lib.rs/main.rs. New classifier tests live in private runtime/invocation/help_tests.rs to stay below the large-file threshold; update the exhaustive match in existing runtime/invocation_tests.rs.

1. Test installed identity `cosh --help` without a TTY: useful Cosh output, no inner-shell startup.
2. Test stdout piping, empty stderr, no ANSI, exit 0 and no config/log creation under a temporary HOME.
3. Test saved language, environment override and unreadable-config fallback through bounded readonly parsing.
4. Reserve only exact root help; preserve explicit Bash help, extra arguments and internal identity behavior.
5. Add a pure output module and existing i18n messages without runtime/provider initialization.
6. List only existing features; do not advertise interactive guide/--plain before implementation.

Done: red then green, classifier compatibility and legacy-help tests pass, layout passes,
and user docs accurately describe implemented behavior.

## P2: Guide entry, static content and Ratatui shell (implemented)

Files: config/guide/cli.rs, config/guide/wizard.rs and wizard/content.rs (shared content/capability),
runtime/guide.rs and guide/{session,status}.rs (interactive controller and readonly state), ui/guide.rs and
guide/{screen,view}.rs (terminal lifecycle and views), the existing Markdown boundary, i18n, tests and user docs.

Architecture correction: config compiles into both lib and bin while runtime/auth exist only in the bin.
The interactive controller needs the login owner, so it lives in runtime/guide; config/guide/wizard keeps only
the content and TTY/TERM/size rules shared by both targets. main.rs checks capability at Invocation::Guide and
routes static/legacy leaves through config::guide::run_cli.

1. Test empty/--plain/--help/on/off/status and invalid combinations; static paths never open writable stores.
2. Share onboarding Markdown and five-setting descriptions. Static output references precise existing commands.
3. Implement TTY/TERM/size selection, terminal recovery guard, single focus, return position and undersized view.
4. Load readonly states first, preserving Unknown; then connect language, health and guidance handlers.
5. One Enter saves ordinary changes; Esc discards drafts; report effect scope.

Done: buffer checks at 80×24/60×20/40×16 and static paths; real Linux PTY enter/exit/resize/Ctrl-C recovery.

Implementation note: 80×24 and 40×16 use TestBackend buffer assertions (single centered column, focus-only
reverse, long-value truncation, undersized notice); 60×20 follows the same single-column path. A real macOS PTY
with a temporary HOME verified enter/exit, resize to undersized and back, Ctrl-C, language save and terminal
recovery (leaves the alternate screen and restores the cursor). Linux PTY and real VNC remain for P6.
AI service and default login shell are readonly pages in P2 (pointing at existing /auth and cosh login);
their interactive flows land in P3/P5.

## P3: Reuse authentication for AI service

Files: existing auth owners, minimal controller boundary, guide AI state/view, i18n and tests.

1. Establish input/state/submit/result contracts and identify session dependencies.
2. Expose only the required reusable boundary while preserving /auth; no copied templates, secret writes or manager.
3. Enter the same management flow from guide and reread active service, source and zero-config preference.
4. Cover system readonly, shadowed deletion, save-and-activate, cancel, partial success, retry and network error.
5. Regress using isolated credentials/fake providers; track real-provider authorization/acceptance separately.

Done: one business implementation for both entries; configured differs from connected; errors preserve focus,
no secret output, existing /auth regression green.

## P4: Tips and recommendations

Files: guide subviews, existing config/mode/recommendation/GuideStore owners, i18n and logic/raw_cli tests.

1. Preserve guidance runtime-publication/persistence outcomes, including partial success.
2. Add a tested persistence boundary for new-session analysis defaults; keep /mode session-only.
3. Show saved/effective values and source for analysis, personalized questions and Bash import, preserving existing precedence; distinguish allowed-but-overridden saves from handler rejection without bypassing forced Off.
4. Align manual pause, disable-and-clear, clear-only for valid data and fixed retention explanation; no retention setting. Separately show Reset data and turn recommendations off for corruption recovery, default Cancel, without promising to preserve an unknown preference.
5. Use one history page with Allow import and save or Stop importing and save according to state, default Back; label overridden saves as preference changes and explain continuing environment control. Stopping retains existing data; no preceding page or second confirmation.
6. Explain baseline reads and transmission; delegate confirmed actions to existing handlers and distinguish clear/recovery failure and partial success from total success.

Done: verify preferences and data outcomes separately, cover the cases below, and let new users reach each relevant control directly:

- COSH_SHELL_ANALYSIS_MODE overrides new-session defaults without claiming a current-session change.
- Saving Off under COSH_RECOMMENDATIONS_BASH_HISTORY=1 still displays effective On; saving On under environment Off does not falsely claim activation.
- COSH_RECOMMENDATIONS_ENABLED=0 causes the existing handler to reject enabling without persistence; otherwise verify user-preference precedence.
- Valid-data clear preserves preferences; corruption reset explicitly turns recommendations off; permission/lock errors never trigger automatic reset; cover cancel, partial success and reread results.
- History import On → Off → On and cancellation at each step; stopping retains existing data, and an active override never produces a false stopped claim.

## P5: Default login shell

Files: guide login view, minimal runtime/login and existing shell_host effect boundary, i18n and isolated Linux tests.

1. Establish and test a private privileged request/result protocol reusing the login owner before connecting the view. Allow only Query/Set/Restore, bind username and expected UID, and define bounded versioned snapshots/results; never parse localized CLI text or elevate the whole guide.
2. Current account first, other accounts on demand; show username/UID/actual path/capability. Current-account reads need no added elevation; query other accounts in a foreground-sudo privileged child, not in guide's unchanged process after sudo authorization alone.
3. Use trusted installation/record checks for Set, Restore, unavailable and unknown states; confirmation and child revalidation bind the same target identity.
4. Leave fullscreen for system foreground sudo password input, keeping passwords out of the protocol. Restore the terminal, validate structured results and reread through the same privilege boundary. Distinguish query complete, cancellation/failure before write, verified change and unconfirmed write; missing valid results or exit 0 alone do not prove success.
5. Treat timeout, abnormal exit and invalid/missing results as unconfirmed, block blind retries and stop on identity mismatch. Refresh restoration after first Set; leave the current shell unchanged.
6. On controlled ALinux 4 RPM systems test current/other users, root, no sudo, cancellation, conflicts, post-write verification failure, protocol timeout/invalid result/UID mismatch and restoration; verify terminal recovery and that passwords never enter protocol traffic or logs.

Done: focused protocol/state classification passes before view integration; real account state, target UID and trusted record agree.
Unconfirmed results must be queryable through the same privileged boundary, blocking repeat mutations until confirmed.
No host mutations; temporary accounts/machines have verified cleanup. Unit tests alone cannot establish login-shell acceptance.

## P6: Entry consistency and acceptance

Files: existing Slash registry/notices/i18n, guide/root content. Do not rewrite HelpPanel renderer.

1. Preserve /help grouping/style/scope/current state; add only necessary entry hints and verified summaries.
2. Compare with actual registry; no independent command table; bilingual consistency, no internal root commands.
3. Walk empty/existing-user paths for duplicate choices, focus loss, redundant confirmations and terminology.
4. Real-terminal light/dark/monochrome, narrow/resize/redirection/restricted TERM; real VNC where available.
5. Run layout/fmt/focused and affected crate/Linux gates; track packaging and x86_64/ARM64 release separately.
6. Update bilingual user docs, evidence index and risks; review before subsequent commit/PR/release decisions.

Done: all five settings and onboarding verified with no duplicated business implementation;
evidence distinguishes prototype, focused, PTY, provider, RPM and release.

## Current handoff

P0, P1, P2, P3, P4 and P5 have complete focused verification and are uncommitted. P6 is not complete and must not be documented as shipped.
P2 landed the guide entry, --plain/--help, shared onboarding content, TTY/TERM/size degradation, the Ratatui
single-focus shell, readonly state (Unknown is not Off) and language/health/guidance-tip saves; AI service and
default login shell are readonly pages for now.
P3 landed the read-only AI service summary (reusing the `auth state` query without an ECS probe) and a handoff
to /auth: the guide suspends the full screen, a child session opens /auth at the first ready prompt, and on exit
the guide restores the screen and re-reads state; the parent ignores SIGINT/SIGQUIT during the handoff.
Default login shell remains a readonly page (lands in P5).
P4 landed the four tips & recommendation sub-items (usage tips / command analysis / personalized prompt
recommendations / recommendation data & privacy): a new config/shell_prefs.rs provides saved/effective/source and
writes for analysis_mode and bash_history; the guide reuses PersonalStore for on/off/clear-only/corrupt-reset, and
COSH_RECOMMENDATIONS_ENABLED=0 rejects an enable request without persisting; read-only status never creates the
store directory. The single-page import primary action switches copy by effective value and env override, and
stopping does not clear existing data.
P5 landed the default login shell: a private versioned bounded privilege protocol in
runtime/login/guide_protocol.rs (request/result travel through a private temp file; the foreground sudo password is
read on the tty and never enters the protocol; only Query/Set/Restore, bound to username+expected UID; exit code 0 is
not proof — the parent re-reads the structured result and checks the target shell; invalid/missing/identity-mismatch =
Unconfirmed, no blind retry). The guide login page uses the protocol for current-user Set/Restore and hands other
users to the existing `cosh login`; the worker reuses cli::run's elevated logic without duplicating the state machine.
On 2026-11-14 the bilingual design and P4/P5 plan were revised for the four review findings. This documentation-only revision does not establish implementation of overrides, corruption recovery, import disabling or the privileged protocol.
Update this section/checks after implementation with actual commands, exits, counts and log locations.
Rollback code by stage without deleting user credentials, config, recommendations or login restoration records.

## P2 evidence (2026-11-14)

Evidence lives under worktree-root `.cosh-lab/runs/<run-id>/`. Exit codes are underlying test process exits.

| Check | Result | Run id |
| --- | --- | --- |
| config::guide::wizard shared content/capability/static routing | 6 passed (lib+bin), exit 0 | 20261113T163029Z-cab2db22 |
| runtime::guide controller/readonly state/saves | 9 passed, exit 0 | 20261113T163126Z-721d77d5 |
| ui::guide terminal view buffers | 5 passed (lib+bin), exit 0 | 20261113T163156Z-b8538614 |
| config::guide full (incl. legacy CLI regression) | 52 passed (lib+bin), exit 0 | 20261113T163216Z-8ee0831d |
| i18n::tests::all_messages_have_en_and_zh_values | 1 passed (lib+bin), exit 0 | 20261113T163856Z-d6438f0a |
| runtime::invocation argument/entry regression | 47 passed, exit 0 | 20261113T163906Z-32400d8c |
| Root help installed-entry regression (guide index updated) | 1 passed, exit 0 | 20261113T164014Z-9a1d929e |
| Legacy internal --help stdout regression | 1 passed, exit 0 | 20261113T163929Z-5e8c03cc |

`cargo fmt --all -- --check` and `bash crates/cosh-shell/scripts/check-layout.sh` exit 0.
A new markdown_styled_lines helper pushed ui/agent_render/mod.rs over 700 lines; it was replaced by exposing
MarkdownRenderModel directly, and the layout audit passes again.
Manual real macOS PTY with a temporary HOME: enter/leave alternate screen, Esc/Ctrl-C exit 0, resize to 30×10
shows the undersized notice and recovers, language save writes config.toml and the cursor is restored; with
COSH_SHELL_LANG set the UI language keeps the environment value (as designed).
Full crate/workspace, Linux PTY, real VNC/provider/sudo, RPM and release gates were not run and cannot be
inferred from focused passes.

## P3 evidence (2026-11-14)

| Check | Result | Run id |
| --- | --- | --- |
| adapter::cosh_core_auth (incl. 2 new pure guide-summary tests) | 9 passed, exit 0 | 20261113T165853Z-67c47911 |
| guide all modules (config/runtime/ui/invocation etc.) | 76 passed, exit 0 | 20261113T165928Z-d589834a |
| i18n completeness and trailing-segment order | 15 passed, exit 0 | 20261113T170045Z-24453620 |
| runtime:: broad regression | 801 passed, 3 failed, exit 101 | 20261113T170113Z-8b221e49 |

The 3 failures are unrelated to this change: two controller::hook_tests::smart_mode_* are pre-existing (still
fail in isolation, 20261113T170145Z-6ae90085, exercising the unmodified render_inline_guidance); one login
lifecycle lock test is parallel-flaky and passes in isolation (20261113T170136Z-483a96e2). The i18n order test
previously hard-coded LoginRecoveryMissing as the tail; after appending the cosh_root_help/guide_wizard segments
the tail assertion was updated (pre-fix failure 20261113T165937Z-2c0bb9aa).
`cargo fmt --all -- --check` and `bash crates/cosh-shell/scripts/check-layout.sh` exit 0.
Real session handoff (TTY+cosh-core) is deferred to P6/Linux; focused passes do not imply product/release acceptance.

## P4 evidence (2026-11-14)

| Check | Result | Run id |
| --- | --- | --- |
| guide all modules (incl. 4 new P4 controller tests) | 80 passed, exit 0 | 20261113T171722Z-6fbac17d |
| i18n completeness and trailing-segment order | 15 passed, exit 0 | 20261113T171738Z-4cd6d2b0 |
| config:: full (incl. indirect shell_prefs coverage) | 110 passed, exit 0 | 20261113T171756Z-38b8e62f |

New P4 tests: analysis save reports new-session scope; forced-off rejects enable without persisting; import toggle
saves the preference; clear-only keeps the on/off setting. Fixed a regression where read-only navigation created the
store directory via PersonalStore::open (now reads only when the directory exists); start_page…without_saving passes.
`cargo fmt --all -- --check` and check-layout.sh exit 0; session.rs 674 lines, tips.rs 361 lines, both <700.
The full override-priority, corruption-recovery and import-disable matrix is deferred to P6 real-terminal/Linux;
focused passes do not imply product acceptance.

## P5 evidence (2026-11-14)

| Check | Result | Run id |
| --- | --- | --- |
| login::guide_protocol classification/version/identity (query worker Linux-gated) | 3 passed, exit 0 | 20261113T173219Z-a279c981 |
| i18n completeness and trailing-segment order | 15 passed, exit 0 | 20261113T173220Z-4543f285 |
| guide all modules | 83 passed, exit 0 | 20261113T173221Z-04a8a928 |
| runtime::invocation incl. new private worker entry | 47 passed, exit 0 | P5 batch |

On macOS query_status is Linux-only, so the query-worker test is #[cfg(target_os="linux")]; real sudo/account
verification is deferred to P6/Linux. `cargo fmt --all -- --check` and check-layout.sh exit 0; session.rs 658 lines,
login.rs 108 lines, both <700. No host account/login shell was modified; the privilege protocol has unit/classification
verification only and does not constitute login-shell product acceptance.

## P6 evidence (2026-11-14, code/docs/focused portion)

Three-entry unification: kept the existing /help HelpPanel renderer and registry groups (no hardcoded
command table), adding only a `cosh guide` entry hint in the footer; the root help index already lists
guide/health/login and in-session /help//auth//recommendations//mode. Bilingual user-doc overview gained a
"Configuration wizard and getting started" section stating the three entries' roles, degradation, and the
sudo/new-login boundary without overclaiming acceptance.

| Check | Result | Run id |
| --- | --- | --- |
| i18n completeness and trailing-segment order | 15 passed, exit 0 | 20261113T173502Z-9e5a68cc |
| guide all modules | 83 passed, exit 0 | 20261113T173512Z-7aec613b |
| runtime::invocation | 47 passed, exit 0 | 20261113T173515Z-d0012a50 |
| Root help installed-entry regression (raw-cli) | 1 passed, exit 0 | 20261113T173525Z-3f9862c8 |

`cargo fmt --all -- --check` and check-layout.sh exit 0.
Not done (verified separately; not inferred from focused): real-terminal light/dark/no-color matrix and
x86_64/ARM64 release acceptance. Real VNC, Linux real sudo/accounts and x86_64/ARM64 RPM install are covered by the
real-machine section below; narrow/resize/redirect/restricted-TERM are covered by macOS PTY and real VNC.

macOS real-PTY walkthrough (temporary HOME, COSH_SHELL_LANG=en-US, 2026-11-14): Home / Tips menu / command analysis /
personalized recommendations / default login shell pages all render; 30x10 shows the undersized notice and 80x24
recovers; Esc/q exit code 0; non-TTY redirect and TERM=dumb both print static text without entering the alternate
screen. The recommendation-data page and import/clear are covered by focused unit tests. Light/dark/no-color remain
to be reviewed; real VNC is covered by the real-machine section below.

## P6 real-environment evidence (2026-11-14, ALinux4 x86_64 ECS)

Environment: temporary cn-hangzhou ECS (x86_64 ecs.g7.xlarge / ARM64 ecs.c6r.xlarge, ALinux4 aliyun_4_*_20G_alibase_20261103).
Official Rust 1.100.0 toolchain and official crates.io registry cache transferred over the OSS internal endpoint and
built offline on the box (no mirror sources); RPM produced by the unmodified scripts/rpm-build.sh cosh-ng as
cosh-ng-0.29.1-1.alnx4.x86_64.rpm and installed via yum. VNC = tigervnc-server + matchbox-window-manager + xterm,
driven and captured over the real RFB protocol with vncdotool.

| Check | Result |
| --- | --- |
| Linux-gated login::guide_protocol (bin target, incl. query_worker) | 4 passed, exit 0 |
| RPM build + install | exit 0; /usr/bin/cosh, capability proof and preflight present; /etc/shells registered |
| Installed cosh --help | exit 0; index lists guide/health/login and in-session /help//auth//config//recommendations//mode |
| Installed cosh guide non-TTY | exit 0; static text, no alternate screen |
| Temp user login set | exit 0; getent /bin/bash -> /usr/bin/cosh; status managed=yes |
| Temp user login restore | exit 0; getent back to /bin/bash |
| Real sudo worker query (temp-file protocol) | exit 0 with structured ok; identity/UID/shell match getent |
| Non-root worker set | exit 0 but structured error (UID/euid guard); exit 0 is not success |
| ALinux3 boundary | status/set/restore exit 1, "ALinux4 RPM only" (platform/degradation boundary evidence) |
| Real VNC interaction | Home/AI/Language/getting-started render; up/down/Enter/Esc/q work; /help grouped panel with cosh guide footer hint |
| VNC degradation (40x12, TERM=dumb) | static guide text, no TUI/alternate screen |
| ARM64 RPM build + install + gated unit tests + set/restore | exit 0; cosh-ng-0.29.1-1.alnx4.aarch64.rpm; 4 passed; getent-confirmed set/restore |

Cleanup: temp account removed; ALinux3, ALinux4 x86_64 and ALinux4 ARM64 temporary ECS released and verified; OSS transfer objects deleted.
Still not done (separate; not inferred): production image publish/rollout; the remaining real-machine acceptance is recorded in the
sections below. Notes: VNC screenshots show a few client-side decode black patches (vncdotool
encoding artifacts), not attributed to the TUI; getting-started reached via repeat Down from a fresh home (post-Esc
key timing under the VNC harness was flaky; a harness limitation, not a product claim).

### Release-acceptance package tests (2026-11-14, ALinux4 x86_64 + ARM64 real ECS)

Environment: temporary cn-hangzhou ECS x86_64 i-bp15n4ew3p63f3tjjk5q (ecs.g7.xlarge) / ARM64 i-bp15tj425p2kov64np6j (ecs.c6r.xlarge),
ALinux4 aliyun_4_*_20G_alibase_20261103. Source and the official dependency cache were transferred over the OSS internal endpoint;
all four package tests and scripts/ ran unmodified (no code or packaging changes). test-package-raw.sh requires the repo layout
<repo>/src/cosh-ng plus <repo>/scripts/build-all.sh (it asserts the unified entrypoint rejects legacy --uninstall). The first run
placed source at /root/cosh-ng, so $ROOT/../../scripts resolved physically to a missing path and raw=1; this is an environment
layout issue, not a product defect. Re-running under the correct repo layout passes.

| Package test | x86_64 | ARM64 |
| --- | --- | --- |
| tests/test-package-rpm.sh | exit 0 (%postun matrix + scriptlets pass) | exit 0 |
| tests/test-package-raw.sh | exit 0 (repo-layout re-run) | exit 0 (repo-layout re-run) |
| tests/test-system-install.sh | exit 0 (prebuilt install pass) | exit 0 |
| tests/test_rpm_split.py | exit 0 (3 tests OK) | exit 0 (3 tests OK) |

Invocations: first pass both arches t-hz0731gj8e6zgu8 / t-hz0731gj9zf5ssg (raw=1, layout); raw repo-layout re-run both arches
t-hz0731hf23cw4qo (raw=0). Cleanup: both acceptance ECS DeleteInstance-verified; OSS cosh-guide/release-accept-20261114/ transfer
objects deleted. Still not done (separate; not inferred): production image publish/rollout.

### Light/dark/no-color matrix (2026-11-14, ALinux4 x86_64 real VNC)

Method: real ECS i-bp1ih82fh5fpt3357h39 (ecs.g7.xlarge, ALinux4). The binary was built from this workspace's source via official
Rust 1.100.0 + official crates cache, offline `cargo build --release -p cosh-shell`; the source tar matches the worktree crates/,
Cargo.toml and Cargo.lock file-for-file. No old RPM/artifact was reused and no code was changed. VNC = tigervnc-server +
matchbox-window-manager + xterm (800x480, Noto Sans Mono CJK SC 12pt); vncdotool drove keys and captures over the real RFB protocol.
Three terminal configs: dark (black bg/white fg), light (white bg/black fg), mono (white bg/black fg + NO_COLOR=1 + TERM=xterm-mono);
each captured home / AI detail / getting-started.

| Terminal | Result |
| --- | --- |
| dark | Text and selected row (full-row reverse video + ">") legible; yellow inline-code and teal bullets are supplementary semantic color |
| light | Black-on-white legible; selected row full-row reverse (black bg/white fg); yellow inline-code with black text readable |
| mono | No color reliance: selection still via reverse video + ">"; inline-code highlight degrades to plain text, content fully readable |

Invocations: build t-hz0731ibde6jny8 (BUILD_RC=0); VNC/font t-hz0731itb7cheyo / t-hz0731j72sv8cg0; three-config walk t-hz0731j9jwad0jk
(9 screenshots). Notes: the top black band on light/mono captures is a vncdotool client decode artifact (invisible on dark), not
attributed to the TUI; the first 480x340 pass lacked a CJK font so Chinese showed tofu boxes, resolved by installing Noto CJK
(environment font issue, not a product defect).
Observation (separate; no code change this round): at low column counts getting-started long lines truncate by column rather than
wrap (~45-66 cols); a narrow-width layout behavior left for product follow-up. The color matrix itself passes.
Cleanup: acceptance ECS DeleteInstance-verified; OSS cosh-guide/color-matrix-20261114/ transfer objects and screenshots deleted.
Still not done (separate; not inferred): production image publish/rollout.

### Release / image integration and real-provider auth handoff (2026-11-14, ALinux4 dual-arch real ECS)

Release artifacts: this workspace's source built by the unmodified scripts/rpm-build.sh cosh-ng into
cosh-ng-0.29.1-1.alnx4.x86_64.rpm (sha256 1e2daea2ab4f3ea14c5fdb365517d2721846357a3b0898163f2b822f7ae7a1fc) and
cosh-ng-0.29.1-1.alnx4.aarch64.rpm (sha256 9d53f0c3edae96404540d0b1536be07611b1d41a1f5d41958cf6cf4854a904d9) plus src.rpm.
Real ECS i-bp11ptfsibohitusqyld (x86_64) / i-bp104w4cs9rfkbm2q93p (ARM64).

Image integration (assets installed unmodified, acting as the image owner):

| Check | x86_64 | ARM64 |
| --- | --- | --- |
| RPM install | exit 0 | exit 0 |
| RPM alone leaves /etc/bashrc and /etc/profile.d untouched | yes | yes |
| Clean login after owner installs profile.d/cosh.sh + bashrc protected include | login guidance banner (Health check + Type cosh guide for help), SEEN=1 | same |
| Conflict login (user ERR trap + PROMPT_COMMAND) | user trap/PC fully preserved, Cosh retreats without overwriting | same |

Real-provider auth handoff (x86_64, zero-config automatic access; no /auth, credentials or RAM role needed):
- guide AI page: Current AI service: Not configured; Zero-config auto use: on.
- Enter on "Configure AI service (opens /auth)" hands off into a real cosh-core session; Provider Management shows
  [1]* [active] 操作系统控制台（免配置接入） auto (CJK renders correctly).
- Interactive-terminal cosh --ask 'Reply with exactly: COSH-REAL-PROVIDER-OK' -> Agent panel returns COSH-REAL-PROVIDER-OK (real model answer).
- Non-TTY cosh --ask correctly refuses (exit 2, requires an interactive terminal).

Invocations: build t-hz0731lwb7xss1s / t-hz0731lwe5uzwn4; integration t-hz0731mjobrxwxs / t-hz0731mjr3bec5c, activation re-probe
t-hz0731mq4jzlse8 / t-hz0731mq7hrsw00; handoff/question t-hz0731mqaj0v75s / t-hz0731mvzjnc9vk.
Notes: the first PTY probe printed the banner vertically because no window size was set (harness limitation); at 80x24 the banner
renders normally. Black bands in screenshots are vncdotool decode artifacts.
Cleanup: both ECS DeleteInstance-verified; OSS cosh-guide/final-accept-20261114/ transfer objects and screenshots deleted.
Still not done (separate; not inferred): production image publish/rollout (a release-operations step, outside this testing scope).

## First-slice evidence (2026-11-13)

Evidence lives under worktree-root `.cosh-lab/runs/<run-id>/` in result.json, commands.log and report.md.
Exit codes below are underlying test process exits. Expected failing baseline is not counted as a pass.

| Check | Result | Run ID |
| --- | --- | --- |
| Existing internal cosh-shell help baseline | 1 passed, exit 0 | `20261113T154651Z-2a71a953` |
| Installed-entry regression before fix | 1 failed, exit 101; child attempted missing shell and exited 127 | `20261113T154744Z-97d9a85c` |
| Installed-entry regression after final fix | 1 passed, exit 0; four scenarios cover empty home, saved language, environment override, FIFO fallback and no writes | `20261113T155324Z-4a02e073` |
| runtime::invocation compatibility | 47 passed, exit 0; zero matches in library target are not counted | `20261113T155229Z-ee608476` |
| All MessageIds have en/zh values | 1 passed in each library/binary target, exit 0 | `20261113T155316Z-29ca851a` |
| Existing internal help final regression | 1 passed, exit 0 | `20261113T155406Z-3232bcae` |

`cargo fmt --all -- --check` and `bash crates/cosh-shell/scripts/check-layout.sh` in src/cosh-ng,
plus `git diff --check` at repository root, exit 0. Two existing raw_cli/external_hook dead_code warnings remain.
Appending i18n IDs reached the macro recursion limit; raise only lib/main limits while preserving discriminants.
The initial new test exceeded invocation.rs's 700-line threshold; moving it into its private owner file restored layout compliance.
Readonly design/code reviews identified four documentation issues and one --context wording issue, all corrected and rechecked.

Test plan classifies documentation as unknown and requests the wider crate gate. Only the listed host focused checks ran.
Crate/workspace, Linux PTY, real VNC/provider/sudo, RPM and release gates are not run; focused success does not establish them.
No cloud resources, host account changes, commits or publication; retain the worktree for subsequent stages.

## PR review corrections (2026-11-15)

This revision supersedes the earlier v1 worker implementation and its
temp-file sudo evidence. The v2 request has no result path, is bounded to
4096 bytes, and binds the account to `SUDO_UID` plus NSS. The parent opens
the result FD; replies are bounded to 16384 bytes. The guide worker cannot
select another account; that entry delegates to the existing login CLI.
Platform tests assert either a real status or a structured refusal, including
unsupported Ubuntu fixtures, rather than assuming every Linux host is ALinux4.

Clear-only now has a separate confirmation page with Cancel selected.
Unreadable configuration focuses privacy instead of Clear. Other-user
executable lookup/spawn failures have a bilingual notice. Interactive guide
initializes the existing private logger and records categorical failures;
static help/plain paths do not initialize logging. The auth child reinstalls
terminal handlers in `run_tui`; the inherited ignore disposition is a startup
interval, not the lifetime of the child session.

New shared config/view tests use `library_tests!`; the overlap ceiling stays
744. Terminal signal recovery is coordinated with the actual screen lifecycle,
including cross-thread exit ownership. Current validation is recorded in the
PR review response; historical focused or RPM evidence above does not validate
these revised sources. Full release/provider acceptance remains separate.

## Reproduction

Set PYTHONPATH to the test skill's scripts/cosh-lab/src as documented by that skill, then run from repository root.
Set COSH_TEST_WORKDIR to this worktree's absolute src/cosh-ng path. Use the full raw-cli test names below.

```bash
python3.12 -m cosh_lab doctor --output json
python3.12 -m cosh_lab test plan --base 1300cfcd50938af9323c0c5df4faaaf00ff6be60 --output json
python3.12 -m cosh_lab test run --on host --scope unit --filter runtime::invocation --workdir "$COSH_TEST_WORKDIR" --output json
python3.12 -m cosh_lab test run --on host --scope unit --filter i18n::tests::all_messages_have_en_and_zh_values --workdir "$COSH_TEST_WORKDIR" --output json
python3.12 -m cosh_lab test run --on host --scope raw-cli --filter startup::installed_cosh_help_is_readonly_localized_and_independent_of_shell --workdir "$COSH_TEST_WORKDIR" --output json
python3.12 -m cosh_lab test run --on host --scope raw-cli --filter startup::help_flag_writes_to_stdout_not_stderr --workdir "$COSH_TEST_WORKDIR" --output json
```
