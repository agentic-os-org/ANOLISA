---
name: add-backend
description: Guide for adding a backend (Rust or Python) to the agent-sec-core security middleware. Use when creating new backends, integrating Rust or Python code into the security middleware, or extending with new backend actions.
arguments:
  - name: backend_name
    description: "Name of the new backend (e.g. 'code_verify'). Spaces are converted to underscores for code identifiers."
    required: true
  - name: backend_type
    description: "Backend implementation type: 'rust' or 'python'"
    required: true
  - name: module_path
    description: "For python type: module path (e.g. 'agent_sec_cli.code_verify.verifier'). Required when backend_type=python."
    required: false
---

# Adding a Backend to Security Middleware

This skill walks through the **complete, end-to-end** process of adding a backend
(Rust or Python) to the security middleware, wiring it into the router, and
exposing it through the CLI.

> **Unified interface**: Both Rust and Python backends implement the same
> `execute(ctx, **kwargs) → ActionResult` contract. The middleware doesn't care
> about the implementation language.

## Backend Type Selection

| Type | Use Case | Pros | Cons |
|------|----------|------|------|
| **rust** | Performance-critical, CPU-intensive tasks | High performance, memory safety | Requires Rust toolchain, compilation |
| **python** | Rapid development, glue code, existing libraries | Fast iteration, rich ecosystem | Slower execution, GIL limitations |

## Naming Convention

Derive all identifiers from the `backend_name` argument:

| Concept | Rule | Example (`backend_name` = "code verify") |
|---------|------|------------------------------------------|
| action_name | lowercase, underscores | `code_verify` |
| Backend class | PascalCase + `Backend` | `CodeVerifyBackend` |
| Python module | `{action_name}.py` | `code_verify.py` |
| lifecycle category | same as action_name | `code_verify` |

**Rust-specific** (only when `backend_type=rust`):

| Concept | Rule | Example |
|---------|------|----------|
| Rust function | same as action_name | `code_verify` |
| Request struct | PascalCase + `Request` | `CodeVerifyRequest` |
| Response struct | PascalCase + `Response` | `CodeVerifyResponse` |

---

## 1. Architecture Overview

Both backend types follow the same execution flow:

```
agent-sec-cli  ──→  security_middleware.invoke("{action_name}", **kwargs)
                            │
                            ├─ router.get_backend("{action_name}")
                            │      └─ _REGISTRY["{action_name}"] → "security_middleware.backends.{action_name}"
                            │      └─ lazy import → {ActionName}Backend()
                            │
                            ├─ backend.execute(ctx, **kwargs) → ActionResult
                            │      │
                            │      ├─ [Rust]  from agent_sec_cli._native import {action_name}
                            │      │          {action_name}(json_in) → json_out
                            │      │
                            │      └─ [Python] import {module_path}
                            │                  module.function(**kwargs) → result
                            │
                            └─ lifecycle.post_action() → SecurityEvent → JSONL
```

**Key contract**: Every backend is a Python class with an `execute(ctx, **kwargs) → ActionResult`
method. The implementation language (Rust/Python) is an **implementation detail** — the middleware
never calls Rust or module functions directly.

---

## 2. Create the Python Backend Wrapper

The Python backend wrapper is the **unified interface** that the middleware calls. It delegates
to either Rust or Python implementation based on `backend_type`.

### 2.1 Choose Template

- **For `backend_type=rust`**: Use `templates/rust_backend.py`
- **For `backend_type=python`**: Use `templates/python_backend.py`

### 2.2 Create Backend File

Create `agent-sec-cli/src/agent_sec_cli/security_middleware/backends/{action_name}.py`

Copy the appropriate template and replace placeholders:
- `{backend_name}` → actual backend name (e.g., "code_verify")
- `{BackendName}` → PascalCase class name (e.g., "CodeVerify")
- `{action_name}` → action name for Rust calls (e.g., "code_verify")
- `{module_path}` → Python module path (only for python type, e.g., "agent_sec_cli.code_verify.verifier")

