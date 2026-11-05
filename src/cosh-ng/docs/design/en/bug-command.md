# cosh-ng `/bug` Slash Command Design

Date: 2026-10-13

Related document: [shell integration and typed
cards](shell-integration-and-card-types.md).

## Status

**Implemented in this change; not yet released.** This document describes the
cosh-ng `/bug` behavior and its security boundaries.

## Background

A valuable CLI bug report needs environment facts users rarely have at hand:
shell version, backend, provider, model, session id, OS, approval and analysis
modes. cosh-shell already renders exactly these facts in `/status`; asking
users to run it and transcribe the panel into the GitHub form loses
information and adds friction.

The missing piece is an in-session path: a user who just hit a bug inside
cosh-shell should not have to leave the shell or retype what `/status` already
shows. `/bug <description>` therefore collects an allowlisted snapshot,
pre-fills the issue form through a URL template, and leaves review and
submission to the user.

## Operating constraints

- Primary environments include headless Linux servers over SSH, where a
  browser usually does not exist.
- The repository issue form expects the `component`, `problem`, `reproduce`,
  `diagnostics`, and `additional` field ids.
- Slash commands span four layers plus two shell marker scripts, with tests
  pinning their consistency.
- Provider endpoints may be internal; they must never enter the report URL.

## Goals

- `/bug <description>` produces a review-ready, pre-filled GitHub issue URL in
  the terminal in one step, with zero network I/O from the command itself.
- The snapshot is richer than `/status` but still credential-free: identity
  fields plus fail-quiet platform facts — kernel, distro, libc, wrapped
  shell, install attribution. No endpoint, no path, no log, no conversation
  content, no credential, no telemetry identifier.
- Evidence remains under user control: the panel points the user at the latest
  files under `~/.copilot-shell/logs/` to review and paste into the issue; the
  command itself reads no logs and writes no files.

## Non-goals

- Raw logs, conversation history, or telemetry inside the pre-filled URL;
  log evidence stays in the user's hands, pasted by them and reviewed by
  them.
- Submitting the issue without user review (no API or OAuth path).
- Running outside the interactive shell.
- Customizable snapshot fields or redaction rules.

## Design decisions

### D1 — URL pre-fill, zero command network I/O

This approach needs no OAuth flow or token handling, keeps final editorial
control with the user, and does not depend on direct API writes through
corporate proxies. The command's only outward actions are printing the URL
and handing a validated URL to the platform opener, best-effort.

Rejected: posting via the GitHub API — it needs credential management inside
the shell binary and removes the review step.

### D2 — The built-in template targets the ANOLISA issue form

```
https://github.com/agentic-os-org/ANOLISA/issues/new?template=bug_report.yml&component=cosh-ng&title={title}&diagnostics={info}
```

- `{title}` receives the user's description, capped at 120 characters.
- `{info}` receives the environment snapshot (D4).
- `component=cosh-ng` is a fixed literal: GitHub issue forms pre-fill fields
  from query parameters matching field ids, and `cosh-ng` matches the
  component dropdown option exactly. No extra placeholder is needed.
- Both placeholders are replaced exactly once via plain string replacement
  (no regex), keeping the encoding contract explicit.

Rejected: pre-filling `problem` or `reproduce` — both are
the user's story to tell, and synthesizing them from a one-line description
adds template machinery without adding information; the form's own required
prompts guide the user there.

### D3 — The URL is the primary deliverable; opening a browser is best-effort

On an ECS instance over SSH there is usually no browser, so the full URL is
always printed as one unframed line after the notice panel. This prevents
terminal wrapping from mixing border glyphs into a copied URL. The command
first attempts the platform opener
(`xdg-open` on graphical Linux, `open` on macOS) only when stdout is a terminal;
headless Linux skips the attempt. Failure to open is an informational line in
the panel, never a command failure, and the URL stays visible.

Rejected: dropping the open attempt entirely — on desktop Linux and macOS it
saves a copy/paste and is harmless when it fails.

### D4 — Snapshot = machine transcription of `/status`, plus fail-quiet platform facts

Two tiers, both rendered as `label: value` lines.

Identity fields reuse the `/status` resolution verbatim:

- `version` — `CARGO_PKG_VERSION`
- `backend` — adapter name
- `provider` — provider id and type in display form; **never** the endpoint
- `model`
- `session` — committed session id (local-file scope; same exposure as
  `/status`)
