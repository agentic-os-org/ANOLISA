# Contributing to ANOLISA

[中文版](CONTRIBUTING_zh.md)

This guide describes the repository-wide contribution flow. It covers the
development entry point and the minimum local checks for every component. Keep
architecture and component-specific rules in each component's own
`AGENTS.md`, `README.md`, or `CONTRIBUTING.md`.

> **Done coding?** Ask your AI assistant to read `AGENTS.md` and help draft the
> commit message and PR description.

Before changing documentation, read
[`specs/documentation-standard.md`](specs/documentation-standard.md). It is the
source of truth for file placement, bilingual names, and documentation
updates.

## Recommended contribution directions

If you are looking for a contribution direction, start with [Ways to Contribute](contribute/en/tracks.md) to pick a track that fits your background and environment, or explore component topics in [Community Roadmap](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap).
For community roles, decision tiers, and component lifecycle rules, see [Community Governance](GOVERNANCE.md).
Bug reports, bug fixes, and other Issues and PRs are also welcome.

Roadmap topics list candidate directions; scope and claiming happen on the corresponding Issue. See [Issues, Roadmap, and Claiming Workflow](contribute/en/workflow.md) for the full label and `/claim` guide, or follow these steps:

1. Search for an existing Issue covering the direction. If none exists, open an
   Issue linking to the source Discussion and describe the proposed scope.
2. Confirm the current version, scope, and acceptance criteria with the component
   owner before starting implementation.
