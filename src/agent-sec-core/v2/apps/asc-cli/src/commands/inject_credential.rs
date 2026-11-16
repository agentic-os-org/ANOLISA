//! One-step credential injection from a packaged built-in Cedar template.
//!
//! The command composes the existing manual steps (fake generation, template
//! instantiation, `policy create`, `scope create`) and never enables the gateway.
//! The packaged template is read-only input: it is rendered in memory with
//! per-run token paths and never written back.

use std::fs::File;
use std::io::{self, Read as _, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use asc_credential_store::{
    Secret, generate_fake, lock_root_directory, read_root_asset, read_token_file, validate_pair,
    write_owner_file,
};
use asc_daemon_protocol::method::{
    GATEWAY_CA, GATEWAY_STATUS, POLICY_BINDINGS_LIST, POLICY_SCOPES_CREATE, POLICY_SCOPES_LIST,
    POLICY_TEMPLATES_CREATE,
};
use asc_daemon_protocol::{CreatePolicyParams, CreateScopeParams, DaemonResponse, ListParams};
use asc_foundation_types::{ResourceId, Revision};
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::{CedarPolicySource, CedarSchemaReference};
use asc_policy_types::scope::{PolicyReference, ProcessMatcher, ScopeSelector};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};

use super::common::encode;

/// Install-time location of packaged gateway policy templates.
const GATEWAY_DATADIR: &str = match option_env!("ASC_GATEWAY_DATADIR") {
    Some(path) => path,
    None => "/usr/share/anolisa/sec-core/gateway",
};
/// Root-only runtime area; the daemon reads staged files once at `policy create`.
const STAGING_ROOT: &str = "/run/agent-sec-core/inject-credential";
const POLICY_PREFIX: &str = "inject-credential";
const TEMPLATE_LIMIT: u64 = 256 * 1024;
const LIST_PAGE: u32 = 1000;

/// Packaged credential templates this command may instantiate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Builtin {
    /// GitHub token injection (`github.cedar`).
    Github,
}

