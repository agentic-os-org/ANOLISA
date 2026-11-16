# Unified Cosh help and configuration guide

[简体中文](unified-help-guide_zh.md) · [Execution plan](unified-help-guide-plan.md)

Status: approved design, implementation in stages. Target behavior below is not a release claim.
Source baseline: `1300cfcd50938af9323c0c5df4faaaf00ff6be60`. Visual reference: the v10 Ratatui browser prototype reviewed in this discussion.

## 1. Goals and ownership

Help new users start and existing users configure Cosh without competing help systems.
Five settings are the primary guide function; getting started is a separate entry.
Reuse authentication, recommendations, health and login-shell business logic.
Do not add providers, credential storage, automatic elevation or change tool approval and shell transparency.

| Entry | User need | Responsibility | Excluded |
| --- | --- | --- | --- |
| `cosh --help` | How do I launch Cosh? | Static launch options and public command index; stdout, exit 0 | Authentication, writes, fullscreen UI, internal developer commands |
| `cosh guide` | How do I configure and start? | Five settings and independent getting started; Ratatui or static fallback | Session command encyclopedia, duplicate authentication/account manager |
| `/help` | What can I do in this session? | Existing inline Slash reference card, then normal input | Fullscreen pager, configuration forms, another navigation layer |

Link entries only where useful. Session actions remain directly accessible through `/auth`,
`/config language`, `/recommendations`, and `/mode analysis`. The guide owns new-session defaults,
login settings and onboarding. `guide on/off/status` controls usage guidance only, never all AI features.

## 2. Information hierarchy and terminal layout

Home lists Language, AI service, Tips and recommendations, Login health summary, Default login shell,
then Start asking questions and a visually separate Getting started entry. Focus starts on questions.
Return restores selection; successful saves return to the list with reread state.
No mandatory first-run sequence or requirement to configure every setting.
Choice rows reserve the label before the value. At narrow supported sizes,
overlong values are ellipsized so setting names remain identifiable.

```text
 Cosh setup and getting started

   Language                     English
   AI service                   Zero-config automatic access; ask directly
   Tips and recommendations     View settings
   Login health summary         On
   Default login shell          Current user · Bash

 > Start asking questions
   Getting started              Commands, questions, exit

 Up/Down Select  Enter Open  Esc Exit
```

This illustrates hierarchy, not browser pixels. Use terminal cell widths.
At 80×24, use one compact centered column, maximum content width 76, minimum horizontal margin 1.
At 60×20, keep the same path and wrap descriptions; at 40×16 use short summaries and scrolling details.
Avoid parallel focus panes, oversized row spacing, and repeating every explanation on home.
Four levels: title, selection, state, supporting text. Only the focused item uses reverse video plus `>`.
Use terminal default background and restrained semantic colors; text must convey state without color.
Support light, dark and monochrome terminals. Measure CJK display width; show full truncated names/paths in details.
Use the existing Markdown renderer for explanatory content and copyable examples.
Reserve local borders for necessary confirmations rather than nesting panels around every setting.

## 3. Interaction contract

| Context | Keys/action | Result |
| --- | --- | --- |
| Home | Up/Down, Enter, Esc/Q | Show only currently valid shortcuts |
| Single choice | Up/Down, Enter saves, Esc cancels | One confirmation for ordinary settings; no global Save button |
| Nested list | Up/Down, Enter, Esc returns | Restore parent selection; no Tab dependency |
| Getting started | Up/Down, PgUp/PgDn, Esc | Scroll Markdown and show position |
| Delete, clear, import consent, shell change | Explain actual impact; default Cancel | Reread state after success; preserve context on failure |
| Authentication or sudo | Suspend fullscreen and restore normal terminal | Delegate to existing flow; resume guide and reread results |

Use a draft while editing; Esc never persists. Saving one setting must not activate unrelated features.
Disable repeat submissions while pending. Distinguish saved, effective now, next session, and next login.
Read failure is Unknown/Unable to read, never Off or Not configured. Avoid redundant confirmations and tutorial gates.

## 4. Five settings

### Language / 语言

Options: Follow system, English, Simplified Chinese. Keep a bilingual entry to recover from the wrong language.
Show saved and effective values separately, including `COSH_SHELL_LANG` override and its implications.
Reuse parsing/writing; refresh guide UI immediately after a successful save. Do not promise fixed AI reply language.

### AI service

