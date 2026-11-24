//! fact_check 事实回查 — verifies symbols/files referenced in the final answer
//! actually exist in the repo.
//!
//! Consumes shared assertions (no own LLM call). Grep runs in a blocking task.
//! - grep miss → L2 (objective, has checker)
//! - API claims → L4 (needs external doc knowledge, capped)
//! - grep execution failure → assertion skipped (not reported as missing)
//! - `repo_root=None` → strategy skips gracefully.

use std::path::{Path, PathBuf};
use std::process::Command;

use async_trait::async_trait;

use crate::types::{DefectType, EvidenceTier, RootObject};

use crate::accuracy::detector::{AnalysisCtx, Detector, RawIssue};
use crate::accuracy::extract::Assertion;

pub struct FactCheckStrategy;

/// Grep check outcome: found / not found / check itself failed.
enum GrepOutcome {
    Found,
    NotFound,
    CheckFailed,
}

impl FactCheckStrategy {
    pub fn new() -> Self {
        Self
    }

    fn grep_symbol(repo_root: &Path, symbol: &str, kind: &str) -> GrepOutcome {
        if symbol.trim().is_empty() {
            return GrepOutcome::NotFound;
        }
        if matches!(kind, "file" | "path") {
            return if Self::repo_contains_path(repo_root, symbol) {
                GrepOutcome::Found
            } else {
                GrepOutcome::NotFound
            };
        }

        let output = Command::new("grep")
            .args([
                "-r",
                "-l",
                "--exclude-dir=.git",
                "--exclude-dir=node_modules",
                "--exclude-dir=target",
                "--exclude-dir=dist",
                // C / C++ / Objective-C
                "--include=*.c",
                "--include=*.h",
                "--include=*.cpp",
                "--include=*.cc",
                "--include=*.cxx",
                "--include=*.hpp",
                "--include=*.hh",
                "--include=*.hxx",
                "--include=*.m",
                "--include=*.mm",
                // JVM / .NET
                "--include=*.java",
                "--include=*.kt",
                "--include=*.kts",
                "--include=*.scala",
                "--include=*.cs",
                // Scripting
                "--include=*.rs",
                "--include=*.py",
                "--include=*.rb",
                "--include=*.php",
                "--include=*.swift",
                "--include=*.sh",
                "--include=*.bash",
                "--include=*.zsh",
                "--include=*.lua",
                "--include=*.pl",
                "--include=*.pm",
                // Web (incl. ES/CommonJS module files)
                "--include=*.ts",
                "--include=*.tsx",
                "--include=*.js",
                "--include=*.mjs",
                "--include=*.cjs",
                "--include=*.jsx",
                "--include=*.vue",
                // Go + data / config
                "--include=*.go",
                "--include=*.sql",
                "--include=*.proto",
                "--include=*.yaml",
                "--include=*.yml",
                "--include=*.toml",
                "--include=*.json",
                "--include=*.xml",
                "--include=*.md",
                "-F",
                "-e",
                symbol,
                "--",
            ])
            .arg(repo_root)
            .output();

        match output {
            // grep exit 0 = match found, exit 1 = no match, >1 = error.
            Ok(out) if out.status.success() && !out.stdout.is_empty() => GrepOutcome::Found,
            Ok(out) if out.status.code() == Some(1) => GrepOutcome::NotFound,
            Ok(_) | Err(_) => GrepOutcome::CheckFailed,
        }
    }

    fn repo_contains_path(repo_root: &Path, symbol: &str) -> bool {
        let candidate = Path::new(symbol);
        if candidate.is_absolute() {
            return false;
        }
        let Ok(root) = repo_root.canonicalize() else {
            return false;
        };
        let Ok(resolved) = root.join(candidate).canonicalize() else {
            return false;
        };
        resolved != root && resolved.starts_with(&root)
    }
}

