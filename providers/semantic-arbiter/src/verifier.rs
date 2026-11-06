//! Git-backed local rebuild verification for the deterministic preservation check.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;

use semantic_arbiter::contract::{EvidenceFact, Provenance, RebuildResult, Scenario};
use semantic_arbiter::deadline::Ingress;
use semantic_arbiter::evidence::{
    EvidenceRequest, EvidenceSource, EvidenceSourceDescriptor, EvidenceSourceError,
    EvidenceSourceOutput, SourceEvidence,
};
use serde_json::json;
use tokio::process::Command;
use tokio::time::Instant;

/// Registry identity of the git criterion.
pub(crate) const SOURCE_ID: &str = "git-rebuild";
/// Criterion revision recorded on every collected item.
pub(crate) const SOURCE_REVISION: &str = "git-v1";

/// Local git invocations are short; the registry deadline stays authoritative over them.
const TIMEOUT_MS: u64 = 5_000;
/// Lifetime of one collected fact, and the age the registry tolerates for it.
const VALID_FOR_MS: u64 = 30_000;
/// One rebuild fact describes one candidate target.
const MAX_ITEMS: usize = 1;
/// Serialized ceiling for a single bound rebuild fact.
const MAX_BYTES: usize = 1_024;
/// Domain separating rebuild evidence identities from every other derived identity.
const IDENTITY_DOMAIN: &str = "git-rebuild-evidence";

/// Establishes whether a candidate target's current bytes are recoverable from git.
///
/// This is a local policy input, not a model capability: the deterministic check consumes
/// the fact without consulting a decision model, so its result is reproducible offline.
///
/// Constructed per request, because the resource identity it speaks for and the path that
/// identity denotes are both fixed by that request. The arbiter core never sees the path.
pub(crate) struct GitRebuildSource {
    descriptor: EvidenceSourceDescriptor,
    resource_id: String,
    /// Git working directory for every invocation.
    workspace: PathBuf,
    /// Repository-discovery ceiling, or `None` when nothing lies above the workspace.
    ceiling: Option<PathBuf>,
    /// Candidate target: absolute, and lexically inside `workspace`.
    path: PathBuf,
    /// Target relative to `workspace`; the form every git pathspec here uses.
    relative: String,
    ingress: Ingress,
}

impl GitRebuildSource {
    /// Bind one resource identity to its workspace-relative target.
    ///
    /// Returns `None` when either path is not absolute, when `path` is not strictly inside
    /// `workspace`, or when its relative form contains an empty, `.` or `..` component that
    /// would let git resolve outside the bound workspace.
    pub(crate) fn new(
        resource_id: &str,
        workspace: &str,
        path: &str,
        ingress: Ingress,
    ) -> Option<Self> {
        let relative = relative_to(path, workspace)?;
        let workspace = PathBuf::from(workspace);
        // The ceiling must be the parent, not the workspace itself: git consults the ceiling
        // only when it walks out of a directory, so listing the workspace would still let it
        // adopt a repository above whenever the workspace has no `.git` of its own.
        let ceiling = workspace.parent().map(Path::to_path_buf);
        Some(Self {
            descriptor: EvidenceSourceDescriptor {
                source_id: SOURCE_ID.into(),
                source_revision: SOURCE_REVISION.into(),
                // The deterministic check only accepts a local comparison as proof of
                // reconstruction. Asserting this provenance is not authentication.
                provenance: Provenance::LocalVerifier,
                supported_scenarios: vec![Scenario::Preservation],
                max_evidence_items: MAX_ITEMS,
                max_evidence_bytes: MAX_BYTES,
                max_age_ms: VALID_FOR_MS,
                timeout_ms: TIMEOUT_MS,
            },
            resource_id: resource_id.to_owned(),
            workspace,
            ceiling,
            path: PathBuf::from(path),
            relative,
            ingress,
        })
    }

