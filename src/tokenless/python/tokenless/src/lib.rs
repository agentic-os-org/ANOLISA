//! Python bindings for the in-process Tokenless runtime.

use std::path::{Path, PathBuf};

use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyValueError};
use pyo3::prelude::*;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokenless_protocol::{
    Attribution as ProtocolAttribution, BeforeModelRequest, PostToolRequest, PreToolRequest,
    RetrieveRequest,
};
use tokenless_runtime::{
    Attribution, CompressOptions, CompressResult, RuntimeConfig, TokenlessRuntime as NativeRuntime,
};
use tokenless_stats::{
    DiffSort, StatsRecord, StatsRecorder, ensure_state_dir, format_compare_json,
    format_summary_json, get_home_dir, record_report, resolve_data_dir, session_report,
    tool_use_report, validate_data_dir, validate_database_path,
};

create_exception!(_native, TokenlessError, PyException);

/// Python view of one structured compression result.
#[pyclass(name = "CompressionResult", frozen, get_all)]
struct PyCompressionResult {
    output: String,
    compressed_output: String,
    disposition: String,
    applied: bool,
    before_tokens: usize,
    after_tokens: usize,
    stash_writes: Option<usize>,
    stash_errors: Option<usize>,
    unrecoverable_truncations: Option<usize>,
    stash_size: Option<usize>,
}

impl From<CompressResult> for PyCompressionResult {
    fn from(result: CompressResult) -> Self {
        let disposition = result.disposition.wire_str().to_string();
        let applied = result.applied();
        Self {
            output: result.output,
            compressed_output: result.compressed_output,
            disposition,
            applied,
            before_tokens: result.before_tokens,
            after_tokens: result.after_tokens,
            stash_writes: result.stash_writes,
            stash_errors: result.stash_errors,
            unrecoverable_truncations: result.unrecoverable_truncations,
            stash_size: result.stash_size,
        }
    }
}

/// Reusable in-process Tokenless runtime exposed to Python.
#[pyclass(name = "TokenlessRuntime")]
struct PyTokenlessRuntime {
    inner: NativeRuntime,
}

/// Native query handle used by the typed Python statistics client.
#[pyclass(name = "_StatsQuery")]
struct PyStatsQuery {
    data_dir: PathBuf,
    database_path: PathBuf,
    recorder: Option<StatsRecorder>,
    error: Option<String>,
}

#[pymethods]
impl PyStatsQuery {
    #[new]
    fn new(data_dir: Option<PathBuf>) -> PyResult<Self> {
        let data_dir = match data_dir {
            Some(path) => validate_data_dir(&path).map_err(to_value_error)?,
            None => {
                let home = get_home_dir();
                let home = (!home.is_empty()).then(|| Path::new(&home));
                let override_path = std::env::var("TOKENLESS_DATA_DIR").ok();
                resolve_data_dir(home, override_path.as_deref()).map_err(to_python_error_message)?
            }
        };
        let database_path = data_dir.join("stats.db");

        let open_result = ensure_state_dir(&data_dir)
            .map_err(|error| format!("cannot create stats directory: {error}"))
            .and_then(|()| {
                validate_database_path(&database_path, &[&data_dir])
                    .map_err(|error| error.to_string())
            })
            .and_then(|path| StatsRecorder::new(path).map_err(|error| error.to_string()));
        let (recorder, error) = match open_result {
            Ok(recorder) => (Some(recorder), None),
            Err(error) => (None, Some(error)),
        };

        Ok(Self {
            data_dir,
            database_path,
            recorder,
            error,
        })
    }

    fn status_json(&self, py: Python<'_>) -> PyResult<String> {
        let records = match self.recorder.as_ref() {
            Some(recorder) => Some(
                py.allow_threads(|| recorder.count())
                    .map_err(to_stats_error)?,
            ),
            None => None,
        };
        Ok(serde_json::json!({
            "data_dir": self.data_dir.to_string_lossy(),
            "database_path": self.database_path.to_string_lossy(),
            "available": self.recorder.is_some(),
            "error": self.error,
            "records": records,
        })
        .to_string())
    }

    #[pyo3(signature = (limit=None))]
    fn summary_json(&self, py: Python<'_>, limit: Option<usize>) -> PyResult<String> {
        let recorder = self.require_recorder()?;
        let (records, retrieve) = py
            .allow_threads(|| {
                let records = recorder.all_records(limit)?;
                // Fail-soft: a broken retrieve aggregate degrades the report,
                // never the query.
                let retrieve = recorder.retrieve_totals().ok();
                Ok((records, retrieve))
            })
            .map_err(to_stats_error)?;
        Ok(format_summary_json(&records, None, retrieve.as_ref()))
    }