- `os` — `os arch`
- `modes` — approval and analysis labels

Platform facts make the report self-sufficient for triage. Every probe is
fail-quiet — a field is simply absent when its source is unavailable:

- `kernel` — `/proc/sys/kernel/osrelease` on Linux, `uname -r` on macOS
- `distro` — `PRETTY_NAME` from `/etc/os-release`, reusing the existing
  health-collector parser (`parse_os_release_pretty_name`)
- `libc` — the compile-time target env (`gnu`/`musl`); a constant, not a probe
- `shell` — the wrapped shell kind (`RawShellKind`: bash/zsh) and integration
  mode; marker and routing bugs are shell-specific, and this field answers
  the first question for that bug class
- `install` — RPM attribution of the current executable via a read-only
  `rpm -qf --queryformat` query, so an RPM-pinned install and a source build
  are distinguishable at a glance; absent when `rpm` is missing or the
  binary is not RPM-owned
- `language` — active UI language

Every field maps to a concrete triage question and is a static environment
fact: no user identifier, no filesystem path (the install field carries only
the owning package name-version), no telemetry id.

**No file output, one nudge toward the logs.** `/bug` produces no file and
reads no log — free-form log text is exactly what keyword redaction cannot
promise to clean, so the safest design is the one that never touches it.
What the command does instead is a single panel line pointing the user at
the latest files under `~/.copilot-shell/logs/` (the structured-logging
directory) to review and paste into the issue if they find them relevant —
the user decides what travels, and the form's own "remove tokens, keys,
private addresses" reminder backs them up. The line is shown only when the
directory exists (one stat, injected like the opener); it disappears
quietly otherwise.

Rejected: container / BTF / eBPF-capability probing — those distro-level
facts need costlier cgroup and proc heuristics, and shell triage rarely hinges
on them.
Rejected: collecting the cosh-core binary version — there is no verified
registry surface for it today; provider, model, and session already
identify the conversation, and user-pasted logs cover version skew when it
matters.

### D5 — Registration spans four layers plus both shell markers

cosh-ng has no command loader; a slash command exists only when all of:

1. `slash/registry.rs` — `SlashCommandSpec { name: "/bug",
   usage: "/bug <description>", summary_id: MessageId::HelpSummaryBug,
   group: Some("Health"), scope: "read-only", state: Public }`.
2. `slash/parser.rs` — `SlashCommand::Bug(&str)` parsed from the raw
   remainder after `/bug`, with `/bug` added to the quoted-arguments
   whitelist so natural-language descriptions survive (same contract as
   `/task`).
3. `slash/commands.rs` — dispatch to `render_bug_command`.
4. `slash/bug.rs` — the implementation module, plus new `MessageId` entries
   (help summary and panel copy) with en-US and zh-CN translations.

are updated — **and** both shell marker scripts
(`shell_host/marker/bash.sh` and `zsh.rs`) gain `/bug` in their exact-match
case lists. Registry unit tests already pin marker-to-registry equality and
the input classifier pins every exact command, so a missed marker update
fails loud in unit tests; that is the intended guardrail, not an extra
burden. The model cannot invoke `/bug`: slash input is intercepted before
agent routing.

### D6 — Hand-rolled opener, no new dependencies, injectable for tests

cosh-shell has no browser-open capability and the workspace carries no
`open`/`webbrowser` crate. Opening is roughly fifteen lines: validate, then
`std::process::Command::new(opener).arg(url)` with no shell in between, so
the URL itself is the only injection surface. Child stdio is null and a
background waiter reaps the opener without blocking the prompt. Validation
before spawn: the scheme must be `http:` or `https:`, and any newline or
control character in the final URL aborts the open.

The opener is injected into the render path as a closure — the real spawner
in production, a no-op in tests. This keeps raw_cli tests from launching
browsers on developer machines and macOS CI.

Rejected: adding the `open` crate — a new dependency for two platform
branches fails the reuse-first bar; revisit only if Windows support ever
matters.

### D7 — Interactive shell only

cosh-ng slash commands exist only in the interactive shell. Keeping `/bug`
within that scope avoids adding a separate non-interactive parse and render
surface.

