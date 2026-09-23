// SPDX-License-Identifier: MIT
// Copyright (c) 2026 eunomia-bpf org.
//! ActPlane taint DSL compiler: parse the DSL (docs/rule-language.md) and lower it
//! to the kernel ABI (struct taint_config) the loader installs into BPF rodata.

pub mod ast;
pub mod lower;
pub mod parse;

use std::collections::HashMap;

pub use lower::{Compiled, RuleMeta, RuleSourceMeta, compile};

/// Parse + compile DSL source text to a kernel config blob + reason table.
pub fn compile_str(src: &str) -> Result<Compiled, String> {
    let mut compiled = compile(&parse::parse(src)?)?;
    attach_source_meta(&mut compiled, src);
    Ok(compiled)
}

/// Parse + compile DSL while preserving an existing label-bit dictionary.
///
/// Runtime policy deltas use this so a later delta in the same runtime domain
/// can refer to labels created by an earlier delta without silently changing
/// their bit positions.
pub fn compile_str_with_labels(
    src: &str,
    existing_labels: &HashMap<String, u64>,
) -> Result<Compiled, String> {
    let mut compiled = lower::compile_with_labels(&parse::parse(src)?, existing_labels)?;
    attach_source_meta(&mut compiled, src);
    Ok(compiled)
}

fn attach_source_meta(compiled: &mut Compiled, src: &str) {
    let spans = rule_source_spans(src);
    for meta in &mut compiled.meta {
        if let Some(span) = spans.iter().find(|span| span.name == meta.name) {
            let clause = span.clauses.get(meta.clause_source_index);
            meta.source = Some(RuleSourceMeta {
                source_ref: span.source_ref.clone(),
                binding_mode: span.binding_mode.clone(),
                start_line: span.start_line,
                end_line: span.end_line,
                text: span.text.clone(),
                clause_start_line: clause.map(|c| c.start_line),
                clause_end_line: clause.map(|c| c.end_line),
                clause_text: clause.map(|c| c.text.clone()),
            });
        }
    }
}

struct RuleSourceSpan {
    name: String,
    source_ref: String,
    binding_mode: Option<String>,
    start_line: usize,
    end_line: usize,
    text: String,
    clauses: Vec<ClauseSourceSpan>,
}

#[derive(Clone)]
struct ClauseSourceSpan {
    start_line: usize,
    end_line: usize,
    text: String,
}

fn rule_source_spans(src: &str) -> Vec<RuleSourceSpan> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    let mut pending_source: Option<(String, Option<String>, usize)> = None;
    let mut i = 0usize;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if let Some(rest) = trimmed.strip_prefix("# actplane-rule-source ") {
            let (source_ref, binding_mode) = parse_rule_source_marker(rest);
            pending_source = Some((source_ref, binding_mode, i + 1));
            i += 1;
            continue;
        }
        let Some(name) = parse_rule_decl_name(trimmed) else {
            i += 1;
            continue;
        };
        let marker = pending_source.take();
        let start = marker
            .as_ref()
            .map(|(_, _, text_start)| *text_start)
            .unwrap_or(i);
        let mut end = i + 1;
        while end < lines.len() {
            let t = lines[end].trim();
            if t.starts_with("# actplane-rule-source ") || is_top_level_decl(t) {
                break;
            }
            end += 1;
        }
        out.push(RuleSourceSpan {
            source_ref: marker
                .as_ref()
                .map(|(source_ref, _, _)| source_ref.clone())
                .unwrap_or_else(|| format!("rule:{name}")),
            binding_mode: marker.and_then(|(_, mode, _)| mode),
            name,
            start_line: start + 1,
            end_line: end,
            text: lines[start..end].join("\n"),
            clauses: clause_source_spans(&lines, i, end),
        });
        i = end;
    }
    out
}

