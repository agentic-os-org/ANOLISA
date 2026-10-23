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

If you are looking for a contribution direction, we recommend starting with the
component topics in [Community Roadmap](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap).
Bug reports, bug fixes, and other Issues and PRs are also welcome.

Roadmap topics are candidate directions, not scheduling commitments or tasks
available to claim. To implement a roadmap direction, follow the Issue process:

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
| agent-sec-core | `src/agent-sec-core/` | Linux only | `sec-core` | `sec-core` | Python 3.11.6 and `uv`; `make build-all`, `make test` |
| agentsight | `src/agentsight/` | Linux full eBPF; macOS `trace`/`serve` only | `sight` | `sight` | Linux uses `make build-all`; macOS uses `make build-mac`; Linux runs `make lint`, `make test` |
| tokenless | `src/tokenless/` | Linux for full development; macOS x64/arm64 for shipped CLI binaries and npm adapters | `tokenless` | `tokenless` | `make build`, `make lint`, `make test` |
| agent-memory | `src/agent-memory/` | Linux only | `memory` | `memory` | `make build`, `make fmt-check`, `make lint`, `make test`; use `make smoke` for MCP changes |
| os-skills | `src/os-skills/` | Assets are cross-platform; individual scripts declare limits | `skills` | `skill` | Static Markdown skill definitions and shell assets; `make build` confirms that no compilation step is required |
| anolisa | `distribution/anolisa/` | Linux and macOS arm64 | n/a | `anolisa` | `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings`, `cargo test --locked` |
| SkillFS | `src/skillfs/` | Linux only | n/a | `skillfs` | `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`; run `scripts/test.sh` for FUSE changes |
| ws-ckpt | `src/ws-ckpt/` | Linux only | `ws-ckpt` | `ckpt` | `make build`, `make test` |
| ktuner | `src/ktuner/` | Linux only | n/a | `ktuner` | `cargo fmt --all --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` |

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
  `agent-sec-core`. `os-skills` is primarily static Markdown and shell data;
  it does not own the repository Python environment.
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

`scripts/build-all.sh` is a Linux convenience entry point. Select the maintained
components you need with repeated `--component` options: `skills`, `sec-core`,
`tokenless`, `ws-ckpt`, `memory`, `cosh-ng`, and `sight`.

The script does not build `anolisa`, `skillfs`, or `ktuner`. Use their
component gates in the matrix above.

With no install-mode flag, each component's install rules select paths within
the user's home directory without elevated installation privileges. Dependency bootstrap
may still request `sudo` for system packages. System installation uses
`--system` or `--usr` and may require `sudo`. For development, prefer a
targeted build with installation disabled.

```bash
./scripts/build-all.sh --help
./scripts/build-all.sh --no-install --component cosh-ng
./scripts/build-all.sh --no-install --component sec-core
./scripts/build-all.sh --no-install --component sec-core --component tokenless
```

The main options are:

| Option | Effect |
| --- | --- |
| `--no-install` | Install dependencies and build, then skip installation. |
| `--install-mode <mode>` | Select `user` or `system`; `user` is the default. |
| `--usr`, `--system` | Select system installation mode. |
| `--ignore-deps` | Skip dependency setup and runtime verification; use only on a pre-provisioned host. |
| `--deps-only` | Install dependencies without building. |
| `--uninstall` | Remove installed files; combine with `--component` to target a component. |
| `--dry-run` | Print actions without changing files or systemd state. |
| `--interactive`, `--non-interactive` | Open the guided flow or explicitly select automation mode. |
| `--component <name>` | Build or uninstall one supported component; repeat for multiple components. |

Use explicit component selections for development. See
`./scripts/build-all.sh --help` for the complete script interface.

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
| Installation method | Shared installation/quickstart pages as applicable, the component manual, and its README. |
| Architecture or protocol | `src/<component>/docs/design/`. |
| New component | Root README and `NOTICE` when applicable. |

Daily feature and fix PRs update README and user-guide pages. Release version
bump PRs aggregate user-perceivable entries into `CHANGELOG*.md`.

## License

By contributing, you agree that your contributions are licensed under the
Apache License 2.0.
