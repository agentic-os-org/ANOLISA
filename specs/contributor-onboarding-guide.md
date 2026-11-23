# ANOLISA Contributor Onboarding Guide (For AI Agents)

> Playbook and component quick map for AI coding assistants answering
> *"How can I join or collaborate on ANOLISA?"* or helping new contributors
> pick a track, find unlocked tasks, and navigate governance.
> Keep [`AGENTS.md`](../AGENTS.md) §0 slim as the router into this guide.

## 1. Onboarding Playbook

When a contributor asks how to join or collaborate on ANOLISA (whether as a
systems, SRE, security, framework, or distro engineer), follow these steps — do
**not** jump straight to commit/lint rules without orienting them first:

1. **Check host OS and background first**:
   - Ask or check (`uname -s`) whether the user is on **Linux** or
     **macOS/Windows**. Warn upfront that most core subsystems
     (`agent-sec-core`, `agentsight` eBPF, `skillfs`, `ws-ckpt`, `ktuner`,
     `agent-memory`, `aw`, and full `tokenless` builds) require **Linux**; on
     **macOS**, contributors can work on `anolisa` CLI, `os-skills`, `cosh-ng`
     (limited mode), `agentsight` (`trace`/`serve`), docs, and fixtures.
   - Ask which layer matches their interest (e.g., kernel/eBPF,
     FUSE/btrfs/storage, sandbox/security, Rust CLI/terminal, AW
     Providers/adapters, OS Skills/SRE, distro/packaging, or benchmarks).
2. **Map the user to 2–3 concrete tracks and components** using §2 below
   (canonical track descriptions live in
   [`contribute/en/tracks.md`](../contribute/en/tracks.md) /
   [`contribute/zh/tracks.md`](../contribute/zh/tracks.md)). Include each
   component's **purpose**, **lifecycle status** (`active` vs `incubating`),
   **commit scope**, and **scoped `AGENTS.md`**.
