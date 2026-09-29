# Component Onboarding Standard

> Canonical checklist and rules for introducing a new component into the ANOLISA monorepo.
> Both human contributors and AI agents MUST follow this specification when adding a new component.

`src/<name>/` is the default location for runtime components. Distribution tooling
lives under `distribution/`; retained deprecated components live under
`deprecated/`. In the examples below, substitute the component's actual registered
path from `.github/components.json`. Moving a component does not change its scope,
package name, release tag, or required CI and ownership registrations.

## Agent Navigation

When creating a new component, the reading priority is:

1. `AGENTS.md` — natural entry; contains redirect to this spec for component introduction work
2. `specs/component-onboarding.md` — this file (normative onboarding rules)
3. `specs/documentation-standard.md` — file naming, bilingual, CHANGELOG format
4. `src/<component>/AGENTS.md` — scoped module rules (create if applicable)

---

## 1. Pre-conditions

Before code enters `src/`, the following decisions MUST be finalized:

| Decision | Example | Used in |
|----------|---------|---------|
| **Positioning statement** — one sentence + 2–4 expanding sentences | "Per-host sandbox daemon that manages sandbox instance lifecycles via HTTP API." | README §4.4 opening, root README table, AGENTS.md §1 |
| **Scope name** — short identifier for commits, branches, and CI | `blaze`, `ktuner` | commitlint, prelint, ci.yaml, AGENTS.md §6 |
| **Tech stack** | Rust / Python / TypeScript / Shell | AGENTS.md §1 platform column |
| **Target platform** | Linux only / Linux + macOS / All | AGENTS.md §1, CI runner selection |
| **Component form** | daemon / CLI tool / library / skill | Determines conditional deliverables (§3) |
| **License** | Apache-2.0 (default) | Cargo.toml / package.json `license` field |

If any item is undecided, the scaffold PR MUST NOT be opened.

---

## 2. Mandatory Deliverables

The scaffold PR (first PR introducing the component) MUST include ALL of the following. Missing items block merge.

### 2.1 Source Code

| Deliverable | Path | Rule |
|-------------|------|------|
| Compilable skeleton | `src/<name>/` | At minimum: builds without error on target platform, has one unit test |
| Build manifest | `src/<name>/Cargo.toml` or `package.json` | `license = "Apache-2.0"` present |
| README | `src/<name>/README.md` | Follows `specs/documentation-standard.md` §4.4 (one-line positioning + expanded paragraph) |
| CHANGELOG | `src/<name>/CHANGELOG.md` | `[Unreleased]` stub; follows Keep a Changelog format |

### 2.2 CI & Lint Registration

| Deliverable | File | What to add |
|-------------|------|-------------|
| Commit scope | `.github/commitlint.config.json` | Add scope name to `scope-enum` array |
| PR title scope | `.github/workflows/prelint.yml` | Add scope to **both** `validScopes` arrays (title check + branch check) |
| CI test job | `.github/workflows/ci.yaml` | Add `workflow_dispatch` input, `detect-changes` output + path filter, and a dedicated test job |

### 2.3 Root Documentation Registration

| Deliverable | File | Section |
|-------------|------|---------|
| Component table row | `README.md` | Component overview table |
| Component table row | `AGENTS.md` | §1 Project Overview table |
| Dev commands | `AGENTS.md` | §2 Development Commands — add per-component block |
| Scope inference | `AGENTS.md` | §6 Scope Inference table — add `src/<name>/` → `<scope>` mapping |

### 2.4 Atomicity Rule

All mandatory deliverables MUST land in a **single PR** (may be multiple commits within that PR). Do NOT split registration across follow-up PRs — reviewers cannot verify completeness otherwise.

### 2.5 Quality Gates (must pass before merge)

The scaffold PR must demonstrate that the component passes all applicable quality gates **locally** before requesting review:

