use std::str::FromStr;

use asc_credential_store::{AgentName, CredentialId, CredentialSpec};

use crate::PlanError;
use crate::plan::{
    CredentialDefault, CredentialMatch, CredentialRef, DefaultEffect, Effect, MethodMatch, Phase,
    Plan, PlanFormat, PlanSource, RequestMatch, Rule, Scheme, Subject,
};
use crate::template::{PresetTemplate, parse_template};

/// Embedded preset source; the JSON lives in `presets/github.json`.
const GITHUB_SOURCE: &str = include_str!("../presets/github.json");

/// Built-in credential presets; users cannot define their own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Preset {
    /// GitHub REST API and Smart HTTP Git with a personal access or app token.
    Github,
}

impl Preset {
    /// User-facing preset name.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.template().name.as_str()
    }

    /// Internal version; a change in rules ships as a new version.
    #[must_use]
    pub fn version(self) -> u32 {
        self.template().version
    }

    /// Credential Store entry the preset manages, with its carriers and fake shape.
    #[must_use]
    pub fn credential_spec(self) -> CredentialSpec {
        self.template().credential_spec.clone()
    }

    fn template(self) -> &'static PresetTemplate {
        match self {
            Self::Github => github_template(),
        }
    }
}

/// Lazily parsed and validated github template; panics at first access if the
/// embedded JSON is malformed (a build-time programming error).
fn github_template() -> &'static PresetTemplate {
    use std::sync::OnceLock;
    static TEMPLATE: OnceLock<PresetTemplate> = OnceLock::new();
    TEMPLATE.get_or_init(|| {
        parse_template(GITHUB_SOURCE, "builtin:github")
            .unwrap_or_else(|error| panic!("builtin github preset is invalid: {error}"))
    })
}

impl FromStr for Preset {
    type Err = PlanError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "github" => Ok(Self::Github),
            _ => Err(PlanError::Invalid {
                field: "preset",
                reason: "unknown preset",
            }),
        }
    }
}

/// Expands the presets configured for one Agent domain into its plan.
///
/// The result is canonical: preset order and duplicates in `presets` do not
/// change the encoding.
///
/// # Errors
/// Returns [`PlanError::Invalid`] when `presets` is empty.
pub fn preset_plan(agent: &AgentName, presets: &[Preset]) -> Result<Plan, PlanError> {
    let mut presets = presets.to_vec();
    presets.sort_unstable();
    presets.dedup();
    if presets.is_empty() {
        return Err(PlanError::Invalid {
            field: "presets",
            reason: "at least one preset is required",
        });
    }

    let mut credentials: Vec<CredentialId> = presets
        .iter()
        .map(|preset| preset.template().credential_spec.id.clone())
        .collect();
    credentials.sort();

    let rules: Vec<Rule> = presets
        .iter()
        .flat_map(|preset| {
            let template = preset.template();
            let credential_id = template.credential_spec.id.clone();
            template
                .rules
                .iter()
                .map(move |rule| (credential_id.clone(), rule))
        })
        .enumerate()
        .map(|(index, (credential_id, rule))| Rule {
            index,
            phase: Phase::Request,
            effect: Effect::Allow,
            matcher: RequestMatch {
                scheme: Scheme::Https,
                host: rule.host.clone(),
                path: rule.path.clone(),
                method: MethodMatch {
                    any_of: rule.methods.clone(),
                },
                credential: CredentialMatch {
                    any_of: vec![credential_id],
                },
            },
            response_rewrite: rule.response_rewrite,
            because: rule.because.clone(),
        })
        .collect();

    let plan = Plan {
        format: PlanFormat::V1,
        subject: Subject::Agent {
            name: agent.clone(),
        },
        source: PlanSource::Presets {
            presets: presets
                .iter()
                .map(|preset| {
                    let t = preset.template();
                    format!("{}@{}", t.name, t.version)
                })
                .collect(),
        },
        credentials: credentials
            .into_iter()
            .map(|id| CredentialRef { id })
            .collect(),
        credential_request_default: CredentialDefault::Block,
        request_default: DefaultEffect::Allow,
        rules,
    };
    plan.validate()?;
    Ok(plan)
}