    #[pyo3(signature = (limit=20))]
    fn list_json(&self, py: Python<'_>, limit: usize) -> PyResult<String> {
        let recorder = self.require_recorder()?;
        let records = py
            .allow_threads(|| recorder.all_records(Some(limit)))
            .map_err(to_stats_error)?;
        let records = records.iter().map(record_metadata_json).collect::<Vec<_>>();
        serde_json::to_string(&records).map_err(to_json_error)
    }

    fn show_json(&self, py: Python<'_>, record_id: i64) -> PyResult<Option<String>> {
        let recorder = self.require_recorder()?;
        let record = py
            .allow_threads(|| recorder.record_by_id(record_id))
            .map_err(to_stats_error)?;
        record
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(to_json_error)
    }

    #[pyo3(signature = (
        *,
        record_id=None,
        session_id=None,
        tool_use_id=None,
        limit=20,
        sort="saved",
        context=3
    ))]
    #[allow(clippy::too_many_arguments)]
    fn diff_json(
        &self,
        py: Python<'_>,
        record_id: Option<i64>,
        session_id: Option<String>,
        tool_use_id: Option<String>,
        limit: usize,
        sort: &str,
        context: usize,
    ) -> PyResult<Option<String>> {
        let recorder = self.require_recorder()?;
        let report = match (record_id, session_id) {
            (Some(record_id), None) => {
                let record = py
                    .allow_threads(|| recorder.record_by_id(record_id))
                    .map_err(to_stats_error)?;
                record.map(|record| record_report(&record, context))
            }
            (None, Some(session_id)) => {
                let records = py
                    .allow_threads(|| {
                        recorder.records_for_diff(&session_id, tool_use_id.as_deref())
                    })
                    .map_err(to_stats_error)?;
                if records.is_empty() {
                    None
                } else if let Some(tool_use_id) = tool_use_id {
                    Some(tool_use_report(
                        &records,
                        &session_id,
                        &tool_use_id,
                        context,
                    ))
                } else {
                    let sort = match sort {
                        "saved" => DiffSort::Saved,
                        "time" => DiffSort::Time,
                        _ => {
                            return Err(PyValueError::new_err(
                                "sort must be either 'saved' or 'time'",
                            ));
                        }
                    };
                    Some(session_report(&records, &session_id, limit, sort))
                }
            }
            _ => {
                return Err(PyValueError::new_err(
                    "specify exactly one of record_id or session_id",
                ));
            }
        };
        report
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(to_json_error)
    }

    #[pyo3(signature = (baseline_session_id, tokenless_session_id, limit=None))]
    fn compare_json(
        &self,
        py: Python<'_>,
        baseline_session_id: &str,
        tokenless_session_id: &str,
        limit: Option<usize>,
    ) -> PyResult<String> {
        let recorder = self.require_recorder()?;
        let (baseline, tokenless) = py
            .allow_threads(|| {
                let baseline = recorder.records_by_session(baseline_session_id, limit)?;
                let tokenless = recorder.records_by_session(tokenless_session_id, limit)?;
                Ok::<_, tokenless_stats::StatsError>((baseline, tokenless))
            })
            .map_err(to_stats_error)?;
        let mut missing_sessions = Vec::new();
        if baseline.is_empty() {
            missing_sessions.push("baseline");
        }
        if tokenless.is_empty() {
            missing_sessions.push("tokenless");
        }
        if !missing_sessions.is_empty() {
            return Ok(serde_json::json!({ "missing_sessions": missing_sessions }).to_string());
        }
        Ok(format_compare_json(&baseline, &tokenless))
    }
}

impl PyStatsQuery {
    fn require_recorder(&self) -> PyResult<&StatsRecorder> {
        self.recorder.as_ref().ok_or_else(|| {
            TokenlessError::new_err(
                self.error
                    .as_deref()
                    .unwrap_or("statistics database is unavailable")
                    .to_string(),
            )
        })
    }
}