| Gate | Rust | Python | TypeScript |
|------|------|--------|------------|
| Format check | `cargo fmt --all -- --check` | `ruff format --check` | `prettier --check` |
| Lint | `cargo clippy --workspace --all-targets -- -D warnings` | `ruff check` | `eslint` |
| Tests | `cargo test --workspace` | `pytest` | `make test` |

**Additional quality invariants:**

- **Version coherence**: version in `Cargo.toml` (or `package.json`), component contract (`.anolisa/component.toml` if present), and CHANGELOG latest **released** entry MUST be identical. This check applies at **release/version-bump PRs**, not at scaffold time (scaffold PRs only have `[Unreleased]`).
- **Example config parsability**: every file under `examples/` MUST be parsable by the language's standard parser (e.g. `tomllib` for TOML). Add a unit test that round-trips or deserializes each example config.
- **Doc–code name alignment**: state names, enum variants, API field names, and CLI subcommand names referenced in README / AGENTS.md MUST exactly match the source code identifiers. Review diffs against `grep` on the enum/struct definitions.
- **Quick Start runnability**: the README quick-start section MUST work in a source-checkout scenario (not only after system install). If paths like `/etc/...` are required, provide a dev-mode alternative or explicit setup step.
- **TODO discipline**: unimplemented behaviors acknowledged during review MUST be annotated in code as `TODO(<target-version>): <description>` (e.g. `TODO(v0.2): implement drain + kill`).

---

## 3. Conditional Deliverables

Include these based on the component's form and characteristics:

| Condition | Deliverable | Path | Trigger |
|-----------|-------------|------|---------|
| Rust component | Add to §3 conventions scope list | `AGENTS.md` §3 opening line | Tech stack = Rust |
| Complex architecture (multi-crate, non-trivial constraints) | Scoped AGENTS.md + register in §11 | `src/<name>/AGENTS.md` + `AGENTS.md` §11 table | More than 1 crate, or has design constraints that differ from global conventions |
| User-facing CLI or interactive tool | User-guide documentation (en + zh) + link from index | `docs/user-guide/{en,zh}/user-entrypoint/<name>.md` | Component has CLI subcommands or user-visible behavior |
| Agent-callable (can be invoked by cosh/agent directly) | Skill registration | `src/os-skills/<domain>/<name>/SKILL.md` | Tool is designed for agent autonomous invocation |
| Daemon / needs packaging | Component contract | `.anolisa/component.toml` | Has systemd service, RPM spec, or runtime directories. See `distribution/anolisa/docs/COMPONENT_CONTRACT.md` for schema |
| Pinned toolchain | rust-toolchain.toml | `src/<name>/rust-toolchain.toml` | Requires a Rust version different from repo default |
| Runtime configuration | Example configs | `src/<name>/examples/` | Has TOML/JSON/YAML config that users must understand |
| Bilingual pair (Chinese) | `README_zh.md` + `CHANGELOG_zh.md` | `src/<name>/` | Required before first release per documentation-standard §1; may be deferred if contributor is non-Chinese speaker |

### 3.1 Decision Guide

```
Is it a Rust component?
├── Yes → Add to AGENTS.md §3 scope list
│         Does it have >1 crate or non-trivial architecture constraints?
│         ├── Yes → Create scoped AGENTS.md, register in §11
│         └── No  → Skip scoped AGENTS.md
└── No  → Skip §3

Does it expose CLI commands or user-visible behavior?
├── Yes → Create user-guide docs (en + zh)
│         Is it designed for agent autonomous invocation?
│         ├── Yes → Also create SKILL.md in os-skills/
│         └── No  → Skip SKILL.md
└── No  → Skip user-guide and SKILL.md

Does it run as a daemon or need system packaging?
├── Yes → Create `.anolisa/component.toml`, add examples/ for config
└── No  → Skip component contract
```

---

## 4. Registration Checklist

Copy into your PR description when introducing a new component:

