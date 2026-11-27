//! Conventional credential-locus clues for read projections; hints, not proof.
//!
//! The vocabulary mirrors widely documented credential storage locations. A
//! match never proves the bytes are secret, and a miss never proves they are
//! not; the signal only routes ambiguous reads to semantic review.

/// Programs whose operands name files whose content they read.
const CONTENT_READERS: &[&str] = &[
    "cat",
    "head",
    "tail",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "less",
    "more",
    "wc",
    "uniq",
    "diff",
    "cmp",
    "cut",
    "tr",
    "nl",
    "od",
    "xxd",
    "hexdump",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "file",
    "sort",
];

/// Whether a path names a conventional credential or secret-storage locus.
pub(crate) fn path(text: &str) -> bool {
    let parts: Vec<&str> = text.split('/').filter(|part| !part.is_empty()).collect();
    let leaf = parts.last().copied().unwrap_or_default();
    if parts
        .iter()
        .any(|part| matches!(*part, ".ssh" | ".aws" | ".gnupg"))
    {
        return true;
    }
    if leaf == ".env"
        || (leaf.starts_with(".env.")
            && !matches!(
                leaf,
                ".env.example" | ".env.sample" | ".env.template" | ".env.dist"
            ))
    {
        return true;
    }
    if matches!(
        leaf,
        ".netrc"
            | ".git-credentials"
            | ".bash_history"
            | ".zsh_history"
            | ".history"
            | ".pypirc"
            | ".npmrc"
            | ".vault-token"
            | "id_rsa"
            | "id_dsa"
            | "id_ecdsa"
            | "id_ed25519"
            | "credentials"
    ) {
        return true;
    }
    leaf.ends_with(".pem")
        || leaf.ends_with(".key")
        || leaf.ends_with(".p12")
        || leaf.ends_with(".pfx")
        || leaf.ends_with(".keystore")
}

/// Whether a read-only shell program may reach credential loci.
///
/// Recursive content searches always qualify: their ignore rules are tool
/// behavior this projection does not verify, so hidden credential files may be
/// read. `printenv` qualifies because an environment dump can carry secrets.
pub(crate) fn read_command(name: &str, args: &[&str]) -> bool {
    if name == "printenv" {
        return true;
    }
    if !CONTENT_READERS.contains(&name) {
        return false;
    }
    if name == "rg" {
        return true;
    }
    if matches!(name, "grep" | "egrep" | "fgrep") && recursive(args) {
        return true;
    }
    args.iter()
        .filter(|arg| !arg.starts_with('-') && **arg != "-")
        .any(|arg| path(arg))
}

fn recursive(args: &[&str]) -> bool {
    args.iter().take_while(|arg| **arg != "--").any(|arg| {
        *arg == "--recursive"
            || (arg.starts_with('-') && !arg.starts_with("--") && arg[1..].contains(['r', 'R']))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conventional_credential_loci_are_sensitive() {
        for text in [
            ".env",
            ".env.local",
            ".env.production",
            "config/.env",
            "/home/user/.ssh/id_rsa",
            ".aws/credentials",
            ".bash_history",
            ".zsh_history",
            ".netrc",
            ".git-credentials",
            ".npmrc",
            ".pypirc",
            "certs/server.pem",
            "private.key",
            "store.p12",
            ".gnupg/private-keys-v1.d",
            "id_ed25519",
        ] {
            assert!(path(text), "{text}");
        }
    }

    #[test]
    fn templates_and_ordinary_files_are_not_sensitive() {
        for text in [
            ".env.example",
            ".env.sample",
            ".env.template",
            ".env.dist",
            "src/parser.rs",
            "data/results.csv",
            "README.md",
            "notes.txt",
            "environment.rs",
            "keys.md",
            "id_rsa.pub",
            "ssh.rs",
        ] {
            assert!(!path(text), "{text}");
        }
    }

    #[test]
    fn recursive_searches_and_environment_dumps_qualify() {
        assert!(read_command("rg", &["pattern", "src"]));
        assert!(read_command("grep", &["-r", "pattern", "."]));
        assert!(read_command("grep", &["-Rn", "pattern", "src"]));
        assert!(read_command("egrep", &["--recursive", "pattern"]));
        assert!(read_command("printenv", &[]));
        assert!(read_command("printenv", &["PATH"]));
    }

    #[test]
    fn named_non_sensitive_reads_stay_clean() {
        assert!(!read_command("cat", &["src/parser.rs", "README.md"]));
        assert!(!read_command("grep", &["pattern", "src/parser.rs"]));
        assert!(!read_command("head", &["-n", "5", "notes.txt"]));
        assert!(!read_command("diff", &["a.txt", "b.txt"]));
        assert!(!read_command("sort", &["data/results.csv"]));
        assert!(!read_command("ls", &["-la"]));
        assert!(!read_command("cat", &[".env.example"]));
        assert!(!read_command("grep", &["--", "pattern", "src/main.rs"]));
    }

    #[test]
    fn sensitive_named_reads_qualify() {
        assert!(read_command("cat", &[".env"]));
        assert!(read_command("head", &[".env.local"]));
        assert!(read_command("grep", &["KEY", ".env.production"]));
        assert!(read_command("sort", &[".bash_history"]));
        assert!(read_command("xxd", &["id_rsa"]));
        // stdin alone names no locus; the piped source is analyzed separately.
        assert!(!read_command("cat", &["-"]));
    }
}
