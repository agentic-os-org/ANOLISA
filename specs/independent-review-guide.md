# Independent Review

A recommended (optional, not a CI gate) checklist for reviewing anolisa code
changes — self-checking your own change before a PR, or reviewing someone
else's. It complements human CODEOWNERS review and CI; it does not replace them.

The root [AGENTS.md](../AGENTS.md) is the mandatory baseline for this
repo; this document is a recommended, deeper self-review methodology layered on
top of it. Where the two conflict, AGENTS.md wins.

## Core discipline

The whole value is in reviewing **without contamination**:

- Do it in a **fresh review pass that does not inherit the implementation
  context** — a separate agent session (if you drive one) or a second reviewer.
  It must not carry the author's intent, suspicions, or fix conclusions.
- **Review-only**: read the code, `git diff`, tests and in-repo docs. Do not edit
  code while reviewing.
- **Zero-direction**: do not enter with "verify that known bug X is fixed". Look
  for problems from scratch. Zero-direction applies to the discovery pass; see
  the two-phase model under Finding discipline below.
- If several reviewers/angles run, **do not cross-feed** one's conclusions into
  another — it biases them.

## When to run how much

- **Simple change → one holistic pass.** Simple = single file / local, no
  interface-contract change, not cross-component, no concurrency or lifecycle, not
  on a hot path, narrow blast radius.
- **Risky change → a full set of review angles.** In anolisa, a change is **never
  "simple"** if it touches any of: another component / crate, a public or FFI
  interface, `src/agentsight/src/bpf/*.bpf.c`, sec-core sandbox/privilege,
  kernel / arch / page-size code, or a semantic change to `*.spec.in` /
  `component.toml` (spec dependencies, `requires_*`, registration); a mechanical
  version bump alone is exempt. A small-but-dangerous eBPF / FFI / sandbox diff
  is not simple.

## Generic review axes (starting skeleton)

architecture · correctness · stability · security · performance · holistic.

Readability is **deprioritized** — clippy / rustfmt / ruff / eslint already gate
style in CI. Only raise what mechanical linters cannot see (naming, dead
abstraction, `mod.rs` layout per AGENTS.md). Apply the language-appropriate
conventions per changed file (Rust / TypeScript / Python / eBPF).

## anolisa-specific review angles (mount by changed path)

These are the classes CI structurally cannot catch. Treat as a checklist keyed
by what the diff touches, not as separate reviewer roles.

1. **eBPF verifier & kernel safety** — regular PR CI does not verifier-load
   `src/agentsight/src/bpf/*.bpf.c`; real-kernel load tests (5.10 / 5.15 / 6.6
   x86 in `agentsight-runner-test.yml`) exist but are not a PR gate, so review
   remains the primary net at PR time. Check: bounded loops; explicit size
   clamps before `bpf_probe_read_*` (an unclamped size trips the verifier with
   "R2 …negative"); no unbounded variable-length access; every producer
   initializes newly added shared-header fields; stack under 512 B; per-CPU vs
   shared-map concurrency. **State whether it was verifier-load-tested on a real
   kernel** — an x86 CI load is not an aarch64 load.

2. **Architecture / environment matrix** — kernel-parameter, `/proc`, `/sys`
   code carries silent arch/kernel assumptions. Flag hardcoded page sizes (page
   size is arch-dependent — use `sysconf(_SC_PAGESIZE)`), kernel-version gates,
   container-vs-host `/proc` reads. Name which (arch × kernel × container) cells
   go untested; recommend a run on the affected arch rather than asserting safety.

3. **FFI / cbindgen ABI boundary** — anolisa crosses Rust ↔ C ↔ Python. Verify
   the generated header == the Rust signature == the example in docs/headers; all
   crossing types are `#[repr(C)]`; no panic unwinds across the FFI boundary
   (`catch_unwind`); the cbindgen drift-guard was actually re-run, not assumed.
   Header/example drift is silent and high-impact; the generic correctness axis
   misses it.

4. **Cross-component contract** — the seams between agentsight, cosh,
   anolisa-cli, and the genai storage schema; SKILL.md auto-discovery;
   `component.toml` fields; CLI/JSON shapes consumed across components. Trigger
   when a diff changes a **producer without its consumer**; ask "does the other
   side assume a stronger contract than this now provides?"

