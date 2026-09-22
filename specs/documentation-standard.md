# ANOLISA Documentation Standard

> Canonical reference for documentation structure, naming, bilingual conventions, and maintenance rules.
> Both human contributors and AI agents MUST follow this specification.

## Agent Navigation (for documentation tasks)

Agents enter this repository via `AGENTS.md` (auto-loaded by tooling). For **documentation-related work**, `AGENTS.md` §9/§11.1/§12 redirects here with MANDATORY directives. The reading priority for documentation tasks is:

1. `AGENTS.md` — natural entry; contains redirect to this spec
2. `specs/documentation-standard.md` — normative documentation rules (this file)
3. `src/<component>/AGENTS.md` — scoped module rules (if working on that component)
4. `src/<component>/README.md` — component context

**Source of truth hierarchy:**

- Code > README/user-guide > design docs
- Before generating or modifying documentation: read source code, verify CLI examples against `clap`/`argparse` definitions, never document unimplemented features as available

---

## 1. Bilingual Naming Convention

| Scope | English (default) | Chinese | Notes |
|-------|-------------------|---------|-------|
| Repo root / component root | `FILE.md` | `FILE_zh.md` | No suffix = English |
| `docs/` standalone pages | `docs/FILE.md` | `docs/FILE_zh.md` | e.g. QUICKSTART, BUILDING |
| `docs/` long-form guides | `docs/{type}/en/` | `docs/{type}/zh/` | Directory-based separation |

**Cross-reference convention:**

- English file header: `[中文版](FILE_zh.md)`
- Chinese file header: `[English](FILE.md)`

**Prohibited patterns:**

- `*_CN.md` — legacy naming, must migrate to `*_zh.md`
- `*_en.md` — English is the default, no suffix needed

## 2. Repository Root Files

### 2.1 Required Files

| File | Bilingual | Purpose |
|------|-----------|---------|
| `README.md` | Yes (`README_zh.md`) | Project overview, component table, quick start, documentation index |
| `AGENTS.md` | English only | AI agent global constraints and development conventions |
| `LICENSE` | English only | Apache License 2.0 |
| `CONTRIBUTING.md` | Yes (`CONTRIBUTING_zh.md`) | General contribution guide: environment, commit rules, PR flow |
| `CHANGELOG.md` | Yes (`CHANGELOG_zh.md`) | Version changelog with component version composition |
| `CODE_OF_CONDUCT.md` | English only | Contributor Covenant (standard text) |
| `SECURITY.md` | English only | Vulnerability reporting process |
| `NOTICE` | English only | Derived works and dependency attribution |

### 2.2 File Format References

