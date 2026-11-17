# AW layout and CI development rules

[中文版](../../zh/development/validation.md)

These are required development rules for the tasks indexed in [AGENTS.md](../../../../AGENTS.md). Read the relevant sections before editing.

The checkout directory name does not select another component's skills or
validation workflow.

Keep workspace/native acceptance under `tests/` and runtime bridge assets under
`crates/aw-adapter/native/`. When changing crate boundaries, update `scripts/check_layout.py`
and its behavior tests alongside the architecture documentation.

Rust, Python and JavaScript (`.js`, `.mjs`, `.cjs`) files share a 699-line ceiling,
including tests and embedded fixtures; the gate reports files at 600 lines.
The existing 711-line `crates/aw-contracts/src/validation.rs` allowance is fixed, not a precedent.
Split by responsibility before crossing the ceiling. Keep related helpers in
named sibling modules, preserve test entrypoints and assertions, and avoid
compressing code or adding size exemptions. Never add `mod.rs` except the
repository-approved `tests/common/mod.rs` convention.

AW/Provider and AW workflow changes run the full AW gate. Tokenless public
CLI/protocol/runtime/statistics source, Provider protocol/executor source, RTK
and package-builder inputs, dependency locks and toolchains also select real
consumer compatibility through scripts/check.py. Ordinary AW runtime, installer,
documentation and shared-crate test changes retain fixture coverage without a
real backend build. Preserve Tokenless's independent component CI. Other
component internals remain in component CI; validate newly consumed contracts explicitly.
Keep daily checks credential-free. The nightly integration workflow builds all
packages once and reuses checksummed candidate-SHA binaries for pinned
native acceptance. Preserve exact version evidence, bounded processes, complete
reports and visible manual boundaries. Missing or skipped suites must fail the
integration result. Prepare private native tool environments; do not weaken
admission to accommodate runner loader overrides or writable shared caches.
Rerun on the exact release SHA before release.

The existing `AW / required` check joins parallel AW and Tokenless jobs after
scope selection. Keep independent build caches, whole-job results and tested
SHAs; a late artifact failure or cancellation must still block the result.
Local entrypoints retain the same checks without requiring Actions orchestration.
Scope selection must use the complete change and preserve candidate/tested SHA
identity. Compile test harnesses once, enumerate them directly and reuse the
build for the workspace run. Do not save CI time by skipping shared security,
Tokenless or lifecycle tests based only on the changed Adapter. Gate changes
need negative behavior tests for omissions, ignored tests and failure paths.

Keep source files, protocol schemas and test fixtures separate from generated
artifacts. Store build/test output under `target/`, bound process lifetimes and
clean up owned child processes, profiles, sockets and temporary state.
