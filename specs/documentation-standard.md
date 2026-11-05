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

Before writing or revising design documents, technical explanations or change
summaries, also read the [change description standard](change-description-standard.md).
It defines reader context and accurate change descriptions alongside this file's
placement, bilingual and maintenance rules.

---

## 1. Bilingual Naming Convention

| Scope | English (default) | Chinese | Notes |
|-------|-------------------|---------|-------|
| Repo root / component root | `FILE.md` | `FILE_zh.md` | No suffix = English |
| `docs/` directory guide | `docs/README.md` | `docs/README_zh.md` | Explains the documentation layout |
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

```text
docs/
├── README.md                  # Documentation layout and placement rationale
├── README_zh.md
├── navigation.yaml            # Website groups, labels, descriptions and ordering
├── user-guide/
│   ├── en/
│   │   ├── index.md           # Manual entry point
│   │   ├── getting-started/
│   │   │   ├── quickstart.md  # One short cross-component first-use journey
│   │   │   ├── installation.md
│   │   │   ├── anolisa-cli.md
│   │   │   ├── building.md
│   │   │   ├── testing.md
│   │   │   └── troubleshooting.md
│   │   ├── aw/
│   │   │   ├── index.md
│   │   │   └── ...
│   │   ├── agent-sec-core/
│   │   │   ├── index.md
│   │   │   └── owasp-agentic-top10.md
│   │   └── {component}/       # Flat component directories, each with index.md
│   └── zh/                    # Mirrors the English manual
└── developer-guide/           # Existing developer docs; separate migration
    ├── en/
    │   └── {component}/
    └── zh/
        └── ...
```

### 3.1 What Goes Where

| Content type | Location | NOT here |
|-------------|----------|----------|
| Full user-facing how-to / reference | `docs/user-guide/{en,zh}/` | — |
| Developer architecture / IPC / hooks | `docs/developer-guide/{en,zh}/` | Component root |
| Archived manuals for deprecated components | `deprecated/<component>/docs/` | Published user/developer guides |
| Component design docs | `src/<component>/docs/` | `docs/` top level |
| Cross-component quick start | `docs/user-guide/{en,zh}/getting-started/quickstart.md` | Component entry pages |
| Build and test instructions | `docs/user-guide/{en,zh}/getting-started/{building,testing}.md` | Duplicate command inventories |

> **Note on component README**: README.md serves as a **summary entry point** (positioning + quick-start + basic install/use). It is NOT a full how-to or reference manual. Complete usage documentation belongs in `docs/user-guide/`. When CLI/config changes occur, update both the README summary and the user-guide reference.

Deprecated component manuals are retained with their source, with the component
README linking to the archive. Keep archived relative links checked, but remove
their entries from current guide indexes and the public website. Copilot Shell's
archive lives in `deprecated/copilot-shell/docs/`.

### 3.2 Design Documents

Design documents live **only** in the component's own directory:

```
src/<component>/docs/design/    ← component-specific design docs
```

A single design may remain directly under this directory. A coherent batch or
multi-stage change may use a dated topic subdirectory with a `README.md` entry
and numbered files, without requiring a topic-specific exception:

```text
src/<component>/docs/design/2026-10-22_cli-refresh/
  README.md
  01-cli-foundation.md
  02-package-management.md
```

Choose this grouping for an actual shared topic. Do not pre-create empty
categories or introduce a cross-component architecture layer for filing alone.

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

- Documentation routes render the paired files under
  `docs/user-guide/{en,zh}/` and `docs/developer-guide/{en,zh}/`.
  `docs/README*.md` explains the source directory and is not a website landing page.
- `docs/navigation.yaml` defines user-manual groups, bilingual labels, fixed
  one-sentence component descriptions, and ordering. Each component item supplies
  non-empty, single-line `description.en` and `description.zh` values for the
  manual index; sidebar entries use labels only. These descriptions summarize
  the component's purpose without release numbers or installation details and
  stay consistent with its documentation.
  The website and generated navigation blocks in the manual indexes consume it.
  Component source directories and canonical URLs stay flat when website groups
  change: `/docs/user-guide/<component>/` is the entry, with chapters below it.
  Shared pages use `/docs/user-guide/getting-started/<page>/`. Previous addresses
  redirect directly to the current URL, retaining locale and incoming fragments;
  navigation group names are not part of canonical component URLs.
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

Deprecated components may also retain their archived user and developer manuals
in this directory, as specified in §3.1.

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

**Entry point convention:** every component has a directory directly under
`docs/user-guide/{en,zh}/`, with `index.md` as its manual entry point, even when
one page is sufficient. Put first-use steps in that index rather than creating
a separate component Quickstart. Keep the project-level `getting-started/quickstart.md`
as a standalone short journey with prerequisites, a first useful result,
verification and next steps. The `getting-started/` directory does not need an
`index.md`; its shared pages can also be linked from contribution documentation.
Website groups belong in `docs/navigation.yaml`, not in source directory names.
The `docs/README*.md` pair describes this layout for documentation maintainers.

## 5. PR Documentation Update Rules

When a PR introduces any of the following changes, documentation MUST be updated in the same PR:

| Change type | Required doc update |
|-------------|-------------------|
| New/modified CLI command or flag | Component README (summary) + user-guide (full reference) |
| New/modified config option | Component README (summary) + user-guide (full reference) |
| Installation method change | Shared installation/quickstart pages as applicable + component index and README |
| Architecture or protocol change | Component `docs/design/` |
| New component added | Root README + NOTICE (if applicable) |

**CHANGELOG update timing**: Daily feature/fix PRs update README and user-guide only; CHANGELOG is written exclusively in **release version bump PRs** that aggregate all user-perceivable changes since the last release.

**Release-note requirement (all release components)**: Every component with a
non-null `release_tag_prefix` in `.github/components.json` MUST include exactly
one non-empty section for the release version in its English `CHANGELOG.md` in
the next and every subsequent version bump PR. Match the version in the release tag, aggregate the
user-visible changes, and include any breaking changes and migration steps.
Keep `CHANGELOG_zh.md` semantically equivalent. The English version section at
the tagged commit is the source of the GitHub Release body.

This requirement also applies to first releases, including ktuner. Before
pushing the first version tag, the version bump PR MUST finalize the applicable
`Unreleased` content into the matching version section. `Unreleased` alone does
not satisfy the requirement. A missing CHANGELOG, missing or duplicate matching
section, or empty section stops publication before the GitHub Release is
created; the workflow does not fall back to commit subjects or another version.

---

*Last updated: 2026-10-23*