**Convention**: Class name = PascalCase of module name + `Backend`.

> **IMPORTANT — `stdout` / `error` contract**: The CLI (`agent-sec-cli`) only
> prints `result.stdout` and `result.error`. If a backend returns an `ActionResult`
> with both `stdout` and `error` empty, the CLI produces **no output at all**.
> Every `ActionResult` **must** populate at least one of:
>
> | Field | When to set |
> |-------|-------------|
> | `stdout` | Always on success — human-readable text for the terminal |
> | `error` | Always on failure — written to stderr by the CLI |
>
> A helper like `_format_stdout()` keeps formatting in one place and makes it
> easy to test independently.

---

## 3. Register Backend in Router and Lifecycle

### 3.1 Register in Router

Edit `agent-sec-cli/src/agent_sec_cli/security_middleware/router.py` — add to `_REGISTRY`:

```python
_REGISTRY: Dict[str, str] = {
    # ... existing entries ...
    "{action_name}":   "agent_sec_cli.security_middleware.backends.{action_name}",
}
```

### 3.2 Add Lifecycle Category Mapping

Edit `agent-sec-cli/src/agent_sec_cli/security_middleware/lifecycle.py` — add to `_ACTION_CATEGORY`:

```python
_ACTION_CATEGORY: Dict[str, str] = {
    # ... existing entries ...
    "{action_name}":   "{action_name}",
}
```

### 3.3 Add CLI Entry Point

Edit `src/agent_sec_cli/cli.py` — add a new `@app.command()` function:

```python
# {action_name} subcommand
@app.command()
def {action_name}(
    param1: str = typer.Option("", "--param1", help="Parameter 1"),
    # Add more arguments as needed for the backend
):
    """{ActionName} description."""
    result = invoke("{action_name}", param1=param1)
    if result.stdout:
        typer.echo(result.stdout)
    if result.error:
        typer.echo(result.error, err=True)
    raise typer.Exit(code=result.exit_code)
```

Now callable as:

```bash
agent-sec-cli {action_name} --param1 value
```

---

## 4. Rust-Specific Steps (backend_type=rust)

> Skip this section if `backend_type=python`.

> **Sub-skill available**: For complex Rust modules (multiple files, shared types,
> or workspace-scale projects), use the **`add-rust-module`** sub-skill at
> `sub-skills/add-rust-module.md`. It covers single-file modules, directory modules,
> and Cargo workspace layouts.
>
> The steps below cover the **simple inline case** (adding a function directly to
> `src/lib.rs`). For anything beyond a single function, delegate to the sub-skill:
>
> ```
> add-rust-module module_name="{action_name}" functions="{action_name}"
> ```

### 4.1 Add Rust Function to lib.rs

Edit `agent-sec-cli/src/lib.rs` — add your Rust function above the `#[pymodule]` block:

```rust
// ---------------------------------------------------------------------------
// {action_name}
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct {ActionName}Request {
    // Add domain-specific fields here
}

#[derive(Serialize)]
struct {ActionName}Response {
    // Add domain-specific output fields here
}

/// Pure Rust logic — no Python API calls.
fn do_{action_name}(req: &{ActionName}Request) -> Result<{ActionName}Response, String> {
    // Implement domain logic here
    todo!("implement {action_name} logic")
}

#[pyfunction]
fn {action_name}(py: Python<'_>, request_json: &str) -> PyResult<String> {
    let req: {ActionName}Request = serde_json::from_str(request_json)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("Invalid JSON: {e}")
        ))?;

    py.allow_threads(|| {
        let resp = do_{action_name}(&req)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e))?;
        serde_json::to_string(&resp)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                format!("Serialization failed: {e}")
            ))
    })
}
```

### 4.2 Register in #[pymodule]

Add this line inside the `_native` pymodule function in `src/lib.rs`:

