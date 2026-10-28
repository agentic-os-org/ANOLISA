# Contributing to AW

[中文版](CONTRIBUTING_zh.md)

Use Linux to change the AW framework, Adapters or gates. See the [developer guide](../../docs/developer-guide/en/aw/index.md) for application integration and deployment, and [repository contributing](../../CONTRIBUTING.md) for PR rules.

## Build and check

Prepare rustup, Python 3.11+ and Node.js 22+ (CI uses 24.15.0). [rust-toolchain.toml](rust-toolchain.toml) pins Rust, rustfmt and Clippy.
Run from the repository root.

```bash
python3 src/aw/scripts/check.py
```

The gate runs Python behavior tests, formatting, Clippy, locked Rust tests, digest vectors, rustdoc, layout and capability inventory checks.
Logs identify failing commands; each command has a deadline and owned-process cleanup. AW implementation changes require the complete Linux gate. See [building](../../contribute/en/aw/building.md) for packages.

| Change | Targeted development check |
| --- | --- |
| Documentation and navigation | `bash scripts/docs-lint.sh`, `python3 scripts/docs-link-check.py` |
| Python gates | `python3 -B -m unittest discover -s src/aw/tests -p 'test_ci_*.py'` |
| Host / service / CLI | From `src/aw`, `cargo test --locked -p aw-host -p aw-service -p aw-cli` |
| Package execution | From `src/aw`, `cargo test --locked -p aw-exec --test packages` |
| Package database / management | From `src/aw`, `cargo test --locked -p aw-package --test management --test database` |
| Transaction recovery | From `src/aw`, `cargo test --locked -p aw-package transaction::tests` |
| Provider | Independent workspace `cargo test --locked --manifest-path providers/PROVIDER/Cargo.toml` |

## Change capability support

1. Update the Adapter catalog, configuration schema and actual wiring.
2. Add real cases for each event/effect, security, Tokenless and lifecycle composition in [regression-matrix.json](tests/regression-matrix.json).
3. Run these commands from `src/aw` to synchronize the visible README and capability-page matrices.

```bash
python3 scripts/check_matrix.py --write-docs
python3 scripts/check.py
```

Missing, ignored or empty required tests fail the gate. [native-matrix.json](tests/native-matrix.json) separately records pinned native versions, commands and reports.
See [native and release acceptance](../../contribute/en/aw/testing.md) for execution and evidence requirements.

## Layout and ownership

Shared framework code lives in `crates/`; concrete Providers have independent workspaces under repository-root `providers/`.
Adapters own native dialects, CLI owns arguments, signals and resources, and service owns state, budgets and audit.
See the [design index](docs/design/en/README.md) for architecture and [AGENTS.md](AGENTS.md#documentation-audiences-and-writing) for documentation audiences and writing rules.

Rust, Python and JavaScript files have a 699-line limit with an advisory at 600; the existing 711-line validation.rs exception stays fixed.
Update `scripts/check_layout.py` and behavior tests with boundary changes. Put test output under `target/` and explicitly clean owned processes, containers and temporary directories.

## CI results

`AW / required` joins AW and selected Tokenless compatibility checks for the same candidate SHA. Missing evidence, failures and cancellations block it.
Ordinary documentation changes use the AW fixture gate. Changes to Tokenless consumer source, protocol, executor and package inputs also select real backend compatibility.
Pinned SDK, package installation and real backend composition run through separate [integration acceptance](../../contribute/en/aw/testing.md).
