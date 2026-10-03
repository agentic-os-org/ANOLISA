//! Strict non-interactive Git HTTPS source validation and materialization.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

use wait_timeout::ChildExt;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const GIT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const GIT_STDOUT_LIMIT: usize = 64 * 1024;
const GIT_STDERR_LIMIT: usize = 4 * 1024;
const REDIRECT_RESOLUTION_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HTTPS_REDIRECTS: usize = 10;

/// Materializes Git sources without inheriting interactive credentials or URL rewrites.
#[derive(Debug, Clone)]
pub struct GitMaterializer {
    fetch_override: Option<PathBuf>,
    final_source_override: Option<String>,
    git_program: PathBuf,
    command_timeout: Duration,
}

impl Default for GitMaterializer {
    fn default() -> Self {
        Self {
            fetch_override: None,
            final_source_override: None,
            git_program: PathBuf::from("git"),
            command_timeout: GIT_COMMAND_TIMEOUT,
        }
    }
}

impl GitMaterializer {
    /// Creates the production HTTPS-only materializer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a materializer that fetches from a local fixture after validating a fake HTTPS URL.
    #[cfg(test)]
    pub fn with_test_fixture(fixture: PathBuf) -> Self {
        Self {
            fetch_override: Some(fixture),
            ..Self::default()
        }
    }

    #[cfg(test)]
    fn with_test_redirect_fixture(fixture: PathBuf, final_source: &str) -> Self {
        Self {
            fetch_override: Some(fixture),
            final_source_override: Some(final_source.to_string()),
            ..Self::default()
        }
    }

    /// Fetches one ref into a detached payload and returns the resolved commit SHA.
    pub fn materialize(
        &self,
        source: &str,
        requested_ref: Option<&str>,
        destination: &Path,
    ) -> Result<MaterializedGitSource, GitSourceError> {
        let requested_source_identity = canonical_https_source(source)?;
        let source_identity = match self.final_source_override.as_deref() {
            Some(final_source) => canonical_redirect_target(final_source)?,
            None if self.fetch_override.is_some() => requested_source_identity,
            None => resolve_final_https_source(&requested_source_identity)?,
        };
        let requested_ref = requested_ref.map(validate_git_ref).transpose()?;
        let remote = self
            .fetch_override
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| source_identity.clone());
        fs::create_dir_all(destination).map_err(|error| {
            GitSourceError::new(
                "extension_git_materialize_failed",
                format!("failed to create {}: {error}", destination.display()),
            )
        })?;
        let empty_config = destination
            .parent()
            .unwrap_or(destination)
            .join(".gitconfig-empty");
        fs::write(&empty_config, []).map_err(|error| {
            GitSourceError::new(
                "extension_git_materialize_failed",
                format!("failed to create isolated Git config: {error}"),
            )
        })?;

