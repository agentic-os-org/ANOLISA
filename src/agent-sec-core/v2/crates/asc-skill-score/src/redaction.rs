//! Redact credential literals while preserving the source expressions needed for attribution.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use asc_model_client::remote::RemoteModel;
use regex::{Captures, Regex};
use serde_json::{Value, json};

use crate::bounded_json::{JsonBudget, JsonOutput};
use crate::{Error, Result};

const MAX_ENCODED_DEPTH: usize = 4;
const MAX_ENCODED_BYTES: usize = 1024 * 1024;

static SOURCE_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(?:^|[^A-Za-z0-9_])["']?(?:agent_sec_model_api_key|anthropic_api_key|anthropic_auth_token|openai_api_key|api[_-]?(?:key|token)|access[_-]?token|refresh[_-]?token|session[_-]?token|client[_-]?secret|authorization|proxy[_-]?authorization|cookie|set[_-]?cookie|token|secret|password|passwd)["']?[^\S\r\n\x1c-\x1e\x85\x{2028}\x{2029}]*[:=][^\S\r\n\x1c-\x1e\x85\x{2028}\x{2029}]*"#)
        .expect("fixed source assignment expression must compile")
});

static SHAPES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?s)-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----.*?-----END(?: [A-Z0-9]+)? PRIVATE KEY-----",
        r"(?i)\b(?:proxy-)?authorization[^\S\r\n]*:[^\S\r\n]*[^\r\n]+",
        r"(?i)\b(?:set-cookie|cookie)[^\S\r\n]*:[^\S\r\n]*[^\r\n]+",
        r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{8,}",
        r"\bsk-ant-[A-Za-z0-9_-]{8,}\b",
        r"\bsk-(?:proj-)?[A-Za-z0-9_-]{16,}\b",
        r"\bgh[pousr]_[A-Za-z0-9]{20,}\b",
        r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b",
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b",
        r"(?i)\bhttps?://[^\s/:@]+:[^\s/@]+@",
    ].into_iter().map(|pattern| {
        // All patterns are compile-time constants exercised by the redaction tests.
        Regex::new(pattern).expect("fixed credential expression must compile")
    }).collect()
});

static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    // Keep the leading delimiter in the replacement; Rust regex does not need lookbehind.
    Regex::new(r#"(?i)(^|[^A-Za-z0-9_])(["']?(?:agent_sec_model_api_key|anthropic_api_key|anthropic_auth_token|openai_api_key|api[_-]?(?:key|token)|access[_-]?token|refresh[_-]?token|session[_-]?token|client[_-]?secret|authorization|proxy[_-]?authorization|cookie|set[_-]?cookie|token|secret|password|passwd)["']?[^\S\r\n\x1c-\x1e\x85\x{2028}\x{2029}]*[:=][^\S\r\n\x1c-\x1e\x85\x{2028}\x{2029}]*)(["']?)([^\s,;"'}]{4,})"#)
        .expect("fixed credential assignment expression must compile")
});

pub(crate) fn text(input: &str) -> String {
    let mut result = input.to_owned();
    for pattern in SHAPES.iter() {
        result = pattern.replace_all(&result, "[REDACTED]").into_owned();
    }
    ASSIGNMENT
        .replace_all(&result, |captures: &Captures<'_>| {
            let quote = &captures[3];
            let value = &captures[4];
            let identifier = value.contains('.')
                && value
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.'));
            if value.starts_with("[REDACTED]")
                || (quote.is_empty() && (identifier || value.contains(['(', ')', '[', ']'])))
            {
                captures[0].to_owned()
            } else if quote.is_empty() {
                format!("{}{}\"[REDACTED]\"", &captures[1], &captures[2])
            } else {
                format!("{}{}{}[REDACTED]", &captures[1], &captures[2], quote)
            }
        })
        .into_owned()
}

