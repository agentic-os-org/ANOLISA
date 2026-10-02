//! SkillFS × external decision command — adapter + result types.
//!
//! Background. The External Decision Protocol
//! (`docs/security/external-decision-protocol.md`) lets SkillFS delegate
//! "which physical entry point should `/skills/<skill>` expose right
//! now?" to an external decision provider. The provider is invoked as a
//! subprocess via a generic, provider-neutral command prefix called a
//! **decision-command** — see [`DecisionCommand`]. Ships two
//! subcommands on the contract:
//!
//! ```text
//! <decision-command> scan <skill_dir> --json
//! <decision-command> resolve <skill_dir> --json
//! ```
//!
//! `scan` runs first (so a provider can refresh its own internal state
//! after a FUSE-observed mutation); SkillFS does **not** parse the scan
//! JSON beyond exit-status / failure reporting. `resolve` is the only
//! command whose JSON is consumed and validated; see
//! [`LedgerResolveResult::from_json_str`] for the strict validator.
//!
//! Scope discipline (intentionally **out of scope** here):
//!
//! * `check` / `certify` subcommands of the decision provider;
//! * a Unix-socket / daemon transport (uses subprocess);
//! * trusted-writer identity, lifecycle state machine,
//!   `.skill-meta` write enablement;
//! * production fail-open / fail-closed policy — every error path here
//!   surfaces an explicit [`LedgerError`] for the caller to decide.
//!
//! Default behavior is **unchanged**: nothing constructs a
//! [`LedgerAdapter`] today unless the CLI is invoked with
//! `--security` and `--decision-command <COMMAND>`. The FUSE
//! callbacks are otherwise untouched.
//!
//! ---
//!
//! ## JSON contract (D1.0 strict subset)
//!
//! The strict validator only accepts the documented `schemaVersion == 1`
//! shape. Unknown fields are tolerated (forward-compat with a future
//! ledger that adds fields), but every required field must be present
//! and well-typed. Extra-strict rules layered on top of the type checks:
//!
//! * `decision == "fallback"` **requires** `target` AND `targetKind`,
//!   and the only currently accepted `targetKind` is
//!   `"relative_to_skill_dir"`. The `target` is further restricted to
//!   a relative path under `.skill-meta/versions/` with at least one
//!   non-empty component after the prefix (e.g. `v000001.snapshot`,
//!   `v000001/snapshot/`). Absolute paths and `..` traversal are
//!   rejected so a malicious or buggy ledger cannot point SkillFS at an
//!   arbitrary location.
//! * `decision == "current"` ignores any `target` field (the entry is
//!   served from the live `<skill_dir>` itself).
//! * `decision == "hidden"` ignores `target`; `reason` is recommended
//!   but not required by D1.0.
//! * `status` must be one of `none|pass|warn|deny|drifted|tampered`.
//! * `decision` must be one of `current|fallback|hidden`.
//!
//! The strict subset matches the §4.2 schema; relaxations live
//! behind explicit follow-up packages (H1 hook protocol, C2 active
//! mapping persistence) and not behind silent fallbacks here.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// Reserved prefix for snapshot targets returned by the ledger.
///
/// The contract only accepts `target` paths that are relative to the
/// skill directory **and** sit under this prefix. Centralized here so the
/// validator and the active-mapping consumer agree byte-for-byte.
pub const LEDGER_SNAPSHOT_PREFIX: &str = ".skill-meta/versions";

/// The only `schemaVersion` accepted by D1.0.
pub const LEDGER_SCHEMA_VERSION: u64 = 1;

/// Coarse trust status of the Skill the ledger was asked about.
///
/// Mirrors the table in
/// `docs/security-ledger-integration-plan.md` §4.3. New variants may be
/// added by later phases; until then, an unknown string is a hard parse
/// error so the gate cannot silently treat an unrecognized status as
/// "current".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerStatus {
    /// No certified version exists.
    None,
    /// Current version is certified and trusted.
    Pass,
    /// Current version has soft findings (low severity).
    Warn,
    /// Current version has hard findings (high severity).
    Deny,
    /// Current version diverged from the certified manifest.
    Drifted,
    /// Current version failed integrity checks (manifest / signature).
    Tampered,
}

impl LedgerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            LedgerStatus::None => "none",
            LedgerStatus::Pass => "pass",
            LedgerStatus::Warn => "warn",
            LedgerStatus::Deny => "deny",
            LedgerStatus::Drifted => "drifted",
            LedgerStatus::Tampered => "tampered",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Self::None),
            "pass" => Some(Self::Pass),
            "warn" => Some(Self::Warn),
            "deny" => Some(Self::Deny),
            "drifted" => Some(Self::Drifted),
            "tampered" => Some(Self::Tampered),
            _ => None,
        }
    }
}

/// What the ledger thinks SkillFS should expose for the Skill.
///
/// Pure decision channel: SkillFS turns each variant into an
/// [`crate::security::active::ActiveTarget`] via
/// [`LedgerResolveResult::into_active_target`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerDecision {
    /// Expose the live source directory as-is.
    Current,
    /// Expose a trusted snapshot under `.skill-meta/versions/...`.
    Fallback,
    /// Hide the Skill (no entry under `/skills`).
    Hidden,
}

impl LedgerDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            LedgerDecision::Current => "current",
            LedgerDecision::Fallback => "fallback",
            LedgerDecision::Hidden => "hidden",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "current" => Some(Self::Current),
            "fallback" => Some(Self::Fallback),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }
}

/// How the ledger expresses the `target` field. D1.0 only accepts
/// `relative_to_skill_dir`; any other value is rejected at parse time
/// because the consumer cannot safely interpret absolute or
/// foreign-rooted paths from the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerTargetKind {
    RelativeToSkillDir,
}

impl LedgerTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LedgerTargetKind::RelativeToSkillDir => "relative_to_skill_dir",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "relative_to_skill_dir" => Some(Self::RelativeToSkillDir),
            _ => None,
        }
    }
}

/// Strongly-typed `resolve` result.
///
/// Built only via [`LedgerResolveResult::from_json_str`] or
/// [`LedgerResolveResult::from_json_value`]; the inner invariants
/// (decision/target/targetKind agreement) are enforced by the parser
/// and do not need to be rechecked downstream.
///
/// `skill_name` is the canonical SkillFS identity for the response and
/// must equal `basename(skill_dir)` for the request that produced it.
/// The optional `declared_name` is the provider's observation of the
/// `name:` field inside `SKILL.md`; SkillFS treats it as metadata only
/// and never uses it as a path key. Callers that own the canonical
/// directory name (the CLI bootstrap, the refresh controller, the
/// inbox install path) consume the result through
/// [`LedgerResolveResult::validate_for_expected_skill`] so a
/// `skillName` mismatch is rejected before the resolver is updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerResolveResult {
    pub schema_version: u64,
    pub skill_name: String,
    /// Optional provider-reported `name:` from the on-disk `SKILL.md`.
    /// Pure metadata; SkillFS never derives path identity from this field.
    pub declared_name: Option<String>,
    pub status: LedgerStatus,
    pub decision: LedgerDecision,
    /// Present iff `decision == Fallback`; always a relative path under
    /// [`LEDGER_SNAPSHOT_PREFIX`].
    pub target: Option<PathBuf>,
    /// Present iff `target` is present.
    pub target_kind: Option<LedgerTargetKind>,
    pub current_version: Option<String>,
    pub trusted_version: Option<String>,
    pub reason: Option<String>,
}