        let materialized = (|| {
            self.run_git(
                destination,
                &empty_config,
                ["init", "--quiet", destination.to_string_lossy().as_ref()],
            )?;
            self.run_git(
                destination,
                &empty_config,
                [
                    "-C",
                    destination.to_string_lossy().as_ref(),
                    "remote",
                    "add",
                    "origin",
                    &remote,
                ],
            )?;
            let mut fetch_args = vec![
                "-C".to_string(),
                destination.to_string_lossy().into_owned(),
                "fetch".to_string(),
                "--quiet".to_string(),
                "--no-tags".to_string(),
                "--no-recurse-submodules".to_string(),
                "--depth=1".to_string(),
                "origin".to_string(),
            ];
            fetch_args.push(requested_ref.unwrap_or("HEAD").to_string());
            let shallow_fetch = self.run_git(
                destination,
                &empty_config,
                fetch_args.iter().map(String::as_str),
            );
            if let Err(error) = shallow_fetch {
                if !error.is_dumb_http_shallow_unsupported() {
                    return Err(error);
                }
                fetch_args.retain(|argument| argument != "--depth=1");
                self.run_git(
                    destination,
                    &empty_config,
                    fetch_args.iter().map(String::as_str),
                )?;
            }
            self.run_git(
                destination,
                &empty_config,
                [
                    "-C",
                    destination.to_string_lossy().as_ref(),
                    "checkout",
                    "--quiet",
                    "--detach",
                    "FETCH_HEAD",
                ],
            )?;
            let revision = self.run_git(
                destination,
                &empty_config,
                [
                    "-C",
                    destination.to_string_lossy().as_ref(),
                    "rev-parse",
                    "--verify",
                    "HEAD",
                ],
            )?;
            let resolved_revision = String::from_utf8(revision.stdout)
                .map_err(|error| {
                    GitSourceError::new(
                        "extension_git_revision_invalid",
                        format!("Git returned a non-UTF-8 revision: {error}"),
                    )
                })?
                .trim()
                .to_string();
            if resolved_revision.len() != 40
                || !resolved_revision
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(GitSourceError::new(
                    "extension_git_revision_invalid",
                    format!("Git returned an invalid commit: {resolved_revision}"),
                ));
            }
            fs::remove_dir_all(destination.join(".git")).map_err(|error| {
                GitSourceError::new(
                    "extension_git_materialize_failed",
                    format!("failed to remove staged Git metadata: {error}"),
                )
            })?;
            Ok(MaterializedGitSource {
                source_identity,
                requested_ref: requested_ref.map(str::to_string),
                resolved_revision,
            })
        })();
        let _ = fs::remove_file(&empty_config);
        materialized
    }

    fn run_git<'a>(
        &self,
        destination: &Path,
        empty_config: &Path,
        args: impl IntoIterator<Item = &'a str>,
    ) -> Result<Output, GitSourceError> {
        let mut command = Command::new(&self.git_program);
        command.args([
            "-c",
            "protocol.allow=never",
            "-c",
            "protocol.https.allow=always",
            "-c",
            "credential.helper=",
            "-c",
            "http.followRedirects=false",
            "-c",
            "submodule.recurse=false",
        ]);
        if self.fetch_override.is_some() {
            command.args(["-c", "protocol.file.allow=always"]);
        }
        command
            .args(args)
            .current_dir(destination)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", empty_config)
            .env_remove("SSH_AUTH_SOCK")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| {
            GitSourceError::new(
                "extension_git_unavailable",
                format!("failed to execute Git: {error}"),
            )
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            GitSourceError::new("extension_git_unavailable", "failed to capture Git stdout")
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            GitSourceError::new("extension_git_unavailable", "failed to capture Git stderr")
        })?;
        let stdout_reader = std::thread::spawn(move || read_bounded(stdout, GIT_STDOUT_LIMIT));
        let stderr_reader = std::thread::spawn(move || read_bounded(stderr, GIT_STDERR_LIMIT));
        let status = match child.wait_timeout(self.command_timeout).map_err(|error| {
            GitSourceError::new(
                "extension_git_command_failed",
                format!("failed while waiting for Git: {error}"),
            )
        })? {
            Some(status) => status,
            None => {
                terminate_git_process(&mut child);
                let _ = child.wait();
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(GitSourceError::new(
                    "extension_git_timeout",
                    format!(
                        "Git command exceeded the {} second timeout",
                        self.command_timeout.as_secs_f64()
                    ),
                ));
            }
        };
        let stdout = stdout_reader.join().map_err(|_| {
            GitSourceError::new("extension_git_command_failed", "Git stdout reader failed")
        })??;
        let stderr = stderr_reader.join().map_err(|_| {
            GitSourceError::new("extension_git_command_failed", "Git stderr reader failed")
        })??;
        let output = Output {
            status,
            stdout,
            stderr,
        };
        if output.status.success() {
            return Ok(output);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim().chars().take(4096).collect::<String>();
        Err(GitSourceError::new(
            "extension_git_command_failed",
            if detail.is_empty() {
                format!("Git exited with {}", output.status)
            } else {
                detail
            },
        ))
    }
}

fn terminate_git_process(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(child.id() as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    #[cfg(not(unix))]
    {
        let _ = child.kill();
    }
}

fn read_bounded(mut reader: impl Read, limit: usize) -> Result<Vec<u8>, GitSourceError> {
    let mut kept = Vec::with_capacity(limit.min(4096));
    let mut buffer = [0u8; 4096];
    loop {
        let count = reader.read(&mut buffer).map_err(|error| {
            GitSourceError::new(
                "extension_git_command_failed",
                format!("failed to read Git output: {error}"),
            )
        })?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(kept)
}

/// Final immutable identity and revision of a staged Git source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterializedGitSource {
    /// Canonical credential-free HTTPS URL.
    pub source_identity: String,
    /// Requested branch, tag, or commit.
    pub requested_ref: Option<String>,
    /// Resolved full commit SHA.
    pub resolved_revision: String,
}

/// Stable Git source validation or materialization error.
#[derive(Debug)]
pub struct GitSourceError {
    code: &'static str,
    message: String,
}

impl GitSourceError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn is_dumb_http_shallow_unsupported(&self) -> bool {
        self.code == "extension_git_command_failed"
            && self
                .message
                .contains("dumb http transport does not support shallow capabilities")
    }
}

