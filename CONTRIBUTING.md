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

## Repository at a glance

ANOLISA is a monorepo. The twelve components and their supported development
platforms are listed below. Build names are accepted by `build-all.sh`; commit
scopes are used in commit subjects and PR titles.

| Component | Path | Platform | Build name | Scope | Development entry and minimum local gate |
| --- | --- | --- | --- | --- | --- |
| copilot-shell | `deprecated/copilot-shell/` | All platforms | `cosh` | `cosh` | `cd deprecated/copilot-shell`; `make deps`, `make build`, `make lint`, `make test` |
| cosh-ng | `src/cosh-ng/` | Linux full; macOS limited functionality | `cosh-ng` | `cosh-ng` | `cd src/cosh-ng`; `cargo build --workspace`, `cargo fmt --all -- --check`, then run the closest targeted test described in `src/cosh-ng/CONTRIBUTING.md` |
| agent-sec-core | `src/agent-sec-core/` | Linux only | `sec-core` | `sec-core` | Python 3.11.6 and `uv`; `make build-all`, `make test` |
| agentsight | `src/agentsight/` | Linux full eBPF; macOS `trace`/`serve` only | `sight` | `sight` | Linux uses `make build-all`; macOS uses `make build-mac`; Linux runs `make lint`, `make test` |
| tokenless | `src/tokenless/` | Linux for full development; macOS x64/arm64 for shipped CLI binaries and npm adapters | `tokenless` | `tokenless` | `make build`, `make lint`, `make test` |
| agent-memory | `src/agent-memory/` | Linux only | `memory` | `memory` | `make build`, `make fmt-check`, `make lint`, `make test`; use `make smoke` for MCP changes |
| os-skills | `src/os-skills/` | Assets are cross-platform; individual scripts declare limits | `skills` | `skill` | Static Markdown skill definitions and shell assets; `make build` confirms that no compilation step is required |
| anolisa | `distribution/anolisa/` | Linux and macOS arm64 | n/a | `anolisa` | `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` |
| SkillFS | `src/skillfs/` | Linux only | n/a | `skillfs` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; run `scripts/test.sh` for FUSE changes |
| ws-ckpt | `src/ws-ckpt/` | Linux only | `ws-ckpt` | `ckpt` | `make build`, `make test` |
| ktuner | `src/ktuner/` | Linux only | n/a | `ktuner` | `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` |
| blaze | `src/blaze/` | Linux only | n/a | `blaze` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` |

`tokenless` can publish macOS artifacts, but its binaries are cross-compiled
from Linux. Do not use a macOS checkout as the tokenless build environment.
The Linux-only components must be built and tested on Linux. `cosh-ng` and
`agentsight` have limited macOS paths described in the matrix.

### Toolchain prerequisites

- Node.js 20 or newer for `copilot-shell` and TypeScript adapters.
- The stable Rust toolchain for Rust components. Follow a component's
  `AGENTS.md` when it pins a toolchain or adds native libraries.
- Python **3.11.6** and [uv](https://docs.astral.sh/uv/) for
  `agent-sec-core`. `os-skills` is primarily static Markdown and shell data;
  it does not own the repository Python environment.
- `clang` and `libbpf` headers when changing `agentsight` eBPF probes.

## Fork and create a branch

Fork the repository and clone your fork. Before starting a change, update your
local `main` from the official repository and create a working branch.

```bash
git clone https://github.com/<your-account>/anolisa.git
cd anolisa
git remote add upstream https://github.com/alibaba/anolisa.git
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

`build-all.sh` supports eight components only:

- Default set: `cosh`, `skills`, `sec-core`, `tokenless`, `ws-ckpt`, `memory`.
- Optional set: `cosh-ng` and `sight`. Add `--all` or name them with
  `--component`.

It does not build `anolisa`, `skillfs`, `ktuner`, or `blaze`. Use their
component gates in the matrix above for those four components.

With no install-mode flag, component files install to the user profile under
`$HOME/.local` without elevated installation privileges. Dependency bootstrap
may still request `sudo` for system packages. System installation uses
`--system` or `--usr` and may require `sudo`. For development, prefer a
targeted build with installation disabled.

```bash
./scripts/build-all.sh --help
./scripts/build-all.sh --no-install --component cosh
./scripts/build-all.sh --no-install --component sec-core
./scripts/build-all.sh --no-install --all
```

