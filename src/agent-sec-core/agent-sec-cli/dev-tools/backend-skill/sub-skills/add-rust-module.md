---
name: add-rust-module
description: Add a new Rust module to the maturin-based agent-sec-cli project and wire it into the PyO3 entry point (src/lib.rs). Use when a new Rust backend needs its own module file instead of inlining everything in lib.rs.
arguments:
  - name: module_name
    description: "Snake_case name of the new Rust module (e.g. 'hardening', 'sandbox', 'code_verify'). Will become both the file/directory name and the `mod` declaration."
    required: true
  - name: functions
    description: "Comma-separated list of #[pyfunction] names to expose (e.g. 'scan_system,evaluate_policy'). Each function follows the JSON-in / JSON-out pattern."
    required: true
  - name: complex
    description: "'true' if the module needs sub-modules (mod.rs + multiple files). Default: 'false' (single file)."
    required: false
---

# Adding a Rust Module to agent-sec-cli

This sub-skill covers creating a **new Rust module** and integrating it into the
maturin-based `agent-sec-cli` project. It is called by the parent skill
`add-backend` (Section 4) when `backend_type=rust`.

> **Scope**: This skill only handles the **Rust side** — creating files, wiring
> `mod` declarations, and registering `#[pyfunction]`s. The Python backend
> wrapper, router registration, and CLI command are handled by the parent skill.

---

## 1. Decide Module Layout

Choose based on complexity:

| Layout | When to use | Structure |
|--------|------------|-----------|
| **Single file** | 1–3 functions, simple logic | `src/{module_name}.rs` |
| **Directory module** | 4+ functions, sub-modules, shared types | `src/{module_name}/mod.rs` + sub-files |

---

## 2. Single-File Module (default)

### 2.1 Create Module File

Create `agent-sec-cli/src/{module_name}.rs`:

```rust
//! {module_name} — native security functions.

use pyo3::prelude::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// {function_name} (repeat for each function in `functions` argument)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct {FunctionName}Request {
    // Add domain-specific fields here
}

#[derive(Serialize)]
pub(crate) struct {FunctionName}Response {
    // Add domain-specific output fields here
}

/// Pure Rust logic — no Python API calls.
fn do_{function_name}(req: &{FunctionName}Request) -> Result<{FunctionName}Response, String> {
    // Implement domain logic here
    todo!("implement {function_name} logic")
}

#[pyfunction]
pub fn {function_name}(py: Python<'_>, request_json: &str) -> PyResult<String> {
    let req: {FunctionName}Request = serde_json::from_str(request_json)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("Invalid JSON: {e}")
        ))?;

    py.allow_threads(|| {
        let resp = do_{function_name}(&req)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e))?;
        serde_json::to_string(&resp)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                format!("Serialization failed: {e}")
            ))
    })
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_{function_name}_basic() {
        let req = {FunctionName}Request { /* fields */ };
        let resp = do_{function_name}(&req).expect("should succeed");
        // Add assertions
    }
}
```

### 2.2 Wire into lib.rs

Edit `agent-sec-cli/src/lib.rs`:

```rust
use pyo3::prelude::*;

mod {module_name};  // <-- add this line

#[pymodule]
fn _native(_py: Python, m: &PyModule) -> PyResult<()> {
    // Register functions from {module_name}
    m.add_function(wrap_pyfunction!({module_name}::{function_name}, m)?)?;
    // Repeat for each function
    Ok(())
}
```

---

## 3. Directory Module (complex=true)

Use when the module has multiple logical sub-components.

### 3.1 Create Directory Structure

```
agent-sec-cli/src/{module_name}/
├── mod.rs          # Public API, re-exports
├── types.rs        # Shared request/response structs
├── {sub1}.rs       # Sub-component 1
├── {sub2}.rs       # Sub-component 2
└── tests.rs        # Module-level tests (optional)
```

### 3.2 Create mod.rs

```rust
//! {module_name} — native security functions.

use pyo3::prelude::*;

mod types;
mod {sub1};
mod {sub2};

// Re-export #[pyfunction]s so lib.rs can register them
pub use {sub1}::{function1};
pub use {sub2}::{function2};
```

### 3.3 Create types.rs (shared types)

```rust
//! Shared types for {module_name} sub-modules.

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub(crate) struct {FunctionName}Request {
    // Shared request fields
}

#[derive(Serialize)]
pub(crate) struct {FunctionName}Response {
    // Shared response fields
}
```

