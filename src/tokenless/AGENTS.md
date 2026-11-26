# AGENTS.md — tokenless

Repository-wide contribution, platform, code, documentation, review-triage, and
commit rules inherit from [the root AGENTS.md](../../AGENTS.md). Follow the root
platform restrictions in [AGENTS.md §1](../../AGENTS.md#1-project-overview)
before running native builds or Rust checks.

## Workspace and build gotchas

- **Toolchain & crates** ([Cargo.toml](Cargo.toml)): Rust `1.91+` (edition 2024).
  `default-members` covers 7 crates (`tokenless-protocol`, `tokenless-ccr`,
  `tokenless-compressors`, `tokenless-schema`, `tokenless-runtime`, `tokenless-stats`,
  `tokenless-cli`). `python/tokenless` (PyO3) is excluded from default
  members, included by `--workspace` (or `-p tokenless-python` /
  `make python-wheel`), and requires a discoverable CPython `3.11+` dev
  environment.
- **Vendored RTK**: `third_party/rtk` is excluded from the workspace. Run
  `just setup-rtk` once before building; prefer Core routing over patching RTK.
- **Platform scope & PyO3 ABI**: Supports Linux (full), macOS x64/arm64 (CLI +
  adapters via npm), and Windows x64 (native CLI + QwenWork adapter). Gate
  OS-specific code with `#[cfg]`, and keep `[profile.python-release]` at
  `panic = "unwind"` so PyO3 never aborts the embedding Python interpreter.
- **Generated manifests**: Edit `*.in` templates, not stamped outputs; regenerate
  via `make stamp-adapter-templates` and `make stamp-python-packages` ([Makefile](Makefile)).
- **Standalone benchmark workspaces**: `benchmark/l1-compressor`,
  `benchmark/l2-module`, and `benchmark/l3-scenario` each declare `[workspace]`
  and are skipped by root `--workspace` (CI checks `l1-compressor` and
  `l2-module` separately; `l3-scenario` is currently ungated and out of CI due
  to an existing build break).

## Local validation gates

Avoid `just test` (covers only a subset of crates). Run matching gates from
`src/tokenless/` (full CI matrix in [Makefile](Makefile) and `.github/workflows/ci.yaml`):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings       # 7 default crates; CI runs `cargo clippy --workspace -- -D warnings` (same as `make lint`, needs CPython dev env)
cargo test --lib --bins -- --test-threads=1     # fast 7-crate check (--test-threads=1 avoids CLI env-var races)
make test-tokenless                             # CI Rust gate: --workspace lib/bins (--test-threads=1), --test '*', --examples --no-run, --doc
make test-hook-scripts                          # fast bash hook suites (rides on test-integration in CI)
make test-integration                           # Python hook + parity + contract suites (CI gate)
make test-rtk-integration                       # bundled RTK patch integration (CI gate)
make test-adapters                              # OpenClaw/DSH/npm + adapter installer suites (CI gate)
make test-python-runtime                        # PyO3 wheel + 4-lifecycle + stats tests (CI gate)
make test-agentscope-integration                # AgentScope 1.x/2.x Python integration suite (CI gate)
make test-packaging                             # installer layout + RPM split fixtures (mock/prebuilt)
make test-install-docs                          # installation doc consistency
```

- `make test-hooks` targets an installed release binary on `PATH` (local dev
  only; use `test-integration` / `test-hook-scripts` to match CI). CI also runs
  a standalone native-Linux `make npm-package` tarball install smoke test.

## Core invariants and review checklist

Leave formatting, clippy, and doc-style checks to local/CI gates; focus code
changes and self-review on the invariants below, platform/ABI safety, and
delivery boundaries. Use [docs/response-compression.md](docs/response-compression.md)
and [docs/stash-reversible-compression.md](docs/stash-reversible-compression.md)
for contract background, and verify current behavior against `tokenless-runtime`
and `tokenless-protocol` whenever docs and code diverge.

- **Core owns policy; adapters own envelopes**: Detection, thresholds,
  compression/TOON, diagnostics, recovery gating, and Stash policy live in Rust
  Core (`tokenless-runtime` / `tokenless-compressors`). Adapters
  (`adapters/tokenless/<host>/`, `python/agentscope/`) translate host envelopes,
  declare capabilities, forward lifecycle calls, and own host-specific
  multi-block/media handling (some hosts skip multi-block/media results; others
  process text blocks individually while preserving non-text blocks and order).
- **Fail-open & iterative scope**: Tool errors, unparseable/oversized inputs,
  non-strictly-smaller candidates, and unsupported host slots pass through
  unchanged. Never swallow non-zero exits or error diagnostics, over-engineer
  obscure format parsers, or add speculative fallbacks already constrained by
  types or upstream boundaries.
- **Origins, fidelity & Stash**: `file_content` passes through without
  compression; `file_read` still compresses diff, build-log, JSON, and CSV/TSV
  (`tabular`), bypassing only HTML (including JSON-embedded HTML),
  browser-snapshot, and search path-list. Lossless codecs preserve semantics,
  exact JSON numbers, and key order (`preserve_order`). Lossy/retrievable
  reductions require host-declared recovery and Stash rollback on discard
  (`recovery: none` accepts lossless only). `tokenless retrieve` output bypasses
  PostTool.
- **Shell & intentional non-coverage (verify against Core tables before changing or flagging gaps)**:
  Rewritten commands must preserve quoting, pipes, redirections, and exit status
  (pass through when ambiguous). Tool Ready is hard-disabled product-wide — do
  not re-enable via env vars or remove compatibility hooks without a redesign
  spec. PreTool exempts Core-owned native commands (and whole compound commands
  containing them) using built-in wrapper prefixes only; custom RTK
  `hooks.transparent_prefixes` are not read (RTK may still rewrite behind them).
  PostTool passes through missing/expired RTK receipts, unreceipted non-POSIX
  wrappers, and formats without explicit lossless record boundaries.
- **Protocol v2 evolution**: Reuse an existing `AppliedOperation` label when a
  new optimization belongs to the same operation family; add a new Protocol v2
  enum variant and sync the Python SDK only when introducing a distinct semantic
  domain. `Recoverability` distinguishes three states: `lossless`, `retrievable`,
  and `unrecoverable`.

## Delivery, RPM ownership, and ANOLISA metadata boundary

### Build and installation entrypoints

- [Makefile](Makefile) (`packaging/*.mk`) is the single public entrypoint and
  [packaging/install.sh](packaging/install.sh) owns installation and layout
  validation — do not add a second public Makefile.
- `make build` builds the native CLI (linking RTK) and framework bundles.
  `make install`, `install-core`, `install-plugins`, and
  `install-core-with-cosh` copy prebuilt outputs via `PREFIX`, `AWPRJ`, and
  optional `DESTDIR` without building, registering frameworks, or activating
  services: `install-core` (CLI + core resources), `install-plugins`
  (adapters, stamped manifests, and Node-built bundles without requiring core
  build outputs), `install-core-with-cosh` (pairs with `make build-core` to
  copy CLI, core resources, and the checked-in native Cosh extension).
  `make install-legacy` runs `build` first and installs using the historical
  user/system profile; `make setup` builds, installs, and registers adapters
  using the same resolved layout as `make uninstall`.
- Without `INSTALL_PROFILE`, UID 0 selects `system` (`PREFIX=/usr/local`);
  other UIDs select `user` (`PREFIX=$HOME/.local`, `XDG_DATA_HOME` or owned
  `$HOME/.local/share`). Keep private executables under `PREFIX/libexec` since
  XDG defines no libexec root. Explicit profile/role paths override defaults.
  Packaging uses `INSTALL_PROFILE=system`, `PREFIX=/usr`, `DESTDIR` and ignores
  user XDG roots. Installs never migrate existing runtime config or state.

### RPM package ownership

[packaging/rpm/tokenless.spec.in](packaging/rpm/tokenless.spec.in) is the
authority for physical RPM products, dependencies, file ownership, and
scriptlets.

| Package | Responsibility |
|---------|----------------|
| `tokenless` | Empty compatibility package selecting matching core and plugins; retains `Provides: anolisa-component(tokenless)`. |
| `tokenless-core` | Public `tokenless` command (runs RTK as `tokenless rtk`), license, and runtime docs. |
| `tokenless-plugins` | Framework integrations, common hooks/resources, lifecycle scripts, and cosh extension. |

- Compatibility, core, and plugins require exact `%{version}-%{release}`;
  `tokenless-plugins` also requires matching core plus Python, `jq`, and Bash.
  There is no Tokenless skills RPM. Preserve conflicts with older monolithic
  `tokenless` packages for same-transaction ownership transfer; keep scriptlets
  in their owning package and guard framework removal for final erase.
- Update shared file inventories and RPM `%install`/`%files` together; never
  place framework payloads in `tokenless-core` or duplicate ownership across roles.

### ANOLISA metadata boundary

- ANOLISA owns the installed RPM registry and its Tokenless contract
  ([distribution/anolisa/manifests/components/tokenless/component.toml](../../distribution/anolisa/manifests/components/tokenless/component.toml)).
  Neither RPMs nor raw layouts install `/usr/share/anolisa/components/tokenless/component.toml`
  or `{datadir}/components/tokenless/component.toml`. Compatibility `Provides`
  identifies the component; it does not transfer ownership of ANOLISA's registry
  to Tokenless. Raw layouts require ANOLISA `0.3.21+` for that registry; keep
  embedded archive contracts and installed-state snapshots separate from it.
- [packaging/anolisa/component.toml.in](packaging/anolisa/component.toml.in) is
  rendered only by `make generate-component-contract` to
  `target/packaging/anolisa/component.toml` (embedded as `.anolisa/component.toml`
  in raw archives). Ordinary framework builds stamp their own manifests
  independently and must not generate ANOLISA metadata; RPM builds must not
  depend on generating or installing that raw contract.