/// Raw, untyped representation produced by `serde`. Used only as an
/// intermediate during parsing — every field is then re-validated by
/// [`LedgerResolveResult::from_json_value`] so the public type is always
/// the strict subset.
#[derive(Debug, Deserialize)]
struct RawResolve {
    #[serde(rename = "schemaVersion")]
    schema_version: Option<Value>,
    #[serde(rename = "skillName")]
    skill_name: Option<String>,
    #[serde(rename = "declaredName")]
    declared_name: Option<String>,
    status: Option<String>,
    decision: Option<String>,
    target: Option<String>,
    #[serde(rename = "targetKind")]
    target_kind: Option<String>,
    #[serde(rename = "currentVersion")]
    current_version: Option<String>,
    #[serde(rename = "trustedVersion")]
    trusted_version: Option<String>,
    reason: Option<String>,
}

impl LedgerResolveResult {
    /// Parse a `resolve --json` payload from raw bytes.
    pub fn from_json_str(s: &str) -> Result<Self, LedgerError> {
        let value: Value = serde_json::from_str(s).map_err(|e| LedgerError::InvalidJson {
            reason: e.to_string(),
        })?;
        Self::from_json_value(value)
    }

    /// Parse a `resolve --json` payload from a pre-decoded JSON value.
    pub fn from_json_value(value: Value) -> Result<Self, LedgerError> {
        let raw: RawResolve =
            serde_json::from_value(value).map_err(|e| LedgerError::InvalidJson {
                reason: e.to_string(),
            })?;

        let schema_version = match raw.schema_version {
            Some(Value::Number(n)) => n
                .as_u64()
                .ok_or(LedgerError::UnsupportedSchemaVersion { got: n.to_string() })?,
            Some(other) => {
                return Err(LedgerError::UnsupportedSchemaVersion {
                    got: other.to_string(),
                });
            }
            None => {
                return Err(LedgerError::MissingField {
                    field: "schemaVersion",
                });
            }
        };
        if schema_version != LEDGER_SCHEMA_VERSION {
            return Err(LedgerError::UnsupportedSchemaVersion {
                got: schema_version.to_string(),
            });
        }

        let skill_name = raw
            .skill_name
            .ok_or(LedgerError::MissingField { field: "skillName" })?;
        validate_skill_name_component(&skill_name)?;

        let status_str = raw
            .status
            .ok_or(LedgerError::MissingField { field: "status" })?;
        let status = LedgerStatus::parse(&status_str).ok_or(LedgerError::UnknownStatus {
            got: status_str.clone(),
        })?;

        let decision_str = raw
            .decision
            .ok_or(LedgerError::MissingField { field: "decision" })?;
        let decision =
            LedgerDecision::parse(&decision_str).ok_or(LedgerError::UnknownDecision {
                got: decision_str.clone(),
            })?;

        let (target, target_kind) = match decision {
            LedgerDecision::Fallback => {
                let target_str = raw.target.ok_or(LedgerError::MissingField {
                    field: "target (required when decision=fallback)",
                })?;
                let target_kind_str = raw.target_kind.ok_or(LedgerError::MissingField {
                    field: "targetKind (required when decision=fallback)",
                })?;
                let target_kind =
                    LedgerTargetKind::parse(&target_kind_str).ok_or(LedgerError::InvalidField {
                        field: "targetKind",
                        reason: format!(
                            "only '{}' is accepted, got '{}'",
                            LedgerTargetKind::RelativeToSkillDir.as_str(),
                            target_kind_str
                        ),
                    })?;
                let path = validate_snapshot_target(&target_str)?;
                (Some(path), Some(target_kind))
            }
            LedgerDecision::Current | LedgerDecision::Hidden => (None, None),
        };

        Ok(Self {
            schema_version,
            skill_name,
            declared_name: raw.declared_name,
            status,
            decision,
            target,
            target_kind,
            current_version: raw.current_version,
            trusted_version: raw.trusted_version,
            reason: raw.reason,
        })
    }

    /// Reject this result if its `skillName` does not match the canonical
    /// SkillFS identity (`basename(skill_dir)`) the request was made for.
    ///
    /// SkillFS keys every active mapping by the directory name, so a
    /// resolve response whose `skillName` differs from the requested
    /// directory cannot be installed safely. The optional
    /// [`Self::declared_name`] field is intentionally **not** consulted
    /// here — providers may report a mismatching `SKILL.md` `name:` as a
    /// security signal, but path identity stays anchored to the directory
    /// name.
    pub fn validate_for_expected_skill(&self, expected: &str) -> Result<(), LedgerError> {
        if self.skill_name == expected {
            Ok(())
        } else {
            Err(LedgerError::SkillNameMismatch {
                expected: expected.to_string(),
                actual: self.skill_name.clone(),
            })
        }
    }
}

/// Maximum byte length of a `skillName` field accepted by the strict
/// validator. Matches the canonical limit used by
/// `skillfs-core::parser::validate_name` so the ledger surface cannot
/// accept names the parser would reject — and more importantly, so the
/// downstream consumer (`ActiveSkillResolver::set_from_resolve`) can
/// safely `source_root.join(skill_name)` without unbounded growth.
pub const MAX_SKILL_NAME_LEN: usize = 64;