    /// Compare the target's working-tree blob against every blob git retains for it.
    async fn classify(&self) -> RebuildResult {
        // Stat rather than open, so a missing target is reported without invoking git while
        // an unreadable one stays undetermined instead of being mistaken for absence.
        match tokio::fs::symlink_metadata(&self.path).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return RebuildResult::Absent
            }
            Err(_) => return RebuildResult::Unknown,
            Ok(_) => {}
        }
        // Clean filters are applied on purpose: a filter changes the stored bytes while its
        // matching smudge filter reproduces the working bytes, so the content is recoverable
        // even though a raw byte comparison would report it as unique.
        let Some(working) = self
            .git(&["hash-object", "--", self.relative.as_str()], 0)
            .await
        else {
            return RebuildResult::Unknown;
        };
        let index = self
            .git(&["ls-files", "-s", "--", self.relative.as_str()], 1)
            .await;
        let head = self
            .git(&["ls-tree", "HEAD", "--", self.relative.as_str()], 2)
            .await;
        if index.as_deref() == Some(working.as_str()) || head.as_deref() == Some(working.as_str()) {
            RebuildResult::Identical
        } else if index.is_some() || head.is_some() {
            RebuildResult::Unique
        } else {
            // Git holds nothing describing these bytes. They may still be reproducible by a
            // build recipe this source cannot know, so it declines rather than claims loss.
            RebuildResult::Unknown
        }
    }

    /// Run one git subcommand in the workspace and extract a single object identifier.
    ///
    /// Returns `None` when git is unusable, the path is not described, or the output is
    /// ambiguous. Exit status is the only success signal: git writes warnings to stderr
    /// while still succeeding, and some subcommands echo their pathspec when they fail.
    async fn git(&self, args: &[&str], id_field: usize) -> Option<String> {
        let mut command = Command::new("git");
        command
            .arg("--no-pager")
            .args(args)
            .current_dir(&self.workspace)
            // Any prompt would outlive the registry deadline and hang the collection.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(ceiling) = &self.ceiling {
            // Without a ceiling git adopts a repository above the bound workspace and reports
            // blobs from a history that does not describe this workspace. A `.git` inside the
            // workspace is still honored, because git resolves it before consulting the ceiling.
            command.env("GIT_CEILING_DIRECTORIES", ceiling);
        }
        let output = command.output().await.ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let line = lines.next()?;
        // Several entries mean git describes multiple stages for this path, as in an
        // unresolved merge, so no single identity can be claimed.
        lines.next().is_none().then_some(())?;
        object_id(line, id_field)
    }
}

impl EvidenceSource for GitRebuildSource {
    fn descriptor(&self) -> EvidenceSourceDescriptor {
        self.descriptor.clone()
    }

    fn collect<'a>(
        &'a self,
        request: EvidenceRequest<'a>,
        _deadline: Instant,
    ) -> Pin<Box<dyn Future<Output = Result<EvidenceSourceOutput, EvidenceSourceError>> + Send + 'a>>
    {
        Box::pin(async move {
            // A source speaks only for the candidate it was constructed with; any other
            // resource is an explicit coverage gap, never a fact about something else.
            if request.candidate.resource_id != self.resource_id {
                return Ok(EvidenceSourceOutput {
                    evidence: vec![],
                    gaps: vec![Scenario::Preservation],
                });
            }
            // Measured before the git work and in the ingress clock domain, which the
            // registry also uses for its staleness recheck. A fresh system-clock reading
            // could jump ahead of it and get the whole batch discarded as stale.
            let captured_at_ms = self.ingress.now_ms();
            let result = self.classify().await;
            let id = crate::mapping::identity(IDENTITY_DOMAIN, &json!([self.resource_id]))
                .map_err(|_| EvidenceSourceError::Unavailable)?;
            Ok(EvidenceSourceOutput {
                evidence: vec![SourceEvidence {
                    id,
                    captured_at_ms,
                    valid_until_ms: captured_at_ms.saturating_add(VALID_FOR_MS),
                    fact: EvidenceFact::Rebuild {
                        resource_id: self.resource_id.clone(),
                        result,
                    },
                }],
                gaps: vec![],
            })
        })
    }
}

/// Workspace-relative pathspec form, or `None` when `path` is not strictly inside `workspace`.
fn relative_to(path: &str, workspace: &str) -> Option<String> {
    if !crate::config::absolute(path) || !crate::config::absolute(workspace) {
        return None;
    }
    let relative = path
        .strip_prefix(workspace.trim_end_matches('/'))?
        .trim_start_matches('/');
    let usable = !relative.is_empty()
        && !relative
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == "..");
    usable.then(|| relative.to_owned())
}