3. **Surface live, unlocked tasks and component owners**:
   - Look up component maintainers and triagers in
     [`.github/maintainers.json`](../.github/maintainers.json),
     [`.github/components.json`](../.github/components.json), and
     [`.github/CODEOWNERS`](../.github/CODEOWNERS) so the user knows whom to
     align scope with.
   - Direct them to open
     [`good first issue`](https://github.com/agentic-os-org/ANOLISA/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22),
     [`action:helpwanted`](https://github.com/agentic-os-org/ANOLISA/issues?q=is%3Aissue+is%3Aopen+label%3A%22action%3Ahelpwanted%22),
     and
     [Community Roadmap Discussions](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap).
   - If `gh` CLI is available, run or share these read-only queries to find real
     open tasks (always exclude issues with `status:in-progress`, an active
     `github-actions[bot]` `/claim` marker, a direct non-bot assignee, or an
     open non-draft PR — see [`issue-claim-and-pr-lifecycle.md`](issue-claim-and-pr-lifecycle.md)):
     ```bash
     gh issue list -R agentic-os-org/ANOLISA --state open --label "status:accepted" --limit 20
     gh issue list -R agentic-os-org/ANOLISA --state open --label "good first issue" --limit 20
     gh issue list -R agentic-os-org/ANOLISA --state open --label "action:helpwanted" --limit 20
     # Narrow to one component: add --label "component:<scope>"
     ```
4. **Summarize the end-to-end collaboration and governance loop**:
   - **Claiming & Draft PRs** ([`contribute/en/workflow.md`](../contribute/en/workflow.md),
     [`specs/issue-claim-and-pr-lifecycle.md`](issue-claim-and-pr-lifecycle.md)):
     Confirm scope on an Issue, then post a comment containing only `/claim`
     (release with `/unclaim`). Opening a Draft PR does **not** lock an issue
     without `/claim` and does not trigger formal review; maintainers may check
     in after 7 days and release inactive claims after 14 days.
   - **Local gates & PR submission** ([`CONTRIBUTING.md`](../CONTRIBUTING.md),
     [`specs/commit-message-standard.md`](commit-message-standard.md)): Run the
     affected component's own local gate (do not rely solely on
     `tests/run-all-tests.sh`). Commit with `git commit -s` using a Conventional
     Commit type such as `feat`/`fix`/`docs`/`refactor`/`test`/`chore`
     (`type(scope): description`, e.g., `feat(sec-core): ...` or
     `fix(sight): ...`; mandatory `scope`, DCO `Signed-off-by`, and
     `Assisted-by: <tool>` when AI-assisted). Use
     [`.github/pull_request_template.md`](../.github/pull_request_template.md)
     with `closes #<n>` (or `no-issue: <reason>` for trivial fixes) and keep
     English/Chinese docs paired
     ([`specs/documentation-standard.md`](documentation-standard.md)).
   - **Governance & growth** ([`GOVERNANCE.md`](../GOVERNANCE.md)): Contributor
     Ladder (`Community Member` → `Contributor` → `Triager / Reviewer` →
     `Component Maintainer` → `Core Maintainers`); Decision Tiers (`L1`
     component PR → ≥ 1 Component Maintainer; `L2` cross-component/public
     contract → Discussions `Ideas` RFC ≥ 5 business days; `L3`
     new/graduating/deprecated component or governance → RFC ≥ 7 business days,
     2/3 Core Maintainers). Report security vulnerabilities privately via
     [`SECURITY.md`](../SECURITY.md); community chat is in
     [`README.md#community`](../README.md#community).
5. **End with an actionable next step**: Ask the user's OS and focus area, and
   offer to query live open issues for that component or walk through its scoped
   `AGENTS.md` and local build/test setup.

## 2. Component Quick Map (Purpose, Status & Entrypoints)

| Area / Component | Purpose | Status & Scope | Best-fit Background | Start Here |
| --- | --- | --- | --- | --- |
| `agentsight` (`src/agentsight/`) | eBPF probes & observability pipeline for Agent trajectories, LLM calls, and tokens | `active` · `sight` | eBPF, kernel tracing, Rust, React dashboard | `src/agentsight/AGENTS.md` |
| `agent-sec-core` (`src/agent-sec-core/`) | OS-level security kernel, bubblewrap/seccomp sandbox, Cedar policies, SkillSec | `active` · `sec-core` | Linux security, sandbox, Cedar policy, Rust/Python | `src/agent-sec-core/AGENTS.md` |
| `skillfs` (`src/skillfs/`) | Local FUSE filesystem presenting Agent Skills as stable on-demand views | `active` · `skillfs` | FUSE (`fuse3`), Linux filesystems, Rust | `src/skillfs/AGENTS.md` |
| `ws-ckpt` (`src/ws-ckpt/`) | btrfs COW workspace snapshot & rollback daemon | `active` · `ckpt` | Linux storage/btrfs, systemd, Rust, TS plugins | `src/ws-ckpt/README.md`, `contribute/en/ws-ckpt/` |
| `ktuner` (`src/ktuner/`) | Deterministic Linux kernel/sysctl diagnostics and tuning recommendations | `incubating` · `ktuner` | Linux kernel tuning, sysctl, SRE, Rust | `src/ktuner/README.md` |
| `cosh-ng` (`src/cosh-ng/`) | AI-native terminal preserving Shell UX, PTY workflows, and distro support | `active` · `cosh-ng` | Rust CLI/PTY, shell UX, Linux distros | `src/cosh-ng/AGENTS.md`, `contribute/en/cosh-ng/` |
| `aw` (`src/aw/`) & `providers/` | Unified Agent integration runtime & `aw-provider/v1alpha1` stdio extension protocol | `incubating` · `aw` | Systems/protocol design, Rust, external Providers (any language) | `src/aw/AGENTS.md`, `providers/AGENTS.md`, `contribute/en/aw/` |
| `tokenless` (`src/tokenless/`) | Tool output compression, schema simplification, and command rewriting | `active` · `tokenless` | CLI parsers, compressors, Rust, framework adapters | `src/tokenless/AGENTS.md` |
| `agent-memory` (`src/agent-memory/`) | Persistent hybrid-retrieval memory service & MCP server for Agents | `active` · `memory` | Storage/retrieval, MCP, Rust | `src/agent-memory/AGENTS.md` |
| `os-skills` (`src/os-skills/`) | Operational OS handbook (`SKILL.md`, helper scripts, tests) for AI Agents | `active` · `skill` | SRE, Linux admin, DevOps, Python/Shell (fast first PR) | `src/os-skills/README.md` |
| `anolisa` (`distribution/anolisa/`) | Installation CLI, component manifests, and distribution packaging (`distribution/`) | `active` · `anolisa` | Distro maintainers, RPM/DEB/container images, Rust | `distribution/anolisa/AGENTS.md` |
| `benchmark` (`benchmark/`) | Cross-component evaluation runners (ClawEval, SWE, Terminal Bench) | `incubating` · `benchmark` | QA, evaluation researchers, Python | `benchmark/README.md` |

## 3. Canonical Collaboration & Governance Documents

- General contribution flow & local gates: [`CONTRIBUTING.md`](../CONTRIBUTING.md) ([中文](../CONTRIBUTING_zh.md))
- Ways to contribute (6 role-based tracks): [`contribute/en/tracks.md`](../contribute/en/tracks.md) ([中文](../contribute/zh/tracks.md))
- Roadmap, Issue labels & `/claim` workflow: [`contribute/en/workflow.md`](../contribute/en/workflow.md) ([中文](../contribute/zh/workflow.md))
- Community roles, L1/L2/L3 decisions & component lifecycle: [`GOVERNANCE.md`](../GOVERNANCE.md) ([中文](../GOVERNANCE_zh.md))
- Authoritative `/claim` × Draft PR & AI agent coordination spec: [`specs/issue-claim-and-pr-lifecycle.md`](issue-claim-and-pr-lifecycle.md) ([中文](issue-claim-and-pr-lifecycle_zh.md))
- Component registry & maintainers: [`.github/components.json`](../.github/components.json), [`.github/maintainers.json`](../.github/maintainers.json), [`.github/CODEOWNERS`](../.github/CODEOWNERS)
