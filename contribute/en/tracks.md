# Ways to Contribute

[中文版](../zh/tracks.md)

ANOLISA spans multiple layers, from operational skills and framework adapters to system daemons and kernel probes. Each area can be worked on independently—pick a track that fits your background and local environment.

Once you pick a track, follow [Issues, Roadmap & Claiming](workflow.md) to get started.

## 1. Feedback, Test Fixtures, and Benchmarks

*Best for: early users, Agent power users, QA engineers, and evaluation researchers (any OS).*

- **Structured bug reports & reproductions**: report reproducible failures or platform gaps using the repository Issue Forms.
- **Anonymized real-world fixtures**: contribute sanitized build logs (Gradle, Maven, cargo, kubectl, etc.) for `tokenless` compression tests, or false-positive/false-negative cases for `agent-sec-core` and `agentsight`.
- **Documentation & walkthrough verification**: verify quickstarts and guides on your environment and fix outdated steps or bilingual mismatches.
- **Benchmarks**: expand evaluation suites and reproducibility scripts under [`benchmark/`](../../benchmark/README.md).

## 2. OS Skills, Kernel Rules, and Security Policies

*Best for: SREs, Linux administrators, and security engineers (OS Skills tests run on Linux and macOS).*

- **OS Skills**: add or improve operational skills (`SKILL.md`, deterministic helper scripts, and regression tests) in [`src/os-skills/`](../../src/os-skills/README.md).
- **Kernel tuning rules**: contribute deterministic sysctl/kernel parameter diagnostics and tuning rationales to `src/ktuner/`.
- **Security rules & policies**: contribute Cedar policy templates, static code patterns, or prompt-safety test cases to `src/agent-sec-core/`.

## 3. Compressors and Independent Extensions (AW Providers)

*Best for: AI infrastructure developers and systems/CLI tool authors.*

- **Tokenless compressors**: add self-contained output compressors for specific CLI tools or structured formats in `src/tokenless/` — clear input/expected-output fixture contracts that can be verified locally with unit tests.
- **AW Providers**: integrate your own Agent-oriented system software via the `aw-provider/v1alpha1` stdio protocol. Providers can live in [`providers/`](../../providers/README.md) or in an external repository in any programming language.

## 4. Agent Framework Adapters and Plugins

*Best for: Agent framework maintainers and TypeScript / Python / Rust SDK developers.*

- **AW native adapters**: connect additional Agent CLIs, SDKs, or harnesses to AW's unified hook, MCP, and policy lifecycle via `aw-adapter`.
- **Component plugins**: maintain and expand framework plugins and MCP integrations for `tokenless`, `agent-memory`, `ws-ckpt`, and `agent-sec-core`.

## 5. Distributions, Packaging, and Platform Portability

*Best for: Linux distribution maintainers, container/sandbox platform engineers, and macOS/Windows contributors.*

- **Linux distributions**: add distro detection and platform support (see [cosh-ng Adding Distributions](cosh-ng/adding-distros.md)) and improve RPM/DEB/raw packaging.
- **Images & cloud environments**: contribute container base images, preinstalled OS images, or sandbox templates to the [Images and cloud services catalog](../../docs/user-guide/en/getting-started/images.md).
- **Cross-platform subsets**: improve supported macOS/Windows workflows (`anolisa` CLI, `cosh-ng` limited mode, `agentsight` trajectory/Dashboard UI).

## 6. Core Systems and Architecture Evolution

*Best for: Rust systems engineers, kernel/eBPF/FUSE/sandbox specialists, and long-term contributors.*

- **Core daemons & subsystems**: evolve eBPF probes (`agentsight`), FUSE views (`skillfs`), btrfs snapshots (`ws-ckpt`), bubblewrap/seccomp sandboxing and SkillSec (`agent-sec-core`), hybrid memory retrieval (`agent-memory`), PTY terminal workflows (`cosh-ng`, see [Adding Commands](cosh-ng/adding-commands.md)), and the AW runtime (`aw`).
- **Cross-component proposals**: participate in RFC discussions for shared protocols and multi-component contracts (see [Community Governance](../../GOVERNANCE.md)).

## Find Open Opportunities

- **Beginner-friendly tasks**: browse open [`good first issue`](https://github.com/agentic-os-org/ANOLISA/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) and [`action:helpwanted`](https://github.com/agentic-os-org/ANOLISA/issues?q=is%3Aissue+is%3Aopen+label%3A%22action%3Ahelpwanted%22) issues.
- **Component roadmaps**: explore candidate directions in [Community Roadmap Discussions](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap).
- **Build & commit gates**: see the [General Contribution Guide](../../CONTRIBUTING.md) for local verification commands and commit standards.
