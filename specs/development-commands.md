# ANOLISA Development Commands

> Canonical per-component build, lint, and test commands for AI agents and
> contributors, routed from [`AGENTS.md`](../AGENTS.md) §2.
> For contribution workflow and environment prerequisites, see
> [`CONTRIBUTING.md`](../CONTRIBUTING.md).

## 1. Unified Build & Partial Test Suites (Linux)

```bash
# Unified build (Linux; six backends including sec-core V2, AW and matching Providers)
./scripts/build-all.sh --all --no-install                         # prepare build dependencies and compile
./scripts/build-all.sh --all                                     # prepare, compile and install system files
./scripts/build-all.sh --all --user --supported-only               # install cosh-ng, tokenless, AW and Tokenless Provider; report skipped system-only targets
./scripts/build-all.sh --all --user --supported-only --check-deps  # read-only check of the user subset
./scripts/build-all.sh --all --user --no-install                   # compile all default targets; never install system dependencies by default
./scripts/build-all.sh --component tokenless --user --install-system-deps # explicitly allow system build packages; component files stay in user paths
./scripts/build-all.sh --component agent-sec-core --component tokenless --no-install
./scripts/build-all.sh --all --prefix /usr --destdir "$PWD/target/pkgroot" # stage package paths without service activation
./scripts/build-all.sh --all --check-deps                         # check build dependencies without changing the host
./scripts/build-all.sh --component agent-sec-core --ignore-deps --no-install

# Partial convenience test suites (may skip unavailable suites)
./tests/run-all-tests.sh --filter sec     # agent-sec-core only
./tests/run-all-tests.sh --filter sight   # agentsight only
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory
```

### `build-all.sh` Installation & Provider Discovery Rules

- `build-all.sh` defaults to system source installation under `/usr/local`;
  `--user` defaults to `$HOME/.local`. Use `--prefix /usr` when staging
  distribution package paths.
- The script selects matching AW Providers automatically for selected backends.
  Selecting AW keeps core and skills and skips standalone component plugin
  bundles. If `cosh-ng` is also selected, the build keeps the Cosh extensions
  for the selected `agent-sec-core` and Tokenless backends.
- Discovery uses `providers/<canonical-backend>/Makefile` and the
  `aw-provider-*` package name from that directory's `Cargo.toml`; `sec-core`
  resolves to `agent-sec-core`, whose Provider is named `aw-provider-sec-core`.
- A partial build such as `--component tokenless` includes its Provider, while
  an explicitly selected Provider does not select its backend.
- In user mode, `--supported-only` filters backends before attaching their
  Providers, so skipping `agent-sec-core` also omits its Provider unless that
  Provider was explicitly selected.

## 2. Per-Component Build & Test Gates

> Do **not** attempt to build Linux-only components (`agent-sec-core`,
> `agent-memory`, `skillfs`, `ws-ckpt`, `ktuner`, `aw`) on macOS or Windows.

```bash
# cosh-ng (Linux full; macOS limited functionality)
cd src/cosh-ng
cargo build --workspace
cargo fmt --all -- --check
# Select the closest targeted test from src/cosh-ng/CONTRIBUTING.md.
# Full gates require a large/cross-cutting change and an explicit request.

# agent-sec-core (Linux only; V2 Rust CLI/daemon and sandbox)
cd src/agent-sec-core
make build         # V2 artifacts; no uv or fixed Python runtime required
make test-rust-v2  # V2 Rust tests; Python-specific tests have separate requirements

# agentsight (Linux full eBPF; macOS trajectory/serve only)
cd src/agentsight
# Linux
make build-all
make lint
make test
# macOS
make build-mac

# os-skills (All platforms; from the repository root with Python 3.11)
python3.11 -m venv .venv
.venv/bin/python -m pip install -r src/os-skills/tests/requirements.txt
.venv/bin/python -m unittest discover -s src/os-skills/tests -v
# No compilation needed. Keep component tests under src/os-skills/tests/.

# tokenless (Linux full; macOS / Windows partial)
cd src/tokenless
make build         # tokenless + RTK + OpenClaw plugin
make lint
make test          # Rust + hooks + integration + adapters

# agent-memory (Linux only)
cd src/agent-memory
make build         # cargo build --release --locked
make fmt-check
make lint
make test          # cargo test --locked
make smoke         # end-to-end MCP stdio smoke test

# anolisa (Linux + macOS arm64)
cd distribution/anolisa
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked

# ws-ckpt (Linux only)
cd src/ws-ckpt
make build         # cargo build --release + openclaw plugin
make test          # cargo test --workspace

# SkillFS (Linux only)
cd src/skillfs
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/test.sh    # FUSE smoke test; skips itself if fuse3 or /dev/fuse is unavailable

# ktuner (Linux only)
cd src/ktuner
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test

# aw (Linux only; toolchain pinned by src/aw/rust-toolchain.toml)
python3 src/aw/scripts/check.py   # from the repository root: CI behavior tests,
                                  # fmt, clippy, locked tests, digest vectors, rustdoc
```