/// Strict validator for the `skillName` field.
///
/// Intentionally treats `skillName` as a **single path component**, not
/// as a free-form string. The downstream
/// [`crate::security::active::ActiveSkillResolver::set_from_resolve`]
/// turns the validated name into a real filesystem path via
/// `source_root.join(skill_name)`; a value like `../evil`,
/// `alpha/beta`, `/tmp/evil`, or `alpha\\beta` would escape the source
/// root or land somewhere outside the intended Skill directory.
/// Validating here means D1.x can wire the resolver into `lookup` /
/// `readdir` / `open` without re-checking the boundary at every call
/// site.
///
/// The rules are deliberately about *safety*, not about SkillFS naming
/// conventions:
///
/// * non-empty;
/// * not exactly `.` or `..`;
/// * no `/` or `\\` separators (path traversal);
/// * no NUL byte (filesystem syscall boundary);
/// * not an absolute path (`/foo`, on POSIX);
/// * byte length ≤ [`MAX_SKILL_NAME_LEN`].
///
/// What this does **not** enforce is the kebab-case shape pinned by
/// `skillfs-core::parser::validate_name`. The product naming policy
/// (whether `Alpha` or `alpha_1` is a legal Skill name) is the
/// parser's job; the ledger surface only needs the path-component
/// safety guarantee so a hostile or buggy ledger cannot point SkillFS
/// at an arbitrary inode. Names that pass safety but fail naming
/// policy will surface later via the store's own load errors, where
/// they are easier to attribute to the right source.
pub(crate) fn validate_skill_name_component(name: &str) -> Result<(), LedgerError> {
    if name.is_empty() {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: "must be non-empty".to_string(),
        });
    }
    if name.len() > MAX_SKILL_NAME_LEN {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: format!(
                "must be at most {MAX_SKILL_NAME_LEN} bytes, got {}",
                name.len()
            ),
        });
    }
    if name == "." || name == ".." {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: format!("must not be '{name}' (path traversal)"),
        });
    }
    if name.contains('/') || name.contains('\\') {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: format!("must not contain path separators: '{name}'"),
        });
    }
    if name.contains('\0') {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: "must not contain NUL bytes".to_string(),
        });
    }
    // Defense in depth: even after the separator checks above, refuse
    // anything that does not lexically reduce to a single Normal
    // component. This catches host-specific edge cases (e.g. a future
    // `Path` interpretation that treats a leading character as a root
    // prefix) without trusting the explicit checks alone.
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: format!("must be relative, got absolute '{name}'"),
        });
    }
    let mut comps = path.components();
    let first = comps.next();
    if comps.next().is_some() || !matches!(first, Some(std::path::Component::Normal(_))) {
        return Err(LedgerError::InvalidField {
            field: "skillName",
            reason: format!("must be a single path component: '{name}'"),
        });
    }
    Ok(())
}

/// Strict validator for a `relative_to_skill_dir` snapshot path.
///
/// Returns the normalized [`PathBuf`] (verbatim, no canonicalization) on
/// success. The constraints are deliberately narrow:
///
/// * non-empty;
/// * not absolute;
/// * no `.` / `..` / Windows prefix components;
/// * starts with the literal two-component prefix
///   `.skill-meta/versions`;
/// * has at least one component beyond the prefix (so a bare
///   `.skill-meta/versions` is rejected — that is a directory, not a
///   snapshot).
///
/// The check is lexical only — the consumer is still responsible for
/// joining the result onto the real skill directory and refusing the
/// mount if the resolved path does not exist. Doing the existence check
/// here would couple D1.0 to a real filesystem, which the unit tests
/// avoid on purpose.
fn validate_snapshot_target(raw: &str) -> Result<PathBuf, LedgerError> {
    if raw.is_empty() {
        return Err(LedgerError::InvalidField {
            field: "target",
            reason: "must be non-empty".to_string(),
        });
    }
    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(LedgerError::InvalidField {
            field: "target",
            reason: format!("must be relative, got absolute '{raw}'"),
        });
    }
    use std::path::Component;
    for c in path.components() {
        match c {
            Component::Normal(_) => {}
            Component::CurDir | Component::ParentDir => {
                return Err(LedgerError::InvalidField {
                    field: "target",
                    reason: format!("must not contain '.' or '..' components: '{raw}'"),
                });
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err(LedgerError::InvalidField {
                    field: "target",
                    reason: format!("must be relative, got rooted '{raw}'"),
                });
            }
        }
    }
    let mut comps = path.components();
    let first = comps.next().and_then(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let second = comps.next().and_then(|c| match c {
        Component::Normal(s) => s.to_str(),
        _ => None,
    });
    let rest_exists = comps.next().is_some();
    let prefix_ok = matches!((first, second), (Some(".skill-meta"), Some("versions")));
    if !prefix_ok || !rest_exists {
        return Err(LedgerError::InvalidField {
            field: "target",
            reason: format!("must be under '{LEDGER_SNAPSHOT_PREFIX}/<snapshot>', got '{raw}'"),
        });
    }
    Ok(path.to_path_buf())
}

/// Error surface for adapter + parser failures.
///
/// Variants are stable strings so a future hook handler can map them
/// onto the policy decision (fail-open / fail-closed) it
/// chooses; D1.0 does not pick that policy on the caller's behalf.
#[derive(Debug)]
pub enum LedgerError {
    /// `serde_json` could not decode the bytes as JSON at all.
    InvalidJson { reason: String },
    /// JSON decoded but a required field was missing.
    MissingField { field: &'static str },
    /// JSON decoded and the field was present but failed a type or
    /// content check.
    InvalidField { field: &'static str, reason: String },
    /// `schemaVersion` is not `1`.
    UnsupportedSchemaVersion { got: String },
    /// `status` carried an unrecognized string.
    UnknownStatus { got: String },
    /// `decision` carried an unrecognized string.
    UnknownDecision { got: String },
    /// The provider returned a `skillName` that does not match the
    /// canonical SkillFS identity (`basename(skill_dir)`) for the request.
    /// SkillFS keys the active mapping by the directory name, so a
    /// mismatched response cannot be installed; callers apply the
    /// configured failure policy.
    SkillNameMismatch { expected: String, actual: String },
    /// The subprocess exited non-zero. Stdout / stderr are preserved so
    /// the caller can surface them in an operator-visible startup error.
    NonZeroExit {
        status: i32,
        stdout: String,
        stderr: String,
    },
    /// `std::process::Command::output` itself failed (e.g. binary not
    /// found, EACCES). The wrapped error chains the underlying
    /// `io::Error`.
    Spawn {
        binary: PathBuf,
        source: std::io::Error,
    },
    /// The subprocess did not exit within the configured timeout.
    Timeout {
        kind: &'static str,
        timeout: Duration,
    },
}

impl std::fmt::Display for LedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LedgerError::InvalidJson { reason } => {
                write!(f, "ledger resolve produced invalid JSON: {reason}")
            }
            LedgerError::MissingField { field } => {
                write!(f, "ledger resolve JSON missing required field: {field}")
            }
            LedgerError::InvalidField { field, reason } => {
                write!(f, "ledger resolve field '{field}' is invalid: {reason}")
            }
            LedgerError::UnsupportedSchemaVersion { got } => {
                write!(
                    f,
                    "ledger resolve schemaVersion '{got}' is not supported (expected {LEDGER_SCHEMA_VERSION})"
                )
            }
            LedgerError::UnknownStatus { got } => {
                write!(f, "ledger resolve status '{got}' is not recognized")
            }
            LedgerError::UnknownDecision { got } => {
                write!(f, "ledger resolve decision '{got}' is not recognized")
            }
            LedgerError::SkillNameMismatch { expected, actual } => {
                write!(f, "skillName mismatch: expected {expected}, got {actual}")
            }
            LedgerError::NonZeroExit {
                status,
                stdout,
                stderr,
            } => {
                let stderr = stderr.trim();
                let stdout = stdout.trim();
                write!(
                    f,
                    "ledger resolve exited with status {status}; stderr='{stderr}'; stdout='{stdout}'"
                )
            }
            LedgerError::Spawn { binary, source } => {
                write!(
                    f,
                    "failed to invoke ledger CLI '{}': {source}",
                    binary.display()
                )
            }
            LedgerError::Timeout { kind, timeout } => {
                write!(
                    f,
                    "decision command {} timed out after {:.1}s",
                    kind,
                    timeout.as_secs_f64()
                )
            }
        }
    }
}

