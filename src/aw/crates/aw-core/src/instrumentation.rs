//! Project a verified builtin profile binding onto the public SDK wire ceiling.

use crate::admission::{AdapterCapabilities, AdmissionError};
use aw_contracts::{
    deployment::{InstrumentationBinding, SDK_ENTRYPOINT, SDK_EVENTS, SDK_PROTOCOL},
    instrumentation::{Codec, InstrumentationProfile, Runtime},
    source::{FailureAction, HookAuthority, HookEffect, HookPointCapability, SourceCapability},
    Error,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Merges the Hook surfaces of all profiles bound in one SDK process.
///
/// Each shared event advertises only what every seam declaring it can adopt.
/// # Errors
/// Rejects invalid profile or Source contracts.
pub fn source(
    profiles: &[&InstrumentationProfile],
    adapter: &str,
) -> Result<Option<SourceCapability>, Error> {
    let Some(first) = profiles.first() else {
        return Ok(None);
    };
    let mut hooks = BTreeMap::<String, HookPointCapability>::new();
    for profile in profiles {
        if profile.targets().iter().any(|target| {
            (target.hooks().contains_key("turn.stop") && target.field("output").is_some())
                || (profile.runtime() == Runtime::Python
                    && target.field("input").and_then(|field| field.codec()) == Some(Codec::Text))
        }) {
            return Err(Error::Invariant(
                "profile field is outside the SDK instrumentation surface",
            ));
        }
        let capability = profile.source_capability(adapter, SDK_ENTRYPOINT)?;
        for (event, hook) in capability.hooks() {
            let (_, allowed) =
                SDK_EVENTS
                    .iter()
                    .find(|(name, _)| *name == event)
                    .ok_or(Error::Invariant(
                        "profile event is outside the SDK instrumentation surface",
                    ))?;
            let mut projected = hook.clone();
            projected
                .effects
                .retain(|effect| allowed.contains(&effect.as_str()));
            if event == "tool.before"
                && profile.runtime() == Runtime::Js
                && profile.targets().iter().any(|target| {
                    target.hooks().contains_key(event)
                        && !target
                            .field("input")
                            .and_then(|field| field.path())
                            .is_some_and(|path| path.starts_with("arg."))
                })
            {
                projected
                    .effects
                    .retain(|effect| *effect != HookEffect::ReplaceInput);
            }
            hooks
                .entry(event.clone())
                .and_modify(|previous| {
                    if projected.mode == HookAuthority::Observe {
                        previous.mode = HookAuthority::Observe;
                    }
                    previous
                        .effects
                        .retain(|effect| projected.effects.contains(effect));
                    previous
                        .failure_actions
                        .retain(|action| projected.failure_actions.contains(action));
                })
                .or_insert(projected);
        }
    }
    for hook in hooks.values_mut() {
        // The SDK wire implies report from the event and block from the effect.
        // Python also requires those actions to be declared by every shared seam.
        if first.runtime() == Runtime::Python
            && (!hook.failure_actions.contains(&FailureAction::Report)
                || (hook.effects.contains(&HookEffect::Block)
                    && !hook.failure_actions.contains(&FailureAction::Block)))
        {
            return Err(Error::Invariant(
                "Python builtin profile lacks an implied SDK failure action",
            ));
        }
        hook.failure_actions
            .retain(|action| *action == FailureAction::WithholdResult);
    }
    SourceCapability::new(
        first.id(),
        first.runtime().source_kind(),
        adapter,
        SDK_PROTOCOL,
        SDK_ENTRYPOINT,
        hooks,
    )
    .map(Some)
}

/// Converts the projected Source to SDK registration capabilities.
pub fn capabilities(source: &SourceCapability) -> AdapterCapabilities {
    AdapterCapabilities {
        adapter: source.adapter().into(),
        version: SDK_PROTOCOL.into(),
        entrypoint: SDK_ENTRYPOINT.into(),
        post_permission: false,
        events: source
            .hooks()
            .iter()
            .map(|(event, hook)| {
                (
                    event.clone(),
                    hook.effects
                        .iter()
                        .map(|effect| effect.as_str().into())
                        .collect(),
                )
            })
            .collect(),
        failure_actions: source
            .hooks()
            .iter()
            .filter(|(_, hook)| !hook.failure_actions.is_empty())
            .map(|(event, hook)| {
                (
                    event.clone(),
                    hook.failure_actions
                        .iter()
                        .map(|action| action.as_str().into())
                        .collect(),
                )
            })
            .collect(),
    }
}

/// Binds canonical compiled profile digests to the projected SDK surface.
///
/// # Errors
/// Returns a contract error for invalid canonical profile bytes.
pub fn binding(
    profiles: &[&InstrumentationProfile],
    source: &SourceCapability,
) -> Result<InstrumentationBinding, Error> {
    let caps = capabilities(source);
    Ok(InstrumentationBinding {
        runtime: profiles
            .first()
            .ok_or(Error::Invariant("empty profile binding"))?
            .runtime(),
        profiles: profiles
            .iter()
            .map(|profile| {
                Ok((
                    profile.id().to_owned(),
                    format!("{:x}", Sha256::digest(profile.to_canonical_bytes()?)),
                ))
            })
            .collect::<Result<_, Error>>()?,
        events: caps.events,
        failure_actions: caps.failure_actions,
    })
}

pub(crate) fn admitted_source(
    agent: &serde_json::Value,
    target: &str,
    offered: &AdapterCapabilities,
) -> Result<Option<SourceCapability>, AdmissionError> {
    let profiles = aw_config::instrumentation::profiles(target, agent)?;
    let Some(source) = source(&profiles, &offered.adapter)
        .map_err(|_| AdmissionError::Rejected("invalid builtin instrumentation capability"))?
    else {
        return Ok(None);
    };
    let ceiling = capabilities(&source);
    let subset = |values: &BTreeMap<String, Vec<String>>,
                  allowed: &BTreeMap<String, Vec<String>>| {
        values.iter().all(|(event, effects)| {
            allowed
                .get(event)
                .is_some_and(|supported| effects.iter().all(|effect| supported.contains(effect)))
        })
    };
    if offered.version != ceiling.version
        || offered.entrypoint != ceiling.entrypoint
        || offered.post_permission
        || !subset(&offered.events, &ceiling.events)
        || !subset(&offered.failure_actions, &ceiling.failure_actions)
    {
        return Err(AdmissionError::Rejected(
            "SDK evidence exceeds the configured instrumentation profiles",
        ));
    }
    let mut hooks = source.hooks().clone();
    hooks.retain(|event, hook| {
        let Some(effects) = offered.events.get(event) else {
            return false;
        };
        hook.effects
            .retain(|effect| effects.iter().any(|name| name == effect.as_str()));
        hook.failure_actions.retain(|action| {
            offered
                .failure_actions
                .get(event)
                .is_some_and(|actions| actions.iter().any(|name| name == action.as_str()))
        });
        true
    });
    SourceCapability::new(
        source.source_id(),
        source.kind(),
        &offered.adapter,
        SDK_PROTOCOL,
        SDK_ENTRYPOINT,
        hooks,
    )
    .map(Some)
    .map_err(|_| AdmissionError::Rejected("invalid SDK instrumentation capability evidence"))
}
