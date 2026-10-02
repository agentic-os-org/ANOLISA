# Rust Integration Guide

## Architecture Overview

This project uses **maturin + PyO3** to embed Rust code directly into the Python package.

```
┌───────────────────────────────────────────────┐
│  agent_sec_cli (Python Package with Rust)     │
│  ┌────────────────────────────────────────┐   │
│  │  backends/                             │   │
│  │  └── 直接调用 _native module          │   │
│  └────────────────────────────────────────┘   │
│  ┌────────────────────────────────────────┐   │
│  │  _native.cpython-312-x86_64-linux-gnu.so│  │
│  │  - maturin 编译嵌入                    │   │
│  │  - PyO3 绑定                           │   │
│  │  - 随 wheel 一起分发                   │   │
│  └────────────────────────────────────────┘   │
│  ┌────────────────────────────────────────┐   │
│  │  Cargo.toml (在 Python 包内)           │   │
│  │  src/lib.rs (在 Python 包内)           │   │
│  └────────────────────────────────────────┘   │
└───────────────────────────────────────────────┘
```

---

## Build Flow

### Development Build

```bash
cd agent-sec-cli
uv run maturin develop --release
```

This will:
1. Compile Rust code using Cargo
2. Generate `.so` extension module
3. Install Python package with native extension
4. Make `from agent_sec_cli._native import ...` available

### Distribution Build

```bash
uv run maturin build --release
```

Output: `dist/agent_sec_cli-0.3.0-cp312-cp312-linux_x86_64.whl`

**Note**: The wheel is **platform-specific** and includes the compiled `.so` file.

---

## Adding New Rust Functions

### Step 1: Add Function to `src/lib.rs`

```rust
use pyo3::prelude::*;
use serde::{Deserialize, Serialize};

// Request/Response structs
#[derive(Deserialize)]
struct MySecurityRequest {
    mode: String,
    target: String,
}

#[derive(Serialize)]
struct MySecurityResponse {
    status: String,
    findings: Vec<String>,
}

// Pure Rust logic
fn do_my_security(req: &MySecurityRequest) -> Result<MySecurityResponse, String> {
    // Implementation here
    Ok(MySecurityResponse {
        status: "ok".to_string(),
        findings: vec![],
    })
}

// Python binding (JSON in/out)
#[pyfunction]
fn my_security_func(py: Python<'_>, request_json: &str) -> PyResult<String> {
    let req: MySecurityRequest = serde_json::from_str(request_json)
        .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(
            format!("Invalid JSON: {e}")
        ))?;

    py.allow_threads(|| {
        let resp = do_my_security(&req)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyValueError, _>(e))?;
        serde_json::to_string(&resp)
            .map_err(|e| PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(
                format!("Serialization failed: {e}")
            ))
    })
}
```

### Step 2: Register in `#[pymodule]`

```rust
#[pymodule]
fn _native(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(my_security_func, m)?)?;
    Ok(())
}
```

### Step 3: Use from Python Backend

```python
# In backends/my_security.py
from agent_sec_cli._native import my_security_func

class MySecurityBackend:
    def execute(self, ctx, **kwargs) -> ActionResult:
        req = json.dumps(kwargs)
        resp = json.loads(my_security_func(req))
        return ActionResult(success=True, data=resp)
```

---

## Project Structure

```
agent-sec-cli/
├── Cargo.toml              # Rust dependencies
├── src/
│   ├── lib.rs              # Rust native module entry point
│   └── agent_sec_cli/      # Python package
│       ├── __init__.py
│       ├── cli.py
│       └── security_middleware/
│           └── backends/
├── pyproject.toml          # Build configuration (maturin)
└── dev-tools/
    └── backend-skill/      # AI skill for adding backends
```

---

## vs. Separate Compilation (方案 A)

| Aspect | Embedded (Current) | Separate |
|--------|-------------------|----------|
| **Rust location** | `src/lib.rs` (in package) | `rust_backends/` (separate project) |
| **Build** | `maturin develop/build` | `cargo build` + manual deploy |
| **Import** | `from agent_sec_cli._native import ...` | `import rust_backends` |
| **Availability** | Always available | Optional (needs fallback) |
| **Distribution** | Single wheel | Wheel + separate .so |
| **Complexity** | Lower | Higher |

**Why embedded is better for this project**:
- Simpler build process
- No manual .so deployment
- Rust is always available (no fallback needed)
- Single distribution artifact
- Easier for users (`pip install` just works)

---

## Dependencies

### Current Dependencies

```toml
[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
```

### Adding More Dependencies

Edit `Cargo.toml`:

```toml
[dependencies]
pyo3 = { version = "0.20", features = ["extension-module"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
# Add more crates as needed
```

---

## Testing

### Rust Unit Tests

```bash
cargo test
```

### Python Integration Tests

```python
from agent_sec_cli._native import my_security_func
import json

req = json.dumps({"mode": "scan", "target": "/tmp"})
resp = json.loads(my_security_func(req))
print(resp)
```

---

## Key Points

1. **PyO3 Version**: Use 0.20 (stable). Do NOT upgrade to 0.21+ (breaking API changes).
2. **Cargo.lock**: Committed to Git to ensure deterministic builds.
3. **Platform-Specific**: Built wheels are platform-specific (Linux x86_64, macOS ARM, etc.).
4. **GIL Management**: Use `py.allow_threads(|| { ... })` to release GIL during computation.
5. **JSON Boundary**: Use JSON for Python-Rust communication (clean boundary, no Python objects in Rust).