The main options are:

| Option | Effect |
| --- | --- |
| `--no-install` | Install dependencies and build, then skip installation. |
| `--install-mode <mode>` | Select `user` or `system`; `user` is the default. |
| `--usr`, `--system` | Select system installation mode. |
| `--ignore-deps` | Skip dependency installation. |
| `--deps-only` | Install dependencies without building. |
| `--uninstall` | Remove installed files; combine with `--component` to target a component. |
| `--dry-run` | Print actions without changing files or systemd state. |
| `--interactive`, `--non-interactive` | Open the guided flow or explicitly select automation mode. |
| `--all` | Include optional components `cosh-ng` and `sight`. |
| `--component <name>` | Build or uninstall one supported component; repeat for multiple components. |

Do not treat `build-all.sh --all` as a build of every `src/<component>/`
directory. Its scope is limited to the eight names printed by
`./scripts/build-all.sh --help`.

The `sight` selection uses the Linux eBPF build. On macOS, build AgentSight
locally with `make build-mac` instead.

See [`docs/BUILDING.md`](docs/BUILDING.md) and
[`docs/BUILDING_zh.md`](docs/BUILDING_zh.md) for the longer dependency and
packaging reference.

### Convenience test runner

`tests/run-all-tests.sh` is a convenience aggregator for five components:
`copilot-shell`, `agent-sec-core`, `agentsight`, `tokenless`, and
`agent-memory`. It accepts these filters:

```bash
./tests/run-all-tests.sh
./tests/run-all-tests.sh --filter shell
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

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/) with an
English, imperative subject in this form:

```text
type(scope): imperative description
```

The scope is mandatory. Commitlint rejects an empty scope. The following
values are the repository's recommended scopes; an unlisted scope produces a
warning and should be justified or replaced before review.

| Scope | Path or purpose |
| --- | --- |
| `cosh` | `deprecated/copilot-shell/` |
| `cosh-ng` | `src/cosh-ng/` |
| `sec-core` | `src/agent-sec-core/` |
| `skill` | `src/os-skills/` |
| `sight` | `src/agentsight/` |
| `tokenless` | `src/tokenless/` |
| `ckpt` | `src/ws-ckpt/` |
| `memory` | `src/agent-memory/` |
| `anolisa` | `distribution/anolisa/` |
| `skillfs` | `src/skillfs/` |
| `ktuner` | `src/ktuner/` |
| `blaze` | `src/blaze/` |
| `deps` | Dependency changes, including lockfiles. |
| `ci` | `.github/workflows/` and CI configuration. |
| `docs` | Documentation-only changes. |
| `chore` | Root scripts, tooling, and other maintenance. |

Repository convention limits the full subject to 50 characters; commitlint
currently enforces a hard ceiling of 120 characters. Start the description
with lowercase, and omit a trailing period. Fold tests and formatting fixes
into the logical commit when possible. Every commit must carry a
`Signed-off-by` trailer using the contributor's own Git identity:

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

Use `git commit --fixup=<commit>` followed by an autosquash rebase when fixing
a defect introduced earlier in the same PR. A fix for code already on `main`
uses a standalone commit with a `Fixes:` reference to the introducing commit.
An enhancement to a feature already on `main` uses `Supplements:`. Keep a
component version bump as the final commit in its feature branch and update
all version-bearing files atomically.

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

## Documentation synchronization

Documentation changes must follow
[`specs/documentation-standard.md`](specs/documentation-standard.md) and keep
English and Chinese pages semantically equivalent. Command examples remain
identical in both languages. The root guide stays a cross-component overview;
long usage and architecture material belongs in the canonical locations below.

| Change | Update in the same PR |
| --- | --- |
| CLI command or flag | Component `README.md` and the relevant `docs/user-guide/` page. |
| Configuration option | Component `README.md` and the relevant `docs/user-guide/` page. |
| Installation method | `docs/QUICKSTART*.md` and the component README. |
| Architecture or protocol | `src/<component>/docs/design/`. |
| New component | Root README and `NOTICE` when applicable. |

Daily feature and fix PRs update README and user-guide pages. Release version
bump PRs aggregate user-perceivable entries into `CHANGELOG*.md`.

## License

By contributing, you agree that your contributions are licensed under the
Apache License 2.0.