```rust
m.add_function(wrap_pyfunction!({action_name}, m)?)?;
```

### 4.3 Update Python Backend Wrapper

Edit `agent-sec-cli/src/agent_sec_cli/security_middleware/backends/{action_name}.py`:

```python
from agent_sec_cli._native import {action_name} as rust_{action_name}

class {BackendName}Backend:
    def execute(self, ctx, **kwargs) -> ActionResult:
        try:
            req = json.dumps(kwargs)
            resp_json = rust_{action_name}(req)
            resp = json.loads(resp_json)
            return ActionResult(
                success=True,
                data=resp,
                stdout=self._format_stdout(resp),
            )
        except Exception as exc:
            return ActionResult(success=False, error=f"Rust error: {exc}", exit_code=1)
```

**Key changes**:
- No `RUST_AVAILABLE` check needed (Rust code is always available)
- Import directly from `agent_sec_cli._native`
- No Python fallback (unless you intentionally keep it)

### 4.4 Build and Test

#### 4.4.1 Rebuild with maturin

```bash
cd agent-sec-cli
uv run maturin develop --release
```

#### 4.4.2 Test from Python

```python
from agent_sec_cli._native import {action_name}
import json

req = json.dumps({"param": "value"})
resp = {action_name}(req)
print(json.loads(resp))
```

#### 4.4.3 Run Rust Tests

```bash
cd agent-sec-cli
cargo test
```

### 4.5 Add Dependencies (if needed)

If your Rust function needs additional crates (e.g., `serde`, `serde_json`),
edit `agent-sec-cli/Cargo.toml`:

```toml
[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
```

### 4.6 Complex Rust Modules

If the Rust logic is too large for a single function in `lib.rs`, use the
**`add-rust-module`** sub-skill to create a dedicated module:

```
add-rust-module module_name="{action_name}" functions="{action_name}" complex="true"
```

This will:
1. Create `src/{action_name}/mod.rs` with proper sub-module layout
2. Wire `mod {action_name};` into `src/lib.rs`
3. Register all `#[pyfunction]`s in the `#[pymodule]` block
4. Add required dependencies to `Cargo.toml`

See `sub-skills/add-rust-module.md` for full details including the Cargo
workspace pattern for very large projects.

---

## 5. Python-Specific Steps (backend_type=python)

> Skip this section if `backend_type=rust`.

### 5.1 Create Python Module

Create the Python module at `{module_path}` (e.g., `agent_sec_cli/code_verify/verifier.py`).

**Example Structure:**

```
agent_sec_cli/{backend_name}/
├── __init__.py
├── verifier.py      # Core logic
├── config.py        # Configuration (optional)
└── tests/
    └── test_verifier.py
```

### 5.2 Implement Core Logic

Implement the main function in your module (e.g., `verifier.py`):

```python
"""{backend_name} — core logic."""

def verify(**kwargs):
    """Main verification logic.
    
    Args:
        **kwargs: Parameters passed from CLI/backend.
    
    Returns:
        dict with 'success', 'output', 'data' keys.
    """
    # Implement domain logic here
    return {
        "success": True,
        "output": "Verification passed",
        "data": {"checked": 10, "passed": 10},
    }
```

### 5.3 Update Backend Wrapper

In `security_middleware/backends/{action_name}.py`, update the `_run` method
to call your module's function:

```python
@staticmethod
def _run(module, **kwargs) -> ActionResult:
    """Execute the module logic."""
    result = module.verify(**kwargs)
    return ActionResult(
        success=result["success"],
        stdout=result["output"],
        data=result["data"],
        exit_code=0 if result["success"] else 1,
    )
```

### 5.4 Python Unit Tests

Create tests in `{module_path}/tests/`:

```python
import pytest
from agent_sec_cli.{backend_name}.verifier import verify

def test_verify_basic():
    result = verify(param1="value")
    assert result["success"] is True
    assert result["data"]["checked"] > 0
```

