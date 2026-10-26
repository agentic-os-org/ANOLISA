//! Native registration for caller-owned idle processes; no Agent launch or policy execution.

mod files;
mod qoder;
mod qwenpaw;

use crate::{LaunchContext, Readiness, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Durable native ownership record stored beside the service binding.
#[derive(Serialize, Deserialize)]
#[serde(tag = "adapter", rename_all = "lowercase", deny_unknown_fields)]
pub enum Registration {
    /// Exact Qoder groups added to one exclusive native settings source.
    Qoder {
        /// Canonical native settings file.
        settings: PathBuf,
        /// Require no-clobber publication when settings were initially absent.
        created: bool,
        /// Owned groups, matched exactly during removal.
        groups: Value,
        /// Private acknowledgement description used by ConfigChange callbacks.
        confirmation: PathBuf,
        /// Correlation nonce for native confirmation.
        token: String,
        /// Private native receipt.
        ready: PathBuf,
    },
    /// Native QwenPaw plugin API and per-install descriptor.
    Qwenpaw {
        /// Canonical initialized native profile.
        profile: PathBuf,
        /// Same-process loopback API port.
        port: u16,
        /// Private staged plugin tree.
        source: PathBuf,
        /// Private descriptor contents; also identify installed ownership.
        descriptor: Value,
    },
}

impl Registration {
    pub(crate) fn qoder(
        settings: PathBuf,
        mut groups: Value,
        context: &LaunchContext<'_>,
    ) -> Result<Self> {
        files::directory(settings.parent().ok_or("settings have no parent")?)?;
        let created = match files::read(&settings) {
            Ok(bytes) => {
                crate::qoder::enabled(&serde_json::from_slice(&bytes)?)?;
                false
            }
            Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => return Err(error),
        };
        let token = std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .to_owned();
        let ready = context.files.directory().join("attach-ready.json");
        let confirmation = context.files.write(
            "confirmation.json",
            &serde_json::to_vec(&json!({"settings":settings,"ready":ready,"token":token}))?,
        )?;
        groups["hooks"]
            .as_object_mut()
            .ok_or("invalid stable groups")?
            .remove("SessionStart");
        groups["hooks"]["ConfigChange"] = json!([{"matcher":"user_settings","hooks":[{"type":"command","command":context.executable,"args":["bridge","--adapter","qoder","--binding",context.binding_path,"--confirm",confirmation],"timeout":4}]}]);
        Ok(Self::Qoder {
            settings,
            created,
            groups,
            confirmation,
            token,
            ready,
        })
    }

    /// Profile scope used to serialize attachment ownership across service configurations.
    pub fn scope(&self) -> &Path {
        match self {
            Self::Qoder { settings, .. } => settings.parent().unwrap_or(settings),
            Self::Qwenpaw { profile, .. } => profile,
        }
    }

    /// Expected native receipt, reused by launch and attachment verification.
    pub fn readiness(&self) -> Result<Readiness> {
        let (path, token, adapter) = match self {
            Self::Qoder { ready, token, .. } => (ready.clone(), token.clone(), "qoder"),
            Self::Qwenpaw { descriptor, .. } => (
                descriptor["ready_path"]
                    .as_str()
                    .ok_or("missing ready path")?
                    .into(),
                descriptor["ready_token"]
                    .as_str()
                    .ok_or("missing ready token")?
                    .into(),
                "qwenpaw",
            ),
        };
        Ok(Readiness {
            path,
            token,
            adapter: adapter.into(),
            hooks: 0,
            timeout: Duration::from_secs(30),
        })
    }

    /// Publish native registration after the service binding and process identity exist.
    pub fn install(&self) -> Result<()> {
        match self {
            Self::Qoder {
                settings,
                groups,
                created,
                ..
            } => {
                if *created {
                    files::create(settings, &serde_json::to_vec(groups)?)
                } else {
                    files::update(settings, |value| merge(value, groups))
                }
            }
            Self::Qwenpaw { .. } => self.install_plugin(),
        }
    }

    /// Cause a bounded native ConfigChange; success still requires its actual callback.
    pub fn probe(&self, sequence: usize) -> Result<()> {
        if let Self::Qoder {
            settings, groups, ..
        } = self
        {
            files::update(settings, |value| {
                let hooks = value["hooks"]["ConfigChange"]
                    .as_array_mut()
                    .ok_or("missing ConfigChange routes")?;
                let owned = &groups["hooks"]["ConfigChange"][0];
                if !hooks.contains(owned) {
                    return Err("attachment confirmation route changed".into());
                }
                // Unmatched native group changes Hook configuration without executing a tool.
                hooks.retain(|group| !probe_group(group, groups));
                let mut probe = owned.clone();
                probe["matcher"] = json!(format!("aw-attach-probe-{sequence}"));
                hooks.push(probe);
                Ok(())
            })?;
        }
        Ok(())
    }

    /// Remove only this registration, preserving native configuration edits and the Agent.
    pub fn remove(&self) -> Result<()> {
        self.remove_groups()
    }

    /// Confirm native removal before the caller deletes the private bridge files.
    pub fn detach(&self) -> Result<()> {
        match self {
            Self::Qoder { .. } => self.detach_qoder(),
            Self::Qwenpaw { .. } => self.remove_plugin(),
        }
    }

    fn remove_groups(&self) -> Result<()> {
        match self {
            Self::Qoder {
                settings, groups, ..
            } => Self::remove_qoder_groups(settings, groups),
            Self::Qwenpaw { .. } => self.remove_plugin(),
        }
    }

    fn remove_qoder_groups(settings: &Path, groups: &Value) -> Result<()> {
        files::update(settings, |value| {
            let Some(hooks) = value.get_mut("hooks").and_then(Value::as_object_mut) else {
                return Ok(());
            };
            let owned_events = groups["hooks"].as_object().ok_or("invalid owned groups")?;
            // A moved callback still references our bridge. Reject it before any
            // write, even when its event was not part of the original registration.
            for (event, current) in hooks.iter() {
                let current = current
                    .as_array()
                    .ok_or("native Hook event is not an array")?;
                let owned = owned_events.get(event).and_then(Value::as_array);
                for group in current {
                    if references_owned_binding(group, groups)
                        && !owned.is_some_and(|owned| owned.contains(group))
                        && !(event == "ConfigChange" && probe_group(group, groups))
                    {
                        return Err(
                            "AW-owned native group was edited; restore it before retrying removal"
                                .into(),
                        );
                    }
                }
            }
            for (event, owned) in owned_events {
                if let Some(current) = hooks.get_mut(event) {
                    let current = current
                        .as_array_mut()
                        .ok_or("native Hook event is not an array")?;
                    current.retain(|group| {
                        !owned.as_array().is_some_and(|owned| owned.contains(group))
                            && !probe_group(group, groups)
                    });
                    if current.is_empty() {
                        hooks.remove(event);
                    }
                }
            }
            if hooks.is_empty() {
                value
                    .as_object_mut()
                    .ok_or("invalid settings")?
                    .remove("hooks");
            }
            Ok(())
        })
    }
}

fn references_owned_binding(value: &Value, groups: &Value) -> bool {
    let binding = &groups["hooks"]["ConfigChange"][0]["hooks"][0]["args"][4];
    fn contains(value: &Value, binding: &Value) -> bool {
        match value {
            Value::Array(values) => values.iter().any(|value| contains(value, binding)),
            Value::Object(values) => values.values().any(|value| contains(value, binding)),
            Value::String(_) => value == binding,
            _ => false,
        }
    }
    binding.is_string() && contains(value, binding)
}

fn probe_group(group: &Value, groups: &Value) -> bool {
    group["matcher"]
        .as_str()
        .is_some_and(|name| name.starts_with("aw-attach-probe-"))
        && groups["hooks"]["ConfigChange"]
            .as_array()
            .is_some_and(|owned| {
                owned
                    .iter()
                    .any(|candidate| group["hooks"] == candidate["hooks"])
            })
}

fn merge(value: &mut Value, groups: &Value) -> Result<()> {
    crate::qoder::enabled(value)?;
    let hooks = value
        .as_object_mut()
        .ok_or("invalid native settings")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid native hooks")?;
    for (event, added) in groups["hooks"].as_object().ok_or("invalid owned groups")? {
        let current = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("native Hook event is not an array")?;
        for group in added.as_array().ok_or("invalid owned event")? {
            if current.contains(group) {
                return Err("native attachment already installed".into());
            }
            current.push(group.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn edited_owned_groups_are_retained_and_unrelated_groups_survive_removal() {
        let parent = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/attachment-tests");
        fs::create_dir_all(&parent).unwrap();
        let directory = tempfile::tempdir_in(parent).unwrap();
        let settings = directory.path().join("settings.json");
        fs::write(&settings, b"{}").unwrap();
        fs::set_permissions(&settings, fs::Permissions::from_mode(0o600)).unwrap();
        let group = json!({"hooks":[{"command":"/aw","args":["bridge","--adapter","qoder","--binding","/private/binding.json"]}]});
        let groups = json!({"hooks":{"ConfigChange":[group]}});
        let registration = Registration::Qoder {
            settings: settings.clone(),
            created: false,
            groups: groups.clone(),
            confirmation: directory.path().join("confirm.json"),
            token: "token".into(),
            ready: directory.path().join("ready.json"),
        };
        registration.install().unwrap();
        let mut edited = groups.clone();
        edited["hooks"]["ConfigChange"][0]["matcher"] = json!("edited");
        fs::write(&settings, edited.to_string()).unwrap();
        assert!(registration.remove().is_err());
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&settings).unwrap()).unwrap(),
            edited
        );
        for event in ["Stop", "PostToolUse"] {
            let mut relocated = groups.clone();
            relocated["hooks"][event] = groups["hooks"]["ConfigChange"].clone();
            fs::write(&settings, relocated.to_string()).unwrap();
            assert!(registration.remove().is_err());
            assert_eq!(
                serde_json::from_slice::<Value>(&fs::read(&settings).unwrap()).unwrap(),
                relocated
            );
        }
        let mut native = groups;
        native["theme"] = json!("kept");
        native["hooks"]["ConfigChange"]
            .as_array_mut()
            .unwrap()
            .push(json!({"hooks":[{"command":"/bin/true"}]}));
        fs::write(&settings, native.to_string()).unwrap();
        registration.remove().unwrap();
        let remaining: Value = serde_json::from_slice(&fs::read(&settings).unwrap()).unwrap();
        assert_eq!(remaining["theme"], "kept");
        assert_eq!(
            remaining["hooks"]["ConfigChange"],
            json!([{"hooks":[{"command":"/bin/true"}]}])
        );
    }
}
