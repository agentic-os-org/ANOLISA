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

Separated from subject by a blank line. Cover three things:

1. What architectural choice was made
2. Why this approach over alternatives
3. Known limitations or trade-offs

Do **not** restate the diff line-by-line or paste design docs.

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

## 6. Fix attribution

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

```
feat(cosh): add --json flag to config command

Scripts need machine-readable config output; chose flat JSON over
nested to keep parsing trivial. Nested config support tracked in #55.

Assisted-by: Qoder
Signed-off-by: Zhang San <zhangsan@example.com>
```

## 10. What CI checks

Commit message linting is configured in `.github/commitlint.config.json`, which
extends `@commitlint/config-conventional`:

| Rule | Level | Effect |
|---|---|---|
| `scope-empty` | error | A commit without a scope fails. |
| `header-max-length` | error | Subject longer than 120 characters fails (repository convention is 50). |
| `scope-enum` | warning | Scope outside the table in §4 warns. |
| `body-max-line-length` | warning | Body lines longer than 100 characters warn. |

If **Commit Message Lint** rejects a commit, follow the recovery workflow in
`CONTRIBUTING.md` §Commit messages.
