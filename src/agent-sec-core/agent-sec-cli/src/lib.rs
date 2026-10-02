//! Native extensions for agent-sec-cli.
//!
//! Exposes the prompt scanner (crates/prompt-scanner) as
//! `agent_sec_cli._native`.  The `prompt_scan` security-middleware
//! backend routes all scan-prompt requests through these functions.

use std::str::FromStr;

use prompt_scanner::{
    PromptScanner, ScanConfig, ScanMode, ScannerError, Turn, ENGINE_VERSION, MODEL_QWEN3_GUARD,
    MODEL_WARDEN_GEN,
};
use pyo3::exceptions::{PyRuntimeError, PyValueError};
use pyo3::prelude::*;

/// Map a scanner error onto the closest Python exception type.
///
/// Bad input and unknown modes are caller mistakes (`ValueError`);
/// everything else is an environment or service failure (`RuntimeError`).
fn to_py_err(err: ScannerError) -> PyErr {
    match err {
        ScannerError::Input(_) => PyValueError::new_err(err.to_string()),
        ScannerError::Config(_)
        | ScannerError::LayerNotAvailable(_)
        | ScannerError::ModelLoad(_)
        | ScannerError::ModelInference(_)
        | ScannerError::ModelService(_) => PyRuntimeError::new_err(err.to_string()),
    }
}

/// Resolve the scan configuration for `mode`, optionally overriding the L2
/// model.
///
/// Pure: no network access, so it is safe to run while holding the GIL.
/// Constructing the scanner itself is deliberately left to the caller, which
/// must do it with the GIL released — see [`build_scanner`].
fn scan_config(mode: &str, model: Option<&str>) -> PyResult<ScanConfig> {
    let mode = ScanMode::from_str(mode).map_err(|e| PyValueError::new_err(e.to_string()))?;
    let mut config = ScanConfig::preset(mode);
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        config.model_name = model.to_string();
    }
    Ok(config)
}

/// Build a scanner from a resolved config.
///
/// Optional layers probe their backing service here, so this performs
/// blocking network I/O and must only be called with the GIL released.
fn build_scanner(config: ScanConfig) -> Result<PromptScanner, ScannerError> {
    PromptScanner::new(config)
}

/// Parse a JSON array of conversation turns.
fn parse_history(history_json: Option<&str>) -> PyResult<Vec<Turn>> {
    let Some(raw) = history_json.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return Ok(Vec::new());
    };
    serde_json::from_str(raw)
        .map_err(|err| PyValueError::new_err(format!("invalid history JSON: {err}")))
}

/// Scan a prompt and return the scan result as a JSON string
/// (schema_version 1.0, the CLI output schema).
///
/// Raises `ValueError` for an unknown mode or empty input, and
/// `RuntimeError` when a mandatory layer or its model is unavailable.
#[pyfunction]
#[pyo3(signature = (text, mode = "standard", source = None, model = None))]
fn scan_prompt_json(
    py: Python<'_>,
    text: &str,
    mode: &str,
    source: Option<&str>,
    model: Option<&str>,
) -> PyResult<String> {
    let config = scan_config(mode, model)?;
    let json = py
        .allow_threads(|| -> Result<String, ScannerError> {
            let scanner = build_scanner(config)?;
            Ok(scanner.scan(text, source)?.to_json())
        })
        .map_err(to_py_err)?;
    Ok(json)
}

/// Scan a conversation triple through the multi-turn (L4) pipeline and
/// return the scan result as a JSON string.
///
/// `history_json` is a JSON array of `{"role", "content"}` objects (the
/// legacy `"role: content"` string form is also accepted).
///
/// Raises `ValueError` for malformed history or an empty query.
#[pyfunction]
#[pyo3(signature = (
    current_query,
    assistant_response,
    history_json = None,
    mode = "multi_turn",
    source = None,
    model = None,
))]
fn scan_multi_turn_json(
    py: Python<'_>,
    current_query: &str,
    assistant_response: &str,
    history_json: Option<&str>,
    mode: &str,
    source: Option<&str>,
    model: Option<&str>,
) -> PyResult<String> {
    let config = scan_config(mode, model)?;
    let history = parse_history(history_json)?;
    let json = py
        .allow_threads(|| -> Result<String, ScannerError> {
            let scanner = build_scanner(config)?;
            let result =
                scanner.scan_multi_turn(&history, current_query, assistant_response, source)?;
            Ok(result.to_json())
        })
        .map_err(to_py_err)?;
    Ok(json)
}

/// Prepare the layers of `mode` so the first scan pays no cold-start cost.
///
/// Raises `RuntimeError` when a required model is not available.
#[pyfunction]
#[pyo3(signature = (mode = "standard", model = None))]
fn warmup_scanner(py: Python<'_>, mode: &str, model: Option<&str>) -> PyResult<()> {
    let config = scan_config(mode, model)?;
    py.allow_threads(|| -> Result<(), ScannerError> {
        build_scanner(config)?.warmup()?;
        Ok(())
    })
    .map_err(to_py_err)
}

/// Describe the native scanner engine (version, implemented layers,
/// supported modes, selectable L2 backends) as a JSON string, so callers can
/// probe engine capabilities.
#[pyfunction]
fn scanner_engine_info() -> String {
    serde_json::json!({
        "engine": "prompt-scanner",
        "engine_version": ENGINE_VERSION,
        "implemented_layers": ["rule_engine", "ml_classifier", "multi_turn_intent"],
        "modes": ["fast", "standard", "strict", "multi_turn"],
        // The default L2 backend; `l2_models` lists every selectable one.
        "l2_model": MODEL_QWEN3_GUARD,
        "l2_models": [MODEL_QWEN3_GUARD, MODEL_WARDEN_GEN],
    })
    .to_string()
}

/// Python module implemented in Rust.
/// Available as `from agent_sec_cli._native import ...` in Python.
#[pymodule]
fn _native(py: Python, m: &PyModule) -> PyResult<()> {
    // Route Rust `log` records into Python's `logging`.  This is the only
    // place a logger can be installed: the crate ships as a cdylib with no
    // `main`, so without this call the `log` facade discards every record —
    // including the model-service warnings about a non-loopback base URL and
    // about rejected tuning values.
    //
    // Caching is off on purpose.  The cached variants pin each target's Python
    // logger and level on first use and can only be invalidated through the
    // `ResetHandle` returned here, so any later reconfiguration on the Python
    // side — attaching a handler at runtime, or the per-test logging reset in
    // `cli_logging` — would silently keep routing records by stale settings.
    // Holding the handle instead would mean exposing a reset hook back to
    // Python; not worth it for a dependency tree that logs a handful of
    // warnings per invocation.
    //
    // A failing `install` means a logger is already registered (module reload,
    // sub-interpreter); the existing bridge stays valid, so importing must not
    // fail over it.  A failing `new` means `import logging` itself broke, which
    // is worth propagating.
    let _ = pyo3_log::Logger::new(py, pyo3_log::Caching::Nothing)?.install();
    m.add_function(wrap_pyfunction!(scan_prompt_json, m)?)?;
    m.add_function(wrap_pyfunction!(scan_multi_turn_json, m)?)?;
    m.add_function(wrap_pyfunction!(warmup_scanner, m)?)?;
    m.add_function(wrap_pyfunction!(scanner_engine_info, m)?)?;
    Ok(())
}
