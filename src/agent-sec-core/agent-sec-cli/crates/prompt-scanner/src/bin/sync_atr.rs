//! ATR → internal rule-pack converter (build-time adapter).
//!
//! Reads a local checkout of Agent-Threat-Rule/agent-threat-rules, keeps
//! stable LLM-input rules, validates every regex the way the engine
//! compiles it, and writes deterministic pack YAML plus an UPSTREAM.toml
//! sync report. Never runs at scan time.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::RegexBuilder;
use serde::{Deserialize, Serialize};

/// ATR category directory → (output file stem, internal category value).
const CATEGORIES: &[(&str, &str)] = &[
    ("prompt-injection", "prompt_injection"),
    ("agent-manipulation", "agent_manipulation"),
    ("context-exfiltration", "context_exfiltration"),
];

/// Top-level scan_target whitelist, from the ATR v3.5.12 corpus as
/// measured (not the schema): the schema enum is mcp/skill/both/runtime,
/// but stable rules universally omit the top-level field — serde defaults
/// it to the empty string, hence "" is whitelisted. Observed input-surface
/// values (mcp/both/llm_io/llm) stay; skill/runtime/tool_* surfaces are
/// excluded. Re-verify on every upstream bump.
const SCAN_TARGETS: &[&str] = &["", "mcp", "both", "llm_io", "llm"];

/// Pattern-level condition fields carrying user prompt input. Conditions
/// on other fields (tool_response, agent_output, tool_args, ...) address
/// non-input surfaces and are skipped with a reported reason.
const CONTENT_FIELDS: &[&str] = &["user_input", "content"];

const UPSTREAM_URL: &str = "https://github.com/Agent-Threat-Rule/agent-threat-rules";

/// The "any character including newline" idiom upstream rule packs use.
const ANY_CHAR_CLASS: &str = r"[\s\S]";

/// The cheap-to-compile spelling the engine rewrites it to.
const ANY_CHAR_INLINE_DOT: &str = r"(?s:.)";

// ---- upstream (ATR) shapes: subset we consume --------------------------

#[derive(Debug, Deserialize)]
struct AtrRule {
    id: String,
    title: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    maturity: String,
    severity: String,
    #[serde(default)]
    scan_target: String,
    #[serde(default)]
    description: String,
    detection: AtrDetection,
    #[serde(default)]
    references: serde_yaml::Value,
    #[serde(default)]
    test_cases: AtrTestCases,
}

#[derive(Debug, Deserialize)]
struct AtrDetection {
    #[serde(default = "default_any")]
    condition: String,
    #[serde(default)]
    conditions: Vec<AtrCondition>,
}

fn default_any() -> String {
    "any".to_string()
}

#[derive(Debug, Deserialize)]
struct AtrCondition {
    #[serde(default)]
    field: String,
    #[serde(default)]
    operator: String,
    #[serde(default)]
    value: String,
}

#[derive(Debug, Default, Deserialize)]
struct AtrTestCases {
    #[serde(default)]
    true_positives: Vec<AtrTestCase>,
    #[serde(default)]
    true_negatives: Vec<AtrTestCase>,
}