fn clause_source_spans(lines: &[&str], rule_decl: usize, rule_end: usize) -> Vec<ClauseSourceSpan> {
    let mut out = Vec::new();
    let mut current: Option<usize> = None;
    let mut line = rule_decl + 1;
    while line < rule_end {
        let trimmed = lines[line].trim();
        if is_clause_head(trimmed) {
            if let Some(start) = current.take() {
                out.push(make_clause_source_span(lines, start, line));
            }
            current = Some(line);
        } else if trimmed.starts_with("because ") {
            break;
        }
        line += 1;
    }
    if let Some(start) = current {
        out.push(make_clause_source_span(lines, start, line));
    }
    out
}

fn make_clause_source_span(lines: &[&str], start: usize, end: usize) -> ClauseSourceSpan {
    ClauseSourceSpan {
        start_line: start + 1,
        end_line: end,
        text: lines[start..end].join("\n"),
    }
}

fn is_clause_head(trimmed: &str) -> bool {
    let mut words = trimmed.split_whitespace();
    let Some(effect) = words.next() else {
        return false;
    };
    if !matches!(effect, "notify" | "block" | "kill") {
        return false;
    }
    matches!(
        words.next(),
        Some("exec" | "read" | "write" | "unlink" | "connect" | "recv" | "open")
    )
}

fn parse_rule_source_marker(text: &str) -> (String, Option<String>) {
    let mut source_ref = None;
    let mut binding_mode = None;
    for part in text.split_whitespace() {
        if let Some(value) = part.strip_prefix("ref=") {
            source_ref = Some(value.to_string());
        } else if let Some(value) = part.strip_prefix("mode=") {
            binding_mode = Some(value.to_string());
        }
    }
    (
        source_ref.unwrap_or_else(|| "inline".to_string()),
        binding_mode,
    )
}

