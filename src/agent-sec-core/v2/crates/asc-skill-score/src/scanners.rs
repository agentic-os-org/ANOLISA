//! Reuse local scanner engines as audit signals; only the Judge and scoring rules make deductions.

use std::collections::BTreeMap;

use asc_capability_code_scan::Language;
use asc_capability_pii_scan::{PiiScanOptions, PiiScanReport, PiiScanner};
use asc_capability_prompt_scan::{PromptScanner, ScanConfig, ScanMode};
use asc_model_client::remote::RemoteModel;
use serde_json::{Value, json};

use crate::{
    Error, Result,
    contracts::{array, text},
};

pub(crate) fn scan(
    files: &mut BTreeMap<String, String>,
    model: &RemoteModel,
    source: bool,
) -> Result<Value> {
    if files
        .keys()
        .any(|reference| model.contains_secret(reference.as_bytes()))
    {
        return Err(Error::Contract(
            "source or evidence reference contains a resolved credential".into(),
        ));
    }
    let pii = PiiScanner::new().map_err(|e| Error::Runtime(e.to_string()))?;
    let prompt = PromptScanner::new(ScanConfig::preset(ScanMode::Fast))
        .map_err(|e| Error::Runtime(e.to_string()))?;
    let masb = crate::masb::Detector::new()?;
    let mut signals = Vec::new();
    let source_redaction = if source {
        review_source(files, model, &pii, &mut signals)?
    } else {
        Value::Null
    };
    for (reference, content) in files {
        if !source {
            let mut value = Value::String(content.clone());
            crate::redaction::redact_value(&mut value, model)?;
            value.as_str().unwrap_or_default().clone_into(content);
        }
        if content.trim().is_empty() {
            continue;
        }
        if source {
            signals.extend(masb.scan(reference, content));
        }
        if !source
            && reference.starts_with("event:")
            && let Ok(event) = serde_json::from_str::<Value>(content)
            && matches!(text(&event, "kind"), "file" | "process_exit")
        {
            continue;
        }
        if !source {
            let pii_result = scan_pii(&pii, content)?;
            for hit in &pii_result.findings {
                signals.push(json!({"scanner":"pii","ref":reference,"detail":hit}));
            }
            if let Some(redacted) = pii_result.redacted_text {
                *content = redacted;
            }
        }
        let result = prompt
            .scan(content, Some(reference))
            .map_err(|e| Error::Runtime(e.to_string()))?
            .to_json_value();
        if !matches!(text(&result, "verdict"), "pass" | "warn" | "deny")
            || result["degraded"] != false
            || result["input_truncated"] != false
            || !array(&result, "layers_failed").is_empty()
            || !result["findings"].is_array()
        {
            return Err(Error::Runtime(
                "prompt scanner failed or returned incomplete coverage".into(),
            ));
        }
        for hit in array(&result, "findings") {
            signals.push(json!({"scanner":"prompt","ref":reference,"detail":hit}));
        }
        if let Some(language) = code_language(reference) {
            let result = asc_capability_code_scan::scan(content, language, None, "regex");
            if !result.ok {
                return Err(Error::Runtime(result.summary));
            }
            for hit in result.findings {
                signals.push(json!({"scanner":"code","ref":reference,"detail":hit}));
            }
        }
    }
    let mut result = json!({"signals":signals,"scanners":["prompt:fast","code:regex","pii:local","masb:frozen"]});
    result["source_redaction"] = source_redaction;
    crate::redaction::redact_value(&mut result, model)?;
    Ok(result)
}

fn code_language(reference: &str) -> Option<Language> {
    if reference
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension == "py")
        || reference.contains(":code-python-")
    {
        Some(Language::Python)
    } else if reference
        .rsplit_once('.')
        .is_some_and(|(_, extension)| extension == "sh")
        || reference.contains(":code-bash-")
    {
        Some(Language::Bash)
    } else {
        None
    }
}

fn scan_pii(pii: &PiiScanner, content: &str) -> Result<PiiScanReport> {
    let result = pii
        .scan(
            content,
            &PiiScanOptions {
                redact_output: true,
                ..PiiScanOptions::default()
            },
        )
        .map_err(|e| Error::Runtime(e.to_string()))?;
    if !result.ok
        || result.summary.truncated
        || result.summary.coverage.status != asc_capability_pii_scan::CoverageStatus::Complete
        || result.summary.execution_status != asc_capability_pii_scan::ScanStatus::Completed
    {
        return Err(Error::Runtime(
            "PII scanner returned incomplete coverage".into(),
        ));
    }
    Ok(result)
}

fn review_source(
    files: &mut BTreeMap<String, String>,
    model: &RemoteModel,
    pii: &PiiScanner,
    signals: &mut Vec<Value>,
) -> Result<Value> {
    let mut protected = Vec::new();
    for (reference, content) in files.iter() {
        let result = scan_pii(pii, content)?;
        if result.summary.findings_truncated {
            return Err(Error::Runtime(
                "source PII findings exceeded safe redaction coverage".into(),
            ));
        }
        let offsets = content
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(content.len()))
            .collect::<Vec<_>>();
        for hit in &result.findings {
            let start = *offsets
                .get(hit.span.start)
                .ok_or_else(|| Error::Runtime("PII source span outside input".into()))?;
            let end = *offsets
                .get(hit.span.end)
                .ok_or_else(|| Error::Runtime("PII source span outside input".into()))?;
            let candidate = content
                .get(start..end)
                .ok_or_else(|| Error::Runtime("PII source span reversed".into()))?;
            if source_expression(&hit.pii_type, candidate) {
                continue;
            }
            let literal = if candidate.len() >= 2
                && matches!(candidate.as_bytes()[0], b'\'' | b'"')
                && candidate.as_bytes().first() == candidate.as_bytes().last()
            {
                &candidate[1..candidate.len() - 1]
            } else {
                candidate
            };
            protected.push((literal.to_owned(), format!("pii:{}", hit.pii_type)));
            // The opaque source view must not regain secret fragments or lengths through signals.
            let line = content[..start].bytes().filter(|b| *b == b'\n').count() + 1;
            signals.push(json!({"scanner":"pii","ref":format!("source:{reference}:{line}"),
                "detail":{"type":hit.pii_type,"category":hit.category,"severity":hit.severity,
                "confidence":hit.confidence,"evidence":"protected source value; see source_redaction"}}));
        }
        if let Some(secret) = model_secret(content, model) {
            protected.push((secret.into(), "controller_credential".into()));
        }
    }
    crate::redaction::source_view(files, protected)
}

