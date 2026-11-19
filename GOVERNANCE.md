# ANOLISA Community Governance

[中文版](GOVERNANCE_zh.md)

This document defines the community roles, collaboration and decision-making processes, and component lifecycle rules for ANOLISA. ANOLISA develops an open, vendor-neutral system software stack for AI Agent workloads and welcomes individual developers, open-source communities, OS distributions, and organizations.

## 1. Governance Principles

1. **Open and vendor-neutral** — Technical decisions are made in public based on engineering evidence and user value, keeping ANOLISA compatible across vendors and Agent frameworks.
2. **Meritocracy** — Community responsibilities and review authority are earned through sustained, high-quality public contributions across code, documentation, OS Skills, test fixtures, issue triage, and reviews.
3. **Clear boundaries** — Components operate autonomously within their own scope; cross-component contracts and shared protocols evolve through public proposals.
4. **Security and quality baseline** — All participation follows [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md), [SECURITY.md](SECURITY.md), and the repository engineering standards.

## 2. Community Roles and Contributor Ladder

ANOLISA's roles align with the repository's ownership metadata in [`.github/components.json`](.github/components.json), [`.github/maintainers.json`](.github/maintainers.json), and [`.github/CODEOWNERS`](.github/CODEOWNERS).

| Role | Responsibilities & Permissions | How to Reach This Role |
| :--- | :--- | :--- |
| **Community Member** | Use ANOLISA, participate in GitHub Discussions, report issues, reproduce bugs, and share real-world cases or test fixtures. | Open to anyone participating in public community channels. |
| **Contributor** | Submit pull requests for code, documentation, OS Skills, compressors, AW Providers/Adapters, distribution packaging, or benchmarks; claim accepted issues via `/claim`. | Have at least one PR merged or make recognized substantive non-code contributions. |
| **Triager / Reviewer** | Assist with issue classification and status transitions (tracked in `issue_triagers`), perform initial PR reviews, verify reproductions, and guide newcomers. | Demonstrate sustained, constructive issue triage, PR reviews, or multiple quality contributions in a component; nominated by a Component Maintainer and approved by two Maintainers. |
| **Component Maintainer** | Own a component's roadmap, final PR approval and merge, releases, and community task curation (recorded in `.github/maintainers.json` and `.github/CODEOWNERS`). | Build a sustained track record of deep contributions and reviews in the component and understand its contracts and release gates; nominated by an existing Maintainer and approved by consensus of the component's Maintainers and Core Maintainers. |
| **Core Maintainers** | Guide project-wide direction, cross-component contracts and shared protocols (such as AW), security coordination, component onboarding/graduation/deprecation, and cross-component dispute resolution. | Composed of representatives across active components and long-term cross-component maintainers; additions require majority approval from existing Core Maintainers. |

### Activity and Emeritus Status

- Any Triager, Reviewer, or Maintainer who needs to step back may request **Emeritus** status at any time, retaining recognition while being removed from active review queues.
- If a Maintainer has no public community activity for **6 months**, Core Maintainers may transition them to Emeritus status after reaching out. Emeritus maintainers may return to active status upon request and brief confirmation.

## 3. Collaboration Flow and Agent-Assisted Operations

ANOLISA combines public human governance with Agent-assisted automation so maintainers can scale triage and review responsibly:

1. **Direction Discovery** — Component maintainers publish candidate directions in GitHub Discussions under [Community Roadmap](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap).
2. **Issue Intake & Triage** — Automated routing and Agent-assisted issue analysis help classify incoming reports, check completeness, and surface component context; human maintainers confirm scope and apply `status:accepted`, `good first issue`, or `action:helpwanted`.
3. **Claiming & Progress** — Contributors claim eligible issues by commenting `/claim` and release them with `/unclaim` (see [Issues, Roadmap & Claiming](contribute/en/workflow.md)). Automated checks and Agent assistants may post gentle progress reminders on inactive claims.
4. **Review & Merge** — Pull requests pass CI gates and `CODEOWNERS` review. Automated AI reviewers help surface potential issues, while human maintainers remain accountable for technical judgment and final merge decisions.

### Service Expectations

- **Initial response**: Maintainers aim to provide an initial response (assisted by automated triage where applicable) to new issues and PRs within **5 business days**.
- **Inactive claims**: If a claimed issue has no visible progress or reply for **14 days**, automated reminders may notify the claimant and a maintainer may politely release the claim so others can pick it up.

## 4. Decision-Making Process

ANOLISA uses tiered decision-making and favors **Lazy Consensus** (a public proposal passes if no substantive objection is raised during the review window):

| Tier | Scope | Process & Review Window | Approvers |
| :--- | :--- | :--- | :--- |
| **L1 · Component-local changes** | Bug fixes, localized enhancements, documentation updates, component tests, and refactoring | Issue + PR | Component Maintainer (at least 1 approval) |
| **L2 · Cross-component & public contracts** | Public CLI/config contract changes, AW Provider/Adapter protocol evolution, or multi-component breaking changes | Lightweight RFC via GitHub Discussions (`Ideas`) or design doc PR; minimum **5 business days** public comment window | Consensus among Maintainers of all affected components |
| **L3 · Project-wide & governance** | Adding a new component, graduating or deprecating a component, amending this governance document, or changing licensing/security policy | Public RFC Discussion; minimum **7 business days** public comment window | Two-thirds majority of Core Maintainers |

When consensus cannot be reached at L1 or L2, the issue may be escalated to Core Maintainers for mediation or a formal vote.

## 5. Component and Ecosystem Lifecycle

ANOLISA separates **in-repo core components** (registered in [`.github/components.json`](.github/components.json)) from **independent ecosystem extensions** (such as out-of-tree AW Providers, external Agent Adapters, and community image catalog entries):

- **Independent extensions**: Developers can build and distribute AW Providers (`aw-provider/v1alpha1`) or external adapters independently in their own repositories. Submissions to [`providers/`](providers/README.md) or the [images catalog](docs/user-guide/en/getting-started/images.md) are reviewed by the relevant maintainers against contract tests and documentation checks.
- **In-repo component lifecycle** (aligned with `status` in `.github/components.json`):
  1. **`incubating`**: Approved via L3 discussion and satisfies [Component Onboarding Standard](specs/component-onboarding.md) (compilable skeleton, baseline tests, CI registration, clear positioning, and at least one owner).
  2. **`active`**: Provides bilingual user/developer documentation, verified release or Preview artifacts, automated test coverage, and an active maintainer roster; promoted by Core Maintainers review.
  3. **`deprecated`**: Superseded by architectural evolution or left without maintainers; moved to `deprecated/` with an advance migration notice and removed from default builds and active website navigation.

## 6. Legal, Code of Conduct, and Amendments

- **License & DCO**: All contributions are licensed under the [Apache License 2.0](LICENSE). Every commit must include a `Signed-off-by` trailer (Developer Certificate of Origin) using the contributor's own Git identity.
- **Code of Conduct**: All community spaces (GitHub, DingTalk groups, and events) are governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
- **Security**: Do not disclose security vulnerabilities in public issues. Report them privately following [SECURITY.md](SECURITY.md).
- **Amendments**: Changes to this governance document follow the L3 decision process and must keep the English and Chinese versions semantically synchronized.