impl std::error::Error for LedgerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LedgerError::Spawn { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// External decision-provider command prefix that SkillFS invokes.
///
/// A `DecisionCommand` is parsed from a single whitespace-separated
/// string (the operator-supplied `--decision-command <COMMAND>` value).
/// The first token is the executable path or PATH-resolvable name; every
/// remaining token is a fixed argument prepended to each invocation.
/// Per-call arguments (`scan <skill_dir> --json` or
/// `resolve <skill_dir> --json`) are appended at call time.
///
/// Examples:
///
/// ```text
/// --decision-command "/usr/local/bin/xxx-cli"
///   program   = "/usr/local/bin/xxx-cli"
///   fixed_args = []
///   scan argv  = ["scan", "<skill_dir>", "--json"]
///
/// --decision-command "agent-sec-cli skill-ledger"
///   program    = "agent-sec-cli"
///   fixed_args = ["skill-ledger"]
///   scan argv  = ["skill-ledger", "scan", "<skill_dir>", "--json"]
/// ```
///
/// Known limitations (deliberate):
///
/// * Whitespace split only. Shell quoting and backslash escaping are not
///   supported yet — a path containing spaces cannot currently be
///   expressed.
/// * Empty / whitespace-only input is rejected.
/// * SkillFS does **not** spawn through `sh -c`; the program is exec'd
///   directly via [`std::process::Command`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionCommand {
    program: PathBuf,
    fixed_args: Vec<String>,
}

impl DecisionCommand {
    /// Parse a whitespace-separated command prefix.
    pub fn parse(raw: &str) -> Result<Self, LedgerError> {
        if raw.trim().is_empty() {
            return Err(LedgerError::InvalidField {
                field: "decision-command",
                reason: "must be non-empty (got empty or whitespace-only)".to_string(),
            });
        }
        let mut tokens = raw.split_whitespace();
        // `split_whitespace` yields at least one non-empty token after the
        // trim-check above; the unwrap mirrors the invariant.
        let program = tokens
            .next()
            .expect("split_whitespace yields at least one token after non-empty trim");
        let fixed_args: Vec<String> = tokens.map(|s| s.to_string()).collect();
        Ok(Self {
            program: PathBuf::from(program),
            fixed_args,
        })
    }

    /// Executable to spawn (first token of the parsed command).
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Fixed argument list prepended to each invocation.
    pub fn fixed_args(&self) -> &[String] {
        &self.fixed_args
    }

    /// Full argv (without the program) for a `scan <skill_dir> --json`
    /// invocation. Pinned by tests so the contract cannot drift.
    pub fn build_scan_args(&self, skill_dir: &Path) -> Vec<String> {
        let mut argv = self.fixed_args.clone();
        argv.push("scan".to_string());
        argv.push(skill_dir.to_string_lossy().into_owned());
        argv.push("--json".to_string());
        argv
    }

    /// Full argv (without the program) for a `resolve <skill_dir> --json`
    /// invocation. Pinned by tests so the contract cannot drift.
    pub fn build_resolve_args(&self, skill_dir: &Path) -> Vec<String> {
        let mut argv = self.fixed_args.clone();
        argv.push("resolve".to_string());
        argv.push(skill_dir.to_string_lossy().into_owned());
        argv.push("--json".to_string());
        argv
    }
}

/// Adapter trait so the refresh controller and the tests can share
/// the same call sites. Implementations must be `Send + Sync` because the
/// demo handler lives behind an `Arc`.
pub trait LedgerAdapter: Send + Sync {
    /// Run `scan <skill_dir> --json` against the decision provider.
    ///
    /// SkillFS does not parse the scan stdout in D1.3.1 — only success
    /// vs. failure is consumed. Implementations surface failure as a
    /// typed [`LedgerError`] so the caller can pick a uniform
    /// fail-open / fail-closed policy regardless of transport.
    fn scan(&self, skill_dir: &Path) -> Result<(), LedgerError>;

    /// Run `resolve <skill_dir> --json` and return the parsed result.
    ///
    /// Implementations are expected to surface every error as a typed
    /// [`LedgerError`] so the caller's fail-open/fail-closed decision is
    /// uniform regardless of transport.
    fn resolve(&self, skill_dir: &Path) -> Result<LedgerResolveResult, LedgerError>;
}

/// Default timeout for decision command subprocesses.
pub const DEFAULT_DECISION_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Subprocess [`LedgerAdapter`] that runs an external decision provider
/// described by a [`DecisionCommand`].
///
/// For a command parsed from `"agent-sec-cli skill-ledger"` and a skill
/// directory `/srv/skills/demo-weather`, `scan` is equivalent to:
///
/// ```text
/// agent-sec-cli skill-ledger scan /srv/skills/demo-weather --json
/// ```
///
/// and `resolve` is equivalent to:
///
/// ```text
/// agent-sec-cli skill-ledger resolve /srv/skills/demo-weather --json
/// ```
///
/// Behavior contract:
///
/// * `resolve` stdout is parsed via [`LedgerResolveResult::from_json_str`];
///   only the strict D1.x subset is accepted.
/// * `scan` stdout is ignored; only the exit status is consumed.
/// * a non-zero exit code surfaces as [`LedgerError::NonZeroExit`] with
///   the captured stdout/stderr so the operator sees the failure mode.
/// * spawning the binary itself failing (PATH miss, EACCES) surfaces as
///   [`LedgerError::Spawn`].
/// * subprocesses that exceed `timeout` are killed and surface as
///   [`LedgerError::Timeout`].
#[derive(Debug, Clone)]
pub struct CliLedgerAdapter {
    command: DecisionCommand,
    timeout: Duration,
}

impl CliLedgerAdapter {
    /// Build a subprocess adapter that drives `command` with the default timeout.
    pub fn new(command: DecisionCommand) -> Self {
        Self {
            command,
            timeout: DEFAULT_DECISION_COMMAND_TIMEOUT,
        }
    }

    /// Build a subprocess adapter with a custom timeout.
    pub fn with_timeout(command: DecisionCommand, timeout: Duration) -> Self {
        Self { command, timeout }
    }

    /// Underlying decision-command this adapter spawns.
    pub fn command(&self) -> &DecisionCommand {
        &self.command
    }
}