Separate the access path from connection verification. Automatic access without a saved provider
is a candidate, not proof of trial eligibility, quota or successful model access. Unconfirmed
environments invite a direct question; configured services remain connection-unverified.
Read failures and disabled automatic access are separate states. Incomplete explicit settings keep
their route and never silently fall back to a trial.

The AI page defaults to Start asking questions, which closes the wizard and opens the existing
Cosh session without submitting a question. Exiting returns to the caller. Manage AI service is
secondary and hands off to the existing authentication manager, returning to the wizard afterward.
Background summary updates preserve the selected action even when source/preference text moves rows.

Core owns route selection through trial_policy::decide. The existing auth state response adds
ecs_trial.route (existing/ecs_identity/disabled), identity_status
(not_checked/supported/unsupported/unknown/failed), and a safe probe error_code.
effective_auth_required and legacy available/active keep their previous meanings. With no probe,
macOS is unsupported by platform fact and other platforms are not_checked; a negative Linux
identity probe is unknown because timeout also yields false.
The guide does not request a probe. Old Core responses degrade conservatively; a saved ECS identity
auth_source is configuration evidence only and must still respect unreadable or disabled preferences.

Reuse templates, state machine, activation, editing, deletion and retries; no duplicate secret input/storage/validation.
Retain Aliyun console, Coding Plan, Token Plan, Bailian and OpenAI-compatible template differences.
Non-ECS AK/SK, ECS RAM role/challenge and zero-configuration service retain their existing paths.
Editing preserves provider type and credential source. Saving follows existing configure-and-activate semantics,
which must be stated before submission. System entries are read-only.
Deleting a user entry may reveal a same-name system entry or system default; reread authoritative state.
Automatic zero-config preference is distinct from the active service; allow explicit activation even when auto is on.
Represent configuration read, validation, activation and connection checks separately, including partial success.
Do not issue network checks on home; only use checks requested by the user or required by the existing flow.

### Tips and recommendations

| Item | Options/actions | Meaning |
| --- | --- | --- |
| Usage guidance | On/off | Usage hints only; reuse GuideStore and runtime publication outcomes |
| Command analysis | Suggest first / Analyze automatically / Analyze manually | smart/auto/manual; guide persists a new-session default, `/mode analysis` changes this session |
| Personalized questions | On/off | manual pauses recommendations but keeps the enabled preference; when editing defaults, say Paused in new sessions rather than inferring an existing session's state |
| Recommendation data and privacy | Bash history, retention explanation, clear | Import off by default; separate consent; explain clear versus disable |

Show saved value, effective value and source for analysis, personalized questions and Bash history import, preserving existing precedence:

| Setting | Resolution and save boundary |
| --- | --- |
| Command analysis | COSH_SHELL_ANALYSIS_MODE overrides the saved default; guide shows the resolved new-session default, not the current session's mode |
| Bash history import | COSH_RECOMMENDATIONS_BASH_HISTORY overrides the saved value; saving Off while the environment forces On must not claim import stopped |
| Personalized questions | COSH_RECOMMENDATIONS_ENABLED=0 forces Off; the existing handler rejects enabling and does not persist that request. Otherwise user preference wins over environment On, then falls back to the configured default |

For allowed saves overridden by the environment, report Saved, but overridden by an environment variable and retain the effective value/source.
For rejected requests, show the reason without reporting a save. Guide does not alter the process environment, bypass forced Off,
or imply that a new session necessarily removes an override.

Disabling recommendations also clears local recommendation data and requires confirmation.
Clear-only preserves switches when data is valid. If corruption, unsupported schema or size limits prevent reliable preference reads,
show Unable to read and reuse the existing Reset data and turn recommendations off recovery action, with explicit confirmation defaulting to Cancel.
Never guess the original preference or claim it was preserved. Permission, lock and other read errors do not automatically authorize a corruption reset.
Reread after clearing/recovery and report partial success separately from failure.
Disabling the Bash import preference stops future import only when effective; existing activity/profiles remain.
Initial synchronization reads/parses the history tail to establish a baseline without importing existing entries;
never claim it does not read old history. Cosh-session collection and Bash import remain separate.
Existing fixed retention: activity 7 days, recent profiles 14 days, frequent patterns and evidence snapshots 90 days,
also capacity bounded. Explain this policy without adding a retention setting. Bounded recent activity may reach the current AI service,
redaction is best effort, and local deletion does not delete provider records.
History import uses one page: saved/effective values and source, purpose/privacy, one primary action and default Back without changes.
Without an environment override, show Allow import and save when effectively Off, or Stop importing and save when On; stopping retains existing data.
With an override, label allowed saves Save On preference or Save Off preference and explain on the same page that the environment still controls the effective state;
do not claim import started/stopped. A read failure allows only reread or return, without guessing consent.
Do not add a preceding toggle page or second consent layer. Show full privacy text there, concise summaries on home.
Mark settings as next-session unless runtime propagation is established.