```markdown
## New Component Checklist

**Component**: `<name>` | **Scope**: `<scope>` | **Tech**: Rust/Python/TS | **Platform**: Linux only / All

### Mandatory (all required for merge)
- [ ] `src/<name>/` compiles on target platform with at least one test
- [ ] `src/<name>/README.md` — §4.4 opening paragraph
- [ ] `src/<name>/CHANGELOG.md` — [Unreleased] stub
- [ ] `.github/commitlint.config.json` — scope added to scope-enum
- [ ] `.github/workflows/prelint.yml` — scope added to both validScopes arrays
- [ ] `.github/workflows/ci.yaml` — workflow_dispatch input + detect-changes + test job
- [ ] `README.md` — component table row added
- [ ] `AGENTS.md` §1 — component table row added
- [ ] `AGENTS.md` §2 — dev commands section added
- [ ] `AGENTS.md` §6 — scope inference table row added
- [ ] License field: `license = "Apache-2.0"` in Cargo.toml / package.json
- [ ] Quality gates: fmt + lint + test pass locally
- [ ] Version coherence: Cargo.toml / contract / CHANGELOG versions match (release PRs only)
- [ ] README quick-start works in source-checkout scenario

### Conditional (check applicable items)
- [ ] Rust: listed in AGENTS.md §3 conventions scope
- [ ] Complex arch: scoped `src/<name>/AGENTS.md` + AGENTS.md §11 entry
- [ ] User-facing CLI: `docs/user-guide/{en,zh}/` docs + index link
- [ ] Agent-callable: `src/os-skills/<domain>/<name>/SKILL.md`
- [ ] Daemon/packaged: `.anolisa/component.toml`
- [ ] Pinned toolchain: `src/<name>/rust-toolchain.toml`
- [ ] Has config: `src/<name>/examples/` with annotated samples + parse test
- [ ] Bilingual: `README_zh.md` + `CHANGELOG_zh.md` (before first release)
- [ ] Doc names match code: enum/struct/CLI names identical in README and source
```

---

## 5. Verification

### 5.1 Manual Verification (reviewer checklist)

Before approving a new-component PR, verify:

1. `grep -c '<scope>' .github/commitlint.config.json` returns 1
2. `grep -c '<scope>' .github/workflows/prelint.yml` returns ≥ 2
3. `grep -c 'src/<name>/' .github/workflows/ci.yaml` returns ≥ 2 (detect-changes + test job)
4. Component appears in both `README.md` and `AGENTS.md` §1 tables
5. `src/<name>/Cargo.toml` (or equivalent) contains `license = "Apache-2.0"`
6. `src/<name>/CHANGELOG.md` exists and has `[Unreleased]` header
7. `src/<name>/README_zh.md` and `CHANGELOG_zh.md` exist (or tracked as follow-up for non-Chinese contributors)

### 5.2 Future Automation (CI check, not yet implemented)

Planned invariants for a `check-component-registry` CI job:

- Every registered component root in `src/`, `distribution/`, or `deprecated/` (excluding `src/os-skills/`) has README.md + CHANGELOG.md
- The set of scopes in `commitlint.config.json` is a superset of component scopes in `AGENTS.md` §6
- The set of scopes in `prelint.yml` includes all scopes from `commitlint.config.json`
- `ci.yaml` has a detect-changes path filter for every `src/<name>/` listed in AGENTS.md §1

---

## 6. Anti-patterns

| Anti-pattern | Rule |
|--------------|------|
| Split registration across multiple PRs | §2.4: all mandatory items in one PR |
| Forget prelint scope | §2.2: prelint is mandatory |
| Forget AGENTS.md registration | §2.3: root doc registration is mandatory |
| License mismatch | §1: license decision is a pre-condition |
| No CHANGELOG until late | §2.1: CHANGELOG stub is day-one deliverable |
| Version number divergence | §2.5: version coherence |
| Example configs unparsable | §2.5: example config parsability |
| Doc names != code identifiers | §2.5: doc–code name alignment |
| Quick Start requires system paths | §2.5: quick start runnability |
| CI not wired in scaffold PR | §2.2 + §2.4: CI in same PR |
| Lint failures shipped | §2.5: lint gate must pass locally |

---

*Last updated: 2026-07-05*