### 3.4 Create Sub-Module Files

Each sub-module follows the same pattern as Section 2.1, but imports shared
types from `super::types`:

```rust
//! {sub1} — specific logic for {module_name}.

use pyo3::prelude::*;
use super::types::{FunctionName}Request;

fn do_{function_name}(req: &{FunctionName}Request) -> Result<String, String> {
    // Domain logic
    todo!()
}

#[pyfunction]
pub fn {function_name}(py: Python<'_>, request_json: &str) -> PyResult<String> {
    // Same JSON-in/JSON-out pattern
    // ...
}
```

### 3.5 Wire into lib.rs

Same as Section 2.2 — the `mod.rs` re-exports ensure `lib.rs` stays clean:

```rust
mod {module_name};

#[pymodule]
fn _native(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!({module_name}::{function1}, m)?)?;
    m.add_function(wrap_pyfunction!({module_name}::{function2}, m)?)?;
    Ok(())
}
```

---

## 4. Add Dependencies

If the new module needs additional crates, edit `agent-sec-cli/Cargo.toml`:

```toml
[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
# Add as needed:
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

> **Note**: `serde` and `serde_json` are required for all modules that use the
> JSON-in / JSON-out pattern. Add them on the first Rust backend.

---

## 5. Build and Verify

```bash
# Compile and install
cd agent-sec-cli
uv run maturin develop --release

# Run Rust unit tests
cargo test

# Verify the function is importable from Python
python -c "from agent_sec_cli._native import {function_name}; print('OK')"
```

---

## 6. Scaling to Workspace (future)

When the Rust codebase grows large enough to warrant multiple independent crates
(e.g., a reusable `security-core` library), convert to a Cargo workspace:

```
agent-sec-cli/
├── Cargo.toml              # [workspace] root + PyO3 bindings
├── src/lib.rs              # Thin PyO3 entry point
└── crates/
    ├── security-core/      # Pure Rust — no PyO3 dependency
    │   ├── Cargo.toml
    │   └── src/lib.rs
    └── crypto-utils/
        ├── Cargo.toml
        └── src/lib.rs
```

**Root Cargo.toml**:

```toml
[workspace]
members = [".", "crates/security-core", "crates/crypto-utils"]

[package]
name = "agent-sec-cli"
version = "0.0.1"
edition = "2021"

[lib]
name = "agent_sec_cli_native"
crate-type = ["cdylib"]

[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
security-core = { path = "crates/security-core" }
crypto-utils = { path = "crates/crypto-utils" }
```

**`src/lib.rs`** stays thin — only PyO3 registration:

```rust
use pyo3::prelude::*;
use security_core::scanner;

#[pyfunction]
fn scan_system(py: Python<'_>, request_json: &str) -> PyResult<String> {
    py.allow_threads(|| scanner::scan_from_json(request_json)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e)))
}

#[pymodule]
fn _native(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(scan_system, m)?)?;
    Ok(())
}
```

> **Maturin handles workspaces natively** — no extra configuration needed.
> The sub-crates can be pure Rust (no PyO3), making them reusable and
> independently testable with `cargo test -p security-core`.

---

## 7. Coding Rules

| Rule | Reason |
|------|--------|
| Use `py: Python<'_>` param, not `Python::with_gil()` | GIL is already held in `#[pyfunction]` |
| Use `py.allow_threads(\|\| { ... })` | Releases GIL for concurrency |
| No Python API calls inside `allow_threads` | GIL not held — would segfault |
| Return `PyResult<String>` (JSON) | Clean boundary, no Python objects in Rust |
| `pub` on `#[pyfunction]`s, `pub(crate)` on types | Functions must be visible to `lib.rs` |
| One `mod` declaration per module in `lib.rs` | Keep entry point minimal |

---

## 8. Checklist

```
- [ ] Module file(s) created under agent-sec-cli/src/
- [ ] `mod {module_name};` added to src/lib.rs
- [ ] All #[pyfunction]s registered via m.add_function(wrap_pyfunction!(...))
- [ ] Required crate dependencies added to Cargo.toml
- [ ] `maturin develop --release` succeeds without warnings
- [ ] `cargo test` passes
- [ ] Functions importable from Python: `from agent_sec_cli._native import ...`
```
