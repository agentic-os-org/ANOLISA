//! Authored Policy commands and bounded source-file decoding.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::PathBuf;

use asc_daemon_protocol::method::{
    POLICY_TEMPLATES_CREATE, POLICY_TEMPLATES_DELETE, POLICY_TEMPLATES_GET, POLICY_TEMPLATES_LIST,
    POLICY_TEMPLATES_UPDATE,
};
use asc_daemon_protocol::{CreatePolicyParams, DaemonRequest, RevisionParams, UpdatePolicyParams};
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::authoring::{PolicySource, source_serde};
use asc_policy_types::cedar::{CedarPolicySource, CedarSchemaReference};
use clap::{Args, Subcommand, ValueEnum};

use super::common::{Page, encode, resource_id, revision};
use crate::InputError;

#[derive(Debug, Subcommand)]
pub(crate) enum PolicyCommand {
    /// Create a Policy with a server-generated ID.
    Create(PolicyInput),
    /// Read the exact current revision.
    Get(PolicyRevision),
    /// List one page of current Policies.
    List(Page),
    /// Replace an existing Policy's name and complete template (not upsert).
    Update {
        #[arg(long, value_parser = resource_id)]
        policy_id: ResourceId,
        #[command(flatten)]
        input: PolicyInput,
    },
    /// Delete the exact current revision.
    Delete(PolicyRevision),
    /// Print the built-in restrictive baseline; never contacts the daemon.
    Default(DefaultPolicy),
}

#[derive(Debug, Args)]
/// Format selector for the embedded restrictive baseline.
pub struct DefaultPolicy {
    /// Print the JSON source envelope instead of the raw Cedar text.
    #[arg(long)]
    json: bool,
}

/// How `policy create/update --file` content is decoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceLanguage {
    /// Raw Cedar text; the schema reference comes from `--schema-*`.
    Cedar,
    /// JSON native template or `{"language": ..., "content": ...}` source envelope.
    Json,
}

#[derive(Debug, Args)]
pub(crate) struct PolicyInput {
    /// Complete Policy name.
    #[arg(long)]
    name: String,
    /// Policy source file, resolved in the CLI's working directory.
    #[arg(long)]
    file: PathBuf,
    /// Source format of `--file`.
    #[arg(long, value_enum, default_value_t = SourceLanguage::Cedar)]
    language: SourceLanguage,
    /// Cedar schema identity [default: the embedded system schema].
    #[arg(long, value_parser = resource_id)]
    schema_id: Option<ResourceId>,
    /// Cedar schema revision [default: the embedded schema's revision].
    #[arg(long, value_parser = revision)]
    schema_revision: Option<Revision>,
}

#[derive(Debug, Args)]
pub(crate) struct PolicyRevision {
    #[arg(long, value_parser = resource_id)]
    policy_id: ResourceId,
    #[arg(long, value_parser = revision)]
    revision: Revision,
}

impl DefaultPolicy {
    /// Renders the baseline that `policy create --file` accepts.
    ///
    /// Raw Cedar text imports with the default `--language cedar`; the JSON
    /// envelope pins the embedded schema revision inside the file and imports
    /// with `--language json`.
    ///
    /// # Errors
    /// Returns the write failure of `out`.
    pub fn render(&self, out: &mut impl io::Write) -> io::Result<()> {
        let source = asc_policy_cedar::default_policy::default_policy_source();
        if !self.json {
            return writeln!(out, "{}", source.text);
        }
        let template = PolicySource::Cedar(source);
        let envelope = serde_json::to_string_pretty(&template).map_err(io::Error::other)?;
        writeln!(out, "{envelope}")
    }
}

impl PolicyCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
            // The baseline is embedded, so the command stays usable on hosts
            // that never deploy a daemon.
            Self::Default(_) => Err(InputError::LocalCommand),
            Self::Create(input) => encode(
                POLICY_TEMPLATES_CREATE,
                &CreatePolicyParams {
                    policy_name: input.name.clone(),
                    template: input.template()?,
                },
            ),
            Self::Update { policy_id, input } => encode(
                POLICY_TEMPLATES_UPDATE,
                &UpdatePolicyParams {
                    policy_id: policy_id.clone(),
                    policy_name: input.name.clone(),
                    template: input.template()?,
                },
            ),
            Self::Get(input) | Self::Delete(input) => encode(
                if matches!(self, Self::Get(_)) {
                    POLICY_TEMPLATES_GET
                } else {
                    POLICY_TEMPLATES_DELETE
                },
                &RevisionParams {
                    id: input.policy_id.clone(),
                    revision: input.revision,
                },
            ),
            Self::List(page) => encode(POLICY_TEMPLATES_LIST, &page.params()),
        }
    }
}