impl fmt::Display for GitSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitSourceError {}

/// Validates and canonicalizes a credential-free HTTPS Git URL.
pub fn canonical_https_source(source: &str) -> Result<String, GitSourceError> {
    let mut url = reqwest::Url::parse(source).map_err(|error| {
        GitSourceError::new(
            "extension_git_url_invalid",
            format!("invalid Git URL: {error}"),
        )
    })?;
    if url.scheme() != "https" {
        return Err(GitSourceError::new(
            "extension_git_protocol_unsupported",
            "Git extension sources must use HTTPS",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GitSourceError::new(
            "extension_git_credentials_forbidden",
            "Git extension source URLs must not contain credentials",
        ));
    }
    if url.host_str().is_none() || url.query().is_some() || url.fragment().is_some() {
        return Err(GitSourceError::new(
            "extension_git_url_invalid",
            "Git extension source must have a host and no query or fragment",
        ));
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

fn canonical_redirect_target(source: &str) -> Result<String, GitSourceError> {
    canonical_https_source(source).map_err(|error| {
        GitSourceError::new(
            if error.code() == "extension_git_protocol_unsupported" {
                "extension_git_redirect_protocol_unsupported"
            } else {
                "extension_git_redirect_invalid"
            },
            format!("unsafe Git redirect target: {error}"),
        )
    })
}

fn resolve_final_https_source(source: &str) -> Result<String, GitSourceError> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(REDIRECT_RESOLUTION_TIMEOUT)
        .build()
        .map_err(|error| {
            GitSourceError::new(
                "extension_git_redirect_resolution_failed",
                format!("failed to create HTTPS redirect resolver: {error}"),
            )
        })?;
    let mut probe = git_info_refs_url(source)?;
    for _ in 0..=MAX_HTTPS_REDIRECTS {
        let response = client
            .get(probe.clone())
            .header("Accept", "application/x-git-upload-pack-advertisement")
            .send()
            .map_err(|error| {
                GitSourceError::new(
                    "extension_git_redirect_resolution_failed",
                    format!("failed to resolve Git HTTPS identity: {error}"),
                )
            })?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| {
                    GitSourceError::new(
                        "extension_git_redirect_invalid",
                        "Git HTTPS redirect omitted a valid Location header",
                    )
                })?;
            let next = probe.join(location).map_err(|error| {
                GitSourceError::new(
                    "extension_git_redirect_invalid",
                    format!("invalid Git HTTPS redirect location: {error}"),
                )
            })?;
            validate_redirect_url(&next)?;
            probe = next;
            continue;
        }
        if !response.status().is_success() {
            return Err(GitSourceError::new(
                "extension_git_redirect_resolution_failed",
                format!(
                    "Git HTTPS identity probe returned status {}",
                    response.status()
                ),
            ));
        }
        return repository_identity_from_probe(response.url());
    }
    Err(GitSourceError::new(
        "extension_git_redirect_limit_exceeded",
        format!("Git HTTPS source exceeded {MAX_HTTPS_REDIRECTS} redirects"),
    ))
}