fn source_expression(kind: &str, candidate: &str) -> bool {
    matches!(
        kind,
        "generic_secret_field" | "api_key" | "aliyun_access_key_secret"
    ) && (candidate.contains(['(', ')', '[', ']'])
        || (candidate.contains('.')
            && candidate
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'))))
}

fn model_secret<'a>(content: &'a str, model: &RemoteModel) -> Option<&'a str> {
    let bytes = content.as_bytes();
    if !model.contains_secret(bytes) {
        return None;
    }
    // Locate the configured exact needle through the existing non-disclosing model API.
    let (mut low, mut high) = (1, bytes.len());
    while low < high {
        let middle = low + (high - low) / 2;
        if model.contains_secret(&bytes[..middle]) {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let end = low;
    (low, high) = (0, end);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if model.contains_secret(&bytes[middle..end]) {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    content.get(low..end)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_redaction_keeps_security_relevant_expressions_and_removes_pii() -> Result<()> {
        let model = RemoteModel::resolve(
            std::path::Path::new("/nonexistent/score-test"),
            &BTreeMap::from([
                ("ANTHROPIC_API_KEY".into(), "local-test-model-secret".into()),
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://dashscope.aliyuncs.com/apps/anthropic".into(),
                ),
            ]),
        )?;
        let source = "token = Path('/app/guard/token').read_text()\nother = os.environ['TOKEN']\nphone = '13812345678'\n";
        let mut files = BTreeMap::from([("run.py".into(), source.into())]);
        let _ = scan(&mut files, &model, true)?;
        let clean = &files["run.py"];
        assert!(clean.contains("token = Path('/app/guard/token').read_text()"));
        assert!(clean.contains("os.environ['TOKEN']"));
        assert!(!clean.contains("13812345678"));
        assert_eq!(clean.lines().count(), source.lines().count());
        Ok(())
    }

    #[test]
    fn review_masks_do_not_rewrite_execution_files_or_collide_with_documented_output() -> Result<()>
    {
        let model = RemoteModel::resolve(
            std::path::Path::new("/nonexistent/score-test"),
            &BTreeMap::from([
                ("ANTHROPIC_API_KEY".into(), "local-test-model-secret".into()),
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://dashscope.aliyuncs.com/apps/anthropic".into(),
                ),
            ]),
        )?;
        let directory = tempfile::tempdir()?;
        let raw = "password = \"SYNTHETIC_PUBLIC_PASSWORD_B04\"\noutput = \"[REDACTED]\"\nassert password not in output\n";
        std::fs::write(
            directory.path().join("SKILL.md"),
            "Write [REDACTED] to output.",
        )?;
        std::fs::write(directory.path().join("redact.py"), raw)?;
        let mut snapshot = crate::source::Snapshot::create(directory.path())?;
        let original_hash = snapshot.hash.clone();
        let result = scan(&mut snapshot.files, &model, true)?;
        let markers = result["source_redaction"]["markers"]
            .as_array()
            .expect("markers");
        assert!(!markers.is_empty());
        assert!(snapshot.files["redact.py"].contains("output = \"[REDACTED]\""));
        assert!(!snapshot.files["redact.py"].contains("SYNTHETIC_PUBLIC_PASSWORD_B04"));
        assert_eq!(
            std::fs::read_to_string(snapshot.path().join("redact.py"))?,
            raw
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("redact.py"))?,
            raw
        );
        assert_eq!(
            crate::source::Snapshot::create(directory.path())?.hash,
            original_hash
        );
        let mut replay = crate::source::Snapshot::create(directory.path())?;
        assert_eq!(
            scan(&mut replay.files, &model, true)?["source_redaction"],
            result["source_redaction"]
        );
        Ok(())
    }

    #[test]
    fn configured_model_credential_is_opaque_even_without_a_credential_shape() -> Result<()> {
        let secret = "configured-value-你好-42";
        let model = RemoteModel::resolve(
            std::path::Path::new("/nonexistent/score-test"),
            &BTreeMap::from([
                ("ANTHROPIC_API_KEY".into(), secret.into()),
                (
                    "ANTHROPIC_BASE_URL".into(),
                    "https://dashscope.aliyuncs.com/apps/anthropic".into(),
                ),
            ]),
        )?;
        assert_eq!(
            model_secret(&format!("first {secret} second {secret}"), &model),
            Some(secret)
        );
        let mut files = BTreeMap::from([(
            "SKILL.md".into(),
            format!("The original [REDACTED] differs from {secret}."),
        )]);
        let result = scan(&mut files, &model, true)?;
        assert!(!files["SKILL.md"].contains(secret));
        assert!(files["SKILL.md"].contains("original [REDACTED] differs"));
        assert!(!result.to_string().contains(secret));
        Ok(())
    }
}
