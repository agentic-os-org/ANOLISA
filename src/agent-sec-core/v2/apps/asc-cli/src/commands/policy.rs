//! Authored Policy commands and bounded template-file decoding.

use std::fs::File;
use std::io::Read as _;
use std::path::PathBuf;

use asc_daemon_protocol::method::{
    POLICY_TEMPLATES_CREATE, POLICY_TEMPLATES_DELETE, POLICY_TEMPLATES_GET, POLICY_TEMPLATES_LIST,
    POLICY_TEMPLATES_UPDATE,
};
use asc_daemon_protocol::{CreatePolicyParams, DaemonRequest, RevisionParams, UpdatePolicyParams};
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::authoring::PolicyTemplate;
use clap::{Args, Subcommand};

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
}

#[derive(Debug, Args)]
pub(crate) struct PolicyInput {
    /// Complete Policy name.
    #[arg(long)]
    name: String,
    /// JSON `PolicyTemplate` file, resolved in the CLI's working directory.
    #[arg(long)]
    file: PathBuf,
}

#[derive(Debug, Args)]
pub(crate) struct PolicyRevision {
    #[arg(long, value_parser = resource_id)]
    policy_id: ResourceId,
    #[arg(long, value_parser = revision)]
    revision: Revision,
}

impl PolicyCommand {
    pub(super) fn request(&self) -> Result<DaemonRequest, InputError> {
        match self {
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
    fn template(&self) -> Result<PolicyTemplate, InputError> {
        let mut bytes = Vec::new();
        File::open(&self.file)?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > asc_daemon_client::MAX_FRAME_BYTES {
            return Err(InputError::TooLarge);
        }
        // Decode the typed template before conversion to Value, so duplicate
        // fields cannot be silently collapsed by an untyped JSON map.
        Ok(serde_json::from_slice(&bytes)?)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

    use super::*;
    use crate::Cli;
    use crate::commands::Command;

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