5. **Agent security** — sec-core is a whole component, and agent workloads bring
   specific threats: prompt injection, sandbox escape, deny-list bypass (a
   code-execution deny-list must match on the resolved path, not the spelling),
   command injection in generated shell, privilege escalation on root / sysctl
   writes, secret leakage into logs or PR bodies. Give privileged-write and
   untrusted-input paths a dedicated look.

6. **Packaging / distribution correctness** — `*.spec.in` / `component.toml` /
   manifests are not exercised by code-test CI. Check spec dependencies match
   Cargo/npm dependencies, `requires_*` matches reality, versions are bumped
   consistently, and there is no orphaned or missing registration.

## Verification discipline (before claiming "verified")

Hard-won on this repo's CI:

Baseline gates: see root AGENTS.md §3.5 (pre-commit checks), §6 (commit rules)
and §13 (commit discipline). This section builds on those and adds review-side
methodology they do not cover.

- **Reproduce with the toolchain CI pins**, not just your local default — a lint
  that passes on an older toolchain can fail on the pinned one. Run
  `cargo +<CI-version> fmt --check`, `clippy --all-targets -- -D warnings`, `test`.
- **New logic goes in a lib crate, not a bin** — otherwise incremental
  coverage reads 0 %.
- **rustfmt is a build dependency** (libbpf-cargo's skeleton builder), not only a
  linter.
- **If it can be end-to-end tested, end-to-end test it.** Compiling or unit tests
  alone is not "verified". In the PR body, the testing status should be split
  into done-E2E / unit-only / not-verified rather than a blanket "all passing".
- **Tests must discriminate**: reverting the fix should make the test fail
  (prove it, e.g. by a quick mutation). Cover both directions. Beware
  short-circuits (`mtime` checks, `Ok(false)`) that give false confidence.
- **Extract pure functions** so logic that doesn't need the kernel / root / I/O
  is unit-testable off a privileged box.

## Finding discipline (avoid false findings)

For triaging and replying to findings raised on your own PR (verify before
acting, reply format, common false-positive patterns), see root AGENTS.md §14
(Responding to Review). The items below are reviewer-side methodology §14 does
not cover.

Note the two-phase model: **discovery** is independent and must not carry the
author's intent (see Core discipline), while **verification** deliberately
reads the commit message / design intent to confirm a finding and filter false
positives before reporting. The rules below belong to the verification phase.

- **"The code differs from what I expected" ≠ a bug.** First read `git blame` /
  the commit message / surrounding design intent and answer "why is it this way".
  Findings raised without checking design intent are frequently walked back.
- **Don't trust the PR's abstract terms — verify the real mechanism.** Confirm
  the claimed threat model / dependency / mechanism ("an X injection", "uses
  library Y") actually exists in the code — the real reader/parser may differ
  from the description, and a fix built on the description may not match the real
  implementation. Review to the code, not to the PR's wording.
- **Kernel / security bugs are not discounted by trigger frequency** — a
  use-after-free, uninitialized read, info leak or race is a deterministic
  property of the source; "not seen in a synthetic run" is about frequency, not
  existence. Report it.
- **Withdraw a finding that doesn't hold up on verification** — don't keep it to
  look thorough.
- **"No change needed" is a valid outcome** — if an audit finds nothing, say so
  and close; don't manufacture changes to look busy.

## When reviewing someone else's PR

- **State the problem, don't prescribe the fix**: give the fact, the evidence
  (`file:line`), and the impact — let the author choose how to address it. This
  applies to blocking findings; a non-blocking suggestion may propose a concrete
  approach directly.
- Read the author's design intent before raising a finding (same lesson as above).

## Output contract

- Findings first, ranked by severity, each with `file:line`. If clean, say so
  explicitly and note residual risk. Don't lead with a long summary.
- **Out of scope**: do not re-report what clippy / rustfmt / ruff / eslint /
  commitlint / coverage already gate — that is noise and erodes trust. Focus only
  on what mechanical CI and a single reviewer cannot see.

## Integration loop

Dedupe all findings → fix the real ones first → re-run affected tests → if a fix
materially widens the diff, review again (at least holistic / correctness /
stability / performance) → report honestly: which tests ran, which angles were
reviewed, what was found, what was fixed, what residual risk remains.