fn sensitive(key: &str) -> bool {
    let canonical = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    matches!(
        canonical.as_str(),
        "authorization"
            | "proxyauthorization"
            | "cookie"
            | "setcookie"
            | "apikey"
            | "apitoken"
            | "agentsecmodelapikey"
            | "anthropicapikey"
            | "anthropicauthtoken"
            | "openaiapikey"
            | "token"
            | "accesstoken"
            | "refreshtoken"
            | "sessiontoken"
            | "password"
            | "passwd"
            | "secret"
            | "clientsecret"
            | "credential"
            | "credentials"
    )
}

pub(crate) fn value(value: &mut Value) -> Result<()> {
    value_inner(value, 0, None, &mut JsonBudget::default())
}

pub(crate) fn redact_value(value: &mut Value, model: &RemoteModel) -> Result<()> {
    redact_with_budget(value, model, &mut JsonBudget::default())
}

pub(crate) fn redact_with_budget(
    value: &mut Value,
    model: &RemoteModel,
    budget: &mut JsonBudget,
) -> Result<()> {
    value_inner(value, 0, Some(model), budget)
}

fn value_inner(
    value: &mut Value,
    encoded_depth: usize,
    model: Option<&RemoteModel>,
    budget: &mut JsonBudget,
) -> Result<()> {
    match value {
        Value::String(content) => *content = string_value(content, encoded_depth, model, budget)?,
        Value::Array(items) => {
            for item in items {
                value_inner(item, encoded_depth, model, budget)?;
            }
        }
        Value::Object(items) => {
            // Dropping credential-bearing keys avoids collisions with legitimate redacted labels.
            if let Some(model) = model {
                items.retain(|key, _| !model.contains_secret(key.as_bytes()));
            }
            for (key, item) in items {
                if sensitive(key) {
                    *item = Value::String("[REDACTED]".into());
                } else {
                    value_inner(item, encoded_depth, model, budget)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

fn string_value(
    content: &str,
    encoded_depth: usize,
    model: Option<&RemoteModel>,
    budget: &mut JsonBudget,
) -> Result<String> {
    if content.trim_start().starts_with(['{', '[', '"']) {
        if content.len() > MAX_ENCODED_BYTES {
            return Ok("[OMITTED: serialized JSON redaction size limit]".into());
        }
        if let Some(mut decoded) = budget.parse(content)? {
            if encoded_depth >= MAX_ENCODED_DEPTH {
                return Ok("[OMITTED: serialized JSON redaction depth limit]".into());
            }
            value_inner(&mut decoded, encoded_depth + 1, model, budget)?;
            let mut output = JsonOutput::new(MAX_ENCODED_BYTES);
            output.append(&decoded)?;
            return String::from_utf8(output.bytes)
                .map_err(|_| Error::Runtime("invalid serialized JSON text".into()));
        }
    }
    let clean = text(content);
    Ok(match model {
        Some(model) => model.redact_without_growth(&clean),
        None => clean,
    })
}

/// Build an analysis view without changing snapshot bytes or treating a mask as a source literal.
pub(crate) fn source_view(
    files: &mut BTreeMap<String, String>,
    additional: Vec<(String, String)>,
) -> Result<Value> {
    let mut protected = additional;
    protected.extend(source_literals(files));
    let mut values = Vec::<String>::new();
    let mut categories = BTreeMap::<String, BTreeSet<String>>::new();
    for (secret, category) in protected {
        if secret.is_empty() {
            continue;
        }
        if !categories.contains_key(&secret) {
            values.push(secret.clone());
        }
        categories.entry(secret).or_default().insert(category);
    }
    // Counters disclose no secret hash, prefix, suffix or length. Their scope is this source view.
    let namespace_pattern = Regex::new(r"\[REDACTED\]__ASC_OPAQUE_([0-9a-f]{8})_")
        .map_err(|_| Error::Runtime("invalid source masking namespace pattern".into()))?;
    let occupied = files
        .values()
        .flat_map(|content| namespace_pattern.captures_iter(content))
        .filter_map(|capture| u32::from_str_radix(&capture[1], 16).ok())
        .collect::<BTreeSet<_>>();
    let namespace = (0_u32..=u32::MAX)
        .find(|number| !occupied.contains(number))
        .ok_or_else(|| Error::Runtime("source masking namespace exhausted".into()))?;
    let tokens = values
        .iter()
        .enumerate()
        .map(|(index, original)| {
            (
                original.clone(),
                format!("[REDACTED]__ASC_OPAQUE_{namespace:08x}_{index:08x}__"),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut provenance = BTreeMap::<String, BTreeSet<String>>::new();
    if !values.is_empty() {
        // Match once against original text so replacement tokens cannot be recursively rewritten.
        values.sort_by_key(|item| std::cmp::Reverse(item.len()));
        let pattern = Regex::new(
            &values
                .iter()
                .map(|value| regex::escape(value))
                .collect::<Vec<_>>()
                .join("|"),
        )
        .map_err(|_| Error::Runtime("source masking expression exceeds its limit".into()))?;
        for (path, content) in files {
            let cleaned = pattern.replace_all(content, |captures: &Captures<'_>| {
                let found = captures
                    .get(0)
                    .expect("a matched literal has a complete capture");
                let token = &tokens[found.as_str()];
                let line = content[..found.start()]
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1;
                provenance
                    .entry(found.as_str().into())
                    .or_default()
                    .insert(format!("source:{path}:{line}"));
                format!(
                    "{token}{}",
                    "\n".repeat(found.as_str().bytes().filter(|b| *b == b'\n').count())
                )
            });
            *content = cleaned.into_owned();
        }
    }
    let mut markers = tokens
        .iter()
        .filter_map(|(original, marker)| {
            provenance
                .get(original)
                .map(|refs| json!({"marker":marker,"refs":refs,"categories":categories[original]}))
        })
        .collect::<Vec<_>>();
    markers.sort_by(|left, right| left["marker"].as_str().cmp(&right["marker"].as_str()));
    Ok(json!({
        "schema":"agent-sec-source-redaction/v1",
        "view":"sanitized_source_not_execution_bytes",
        "markers":markers,
        "semantics":"Only exact protected source-spelling equality within this source view is retained; language escapes are not evaluated. Tokens in separate views cannot be compared. Length, substring, encoding, ordering, regex and other hidden-value behavior are unknown; never infer a business failure or invent concrete test values from them. Original documented output literals are not inserted masks."
    }))
}

fn source_literals(files: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut protected = Vec::new();
    for content in files.values() {
        for assignment in SOURCE_ASSIGNMENT.find_iter(content) {
            if let Some(literal) = source_literal(&content[assignment.end()..]) {
                protected.push((literal.to_owned(), "credential_literal".into()));
            }
        }
        for shape in SHAPES.iter() {
            protected.extend(
                shape
                    .find_iter(content)
                    .map(|m| (m.as_str().to_owned(), "credential_shape".into())),
            );
        }
    }
    protected
}

fn source_literal(tail: &str) -> Option<&str> {
    if tail.starts_with(['\'', '"']) {
        let quote = tail.as_bytes()[0];
        let triple = tail
            .as_bytes()
            .get(..3)
            .is_some_and(|head| head == [quote; 3]);
        let width = if triple { 3 } else { 1 };
        let mut position = width;
        while position < tail.len() {
            if tail.as_bytes()[position] == b'\\' {
                position += 2;
            } else if tail.as_bytes()[position..].starts_with(&[quote; 3][..width]) {
                return (position - width >= 4).then_some(&tail[width..position]);
            } else {
                position += 1;
            }
        }
        return (tail.len() - width >= 4).then_some(&tail[width..]);
    }
    let literal = tail
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '"' | '\'' | '}'))
        .next()?;
    let expression = literal.contains(['(', ')', '[', ']'])
        || (literal.contains('.')
            && literal
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.')));
    (literal.len() >= 4 && !expression).then_some(literal)
}

/// Reject invented executable values without forbidding opaque references in risk explanations.
pub(crate) fn validate_source_values(probe: &Value, provenance: &Value) -> Result<()> {
    for entry in provenance["markers"].as_array().into_iter().flatten() {
        let Some(marker) = entry["marker"].as_str().filter(|value| !value.is_empty()) else {
            continue;
        };
        for field in [
            "entry_path",
            "input_path",
            "turns",
            "fixtures",
            "expected_result",
            "activation",
            "input_preparation",
            "payload_path",
            "target",
            "observe_paths",
            "mock_responses",
            "required_tools",
            "required_modules",
        ] {
            if probe
                .get(field)
                .is_some_and(|value| value.to_string().contains(marker))
            {
                return Err(Error::Contract(format!(
                    "probe {field} uses an inserted source mask as a concrete value; derive a non-secret documented business oracle or mark planning unsupported"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_expressions_survive_but_literals_do_not() {
        for expression in [
            "Path('/app/guard/token').read_text()",
            "os.environ['TOKEN']",
            "get_secret()",
            "config.token",
        ] {
            let source = format!("token = {expression}\n");
            assert_eq!(text(&source), source);
        }
        assert_eq!(
            text("api_key = concrete-secret-value"),
            "api_key = \"[REDACTED]\""
        );
        assert_eq!(
            text("token = \"literal-secret-value\""),
            "token = \"[REDACTED]\""
        );
        assert_eq!(
            text("if not api_key:\n  stop()"),
            "if not api_key:\n  stop()"
        );
    }

    #[test]
    fn structured_redaction_remains_valid_json_and_idempotent() -> Result<()> {
        let mut evidence = json!({"AGENT_SEC_MODEL_API_KEY":"arbitrary-secret", "headers":{"cookie":"sid=secret"},"code":"token = Path('input').read_text()","n_input_tokens":100});
        value(&mut evidence)?;
        assert_eq!(evidence["headers"]["cookie"], "[REDACTED]");
        assert_eq!(evidence["AGENT_SEC_MODEL_API_KEY"], "[REDACTED]");
        assert_eq!(evidence["code"], "token = Path('input').read_text()");
        let saved = evidence.clone();
        value(&mut evidence)?;
        assert_eq!(evidence, saved);
        let encoded = serde_json::to_string(&evidence)?;
        assert_eq!(serde_json::from_str::<Value>(&encoded)?, evidence);
        Ok(())
    }

    #[test]
    fn source_masks_keep_real_output_literals_and_exact_equality_classes() -> Result<()> {
        let raw = BTreeMap::from([
            ("SKILL.md".into(), "Write the literal [REDACTED].".into()),
            ("a.py".into(), "password = \"SYNTHETIC_PUBLIC_PASSWORD_B04\"\noutput = \"[REDACTED]\"\nassert password not in output\n".into()),
            ("b.py".into(), "token = \"SYNTHETIC_PUBLIC_PASSWORD_B04\"\nsecret = \"OTHER_VALUE\"\n".into()),
        ]);
        let mut files = raw.clone();
        let provenance = source_view(&mut files, Vec::new())?;
        let markers = provenance["markers"].as_array().expect("markers");
        assert_eq!(markers.len(), 2);
        let shared = markers
            .iter()
            .find(|item| item["refs"].as_array().expect("refs").len() == 2)
            .expect("same value in two files");
        let token = shared["marker"].as_str().expect("marker");
        assert!(files["a.py"].contains(token));
        assert!(files["b.py"].contains(token));
        assert!(files["a.py"].contains("output = \"[REDACTED]\""));
        assert_eq!(files["SKILL.md"], raw["SKILL.md"]);
        assert!(
            !files
                .values()
                .any(|value| value.contains("SYNTHETIC_PUBLIC_PASSWORD_B04"))
        );
        assert!(!provenance.to_string().contains("OTHER_VALUE"));
        assert_eq!(
            markers[0]["marker"].as_str().map(str::len),
            markers[1]["marker"].as_str().map(str::len)
        );
        let mut replay = raw;
        assert_eq!(source_view(&mut replay, Vec::new())?, provenance);
        assert_eq!(replay, files);
        Ok(())
    }

    #[test]
    fn an_actual_placeholder_secret_keeps_its_true_equality_with_output() -> Result<()> {
        let mut files = BTreeMap::from([(
            "a.py".into(),
            "password = \"[REDACTED]\"\noutput = \"[REDACTED]\"".into(),
        )]);
        let provenance = source_view(&mut files, Vec::new())?;
        let marker = provenance["markers"][0]["marker"].as_str().expect("marker");
        assert_eq!(files["a.py"].matches(marker).count(), 2);
        Ok(())
    }

    #[test]
    fn original_marker_lookalikes_unicode_and_multiline_keep_provenance() -> Result<()> {
        let original_marker = "[REDACTED]__ASC_OPAQUE_00000000_00000000__";
        let raw = format!(
            "original = '{original_marker}'\npassword = '''多行 secret\nsecond line'''\nprint('done')\n"
        );
        let mut files = BTreeMap::from([("a.py".into(), raw.clone())]);
        let provenance = source_view(&mut files, Vec::new())?;
        let marker = provenance["markers"][0]["marker"].as_str().expect("marker");
        assert_ne!(marker, original_marker);
        assert!(files["a.py"].contains(original_marker));
        assert!(marker.contains("00000001_"));
        assert_eq!(files["a.py"].lines().count(), raw.lines().count());
        assert_eq!(provenance["markers"][0]["refs"], json!(["source:a.py:2"]));
        assert!(!files["a.py"].contains("多行 secret"));
        Ok(())
    }

    #[test]
    fn inserted_values_cannot_become_fixtures_or_business_expectations() -> Result<()> {
        let mut files = BTreeMap::from([("a.py".into(), "password = 'a-private-value'".into())]);
        let provenance = source_view(&mut files, Vec::new())?;
        let marker = provenance["markers"][0]["marker"].as_str().expect("marker");
        for field in [
            "expected_result",
            "turns",
            "fixtures",
            "mock_responses",
            "input_preparation",
        ] {
            let probe = json!({field:{"nested":marker}});
            assert!(validate_source_values(&probe, &provenance).is_err());
        }
        assert!(
            validate_source_values(
                &json!({"expected_result":"Write [REDACTED] to the output file."}),
                &provenance
            )
            .is_ok()
        );
        assert!(validate_source_values(&json!({"reason":marker}), &provenance).is_ok());
        Ok(())
    }

    #[test]
    fn serialized_metadata_has_the_same_sanitized_fields() -> Result<()> {
        let raw = json!({"file":"password = \"NEW_SECRET_QA73\"\nprint('quoted \\\"text\\\"')", "exit_code":0});
        let mut direct = raw.clone();
        value(&mut direct)?;
        for encodings in 1..=3 {
            let mut nested = raw.clone();
            for _ in 0..encodings {
                nested = serde_json::to_string(&nested)?.into();
            }
            value(&mut nested)?;
            assert!(!nested.to_string().contains("NEW_SECRET_QA73"));
            let saved = nested.clone();
            value(&mut nested)?;
            assert_eq!(nested, saved);
            for _ in 0..encodings {
                nested = serde_json::from_str(nested.as_str().expect("encoded string"))?;
            }
            assert_eq!(nested, direct);
        }
        Ok(())
    }

    #[test]
    fn encoded_limits_omit_unknown_duplicates_without_exposing_their_content() -> Result<()> {
        let mut nested = json!({"password":"DO_NOT_EXPOSE_THE_SECRET"});
        for _ in 0..6 {
            nested = serde_json::to_string(&nested)?.into();
        }
        value(&mut nested)?;
        assert!(!nested.to_string().contains("DO_NOT_EXPOSE_THE_SECRET"));
        assert!(nested.to_string().contains("redaction depth limit"));
        let mut oversized = Value::String(format!(
            "{{\"password\":\"{}\"}}",
            "x".repeat(MAX_ENCODED_BYTES)
        ));
        value(&mut oversized)?;
        assert_eq!(oversized, "[OMITTED: serialized JSON redaction size limit]");
        Ok(())
    }
}