On streaming: cosh-ng has no `canRunDuringStreaming` flag because slash
rendering is a synchronous, local path that never touches the adapter stream.
The invariant this design requires — `/bug` stays executable while a response
streams and must not block on adapter state — is locked by the raw_cli test
listed under Verification.

## Interaction flow

1. The user types `/bug <description>`; the description is optional.
2. The snapshot is collected synchronously — identity resolution costs the
   same as `/status`; the platform facts are bounded read-only probes (two
   file reads, one compile-time constant, shell-host state, one `rpm -qf`
   query) that fail quiet when a source is unavailable.
3. The URL is built: percent-encode the title and the snapshot, then replace
   `{title}` and `{info}` once each in the built-in template.
4. When stdout is a terminal, the command best-effort opens the URL; Linux
   attempts this only with `DISPLAY` or `WAYLAND_DISPLAY`. The notice panel
   then renders guidance, the open result, and — when the logs directory
   exists — a one-line nudge to review and paste its latest files. The full
   URL is printed as one unframed line after the panel. The command always
   completes successfully.

## Privacy and security

- Allowlist construction: only the fields listed in D4 exist; there is no
  collect-then-filter step to get wrong.
- No endpoint, no workspace path, no telemetry identifier, and no credential
  ever enters the snapshot; install attribution exposes only the owning
  package name-version. Redaction is by omission, before encoding, so
  nothing sensitive can reach the query string.
- The platform probes are read-only: two file reads
  (`/proc/sys/kernel/osrelease`, `/etc/os-release`), one compile-time
  constant, shell-host state, and one read-only `rpm -qf` query. No user
  data is read.
- The command itself performs no network I/O, writes no files, and reads no
  logs; its only outward actions are printing the URL and handing it to the
  platform opener.
- Loading the form — via the best-effort open or by the user clicking the
  printed link — sends the pre-filled `title` and `diagnostics` query
  parameters to github.com. The issue itself is created only when the user
  submits the form. Log content reaches GitHub only when the user pastes it
  themselves, and the form reminds them to strip secrets first.
- Final-URL validation (scheme allowlist, control-character rejection) runs
  immediately before spawn; the spawn passes the URL as a single argv entry
  with no shell.

## Verification

- `slash/bug.rs` unit tests: built-in-template URL with both placeholders
  percent-encoded and replaced exactly once; title capping; an empty
  description emits no `title` param; UTF-8 query-component encoding.
- `slash/bug.rs` unit tests: the log nudge line appears only when the logs
  directory exists; the injected opener is skipped without a terminal and
  its success is reported when available.
- `registry.rs` unit test: `/bug` is Public, in the Health group, read-only
  scope — mirrors the existing config-controls test.
- `parser.rs` unit test: a quoted description survives intact — mirrors the
  `/task` test.
- Existing pinned tests (marker-to-registry equality, classifier coverage)
  force the `bash.sh`/`zsh.rs` updates at build time.
- `tests/raw_cli/slash.rs`: `/bug <desc>` renders the en-US panel and encoded
  URL, then returns to the prompt without reaching bash. Piped stdout keeps
  the opener disabled; translation completeness is covered by the existing
  i18n catalog tests.
- Manual: on desktop Linux the browser opens the pre-filled form; over SSH on
  a headless host the panel shows the URL and the open failure stays
  informational.

## Out of scope

- Logs, conversation, telemetry, or diagnostic bundles inside the URL.
- Reading any log file or writing any file.
- Unreviewed submission via API or OAuth.
- Custom issue targets, snapshot fields, or redaction rules.
- Non-interactive and ACP slash surfaces.

## Open questions

1. **telemetry_id in the snapshot?** It would let maintainers correlate a
   public issue with the device's own telemetry, but it is a pseudonymous
   device identifier, and opt-out users should not have it surfaced. Default
   in this design: excluded. Decision needed before implementation.
2. **OSC 52 auto-copy of the URL?** The `/copy` command already exercises the
   clipboard path; auto-copying the bug URL would save a selection step.
   Default: not in the first cut.
3. **Deeper diagnostics riding along?** Deliberately not: bundle write-out,
   structural summaries, and upload relays all presuppose the command
   handling log content, which the simplest version avoids. If in-shell
   manual export (`/debug diagnostics`) is ever wanted, it is its own
   design — `/bug` stays a link builder.