fn run_with_timeout(
    program: &Path,
    args: &[String],
    timeout: Duration,
    kind: &'static str,
) -> Result<std::process::Output, LedgerError> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| LedgerError::Spawn {
            binary: program.to_path_buf(),
            source: e,
        })?;

    // Drain stdout/stderr on background threads to avoid pipe
    // backpressure deadlock when the child produces large output.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let stdout_handle = std::thread::spawn(move || {
        stdout_pipe
            .map(|mut r| {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut r, &mut buf).ok();
                buf
            })
            .unwrap_or_default()
    });
    let stderr_handle = std::thread::spawn(move || {
        stderr_pipe
            .map(|mut r| {
                let mut buf = Vec::new();
                std::io::Read::read_to_end(&mut r, &mut buf).ok();
                buf
            })
            .unwrap_or_default()
    });

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout_handle.join().unwrap_or_default();
                let stderr = stderr_handle.join().unwrap_or_default();
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(LedgerError::Timeout { kind, timeout });
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return Err(LedgerError::Spawn {
                    binary: program.to_path_buf(),
                    source: e,
                });
            }
        }
    }
}

impl LedgerAdapter for CliLedgerAdapter {
    fn scan(&self, skill_dir: &Path) -> Result<(), LedgerError> {
        let args = self.command.build_scan_args(skill_dir);
        let output = run_with_timeout(self.command.program(), &args, self.timeout, "scan")?;
        if !output.status.success() {
            return Err(LedgerError::NonZeroExit {
                status: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        Ok(())
    }

    fn resolve(&self, skill_dir: &Path) -> Result<LedgerResolveResult, LedgerError> {
        let args = self.command.build_resolve_args(skill_dir);
        let output = run_with_timeout(self.command.program(), &args, self.timeout, "resolve")?;
        if !output.status.success() {
            return Err(LedgerError::NonZeroExit {
                status: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let stdout = std::str::from_utf8(&output.stdout).map_err(|e| LedgerError::InvalidJson {
            reason: format!("stdout was not valid UTF-8: {e}"),
        })?;
        LedgerResolveResult::from_json_str(stdout)
    }
}

/// In-memory [`LedgerAdapter`] used by tests and by the demo events
/// playback path (`--demo-events`).
///
/// The adapter returns a pre-computed [`LedgerResolveResult`] (or
/// [`LedgerError`]) per skill name lookup. It does **not** spawn a
/// subprocess and can therefore exercise the validator and the
/// downstream active-mapping consumer without a real ledger binary.
///
/// `scan` defaults to `Ok(())` for every skill — tests that exercise the
/// scan failure path can opt in via [`StaticLedgerAdapter::insert_scan_err`].
/// `scan` and `resolve` both consume the registered entry on the first
/// call, mirroring the one-shot pattern the existing resolve tests rely
/// on; an unmatched `resolve` surfaces an [`LedgerError::InvalidField`]
/// while an unmatched `scan` quietly succeeds (the demo refresh
/// controller treats absence of a scan failure as the happy path).
pub struct StaticLedgerAdapter {
    resolve_entries: parking_lot::Mutex<
        std::collections::HashMap<String, Result<LedgerResolveResult, LedgerError>>,
    >,
    scan_entries: parking_lot::Mutex<std::collections::HashMap<String, Result<(), LedgerError>>>,
    call_log: parking_lot::Mutex<Vec<StaticAdapterCall>>,
}

/// One observed adapter call. Tests can use [`StaticLedgerAdapter::calls`]
/// to pin scan-before-resolve ordering without timing-sensitive hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticAdapterCall {
    Scan { skill_name: String },
    Resolve { skill_name: String },
}

impl Default for StaticLedgerAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl StaticLedgerAdapter {
    pub fn new() -> Self {
        Self {
            resolve_entries: parking_lot::Mutex::new(std::collections::HashMap::new()),
            scan_entries: parking_lot::Mutex::new(std::collections::HashMap::new()),
            call_log: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Register a success response keyed by skill name. The skill name is
    /// derived from the final component of the queried `skill_dir`.
    pub fn insert(&self, skill_name: impl Into<String>, result: LedgerResolveResult) {
        self.resolve_entries
            .lock()
            .insert(skill_name.into(), Ok(result));
    }

    /// Register a failure response.
    pub fn insert_err(&self, skill_name: impl Into<String>, error: LedgerError) {
        self.resolve_entries
            .lock()
            .insert(skill_name.into(), Err(error));
    }

    /// Register a `scan` failure for `skill_name`. Without this, every
    /// scan call succeeds with `Ok(())`.
    pub fn insert_scan_err(&self, skill_name: impl Into<String>, error: LedgerError) {
        self.scan_entries
            .lock()
            .insert(skill_name.into(), Err(error));
    }

    /// Snapshot of the ordered call log; tests use this to assert
    /// scan-before-resolve ordering.
    pub fn calls(&self) -> Vec<StaticAdapterCall> {
        self.call_log.lock().clone()
    }
}

impl LedgerAdapter for StaticLedgerAdapter {
    fn scan(&self, skill_dir: &Path) -> Result<(), LedgerError> {
        let key = skill_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.call_log.lock().push(StaticAdapterCall::Scan {
            skill_name: key.clone(),
        });
        let mut guard = self.scan_entries.lock();
        match guard.remove(&key) {
            Some(v) => v,
            None => Ok(()),
        }
    }

    fn resolve(&self, skill_dir: &Path) -> Result<LedgerResolveResult, LedgerError> {
        let key = skill_dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.call_log.lock().push(StaticAdapterCall::Resolve {
            skill_name: key.clone(),
        });
        let mut guard = self.resolve_entries.lock();
        match guard.remove(&key) {
            Some(v) => v,
            None => Err(LedgerError::InvalidField {
                field: "skill_dir",
                reason: format!("no static response registered for '{key}'"),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn valid_current_json() -> &'static str {
        r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "pass",
            "decision": "current",
            "currentVersion": "v000001",
            "trustedVersion": "v000001"
        }"#
    }

    fn valid_fallback_json() -> &'static str {
        r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "deny",
            "decision": "fallback",
            "currentVersion": "v000003",
            "trustedVersion": "v000001",
            "target": ".skill-meta/versions/v000001.snapshot",
            "targetKind": "relative_to_skill_dir",
            "reason": "current version has high-risk findings"
        }"#
    }

    fn valid_hidden_json() -> &'static str {
        r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "none",
            "decision": "hidden",
            "reason": "no certified version yet"
        }"#
    }

    #[test]
    fn status_strings_round_trip() {
        for s in [
            LedgerStatus::None,
            LedgerStatus::Pass,
            LedgerStatus::Warn,
            LedgerStatus::Deny,
            LedgerStatus::Drifted,
            LedgerStatus::Tampered,
        ] {
            assert_eq!(LedgerStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(LedgerStatus::parse("bogus"), None);
    }

    #[test]
    fn decision_strings_round_trip() {
        for d in [
            LedgerDecision::Current,
            LedgerDecision::Fallback,
            LedgerDecision::Hidden,
        ] {
            assert_eq!(LedgerDecision::parse(d.as_str()), Some(d));
        }
        assert_eq!(LedgerDecision::parse("bogus"), None);
    }

    #[test]
    fn target_kind_strings_round_trip() {
        assert_eq!(
            LedgerTargetKind::parse("relative_to_skill_dir"),
            Some(LedgerTargetKind::RelativeToSkillDir)
        );
        assert_eq!(LedgerTargetKind::parse("absolute"), None);
    }

    #[test]
    fn parses_valid_current_response() {
        let r = LedgerResolveResult::from_json_str(valid_current_json()).unwrap();
        assert_eq!(r.schema_version, 1);
        assert_eq!(r.skill_name, "demo-weather");
        assert_eq!(r.status, LedgerStatus::Pass);
        assert_eq!(r.decision, LedgerDecision::Current);
        assert!(r.target.is_none());
        assert!(r.target_kind.is_none());
        assert_eq!(r.current_version.as_deref(), Some("v000001"));
        assert_eq!(r.trusted_version.as_deref(), Some("v000001"));
    }

    #[test]
    fn parses_valid_fallback_response() {
        let r = LedgerResolveResult::from_json_str(valid_fallback_json()).unwrap();
        assert_eq!(r.status, LedgerStatus::Deny);
        assert_eq!(r.decision, LedgerDecision::Fallback);
        assert_eq!(
            r.target.as_deref(),
            Some(Path::new(".skill-meta/versions/v000001.snapshot"))
        );
        assert_eq!(r.target_kind, Some(LedgerTargetKind::RelativeToSkillDir));
        assert_eq!(r.trusted_version.as_deref(), Some("v000001"));
        assert!(r.reason.is_some());
    }

    #[test]
    fn parses_valid_hidden_response() {
        let r = LedgerResolveResult::from_json_str(valid_hidden_json()).unwrap();
        assert_eq!(r.status, LedgerStatus::None);
        assert_eq!(r.decision, LedgerDecision::Hidden);
        assert!(r.target.is_none());
        assert!(r.target_kind.is_none());
        assert!(r.reason.is_some());
    }

    #[test]
    fn rejects_unknown_decision() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "pass",
            "decision": "warmup"
        }"#;
        match LedgerResolveResult::from_json_str(json).unwrap_err() {
            LedgerError::UnknownDecision { got } => assert_eq!(got, "warmup"),
            other => panic!("expected UnknownDecision, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_status() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "exploded",
            "decision": "current"
        }"#;
        match LedgerResolveResult::from_json_str(json).unwrap_err() {
            LedgerError::UnknownStatus { got } => assert_eq!(got, "exploded"),
            other => panic!("expected UnknownStatus, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let json = r#"{
            "schemaVersion": 2,
            "skillName": "demo-weather",
            "status": "pass",
            "decision": "current"
        }"#;
        match LedgerResolveResult::from_json_str(json).unwrap_err() {
            LedgerError::UnsupportedSchemaVersion { got } => assert_eq!(got, "2"),
            other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
        }
    }

    #[test]
    fn rejects_non_numeric_schema_version() {
        let json = r#"{
            "schemaVersion": "1",
            "skillName": "demo-weather",
            "status": "pass",
            "decision": "current"
        }"#;
        // serde rejects the string -> Value::Number type mismatch first;
        // either UnsupportedSchemaVersion or InvalidJson is acceptable so
        // long as the strict validator does not accept the payload.
        let err = LedgerResolveResult::from_json_str(json).unwrap_err();
        assert!(matches!(
            err,
            LedgerError::UnsupportedSchemaVersion { .. } | LedgerError::InvalidJson { .. }
        ));
    }

    #[test]
    fn rejects_missing_required_fields() {
        let missing_skill = r#"{
            "schemaVersion": 1,
            "status": "pass",
            "decision": "current"
        }"#;
        assert!(matches!(
            LedgerResolveResult::from_json_str(missing_skill).unwrap_err(),
            LedgerError::MissingField { field: "skillName" }
        ));

        let missing_status = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "decision": "current"
        }"#;
        assert!(matches!(
            LedgerResolveResult::from_json_str(missing_status).unwrap_err(),
            LedgerError::MissingField { field: "status" }
        ));

        let missing_decision = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "pass"
        }"#;
        assert!(matches!(
            LedgerResolveResult::from_json_str(missing_decision).unwrap_err(),
            LedgerError::MissingField { field: "decision" }
        ));
    }

    #[test]
    fn fallback_requires_target_and_target_kind() {
        let no_target = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "deny",
            "decision": "fallback",
            "targetKind": "relative_to_skill_dir"
        }"#;
        match LedgerResolveResult::from_json_str(no_target).unwrap_err() {
            LedgerError::MissingField { field } => {
                assert!(field.starts_with("target "));
            }
            other => panic!("expected MissingField, got {other:?}"),
        }

        let no_kind = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "deny",
            "decision": "fallback",
            "target": ".skill-meta/versions/v000001.snapshot"
        }"#;
        match LedgerResolveResult::from_json_str(no_kind).unwrap_err() {
            LedgerError::MissingField { field } => {
                assert!(field.starts_with("targetKind"));
            }
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn fallback_target_must_be_under_skill_meta_versions() {
        // Bare prefix is rejected — no snapshot component.
        for bad in [
            ".skill-meta/versions",
            "/abs/path",
            ".skill-meta/other/v1",
            "scripts/v000001.snapshot",
            "../escape/v000001.snapshot",
            ".skill-meta/versions/../../etc/passwd",
            ".skill-meta/versions/./.",
            "",
        ] {
            let json = format!(
                r#"{{
                    "schemaVersion": 1,
                    "skillName": "demo-weather",
                    "status": "deny",
                    "decision": "fallback",
                    "target": "{}",
                    "targetKind": "relative_to_skill_dir"
                }}"#,
                bad
            );
            let err = LedgerResolveResult::from_json_str(&json).unwrap_err();
            assert!(
                matches!(
                    err,
                    LedgerError::InvalidField {
                        field: "target",
                        ..
                    }
                ),
                "expected InvalidField(target) for {bad:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn fallback_target_kind_must_be_relative_to_skill_dir() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "deny",
            "decision": "fallback",
            "target": ".skill-meta/versions/v000001.snapshot",
            "targetKind": "absolute"
        }"#;
        match LedgerResolveResult::from_json_str(json).unwrap_err() {
            LedgerError::InvalidField {
                field: "targetKind",
                reason,
            } => {
                assert!(reason.contains("relative_to_skill_dir"));
            }
            other => panic!("expected InvalidField(targetKind), got {other:?}"),
        }
    }

    #[test]
    fn current_decision_ignores_target_field() {
        // Even if the ledger sends a target alongside `current`, D1.0 is
        // happy to drop it on the floor; the validator only enforces
        // target presence/shape when decision == fallback.
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "pass",
            "decision": "current",
            "target": ".skill-meta/versions/v000001.snapshot",
            "targetKind": "relative_to_skill_dir"
        }"#;
        let r = LedgerResolveResult::from_json_str(json).unwrap();
        assert_eq!(r.decision, LedgerDecision::Current);
        assert!(r.target.is_none());
        assert!(r.target_kind.is_none());
    }

    #[test]
    fn hidden_decision_does_not_require_reason() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "none",
            "decision": "hidden"
        }"#;
        let r = LedgerResolveResult::from_json_str(json).unwrap();
        assert_eq!(r.decision, LedgerDecision::Hidden);
        assert!(r.reason.is_none());
    }