#[pymethods]
impl PyTokenlessRuntime {
    #[new]
    #[pyo3(signature = (
        data_dir=None,
        *,
        compression_enabled=true,
        search_path_sharing_enabled=true,
        diff_compression_enabled=false,
        html_extraction_enabled=false,
        stats_enabled=true,
        sls_enabled=false
    ))]
    fn new(
        data_dir: Option<PathBuf>,
        compression_enabled: bool,
        search_path_sharing_enabled: bool,
        diff_compression_enabled: bool,
        html_extraction_enabled: bool,
        stats_enabled: bool,
        sls_enabled: bool,
    ) -> PyResult<Self> {
        let inner = NativeRuntime::new(RuntimeConfig {
            data_dir,
            stats_enabled,
            sls_enabled,
            compression_enabled,
            search_path_sharing_enabled,
            diff_compression_enabled,
            html_extraction_enabled,
        })
        .map_err(to_python_error)?;
        Ok(Self { inner })
    }

    #[pyo3(signature = (
        request_json,
        *,
        agent_id="python",
        session_id=None,
        tool_use_id=None
    ))]
    fn _before_model_json(
        &self,
        py: Python<'_>,
        request_json: &str,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
    ) -> PyResult<String> {
        let request = decode_lifecycle_payload::<BeforeModelRequest>(request_json)?;
        let attribution = protocol_attribution(agent_id, session_id, tool_use_id);
        py.allow_threads(|| {
            self.inner
                .before_model(&request, &attribution)
                .map_err(to_python_error)
                .and_then(encode_lifecycle_payload)
        })
    }

    #[pyo3(signature = (
        request_json,
        rtk_path,
        *,
        agent_id="python",
        session_id=None,
        tool_use_id=None
    ))]
    fn _pre_tool_json(
        &self,
        py: Python<'_>,
        request_json: &str,
        rtk_path: PathBuf,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
    ) -> PyResult<String> {
        let request = decode_lifecycle_payload::<PreToolRequest>(request_json)?;
        let attribution = protocol_attribution(agent_id, session_id, tool_use_id);
        py.allow_threads(|| {
            self.inner
                .pre_tool(&request, &attribution, &rtk_path)
                .map_err(to_python_error)
                .and_then(encode_lifecycle_payload)
        })
    }

    #[pyo3(signature = (
        request_json,
        *,
        agent_id="python",
        session_id=None,
        tool_use_id=None
    ))]
    fn _post_tool_json(
        &self,
        py: Python<'_>,
        request_json: &str,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
    ) -> PyResult<String> {
        let request = decode_lifecycle_payload::<PostToolRequest>(request_json)?;
        let attribution = protocol_attribution(agent_id, session_id, tool_use_id);
        py.allow_threads(|| {
            self.inner
                .post_tool(&request, &attribution)
                .map_err(to_python_error)
                .and_then(encode_lifecycle_payload)
        })
    }

    #[pyo3(signature = (
        request_json,
        *,
        agent_id="python",
        session_id=None,
        tool_use_id=None
    ))]
    fn _retrieve_authorized_json(
        &self,
        py: Python<'_>,
        request_json: &str,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
    ) -> PyResult<String> {
        let request = decode_lifecycle_payload::<RetrieveRequest>(request_json)?;
        let attribution = protocol_attribution(agent_id, session_id, tool_use_id);
        py.allow_threads(|| {
            self.inner
                .retrieve_authorized(&request, &attribution)
                .map_err(to_python_error)
                .and_then(encode_lifecycle_payload)
        })
    }

    #[pyo3(signature = (
        input,
        *,
        truncate_strings_at=None,
        truncate_arrays_at=None,
        max_depth=None,
        agent_id="python",
        session_id=None,
        tool_use_id=None,
        stash_enabled=true,
        require_reversible=true
    ))]
    #[allow(clippy::too_many_arguments)]
    fn compress_response(
        &self,
        py: Python<'_>,
        input: String,
        truncate_strings_at: Option<usize>,
        truncate_arrays_at: Option<usize>,
        max_depth: Option<usize>,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
        stash_enabled: bool,
        require_reversible: bool,
    ) -> PyResult<PyCompressionResult> {
        let options = CompressOptions {
            truncate_strings_at,
            truncate_arrays_at,
            array_tail_preserve: None,
            max_depth,
            stash_enabled,
            require_reversible,
        };
        let attribution = Attribution {
            agent_id: agent_id.to_string(),
            session_id,
            tool_use_id,
        };
        py.allow_threads(|| {
            self.inner
                .compress_response(&input, &options, &attribution)
                .map(PyCompressionResult::from)
                .map_err(to_python_error)
        })
    }

    #[pyo3(signature = (
        input,
        *,
        agent_id="python",
        session_id=None,
        tool_use_id=None
    ))]
    fn compress_schema(
        &self,
        py: Python<'_>,
        input: String,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
    ) -> PyResult<PyCompressionResult> {
        let attribution = Attribution {
            agent_id: agent_id.to_string(),
            session_id,
            tool_use_id,
        };
        py.allow_threads(|| {
            self.inner
                .compress_schema(&input, &attribution)
                .map(PyCompressionResult::from)
                .map_err(to_python_error)
        })
    }

    #[pyo3(signature = (
        input,
        *,
        agent_id="python",
        session_id=None,
        tool_use_id=None
    ))]
    fn compress_toon(
        &self,
        py: Python<'_>,
        input: String,
        agent_id: &str,
        session_id: Option<String>,
        tool_use_id: Option<String>,
    ) -> PyResult<PyCompressionResult> {
        let attribution = Attribution {
            agent_id: agent_id.to_string(),
            session_id,
            tool_use_id,
        };
        py.allow_threads(|| {
            self.inner
                .compress_toon(&input, &attribution)
                .map(PyCompressionResult::from)
                .map_err(to_python_error)
        })
    }

    fn retrieve(&self, py: Python<'_>, hash_or_marker: String) -> PyResult<String> {
        py.allow_threads(|| {
            self.inner
                .retrieve(&hash_or_marker)
                .map_err(to_python_error)
        })
    }

    #[getter]
    fn data_dir(&self) -> String {
        self.inner.data_dir().to_string_lossy().into_owned()
    }

    #[getter]
    fn stash_available(&self) -> bool {
        self.inner.stash_available()
    }

    #[getter]
    fn stash_error(&self) -> Option<String> {
        self.inner.stash_error().map(str::to_string)
    }

    #[getter]
    fn stats_available(&self) -> bool {
        self.inner.stats_available()
    }

    #[getter]
    fn stats_error(&self) -> Option<String> {
        self.inner.stats_error().map(str::to_string)
    }
}

