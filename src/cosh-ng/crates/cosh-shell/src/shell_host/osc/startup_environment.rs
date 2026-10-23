//! One-shot PATH report the managed shell emits after its startup files ran.
//!
//! Provider launches trust this PATH instead of re-reading login profiles, so
//! only a token-authenticated report for this session, received before the
//! first prompt, is accepted. Anything else leaves providers on the inherited
//! process PATH.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::super::model::ShellStartupPathObserver;
use super::marker_sequence::Marker;

const STARTUP_ENVIRONMENT_EVENT: &str = "startup_environment";
pub(super) const SHELL_PATH_MAX_BYTES: usize = 8 * 1024;

#[derive(Debug, Default)]
pub(super) struct StartupEnvironmentGate {
    observer: Option<ShellStartupPathObserver>,
    closed: bool,
}

impl StartupEnvironmentGate {
    pub(super) fn set_observer(&mut self, observer: ShellStartupPathObserver) {
        self.observer = Some(observer);
    }

    /// Accepts the first valid report; malformed reports do not consume it.
    pub(super) fn observe_if_startup_report(&mut self, marker: &Marker, session_id: &str) -> bool {
        if marker.event != STARTUP_ENVIRONMENT_EVENT {
            return false;
        }
        if self.closed || marker.session_id.as_deref() != Some(session_id) {
            return true;
        }
        let Some(path) = marker.path.as_deref().and_then(trusted_startup_path) else {
            return true;
        };
        self.closed = true;
        if let Some(observer) = &self.observer {
            observer.observe(Some(path));
        }
        true
    }

    /// Startup ends at the first prompt; missing reports keep inherited PATH.
    pub(super) fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(observer) = &self.observer {
            observer.observe(None);
        }
    }
}

impl super::OscParser {
    pub(in crate::shell_host) fn with_startup_path_observer(
        mut self,
        observer: ShellStartupPathObserver,
    ) -> Self {
        self.startup_environment.set_observer(observer);
        self
    }

    pub(super) fn observe_startup_environment(&mut self, marker: &Marker) -> bool {
        self.startup_environment
            .observe_if_startup_report(marker, &self.session_id)
    }
}

fn trusted_startup_path(path: &str) -> Option<String> {
    if path.len() > SHELL_PATH_MAX_BYTES || path.chars().any(char::is_control) {
        return None;
    }
    let normalized = normalize_shell_path(path);
    (!normalized.is_empty()).then_some(normalized)
}

pub(super) fn normalize_shell_path(path: &str) -> String {
    let mut seen = HashSet::new();
    path.split(':')
        .filter_map(normalize_absolute_path)
        .filter(|entry| seen.insert(entry.clone()))
        .collect::<Vec<_>>()
        .join(":")
}

fn normalize_absolute_path(value: &str) -> Option<String> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => normalized.push(".."),
            Component::Normal(part) => normalized.push(part),
            Component::Prefix(_) => return None,
        }
    }
    Some(normalized.to_string_lossy().into_owned())
}

#[cfg(test)]
#[path = "startup_environment_tests.rs"]
mod tests;