    #[test]
    fn rejects_invalid_json_bytes() {
        let err = LedgerResolveResult::from_json_str("not json at all").unwrap_err();
        assert!(matches!(err, LedgerError::InvalidJson { .. }));
    }

    #[test]
    fn empty_skill_name_is_rejected() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "",
            "status": "pass",
            "decision": "current"
        }"#;
        match LedgerResolveResult::from_json_str(json).unwrap_err() {
            LedgerError::InvalidField {
                field: "skillName", ..
            } => {}
            other => panic!("expected InvalidField(skillName), got {other:?}"),
        }
    }

    fn skill_name_payload(name: &str) -> String {
        // Build the payload via serde_json so quotes / escapes / NUL in
        // the test names land in the JSON safely (a raw format! would
        // mangle them).
        serde_json::json!({
            "schemaVersion": 1,
            "skillName": name,
            "status": "pass",
            "decision": "current",
        })
        .to_string()
    }

    #[test]
    fn skill_name_accepts_single_path_components() {
        // Safety boundary only — naming policy (kebab-case) is the
        // parser's job. Anything that is a single path component with
        // no traversal / no NUL / under the length cap must be
        // accepted by the ledger surface.
        let max_len_name = "a".repeat(MAX_SKILL_NAME_LEN);
        let accepted: [&str; 6] = [
            "demo-weather",
            "Alpha",
            "alpha_1",
            "alpha.beta",
            "a",
            // Exactly at the length cap is still a single component.
            &max_len_name,
        ];
        for name in accepted {
            let json = skill_name_payload(name);
            let r = LedgerResolveResult::from_json_str(&json).expect("expected acceptance");
            assert_eq!(r.skill_name, name, "round-trip mismatch for {name:?}");
        }
    }

    #[test]
    fn skill_name_rejects_traversal_and_separators() {
        // These are the inputs that would let a hostile or buggy
        // ledger steer `source_root.join(skill_name)` out of the
        // intended Skill directory. Each must produce
        // `InvalidField(skillName)` so the resolver never sees them.
        let rejected = [
            "",
            ".",
            "..",
            "../evil",
            "alpha/beta",
            "/tmp/evil",
            "alpha\\beta",
            "alpha\0beta",
        ];
        for name in rejected {
            let json = skill_name_payload(name);
            let err = LedgerResolveResult::from_json_str(&json)
                .err()
                .unwrap_or_else(|| panic!("expected error for {name:?}, got Ok"));
            match err {
                LedgerError::InvalidField {
                    field: "skillName", ..
                } => {}
                other => {
                    panic!("expected InvalidField(skillName) for {name:?}, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn skill_name_rejects_overlong_names() {
        let too_long = "a".repeat(MAX_SKILL_NAME_LEN + 1);
        let json = skill_name_payload(&too_long);
        let err = LedgerResolveResult::from_json_str(&json).unwrap_err();
        match err {
            LedgerError::InvalidField {
                field: "skillName",
                reason,
            } => {
                assert!(
                    reason.contains(&format!("{MAX_SKILL_NAME_LEN}")),
                    "length cap should be reported, got: {reason}"
                );
            }
            other => panic!("expected InvalidField(skillName), got {other:?}"),
        }
    }

    #[test]
    fn parses_declared_name_when_present() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "weather",
            "declaredName": "calculator",
            "status": "deny",
            "decision": "hidden"
        }"#;
        let r = LedgerResolveResult::from_json_str(json).unwrap();
        assert_eq!(r.skill_name, "weather");
        assert_eq!(r.declared_name.as_deref(), Some("calculator"));
        assert_eq!(r.decision, LedgerDecision::Hidden);
    }

    #[test]
    fn declared_name_is_optional_for_backwards_compat() {
        // Pre-N1 payloads with no declaredName must keep parsing.
        let r = LedgerResolveResult::from_json_str(valid_current_json()).unwrap();
        assert!(r.declared_name.is_none());
    }

    #[test]
    fn validate_for_expected_skill_accepts_matching_name() {
        let r = LedgerResolveResult::from_json_str(valid_current_json()).unwrap();
        r.validate_for_expected_skill("demo-weather")
            .expect("matching name must validate");
    }

    #[test]
    fn validate_for_expected_skill_rejects_mismatch() {
        let r = LedgerResolveResult::from_json_str(valid_current_json()).unwrap();
        let err = r.validate_for_expected_skill("calculator").unwrap_err();
        match err {
            LedgerError::SkillNameMismatch { expected, actual } => {
                assert_eq!(expected, "calculator");
                assert_eq!(actual, "demo-weather");
            }
            other => panic!("expected SkillNameMismatch, got {other:?}"),
        }
    }

    #[test]
    fn skill_name_mismatch_display_includes_expected_and_actual() {
        let err = LedgerError::SkillNameMismatch {
            expected: "weather".to_string(),
            actual: "calculator".to_string(),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("weather"));
        assert!(rendered.contains("calculator"));
        assert!(
            rendered.contains("mismatch"),
            "display should mention mismatch, got {rendered:?}"
        );
    }

    #[test]
    fn declared_name_does_not_change_skill_name_match() {
        // skillName=weather declaredName=calculator: validation against
        // "weather" must pass; validation against "calculator" must fail
        // because declared_name is metadata only.
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "weather",
            "declaredName": "calculator",
            "status": "deny",
            "decision": "hidden"
        }"#;
        let r = LedgerResolveResult::from_json_str(json).unwrap();
        r.validate_for_expected_skill("weather").unwrap();
        let err = r.validate_for_expected_skill("calculator").unwrap_err();
        assert!(matches!(err, LedgerError::SkillNameMismatch { .. }));
    }

    #[test]
    fn tolerates_unknown_fields_for_forward_compat() {
        let json = r#"{
            "schemaVersion": 1,
            "skillName": "demo-weather",
            "status": "pass",
            "decision": "current",
            "futureField": "ignored"
        }"#;
        let r = LedgerResolveResult::from_json_str(json).unwrap();
        assert_eq!(r.decision, LedgerDecision::Current);
    }

    #[test]
    fn decision_command_parses_single_binary() {
        let cmd = DecisionCommand::parse("/usr/local/bin/xxx-cli").unwrap();
        assert_eq!(cmd.program(), Path::new("/usr/local/bin/xxx-cli"));
        assert!(cmd.fixed_args().is_empty());
    }

    #[test]
    fn decision_command_parses_whitespace_split_prefix() {
        let cmd = DecisionCommand::parse("agent-sec-cli skill-ledger").unwrap();
        assert_eq!(cmd.program(), Path::new("agent-sec-cli"));
        assert_eq!(cmd.fixed_args(), &["skill-ledger".to_string()]);
    }

    #[test]
    fn decision_command_collapses_repeated_whitespace() {
        let cmd = DecisionCommand::parse("  agent-sec-cli\t skill-ledger  ").unwrap();
        assert_eq!(cmd.program(), Path::new("agent-sec-cli"));
        assert_eq!(cmd.fixed_args(), &["skill-ledger".to_string()]);
    }

    #[test]
    fn decision_command_rejects_empty_input() {
        for raw in ["", " ", "\t", "   \n"] {
            let err = DecisionCommand::parse(raw).unwrap_err();
            match err {
                LedgerError::InvalidField {
                    field: "decision-command",
                    ..
                } => {}
                other => {
                    panic!("expected InvalidField(decision-command) for {raw:?}, got {other:?}")
                }
            }
        }
    }

    #[test]
    fn decision_command_scan_argv_pins_contract() {
        let cmd = DecisionCommand::parse("agent-sec-cli skill-ledger").unwrap();
        assert_eq!(
            cmd.build_scan_args(Path::new("/srv/skills/demo-weather")),
            vec![
                "skill-ledger".to_string(),
                "scan".to_string(),
                "/srv/skills/demo-weather".to_string(),
                "--json".to_string(),
            ]
        );
        let single = DecisionCommand::parse("/usr/local/bin/xxx-cli").unwrap();
        assert_eq!(
            single.build_scan_args(Path::new("/srv/skills/demo-weather")),
            vec![
                "scan".to_string(),
                "/srv/skills/demo-weather".to_string(),
                "--json".to_string(),
            ]
        );
    }

    #[test]
    fn decision_command_resolve_argv_pins_contract() {
        let cmd = DecisionCommand::parse("agent-sec-cli skill-ledger").unwrap();
        assert_eq!(
            cmd.build_resolve_args(Path::new("/srv/skills/demo-weather")),
            vec![
                "skill-ledger".to_string(),
                "resolve".to_string(),
                "/srv/skills/demo-weather".to_string(),
                "--json".to_string(),
            ]
        );
        let single = DecisionCommand::parse("/usr/local/bin/xxx-cli").unwrap();
        assert_eq!(
            single.build_resolve_args(Path::new("/srv/skills/demo-weather")),
            vec![
                "resolve".to_string(),
                "/srv/skills/demo-weather".to_string(),
                "--json".to_string(),
            ]
        );
    }

    #[test]
    fn cli_adapter_scan_times_out_on_hanging_subprocess() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hang.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cmd = DecisionCommand::parse(&format!("/bin/sh {}", script.display())).unwrap();
        let adapter = CliLedgerAdapter::with_timeout(cmd, Duration::from_millis(200));
        let err = adapter
            .scan(Path::new("/srv/skills/demo-weather"))
            .unwrap_err();
        match err {
            LedgerError::Timeout { kind, .. } => assert_eq!(kind, "scan"),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn cli_adapter_resolve_times_out_on_hanging_subprocess() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("hang.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cmd = DecisionCommand::parse(&format!("/bin/sh {}", script.display())).unwrap();
        let adapter = CliLedgerAdapter::with_timeout(cmd, Duration::from_millis(200));
        let err = adapter
            .resolve(Path::new("/srv/skills/demo-weather"))
            .unwrap_err();
        match err {
            LedgerError::Timeout { kind, .. } => assert_eq!(kind, "resolve"),
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn cli_adapter_timeout_display_includes_kind_and_duration() {
        let err = LedgerError::Timeout {
            kind: "scan",
            timeout: Duration::from_secs(10),
        };
        let s = err.to_string();
        assert!(s.contains("scan"), "display should mention kind");
        assert!(s.contains("10"), "display should mention timeout");
    }

    #[test]
    fn cli_adapter_surfaces_spawn_failure_for_missing_binary() {
        // Use a path that cannot exist as an executable. The adapter
        // returns LedgerError::Spawn rather than panicking or hanging
        // for both scan and resolve.
        let cmd = DecisionCommand::parse("/nonexistent/decision-binary").unwrap();
        let adapter = CliLedgerAdapter::new(cmd);
        let scan_err = adapter
            .scan(Path::new("/srv/skills/demo-weather"))
            .unwrap_err();
        assert!(
            matches!(scan_err, LedgerError::Spawn { .. }),
            "expected Spawn from scan, got {scan_err:?}"
        );
        let resolve_err = adapter
            .resolve(Path::new("/srv/skills/demo-weather"))
            .unwrap_err();
        assert!(
            matches!(resolve_err, LedgerError::Spawn { .. }),
            "expected Spawn from resolve, got {resolve_err:?}"
        );
    }

    #[test]
    fn static_adapter_replays_registered_responses() {
        let adapter = StaticLedgerAdapter::new();
        let parsed = LedgerResolveResult::from_json_str(valid_current_json()).unwrap();
        adapter.insert("demo-weather", parsed.clone());
        let got = adapter
            .resolve(Path::new("/srv/skills/demo-weather"))
            .unwrap();
        assert_eq!(got, parsed);
    }

    #[test]
    fn static_adapter_surfaces_registered_error() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_err(
            "demo-weather",
            LedgerError::NonZeroExit {
                status: 7,
                stdout: String::new(),
                stderr: "boom".to_string(),
            },
        );
        let err = adapter
            .resolve(Path::new("/srv/skills/demo-weather"))
            .unwrap_err();
        assert!(matches!(err, LedgerError::NonZeroExit { status: 7, .. }));
    }

    #[test]
    fn static_adapter_scan_defaults_to_success() {
        let adapter = StaticLedgerAdapter::new();
        adapter
            .scan(Path::new("/srv/skills/demo-weather"))
            .expect("default scan must succeed");
        assert_eq!(
            adapter.calls(),
            vec![StaticAdapterCall::Scan {
                skill_name: "demo-weather".to_string()
            }]
        );
    }

    #[test]
    fn static_adapter_scan_surfaces_registered_failure_once() {
        let adapter = StaticLedgerAdapter::new();
        adapter.insert_scan_err(
            "demo-weather",
            LedgerError::NonZeroExit {
                status: 9,
                stdout: String::new(),
                stderr: "scan blew up".to_string(),
            },
        );
        let err = adapter
            .scan(Path::new("/srv/skills/demo-weather"))
            .unwrap_err();
        assert!(matches!(err, LedgerError::NonZeroExit { status: 9, .. }));
        // One-shot: a follow-up scan defaults back to Ok(()).
        adapter
            .scan(Path::new("/srv/skills/demo-weather"))
            .expect("scan recovers after the failure is consumed");
    }
}
