# ANOLISA Commit Message Standard

> Both human contributors and AI agents MUST follow this specification.

This file is the single source of truth for commit messages and commit
discipline. `AGENTS.md` §6 and §13, and the commit sections of
`CONTRIBUTING.md` / `CONTRIBUTING_zh.md`, redirect here and intentionally do
not duplicate these rules. Do NOT rely on cached or memorized rules — read
this file directly before composing a commit message.

## 1. Subject line

Format: `type(scope): imperative description`, following
[Conventional Commits](https://www.conventionalcommits.org/).

- **Repository maximum: 50 characters** (commitlint currently hard-fails above 120)
- Language: **English only**
- Imperative mood ("add", "fix", "remove" — not "added", "fixes", "removing")
- Lowercase first letter, no trailing period
- Breaking changes: append `!` before colon, e.g. `feat(cosh)!: remove legacy flag`

## 2. Body (when non-trivial)

Separate the body from the subject with a blank line. First explain the concrete
change and its main effect for users or maintainers. The subject's scope
classifies the commit; it does not replace naming the affected command or
component in the body. Follow the shared
[change description standard](change-description-standard.md) for reader context
and clear references.

Supporting paragraphs explain how the implementation achieves that effect and
why the choice is appropriate. Name a crate or path where that helps the reader
locate the change. Include meaningful alternatives, compatibility constraints,
limitations and trade-offs when relevant, not as a fixed three-part template.

The subject's imperative form does not require subjectless imperative prose
throughout the body. A sequence such as "Expose facts. Keep errors. Retain
contracts." is insufficient when the reader still cannot tell which program
changes or what users gain.

Wrap body prose at 72 columns and keep each inline command intact on one
physical line. Do **not** restate the diff line by line or paste design docs.

### User-visible changes are deltas

End a non-trivial commit body with `User-visible changes:` before the trailers.
This section describes changes relative to the commit's parent, while making
clear when the underlying behavior already existed on main. It is not a command
reference or a list of what the software can do.

- Distinguish a new command, a new option, a new mode of an existing command, a
  changed behavior or output, a code-only move, and retained compatibility.
- State the actual parameter, default, output or removal change. If an operation
  merely becomes available through another entrypoint, say which entrypoint is
  new and where the operation was already available.
- Give each affected complete command its own bullet in closed backticks. Keep
  the command on one physical line, wrapping its explanation separately. Do not
  combine command names with slashes or omit a shared prefix.
- Include compatibility and removed commands when relevant. For internal-only
  changes, explicitly state that command syntax and behavior are unchanged.
- Claim only behavior implemented by this commit. A name in the design or a
  parsed but unused flag is not a delivered feature.

## 3. Trailers

```
Assisted-by: <tool>:<version>
Signed-off-by: Name <email>
```

`Assisted-by` goes **above** `Signed-off-by`. Omit `Assisted-by` if no AI was
involved. Every commit must carry a `Signed-off-by` trailer using the
contributor's own Git identity.

```bash
git commit \
  --trailer "Assisted-by: Qoder:1.7.0" \
  --trailer "Signed-off-by: $(git config user.name) <$(git config user.email)>" \
  -m '...'
```

### Tool identifier detection

| Detection method | Tool identifier |
|---|---|
| `$QODER_VERSION` env var | `Qoder:<ver>` |
| `$CLAUDE_CODE_VERSION` env var | `Claude Code:<ver>` |
| Parent process is Qoder.app / QoderWork.app | Read `CFBundleShortVersionString` from app bundle |
| Parent process is Claude.app | `Claude:<ver>` |
| Parent process is Cursor.app | `Cursor:<ver>` |

When generating commits, detect the active tool and fill in the actual version.
Do **not** hardcode a fixed string like `Qoder:latest`.

## 4. Scope

> **scope is mandatory** — CI will error if scope is missing.

The values below are the repository's scopes; an unlisted scope produces a
warning and should be justified or replaced before review.

### Scope Inference (by changed file path)

| Changed path | Scope |
|---|---|
| `deprecated/copilot-shell/` | `cosh` |
| `src/cosh-ng/` | `cosh-ng` |
| `src/agent-sec-core/` | `sec-core` |
| `src/os-skills/` | `skill` |
| `src/agentsight/` | `sight` |
| `src/tokenless/` | `tokenless` |
| `src/ws-ckpt/` | `ckpt` |
| `src/agent-memory/` | `memory` |
| `distribution/anolisa/` | `anolisa` |
| `src/skillfs/` | `skillfs` |
| `src/ktuner/` | `ktuner` |
| `src/aw/` | `aw` |
| `benchmark/` | `benchmark` |
| `.github/workflows/` | `ci` |
| `docs/` | `docs` |
| `**/package*.json`, `Cargo.lock`, `*.toml` (dep bumps) | `deps` |
| Other root-level config / scripts / tooling | `chore` |

**Multi-component changes**: use the scope covering the most changed files.

## 5. Atomicity

- One commit = one logical change
- Scope must match the actual files changed
- Every commit in a PR must compile independently
- Squash fixup commits before merge
- Fold tests and formatting fixes into the logical commit when possible

## 6. Rebases and fix attribution

### Refresh before rebasing

Before **every** rebase, use `git remote -v` to identify the remote whose fetch
URL points to the official `agentic-os-org/ANOLISA` repository. Remote names
alone do not establish repository identity:

- In the documented fork workflow, `origin` is the contributor's fork and
  `upstream` is official. Run `git fetch upstream main`, then inspect divergence
  between `HEAD` and the freshly fetched `upstream/main`.
- When `origin` points to the official repository, run `git fetch origin main`,
  then inspect divergence between `HEAD` and the freshly fetched `origin/main`.

For another remote name, use that verified official remote consistently for
both the fetch and the comparison. If none is configured, configure an official
remote before continuing; do not substitute the fork's `main`.

Do not rely on an earlier fetch or a remembered remote-tracking state. This also
applies to a rebase used only to edit commit messages. Fetching does not change
a deliberately pinned review base; preserve that base unless the task calls for
updating it.

### Attribute fixes to their source

Every fix commit must be attributed correctly:

| Situation | Action |
|-----------|--------|
| Bug introduced by a commit **in this PR** | `git commit --fixup=<hash>` then `rebase --autosquash`. No separate commit. |
| Bug introduced by a commit **already on main** | Standalone commit with `Fixes: <hash> ("<subject>")` in body. |
| Enhancement supplementing a feature **already on main** | Standalone commit with `Supplements: <hash> ("<subject>")` in body. |
| Brand-new feature or unrelated change | Standalone commit, no Fixes/Supplements needed. |

**Never** create a standalone "fix" commit for something introduced earlier in
the same PR branch. Always amend or fixup into the originating commit.

## 7. Version bump and CHANGELOG timing

- Use `chore(<scope>): bump version to X.Y.Z` (not `release(...)` — commitlint
  rejects non-standard types).
- Version bump is always the **last commit** in a feature branch.
- All version-bearing files for the component must be updated atomically. This
  includes whichever of the following exist: `Cargo.toml` or `package.json`,
  `.anolisa/component.toml`, `manifests/<name>.toml`, `dist/<name>.spec`, and
  `CHANGELOG.md`.
- `CHANGELOG.md` (and its `_zh` counterpart) is edited **only** in the version
  bump commit. Feature, fix, docs and test commits must not touch it, even when
  the change is user-visible — the entry is written when the version is cut,
  together with the other version-bearing files. Do not add a changelog line to
  a feature PR and expect the bump to merge it.

## 8. Format check

Run the component's applicable formatter before every commit (e.g.
`cargo fmt --all` for Rust from the workspace root, `npm run format` for
TypeScript, `make python-code-pretty` for Python). CI will reject formatting
diffs. If a rebase introduces format changes, amend them into the commit that
caused the change — do not create a standalone "style" commit.