/// Full object identifier at `field`, rejecting anything that is not one.
fn object_id(line: &str, field: usize) -> Option<String> {
    let candidate = line.split_whitespace().nth(field)?;
    // SHA-1 and SHA-256 repositories only, so an echoed pathspec is never an identifier.
    let valid = (candidate.len() == 40 || candidate.len() == 64)
        && candidate.bytes().all(|byte| byte.is_ascii_hexdigit());
    valid.then(|| candidate.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use semantic_arbiter::check::check;
    use semantic_arbiter::contract::{
        AnswerValue, Binding, Budget, Candidate, Operation, Reason, Task, TaskOrigin,
    };
    use semantic_arbiter::evidence::EvidenceSourceRegistry;
    use semantic_arbiter::protocol::{
        CheckMode, CheckRequest, CHECK_API_VERSION, MAX_EVIDENCE_ITEMS, MAX_MESSAGE_BYTES,
    };

    /// git is a build prerequisite of this repository, so its absence is a test failure
    /// rather than a skip: a silently skipped criterion proves nothing.
    fn git_run(dir: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .expect("git must be runnable")
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = git_run(dir, args);
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_ok(dir: &Path, args: &[&str]) -> bool {
        git_run(dir, args).status.success()
    }

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        String::from_utf8_lossy(&git_run(dir, args).stdout).into_owned()
    }

    fn committed(dir: &Path) {
        git(dir, &["init", "-q", "."]);
        std::fs::write(dir.join("tracked.txt"), "v1\n").expect("fixture write");
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", "init"]);
    }

    fn source(workspace: &Path, relative: &str) -> GitRebuildSource {
        let ingress = Ingress::capture().expect("clock must be available");
        GitRebuildSource::new(
            &format!("path:{}", relative.replace('/', ":")),
            workspace.to_str().expect("utf-8 workspace"),
            workspace.join(relative).to_str().expect("utf-8 path"),
            ingress,
        )
        .expect("fixture paths are contained")
    }

    async fn classify(workspace: &Path, relative: &str) -> RebuildResult {
        let source = source(workspace, relative);
        assert_eq!(source.descriptor().source_id, SOURCE_ID);
        assert_eq!(source.descriptor().provenance, Provenance::LocalVerifier);
        source.classify().await
    }

    #[tokio::test]
    async fn clean_tracked_file_is_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        assert_eq!(
            classify(dir.path(), "tracked.txt").await,
            RebuildResult::Identical
        );
    }

    #[tokio::test]
    async fn modified_tracked_file_is_unique() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        std::fs::write(dir.path().join("tracked.txt"), "v2\n").expect("fixture write");
        assert_eq!(
            classify(dir.path(), "tracked.txt").await,
            RebuildResult::Unique
        );
    }

    #[tokio::test]
    async fn staged_modification_is_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        std::fs::write(dir.path().join("tracked.txt"), "v2\n").expect("fixture write");
        git(dir.path(), &["add", "tracked.txt"]);
        // Git holds the exact working bytes in its object store, so nothing is unrecoverable
        // even though no commit describes them yet.
        assert_eq!(
            classify(dir.path(), "tracked.txt").await,
            RebuildResult::Identical
        );
    }

    #[tokio::test]
    async fn untracked_file_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        std::fs::write(dir.path().join("build.log"), "output\n").expect("fixture write");
        assert_eq!(
            classify(dir.path(), "build.log").await,
            RebuildResult::Unknown
        );
    }

    #[tokio::test]
    async fn missing_file_is_absent() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        assert_eq!(
            classify(dir.path(), "never-existed.txt").await,
            RebuildResult::Absent
        );
    }

    #[tokio::test]
    async fn directory_target_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        std::fs::create_dir(dir.path().join("assets")).expect("fixture mkdir");
        assert_eq!(classify(dir.path(), "assets").await, RebuildResult::Unknown);
    }

    #[tokio::test]
    async fn file_outside_a_repository_is_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("loose.txt"), "v1\n").expect("fixture write");
        assert_eq!(
            classify(dir.path(), "loose.txt").await,
            RebuildResult::Unknown
        );
    }

    #[tokio::test]
    async fn parent_repository_is_not_adopted() {
        let outer = tempfile::tempdir().expect("tempdir");
        committed(outer.path());
        // A workspace that is not itself a repository must not inherit the parent's history:
        // those blobs describe the parent, not this workspace.
        let inner = outer.path().join("workspace");
        std::fs::create_dir(&inner).expect("fixture mkdir");
        std::fs::write(inner.join("tracked.txt"), "v1\n").expect("fixture write");
        git(outer.path(), &["add", "-A"]);
        git(outer.path(), &["commit", "-q", "-m", "adopt"]);
        assert_eq!(
            classify(&inner, "tracked.txt").await,
            RebuildResult::Unknown
        );
    }

    #[tokio::test]
    async fn staged_file_in_a_repository_without_commits_is_identical() {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-q", "."]);
        std::fs::write(dir.path().join("new.txt"), "v1\n").expect("fixture write");
        git(dir.path(), &["add", "new.txt"]);
        // HEAD does not resolve yet, which must not be reported as an unusable repository.
        assert_eq!(
            classify(dir.path(), "new.txt").await,
            RebuildResult::Identical
        );
    }

    #[tokio::test]
    async fn conflicted_path_reports_unique_because_no_blob_holds_conflict_markers() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        git(dir.path(), &["checkout", "-q", "-b", "side"]);
        std::fs::write(dir.path().join("tracked.txt"), "v3\n").expect("fixture write");
        git(dir.path(), &["commit", "-qam", "side"]);
        git(dir.path(), &["checkout", "-q", "-"]);
        std::fs::write(dir.path().join("tracked.txt"), "v2\n").expect("fixture write");
        git(dir.path(), &["commit", "-qam", "base"]);
        assert!(
            !git_ok(dir.path(), &["merge", "side"]),
            "fixture merge must conflict"
        );
        // Assert the precondition instead of trusting the merge exit status, which also fails
        // for an unknown ref and would silently leave an unconflicted working tree behind.
        assert_eq!(
            git_stdout(dir.path(), &["ls-files", "-u", "--", "./tracked.txt"])
                .lines()
                .count(),
            3,
            "fixture must leave three index stages"
        );
        assert!(
            std::fs::read_to_string(dir.path().join("tracked.txt"))
                .expect("fixture read")
                .contains("<<<<<<<"),
            "fixture must write conflict markers"
        );
        // Three stages leave the index without a single identity, but HEAD still supplies a
        // comparison, and git holds no blob equal to the marked-up working file.
        assert_eq!(
            classify(dir.path(), "tracked.txt").await,
            RebuildResult::Unique
        );
    }

    #[tokio::test]
    async fn constructor_rejects_paths_outside_the_workspace() {
        let ingress = Ingress::capture().expect("clock must be available");
        for (workspace, path) in [
            ("/work", "/other/file.txt"),
            ("/work", "/work"),
            ("/work", "/work/../etc/passwd"),
            ("/work", "/work/./file.txt"),
            ("work", "/work/file.txt"),
            ("/work", "file.txt"),
        ] {
            assert!(
                GitRebuildSource::new("path:file.txt", workspace, path, ingress).is_none(),
                "{workspace} + {path} must not bind"
            );
        }
        assert!(
            GitRebuildSource::new("path:file.txt", "/work", "/work/file.txt", ingress).is_some()
        );
    }

    #[test]
    fn object_id_rejects_anything_that_is_not_a_full_identifier() {
        // Some git subcommands echo their pathspec when they fail, so a parser that merely
        // reads the expected field would mistake that echo for a blob identity.
        assert_eq!(object_id(":./tracked.txt", 0), None);
        assert_eq!(object_id("fatal: not a git repository", 0), None);
        assert_eq!(object_id("", 0), None);
        assert_eq!(object_id("100644 110ed9b 0\ttracked.txt", 1), None);
        assert_eq!(object_id(&"g".repeat(40), 0), None);
        assert_eq!(
            object_id(
                "100644 blob deadbeefdeadbeefdeadbeefdeadbeefdeadbeef\tf.txt",
                2
            ),
            Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_owned())
        );
        assert_eq!(object_id(&"f".repeat(64), 0), Some("f".repeat(64)));
    }

    #[tokio::test]
    async fn foreign_candidate_reports_a_gap_instead_of_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        let source = source(dir.path(), "tracked.txt");
        let mut request = preservation_request("path:other:file.txt");
        request.budget.deadline_ms = source.ingress.now_ms() + 5_000;
        let mut registry = EvidenceSourceRegistry::new();
        registry.register(source).expect("descriptor must be valid");
        let collection = registry
            .collect(
                &[SOURCE_ID.into()],
                EvidenceRequest::from(&request),
                Ingress::capture().expect("clock must be available"),
            )
            .await
            .expect("collection must succeed");
        assert!(collection.evidence.is_empty());
        assert_eq!(collection.gaps.len(), 1);
        assert_eq!(collection.gaps[0].scenario, Scenario::Preservation);
        assert_eq!(collection.gaps[0].source_id, SOURCE_ID);
    }

    /// Deterministic preservation end to end: collected git fact in, outcome out, no model.
    async fn preservation_outcome(relative: &str) -> (Reason, AnswerValue) {
        let dir = tempfile::tempdir().expect("tempdir");
        committed(dir.path());
        std::fs::write(dir.path().join(relative), "v2\n").expect("fixture write");
        let resource_id = format!("path:{}", relative.replace('/', ":"));
        let source = source(dir.path(), relative);
        let ingress = Ingress::capture().expect("clock must be available");
        let mut request = preservation_request(&resource_id);
        request.budget.deadline_ms = ingress.now_ms() + 5_000;
        let mut registry = EvidenceSourceRegistry::new();
        registry.register(source).expect("descriptor must be valid");
        let collection = registry
            .collect(
                &[SOURCE_ID.into()],
                EvidenceRequest::from(&request),
                ingress,
            )
            .await
            .expect("collection must succeed");
        request.check_at_ms = collection.collected_at_ms;
        request.evidence = collection.evidence;
        request.gaps = collection.gaps;
        let response = check(&request).expect("request must satisfy the contract");
        let answer = response
            .answers
            .iter()
            .find(|answer| answer.unit == Scenario::Preservation)
            .expect("preservation was requested");
        (answer.reason, answer.value)
    }

    fn preservation_request(resource_id: &str) -> CheckRequest {
        let now_ms = Ingress::capture()
            .expect("clock must be available")
            .now_ms();
        CheckRequest {
            api_version: CHECK_API_VERSION.into(),
            request_id: "verifier-test".into(),
            mode: CheckMode::DeterministicCheck,
            binding: Binding {
                session_id: "session-test".into(),
                instance_id: "instance-test".into(),
                workspace_id: "workspace-test".into(),
                task_version: "task-v1".into(),
                config_revision: "policy-v1".into(),
                candidate_id: "candidate-test".into(),
                candidate_revision: "candidate-v1".into(),
            },
            budget: Budget {
                opened_at_ms: now_ms.saturating_sub(1),
                deadline_ms: now_ms + 5_000,
                max_evidence_items: MAX_EVIDENCE_ITEMS,
                max_evidence_bytes: MAX_MESSAGE_BYTES,
            },
            check_at_ms: now_ms,
            scenarios: vec![Scenario::Preservation],
            task: Some(Task {
                session_id: "session-test".into(),
                version: "task-v1".into(),
                origin: TaskOrigin::Operator,
                text: "Replace the resource".into(),
                allowed_resource_ids: vec![],
                preserve_resource_ids: vec![],
            }),
            candidate: Candidate {
                operation: Operation::Replace,
                resource_id: resource_id.into(),
                native_tool: None,
            },
            evidence: vec![],
            gaps: vec![],
        }
    }

    #[tokio::test]
    async fn unique_content_requires_a_snapshot_without_a_model() {
        assert_eq!(
            preservation_outcome("tracked.txt").await,
            (Reason::NeedsSnapshot, AnswerValue::NeedsSnapshot)
        );
    }

    #[tokio::test]
    async fn untracked_content_is_undetermined_and_blocks() {
        assert_eq!(
            preservation_outcome("untracked.txt").await,
            (Reason::Undetermined, AnswerValue::Unknown)
        );
    }
}