fn parse_rule_decl_name(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("rule ")?;
    let name = rest.split(':').next()?.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

fn is_top_level_decl(trimmed: &str) -> bool {
    matches!(
        trimmed.split_whitespace().next(),
        Some("source" | "declassify" | "endorse" | "rule" | "label")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

    fn ok(src: &str) -> Compiled {
        compile_str(src).expect("compile")
    }

    fn corpus_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test/policies")
    }

    fn corpus_policy_sources() -> Vec<(PathBuf, String)> {
        let dir = corpus_dir();
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
            .map(|ent| ent.expect("policy dir entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "yaml"))
            .collect();
        paths.sort();
        assert!(
            !paths.is_empty(),
            "no YAML policy corpus files in {}",
            dir.display()
        );
        paths
            .into_iter()
            .flat_map(|path| {
                let src = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                yaml_policy_sources(&path, &src)
                    .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
            })
            .collect()
    }

    fn yaml_policy_sources(path: &Path, src: &str) -> Result<Vec<(PathBuf, String)>, String> {
        let value: serde_yaml::Value = serde_yaml::from_str(src).map_err(|e| e.to_string())?;
        if let Some(policy) = yaml_get(&value, "policy").and_then(serde_yaml::Value::as_str) {
            return Ok(vec![(path.to_path_buf(), policy.to_string())]);
        }
        let Some(rules) = yaml_get(&value, "rules").and_then(serde_yaml::Value::as_mapping) else {
            return Ok(Vec::new());
        };
        let mut source = String::new();
        for (name, entry) in rules {
            let Some(rule_name) = name.as_str() else {
                continue;
            };
            let Some(ifc) = yaml_get(entry, "ifc")
                .or_else(|| yaml_get(entry, "policy"))
                .and_then(serde_yaml::Value::as_str)
            else {
                continue;
            };
            source.push_str("\n# actplane-rule-source ref=rules.");
            source.push_str(rule_name);
            source.push_str(".ifc mode=test\n# rule ");
            source.push_str(rule_name);
            source.push('\n');
            source.push_str(ifc.trim());
            source.push('\n');
        }
        Ok(vec![(path.to_path_buf(), source)])
    }

    fn yaml_get<'a>(value: &'a serde_yaml::Value, key: &str) -> Option<&'a serde_yaml::Value> {
        value
            .as_mapping()?
            .get(serde_yaml::Value::String(key.to_string()))
    }

    #[test]
    fn e1_secret_no_exfil() {
        let c = ok(r#"
            source SECRET = file "**/.env"
            source SECRET = file "/etc/secrets/**"
            rule no-exfil:
              block connect endpoint "*"      if SECRET
              block write   file "/shared/**" if SECRET
              because "secret data must not leave the host"
            declassify SECRET by exec "**/redact"
        "#);
        assert_eq!(c.reasons.len(), 2);
        assert!(!c.bytes.is_empty());
    }

    #[test]
    fn compile_with_labels_preserves_runtime_delta_label_bits() {
        let mut existing = HashMap::new();
        existing.insert("SECRET".to_string(), 1u64 << 7);
        let c = compile_str_with_labels(
            r#"
            source SECRET = file "**/.env"
            rule no-secret:
              notify exec "git" if SECRET and not REVIEWED
              because "secret data needs review"
            "#,
            &existing,
        )
        .expect("compile with existing labels");

        assert_eq!(c.labels.get("SECRET"), Some(&(1u64 << 7)));
        assert_eq!(c.labels.get("REVIEWED"), Some(&1u64));
    }

    #[test]
    fn rule_source_metadata_records_marker_span_and_text() {
        let c = compile_str(
            r#"# actplane-rule-source ref=rules.secret.ifc mode=locked
# rule secret
source SECRET = file "**/.env"
rule secret:
  block exec "git" if SECRET
  because "secret needs review"
"#,
        )
        .expect("compile");

        let source = c.meta[0].source.as_ref().expect("source metadata");
        assert_eq!(source.source_ref, "rules.secret.ifc");
        assert_eq!(source.binding_mode.as_deref(), Some("locked"));
        assert_eq!(source.start_line, 2);
        assert_eq!(source.end_line, 6);
        assert!(source.text.contains("source SECRET"));
        assert!(source.text.contains("rule secret:"));
        assert_eq!(source.clause_start_line, Some(5));
        assert_eq!(source.clause_end_line, Some(5));
        assert_eq!(
            source.clause_text.as_deref(),
            Some("  block exec \"git\" if SECRET")
        );
    }

    #[test]
    fn e2_prompt_injection() {
        let c = ok(r#"
            source UNTRUST = endpoint "*"
            source UNTRUST = file "**/downloads/**"
            rule no-injected-priv:
              block exec "git" "push" if UNTRUST and not REVIEWED
              block exec "**/deploy*"         if UNTRUST and not REVIEWED
              because "untrusted input must not drive privileged actions"
            endorse REVIEWED by exec "**/human-approve"
        "#);
        assert_eq!(c.reasons.len(), 2);
    }

    #[test]
    fn e3_mandatory_mediation() {
        ok(r#"
            rule mediate-proddb:
              block open file "**/prod.db" unless lineage-includes exec "**/migrate"
              because "prod.db only via the migration tool"
        "#);
    }

    #[test]
    fn e4_workspace_confinement() {
        ok(r#"
            source AGENT = exec "**/codex"
            rule confine-writes:
              block write  file "/**" if AGENT unless target "/work/**"
              block unlink file "/**" if AGENT unless target "/work/**"
              because "agent may only modify /work"
        "#);
    }

    #[test]
    fn e5_test_before_commit() {
        ok(r#"
            source AGENT = exec "**/codex"
            rule test-before-commit:
              block exec "git" "commit" if AGENT unless after exec "**/pytest"
              because "run tests before committing"
        "#);
    }

    #[test]
    fn e5_test_before_commit_requires_successful_exit() {
        ok(r#"
            source AGENT = exec "**/codex"
            rule test-before-commit:
              block exec "git" "commit" if AGENT unless after exec "**/pytest" exits 0
              because "run tests successfully before committing"
        "#);
    }

    #[test]
    fn e5p_test_before_commit_since() {
        // v2 staleness: editing src after the gate makes the prior pytest stale.
        let c = ok(r#"
            source AGENT = exec "**/codex"
            rule test-before-commit:
              block exec "git" "commit"
                if AGENT
                unless after exec "**/pytest" since write "src/**" or write "tests/**"
              because "tests are stale — you edited code after the last run"
        "#);
        assert_eq!(c.reasons.len(), 1);
    }

    #[test]
    fn e11p_confirm_single_shot_since() {
        // v2: each force-push needs a fresh confirm (a later git makes it stale).
        ok(r#"
            source AGENT = exec "**/codex"
            rule confirm-destructive:
              block exec "git" "--force"
                if AGENT
                unless after exec "**/confirm" since exec "git"
              because "each force-push needs a fresh confirm"
        "#);
    }

    #[test]
    fn e13_migrate_check_since() {
        // v2: prod.db write needs a migration-check fresh w.r.t. the migrations.
        ok(r#"
            source AGENT = exec "**/codex"
            rule migrate-checked:
              block write file "**/prod.db"
                if AGENT
                unless after exec "**/migrate-check" since write "migrations/**"
            because "migration-check must have seen the current migrations"
        "#);
    }

    #[test]
    fn e14_stdio_channels_are_ifc_files() {
        ok(r#"
            source PROMPT = file "stdio:stdin"
            rule no-prompt-to-stdout:
              notify write file "stdio:stdout" if PROMPT
              notify write file "stdio:stderr" if PROMPT
              because "prompt-derived data should not be printed without review"
        "#);
    }

    #[test]
    fn since_without_clause_is_v1_latching() {
        // `after` with no `since` must still compile (v1 semantics, since_mask=0)
        // and produce the same fixed-size blob as a since-bearing policy.
        let v1 = ok(
            "rule r:\n  block exec \"git\" if A unless after exec \"**/pytest\"\n  because \"x\"\n",
        );
        let v2 = ok(
            "rule r:\n  block exec \"git\" if A unless after exec \"**/pytest\" since write \"src/**\"\n  because \"x\"\n",
        );
        assert_eq!(v1.bytes.len(), v2.bytes.len());
    }

    #[test]
    fn since_bad_invalidator_op_is_rejected() {
        assert!(compile_str(
            "rule r:\n  block exec \"git\" if A unless after exec \"**/pytest\" since connect \"*\"\n  because \"x\"\n"
        )
        .is_err());
    }

    #[test]
    fn exits_is_only_valid_for_exec_gates() {
        assert!(compile_str(
            "rule r:\n  block exec \"git\" if A unless after read \"src/**\" exits 0\n  because \"x\"\n"
        )
        .is_err());
    }

    #[test]
    fn e6_research_readonly() {
        ok(r#"
            source RESEARCH = exec "**/research-agent"
            rule research-readonly:
              block write   file "/**"   if RESEARCH
              block connect endpoint "*" if RESEARCH
              block exec    "git"        if RESEARCH
              because "research sub-agent is read-only"
        "#);
    }

    #[test]
    fn e7_e8_secret_with_declassify() {
        // same policy as E1; E7 (derivation) and E8 (declassify) are runtime behaviors
        ok(r#"
            source SECRET = file "**/.env"
            rule no-exfil:
              block connect endpoint "*" if SECRET
              because "no exfil"
            declassify SECRET by exec "**/redact"
        "#);
    }

    #[test]
    fn e9_cross_tool() {
        ok(r#"
            source AGENT = exec "**/codex"
            rule no-git:
              block exec "git" if AGENT
              because "no git on any path"
        "#);
    }

    #[test]
    fn e10_pii_egress() {
        ok(r#"
            source PII = file "/data/customers/**"
            rule pii-egress:
              block connect endpoint "*" if PII unless target "*.internal"
              because "PII only to internal"
        "#);
    }

    #[test]
    fn e11_destructive_confirm() {
        ok(r#"
            source AGENT = exec "**/codex"
            rule confirm-destructive:
              block exec "git" "--force" if AGENT unless after exec "**/confirm"
              block unlink file "/data/**"    if AGENT unless after exec "**/confirm"
              because "destructive needs confirm"
        "#);
    }

    #[test]
    fn e12_non_interference() {
        let c = ok(r#"
            source TASK_A = exec "**/task-a"
            source TASK_B = exec "**/task-b"
            rule no-cross-task-commit:
              block exec "git" "commit" if TASK_A and TASK_B
              because "no cross-task commit"
        "#);
        assert_eq!(c.reasons.len(), 1);
    }

    #[test]
    fn dnf_or_splits_into_multiple_rules() {
        // `if A or B` must compile to 2 kernel rules with exact metadata for
        // each lowered clause.
        let a = compile_str("rule r:\n  block exec \"x\" if A\n  because \"z\"\n").unwrap();
        let b = compile_str("rule r:\n  block exec \"x\" if A or B\n  because \"z\"\n").unwrap();
        assert!(b.bytes.len() == a.bytes.len()); // fixed-size config
        assert_eq!(b.reasons.len(), 2);
        assert_eq!(b.meta.len(), 2);
        assert_eq!(b.meta[0].name, "r");
        assert_eq!(b.meta[1].name, "r");
        assert_eq!(b.meta[0].clause_op, "exec");
        assert_eq!(b.meta[1].kernel_op, "exec");
    }

    #[test]
    fn config_blob_is_fixed_size() {
        const TAINT_CONFIG_SIZE: usize = 74_760;
        // every policy produces the same fixed-size struct taint_config blob
        let a = ok("rule r:\n  block exec \"git\" if A\n  because \"x\"\n");
        let b = ok(
            "source S = file \"/x/**\"\nrule r:\n  block open file \"/y/**\" if S\n  because \"x\"\n",
        );
        assert_eq!(a.bytes.len(), TAINT_CONFIG_SIZE);
        assert_eq!(a.bytes.len(), b.bytes.len());
    }

    #[test]
    fn policy_corpus_files_compile() {
        const TAINT_CONFIG_SIZE: usize = 74_760;
        let policies = corpus_policy_sources();
        let mut blob_len = None;
        for (path, src) in &policies {
            let compiled =
                compile_str(src).unwrap_or_else(|e| panic!("compile {}: {e}", path.display()));
            assert!(
                !compiled.meta.is_empty(),
                "{} should contain at least one rule",
                path.display()
            );
            if let Some(n) = blob_len {
                assert_eq!(
                    compiled.bytes.len(),
                    n,
                    "{} blob size drift",
                    path.display()
                );
            } else {
                blob_len = Some(compiled.bytes.len());
            }
        }
        assert_eq!(blob_len, Some(TAINT_CONFIG_SIZE));
    }

    #[test]
    fn domain_policy_corpus_all_domains_compile() {
        let dir = corpus_dir();
        let mut checked = 0usize;
        for ent in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        {
            let path = ent.expect("policy dir entry").path();
            if path.extension().is_none_or(|ext| ext != "yaml") {
                continue;
            }
            let src = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            let value: serde_yaml::Value = serde_yaml::from_str(&src)
                .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
            if yaml_get(&value, "domains").is_none() {
                continue;
            }
            for (_, policy) in yaml_policy_sources(&path, &src)
                .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
            {
                let compiled = compile_str(&policy)
                    .unwrap_or_else(|e| panic!("compile {} rule bodies: {e}", path.display()));
                assert!(
                    !compiled.meta.is_empty(),
                    "{} should contain at least one rule body",
                    path.display()
                );
                checked += 1;
            }
        }
        assert!(checked >= 1, "expected domain policies in corpus");
    }

    #[test]
    #[ignore = "run test/policy-corpus.sh for the release microbench"]
    fn policy_corpus_compile_perf() {
        let policies = corpus_policy_sources();
        let rounds = std::env::var("ACTPLANE_POLICY_BENCH_ROUNDS")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(200);

        for (_, src) in &policies {
            compile_str(src).expect("warmup compile");
        }

        let start = Instant::now();
        let mut bytes = 0usize;
        for _ in 0..rounds {
            for (_, src) in &policies {
                bytes += compile_str(src).expect("bench compile").bytes.len();
            }
        }
        let elapsed = start.elapsed();
        let total = rounds * policies.len();
        let us_per_policy = elapsed.as_secs_f64() * 1_000_000.0 / total as f64;
        eprintln!(
            "ActPlane IFC compile perf: {total} policies in {:.3}s = {:.2} us/policy ({} bytes)",
            elapsed.as_secs_f64(),
            us_per_policy,
            bytes
        );
        assert!(bytes > 0);
    }

    #[test]
    fn rule_effect_is_metadata_and_kernel_config() {
        let c = ok("rule r:\n  kill exec \"git\"\n  because \"x\"\n");
        assert_eq!(c.meta[0].effect, ast::Effect::Kill);
        assert!(!c.bytes.is_empty());
    }

    #[test]
    fn source_labels_are_allocated_for_runner_seeding() {
        let c = ok(
            "source AGENT = exec \"**/claude\"\nrule r:\n  block exec \"git\" if AGENT\n  because \"x\"\n",
        );
        assert!(c.labels.contains_key("AGENT"));
    }

    #[test]
    fn old_label_keyword_is_rejected() {
        assert!(
            compile_str("label AGENT\nrule r:\n  block exec \"git\" if AGENT\n  because \"x\"\n")
                .is_err()
        );
    }

    #[test]
    fn old_deny_keyword_is_rejected() {
        assert!(compile_str("rule r:\n  deny exec \"git\"\n  because \"x\"\n").is_err());
    }

    #[test]
    fn duplicate_rule_names_are_rejected() {
        let err = match compile_str(
            r#"
            rule same:
              notify exec "git" if true
              because "one"
            rule same:
              notify exec "make" if true
              because "two"
        "#,
        ) {
            Ok(_) => panic!("duplicate rule name compiled successfully"),
            Err(err) => err,
        };
        assert!(err.contains("duplicate rule name `same`"));
    }

    #[test]
    fn implicit_basename_matching() {
        // `exec "git"` should be equivalent to `exec "**/git"` — both produce
        // the same compiled output.
        let a = ok("rule r:\n  block exec \"git\" if A\n  because \"x\"\n");
        let b = ok("rule r:\n  block exec \"**/git\" if A\n  because \"x\"\n");
        assert_eq!(a.bytes, b.bytes);
    }

    #[test]
    fn positional_args_work() {
        // positional args (no @arg keyword) should compile successfully
        let c = ok("rule r:\n  kill exec \"git\" \"commit\" if A\n  because \"x\"\n");
        assert_eq!(c.meta[0].effect, ast::Effect::Kill);
    }

    #[test]
    fn multi_verb_rule_preserves_clause_effects() {
        // When clauses have different effects, each lowered rule keeps the
        // clause-level effect that the kernel will enforce.
        let c = ok(r#"
            rule mixed:
              notify exec "git" if A
              kill exec "make" if A
              because "mixed effects"
        "#);
        assert_eq!(c.meta.len(), 2);
        assert_eq!(c.meta[0].effect, ast::Effect::Notify);
        assert_eq!(c.meta[0].target_pattern, "**/git");
        assert_eq!(c.meta[1].effect, ast::Effect::Kill);
        assert_eq!(c.meta[1].target_pattern, "**/make");
        assert_eq!(
            c.meta[0]
                .source
                .as_ref()
                .and_then(|source| source.clause_text.as_deref()),
            Some("              notify exec \"git\" if A")
        );
        assert_eq!(
            c.meta[1]
                .source
                .as_ref()
                .and_then(|source| source.clause_text.as_deref()),
            Some("              kill exec \"make\" if A")
        );
    }

    #[test]
    fn duplicate_clause_targets_keep_distinct_source_spans() {
        let c = ok(r#"
            rule repeated:
              notify exec "git" if A
              notify exec "git" if B
              because "same operation, different label"
        "#);
        assert_eq!(c.meta.len(), 2);
        assert_eq!(c.meta[0].clause_source_index, 0);
        assert_eq!(c.meta[1].clause_source_index, 1);
        assert_eq!(
            c.meta[0]
                .source
                .as_ref()
                .and_then(|source| source.clause_text.as_deref()),
            Some("              notify exec \"git\" if A")
        );
        assert_eq!(
            c.meta[1]
                .source
                .as_ref()
                .and_then(|source| source.clause_text.as_deref()),
            Some("              notify exec \"git\" if B")
        );
    }
}
