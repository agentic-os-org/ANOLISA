# Agent Security CLI - Build Guide

## Quick Start

### Build the Wheel Package

```bash
# Navigate to the agent-sec-cli directory
cd src/agent-sec-core/agent-sec-cli

# Install all dependencies (uv manages .venv automatically)
uv sync

# Build and install in development mode
uv run maturin develop --release

# Or build wheel package
uv run maturin build --release

# Output files:
# dist/agent_sec_cli-0.3.0-cp312-cp312-linux_x86_64.whl
```

### Install the Package

```bash
# From wheel file
uv pip install dist/agent_sec_cli-0.3.0-py3-none-any.whl

# Or install in development mode (recommended)
uv sync
```

### Usage

```bash
# After installation, use the CLI command
agent-sec-cli --help
agent-sec-cli harden --scan --config agentos_baseline
agent-sec-cli verify
```

---

## Project Structure

```
agent-sec-cli/
├── src/
│   └── agent_sec_cli/              # Main Python package
│       ├── __init__.py             # Package metadata
│       ├── cli.py                  # CLI entry point
│       ├── asset_verify/           # Integrity verification
│       │   ├── __init__.py
│       │   ├── verifier.py
│       │   ├── errors.py
│       │   ├── config.conf
│       │   └── trusted-keys/
│       ├── sandbox/                # Sandbox policy
│       │   ├── __init__.py
│       │   ├── sandbox_policy.py
│       │   ├── classify_command.py
│       │   └── rules.py
│       ├── security_events/        # Event logging
│       │   ├── __init__.py
│       │   ├── writer.py
│       │   ├── schema.py
│       │   └── config.py
│       └── security_middleware/    # Middleware layer
│           ├── __init__.py
│           ├── router.py
│           ├── lifecycle.py
│           ├── context.py
│           ├── result.py
│           └── backends/
│               ├── __init__.py
│               ├── hardening.py
│               ├── sandbox.py
│               ├── asset_verify.py
│               ├── summary.py
│               └── intent.py
├── pyproject.toml                  # Build configuration
├── README.md                       # Documentation
├── .gitignore
└── dist/                           # Build output
    ├── agent_sec_cli-0.3.0-py3-none-any.whl
    └── agent_sec_cli-0.3.0.tar.gz
```

---

## Build Configuration

### pyproject.toml

The package uses modern Python packaging with `pyproject.toml`:

- **Build system**: maturin >= 1.0 (Rust + Python hybrid)
- **Package layout**: src/ layout (recommended best practice)
- **Entry point**: `agent-sec-cli` command → `agent_sec_cli.cli:main`
- **Package data**: Includes config files and trusted keys

### Dependencies

**Runtime:**
- System `gpg` / `gnupg2` binary >= 2.0

**Optional:**
- pgpy >= 0.5 (faster PGP verification)

**Development:**
- black (code formatting)
- isort (import sorting)
- pytest (testing)
- pytest-cov (coverage)

---

## Migration Notes

### What Changed

1. **Directory renamed**: `skill/scripts` → `agent-sec-cli`
2. **Package structure added**: Proper Python package with `src/` layout
3. **Naming convention**: Hyphens replaced with underscores in Python packages
   - `asset-verify` → `asset_verify`
   - `security_middleware` (unchanged)
   - `security_events` (unchanged)
   - `sandbox` (unchanged)
4. **Imports updated**: All imports now use fully qualified package paths
   - Example: `from security_middleware import X` → `from agent_sec_cli.security_middleware import X`
5. **Packaging files created**:
   - `pyproject.toml` - Modern build configuration
   - `__init__.py` files in all packages
   - `README.md` - Comprehensive documentation
   - `.gitignore` - Standard Python ignores

### Backward Compatibility

## CLI Usage

The CLI is now installed as a Python package:

```bash
# Installed command (recommended)
agent-sec-cli verify
agent-sec-cli harden --scan --config agentos_baseline
```

---

## Development Workflow

### Install Development Dependencies

```bash
uv sync
```

### Run Tests

```bash
# Unit tests (from agent-sec-core directory)
uv run --project agent-sec-cli pytest tests/unit-test/

# Integration tests
uv run --project agent-sec-cli pytest tests/integration-test/

# With coverage
uv run --project agent-sec-cli pytest --cov=agent_sec_cli tests/
```

### Code Formatting

```bash
# Format code
uv run black src/
uv run isort src/

# Or use Makefile
make python-code-pretty
```

---

## Troubleshooting

### Build Errors

**Error**: `externally-managed-environment`
- **Solution**: Use uv to manage environment: `uv sync`

**Error**: `ModuleNotFoundError` during build
- **Solution**: Ensure you're building from the `agent-sec-cli/` directory (not parent)

**Error**: License warning in pyproject.toml
- **Note**: This is a deprecation warning, not an error. The build still succeeds.
- **Fix**: Update to setuptools >= 77.0.0 and use SPDX license expression

### Import Errors

**Error**: Import conflicts between old and new structure
- **Solution**: Remove old `skill/scripts` directory or ensure PYTHONPATH is clean

---

## Distribution

### Include in RPM

The RPM spec file (`agent-sec-core.spec`) has been updated to copy files from the new location:

```spec
# Install scripts
pip3 install --root=$RPM_BUILD_ROOT --no-deps --no-cache-dir --prefix=/usr \
    agent-sec-cli/target/wheels/agent_sec_cli-*.whl
```

---

## Rust Development

### Project Structure

```
agent-sec-cli/
├── Cargo.toml              # Rust dependencies
├── src/
│   ├── lib.rs              # Rust native module
│   └── agent_sec_cli/      # Python package
└── pyproject.toml          # Build configuration (maturin)
```

### Build Commands

```bash
# Build and install in development mode
uv run maturin develop --release

# Build wheel for distribution
uv run maturin build --release

# Build for specific Python version
uv run maturin build --release -i python3.11

# Run Rust tests
cargo test

# Check Rust code
cargo clippy
```

### Adding New Native Functions

1. Add function in `src/lib.rs`:

```rust
#[pyfunction]
fn my_security_function(param: &str) -> PyResult<String> {
    // Implementation
    Ok(format!("Result: {}", param))
}
```

2. Register in `#[pymodule]`:

```rust
#[pymodule]
fn _native(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(my_security_function, m)?)?;
    Ok(())
}
```

3. Use from Python:

```python
from agent_sec_cli._native import my_security_function
result = my_security_function("test")
```

---

## Version History

- **0.3.0** - Current version
  - Restructured as proper Python package
  - Added wheel build support
  - Updated all imports to use package paths
  - Created comprehensive documentation

---

## References

- [Python Packaging Guide](https://packaging.python.org/)
- [pyproject.toml Specification](https://packaging.python.org/en/latest/specifications/pyproject-toml/)
- [setuptools Documentation](https://setuptools.pypa.io/)
- [Build Tool](https://pypa-build.readthedocs.io/)