### Login health summary

The name includes health and resource information. Reuse existing config/CLI; affect subsequent logins.
Keep separate from usage guidance and AI switches. Read failure is not Off; do not reprint a login summary on save.

### Default login shell

Open the current account directly, showing username, UID and actual system shell; Choose another user is secondary.
Preserve the chosen identity across privilege elevation; show it again before writing.
Reading another account's status and changing it both require administrator access: leave fullscreen for system sudo before querying.
Cancellation/failure leaves status unknown and blocks changes. Current-account readonly queries do not add elevation.
Before connecting the view, define a private request/result contract for a controlled privileged child reusing the login owner.
Obtaining sudo authorization does not change guide's EUID and cannot make its in-process other-account query succeed.
Requests allow only Query, Set Cosh or Restore original shell, binding username and expected UID; the child revalidates identity and reuses existing queries, checks and transactions.
Guide retains its original privileges and exposes no arbitrary-command or shell-path execution interface.

| Protocol content | Required facts |
| --- | --- |
| Status snapshot | Target username/UID, actual shell, trusted original shell and transaction state, allowed actions and unavailability reasons |
| Action result | Query complete, cancelled before write, failed before write, verified change, or possibly written but unverified; include an available snapshot and specific reason |
| Transport boundary | Versioned, bounded structured requests/results; separate protocol traffic from the foreground sudo terminal, do not parse localized stdout or transmit passwords |

Results must match the target identity. After an action child starts, timeout, abnormal exit or missing/invalid results mean outcome unconfirmed;
exit 0 alone proves neither success nor absence of a write. After restoring the terminal, query through the same privilege boundary.
Block repeated mutations until confirmed, and stop on identity mismatch.
Reuse ALinux 4 managed-RPM capability checks, trusted installation verification, account queries, records and mutation.
Unsupported environments explain why. Root skips sudo only, never checks.

The implemented v2 guide worker is limited to the invoking sudo account:
`SUDO_UID`, expected UID and both NSS lookups must agree. Other-user selection
shows the explicit administrator CLI commands (`cosh login status --user USER`,
`cosh login set --user USER`, `cosh login restore --user USER`) inside the page;
it does not spawn a bare `cosh login` child or
extend the worker to arbitrary targets. A full guided other-user flow remains
future work. The parent opens the result FD before sudo; requests contain no
output path. Requests are limited to 4096 bytes and replies to 16384 bytes.
Request files must be private, regular, caller-owned files with one link;
symlinks are rejected.
Result files have the same ownership checks. Inherited sudo audit pipes are
also accepted with nonblocking writes; devices and sockets are rejected.
The existing administrator CLI retains its explicit other-user operations.

| State | Action |
| --- | --- |
| Supported Bash account | Set Cosh |
| Cosh with trusted original-shell record | Restore original shell, displaying its recorded Bash path |
| Bash already restored from a trusted record | Set Cosh; other-user guidance remains available; do not offer Restore again |
| Cosh without a trusted record, conflicting record, or read error | Explain and reread; never guess a restore target or offer a mutation |
| Installation check fails but a valid restore record exists | Block Set Cosh; retain restoration through existing checks |

Confirmation shows account, original/new paths and next-login scope. Rendering,
routing and cursor clamping share one state-derived choice list. Reread and
other-user guidance stay inside the fullscreen page. Set/Restore leave
fullscreen for foreground sudo without printing a generic handoff notice that
would remain in the original terminal; never collect/store/relay passwords in
guide. No automatic infinite retries.
Distinguish cancellation before write, write failure, and possibly-written-but-unverified outcomes.
Reread before retrying an uncertain write. A successful first set creates the record and makes Restore available.
Stay on that account's state page after success; do not switch the current terminal shell.

## 5. Independent getting started

Render concise Markdown with short headings and examples, not large monospace tables.
It is not a sixth setting or a mandatory tutorial and never submits a real AI request automatically.

