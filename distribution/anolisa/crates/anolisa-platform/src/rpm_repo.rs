//! Explicit RPM repository source shared by metadata queries and native transactions.

/// RPM repository supplied by ANOLISA configuration for one command run.
///
/// Native transactions inject the repo temporarily,
/// keeping `repo.toml` authoritative for ANOLISA-managed RPM operations while
/// leaving the host's persistent package-manager configuration untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpmRepoSource {
    id: String,
    base_url: String,
    gpgcheck: Option<bool>,
}

impl RpmRepoSource {
    /// Builds an explicit RPM repository descriptor.
    pub fn new(id: impl Into<String>, base_url: impl Into<String>, gpgcheck: Option<bool>) -> Self {
        Self {
            id: id.into(),
            base_url: base_url.into(),
            gpgcheck,
        }
    }

    /// Repository id used by DNF for this temporary source.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Base URL passed to DNF as the repo path.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Package signature verification setting from `repo.toml`.
    pub fn gpgcheck(&self) -> Option<bool> {
        self.gpgcheck
    }

    /// Reduces URLs in native-tool diagnostics to non-secret origins.
    pub(crate) fn redact_diagnostic(&self, text: &str) -> String {
        redact_url_runs(text, &self.base_url)
    }

    /// DNF options for **write transactions** (`install`/`update`/`remove`).
    ///
    /// This does **not**
    /// emit `--disablerepo=*`. RPM packages declare their own `Requires:` and dnf
    /// resolves the entire dependency graph in one transaction. If all system
    /// repos are disabled, dnf cannot satisfy cross-repo dependencies that live
    /// outside the ANOLISA repo (e.g. `bubblewrap` in EPEL). Keeping system repos
    /// enabled lets dnf pull ANOLISA components from the configured repo while
    /// still resolving system-level `Requires` from the host's enabled repos.
    pub(crate) fn append_dnf_txn_options(&self, args: &mut Vec<String>) {
        args.push(format!("--repofrompath={},{}", self.id, self.base_url));
        args.push(format!("--enablerepo={}", self.id));
        if let Some(gpgcheck) = self.gpgcheck {
            args.push(format!(
                "--setopt={}.gpgcheck={}",
                self.id,
                if gpgcheck { "1" } else { "0" }
            ));
        }
    }
}

fn redact_url_runs(text: &str, known_url: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = next_url_start(rest) {
        out.push_str(&rest[..at]);
        let matched = &rest[at..];
        // DNF echoes the configured value verbatim, including embedded whitespace.
        let known_len = if matched.starts_with(known_url) {
            known_url.len()
        } else {
            0
        };
        let end = matched[known_len..]
            .find(char::is_whitespace)
            .map_or(matched.len(), |offset| known_len + offset);
        let url = &matched[..end];
        out.push_str(&diagnostic_origin(url));
        rest = &matched[end..];
    }
    out.push_str(rest);
    out
}

fn next_url_start(text: &str) -> Option<usize> {
    let lowercase = text.to_ascii_lowercase();
    ["http://", "https://", "file://"]
        .into_iter()
        .filter_map(|scheme| lowercase.find(scheme))
        .min()
}

fn diagnostic_origin(value: &str) -> String {
    match url::Url::parse(value) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url.origin().ascii_serialization(),
        Ok(url) if url.scheme() == "file" => "file://<local>".to_string(),
        _ => "<repository>".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::RpmRepoSource;

    #[test]
    fn diagnostic_redaction_consumes_whitespace_in_configured_urls() {
        for (base_url, origin) in [
            (
                "http://testuser:secret tail@repo.example.internal/private",
                "http://repo.example.internal",
            ),
            (
                "http://repo.example.internal/private secret-token",
                "http://repo.example.internal",
            ),
            (
                "https://testuser:secret\ttail@repo.example.internal/private\nsecret-token",
                "https://repo.example.internal",
            ),
            ("file:///private repo/secret-token", "file://<local>"),
        ] {
            let repo = RpmRepoSource::new("private", base_url, None);
            let text = format!(
                "Added repo from {base_url}\nFailed to fetch {base_url}/repodata/repomd.xml: unavailable; mirror https://user:secret@mirror.example/private"
            );

            assert_eq!(
                repo.redact_diagnostic(&text),
                format!(
                    "Added repo from {origin}\nFailed to fetch {origin} unavailable; mirror https://mirror.example"
                ),
                "configured URL: {base_url:?}"
            );
        }
    }

    #[test]
    fn diagnostic_redaction_reduces_all_urls_to_origins() {
        let repo = RpmRepoSource::new(
            "private",
            "https://user:encoded%2Fsecret@repo.example/private/path?token=known",
            None,
        );
        let text = "Configured HTTPS://user:decoded-secret@repo.example/private/path and redirected to https://mirror-user:mirror-secret@mirror.example/cache/item";

        let redacted = repo.redact_diagnostic(text);

        assert_eq!(
            redacted,
            "Configured https://repo.example and redirected to https://mirror.example"
        );
        assert!(!redacted.contains("secret"));
        assert!(!redacted.contains("/private/"));
        assert!(!redacted.contains("/cache/"));
    }
}