#[derive(Debug, Deserialize)]
struct AtrTestCase {
    /// Sample text for input-surface test cases. Upstream keys a few
    /// cases on tool_response/agent_output/... instead; those carry no
    /// `input` and are dropped — they exercise surfaces we do not scan.
    #[serde(default)]
    input: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DisabledFile {
    #[serde(default)]
    disabled: Vec<DisabledEntry>,
}

#[derive(Debug, Deserialize)]
struct DisabledEntry {
    id: String,
    #[allow(dead_code)] // reason is for humans reading the file
    reason: String,
}

// ---- output (internal pack v2) shapes -----------------------------------

#[derive(Debug, Serialize)]
struct OutPack {
    pack: OutMeta,
    rules: Vec<OutRule>,
}

#[derive(Debug, Serialize)]
struct OutMeta {
    name: String,
    source: String,
    version: String,
    license: String,
    upstream: String,
    generated_by: String,
}

#[derive(Debug, Serialize)]
struct OutRule {
    id: String,
    name: String,
    category: String,
    subcategory: String,
    severity: String,
    patterns: Vec<String>,
    description: String,
    url: String,
    references: Vec<String>,
    enabled: bool,
    single_line: bool,
    test_cases: OutTestCases,
}

#[derive(Debug, Serialize)]
struct OutTestCases {
    true_positives: Vec<String>,
    true_negatives: Vec<String>,
}

/// Why a rule or pattern was excluded — every exclusion is reported.
#[derive(Debug)]
struct Skip {
    id: String,
    reason: String,
}

/// Renders untrusted text as a single bounded line for the UPSTREAM.toml
/// report: newlines/carriage returns become the literal `\n`, and anything
/// past 80 characters is truncated with an ellipsis. Prevents comment
/// injection into the generated report.
fn sanitize_for_report(s: &str) -> String {
    let one_line = s.replace("\r\n", "\\n").replace(['\r', '\n'], "\\n");
    let mut chars = one_line.chars();
    let truncated: String = chars.by_ref().take(80).collect();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

/// Maps ATR severity onto the engine's four levels.
fn map_severity(atr: &str) -> Option<&'static str> {
    match atr {
        "informational" | "low" => Some("low"),
        "medium" => Some("medium"),
        "high" => Some("high"),
        "critical" => Some("critical"),
        _ => None,
    }
}

/// Mirror of the engine's `[\s\S]` → `(?s:.)` rewrite.
///
/// Verbatim copy of the private `rule_engine::normalize_any_char_class`
/// (a bin is a separate crate target); keep both sides identical.
fn normalize_any_char_class(pattern: &str) -> Cow<'_, str> {
    if pattern.contains(ANY_CHAR_CLASS) {
        Cow::Owned(pattern.replace(ANY_CHAR_CLASS, ANY_CHAR_INLINE_DOT))
    } else {
        Cow::Borrowed(pattern)
    }
}

/// True when the pattern compiles the way the engine compiles it: same
/// rewrite first, then the exact flags a `single_line: true` rule gets.
fn compiles(pattern: &str) -> bool {
    RegexBuilder::new(&normalize_any_char_class(pattern))
        .case_insensitive(true)
        .dot_matches_new_line(false)
        .build()
        .is_ok()
}

/// Flatten ATR's references map ({owasp_llm: [...], ...}) into strings.
fn flatten_references(value: &serde_yaml::Value) -> Vec<String> {
    let mut out = Vec::new();
    if let serde_yaml::Value::Mapping(map) = value {
        for (_key, val) in map {
            if let serde_yaml::Value::Sequence(seq) = val {
                for item in seq {
                    if let serde_yaml::Value::String(s) = item {
                        out.push(s.clone());
                    }
                }
            }
        }
    }
    out
}

/// Subcategory slug from the upstream file name:
/// "ATR-2026-00001-direct-prompt-injection.yaml" → "direct-prompt-injection".
fn subcategory_from_filename(file_name: &str, rule_id: &str) -> String {
    file_name
        .trim_end_matches(".yaml")
        .strip_prefix(&format!("{rule_id}-"))
        .unwrap_or("")
        .to_string()
}