| File | Format standard | Root-specific rule |
|------|----------------|-------------------|
| README | First sentence = one-line positioning; then 2–4 sentences expanding scope | Root adds component table + documentation index |
| CONTRIBUTING | Structured sections: prerequisites, build, test, PR checklist | Root covers general flow; component covers local build/test only |
| CHANGELOG | [Keep a Changelog](https://keepachangelog.com/) format (Added / Changed / Fixed) | Root MUST prepend a "Component Versions" table before highlights |

**CHANGELOG writing rules** (content standard — what to write, not when):

1. Only record user-perceivable changes; skip pure refactors, test infra, CI tweaks
2. Three-part bullet format: `**component**: [Updated to vX.Y.Z | First release vX.Y.Z], [verb-object action], [user-perceivable effect]`
3. User perspective — the third part must describe what the user/Agent can now do, not internal implementation
4. No internal jargon — command names and config keys are fine; kernel APIs and syscalls are not
5. One bullet, one component — do not combine multiple components in one bullet
6. Key entries should reference the implementing PR

**Root CHANGELOG section structure** — each version entry contains (in order):

```markdown
## [X.Y] - YYYY-MM-DD

### Component Versions
| Component | Version |
|-----------|--------|
| ... | ... |

### Highlights
- **component**: Updated to vX.Y.Z, [action], [user effect]

### New Components
- **component**: First release vX.Y.Z, [action], [user effect]

### Updated
- **component**: Updated to vX.Y.Z, [action], [user effect]
```

- `Highlights`: one bullet per component, version-level summary
- `New Components`: components first introduced in this release; use "First release vX.Y.Z"
- `Updated`: existing components with version bumps; use "Updated to vX.Y.Z"
- Sections with no content may be omitted
- Unreleased versions use `## [X.Y] - Unreleased`; replace with actual date at release time

Chinese version (`CHANGELOG_zh.md`) mirrors the same structure with translated section headers (`重点特性` / `新增组件` / `组件更新`).

## 3. docs/ Directory Structure

```
docs/
├── QUICKSTART.md              # Cross-component quick start (English)
├── QUICKSTART_zh.md           # Cross-component quick start (Chinese)
├── BUILDING.md                # Build from source (English)
├── BUILDING_zh.md             # Build from source (Chinese)
├── user-guide/
│   ├── en/                    # User manual (English)
│   │   ├── README.md          # Index page
│   │   ├── installation.md
│   │   └── {capability}/      # Organized by capability domain
│   └── zh/                    # User manual (Chinese, mirrors en/)
│       └── ...
└── developer-guide/
    ├── en/                    # Developer documentation (English)
    │   └── {component}/       # Organized by component name
    └── zh/                    # Developer documentation (Chinese)
        └── ...
```

### 3.1 What Goes Where

| Content type | Location | NOT here |
|-------------|----------|----------|
| Full user-facing how-to / reference | `docs/user-guide/{en,zh}/` | — |
| Developer architecture / IPC / hooks | `docs/developer-guide/{en,zh}/` | Component root |
| Component design docs | `src/<component>/docs/` | `docs/` top level |
| Cross-component quick start | `docs/QUICKSTART.md` | README |
| Build instructions | `docs/BUILDING.md` | README |

> **Note on component README**: README.md serves as a **summary entry point** (positioning + quick-start + basic install/use). It is NOT a full how-to or reference manual. Complete usage documentation belongs in `docs/user-guide/`. When CLI/config changes occur, update both the README summary and the user-guide reference.

### 3.2 Design Documents

Design documents live **only** in the component's own directory:

```
src/<component>/docs/design/    ← component-specific design docs
```

For distribution tooling or retained deprecated components, substitute the source
root registered in `.github/components.json`, such as
`distribution/anolisa/docs/design/`. Component-local documentation stays with its
component when that directory moves.

Design docs are **never** placed at:
- Repository root
- `docs/` top level
- `docs/user-guide/` or `docs/developer-guide/`

### 3.3 Public Website and Agent Endpoints

The Docusaurus source lives in `website/` on `main`. GitHub Pages is a
deployment target, not a hand-maintained documentation source.

The website MUST consume canonical repository content at build time:

- Documentation routes render `docs/QUICKSTART*.md`, `docs/BUILDING*.md`,
  and the paired files under `docs/user-guide/{en,zh}/` and
  `docs/developer-guide/{en,zh}/`.
- Changelog routes render the root and component `CHANGELOG*.md` files.
- `/agents/`, `/agents/repo-index.*`, `/agents/cli-reference.txt`,
  `/agents/changelog.txt`, `/llms.txt`, and `/llms-full.txt` are generated
  from documentation, component manifests, CLI definitions, and changelogs.
- Generated files live under `website/.generated/`; they MUST NOT be edited
  or committed as independent sources.

Homepage copy may summarize the project and direct readers to a capability,
but installation commands, platform support, version numbers, and behavioral
contracts MUST be derived from or link to their canonical repository source.

English remains the default locale and Chinese uses the `/zh/` route prefix.
Both locales MUST expose equivalent navigation for Docs, User Guide,
Developer Guide, Changelog, and the Agent entry point.

Pages builds run automatically for relevant changes merged to `main`.
Pull requests build and validate the artifact without deploying it, and
`workflow_dispatch` remains available for an explicit manual deployment.

## 4. Component-Level Files

### 4.1 Required Files (every component)

| File | Bilingual | Purpose |
|------|-----------|---------|
| `README.md` | Yes (`README_zh.md`) | Entry point: positioning, use cases, install, usage, relationships |
| `CHANGELOG.md` | Yes (`CHANGELOG_zh.md`) | User-perceivable changes per release |

### 4.2 Optional Files

| File | Bilingual | When needed | Rule |
|------|-----------|-------------|------|
| `CONTRIBUTING.md` | **If exists, `CONTRIBUTING_zh.md` is also required** | When component has specific build/test/lint process | Must NOT repeat root-level general rules |
| `AGENTS.md` | English only | When component has non-trivial AI-agent constraints | Specific to module scope |
| `LICENSE` | English only | When component uses a different license than root | e.g. MIT sub-component |
| `docs/` | — | For design documents, internal protocol specs | NOT for user-guide content |

### 4.3 Files NOT Needed at Component Level

These are inherited from the repository root:

- `CODE_OF_CONDUCT.md`
- `SECURITY.md`
- `NOTICE` (maintained centrally at root)

### 4.4 README Opening Paragraph Convention

Every component `README.md` MUST open with:

1. **First sentence**: one-line positioning statement (reusable verbatim in indexes and tables)
2. **Remainder of paragraph**: 2–4 sentences expanding scope, differentiators, and target users

Example:
```markdown
# AgentSight

[中文版](README_zh.md)

eBPF-based observability tool for AI Agents on Linux, providing zero-intrusion
monitoring of LLM API calls, token consumption, and process behavior.
AgentSight captures kernel-level events without modifying agent code...
```

### 4.5 Differentiation from Root Level

| Dimension | Root | Component |
|-----------|------|-----------|
| README | Panoramic — what project, what problem, what's included, how to start | Focused — what this component does, who uses it, how to install & use |
| CONTRIBUTING | General — environment, commit format, PR flow, branch naming | Specific — this component's build/test/lint commands, special deps |
| CHANGELOG | Aggregated — version composition + highlights, links to component CHANGELOGs | Detailed — every user-perceivable entry for this component |

### 4.6 User Guide Writing Standards

**Installation priority** (all component docs MUST follow this order):

1. `anolisa install <component>` — always first (agentsight / agent-sec-core require `sudo` system mode)
2. RPM package (`yum install`) — alternative for Alinux users
3. Source build — developers only, always last

**Content boundaries:**

- Only document components whose source code exists at their registered repository path (`src/`, `distribution/`, or `deprecated/`). No code = no docs.
- Cloud-specific configuration (SLS endpoints, AK/SK auth, security groups) belongs to cloud vendor docs
- Never document planned-but-unimplemented features as available

**Framing:**

- Open with a value proposition ("why install this?"), not architecture description
- Cross-component integration stories belong in user-guide (e.g. "install AgentSight + Tokenless, savings appear in Dashboard")

**Language rules for bilingual docs:**

- `en/` and `zh/` MUST be semantically equivalent
- Technical terms keep English form in Chinese docs (eBPF, Token, CLI)
- Command examples identical across languages; only prose differs

**Entry point convention:** when one page is sufficient, use a flat
`{component}.md` file directly under its capability directory. Create a
component subdirectory only when it contains multiple user documents. Reserve
`QUICKSTART.md` for cross-component journeys or components whose setup needs a
distinct, short onboarding path; do not duplicate a component manual into a
second quickstart.

## 5. PR Documentation Update Rules

When a PR introduces any of the following changes, documentation MUST be updated in the same PR:

| Change type | Required doc update |
|-------------|-------------------|
| New/modified CLI command or flag | Component README (summary) + user-guide (full reference) |
| New/modified config option | Component README (summary) + user-guide (full reference) |
| Installation method change | docs/QUICKSTART + component README |
| Architecture or protocol change | Component `docs/design/` |
| New component added | Root README + NOTICE (if applicable) |

**CHANGELOG update timing**: Daily feature/fix PRs update README and user-guide only; CHANGELOG is written exclusively in **release version bump PRs** that aggregate all user-perceivable changes since the last release.

---

*Last updated: 2026-07-29*