impl Builtin {
    const fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct InjectCredentialCommand {
    /// Packaged credential template to instantiate.
    #[arg(long, value_enum)]
    builtin: Builtin,
    /// Private file holding the real token, owned by root or the Agent owner (0600).
    #[arg(long)]
    real_token_file: PathBuf,
    #[command(flatten)]
    owner: Owner,
    #[command(flatten)]
    selector: Selector,
    /// Existing fake token file to use instead of generating one.
    #[arg(long)]
    fake_token_file: Option<PathBuf>,
    /// Directory receiving the generated fake [default: ~OWNER/.config/agent-sec/gateway].
    #[arg(long, conflicts_with = "fake_token_file")]
    output_dir: Option<PathBuf>,
    /// Seconds to wait for every Binding of the new Scope to become READY (0: no wait).
    #[arg(long, default_value_t = 0)]
    wait_seconds: u32,
    /// Validate the template and print the plan; read no token and contact no daemon.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct Owner {
    /// Login name of the Agent process owner (not root).
    #[arg(long)]
    owner: Option<String>,
    /// UID of the Agent process owner (not 0).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    owner_uid: Option<u32>,
}

#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
struct Selector {
    /// Exact normalized absolute executable path of the Agent.
    #[arg(long)]
    executable: Option<String>,
    /// Exact kernel process name (comm) of the Agent.
    #[arg(long)]
    process_name: Option<String>,
    /// Exact normalized absolute path equal to one whole /proc/PID/cmdline argument.
    #[arg(long, value_name = "PATH")]
    cmdline_arg: Option<String>,
}

impl Selector {
    fn value(&self) -> ScopeSelector {
        let matcher = match (&self.executable, &self.process_name, &self.cmdline_arg) {
            (Some(executable), None, None) => ProcessMatcher::Executable {
                executable: executable.clone(),
            },
            (None, Some(name), None) => ProcessMatcher::Name {
                process_name: name.clone(),
            },
            (None, None, Some(path)) => ProcessMatcher::CmdlineArg {
                cmdline_arg: path.clone(),
            },
            _ => unreachable!("clap requires exactly one selector; fields are private"),
        };
        ScopeSelector::Process { matcher }
    }
}

struct ResolvedOwner {
    uid: u32,
    gid: u32,
    home: Option<PathBuf>,
}

/// Removes the staged real/fake copies once the daemon has read them.
struct Staging {
    path: PathBuf,
    retain: bool,
}

impl Drop for Staging {
    fn drop(&mut self) {
        if !self.retain {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Inputs resolved before any token read or daemon request.
struct Plan {
    owner: ResolvedOwner,
    selector: ScopeSelector,
    id: String,
    policy_name: String,
    staging: PathBuf,
    delivered: PathBuf,
    rendered: CedarPolicySource,
}

/// Daemon records created by one run.
struct Created {
    policy: Value,
    policy_id: ResourceId,
    revision: Revision,
    scope_id: ResourceId,
    teardown: [String; 2],
}

impl InjectCredentialCommand {
    pub(crate) fn run(
        &self,
        socket: &Path,
        timeout: Duration,
        stdout: &mut impl Write,
        stderr: &mut impl Write,
    ) -> io::Result<u8> {
        let plan = self.plan()?;
        if self.dry_run {
            let preview = json!({
                "dryRun": true,
                "builtin": self.builtin.name(),
                "template": self.template_path(),
                "ownerUid": plan.owner.uid,
                "policyName": plan.policy_name,
                "selector": plan.selector,
                "fakeTokenFile": plan.delivered,
                "rules": rule_count(&plan.rendered),
            });
            return print_json(stdout, &preview);
        }
        if !rustix::process::geteuid().is_root() {
            return Err(failure("inject-credential must run as root"));
        }
        let real = read_private(&std::path::absolute(&self.real_token_file)?, plan.owner.uid)?;
        let fake = match &self.fake_token_file {
            Some(_) => read_private(&plan.delivered, plan.owner.uid)?
                .expose()
                .to_owned(),
            None => generate_fake(real.expose(), "auto").map_err(io::Error::other)?,
        };
        validate_pair(real.expose(), &fake).map_err(io::Error::other)?;
        let _lock = lock_root_directory(Path::new(STAGING_ROOT)).map_err(io::Error::other)?;
        self.reject_duplicate(socket, timeout, plan.owner.uid, &plan.selector)?;
        let created = create(socket, timeout, &plan, &real, &fake)?;
        let (bindings, waited) = if self.wait_seconds == 0 {
            (Value::Null, Ok(()))
        } else {
            wait_bindings(
                socket,
                timeout,
                &created.scope_id,
                Duration::from_secs(u64::from(self.wait_seconds)),
            )
        };
        let result = self.report(socket, timeout, &plan, &created, &bindings);
        if self.fake_token_file.is_none() {
            // Owner-writable parents can substitute directories between mkdir and open.
            let directory = plan.delivered.parent().unwrap_or(Path::new("/"));
            (|| {
                nix::unistd::setgroups(&[]).map_err(io::Error::other)?;
                nix::unistd::setgid(nix::unistd::Gid::from_raw(plan.owner.gid))
                    .map_err(io::Error::other)?;
                nix::unistd::setuid(nix::unistd::Uid::from_raw(plan.owner.uid))
                    .map_err(io::Error::other)?;
                write_owner_file(
                    directory,
                    &fake_name(self.builtin, &plan.id),
                    &fake,
                    plan.owner.uid,
                    plan.owner.gid,
                )
                .map_err(io::Error::other)
            })()
            .map_err(|error| {
                failure(format!(
                    "Policy and Scope were created but the fake file was not delivered: {error}; \
                     tear down with: {}",
                    created.teardown.join(" && ")
                ))
            })?;
        }
        print_json(stdout, &result)?;
        match waited {
            Ok(()) => Ok(0),
            Err(error) => {
                writeln!(stderr, "agent-sec-cli: {error}")?;
                Ok(1)
            }
        }
    }

    fn plan(&self) -> io::Result<Plan> {
        use asc_policy_types::Validate as _;
        let owner = self.resolve_owner()?;
        let selector = self.selector.value();
        selector.validate().map_err(io::Error::other)?;
        let template = self.template()?;
        let id = random_id()?;
        let staging = Path::new(STAGING_ROOT).join(&id);
        let delivered = match &self.fake_token_file {
            Some(path) => std::path::absolute(path)?,
            None => self.output_dir(&owner)?.join(fake_name(self.builtin, &id)),
        };
        // Validates the template structure before any token or daemon access.
        let rendered = render(&template, &staging.join("real"), &staging.join("fake"))?;
        Ok(Plan {
            policy_name: format!("{POLICY_PREFIX}/{}/{}/{id}", self.builtin.name(), owner.uid),
            selector,
            owner,
            id,
            staging,
            delivered,
            rendered,
        })
    }

    /// Builds the result; gateway state is informational and never fails the run.
    fn report(
        &self,
        socket: &Path,
        timeout: Duration,
        plan: &Plan,
        created: &Created,
        bindings: &Value,
    ) -> Value {
        let status = call(socket, timeout, GATEWAY_STATUS, &json!({})).unwrap_or(Value::Null);
        let enabled = status["enabled"].as_bool().unwrap_or(false);
        let ready = status["ready"].as_bool().unwrap_or(false);
        let ca = if ready {
            call(socket, timeout, GATEWAY_CA, &json!({}))
                .ok()
                .and_then(|value| value["caBundlePath"].as_str().map(str::to_owned))
        } else {
            None
        };
        let mut next = vec![format!(
            "start the Agent as UID {} with GH_TOKEN/GITHUB_TOKEN read from {}",
            plan.owner.uid,
            plan.delivered.display()
        )];
        if ca.is_some() {
            next.push(
                "point SSL_CERT_FILE/REQUESTS_CA_BUNDLE/GIT_SSL_CAINFO at caBundlePath".into(),
            );
        } else if !enabled {
            next.push("run `agent-sec-cli gateway enable` as root to start injection".into());
        }
        let pair_ids: std::collections::BTreeSet<&str> = created.policy["credentialBindings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|item| item["pairId"].as_str())
            .collect();
        json!({
            "builtin": self.builtin.name(),
            "ownerUid": plan.owner.uid,
            "policyId": created.policy_id,
            "policyRevision": created.revision,
            "pairIds": pair_ids,
            "scopeId": created.scope_id,
            "fakeTokenFile": plan.delivered,
            "gateway": {"enabled": enabled, "ready": ready, "caBundlePath": ca},
            "bindings": bindings,
            "next": next,
            "teardown": created.teardown,
        })
    }

    fn template_path(&self) -> PathBuf {
        Path::new(GATEWAY_DATADIR).join(format!("{}.cedar", self.builtin.name()))
    }

    fn template(&self) -> io::Result<CedarPolicySource> {
        let path = self.template_path();
        let bytes = read_root_asset(&path, TEMPLATE_LIMIT).map_err(io::Error::other)?;
        let text = String::from_utf8(bytes)
            .map_err(|_| failure(format!("{} is not valid UTF-8", path.display())))?;
        Ok(CedarPolicySource {
            schema: CedarSchemaReference {
                schema_id: ResourceId::new(asc_policy_cedar::schema::SCHEMA_ID)
                    .map_err(io::Error::other)?,
                revision: Revision::new(asc_policy_cedar::schema::HTTP_SCHEMA_REVISION)
                    .map_err(io::Error::other)?,
            },
            text,
        })
    }

    fn resolve_owner(&self) -> io::Result<ResolvedOwner> {
        let user = match (&self.owner.owner, self.owner.owner_uid) {
            (Some(name), None) => Some(
                nix::unistd::User::from_name(name)
                    .map_err(io::Error::other)?
                    .ok_or_else(|| failure(format!("unknown owner {name:?}")))?,
            ),
            (None, Some(uid)) => nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
                .map_err(io::Error::other)?,
            _ => unreachable!("clap requires exactly one owner; fields are private"),
        };
        let owner = if let Some(user) = user {
            ResolvedOwner {
                uid: user.uid.as_raw(),
                gid: user.gid.as_raw(),
                home: Some(user.dir),
            }
        } else {
            return Err(failure("the Agent owner UID must have a passwd entry"));
        };
        if owner.uid == 0 {
            return Err(failure("the Agent owner must not be root"));
        }
        Ok(owner)
    }

    fn output_dir(&self, owner: &ResolvedOwner) -> io::Result<PathBuf> {
        match (&self.output_dir, &owner.home) {
            (Some(directory), _) => std::path::absolute(directory),
            (None, Some(home)) => Ok(home.join(".config/agent-sec/gateway")),
            (None, None) => Err(failure(
                "the owner has no passwd entry; pass --output-dir or --fake-token-file",
            )),
        }
    }

    /// Refuses a second active injection Scope for the same owner and selector.
    fn reject_duplicate(
        &self,
        socket: &Path,
        timeout: Duration,
        owner_uid: u32,
        selector: &ScopeSelector,
    ) -> io::Result<()> {
        let selector = serde_json::to_value(selector).map_err(io::Error::other)?;
        let prefix = format!("{POLICY_PREFIX}/{}/", self.builtin.name());
        for scope in list_all(socket, timeout, POLICY_SCOPES_LIST)? {
            let injected = scope["policySnapshots"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|policy| {
                    policy["policyName"]
                        .as_str()
                        .is_some_and(|name| name.starts_with(&prefix))
                });
            if scope["status"] == "ACTIVE"
                && scope["ownerUid"] == owner_uid
                && scope["selector"] == selector
                && injected
            {
                return Err(failure(format!(
                    "Scope {} already injects this credential for the same owner and selector; \
                     delete it first",
                    scope["scopeId"].as_str().unwrap_or("?")
                )));
            }
        }
        Ok(())
    }
}

/// Registers the staged pair through `policy create`, then assigns it with `scope create`.
fn create(
    socket: &Path,
    timeout: Duration,
    plan: &Plan,
    real: &Secret,
    fake: &str,
) -> io::Result<Created> {
    let policy = {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut staging = Staging {
            path: plan.staging.clone(),
            retain: false,
        };
        write_owner_file(&staging.path, "real", real.expose(), 0, 0).map_err(io::Error::other)?;
        write_owner_file(&staging.path, "fake", fake, 0, 0).map_err(io::Error::other)?;
        let params = CreatePolicyParams {
            policy_name: plan.policy_name.clone(),
            template: PolicySource::Cedar(plan.rendered.clone()),
        };
        let recovery = staging.path.join("policy-source.json");
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&recovery)?;
        file.write_all(&serde_json::to_vec_pretty(&params.template).map_err(io::Error::other)?)?;
        file.sync_all()?;
        File::open(&staging.path)?.sync_all()?;
        call(socket, timeout, POLICY_TEMPLATES_CREATE, &params).map_err(|error| {
            // Registration may commit before Policy persistence, so preserve its exact source.
            staging.retain = true;
            failure(format!(
                "{error}; root-only recovery files retained in {}. Find Policy {:?} with \
                 `agent-sec-cli policy list`; if absent, after fixing the failure run \
                 `agent-sec-cli policy create --name {} --file {} --language json` to recover \
                 the same registration and its pair IDs. Check for referencing Scopes before \
                 deleting the Policy or retiring unused pairs. Remove recovery files after cleanup",
                staging.path.display(),
                plan.policy_name,
                plan.policy_name,
                recovery.display()
            ))
        })?
    };
    let policy_id: ResourceId = field(&policy, "policyId")?;
    let revision: Revision = field(&policy, "revision")?;
    let delete_policy = format!(
        "agent-sec-cli policy delete --policy-id {policy_id} --revision {}",
        revision.get()
    );
    let scope = call(
        socket,
        timeout,
        POLICY_SCOPES_CREATE,
        &CreateScopeParams {
            selector: plan.selector.clone(),
            owner_uid: Some(plan.owner.uid),
            policy_templates: vec![PolicyReference {
                policy_id: policy_id.clone(),
                policy_revision: revision,
            }],
        },
    )
    .map_err(|error| {
        failure(format!(
            "{error}; Scope admission may have committed: list Scopes referencing Policy {policy_id}, \
             delete those Scopes and wait for cleanup before running `{delete_policy}`. \
             If no Scope was created, retire newly imported pair IDs {:?} using \
             `agent-sec-cli gateway key remove PAIR_ID` only after checking they have no other users",
            policy["credentialBindings"].as_array().into_iter().flatten()
                .filter_map(|binding| binding["pairId"].as_str())
                .collect::<std::collections::BTreeSet<_>>()
        ))
    })?;
    let scope_id: ResourceId = field(&scope, "scopeId")?;
    Ok(Created {
        teardown: [
            format!("agent-sec-cli scope delete --scope-id {scope_id}"),
            delete_policy,
        ],
        policy,
        policy_id,
        revision,
        scope_id,
    })
}

/// Renders the packaged template with per-run token paths.
fn render(template: &CedarPolicySource, real: &Path, fake: &Path) -> io::Result<CedarPolicySource> {
    let (Some(real), Some(fake)) = (real.to_str(), fake.to_str()) else {
        return Err(failure("token file paths must be UTF-8"));
    };
    asc_policy_cedar::rewrite_token_files(template, real, fake).map_err(io::Error::other)
}

fn rule_count(source: &CedarPolicySource) -> usize {
    asc_policy_cedar::credential_rules(source).map_or(0, |rules| rules.len())
}

fn fake_name(builtin: Builtin, id: &str) -> String {
    format!("{}-{id}.fake", builtin.name())
}

/// Reads a token file owned by root or the Agent owner.
fn read_private(path: &Path, owner_uid: u32) -> io::Result<Secret> {
    use std::os::unix::fs::MetadataExt as _;
    let uid = std::fs::symlink_metadata(path)?.uid();
    if uid != 0 && uid != owner_uid {
        return Err(failure(format!(
            "{} must be owned by root or the Agent owner",
            path.display()
        )));
    }
    read_token_file(path, uid).map_err(io::Error::other)
}

fn random_id() -> io::Result<String> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes
        .iter()
        .fold(String::with_capacity(32), |mut text, byte| {
            let _ = std::fmt::Write::write_fmt(&mut text, format_args!("{byte:02x}"));
            text
        }))
}

fn call(
    socket: &Path,
    timeout: Duration,
    method: &str,
    params: &impl serde::Serialize,
) -> io::Result<Value> {
    let request = encode(method, params).map_err(io::Error::other)?;
    match asc_daemon_client::call(socket, &request, timeout).map_err(io::Error::other)? {
        DaemonResponse::Success(response) => Ok(response.result),
        DaemonResponse::Error(response) => Err(failure(format!(
            "{method} failed ({}): {}",
            response.error.code,
            response.error.message()
        ))),
    }
}

fn list_all(socket: &Path, timeout: Duration, method: &str) -> io::Result<Vec<Value>> {
    let mut items = Vec::new();
    loop {
        let offset = u32::try_from(items.len()).map_err(io::Error::other)?;
        let page = call(
            socket,
            timeout,
            method,
            &ListParams {
                limit: LIST_PAGE,
                offset,
            },
        )?;
        let batch = page["items"].as_array().cloned().unwrap_or_default();
        let total = page["total"].as_u64().unwrap_or(0);
        let done = batch.is_empty();
        items.extend(batch);
        if done || u64::try_from(items.len()).unwrap_or(u64::MAX) >= total {
            return Ok(items);
        }
    }
}

/// Polls until every Binding of `scope_id` is READY, one fails, or `limit` elapses.
fn wait_bindings(
    socket: &Path,
    timeout: Duration,
    scope_id: &ResourceId,
    limit: Duration,
) -> (Value, io::Result<()>) {
    let deadline = Instant::now() + limit;
    loop {
        let bindings = match list_all(socket, timeout, POLICY_BINDINGS_LIST) {
            Ok(items) => items
                .into_iter()
                .filter(|item| item["spec"]["scope"]["scopeId"] == scope_id.as_str())
                .map(|item| {
                    json!({
                        "bindingId": item["bindingId"],
                        "pid": item["spec"]["scope"]["process"]["pid"],
                        "phase": item["status"]["phase"],
                    })
                })
                .collect::<Vec<_>>(),
            Err(error) => return (Value::Null, Err(error)),
        };
        let phases: Vec<&str> = bindings
            .iter()
            .filter_map(|item| item["phase"].as_str())
            .collect();
        let outcome = if phases.iter().any(|phase| phase.ends_with("FAILED")) {
            Some(Err(failure("a Binding of the new Scope failed")))
        } else if !phases.is_empty() && phases.iter().all(|phase| *phase == "READY") {
            Some(Ok(()))
        } else if Instant::now() >= deadline {
            Some(Err(failure(
                "timed out waiting for the Scope's Bindings to become READY; \
                 is the Agent process running as the owner?",
            )))
        } else {
            None
        };
        if let Some(outcome) = outcome {
            return (Value::Array(bindings), outcome);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn field<T: serde::de::DeserializeOwned>(value: &Value, name: &str) -> io::Result<T> {
    serde_json::from_value(value[name].clone())
        .map_err(|_| failure(format!("daemon result has no valid {name}")))
}

fn print_json(stdout: &mut impl Write, value: &Value) -> io::Result<u8> {
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    writeln!(stdout, "{text}")?;
    Ok(0)
}

fn failure(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Arguments {
        #[command(flatten)]
        command: InjectCredentialCommand,
    }

    fn parse(extra: &[&str]) -> Result<InjectCredentialCommand, clap::Error> {
        let mut argv = vec![
            "inject-credential",
            "--builtin",
            "github",
            "--real-token-file",
            "/root/real",
        ];
        argv.extend_from_slice(extra);
        Arguments::try_parse_from(argv).map(|arguments| arguments.command)
    }

    #[test]
    fn exactly_one_owner_and_one_process_selector_are_required() {
        let command = parse(&["--owner-uid", "1000", "--executable", "/usr/bin/agent"]).unwrap();
        assert_eq!(
            command.selector.value(),
            ScopeSelector::Process {
                matcher: ProcessMatcher::Executable {
                    executable: "/usr/bin/agent".into()
                }
            }
        );
        for extra in [
            &["--executable", "/usr/bin/agent"][..],
            &["--owner-uid", "1000"],
            &["--owner-uid", "0", "--executable", "/a"],
            &["--owner", "u", "--owner-uid", "1000", "--executable", "/a"],
            &[
                "--owner-uid",
                "1000",
                "--executable",
                "/a",
                "--process-name",
                "a",
            ],
            &["--owner-uid", "1000", "--pid", "1"],
            &[
                "--owner-uid",
                "1000",
                "--executable",
                "/a",
                "--real-token-stdin",
            ],
            &[
                "--owner-uid",
                "1000",
                "--executable",
                "/a",
                "--fake-token-file",
                "/f",
                "--output-dir",
                "/d",
            ],
        ] {
            assert!(parse(extra).is_err(), "{extra:?}");
        }
        let other = ["inject-credential", "--builtin", "gitlab"];
        assert!(Arguments::try_parse_from(other).is_err());
    }

    #[test]
    fn templates_resolve_only_inside_the_packaged_directory() {
        let command = parse(&["--owner-uid", "1000", "--cmdline-arg", "/opt/agent"]).unwrap();
        assert_eq!(
            command.template_path(),
            Path::new(GATEWAY_DATADIR).join("github.cedar")
        );
        assert_eq!(fake_name(Builtin::Github, "ab"), "github-ab.fake");
    }

    #[test]
    fn failed_policy_admission_preserves_exact_registration_recovery() {
        use std::io::BufRead as _;
        use std::os::unix::net::UnixListener;
        if !cfg!(target_os = "linux") || !rustix::process::geteuid().is_root() {
            return;
        }
        let directory = tempfile::tempdir_in(std::fs::canonicalize(".").unwrap()).unwrap();
        let socket = directory.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            std::io::BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["method"],
                POLICY_TEMPLATES_CREATE
            );
            let response = DaemonResponse::<Value>::error(
                asc_daemon_protocol::RequestId::new("failed-policy").unwrap(),
                "internal",
                "Policy persistence failed",
            );
            writeln!(stream, "{}", serde_json::to_string(&response).unwrap()).unwrap();
        });
        let rendered = CedarPolicySource {
            schema: asc_policy_cedar::schema::embedded_reference(),
            text: "permit(principal, action, resource);".into(),
        };
        let plan = Plan {
            owner: ResolvedOwner {
                uid: 1000,
                gid: 1000,
                home: None,
            },
            selector: ScopeSelector::Process {
                matcher: ProcessMatcher::Name {
                    process_name: "agent".into(),
                },
            },
            id: "recovery".into(),
            policy_name: "inject-credential/github/1000/recovery".into(),
            staging: directory.path().join("staged"),
            delivered: directory.path().join("delivered"),
            rendered: rendered.clone(),
        };
        let real = Secret::new("aaaaaaaaaaaaaaaaaaaa".into()).unwrap();
        let error = create(
            &socket,
            Duration::from_secs(5),
            &plan,
            &real,
            "bbbbbbbbbbbbbbbbbbbb",
        )
        .err()
        .unwrap()
        .to_string();
        assert!(
            error.contains("root-only recovery files retained"),
            "{error}"
        );
        server.join().unwrap();
        assert!(!error.contains(real.expose()));
        assert_eq!(
            read_token_file(&plan.staging.join("real"), 0)
                .unwrap()
                .expose(),
            real.expose()
        );
        let recovery =
            read_root_asset(&plan.staging.join("policy-source.json"), TEMPLATE_LIMIT).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&recovery).unwrap(),
            serde_json::to_value(PolicySource::Cedar(rendered)).unwrap()
        );
        assert!(!plan.delivered.exists());
    }

    #[test]
    fn random_ids_are_unique_lowercase_hex() {
        let (first, second) = (random_id().unwrap(), random_id().unwrap());
        assert_ne!(first, second);
        assert_eq!(first.len(), 32);
        assert!(
            first
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        );
    }
}