/// Convert one ATR rule; `None` plus report entries when filtered out.
fn convert_rule(
    rule: AtrRule,
    file_name: &str,
    atr_category: &str,
    internal_category: &str,
    disabled: &BTreeSet<String>,
    skips: &mut Vec<Skip>,
) -> Option<OutRule> {
    if rule.maturity != "stable" {
        skips.push(Skip {
            id: rule.id,
            reason: format!("maturity {}", rule.maturity),
        });
        return None;
    }
    if rule.status == "draft" || rule.status == "deprecated" {
        skips.push(Skip {
            id: rule.id,
            reason: format!("status {}", rule.status),
        });
        return None;
    }
    if !SCAN_TARGETS.contains(&rule.scan_target.as_str()) {
        skips.push(Skip {
            id: rule.id,
            reason: format!("scan_target {}", rule.scan_target),
        });
        return None;
    }
    if rule.detection.condition != "any" {
        skips.push(Skip {
            id: rule.id,
            reason: format!("condition {}", rule.detection.condition),
        });
        return None;
    }
    let severity = match map_severity(&rule.severity) {
        Some(s) => s,
        None => {
            skips.push(Skip {
                id: rule.id,
                reason: format!("severity {}", rule.severity),
            });
            return None;
        }
    };
    let mut patterns = Vec::new();
    for cond in &rule.detection.conditions {
        if cond.operator != "regex" || !CONTENT_FIELDS.contains(&cond.field.as_str()) {
            skips.push(Skip {
                id: rule.id.clone(),
                reason: format!(
                    "pattern skipped: operator={} field={}",
                    cond.operator, cond.field
                ),
            });
            continue;
        }
        if !compiles(&cond.value) {
            skips.push(Skip {
                id: rule.id.clone(),
                reason: format!(
                    "pattern not regex-crate compatible: {}",
                    sanitize_for_report(&cond.value)
                ),
            });
            continue;
        }
        patterns.push(cond.value.clone());
    }
    if patterns.is_empty() {
        skips.push(Skip {
            id: rule.id,
            reason: "no usable patterns".to_string(),
        });
        return None;
    }
    let subcategory = subcategory_from_filename(file_name, &rule.id);
    Some(OutRule {
        url: format!("{UPSTREAM_URL}/blob/main/rules/{atr_category}/{file_name}"),
        name: rule.title,
        category: internal_category.to_string(),
        subcategory,
        severity: severity.to_string(),
        patterns,
        description: rule.description.trim().to_string(),
        references: flatten_references(&rule.references),
        enabled: !disabled.contains(&rule.id),
        // ATR's reference engine is JS: '.' does not cross newlines.
        // Keeping that semantic avoids widening upstream-validated rules.
        single_line: true,
        test_cases: OutTestCases {
            true_positives: rule
                .test_cases
                .true_positives
                .into_iter()
                .filter_map(|t| t.input)
                .collect(),
            true_negatives: rule
                .test_cases
                .true_negatives
                .into_iter()
                .filter_map(|t| t.input)
                .collect(),
        },
        id: rule.id,
    })
}

/// Renders one exclusion as a single UPSTREAM.toml comment line.
///
/// Both fields are sanitized: the reason may embed upstream pattern content,
/// and the id itself comes verbatim from the upstream YAML — either one
/// could otherwise inject extra lines into the report.
fn skip_report_line(skip: &Skip) -> String {
    format!(
        "# {} — {}",
        sanitize_for_report(&skip.id),
        sanitize_for_report(&skip.reason)
    )
}