fn protocol_attribution(
    agent_id: &str,
    session_id: Option<String>,
    tool_use_id: Option<String>,
) -> ProtocolAttribution {
    ProtocolAttribution {
        agent_id: agent_id.to_owned(),
        session_id,
        tool_use_id,
    }
}

fn decode_lifecycle_payload<T: DeserializeOwned>(input: &str) -> PyResult<T> {
    serde_json::from_str(input)
        .map_err(|error| PyValueError::new_err(format!("invalid lifecycle payload: {error}")))
}

fn encode_lifecycle_payload<T: Serialize>(value: T) -> PyResult<String> {
    serde_json::to_string(&value).map_err(to_json_error)
}

fn to_python_error(error: tokenless_runtime::RuntimeError) -> PyErr {
    TokenlessError::new_err(error.to_string())
}

fn to_python_error_message(error: impl std::fmt::Display) -> PyErr {
    TokenlessError::new_err(error.to_string())
}

fn to_value_error(error: impl std::fmt::Display) -> PyErr {
    PyValueError::new_err(error.to_string())
}

fn to_stats_error(error: tokenless_stats::StatsError) -> PyErr {
    TokenlessError::new_err(error.to_string())
}

fn to_json_error(error: serde_json::Error) -> PyErr {
    TokenlessError::new_err(format!("cannot serialize statistics result: {error}"))
}

fn record_metadata_json(record: &StatsRecord) -> serde_json::Value {
    serde_json::json!({
        "id": record.id,
        "timestamp": record.timestamp,
        "operation": record.operation,
        "agent_id": record.agent_id,
        "source_pid": record.source_pid,
        "session_id": record.session_id,
        "tool_use_id": record.tool_use_id,
        "before_chars": record.before_chars,
        "before_tokens": record.before_tokens,
        "after_chars": record.after_chars,
        "after_tokens": record.after_tokens,
        "before_text": null,
        "after_text": null,
        "before_output": null,
        "after_output": null,
        "mode": record.mode,
        "stash_writes": record.stash_writes,
        "stash_errors": record.stash_errors,
        "stash_size": record.stash_size,
        "content_type": record.content_type,
        "content_origin": record.content_origin,
        "applied_operations": record.applied_operations,
        "recoverability": record.recoverability,
        "tokenizer_id": record.tokenizer_id,
        "unrecoverable_truncations": record.unrecoverable_truncations,
    })
}

/// Register the native Tokenless module.
#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyTokenlessRuntime>()?;
    module.add_class::<PyCompressionResult>()?;
    module.add_class::<PyStatsQuery>()?;
    module.add("TokenlessError", module.py().get_type::<TokenlessError>())?;
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokenless_stats::OperationType;

    /// `show` serializes the record; `list` builds its payload by hand. A
    /// field added to [`StatsRecord`] therefore reads as `None` through
    /// `list` until it is added here too — silently, because the Python
    /// parser uses `.get()`. Compare the shapes rather than any one field.
    #[test]
    fn the_list_payload_carries_every_field_show_does() {
        let record = StatsRecord::new(
            OperationType::CompressResponse,
            "cli".into(),
            100,
            40,
            50,
            20,
        );
        let mut shown: Vec<String> = serde_json::to_value(&record)
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        let mut listed: Vec<String> = record_metadata_json(&record)
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        shown.sort();
        listed.sort();
        assert_eq!(listed, shown);
    }
}
