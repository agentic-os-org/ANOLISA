//! `cosh-shell doctor`: non-interactive on-demand health check.
//!
//! Loads config, runs the shared doctor report (resource + environment
//! checks), prints a plain-text summary to stdout, and returns the stable
//! exit code (healthy=0, warning=1, error=2). Never starts a PTY.

use std::io::Write;

use crate::config::{
    detect_language_from_env, load_config, parse_language_setting, resolve_language_setting,
};
use crate::diagnostics::doctor::{format_doctor_report_plain, run_doctor_report_from_process_cwd};
use crate::diagnostics::health::report_exit_code;
use crate::I18n;

pub(crate) fn run_doctor() -> i32 {
    let config = load_config();
    let language = parse_language_setting(&config.language)
        .map(resolve_language_setting)
        .unwrap_or_else(detect_language_from_env);
    let i18n = I18n::new(language);

    let report = run_doctor_report_from_process_cwd(&config);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in format_doctor_report_plain(&report, i18n) {
        let _ = writeln!(out, "{line}");
    }
    let _ = out.flush();

    report_exit_code(&report)
}
