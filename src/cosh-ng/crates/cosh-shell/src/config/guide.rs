use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::private_state;
mod claim_policy;
pub(crate) mod cli;
mod context;
mod episode_claim;
mod health_exposure;
pub(crate) use context::{load_failure_context, valid_context_id};
mod runtime;
pub(crate) mod wizard;
pub(crate) use cli::{run_cli, UserPaths};
pub(crate) use runtime::{RuntimeSnapshot, RuntimeState};
pub(crate) use wizard::{terminal_capable, wants_wizard};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
const WEEK_MS: u64 = 7 * DAY_MS;
const MAX_BYTES: usize = 128 * 1024;
const MAX_CLAIMS: usize = 256;

#[derive(Clone)]
pub(crate) struct HealthExposure<'a> {
    pub(crate) evidence: &'a str,
    pub(crate) event: Option<(&'a str, &'a str)>,
    pub(crate) severity: Option<u8>,
    pub(crate) legacy_problems: &'a [String],
    pub(crate) episode: Option<crate::diagnostics::health::suppression::episode::CandidateEpisode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Error {
    State(private_state::Error),
    Corrupt,
    UnsupportedVersion,
    InvalidRequest,
    RuntimeUnavailable,
    Capacity,
}

impl From<private_state::Error> for Error {
    fn from(error: private_state::Error) -> Self {
        Self::State(error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Preferences {
    version: u32,
    pub(crate) guide_enabled: bool,
    pub(crate) banner_enabled: bool,
    pub(crate) generation: String,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: 1,
            guide_enabled: true,
            banner_enabled: true,
            generation: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GuideSnapshot {
    runtime_generation: bool,
    pub(crate) preferences: Preferences,
    pub(crate) generation: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetGuideOutcome {
    pub(crate) preferences: Preferences,
    pub(crate) runtime: Option<RuntimeSnapshot>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetGuideError {
    Runtime {
        disabled_published: bool,
        error: Error,
    },
    Persistence {
        runtime: Option<RuntimeSnapshot>,
        error: Error,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum InviteKind {
    LoginEntry,
    LoginHealth,
    Failure,
    Help,
    DefaultLogin,
}

pub(crate) struct ClaimRequest<'a> {
    pub(crate) kind: InviteKind,
    pub(crate) host: &'a str,
    pub(crate) problem: &'a str,
    pub(crate) session: &'a str,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Claim {
    runtime_generation: bool,
    pub(crate) generation: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClaimDecision {
    Claimed(Claim),
    Disabled,
    Suppressed,
    Stale,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claims {
    version: u32,
    entries: Vec<ClaimEntry>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClaimEntry {
    kind: InviteKind,
    host: String,
    problem: String,
    scope: String,
    shown_at_ms: u64,
}

pub(crate) fn open_user_store() -> Result<GuideStore, Error> {
    open_user_store_at(super::load::copilot_shell_cosh_dir().as_deref())
}

fn open_user_store_at(cosh_dir: Option<&Path>) -> Result<GuideStore, Error> {
    let cosh_dir = cosh_dir.ok_or(Error::State(private_state::Error::Io(
        std::io::ErrorKind::NotFound,
    )))?;
    GuideStore::open(cosh_dir.join("guide"))
}

pub(crate) fn open_user_state() -> Result<(GuideStore, RuntimeState), Error> {
    let cosh_dir = super::load::copilot_shell_cosh_dir();
    let runtime_base = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    open_user_state_at(cosh_dir.as_deref(), runtime_base.as_deref())
}

fn open_user_state_at(
    cosh_dir: Option<&Path>,
    runtime_base: Option<&Path>,
) -> Result<(GuideStore, RuntimeState), Error> {
    let cosh_dir = cosh_dir.ok_or(Error::State(private_state::Error::Io(
        std::io::ErrorKind::NotFound,
    )))?;
    let runtime = RuntimeState::open(runtime_base.ok_or(Error::RuntimeUnavailable)?)?;
    let store = open_user_store_at(Some(cosh_dir))?;
    Ok((store, runtime))
}

pub(crate) fn open_optional_user_state() -> Result<
    (
        crate::config::guide::GuideStore,
        Option<crate::config::guide::RuntimeState>,
    ),
    crate::config::guide::Error,
> {
    match crate::config::guide::open_user_state() {
        Ok((store, runtime)) => {
            let available = runtime.read()?.is_some();
            Ok((store, available.then_some(runtime)))
        }
        Err(_) => crate::config::guide::open_user_store().map(|store| (store, None)),
    }
}

pub(crate) struct GuideStore {
    root: PathBuf,
}

impl GuideStore {
    pub(crate) fn open(root: impl AsRef<Path>) -> Result<Self, Error> {
        private_state::ensure_directory(root.as_ref())?;
        Ok(Self {
            root: root.as_ref().to_path_buf(),
        })
    }

    /// Opens an existing store without creating directories; `None` when absent.
    pub(crate) fn open_existing(root: impl AsRef<Path>) -> Result<Option<Self>, Error> {
        let root = root.as_ref();
        if !root.exists() {
            return Ok(None);
        }
        private_state::validate_directory(root)?;
        Ok(Some(Self {
            root: root.to_path_buf(),
        }))
    }

    fn lock(&self) -> Result<private_state::LockGuard, Error> {
        Ok(private_state::LockGuard::try_acquire(
            &self.root.join("state.lock"),
        )?)
    }

    pub(crate) fn load_preferences(&self) -> Result<Option<Preferences>, Error> {
        let _lock = self.lock()?;
        self.read_preferences()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    /// Reads preferences without the write lock so status stays read-only.
    pub(crate) fn peek_preferences(&self) -> Result<Option<Preferences>, Error> {
        self.read_preferences()
    }

    fn read_preferences(&self) -> Result<Option<Preferences>, Error> {
        let Some(bytes) = private_state::read_bounded(&self.root.join("preferences.json"), 4096)?
        else {
            return Ok(None);
        };
        let preferences: Preferences =
            serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        if preferences.version != 1 {
            return Err(Error::UnsupportedVersion);
        }
        if !preferences.generation.is_empty() && !runtime::valid_generation(&preferences.generation)
        {
            return Err(Error::Corrupt);
        }
        Ok(Some(preferences))
    }

    fn write_preferences(&self, preferences: &Preferences) -> Result<(), Error> {
        let bytes = serde_json::to_vec(preferences).map_err(|_| Error::Corrupt)?;
        Ok(private_state::atomic_write(
            &self.root.join("preferences.json"),
            &bytes,
            4096,
        )?)
    }

    pub(crate) fn initialize_runtime(&self, runtime: &RuntimeState) -> Result<(), Error> {
        let preferences = self.load_preferences()?.unwrap_or_default();
        runtime.initialize(preferences.guide_enabled)
    }

    pub(crate) fn snapshot(&self, runtime: Option<&RuntimeState>) -> Result<GuideSnapshot, Error> {
        let _lock = self.lock()?;
        let preferences = self.read_preferences()?.unwrap_or_default();
        let runtime_generation = match runtime {
            Some(runtime) => Some(runtime.read()?.ok_or(Error::RuntimeUnavailable)?.generation),
            None => None,
        };
        let generation = generation_token(&preferences.generation, runtime_generation.as_deref());
        Ok(GuideSnapshot {
            preferences,
            generation,
            runtime_generation: runtime.is_some(),
        })
    }

    pub(crate) fn set_banner(&self, enabled: bool) -> Result<Preferences, Error> {
        let _lock = self.lock()?;
        let mut preferences = self.read_preferences()?.unwrap_or_default();
        preferences.banner_enabled = enabled;
        self.write_preferences(&preferences)?;
        Ok(preferences)
    }

    pub(crate) fn set_guide(
        &self,
        runtime: Option<&RuntimeState>,
        enabled: bool,
    ) -> Result<SetGuideOutcome, SetGuideError> {
        let (published, _runtime_lock) = match runtime {
            Some(runtime) => {
                let (state, lock) =
                    runtime
                        .publish(enabled)
                        .map_err(|error| SetGuideError::Runtime {
                            disabled_published: runtime.disabled().unwrap_or(false),
                            error,
                        })?;
                (Some(state), Some(lock))
            }
            None => (None, None),
        };
        let persist = || {
            let _lock = self.lock()?;
            let mut preferences = self.read_preferences()?.unwrap_or_default();
            preferences.guide_enabled = enabled;
            preferences.generation = published
                .as_ref()
                .map(|state| state.generation.clone())
                .unwrap_or_else(|| uuid::Uuid::new_v4().simple().to_string());
            self.write_preferences(&preferences)?;
            Ok(preferences)
        };
        let preferences = persist().map_err(|error| SetGuideError::Persistence {
            runtime: published.clone(),
            error,
        })?;
        Ok(SetGuideOutcome {
            preferences,
            runtime: published,
        })
    }

    pub(crate) fn claim(
        &self,
        runtime: Option<&RuntimeState>,
        snapshot: &GuideSnapshot,
        request: ClaimRequest<'_>,
        now_ms: u64,
    ) -> Result<ClaimDecision, Error> {
        self.claim_prepared(runtime, snapshot, request, now_ms, None, || Ok(()))
    }

    #[cfg(test)]
    pub(crate) fn claim_login_health(
        &self,
        runtime: Option<&RuntimeState>,
        snapshot: &GuideSnapshot,
        request: ClaimRequest<'_>,
        now_ms: u64,
        evidence: &str,
    ) -> Result<ClaimDecision, Error> {
        self.claim_health_version(
            runtime,
            snapshot,
            request,
            now_ms,
            HealthExposure {
                evidence,
                event: Some(("fixture-v1", evidence)),
                severity: None,
                legacy_problems: &[],
                episode: None,
            },
        )
    }

    pub(crate) fn claim_health_version(
        &self,
        runtime: Option<&RuntimeState>,
        snapshot: &GuideSnapshot,
        request: ClaimRequest<'_>,
        now_ms: u64,
        version: HealthExposure<'_>,
    ) -> Result<ClaimDecision, Error> {
        self.claim_prepared(runtime, snapshot, request, now_ms, Some(version), || Ok(()))
    }

    fn claim_prepared(
        &self,
        runtime: Option<&RuntimeState>,
        snapshot: &GuideSnapshot,
        request: ClaimRequest<'_>,
        now_ms: u64,
        health_evidence: Option<HealthExposure<'_>>,
        prepare: impl FnOnce() -> Result<(), Error>,
    ) -> Result<ClaimDecision, Error> {
        validate_request(&request)?;
        if snapshot.runtime_generation != runtime.is_some() {
            return Ok(ClaimDecision::Stale);
        }
        // Disabled guidance must not depend on the quota lock becoming available.
        if !self.read_preferences()?.unwrap_or_default().guide_enabled {
            return Ok(ClaimDecision::Disabled);
        }
        let _lock = self.lock()?;
        let preferences = self.read_preferences()?.unwrap_or_default();
        if !preferences.guide_enabled {
            return Ok(ClaimDecision::Disabled);
        }
        let runtime_generation = match runtime {
            Some(runtime) => {
                let state = runtime.read()?.ok_or(Error::RuntimeUnavailable)?;
                if !state.enabled {
                    return Ok(ClaimDecision::Disabled);
                }
                Some(state.generation)
            }
            None => None,
        };
        let generation = generation_token(&preferences.generation, runtime_generation.as_deref());
        if generation != snapshot.generation {
            return Ok(ClaimDecision::Stale);
        }
        let mut claims = self.read_claims()?;
        let host = digest(&[request.host]);
        let problem = digest(&[request.host, request.problem]);
        let scope = digest(&[request.host, request.session]);
        let exposures = if request.kind == InviteKind::LoginHealth && health_evidence.is_some() {
            match health_exposure::read(&self.root) {
                Ok(exposures) => Some(exposures),
                Err(_) => return Ok(ClaimDecision::Suppressed),
            }
        } else {
            None
        };
        if let Some(version) = health_evidence.as_ref() {
            if version
                .event
                .is_some_and(|(domain, _)| !health_exposure::valid_event_domain(domain))
            {
                return Err(Error::InvalidRequest);
            }
        }
        let episode_candidate = health_evidence
            .as_ref()
            .and_then(|version| version.episode.as_ref());
        let episode_claim = match episode_claim::prepare(episode_candidate, now_ms) {
            Ok(claim) => claim,
            Err(()) => return Ok(ClaimDecision::Suppressed),
        };
        if episode_candidate.is_none() {
            if let Some(version) = health_evidence.as_ref() {
                let legacy: Vec<_> = version
                    .legacy_problems
                    .iter()
                    .map(|problem| digest(&[request.host, problem]))
                    .collect();
                if claims.entries.iter().any(|entry| {
                    entry.host == host
                        && matches!(entry.kind, InviteKind::LoginHealth | InviteKind::Failure)
                        && now_ms.saturating_sub(entry.shown_at_ms) < DAY_MS
                        && legacy.contains(&entry.problem)
                        && !claim_policy::new_health_event(
                            exposures.as_ref(),
                            health_evidence.as_ref(),
                            entry,
                        )
                }) {
                    return Ok(ClaimDecision::Suppressed);
                }
            }
            if claims.entries.iter().any(|entry| {
                suppresses(
                    entry,
                    &request,
                    &host,
                    &problem,
                    &scope,
                    now_ms,
                    exposures.as_ref(),
                    health_evidence.as_ref(),
                )
            }) {
                return Ok(ClaimDecision::Suppressed);
            }
        }
        claims
            .entries
            .retain(|entry| now_ms.saturating_sub(entry.shown_at_ms) < claim_ttl(entry.kind));
        if claims.entries.len() >= MAX_CLAIMS {
            return Err(Error::Capacity);
        }
        // Save the referenced context before committing suppression under the same lock.
        prepare()?;
        let stored_problem = health_evidence
            .as_ref()
            .filter(|_| request.kind == InviteKind::LoginHealth)
            .and_then(|version| version.legacy_problems.first())
            .map(|problem| digest(&[request.host, problem]))
            .unwrap_or_else(|| problem.clone());
        claims.entries.push(ClaimEntry {
            kind: request.kind,
            host,
            problem: stored_problem.clone(),
            scope,
            shown_at_ms: now_ms,
        });
        let bytes = serde_json::to_vec(&claims).map_err(|_| Error::Corrupt)?;
        private_state::atomic_write(&self.root.join("claims.json"), &bytes, MAX_BYTES)?;
        if request.kind == InviteKind::LoginHealth {
            if let Some(evidence) = health_evidence.as_ref() {
                let mut exposures = exposures.unwrap_or_default();
                exposures
                    .entries
                    .retain(|entry| now_ms.saturating_sub(entry.shown_at_ms) < DAY_MS);
                exposures
                    .entries
                    .push(health_exposure::HealthExposureEntry {
                        problem: stored_problem,
                        shown_at_ms: now_ms,
                        evidence: digest(&[evidence.evidence]),
                        identity: claims.entries.last().map(claim_identity),
                        event: evidence.event.map(|(domain, identity)| {
                            health_exposure::ExposureEvent {
                                domain: domain.to_string(),
                                identity: digest(&[identity]),
                            }
                        }),
                        severity: evidence.severity,
                    });
                if exposures.entries.len() > MAX_CLAIMS
                    || health_exposure::write(&self.root, &exposures).is_err()
                {
                    return Ok(ClaimDecision::Suppressed);
                }
            }
        }
        // Record the episode only once the card is durably claimed; a failed
        // claim must not consume the cooldown of still-undelivered members.
        if let Some(claim) = episode_claim {
            if !episode_claim::commit(claim, now_ms) {
                return Ok(ClaimDecision::Suppressed);
            }
        }
        Ok(ClaimDecision::Claimed(Claim {
            runtime_generation: snapshot.runtime_generation,
            generation,
        }))
    }

    pub(crate) fn validate_claim(
        &self,
        runtime: Option<&RuntimeState>,
        claim: &Claim,
    ) -> Result<bool, Error> {
        self.guidance_generation_matches(runtime, claim.runtime_generation, &claim.generation)
    }

    /// Reads the same effective guidance state as a claim check without spending an invitation.
    pub(crate) fn guidance_still_enabled(
        &self,
        runtime: Option<&RuntimeState>,
        snapshot: &GuideSnapshot,
    ) -> Result<bool, Error> {
        self.guidance_generation_matches(runtime, snapshot.runtime_generation, &snapshot.generation)
    }

    fn guidance_generation_matches(
        &self,
        runtime: Option<&RuntimeState>,
        expected_runtime: bool,
        expected_generation: &str,
    ) -> Result<bool, Error> {
        if expected_runtime != runtime.is_some() {
            return Err(Error::RuntimeUnavailable);
        }
        let _lock = self.lock()?;
        let preferences = self.read_preferences()?.unwrap_or_default();
        if !preferences.guide_enabled {
            return Ok(false);
        }
        let runtime_generation = match runtime {
            Some(runtime) => {
                let state = runtime.read()?.ok_or(Error::RuntimeUnavailable)?;
                if !state.enabled {
                    return Ok(false);
                }
                Some(state.generation)
            }
            None => None,
        };
        let generation = generation_token(&preferences.generation, runtime_generation.as_deref());
        Ok(generation == expected_generation)
    }

    fn read_claims(&self) -> Result<Claims, Error> {
        let Some(bytes) = private_state::read_bounded(&self.root.join("claims.json"), MAX_BYTES)?
        else {
            return Ok(Claims {
                version: 1,
                entries: Vec::new(),
            });
        };
        let claims: Claims = serde_json::from_slice(&bytes).map_err(|_| Error::Corrupt)?;
        if claims.version != 1 {
            return Err(Error::UnsupportedVersion);
        }
        if claims.entries.len() > MAX_CLAIMS
            || claims.entries.iter().any(|entry| {
                [&entry.host, &entry.problem, &entry.scope]
                    .iter()
                    .any(|value| !valid_digest(value))
            })
        {
            return Err(Error::Corrupt);
        }
        Ok(claims)
    }
}

fn validate_request(request: &ClaimRequest<'_>) -> Result<(), Error> {
    if request.host.is_empty()
        || [request.host, request.problem, request.session]
            .iter()
            .any(|value| value.len() > 4096)
        || (matches!(
            request.kind,
            InviteKind::LoginHealth | InviteKind::Failure | InviteKind::Help
        ) && request.problem.is_empty())
        || (matches!(request.kind, InviteKind::LoginHealth | InviteKind::Failure)
            && request.session.is_empty())
    {
        return Err(Error::InvalidRequest);
    }
    Ok(())
}

// Session owners keep shown bits beyond these bounded deduplication windows.
fn claim_ttl(kind: InviteKind) -> u64 {
    match kind {
        InviteKind::LoginEntry | InviteKind::DefaultLogin => WEEK_MS,
        InviteKind::LoginHealth | InviteKind::Failure => DAY_MS,
        InviteKind::Help => 30_000,
    }
}

fn suppresses(
    entry: &ClaimEntry,
    request: &ClaimRequest<'_>,
    host: &str,
    problem: &str,
    scope: &str,
    now_ms: u64,
    exposures: Option<&health_exposure::HealthExposures>,
    evidence: Option<&HealthExposure<'_>>,
) -> bool {
    if entry.host != host {
        return false;
    }
    let age = now_ms.saturating_sub(entry.shown_at_ms);
    match request.kind {
        InviteKind::LoginEntry | InviteKind::DefaultLogin => {
            entry.kind == request.kind && age < WEEK_MS
        }
        InviteKind::LoginHealth | InviteKind::Failure => {
            (entry.kind == request.kind && entry.scope == scope && age < DAY_MS)
                || (matches!(entry.kind, InviteKind::LoginHealth | InviteKind::Failure)
                    && entry.problem == problem
                    && age < DAY_MS
                    && !claim_policy::new_health_event(exposures, evidence, entry))
        }
        InviteKind::Help => {
            entry.kind == InviteKind::Help && entry.problem == problem && age < 30_000
        }
    }
}

fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(unsafe { nix::libc::geteuid() }.to_le_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn claim_identity(entry: &ClaimEntry) -> String {
    digest(&[
        &format!("{:?}", entry.kind),
        &entry.host,
        &entry.problem,
        &entry.scope,
        &entry.shown_at_ms.to_string(),
    ])
}

fn generation_token(persistent: &str, runtime: Option<&str>) -> String {
    digest(&["guide-generation", persistent, runtime.unwrap_or("")])
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
library_tests! {
    mod health_version_tests;
}
#[cfg(test)]
mod tests;