#[async_trait]
impl Detector for FactCheckStrategy {
    fn name(&self) -> &'static str {
        "fact_check"
    }

    async fn detect(&self, ctx: &AnalysisCtx<'_>) -> Vec<RawIssue> {
        let repo_root: PathBuf = match ctx.repo_root {
            Some(root) => root.to_path_buf(),
            None => {
                tracing::debug!("[fact_check] No repo_root, skipping");
                return vec![];
            }
        };

        let assertions: Vec<Assertion> = ctx.extraction.assertions.clone();
        if assertions.is_empty() {
            tracing::debug!("[fact_check] No assertions, skipping");
            return vec![];
        }

        let repo_display = repo_root.display().to_string();

        // Grep is blocking I/O — run off the async executor.
        let results = tokio::task::spawn_blocking(move || {
            assertions
                .into_iter()
                .map(|a| {
                    let outcome = Self::grep_symbol(&repo_root, &a.symbol, &a.kind);
                    (a, outcome)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_else(|e| {
            tracing::warn!("[fact_check] Grep task panicked: {e}");
            vec![]
        });

        let mut issues = Vec::new();
        for (assertion, outcome) in results {
            match outcome {
                GrepOutcome::Found => {}
                GrepOutcome::CheckFailed => {
                    tracing::debug!(
                        "[fact_check] Check failed for `{}`, skipping assertion",
                        assertion.symbol
                    );
                }
                GrepOutcome::NotFound => {
                    let tier = if assertion.kind == "api" {
                        EvidenceTier::L4
                    } else {
                        EvidenceTier::L2
                    };
                    issues.push(RawIssue {
                        symptom: format!(
                            "引用的 {} `{}` 在代码仓库中未找到",
                            assertion.kind, assertion.symbol
                        ),
                        defect_type: DefectType::Knowledge,
                        primary_object: RootObject::Skill,
                        evidence_tier: tier,
                        tool_call_id: None,
                        detail: format!(
                            "最终答案引用了 {} `{}`，但在仓库 `{}` 中 grep 未找到任何匹配。\
                             可能是编造的符号或已过时的 API。",
                            assertion.kind, assertion.symbol, repo_display
                        ),
                        verify: "人工确认该符号是否确实不存在，或是否因搜索范围不足导致漏检。"
                            .into(),
                        fix: format!(
                            "Skill 知识库更新：移除或替换对 `{}` 的引用，确保引用符号与实际代码一致。",
                            assertion.symbol
                        ),
                    });
                }
            }
        }

        issues
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The verifier greps the repo for symbols the final answer cites; a
    /// miss is reported as a fabricated-reference L2 issue. A symbol defined
    /// in a C file or a shell script is a real symbol — the include list
    /// must not be blind to whole language families.
    #[test]
    fn grep_finds_symbols_outside_the_original_include_list() {
        let dir = std::env::temp_dir().join("agentsight-opt-factcheck-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("main.c"), "int parse_config(void) { return 0; }\n").unwrap();
        std::fs::write(dir.join("run.sh"), "echo parse_config\n").unwrap();

        assert!(matches!(
            FactCheckStrategy::grep_symbol(&dir, "parse_config", "function"),
            GrepOutcome::Found
        ));
        assert!(matches!(
            FactCheckStrategy::grep_symbol(&dir, "no_such_symbol_here", "function"),
            GrepOutcome::NotFound
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Same blind-spot class as above for modern file types: a symbol
    /// defined ONLY in an ES-module script, an Objective-C class file, a
    /// Lua module, or a Perl script is a real symbol the answer may cite —
    /// reporting it as fabricated (`NotFound` → L2) is a false accusation.
    #[test]
    fn grep_finds_symbols_in_modern_file_types() {
        let dir = std::env::temp_dir().join("agentsight-opt-factcheck-modern-types");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The symbol exists ONLY in files the original include list skipped,
        // one file per family.
        std::fs::write(
            dir.join("api.mjs"),
            "export function esm_only_symbol() {}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("ViewController.mm"),
            "- (void)applyEsmOnlySymbol { esm_only_symbol(); }\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("handler.lua"),
            "local function esm_only_symbol() end\n",
        )
        .unwrap();
        std::fs::write(dir.join("tool.pl"), "sub esm_only_symbol { return 1; }\n").unwrap();

        assert!(
            matches!(
                FactCheckStrategy::grep_symbol(&dir, "esm_only_symbol", "function"),
                GrepOutcome::Found
            ),
            "a symbol defined in .mjs/.mm/.lua/.pl files must not be reported as fabricated"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grep_treats_leading_dash_symbols_as_literal_patterns() {
        let dir = std::env::temp_dir().join(format!(
            "agentsight-opt-factcheck-flags-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.md"), "plain_text_only\n").unwrap();

        assert!(
            matches!(
                FactCheckStrategy::grep_symbol(&dir, "--help", "api"),
                GrepOutcome::NotFound
            ),
            "absent `--help` must not be reported Found via grep --help output"
        );
        assert!(
            matches!(
                FactCheckStrategy::grep_symbol(&dir, "-V", "api"),
                GrepOutcome::NotFound
            ),
            "absent `-V` must not be reported Found via grep -V output"
        );

        std::fs::write(
            dir.join("flags.rs"),
            "const FLAG_A: &str = \"--help\";\nconst FLAG_B: &str = \"--exclude=*\";\n",
        )
        .unwrap();
        assert!(matches!(
            FactCheckStrategy::grep_symbol(&dir, "--exclude=*", "api"),
            GrepOutcome::Found
        ));
        assert!(matches!(
            FactCheckStrategy::grep_symbol(&dir, "--help", "api"),
            GrepOutcome::Found
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_check_rejects_paths_outside_repo_root() {
        let root = std::env::temp_dir().join(format!(
            "agentsight-opt-factcheck-escape-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let outside = root.join("outside.txt");
        std::fs::write(&outside, "secret\n").unwrap();
        std::fs::write(repo.join("inside.txt"), "ok\n").unwrap();

        assert!(matches!(
            FactCheckStrategy::grep_symbol(&repo, "inside.txt", "file"),
            GrepOutcome::Found
        ));
        assert!(
            matches!(
                FactCheckStrategy::grep_symbol(&repo, "../outside.txt", "file"),
                GrepOutcome::NotFound
            ),
            "parent-dir escape must not report Found"
        );
        assert!(
            matches!(
                FactCheckStrategy::grep_symbol(&repo, outside.to_str().unwrap(), "path"),
                GrepOutcome::NotFound
            ),
            "absolute path outside repo_root must not report Found"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
