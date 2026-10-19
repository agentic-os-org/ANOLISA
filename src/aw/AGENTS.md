# AW development contract

Read the repository AGENTS.md and [CONTRIBUTING.md](CONTRIBUTING.md) first.
AW is a Linux component. The checkout directory name does not select another
component's skills or validation workflow.

## Integrated user experience

The preferred entrypoint is `aw run --config aw.yaml --agent <target>`.
One configuration composes native Agent targets, security, Tokenless, optional
observation and user Hooks. Keep validation, launch, inspection and cleanup
coherent with that entrypoint. New integrations should extend these contracts
instead of adding a parallel launcher or requiring users to assemble internal
service RPCs and callback commands.

Frameworks retain their model configuration, UI, scheduling, permissions and
session lifecycle. AW owns configuration admission, bounded Provider invocation,
event correlation and metadata audit. An Adapter translates native callbacks
and adopts supported effects; a Provider implements policy through its protocol.
Do not put framework-specific code into the service, Core or Provider Host.
Optional panel views must not participate in tool admission or execution.

Security denial and native permission denial retain precedence. Tokenless
PreTool suggestions block the original call and require a fresh call through
security and native authorization. They must not rewrite arguments in place or
emit approval. PostTool compression changes only an authorized text slot at the
final replacement step; preserve the native envelope, failure status and audit.
Keep unsupported capabilities visible at admission. Configuration vocabulary
and roadmap entries are not evidence of native support.

## Adding an Agent or Hook

Update these together in the same change:

1. Register the Adapter and its event/effect profile in
   `crates/aw-service/src/cli/adapter/catalog.rs`; use that profile when building
   launch capabilities. Keep the configuration schema aligned.
2. Implement native normalization, registration, effect adoption and teardown in
   the Adapter. Keep process ownership in `aw-exec` and common lifecycle in the
   launcher. Reuse the existing admission and callback helpers.
3. Add exact runnable case references to `tests/regression-matrix.json` for every
   event/effect and for lifecycle, security and Tokenless composition. Extend
   `crates/aw-service/tests/regression/native.rs` for new native payloads. The
   shared PreTool and PostTool tests iterate the matrix without a fallback
   dialect, so an unimplemented fixture fails.
4. Run `python3 scripts/check_matrix.py --write-readmes` from this directory and
   document native adoption limits in both READMEs. Maintain the roadmap
   separately from supported capabilities.
5. Add pinned native commands, required reports, an owner and the exact acceptance
   boundary to `tests/native-matrix.json`. Unit suites declare counts; custom
   suites declare JSON result files with exact case names or observed counts.
   Both execution and artifact aggregation verify those reports. Every registered
   Agent must be accounted for; new integrations cannot silently opt out of nightly.
6. Run `python3 scripts/check.py`. The gate compares the compiled catalog with
   the matrix and schema, resolves case names against real libtest executables,
   rejects missing or ignored cases and checks both README grids.

Use deterministic service/CLI/Provider fixtures on every relevant push. Keep
real SDK/CLI adoption checks explicit and pinned; describe which native seam
was exercised. A callback transport test does not certify native adoption.
Do not substitute ignored, opt-in or credential-dependent tests for a required
matrix case. Package acceptance separately covers real Tokenless/RTK binaries.

## Layout and CI cost

Rust, Python and JavaScript (`.js`, `.mjs`, `.cjs`) files share a 699-line ceiling,
including tests and embedded fixtures; the gate reports files at 600 lines.
The existing 711-line `src/validation.rs` allowance is fixed, not a precedent.
Split by responsibility before crossing the ceiling. Keep related helpers in
named sibling modules, preserve test entrypoints and assertions, and avoid
compressing code or adding size exemptions. Never add `mod.rs` except the
repository-approved `tests/common/mod.rs` convention.

Only PRs changing `src/aw/**` automatically run AW CI. Component internals belong
in component CI; explicitly validate public contract changes consumed by AW.
Keep daily checks credential-free. The nightly integration workflow builds all
Preview packages once and reuses checksummed candidate-SHA binaries for pinned
native acceptance. Preserve exact version evidence, bounded processes, complete
reports and visible manual boundaries. Missing or skipped suites must fail the
integration result. Prepare private native tool environments; do not weaken
admission to accommodate runner loader overrides or writable shared caches.
Rerun on the exact release SHA before release.

CI checks belong to the existing `AW / required` workflow and local entrypoint.
Scope selection must use the complete change and preserve candidate/tested SHA
identity. Compile test harnesses once, enumerate them directly and reuse the
build for the workspace run. Do not save CI time by skipping shared security,
Tokenless or lifecycle tests based only on the changed Adapter. Gate changes
need negative behavior tests for omissions, ignored tests and failure paths.

Keep source files, protocol schemas and test fixtures separate from generated
artifacts. Store build/test output under `target/`, bound process lifetimes and
clean up owned child processes, profiles, sockets and temporary state.
