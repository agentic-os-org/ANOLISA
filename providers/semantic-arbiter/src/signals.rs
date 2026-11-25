//! Bounded candidate-request clues, never permissions or evidence of actual effects.
//!
//! Path categories refer only to conventional candidate names. In particular,
//! SharedWorkflowChange does not establish shared ownership or production use,
//! and ExternalCode does not establish that retrieved bytes are executable code.

use semantic_arbiter::contract::{Candidate, Operation};
use semantic_arbiter::local_policy::{ActionKind, ActionSignal};

use crate::command;
use crate::sensitive;
use ActionKind::*;

/// Observe local lexical facts without I/O or modifying the complete native input.
///
/// Unknown tools retain an opaque signal; their parameters remain in Candidate
/// for semantic review. Descriptions, dry-run labels and tool names grant no authority.
/// Results are deduplicated by kind/resource and capped at 64, retaining opacity
/// when either shell projection or the signal inventory is incomplete.
pub(crate) fn observe(candidate: &Candidate, workspace: &str) -> Vec<ActionSignal> {
    let mut signals = Signals::default();
    for change in &candidate.changes {
        let id = Some(change.resource_id.as_str());
        if change.operation == Operation::Delete {
            signals.add(Delete, id);
        }
        if change.resource_id == "command:unresolved" {
            signals.add(OpaqueExecution, None);
        }
        if let Some(path) = change.resource_id.strip_prefix("path:") {
            path_signals(&path.replace(':', "/"), id, &mut signals);
        }
    }
    if let Some(tool) = &candidate.native_tool {
        match tool.name.as_str() {
            "Bash" | "bash" => match tool.input.get("command").and_then(|value| value.as_str()) {
                Some(text) if !text.trim().is_empty() => {
                    for kind in command::observe_actions(text, workspace) {
                        signals.add(kind, None);
                    }
                }
                _ => signals.add(OpaqueExecution, None),
            },
            // Exact native names, not substring guesses about custom tool behavior.
            "Write" | "write" | "Edit" | "edit" | "Delete" | "delete" | "apply_patch"
            | "apply-patch" => {}
            "WebFetch" | "web_fetch" | "WebSearch" | "web_search" => {
                signals.add(ExternalCode, None);
            }
            // Native inspection tools read without mutating; a recursive content
            // search may reach credential loci its ignore rules do not exclude.
            "Read" | "read" => {
                signals.add(ReadOnly, None);
                if tool
                    .input
                    .get("file_path")
                    .or_else(|| tool.input.get("path"))
                    .and_then(|value| value.as_str())
                    .is_some_and(sensitive::path)
                {
                    signals.add(SensitiveRead, None);
                }
            }
            "Grep" | "grep" => {
                signals.add(ReadOnly, None);
                signals.add(SensitiveRead, None);
            }
            "Glob" | "glob" | "LS" | "ls" => signals.add(ReadOnly, None),
            _ => signals.add(OpaqueExecution, None),
        }
    }
    if signals.0.iter().any(|s| s.kind == TestChange) {
        if let Some(signal) = assertion_weakening(candidate) {
            signals.add(signal.kind, signal.resource_id.as_deref());
        }
    }
    signals.0
}

#[derive(Default)]
struct Signals(Vec<ActionSignal>);

impl Signals {
    fn add(&mut self, kind: ActionKind, resource_id: Option<&str>) {
        let signal = ActionSignal {
            kind,
            resource_id: resource_id.map(str::to_owned),
        };
        if self.0.contains(&signal) {
            return;
        }
        if self.0.len() < 64 {
            self.0.push(signal);
        } else if !self.0.iter().any(|signal| signal.kind == OpaqueExecution) {
            // Overflow itself loses projection detail. Reserve the final slot so
            // later unknown execution can never be silently truncated away.
            self.0[63] = ActionSignal {
                kind: OpaqueExecution,
                resource_id: None,
            };
        }
    }
}

