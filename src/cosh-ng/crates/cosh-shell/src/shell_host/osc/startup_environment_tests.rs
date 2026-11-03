use std::sync::{Arc, Mutex};

use super::super::super::model::ShellStartupPathObserver;
use super::super::OscParser;

const TOKEN: &str = "startup-environment-token";
const SESSION: &str = "startup-environment-session";

struct ObservedParser {
    parser: OscParser,
    observed: Arc<Mutex<Vec<Option<String>>>>,
    _dir: tempfile::TempDir,
}

impl ObservedParser {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("output ref dir");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&observed);
        let parser = OscParser::new(
            SESSION.to_string(),
            dir.path().to_path_buf(),
            TOKEN.to_string(),
        )
        .with_startup_path_observer(ShellStartupPathObserver::new(move |path| {
            sink.lock().expect("observed paths").push(path);
        }));
        Self {
            parser,
            observed,
            _dir: dir,
        }
    }

    fn feed(&mut self, marker: serde_json::Value) {
        let bytes = format!("\x1b]1337;COSH;{marker}\x07");
        self.parser.feed(bytes.as_bytes()).expect("feed marker");
    }

    fn report(&mut self, path: &str) {
        self.feed(serde_json::json!({
            "event": "startup_environment",
            "token": TOKEN,
            "session_id": SESSION,
            "path": path,
        }));
    }

    fn observed(&self) -> Vec<Option<String>> {
        self.observed.lock().expect("observed paths").clone()
    }
}

#[test]
fn first_valid_startup_report_is_observed_once_and_normalized() {
    let mut parser = ObservedParser::new();

    parser.report("/profile/bin:relative:/usr/bin:/usr/bin/:/bin");
    parser.report("/second/report");

    assert_eq!(
        parser.observed(),
        vec![Some("/profile/bin:/usr/bin:/bin".to_string())]
    );
}

#[test]
fn startup_report_requires_the_session_token_and_session_id() {
    let mut parser = ObservedParser::new();

    for marker in [
        serde_json::json!({"event": "startup_environment", "token": "forged",
            "session_id": SESSION, "path": "/forged"}),
        serde_json::json!({"event": "startup_environment", "token": TOKEN, "path": "/forged"}),
        serde_json::json!({"event": "startup_environment", "token": TOKEN,
            "session_id": "other-session", "path": "/forged"}),
    ] {
        parser.feed(marker);
    }
    parser.report("/trusted");

    assert_eq!(parser.observed(), vec![Some("/trusted".to_string())]);
}

#[test]
fn malformed_startup_paths_are_rejected_without_closing_the_report() {
    let mut parser = ObservedParser::new();
    let oversized = format!("/{}", "x".repeat(8 * 1024));

    for path in [
        oversized.as_str(),
        "/bin\n/injected",
        "relative:also-relative",
        "",
    ] {
        parser.report(path);
    }
    parser.feed(
        serde_json::json!({"event": "startup_environment", "token": TOKEN,
        "session_id": SESSION}),
    );
    parser.report("/valid");

    assert_eq!(parser.observed(), vec![Some("/valid".to_string())]);
}

#[test]
fn startup_report_after_the_first_prompt_is_ignored() {
    let mut parser = ObservedParser::new();

    parser.feed(serde_json::json!({"e": "p", "t": TOKEN, "c": "/tmp", "pc": "/tmp", "s": 0}));
    parser.report("/late");

    assert_eq!(parser.observed(), vec![None]);
}