1. Run `cosh` from the original terminal; execute ordinary commands such as `df -h` inside it.
2. Use `/agent` to open the question editor. Reuse authentication when AI is first needed, or preconfigure via AI service.
3. Alternatively run `cosh --ask 'Explain disk usage'` from the original terminal; do not suggest nested Cosh sessions.
4. `/help` describes session commands; `cosh --help` describes launching.
5. `exit` returns to the parent Bash when launched from Bash, but may end the login when Cosh is the login shell.

Share tutorial content with static output. Do not duplicate home configuration summaries in the tutorial.

## 6. Terminal capability and fallback

`cosh guide --plain` explicitly prints the same onboarding/configuration entry index without input, clearing or writes.
Use static output when any of stdin/stdout/stderr is not a TTY, TERM is absent/dumb, or dimensions are unknown/below 40×16.
VNC/SSH/serial names do not imply restrictions: normal terminal capabilities permit normal interaction.
Static output contains no ANSI, mouse, animations or numbered question flow. Return 0 after output, 1 on output failure.
Point to precise existing CLI/Slash actions; never authenticate, clear data or change a login shell as a fallback side effect.
On resize below minimum during a TUI session, show a compact terminal-too-small view with Esc exit;
do not turn an unsaved draft into an executed static command.
Guide `--plain` and `--help` are standalone forms; invalid combinations exit 2; legacy on/off/status stays compatible.

## 7. Existing /help and content ownership

Retain `slash/notices.rs::render_help → HelpPanelModel → write_help_panel`, rounded complete border,
existing group order, two-space command/six-space summary indentation, scope labels, command highlighting,
and bold underlined headings. The baseline registry has 18 public records and 17 grouped card entries;
`/help` itself has group=None and remains omitted from that card.
Do not copy the prototype renderer shell or introduce fullscreen, line numbers, arrow navigation or an Enter/Esc return step.
Preserve current-session state in the card. Cross-entry and composition hints follow it, then normal Shell input resumes.

Production sources of truth are invocation/CLI definitions, SlashCommandSpec, i18n and existing configuration descriptors.
Share entry names, tutorials and status terminology; prototype JSON must not become a second command registry.
Append MessageIds in trailing segments to preserve stable discriminants. Keep bilingual command examples identical.
Reserve only the exact root `cosh --help`; explicit `cosh --shell bash --help` and existing shell argument shapes still pass through.
Keep internal `cosh-shell --help` developer output; exclude raw/demo/private workers from the public Cosh index.
Help only reads bounded language config, falls back to locale/English on read failure, and does not initialize providers, logging or a shell.

## 8. Ownership and allowed changes

| Responsibility | Owner/files |
| --- | --- |
| Root help | runtime/invocation.rs, new runtime/help.rs, runtime/mod.rs, main.rs; lib.rs/main.rs only raise the i18n macro recursion limit |
| Guide CLI, content, coordination | config/guide/cli.rs and private siblings; existing config owners |
| Ratatui views and lifecycle | ui/guide files, existing terminal guards and Markdown renderer |
| Authentication | Existing auth state machine and controller integration, minimal reusable call boundary |
| Recommendations, health, analysis default | Existing slash/recommendations and recommendation/config/login_health/mode owners |
| Login shell | runtime/login and existing shell_host effects; no independent chsh/usermod implementation |
| Slash reference | slash/registry.rs, slash/notices.rs, i18n; preserve HelpPanel renderer |
| Verification and user docs | Owner unit tests, raw_cli/startup, relevant logic/shell_host suites; bilingual component README and repository user-guide |

No lock/dependency/.env/version/changelog changes or agent_render migration.
Split additions into private owner files rather than growing oversized implementations. The plan defines per-stage scope.

## 9. Acceptance, risks and rollback

Walk new-user paths: empty config → default question action → one question; optional AI management; language-only change;
recommendation disable/clear/cancel; current/other-user login shell → sudo → verified state.
Cover zh/en, light/dark/monochrome, 80×24/60×20/40×16/below-minimum, resize, redirection and TERM=dumb.
Prototype frame checks do not establish real PTY/VNC/provider/sudo or Linux RPM acceptance.
Regress static fallback, legacy CLI, registry coverage and argument transparency.
Host tests use temporary homes only, never real credentials/accounts/system files.
Run terminal/account cases in controlled Linux environments, clean resources and record scope;
architecture and packaging gates remain required before release.

Risks: auth/runtime coupling, destructive recommendation semantics, sudo/terminal recovery, partial success and overrides.
Keep legacy subcommands throughout staged delivery; UI rollback must not migrate/delete credentials or account records.
If a handler cannot run independently, first establish and test one owner boundary, then connect the view.