impl PolicyInput {
    fn template(&self) -> Result<PolicySource, InputError> {
        // A JSON source carries its own schema; a flag that would be ignored is an error.
        if self.language == SourceLanguage::Json
            && (self.schema_id.is_some() || self.schema_revision.is_some())
        {
            return Err(InputError::SchemaWithJson);
        }
        let mut bytes = Vec::new();
        File::open(&self.file)?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > asc_daemon_client::MAX_FRAME_BYTES {
            return Err(InputError::TooLarge);
        }
        match self.language {
            // Decode the typed template before conversion to Value, so duplicate
            // fields cannot be silently collapsed by an untyped JSON map.
            SourceLanguage::Json => Ok(source_serde::from_slice(&bytes)?),
            SourceLanguage::Cedar => {
                let text = String::from_utf8(bytes).map_err(|_| InputError::PolicyUtf8)?;
                // The resolved reference is stored with the Policy, so a later
                // binary with a newer default cannot reinterpret this source.
                let embedded = asc_policy_cedar::schema::embedded_reference();
                Ok(PolicySource::Cedar(CedarPolicySource {
                    schema: CedarSchemaReference {
                        schema_id: self.schema_id.clone().unwrap_or(embedded.schema_id),
                        revision: self.schema_revision.unwrap_or(embedded.revision),
                    },
                    text,
                }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    use super::*;
    use crate::Cli;
    use crate::commands::Command;

    #[test]
    fn default_policy_prints_without_a_daemon() {
        let cli = Cli::parse_from(["agent-sec-cli", "policy", "default"]).expect("argv parses");
        assert!(cli.socket().is_none());
        assert!(matches!(cli.plan(), crate::Plan::PolicyDefault(_)));
        assert!(matches!(cli.request(), Err(InputError::LocalCommand)));
    }

    /// Renders `policy default` with `extra` flags into memory.
    fn rendered_default(extra: &[&str]) -> Vec<u8> {
        let mut argv = vec!["agent-sec-cli", "policy", "default"];
        argv.extend_from_slice(extra);
        let cli = Cli::parse_from(argv).expect("argv parses");
        let crate::Plan::PolicyDefault(command) = cli.plan() else {
            panic!("argv did not resolve to `policy default`");
        };
        let mut out = Vec::new();
        command.render(&mut out).expect("renders to memory");
        out
    }

    /// Builds the request of `policy create --file <path>` plus `extra` flags.
    fn create_request(path: &std::path::Path, extra: &[&str]) -> Result<DaemonRequest, InputError> {
        let mut argv = vec![
            "agent-sec-cli",
            "--socket=/run/asc.sock",
            "policy",
            "create",
            "--name=p",
            "--file",
            path.to_str().expect("temp paths are UTF-8"),
        ];
        argv.extend_from_slice(extra);
        Cli::parse_from(argv).expect("argv parses").request()
    }

    #[test]
    fn exported_default_policy_round_trips_in_both_languages() {
        let directory = tempfile::tempdir().expect("temp dir");
        let source = asc_policy_cedar::default_policy::default_policy_source();
        // Raw Cedar is imported verbatim, including the newline `writeln!` appends.
        let mut cedar = source.clone();
        cedar.text.push('\n');
        for (extra, language, expected) in [
            (&[][..], "cedar", PolicySource::Cedar(cedar)),
            (&["--json"][..], "json", PolicySource::Cedar(source)),
        ] {
            let path = directory.path().join(format!("default.{language}"));
            std::fs::write(&path, rendered_default(extra)).expect("writes export");
            let request = create_request(&path, &["--language", language]).expect("imports");
            assert_eq!(
                request.params["template"],
                serde_json::to_value(expected).expect("source serializes"),
                "--language {language}"
            );
        }
    }

    #[test]
    fn raw_cedar_is_the_default_language_and_schema_flags_override_the_reference() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("policy.cedar");
        let text = "forbid(principal, action, resource);\n";
        std::fs::write(&path, text).expect("writes policy");

        let request = create_request(&path, &[]).expect("imports with defaults");
        assert_eq!(
            request.params["template"],
            serde_json::json!({"language": "cedar", "content": {
                "schema": {"schemaId": "seccore.system", "revision": 1},
                "text": text,
            }})
        );

        let request = create_request(
            &path,
            &["--schema-id", "other.schema", "--schema-revision", "7"],
        )
        .expect("schema flags are forwarded and validated by the daemon");
        assert_eq!(
            request.params["template"]["content"]["schema"],
            serde_json::json!({"schemaId": "other.schema", "revision": 7})
        );
    }

    #[test]
    fn cedar_rejects_non_utf8_and_json_rejects_schema_flags() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("policy.cedar");
        std::fs::write(&path, b"forbid(principal, action, resource);\xff").expect("writes policy");
        assert!(matches!(
            create_request(&path, &[]),
            Err(InputError::PolicyUtf8)
        ));
        for flag in ["--schema-id=seccore.system", "--schema-revision=1"] {
            assert!(matches!(
                create_request(&path, &["--language=json", flag]),
                Err(InputError::SchemaWithJson)
            ));
        }
    }

    /// Returns the template path `argv` parsed into, or panics if it did not
    /// resolve to a `policy create`.
    fn parsed_file(argv: Vec<OsString>) -> PathBuf {
        let cli = Cli::parse_from(argv).expect("argv parses");
        let Command::Policy(PolicyCommand::Create(input)) = cli.command else {
            panic!("argv did not resolve to `policy create`");
        };
        input.file
    }

    /// Builds `policy create` argv whose `--file` value is exactly `file`.
    fn create_argv(file: OsString) -> Vec<OsString> {
        let mut inline = OsString::from("--file=");
        inline.push(file);
        vec![
            "agent-sec-cli".into(),
            "policy".into(),
            "create".into(),
            "--name=p".into(),
            inline,
            "--socket=/run/asc.sock".into(),
        ]
    }

    #[test]
    fn non_utf8_template_paths_reach_the_open_call_byte_for_byte() {
        // `template` opens `self.file` directly, so preserving these bytes
        // through parsing is the whole of the guarantee. Asserting it here
        // rather than end-to-end keeps the check running on macOS, whose
        // filesystem refuses to create a file with this name at all.
        let native = b"/work/policy-\xff.json";
        let file = parsed_file(create_argv(OsString::from_vec(native.to_vec())));
        assert_eq!(file.as_os_str().as_bytes(), native);
    }

    #[test]
    fn option_looking_template_paths_are_values_not_options() {
        // Inline syntax is the documented way to pass a value clap would
        // otherwise reject as an unknown option.
        let file = parsed_file(create_argv(OsString::from("--socket")));
        assert_eq!(file, PathBuf::from("--socket"));
    }
}