Run tests:

```bash
pytest agent_sec_cli/{backend_name}/tests/
```

---

## 6. Testing (Both Types)

### 6.1 Integration Tests

**For Rust backends**, create a test script (e.g., `tests/test_{action_name}_integration.py`):

```python
#!/usr/bin/env python3
"""Integration test for {action_name} native function.

Run after ``maturin develop``::
    python3 tests/test_{action_name}_integration.py
"""
import json
import sys


def main() -> int:
    try:
        from agent_sec_cli._native import {action_name}
    except ImportError:
        print("SKIP: _native not importable (run `maturin develop` first)")
        return 0

    errors = 0

    # 1. Basic test
    req = json.dumps({"param": "value"})
    resp = json.loads({action_name}(req))
    # Add assertions
    print(f"PASS: basic test (resp={{resp}})")

    # 2. Invalid JSON
    try:
        {action_name}("{{bad json")
        print("FAIL: expected ValueError for invalid JSON")
        errors += 1
    except ValueError:
        print("PASS: invalid JSON raises ValueError")

    if errors:
        print(f"\n{{errors}} test(s) FAILED")
        return 1
    print("\nAll tests passed!")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

**For Python backends**, create tests in `{module_path}/tests/` (see Section 5.4).

### 6.2 E2E CLI Tests

This verifies the **full call chain**: CLI → `invoke()` → router → backend → result → CLI output.

#### 6.2.1 CLI Smoke Test

```bash
agent-sec-cli {action_name} --param1 test
```

Expected behaviour:
- Exit code `0` on success.
- Output contains expected result (no errors).

#### 6.2.2 Verify Rust Path (Rust backends only)

```bash
# From agent-sec-core/
agent-sec-cli {action_name} --param1 value
```

Expected behaviour:
- Exit code `0` on success.
- `result.data` contains the Rust backend's JSON response fields.
- No `"python fallback"` note in the output (confirms the Rust path was used).

#### 6.2.3 Negative / Error-Path Test

Pass invalid or adversarial input to confirm the backend returns a non-zero exit
code and a meaningful error message:

```bash
# Example: omit required fields or pass unexpected values
agent-sec-cli {action_name}
```

Expected behaviour:
- The CLI exits with a non-zero code **or** returns an error message on stderr
  if the backend rejects the input.

---

## 7. Checklist

### Common (Both Types)

```
- [ ] Python backend wrapper created in security_middleware/backends/{action_name}.py
- [ ] Class name follows PascalCase + Backend convention
- [ ] Action registered in router._REGISTRY
- [ ] Category mapped in lifecycle._ACTION_CATEGORY
- [ ] CLI command added to `src/agent_sec_cli/cli.py`
- [ ] stdout/error contract satisfied (ActionResult always has output)
- [ ] Unit tests created and pass
- [ ] E2E CLI test passes
```

### Rust-Specific (backend_type=rust)

```
- [ ] Rust function added to src/lib.rs or separate module (see sub-skills/add-rust-module.md)
- [ ] Rust function uses JSON-in/JSON-out boundary
- [ ] GIL released with py.allow_threads()
- [ ] No Python API calls inside allow_threads block
- [ ] PyO3 0.20 API (Python + &PyModule)
- [ ] Function registered in #[pymodule] via m.add_function()
- [ ] Required crate dependencies added to Cargo.toml
- [ ] maturin develop --release succeeds
- [ ] Rust unit tests added and pass (cargo test)
- [ ] Function importable: from agent_sec_cli._native import {action_name}
- [ ] E2E: CLI returns Rust result with correct data
```

### Python-Specific (backend_type=python)

```
- [ ] Python module created at {module_path}
- [ ] Core logic implemented (e.g., verify(), scan(), etc.)
- [ ] Backend wrapper _run() method calls module function
- [ ] Python unit tests pass (pytest)
- [ ] Module structure follows asset_verify pattern
```