fn path_signals(path: &str, id: Option<&str>, signals: &mut Signals) {
    let parts: Vec<_> = path.split('/').collect();
    let leaf = parts.last().copied().unwrap_or_default();
    if parts
        .iter()
        .any(|part| matches!(*part, "test" | "tests" | "__tests__" | "spec" | "specs"))
        || (leaf.ends_with(".py")
            && (leaf.starts_with("test_")
                || leaf.ends_with("_test.py")
                || matches!(leaf, "test.py" | "conftest.py")))
        || [
            ".test.js",
            ".test.jsx",
            ".test.ts",
            ".test.tsx",
            ".spec.js",
            ".spec.ts",
            ".spec.tsx",
            "_test.go",
            "_test.rs",
        ]
        .iter()
        .any(|suffix| leaf.ends_with(suffix))
    {
        signals.add(TestChange, id);
    }
    if parts
        .iter()
        .any(|part| matches!(*part, "config" | "configs" | ".config"))
        || [
            ".toml",
            ".ini",
            ".yaml",
            ".yml",
            ".cfg",
            ".conf",
            ".config.js",
            ".config.ts",
            ".config.json",
        ]
        .iter()
        .any(|suffix| leaf.ends_with(suffix))
        || matches!(
            leaf,
            ".editorconfig" | ".npmrc" | ".gitconfig" | ".env" | "tsconfig.json"
        )
    {
        signals.add(ConfigChange, id);
    }
    if parts
        .windows(2)
        .any(|pair| pair == [".github", "workflows"])
        || parts
            .iter()
            .any(|part| matches!(*part, ".circleci" | ".buildkite" | "ci" | "CI"))
        || matches!(
            leaf,
            ".gitlab-ci.yml" | "Jenkinsfile" | ".travis.yml" | "azure-pipelines.yml"
        )
    {
        signals.add(SharedWorkflowChange, id);
    }
    if matches!(
        leaf,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | "Cargo.toml"
            | "Cargo.lock"
            | "pyproject.toml"
            | "uv.lock"
            | "poetry.lock"
            | "Pipfile"
            | "Pipfile.lock"
            | "setup.py"
            | "setup.cfg"
            | "requirements.txt"
            | "go.mod"
            | "go.sum"
            | "Gemfile"
            | "Gemfile.lock"
            | "composer.json"
            | "composer.lock"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
    ) || (leaf.starts_with("requirements-") && leaf.ends_with(".txt"))
        || (parts.contains(&"requirements") && leaf.ends_with(".txt"))
    {
        signals.add(DependencyChange, id);
    }
    if matches!(
        leaf,
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | ".zprofile" | "crontab" | "rc.local"
    ) || parts.iter().any(|part| {
        matches!(
            *part,
            "cron.d" | "cron.daily" | "cron.hourly" | "cron.weekly" | "cron.monthly" | "init.d"
        )
    }) || parts.windows(2).any(|pair| {
        matches!(
            pair,
            ["systemd", "system" | "user"] | [".config", "autostart"] | [".git", "hooks"]
        )
    }) {
        signals.add(Persistence, id);
    }
}

