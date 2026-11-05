//! GitHub credential shape and environment metadata, independent of Policy rules.

use asc_credential_store::{Carrier, CredentialId, CredentialSpec, FakeFormat};

/// Metadata for importing GitHub personal access or app tokens into the store.
#[must_use]
pub fn github_credential_spec() -> CredentialSpec {
    CredentialSpec {
        // The literal is a valid lowercase DNS label.
        id: CredentialId::new("github")
            .unwrap_or_else(|_| unreachable!("static credential identifier is valid")),
        carriers: vec![Carrier::HeaderToken, Carrier::BasicPassword],
        format: FakeFormat::with_prefixes(["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_"]),
    }
}

/// Environment variables receiving the issued fake; unknown credentials have no mapping.
#[must_use]
pub fn credential_environment_variables(credential: &CredentialId) -> &'static [&'static str] {
    match credential.as_str() {
        "github" => &["GITHUB_TOKEN", "GH_TOKEN"],
        _ => &[],
    }
}