3. Once the Issue is ready to claim, follow the existing
   [community claiming process](.github/TRIAGE.md#claiming-community-work) and
   comment `/claim` on the Issue.
4. Implement the agreed scope and submit a PR linking to the Issue. Use
   `closes #<issue-number>` when the PR completes the Issue.

## Repository at a glance

ANOLISA is a monorepo. The table below lists component development platforms
and entry points. Build names are accepted by `build-all.sh`; commit
scopes are used in commit subjects and PR titles.

| Component | Path | Platform | Build name | Scope | Development entry and minimum local gate |
| --- | --- | --- | --- | --- | --- |
| cosh-ng | `src/cosh-ng/` | Linux full; macOS limited functionality | `cosh-ng` | `cosh-ng` | `cd src/cosh-ng`; `cargo build --workspace`, `cargo fmt --all -- --check`, then run the closest targeted test described in `src/cosh-ng/CONTRIBUTING.md` |
| agent-sec-core | `src/agent-sec-core/` | Linux only | `agent-sec-core` (`sec-core`) | `sec-core` | V2 Rust CLI/daemon and sandbox: `make build`, `make test-rust-v2` (no `uv` or fixed Python runtime required); Python-specific tests use Python 3.11.6 and `uv` |
| agentsight | `src/agentsight/` | Linux full eBPF; macOS `trace`/`serve` only | `agentsight` (`sight`) | `sight` | Linux uses `make build-all`; macOS uses `make build-mac`; Linux runs `make lint`, `make test` |
| tokenless | `src/tokenless/` | Linux for full development; macOS x64/arm64 for shipped CLI binaries and npm adapters | `tokenless` | `tokenless` | `make build`, `make lint`, `make test` |
| agent-memory | `src/agent-memory/` | Linux only | `agent-memory` (`memory`) | `memory` | `make build`, `make fmt-check`, `make lint`, `make test`; use `make smoke` for MCP changes |
| os-skills | `src/os-skills/` | Assets are cross-platform; individual scripts declare limits | `os-skills` (`skills`) | `skill` | Python 3.6+ for source installation; no compilation; run the [component regression tests](#skill-regression-tests-and-path-ownership) with Python 3.11 |
| anolisa | `distribution/anolisa/` | Linux and macOS arm64 | n/a | `anolisa` | `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` |
| SkillFS | `src/skillfs/` | Linux only | `skillfs` | `skillfs` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; run `scripts/test.sh` for FUSE changes |
| ws-ckpt | `src/ws-ckpt/` | Linux only | `ws-ckpt` | `ckpt` | `make build`, `make test` |
| ktuner | `src/ktuner/` | Linux only | n/a | `ktuner` | `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` |
| aw | `src/aw/` | Linux only | `aw` | `aw` | From repo root: `python3 src/aw/scripts/check.py` (toolchain pinned by `src/aw/rust-toolchain.toml`) |

Concrete AW Providers live in [`providers/`](providers/README.md) (each with its own workspace, contract tests, and optional `--component aw-provider-<name>` build target). Cross-component evaluation runners live in [`benchmark/`](benchmark/README.md) (scope `benchmark`).

`tokenless` can publish macOS artifacts, but its binaries are cross-compiled
from Linux. Do not use a macOS checkout as the tokenless build environment.
The Linux-only components must be built and tested on Linux. `cosh-ng` and
`agentsight` have limited macOS paths described in the matrix.

### Toolchain prerequisites

- Node.js and npm for TypeScript plugins, adapters, and the website. Follow
  each package's `package.json` and build instructions for version requirements.
- The stable Rust toolchain for Rust components. Follow a component's
  `AGENTS.md` when it pins a toolchain or adds native libraries.
- Python **3.11.6** and [uv](https://docs.astral.sh/uv/) for
  `agent-sec-core` Python-specific tests and tooling (the V2 Rust build and
  `make test-rust-v2` do not require `uv` or a fixed Python runtime). The
  `os-skills` source installer and release-catalog tools
  require Python **3.6+** with its standard library; select a source-install
  interpreter with `PYTHON3=/path/to/python3`. Component regression tests use
  Python **3.11** in a separate virtual environment.
- `clang` and `libbpf` headers when changing `agentsight` eBPF probes.

## Fork and create a branch

Fork the repository and clone your fork. Before starting a change, update your
local `main` from the official repository and create a working branch.

```bash
git clone https://github.com/<your-account>/anolisa.git
cd anolisa
git remote add upstream https://github.com/agentic-os-org/ANOLISA.git
git fetch upstream
git switch main
git merge --ff-only upstream/main
git switch -c feature/<scope>/<short-desc>
```

You only need to add `upstream` once. For later changes, repeat the final four
commands with a new branch name. Keep each branch focused on one logical change.

Use one of the following names for internal branches:

```text
feature/<scope>/<short-desc>
fix/<scope>/<short-desc>
hotfix/<scope>/<short-desc>
release/<scope>/vX.Y
```

Fork branch names are accepted even when they do not match this recommendation;
the branch check reports a non-blocking warning.

## Issues and contribution scope

Open or find an issue before implementing a non-trivial feature, behavior
change, or bug fix. Security vulnerabilities must follow
[`SECURITY.md`](SECURITY.md) rather than a public issue. A genuinely trivial
change, such as a typo-only documentation fix, may use
`no-issue: <brief reason>` in the PR description.

Keep a PR focused on one logical change. If a change crosses component
boundaries, explain the contract and test impact in the PR.

## Build and test entry points

### Unified build

`scripts/build-all.sh` is the native Linux build and file installation entry point (requires Bash 4.4+). Its default stack (`--all`, or no `--component` flags) builds `cosh-ng`, `skillfs`, `ws-ckpt`, `tokenless`, `agentsight`, `agent-sec-core` (V2), and `aw`, automatically attaching matching `providers/<component>/` (`aw-provider-*`) packages. Additional selectable components are `agent-memory` and `os-skills`, with aliases `sec-core`, `sight`, `memory`, and `skills` (explicit `--component aw-provider-<name>` is also supported).

The script does not build `anolisa` or `ktuner`. Use their component gates in the matrix above.

By default, `build-all.sh` uses the **system** installation profile (`PREFIX=/usr/local`) and may use `sudo` for system build packages and direct installation. Use `--user` for user-scoped paths under `$HOME/.local` (never invokes `sudo` unless `--install-system-deps` is explicitly passed; combine with `--supported-only` to skip system-only targets). Use `--prefix /usr --destdir "$PWD/target/pkgroot"` to stage package paths without `sudo`. For development, prefer a targeted build with `--no-install`.

```bash
./scripts/build-all.sh --help
./scripts/build-all.sh --all --check-deps
./scripts/build-all.sh --no-install --component cosh-ng
./scripts/build-all.sh --no-install --component agent-sec-core --component tokenless
./scripts/build-all.sh --all --user --supported-only
```

The main options are:

| Option | Effect |
| --- | --- |
| `--all` | Select default components and their matching AW Providers. |
| `--component <name>` | Build one supported component (or `aw-provider-<name>`); repeat for multiple components. |
| `--no-install` | Prepare dependencies and build without installing files. |
| `--install` | Prepare, build, and install files (default). |
| `--system`, `--user` | Select the system (`/usr/local`, default) or user (`$HOME/.local`) installation profile. |
| `--supported-only` | With `--user`, filter to user-installable components and report skipped targets. |
| `--install-mode <mode>` | Explicit `system` or `user` profile. |
| `--prefix <path>`, `--destdir <path>` | Override target prefix or stage files under an absolute package root without `sudo`. |
| `--deps-only`, `--check-deps` | Prepare build dependencies without compiling, or inspect dependencies without changing the host. |
| `--ignore-deps` | Skip dependency setup and runtime verification; use only on a pre-provisioned host. |
| `--install-system-deps` | Explicitly allow installing native build packages in `--user` mode. |
| `--jobs <N>` | Limit maximum Cargo build jobs per component. |
| `--dry-run` | Print the plan without downloads or filesystem changes. |
| `--interactive`, `--non-interactive` | Open the guided flow or explicitly select automation mode (default). |

Legacy `--uninstall` is rejected by `build-all.sh`; remove installed files and services using the component-specific removal procedure. Use explicit component selections for development. See `./scripts/build-all.sh --help` for the complete script interface.

The `sight` selection uses the Linux eBPF build. On macOS, build AgentSight
locally with `make build-mac` instead.

See [Build from source](docs/user-guide/en/getting-started/building.md)
([中文版](docs/user-guide/zh/getting-started/building.md)) for the longer
dependency and packaging reference.

### Convenience test runner

`tests/run-all-tests.sh` provides partial convenience suites. Select the
affected maintained component explicitly with one of these filters:

```bash
./tests/run-all-tests.sh --filter sec
./tests/run-all-tests.sh --filter sight
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory
```

The runner may skip a component when `uv`, `cargo`, Linux, or the installed
`linux-sandbox` binary is unavailable, and it can still print a successful
summary. A successful run therefore does not prove that the repository or
even every selected test suite passed. Use the affected-component gate from
the matrix above as the PR acceptance check.

## Local gates by component

Run the applicable formatter, linter, tests, and any component-specific smoke
test for every component touched by the PR. The root guide intentionally keeps
the matrix small; read the scoped `AGENTS.md` before changing component
architecture, security behavior, FUSE code, or protocol contracts.

The common baseline for Rust code changes is:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Public Rust API or rustdoc changes also require
`cargo doc --workspace --no-deps`. Documentation-only changes use the
repository documentation checks and do not require Cargo validation unless a
component guide says otherwise.

Apply the component-specific commands in the repository matrix. For
`agent-sec-core`, use `uv` and Python 3.11.6 for Python tests; the version
command must report 3.11.6. For
`agentsight`, include frontend or eBPF smoke checks when those areas change.
For `agent-memory`, MCP changes require `make smoke`; for `SkillFS`,
filesystem-layer changes require `scripts/test.sh` when FUSE is available.
CI adds coverage, packaging, frontend, adapter, and integration jobs according
to the changed paths. Check [`.github/workflows/ci.yaml`](.github/workflows/ci.yaml)
when changing generated packages or framework adapters.

### Skill regression tests and path ownership

For contributors changing skill scripts, keep component regression tests in
`src/os-skills/tests/`. Tests for SkillFS belong in `src/skillfs/tests/`,
including tests of its bundled `skillfs-mount` scripts. The former
`tests/os-skills/` directory has been consolidated into these component
directories. Reserve repository-level `tests/` for shared test runners and
tests that exercise multiple components.

On Linux, make sure Git and Make are available, then run the following commands
from the repository root with Python 3.11:

```bash
python3.11 -m venv .venv
.venv/bin/python -m unittest discover -s src/os-skills/tests -v
.venv/bin/python -m unittest discover -s src/skillfs/tests -p 'test_skill_usage*.py' -v
```

The OS Skills suite uses the Python standard library and covers skill scripts,
installation, and package layout. The SkillFS command above covers the Python
log-usage scripts. Run the Rust and FUSE checks separately on Linux. Both
suites run in their component's prebuilt CI workflow.

For maintainers changing component ownership, `.github/components.json`
keeps source roots in `path_prefixes` and component-specific files outside
those roots in `supporting_paths`. A trailing slash denotes a directory;
other supporting paths name exact files. For example, the OS Skills prebuilt
action and workflow belong to OS Skills, while shared build actions and release
workflows retain their shared ownership. Update the matching component entries
in `.github/CODEOWNERS` too; CI checks that the declarations agree.

## Commit messages

Commit message rules are defined in
[`specs/commit-message-standard.md`](specs/commit-message-standard.md) —
subject format, body content, `Assisted-by` and `Signed-off-by` trailers, the
scope list with its path inference, atomicity, fix attribution, and the
version bump rule. Read that file before committing; this section keeps only
the recovery workflow that humans need.

Every commit must carry a `Signed-off-by` trailer using your own Git identity:

```bash
git commit -s -m 'docs(docs): refresh contribution guide'
```

If **Commit Message Lint** rejects the latest commit, update its message and
push the rewritten branch safely.

```bash
git commit --amend
git push --force-with-lease
```

For an older commit, select `reword` in an interactive rebase.

```bash
git rebase -i HEAD~N
git push --force-with-lease
```

Do not use plain `git push --force`. The lease prevents overwriting remote work
that is not present in your local checkout.

## Pull requests and CI

Start the PR from [`.github/pull_request_template.md`](.github/pull_request_template.md)
and keep its sections intact. The description should cover the reason for the
change, what changed, related issue or `no-issue` reason, user or agent impact,
risk and compatibility, validation commands and environment, and documentation
or rollback notes.

Use `closes #<number>`, `fixes #<number>`, or `resolves #<number>` when an issue
exists. The PR title follows `type(scope): description`. The prelint workflow
reports title, branch, issue-link, and unlisted-scope problems as warnings.
Commitlint errors, including an empty scope, invalid Conventional Commit
syntax, or a subject over 120 characters, block the prelint job.

Before requesting review, confirm that:

- The affected component gate passes on its supported platform.
- New or changed behavior has tests, or the PR explains why a test is not
  applicable.
- The PR template records risk, compatibility, and rollback considerations.
- Documentation and bilingual counterparts are updated when required.

### Draft PRs and claiming

Use a **Draft PR** (`isDraft=true`) for early work-in-progress or prototype
feedback, and mark it **Ready for review** (`isDraft=false`) when it is ready
for formal review and merge gates:

- Opening a Draft PR does **not** claim or lock an issue by itself. To reserve
  an accepted issue while iterating, comment `/claim` on the issue first (see
  [`.github/TRIAGE.md`](.github/TRIAGE.md#claiming-community-work)).
- Draft PRs do not enter formal code review unless you explicitly ask for early
  feedback in a PR comment.
- Post a short progress update if you need more time on a claimed issue, or
  comment `/unclaim` if you can no longer continue. Maintainers may check in
  after 7 days without visible progress and release inactive claims after 14
  days (staying in Draft does not extend an inactive claim indefinitely).
- When a linked issue is resolved by another PR or closed, competing
  ready-for-review PRs are closed with a courtesy note linking the resolution;
  unconverted Draft PRs do not block issue closure.

The full normative lifecycle matrix and automation rules are defined in
[`specs/issue-claim-and-pr-lifecycle.md`](specs/issue-claim-and-pr-lifecycle.md).

### Maintainer assistance

Some problems are unrelated to the PR's implementation but can block merging.
To avoid asking authors to rework their PRs for these reasons, maintainers may
handle the following problems directly without changing the implementation:

- Retry each eligible failed workflow run once per PR revision when the failure
  is caused by a known flaky test or a transient fault.
- Correct PR title formatting, or restore missing template headings and
  placeholders. Validation results, risk assessments, and issue links remain
  for the author to fill in.
- Rebase the PR branch to resolve merge conflicts where both sides' intent can
  be preserved without choosing behavior, ordering, or precedence, such as
  identical content, formatting-only differences, or independent additions at
  the same location.

Updating the PR branch requires write access to it. Authors of personal forks
can grant this access by enabling **Allow edits from maintainers**. If the fork
contains GitHub Actions workflows, GitHub labels the option **Allow edits and
access to secrets by maintainers**, because maintainers can then edit the
fork's workflows. If you prefer not to have others modify your branch, leave it
disabled or turn it off at any time on the PR page.
Rebasing changes commit hashes and invalidates existing cryptographic
signatures, but keeps commit authors, messages, and `Signed-off-by` trailers.
If cryptographic signatures are required, rewritten commits must be signed
again. Maintainers record the old and new branch heads in the PR. Before making
further changes, switch to your local PR branch, separately save uncommitted
work (including untracked files), and back up the branch; a backup branch
preserves only existing commits. The commands act on the checked-out branch.
After switching and backing up, confirm the old head is an ancestor of your
`HEAD`. If the check fails, your local history has diverged: the chain stops
before the rebase, so keep the backup and review which commits to replay first
using the [rebase recovery guidance](https://git-scm.com/docs/git-rebase#_recovering_from_upstream_rebase).
Otherwise, replay any local commits after the old head onto the new head:

```bash
git checkout <pr-branch> &&
git fetch origin &&
git branch <backup-branch> &&
git merge-base --is-ancestor <old-head> HEAD &&
git -c rebase.updateRefs=false rebase --onto <new-head> <old-head>
```

`rebase.updateRefs=false` keeps the backup branch at the original tip.

Maintainers describe what they changed in the PR. If CI still fails after the
retry, a conflict involves an implementation choice, or the PR branch cannot be
updated, the author handles it.

## Documentation synchronization

Follow the [PR documentation update rules in §5 of the documentation standard](specs/documentation-standard.md#5-pr-documentation-update-rules)
to identify which pages to update in the same PR. Keep English and Chinese pages
semantically equivalent and command examples identical.

Release version bump PRs aggregate user-perceivable entries into `CHANGELOG*.md`.

## License

By contributing, you agree that your contributions are licensed under the
Apache License 2.0.