fn git_head(dir: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "HEAD"])
        .output()?;
    if !out.status.success() {
        return Err(format!("git rev-parse failed in {}", dir.display()).into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut atr_dir: Option<PathBuf> = None;
    let mut tag: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--atr-dir" => {
                atr_dir = Some(PathBuf::from(
                    args.next().ok_or("missing value for --atr-dir")?,
                ));
            }
            "--tag" => tag = Some(args.next().ok_or("missing value for --tag")?),
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let atr_dir = atr_dir.ok_or("--atr-dir <path to ATR checkout> is required")?;
    let tag = tag.ok_or("--tag <pinned upstream tag> is required")?;
    // The tag lands verbatim in generated file headers and UPSTREAM.toml;
    // restrict it to a safe character set to rule out injection there.
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "--tag {tag:?} is invalid: only characters [A-Za-z0-9._-] are allowed"
        )
        .into());
    }
    if !atr_dir.join("rules").is_dir() {
        return Err(format!(
            "{} does not look like an ATR checkout (missing rules/ directory)",
            atr_dir.display()
        )
        .into());
    }

    let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("rules/atr");
    let disabled_path = out_dir.join("disabled.yaml");
    let disabled_raw = fs::read_to_string(&disabled_path)
        .map_err(|e| format!("{}: {e}", disabled_path.display()))?;
    let disabled_file: DisabledFile = serde_yaml::from_str(&disabled_raw)
        .map_err(|e| format!("{}: {e}", disabled_path.display()))?;
    let disabled: BTreeSet<String> = disabled_file.disabled.into_iter().map(|e| e.id).collect();

    let commit = git_head(&atr_dir)?;
    let mut skips: Vec<Skip> = Vec::new();
    let mut kept_total = 0usize;

    for (atr_category, internal_category) in CATEGORIES {
        let dir = atr_dir.join("rules").join(atr_category);
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "yaml"))
            .collect();
        entries.sort();

        let mut rules: Vec<OutRule> = Vec::new();
        for path in entries {
            // read_dir entries always have a file name
            let file_name = path.file_name().unwrap().to_string_lossy().to_string();
            let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let rule: AtrRule =
                serde_yaml::from_str(&raw).map_err(|exc| format!("{}: {exc}", path.display()))?;
            if let Some(out) = convert_rule(
                rule,
                &file_name,
                atr_category,
                internal_category,
                &disabled,
                &mut skips,
            ) {
                rules.push(out);
            }
        }
        rules.sort_by(|a, b| a.id.cmp(&b.id));
        kept_total += rules.len();

        let pack = OutPack {
            pack: OutMeta {
                name: format!("atr-{atr_category}"),
                source: "atr".to_string(),
                version: tag.trim_start_matches('v').to_string(),
                license: "MIT".to_string(),
                upstream: UPSTREAM_URL.to_string(),
                generated_by: "sync_atr".to_string(),
            },
            rules,
        };
        let header = format!(
            "# Generated by sync_atr from ATR {tag} — DO NOT EDIT MANUALLY.\n# Disable false positives via rules/atr/disabled.yaml and re-run the sync.\n"
        );
        let body = serde_yaml::to_string(&pack)?;
        let out_path = out_dir.join(format!("{internal_category}.yaml"));
        fs::write(&out_path, header + &body).map_err(|e| format!("{}: {e}", out_path.display()))?;
    }

    let mut report = String::new();
    writeln!(
        report,
        "# Generated by sync_atr — sync provenance and audit trail."
    )?;
    writeln!(report, "[upstream]")?;
    writeln!(report, "url = \"{UPSTREAM_URL}\"")?;
    writeln!(report, "tag = \"{tag}\"")?;
    writeln!(report, "commit = \"{commit}\"")?;
    writeln!(report, "\n[stats]")?;
    writeln!(report, "rules_kept = {kept_total}")?;
    writeln!(report, "exclusions = {}", skips.len())?;
    writeln!(report, "\n# Every excluded rule/pattern with its reason:")?;
    for skip in &skips {
        writeln!(report, "{}", skip_report_line(skip))?;
    }
    let report_path = out_dir.join("UPSTREAM.toml");
    fs::write(&report_path, report).map_err(|e| format!("{}: {e}", report_path.display()))?;
    println!(
        "kept {kept_total} rules; {} exclusions; report written",
        skips.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rule(maturity: &str, status: &str, scan_target: &str) -> AtrRule {
        AtrRule {
            id: "ATR-2026-00001".to_string(),
            title: "T".to_string(),
            status: status.to_string(),
            maturity: maturity.to_string(),
            severity: "high".to_string(),
            scan_target: scan_target.to_string(),
            description: "d".to_string(),
            detection: AtrDetection {
                condition: "any".to_string(),
                conditions: vec![AtrCondition {
                    field: "content".to_string(),
                    operator: "regex".to_string(),
                    value: "(?i)ignore previous".to_string(),
                }],
            },
            references: serde_yaml::Value::Null,
            test_cases: AtrTestCases::default(),
        }
    }

    fn convert(rule: AtrRule) -> (Option<OutRule>, Vec<Skip>) {
        let mut skips = Vec::new();
        let out = convert_rule(
            rule,
            "ATR-2026-00001-direct-prompt-injection.yaml",
            "prompt-injection",
            "prompt_injection",
            &BTreeSet::new(),
            &mut skips,
        );
        (out, skips)
    }

    #[test]
    fn stable_rule_without_scan_target_is_kept() {
        // Stable ATR rules omit the top-level scan_target field; serde
        // defaults it to "" which must pass the whitelist.
        let (out, skips) = convert(sample_rule("stable", "stable", ""));
        let out = out.expect("kept");
        assert!(skips.is_empty());
        assert_eq!(out.severity, "high");
        assert_eq!(out.subcategory, "direct-prompt-injection");
        assert!(out.single_line);
        assert!(out.enabled);
    }

    #[test]
    fn non_stable_and_wrong_target_are_skipped_with_reason() {
        let (out, skips) = convert(sample_rule("test", "stable", "mcp"));
        assert!(out.is_none());
        assert!(skips[0].reason.contains("maturity"));

        let (out, skips) = convert(sample_rule("stable", "draft", "mcp"));
        assert!(out.is_none());
        assert!(skips[0].reason.contains("status"));

        let (out, skips) = convert(sample_rule("stable", "stable", "skill"));
        assert!(out.is_none());
        assert!(skips[0].reason.contains("scan_target"));
    }

    #[test]
    fn user_input_field_is_kept_and_tool_response_field_is_skipped() {
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.detection.conditions = vec![
            AtrCondition {
                field: "user_input".to_string(),
                operator: "regex".to_string(),
                value: "(?i)override the system".to_string(),
            },
            AtrCondition {
                field: "tool_response".to_string(),
                operator: "regex".to_string(),
                value: "(?i)poisoned tool output".to_string(),
            },
        ];
        let (out, skips) = convert(rule);
        let out = out.expect("kept with the user_input pattern");
        assert_eq!(out.patterns, vec!["(?i)override the system".to_string()]);
        let skip = skips
            .iter()
            .find(|s| s.reason.contains("pattern skipped"))
            .expect("tool_response pattern reported");
        assert!(skip.reason.contains("field=tool_response"));
    }

    #[test]
    fn incompatible_pattern_is_dropped_but_rule_survives() {
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.detection.conditions.push(AtrCondition {
            field: "content".to_string(),
            operator: "regex".to_string(),
            // Lookahead: valid JS regex, rejected by the regex crate.
            value: r"(?=forbidden)".to_string(),
        });
        let (out, skips) = convert(rule);
        let out = out.expect("kept with remaining pattern");
        assert_eq!(out.patterns.len(), 1);
        assert!(skips
            .iter()
            .any(|s| s.reason.contains("not regex-crate compatible")));
    }

    #[test]
    fn normalization_mirrors_the_engine_rewrite() {
        // Same cases the engine asserts on its own copy. If these two sets
        // ever disagree, the validator stops speaking for the engine.
        assert_eq!(normalize_any_char_class(r"a[\s\S]{0,5}b"), r"a(?s:.){0,5}b");
        assert_eq!(normalize_any_char_class(r"[\s\S]x[\s\S]"), r"(?s:.)x(?s:.)");
        assert_eq!(normalize_any_char_class(r"\s+plain\S"), r"\s+plain\S");
    }

    #[test]
    fn pattern_breaking_only_after_normalization_is_dropped() {
        // `\[\s\S]` compiles as written, but the engine's rewrite turns it
        // into `\(?s:.)` whose trailing paren is unopened. Validating the raw
        // string would ship a pattern that fails at engine load time and
        // takes the whole pack down with it.
        let raw = r"\[\s\S]";
        assert!(
            RegexBuilder::new(raw)
                .case_insensitive(true)
                .dot_matches_new_line(false)
                .build()
                .is_ok(),
            "precondition: the raw pattern must compile, or this test proves nothing"
        );

        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.detection.conditions.push(AtrCondition {
            field: "content".to_string(),
            operator: "regex".to_string(),
            value: raw.to_string(),
        });
        let (out, skips) = convert(rule);
        assert_eq!(out.expect("kept with remaining pattern").patterns.len(), 1);
        assert!(skips
            .iter()
            .any(|s| s.reason.contains("not regex-crate compatible")));
    }

    #[test]
    fn sanitize_for_report_flattens_newlines_and_truncates() {
        assert_eq!(sanitize_for_report("a\nb\r\nc\rd"), "a\\nb\\nc\\nd");
        assert_eq!(sanitize_for_report("plain"), "plain");
        let long = "x".repeat(100);
        let sanitized = sanitize_for_report(&long);
        assert_eq!(sanitized.chars().count(), 81);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn skip_id_with_newline_cannot_inject_report_lines() {
        // The id comes from the upstream YAML `id:` field, so it is as
        // untrusted as the pattern content: a newline in it must not become
        // its own line in UPSTREAM.toml (e.g. a forged `commit = ...` entry).
        let skip = Skip {
            id: "ATR-2026-00001\ncommit = \"attacker\"".to_string(),
            reason: "maturity test".to_string(),
        };
        let line = skip_report_line(&skip);
        assert!(!line.contains('\n'));
        assert!(line.contains("\\ncommit"));
        assert!(line.starts_with("# "));
    }

    #[test]
    fn multiline_pattern_skip_reason_stays_single_line() {
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.detection.conditions.push(AtrCondition {
            field: "content".to_string(),
            operator: "regex".to_string(),
            // Invalid for the regex crate (lookahead) AND multi-line: the
            // second line would land as its own line in UPSTREAM.toml.
            value: "(?=bad)\ncommit = \"attacker\"".to_string(),
        });
        let (out, skips) = convert(rule);
        assert!(out.is_some());
        let skip = skips
            .iter()
            .find(|s| s.reason.contains("not regex-crate compatible"))
            .expect("incompatible pattern reported");
        assert!(!skip.reason.contains('\n'));
        assert!(skip.reason.contains("\\n"));
    }

    #[test]
    fn informational_severity_maps_to_low() {
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.severity = "informational".to_string();
        let (out, _) = convert(rule);
        assert_eq!(out.unwrap().severity, "low");
    }

    #[test]
    fn disabled_list_flips_enabled_off() {
        let mut skips = Vec::new();
        let disabled: BTreeSet<String> = ["ATR-2026-00001".to_string()].into();
        let out = convert_rule(
            sample_rule("stable", "stable", "mcp"),
            "ATR-2026-00001-direct-prompt-injection.yaml",
            "prompt-injection",
            "prompt_injection",
            &disabled,
            &mut skips,
        )
        .unwrap();
        assert!(!out.enabled);
    }

    #[test]
    fn non_any_condition_is_skipped_with_reason() {
        // ATR's "all" semantics require every condition to match; the L1
        // engine ORs patterns, so importing such a rule would loosen it.
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.detection.condition = "all".to_string();
        let (out, skips) = convert(rule);
        assert!(out.is_none());
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].reason, "condition all");
    }

    #[test]
    fn unknown_severity_is_skipped_with_reason() {
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.severity = "catastrophic".to_string();
        let (out, skips) = convert(rule);
        assert!(out.is_none());
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].reason, "severity catastrophic");
    }

    #[test]
    fn rule_is_dropped_when_no_pattern_survives_filtering() {
        // Distinct from incompatible_pattern_is_dropped_but_rule_survives:
        // here nothing is left to match on, so the rule itself is reported
        // in addition to each dropped pattern.
        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.detection.conditions = vec![AtrCondition {
            field: "tool_args".to_string(),
            operator: "regex".to_string(),
            value: "(?i)anything".to_string(),
        }];
        let (out, skips) = convert(rule);
        assert!(out.is_none());
        let reasons: Vec<&str> = skips.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(
            reasons,
            vec![
                "pattern skipped: operator=regex field=tool_args",
                "no usable patterns",
            ]
        );
    }

    #[test]
    fn severity_map_covers_every_upstream_level() {
        assert_eq!(map_severity("informational"), Some("low"));
        assert_eq!(map_severity("low"), Some("low"));
        assert_eq!(map_severity("medium"), Some("medium"));
        assert_eq!(map_severity("high"), Some("high"));
        assert_eq!(map_severity("critical"), Some("critical"));
        assert_eq!(map_severity("moderate"), None);
        assert_eq!(map_severity(""), None);
    }

    #[test]
    fn references_flatten_across_keys_and_ignore_non_strings() {
        let refs: serde_yaml::Value =
            serde_yaml::from_str("owasp_llm:\n  - LLM01\n  - 7\ncwe:\n  - CWE-77\nnote: plain\n")
                .unwrap();
        assert_eq!(
            flatten_references(&refs),
            vec!["LLM01".to_string(), "CWE-77".to_string()]
        );
        // Rules with no `references:` key deserialize to Null, not a mapping.
        assert!(flatten_references(&serde_yaml::Value::Null).is_empty());

        let mut rule = sample_rule("stable", "stable", "mcp");
        rule.references = refs;
        let (out, _) = convert(rule);
        assert_eq!(
            out.unwrap().references,
            vec!["LLM01".to_string(), "CWE-77".to_string()]
        );
    }

    #[test]
    fn omitted_detection_condition_defaults_to_any() {
        // Most upstream rules omit `condition:`; without the serde default
        // they would all trip the non-"any" skip and the pack would empty out.
        let detection: AtrDetection = serde_yaml::from_str(
            "conditions:\n  - field: content\n    operator: regex\n    value: \"(?i)x\"\n",
        )
        .unwrap();
        assert_eq!(detection.condition, "any");
        assert_eq!(detection.conditions.len(), 1);
    }
}