fn git_info_refs_url(source: &str) -> Result<reqwest::Url, GitSourceError> {
    let mut url = reqwest::Url::parse(source).map_err(|error| {
        GitSourceError::new(
            "extension_git_url_invalid",
            format!("invalid Git URL: {error}"),
        )
    })?;
    let path = format!("{}/info/refs", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(Some("service=git-upload-pack"));
    Ok(url)
}

fn repository_identity_from_probe(probe: &reqwest::Url) -> Result<String, GitSourceError> {
    let mut repository = probe.clone();
    validate_redirect_url(&repository)?;
    let Some(path) = repository.path().strip_suffix("/info/refs") else {
        return Err(GitSourceError::new(
            "extension_git_redirect_invalid",
            "final Git HTTPS identity does not end in /info/refs",
        ));
    };
    let path = path.to_string();
    repository.set_path(&path);
    repository.set_query(None);
    repository.set_fragment(None);
    canonical_redirect_target(repository.as_str())
}

fn validate_redirect_url(url: &reqwest::Url) -> Result<(), GitSourceError> {
    if url.scheme() != "https" {
        return Err(GitSourceError::new(
            "extension_git_redirect_protocol_unsupported",
            "Git redirect targets must use HTTPS",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() || url.host_str().is_none() {
        return Err(GitSourceError::new(
            "extension_git_redirect_invalid",
            "Git redirect target must have a host and must not contain credentials",
        ));
    }
    Ok(())
}

fn validate_git_ref(reference: &str) -> Result<&str, GitSourceError> {
    if reference.is_empty()
        || reference.len() > 512
        || reference.starts_with('-')
        || reference.contains("..")
        || reference.contains("@{")
        || reference
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
        || reference
            .chars()
            .any(|character| matches!(character, '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
    {
        return Err(GitSourceError::new(
            "extension_git_ref_invalid",
            "Git ref contains unsupported syntax",
        ));
    }
    Ok(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repository_fixture(root: &Path) -> PathBuf {
        let repository = root.join("repository");
        fs::create_dir_all(&repository).unwrap();
        for args in [
            vec!["init", "--quiet"],
            vec!["config", "user.name", "Cosh Test"],
            vec!["config", "user.email", "cosh-test@example.invalid"],
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(&repository)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        fs::write(repository.join("README.md"), "fixture").unwrap();
        for args in [vec!["add", "."], vec!["commit", "--quiet", "-m", "fixture"]] {
            let output = Command::new("git")
                .args(args)
                .current_dir(&repository)
                .output()
                .unwrap();
            assert!(output.status.success());
        }
        repository
    }

    fn fixture_git(repository: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repository)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[cfg(unix)]
    fn fake_git(root: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let program = root.join("git-fixture.sh");
        fs::write(&program, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();
        program
    }

    #[test]
    fn rejects_non_https_and_credential_urls() {
        assert_eq!(
            canonical_https_source("ssh://example.com/repo.git")
                .unwrap_err()
                .code(),
            "extension_git_protocol_unsupported"
        );
        assert_eq!(
            canonical_https_source("https://token@example.com/repo.git")
                .unwrap_err()
                .code(),
            "extension_git_credentials_forbidden"
        );
        assert_eq!(
            canonical_https_source("git@example.com:repo.git")
                .unwrap_err()
                .code(),
            "extension_git_url_invalid"
        );
    }

    #[test]
    fn rejects_ambiguous_git_refs() {
        assert_eq!(
            validate_git_ref("--upload-pack=evil").unwrap_err().code(),
            "extension_git_ref_invalid"
        );
        assert_eq!(
            validate_git_ref("main..evil").unwrap_err().code(),
            "extension_git_ref_invalid"
        );
    }

    #[test]
    fn final_probe_identity_strips_transport_suffix_and_query() {
        let probe = reqwest::Url::parse(
            "https://redirect.example/repos/example.git/info/refs?service=git-upload-pack",
        )
        .unwrap();
        assert_eq!(
            repository_identity_from_probe(&probe).unwrap(),
            "https://redirect.example/repos/example.git"
        );
        let downgrade = reqwest::Url::parse(
            "http://redirect.example/repos/example.git/info/refs?service=git-upload-pack",
        )
        .unwrap();
        assert_eq!(
            repository_identity_from_probe(&downgrade)
                .unwrap_err()
                .code(),
            "extension_git_redirect_protocol_unsupported"
        );
    }

    #[test]
    fn materialization_records_fixture_redirect_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = repository_fixture(temporary.path());
        let destination = temporary.path().join("payload");
        let materializer = GitMaterializer::with_test_redirect_fixture(
            repository,
            "https://final.example/owner/example.git",
        );
        let materialized = materializer
            .materialize(
                "https://initial.example/owner/example.git",
                Some("HEAD"),
                &destination,
            )
            .unwrap();
        assert_eq!(
            materialized.source_identity,
            "https://final.example/owner/example.git"
        );
        assert_eq!(materialized.resolved_revision.len(), 40);
    }

    #[test]
    fn materializes_default_branch_branch_tag_and_commit_refs() {
        let temporary = tempfile::tempdir().unwrap();
        let repository = repository_fixture(temporary.path());
        let primary_branch = fixture_git(&repository, &["branch", "--show-current"]);
        let base_revision = fixture_git(&repository, &["rev-parse", "HEAD"]);
        fixture_git(&repository, &["tag", "v1"]);
        fixture_git(&repository, &["checkout", "--quiet", "-b", "feature"]);
        fs::write(repository.join("README.md"), "feature").unwrap();
        fixture_git(&repository, &["add", "."]);
        fixture_git(&repository, &["commit", "--quiet", "-m", "feature"]);
        let feature_revision = fixture_git(&repository, &["rev-parse", "HEAD"]);
        fixture_git(&repository, &["checkout", "--quiet", &primary_branch]);
        let materializer = GitMaterializer::with_test_fixture(repository);

        for (label, reference, expected) in [
            ("default", None, base_revision.as_str()),
            ("branch", Some("feature"), feature_revision.as_str()),
            ("tag", Some("v1"), base_revision.as_str()),
            (
                "commit",
                Some(feature_revision.as_str()),
                feature_revision.as_str(),
            ),
        ] {
            let destination = temporary.path().join(format!("payload-{label}"));
            let materialized = materializer
                .materialize(
                    "https://example.invalid/owner/example.git",
                    reference,
                    &destination,
                )
                .unwrap();
            assert_eq!(materialized.resolved_revision, expected, "{label}");
        }
    }

    #[test]
    fn materialization_rejects_fixture_protocol_downgrade() {
        let temporary = tempfile::tempdir().unwrap();
        let materializer = GitMaterializer::with_test_redirect_fixture(
            temporary.path().join("unused"),
            "http://final.example/owner/example.git",
        );
        let error = materializer
            .materialize(
                "https://initial.example/owner/example.git",
                None,
                &temporary.path().join("payload"),
            )
            .unwrap_err();
        assert_eq!(error.code(), "extension_git_redirect_protocol_unsupported");
    }

    #[test]
    fn recognizes_only_the_dumb_http_shallow_failure_for_retry() {
        let retryable = GitSourceError::new(
            "extension_git_command_failed",
            "fatal: dumb http transport does not support shallow capabilities",
        );
        assert!(retryable.is_dumb_http_shallow_unsupported());

        let authentication = GitSourceError::new(
            "extension_git_command_failed",
            "fatal: authentication failed",
        );
        assert!(!authentication.is_dumb_http_shallow_unsupported());

        let wrong_code = GitSourceError::new(
            "extension_git_materialize_failed",
            "dumb http transport does not support shallow capabilities",
        );
        assert!(!wrong_code.is_dumb_http_shallow_unsupported());
    }

    #[cfg(unix)]
    #[test]
    fn git_command_timeout_kills_the_child() {
        let temporary = tempfile::tempdir().unwrap();
        let program = fake_git(temporary.path(), "sleep 2");
        let config = temporary.path().join("empty-config");
        fs::write(&config, []).unwrap();
        let materializer = GitMaterializer {
            fetch_override: None,
            final_source_override: None,
            git_program: program,
            command_timeout: Duration::from_millis(50),
        };
        let error = materializer
            .run_git(temporary.path(), &config, ["status"])
            .unwrap_err();
        assert_eq!(error.code(), "extension_git_timeout");
    }

    #[cfg(unix)]
    #[test]
    fn git_command_stderr_is_bounded_while_running() {
        let temporary = tempfile::tempdir().unwrap();
        let program = fake_git(
            temporary.path(),
            "i=0; while [ $i -lt 10000 ]; do printf x >&2; i=$((i + 1)); done; exit 1",
        );
        let config = temporary.path().join("empty-config");
        fs::write(&config, []).unwrap();
        let materializer = GitMaterializer {
            fetch_override: None,
            final_source_override: None,
            git_program: program,
            command_timeout: Duration::from_secs(2),
        };
        let error = materializer
            .run_git(temporary.path(), &config, ["status"])
            .unwrap_err();
        assert_eq!(error.code(), "extension_git_command_failed");
        assert!(error.to_string().len() <= GIT_STDERR_LIMIT);
    }
}