## 9. Examples

### An existing command gains a software mode

In this example, the parent commit already had
`aw install --config FILE --agent TARGET` for connecting native Hooks.
The software installer already existed behind `aw-package`. The change adds
software installation to `aw`, including its local mode's `--prefix` and
`--bundle` options. These facts describe this example's parent, not an enduring
claim about what a later main branch supports.

Poor description:

```text
User-visible changes:
- `aw install`: add package installation.
```

This hides the existing command's meaning and makes the old package installer
sound like new functionality.

Clear description:

```text
feat(aw): expose software installation in the CLI

The aw command now offers local software installation through its
existing package library. Users can install a local bundle without
switching to the separate aw-package executable.

User-visible changes:
- `aw install --prefix DIR --bundle DIR`:
  The existing command gains a software mode and these two options.
  The installer was already available through aw-package; this change
  adds its aw entrypoint rather than a new installation engine.
- `aw install --config FILE --agent TARGET`:
  The existing native Hook-connection mode retains its arguments and
  behavior.

Assisted-by: <tool>:<version>
Signed-off-by: Zhang San <zhangsan@example.com>
```

These are examples of an accurate delta, not required sentences or a mandatory
layout for every commit. Replace the trailer placeholders with the detected
tool/version and the contributor's own Git identity.

## 10. What CI checks

Commit message linting is configured in `.github/commitlint.config.json`, which
extends `@commitlint/config-conventional`:

| Rule | Level | Effect |
|---|---|---|
| `scope-empty` | error | A commit without a scope fails. |
| `header-max-length` | error | Subject longer than 120 characters fails (repository convention is 50). |
| `scope-enum` | warning | Scope outside the table in §4 warns. |
| `body-max-line-length` | warning | Body lines longer than 100 characters warn. |

The looser CI thresholds do not replace the repository's 50-character subject
limit and 72-column body prose convention.

If **Commit Message Lint** rejects a commit, follow the recovery workflow in
`CONTRIBUTING.md` §Commit messages.