/// Detects assertion removal or weakening in an Edit to a test file.
fn assertion_weakening(candidate: &Candidate) -> Option<ActionSignal> {
    let tool = candidate.native_tool.as_ref()?;
    if !tool.name.eq_ignore_ascii_case("edit") {
        return None;
    }
    let old = tool
        .input
        .get("old_string")
        .or_else(|| tool.input.get("old_str"))?
        .as_str()?;
    let new = tool
        .input
        .get("new_string")
        .or_else(|| tool.input.get("new_str"))?
        .as_str()?;
    let weakened = (old.contains("assertRaises") && !new.contains("assertRaises"))
        || (old.contains("assertEqual") && !new.contains("assertEqual"))
        || (old.contains("def test_") && !new.contains("def test_"))
        || (!old.contains("@unittest.skip") && new.contains("@unittest.skip"))
        || (!old.contains("pytest.mark.skip") && new.contains("pytest.mark.skip"))
        || (!old.contains("@pytest.mark.skip") && new.contains("@pytest.mark.skip"));
    if !weakened {
        return None;
    }
    let resource_id = candidate
        .changes
        .first()
        .map(|change| change.resource_id.clone());
    Some(ActionSignal {
        kind: AssertionWeakening,
        resource_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantic_arbiter::contract::{Change, NativeTool};
    use serde_json::json;

    fn shell(text: &str) -> Candidate {
        Candidate {
            changes: vec![],
            native_tool: Some(NativeTool {
                name: "Bash".into(),
                input: json!({"command":text}),
            }),
        }
    }

    fn kinds(text: &str) -> Vec<ActionKind> {
        observe(&shell(text), "/work")
            .into_iter()
            .map(|signal| signal.kind)
            .collect()
    }

    #[test]
    fn static_commands_include_requested_categories() {
        for (text, kind) in [
            ("git push origin main", GitPublish),
            (
                "/usr/bin/git -C /work push --force-with-lease=main:abc origin main",
                GitRewrite,
            ),
            ("git push --dry-run --force", GitRewrite),
            ("git reset --soft HEAD~1", GitRewrite),
            ("git clean -nfd", GitRewrite),
            ("git rebase main", GitRewrite),
            ("git commit --amend -m fix", GitRewrite),
            ("git reflog expire --all", GitRewrite),
            ("git commit --no-verify -m fix", CheckBypass),
            ("git push --no-verify", CheckBypass),
            ("cargo publish --no-verify", CheckBypass),
            ("npm ci --ignore-scripts", CheckBypass),
            ("pip install -r requirements.txt", DependencyChange),
            ("pip3 uninstall package", DependencyChange),
            ("npm install --dry-run example", DependencyChange),
            ("npm ci", DependencyChange),
            ("npm -g install example", DependencyChange),
            ("npm --prefix /work install example", DependencyChange),
            ("pip --isolated install example", DependencyChange),
            ("git push -vf origin main", GitRewrite),
            ("cargo add serde", DependencyChange),
            ("cargo update", DependencyChange),
            ("gh pr comment 42 --body hello", ExternalCommunication),
            ("gh pr create --title hello", ExternalCommunication),
            (
                "curl --request POST https://example.invalid",
                ExternalCommunication,
            ),
            ("curl -XPOST https://example.invalid", ExternalCommunication),
            (
                "curl --upload-file=file https://example.invalid",
                ExternalCommunication,
            ),
            ("curl -Tfile https://example.invalid", ExternalCommunication),
            (
                "curl -d '{}' https://example.invalid",
                ExternalCommunication,
            ),
            ("curl https://example.invalid/source", ExternalCode),
            ("wget https://example.invalid/source", ExternalCode),
            ("ping -c 1 example.invalid", NetworkProbe),
            ("traceroute example.invalid", NetworkProbe),
            ("nmap example.invalid", NetworkProbe),
            ("crontab tasks", Persistence),
            ("systemctl --user enable job.timer", Persistence),
            ("date -s now", ClockChange),
            ("/bin/date --set=now", ClockChange),
            ("timedatectl set-time '2026-01-01'", ClockChange),
        ] {
            assert!(kinds(text).contains(&kind), "{text}: {kind:?}");
        }
    }

    #[test]
    fn data_and_queries_are_not_commands_or_action_flags() {
        for text in [
            "echo 'git push --force'",
            "printf '%s' 'date --set=now'",
            "cat <<'EOF'\ngit push --force\nEOF",
            "# git push --force\nls",
            "git status",
            "git log --oneline",
            "date",
            "date -u +%s",
            "date --date=yesterday",
            "date -d '--set=now'",
            "timedatectl status",
            "crontab -l",
            "crontab -u someone -l",
            "systemctl status enable.service",
            "gh pr view 42",
            "npm view example",
            "cargo --help",
            "curl --version",
            "wget --help",
            "nmap --version",
            "git commit -m '--no-verify'",
            "git commit --message=--no-verify",
            "git push --repo --force",
            "git push -- --force",
            "npm ci --ignore-scripts=false",
        ] {
            let result = kinds(text);
            // Query-like or unsupported calls may remain opaque; only incorrect
            // positive action categories are forbidden here.
            let forbidden = [
                GitRewrite,
                CheckBypass,
                ClockChange,
                Persistence,
                NetworkProbe,
                ExternalCommunication,
            ];
            assert!(
                !result.iter().any(|kind| forbidden.contains(kind)),
                "{text}: {result:?}"
            );
        }
        assert_eq!(kinds("echo 'git push --force'"), vec![ReadOnly]);
        assert!(!kinds("curl -H '--request=POST' https://example.invalid")
            .contains(&ExternalCommunication));
        assert!(!kinds("echo 'npm install x'").contains(&DependencyChange));
    }

    #[test]
    fn compounds_and_quoted_programs_share_the_lexer() {
        let result = kinds("echo 'git push --force'; '/usr/bin/git' push --force-with-lease && date --set=now | cat");
        for kind in [GitPublish, GitRewrite, ClockChange, OpaqueExecution] {
            assert!(result.contains(&kind), "{result:?}");
        }
        assert!(kinds("echo okay\n/usr/bin/ping example.invalid").contains(&NetworkProbe));
    }

    #[test]
    fn wrappers_and_dynamic_execution_remain_unknown() {
        for text in [
            "env git push --force",
            "sudo git push --force",
            "/usr/bin/env git push --force",
            "sudo -u root date -s now",
            "$PROGRAM push --force",
            "git $ACTION --force",
            "echo $(git push --force)",
            "echo `git push --force`",
            "./custom --version",
            "echo 'unterminated",
            "(git push --force)",
            "python script.py",
        ] {
            let result = kinds(text);
            assert!(result.contains(&OpaqueExecution), "{text}: {result:?}");
            assert!(!result.contains(&GitRewrite), "{text}: {result:?}");
        }
    }

    #[test]
    fn path_clues_are_candidate_bound_not_proven_shared_state() {
        for (path, kind) in [
            ("test_parser.py", TestChange),
            ("pkg:parser_test.py", TestChange),
            ("tests:unit.rs", TestChange),
            ("ui:thing.spec.ts", TestChange),
            (".github:workflows:check.yml", SharedWorkflowChange),
            ("ci:check.sh", SharedWorkflowChange),
            ("config:app.json", ConfigChange),
            ("settings.toml", ConfigChange),
            ("package.json", DependencyChange),
            ("Cargo.lock", DependencyChange),
            ("requirements-dev.txt", DependencyChange),
            ("pnpm-lock.yaml", DependencyChange),
            (".config:systemd:user:job.service", Persistence),
            (".bashrc", Persistence),
        ] {
            let id = format!("path:{path}");
            let candidate = Candidate {
                changes: vec![Change {
                    operation: Operation::Replace,
                    resource_id: id.clone(),
                }],
                native_tool: None,
            };
            assert!(
                observe(&candidate, "/work").contains(&ActionSignal {
                    kind,
                    resource_id: Some(id)
                }),
                "{path}"
            );
        }
        let candidate = Candidate {
            changes: vec![Change {
                operation: Operation::Replace,
                resource_id: "custom:test_config".into(),
            }],
            native_tool: None,
        };
        assert!(observe(&candidate, "/work").is_empty());
    }

    #[test]
    fn structured_names_are_exact_and_custom_parameters_remain_intact() {
        let mut candidate = shell("git push --force");
        candidate.native_tool.as_mut().unwrap().name = "custom_Bash".into();
        candidate.native_tool.as_mut().unwrap().input["native"] =
            json!({"dry_run":true,"payload":[1,2,3]});
        let original = candidate.clone();
        assert_eq!(
            observe(&candidate, "/work"),
            vec![ActionSignal {
                kind: OpaqueExecution,
                resource_id: None
            }]
        );
        assert_eq!(candidate, original);
        candidate.native_tool.as_mut().unwrap().name = "WebFetch".into();
        assert!(observe(&candidate, "/work")
            .iter()
            .any(|signal| signal.kind == ExternalCode));
        candidate.native_tool.as_mut().unwrap().name = "Write".into();
        assert!(observe(&candidate, "/work").is_empty());
    }

    #[test]
    fn deduplication_and_bounds_never_drop_opacity() {
        let mut candidate = shell("git push; git push; unknown_program");
        candidate.changes = (0..90)
            .map(|index| Change {
                operation: Operation::Delete,
                resource_id: format!("path:tests:test_{index}.py"),
            })
            .collect();
        let result = observe(&candidate, "/work");
        assert_eq!(result.len(), 64);
        assert!(result.iter().any(|signal| signal.kind == OpaqueExecution));
        for (index, signal) in result.iter().enumerate() {
            assert!(!result[..index].contains(signal));
        }
        assert_eq!(
            kinds("git push; git push")
                .iter()
                .filter(|kind| **kind == GitPublish)
                .count(),
            1
        );
        candidate.changes.truncate(1);
        candidate.changes.push(candidate.changes[0].clone());
        let result = observe(&candidate, "/work");
        assert_eq!(
            result
                .iter()
                .filter(|signal| signal.kind == TestChange)
                .count(),
            1
        );
    }

    #[test]
    fn assertion_weakening_detected_only_for_test_edits() {
        let edit = |old: &str, new: &str, path: &str| Candidate {
            changes: vec![Change {
                operation: Operation::Replace,
                resource_id: format!("path:{path}"),
            }],
            native_tool: Some(NativeTool {
                name: "Edit".into(),
                input: json!({"file_path": path, "old_string": old, "new_string": new}),
            }),
        };
        let weakening = edit(
            "with self.assertRaises(ValueError):\n            normalize_name('')",
            "self.assertEqual(normalize_name(''), '')",
            "tests/test_app.py",
        );
        let result = observe(&weakening, "/work");
        assert!(result.iter().any(|s| s.kind == AssertionWeakening));
        assert!(result.iter().any(|s| s.kind == TestChange));

        let adding = edit(
            "    def test_spaces(self):\n        self.assertEqual(normalize_name(' A '), 'a')",
            "    def test_spaces(self):\n        self.assertEqual(normalize_name(' A '), 'a')\n    def test_empty(self):\n        self.assertEqual(normalize_name(''), '')",
            "tests/test_app.py",
        );
        let result = observe(&adding, "/work");
        assert!(!result.iter().any(|s| s.kind == AssertionWeakening));

        let non_test = edit(
            "assertRaises(ValueError)",
            "assertEqual(x, y)",
            "src/app.py",
        );
        let result = observe(&non_test, "/work");
        assert!(!result.iter().any(|s| s.kind == AssertionWeakening));

        let skip_added = edit(
            "    def test_hard(self):\n        self.assertEqual(f(), 1)",
            "    @unittest.skip\n    def test_hard(self):\n        self.assertEqual(f(), 1)",
            "tests/test_app.py",
        );
        assert!(observe(&skip_added, "/work")
            .iter()
            .any(|s| s.kind == AssertionWeakening));
    }

    #[test]
    fn working_state_destruction_only_for_working_tree_discards() {
        for text in [
            "git reset --hard HEAD",
            "git clean -fd",
            "git clean -fdx",
            "git checkout -- src/main.rs",
            "git restore .",
            "git restore --worktree src/main.rs",
            "git restore -SW src/main.rs",
        ] {
            assert!(kinds(text).contains(&WorkingStateDestruction), "{text}");
        }
        for text in [
            "git reset --soft HEAD~1",
            "git reset --mixed HEAD~1",
            "git reset HEAD~1",
            "git clean -nfd",
            "git clean --dry-run",
            "git restore --staged .",
            "git restore -S src/main.rs",
            "git checkout main",
            "git push --force origin main",
            "git rebase main",
        ] {
            assert!(!kinds(text).contains(&WorkingStateDestruction), "{text}");
        }
    }

    #[test]
    fn sensitive_reads_are_signalized_for_shell_and_inspection_tools() {
        for text in [
            "cat .env",
            "head -n 5 .env.local",
            "grep -r KEY .",
            "rg KEY src",
            "printenv",
            "sort .bash_history",
            "cat ~/.ssh/id_rsa",
        ] {
            let result = kinds(text);
            // Tilde expansion is dynamic; the opaque remainder still routes to
            // the model, and the named locus is signalized where attributable.
            if text.contains('~') {
                assert!(result.contains(&OpaqueExecution), "{text}");
            } else {
                assert!(result.contains(&SensitiveRead), "{text}");
            }
        }
        assert!(!kinds("cat src/main.rs").contains(&SensitiveRead));
        assert!(!kinds("grep KEY src/main.rs").contains(&SensitiveRead));
        assert!(!kinds("ls -la .ssh").contains(&SensitiveRead));

        let inspect = |name: &str, input: serde_json::Value| {
            observe(
                &Candidate {
                    changes: vec![],
                    native_tool: Some(NativeTool {
                        name: name.into(),
                        input,
                    }),
                },
                "/work",
            )
        };
        assert_eq!(
            inspect("Read", json!({"file_path": "src/main.rs"})),
            vec![ActionSignal {
                kind: ReadOnly,
                resource_id: None
            }]
        );
        assert!(inspect("Read", json!({"file_path": ".env"}))
            .iter()
            .any(|signal| signal.kind == SensitiveRead));
        assert!(inspect("Grep", json!({"pattern": "KEY"}))
            .iter()
            .any(|signal| signal.kind == SensitiveRead));
        assert_eq!(
            inspect("Glob", json!({"pattern": "**/*.rs"})),
            vec![ActionSignal {
                kind: ReadOnly,
                resource_id: None
            }]
        );
        assert_eq!(
            inspect("LS", json!({"path": "."})),
            vec![ActionSignal {
                kind: ReadOnly,
                resource_id: None
            }]
        );
    }

    #[test]
    fn read_only_signal_for_fully_attributed_commands() {
        assert!(kinds("cat results/previous.csv results/notes.txt").contains(&ReadOnly));
        assert!(kinds("ls -la").contains(&ReadOnly));
        assert!(kinds("git status").contains(&ReadOnly));
        assert!(kinds("git log --oneline").contains(&ReadOnly));
        assert!(!kinds("rm file").contains(&ReadOnly));
        assert!(!kinds("echo hi > file").contains(&ReadOnly));
        assert!(!kinds("python script.py").contains(&ReadOnly));
    }
}
