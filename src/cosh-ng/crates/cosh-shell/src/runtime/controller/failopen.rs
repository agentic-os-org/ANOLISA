//! Fresh-login fail-open guardian for the interactive-login relay.
//!
//! When cosh is a default login shell, a failure while bringing up the TUI
//! runtime must not lock the user out of a fresh SSH/PAM login. This guardian
//! combines immutable fallback eligibility with the shared login-effect state.

use crate::shell_host::LoginEffectGuard;
#[cfg(debug_assertions)]
use crate::shell_host::LoginEffectSource;

#[derive(Clone, Copy, Debug)]
pub(super) enum FailOpenFailure {
    InvalidIntegration,
    RelayPanic,
    RelayError,
}

pub(super) struct FreshLoginFailOpen {
    eligible: bool,
    effects: LoginEffectGuard,
    bash_path: String,
}

impl FreshLoginFailOpen {
    pub(super) fn new(
        login: bool,
        all_tty: bool,
        isolated: bool,
        effects: LoginEffectGuard,
    ) -> Self {
        Self {
            eligible: login && all_tty && !isolated,
            effects,
            bash_path: "bash".into(),
        }
    }

    pub(super) fn with_bash_path(mut self, path: &str) -> Self {
        self.bash_path = path.to_owned();
        self
    }

    fn may_fall_open(&self, failure: FailOpenFailure) -> bool {
        match failure {
            FailOpenFailure::InvalidIntegration
            | FailOpenFailure::RelayPanic
            | FailOpenFailure::RelayError => self.eligible && !self.effects.may_have_started(),
        }
    }

    /// Executes the native login fallback only when policy and effect state permit it.
    pub(super) fn try_exec(&self, failure: FailOpenFailure, diagnostic: &str) -> Option<i32> {
        if !self.may_fall_open(failure) {
            return None;
        }
        eprintln!("{diagnostic}");
        Some(exec_native_login_bash(&self.bash_path))
    }
}

/// Debug-only fault injection for the fresh-login fail-open tests. Compiled out
/// of release builds. `COSH_FAILOPEN_FORCE_PANIC=pre-spawn` panics before the
/// managed child shell is spawned (exercises the catch_unwind fallback);
/// `post-spawn` panics only after it (must NOT fall open).
#[cfg(debug_assertions)]
pub(super) fn maybe_inject_failopen_panic(phase: &str, effects: &LoginEffectGuard) {
    let managed_shell_started = effects.has(LoginEffectSource::ManagedShell);
    match std::env::var("COSH_FAILOPEN_FORCE_PANIC").as_deref() {
        Ok("pre-spawn") if phase == "pre-spawn" && !managed_shell_started => {
            panic!("cosh fail-open fault injection: pre-spawn panic")
        }
        Ok("post-spawn") if phase == "post-spawn" && managed_shell_started => {
            panic!("cosh fail-open fault injection: post-spawn panic")
        }
        _ => {}
    }
}

fn native_login_bash_command(path: &str) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new(path);
    command
        .arg0("-bash")
        .arg("-i")
        .env("COSH_INTERNAL_SHELL", "1");
    command
}

/// Replace this process with a clean native interactive login `bash` — the
/// interactive-login counterpart to the `ExecShell` classifier branch, so a
/// fresh login is never locked out when cosh's own startup fails.
///
/// The child is a real login shell (`argv[0] = -bash`, so it runs the login
/// startup files), marks the shell internal to suppress Cosh guidance, and restores the
/// inherited SIGPIPE disposition (on Linux) via `pre_exec`. It still inherits
/// this process's environment, so it is not a pristine login environment.
/// Callers must ensure the terminal is restored to a sane mode (the raw-mode
/// guard dropped) first, since `exec` never returns on success. Returns 127/126
/// only when `bash` itself cannot be executed (already the hard-lock domain).
fn exec_native_login_bash(path: &str) -> i32 {
    use std::os::unix::process::CommandExt;

    let mut command = native_login_bash_command(path);
    unsafe {
        command.pre_exec(crate::shell_host::sigpipe::restore_in_child);
    }
    let error = command.exec();
    let reason = match error.kind() {
        std::io::ErrorKind::NotFound => "No such file or directory".to_string(),
        std::io::ErrorKind::PermissionDenied => "Permission denied".to_string(),
        _ => error.to_string(),
    };
    eprintln!("-bash: bash: {reason}");
    if error.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_fallback_keeps_the_verified_absolute_bash_program() {
        let fallback = FreshLoginFailOpen::new(true, true, false, LoginEffectGuard::new())
            .with_bash_path("/usr/bin/bash");
        let command = native_login_bash_command(&fallback.bash_path);
        assert_eq!(command.get_program(), std::ffi::OsStr::new("/usr/bin/bash"));
        assert!(fallback.may_fall_open(FailOpenFailure::InvalidIntegration));
    }

    #[test]
    fn early_login_fallback_is_marked_internal_without_spawning() {
        let command = native_login_bash_command("bash");
        assert!(command
            .get_envs()
            .any(|(name, value)| name == "COSH_INTERNAL_SHELL"
                && value == Some(std::ffi::OsStr::new("1"))));
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [std::ffi::OsStr::new("-i")]
        );
    }

    use crate::shell_host::LoginEffectSource::{
        ManagedShell, PathBootstrapProbe, R2CapabilityProbe,
    };

    mod fresh_login_fail_open {
        use super::*;

        const FAILURES: [FailOpenFailure; 3] = [
            FailOpenFailure::InvalidIntegration,
            FailOpenFailure::RelayPanic,
            FailOpenFailure::RelayError,
        ];

        #[test]
        fn fallback_requires_eligible_policy_and_armed_guard() {
            assert!(
                FreshLoginFailOpen::new(true, true, false, LoginEffectGuard::new())
                    .may_fall_open(FailOpenFailure::InvalidIntegration)
            );
            assert!(
                !FreshLoginFailOpen::new(false, true, false, LoginEffectGuard::new())
                    .may_fall_open(FailOpenFailure::InvalidIntegration)
            );
            assert!(
                !FreshLoginFailOpen::new(true, false, false, LoginEffectGuard::new())
                    .may_fall_open(FailOpenFailure::InvalidIntegration)
            );
            assert!(
                !FreshLoginFailOpen::new(true, true, true, LoginEffectGuard::new())
                    .may_fall_open(FailOpenFailure::InvalidIntegration)
            );
        }

        #[test]
        fn supplied_guard_clone_controls_fail_open_state() {
            let effects = LoginEffectGuard::new();
            let fallback = FreshLoginFailOpen::new(true, true, false, effects.clone());

            assert!(fallback.may_fall_open(FailOpenFailure::RelayError));
            effects.mark_possible(PathBootstrapProbe);
            assert!(!fallback.may_fall_open(FailOpenFailure::RelayError));
        }

        #[test]
        fn denied_fallback_does_not_execute_native_login_bash() {
            let fallback = FreshLoginFailOpen::new(false, true, false, LoginEffectGuard::new());

            assert_eq!(
                fallback.try_exec(FailOpenFailure::RelayError, "must not be emitted"),
                None
            );
        }

        #[test]
        fn every_failure_observes_armed_and_disarmed_guard_state() {
            for failure in FAILURES {
                let armed = FreshLoginFailOpen::new(true, true, false, LoginEffectGuard::new());
                assert!(
                    armed.may_fall_open(failure),
                    "{failure:?} must allow an armed guard"
                );
            }

            for source in [PathBootstrapProbe, R2CapabilityProbe, ManagedShell] {
                for failure in FAILURES {
                    let effects = LoginEffectGuard::new();
                    let disarmed = FreshLoginFailOpen::new(true, true, false, effects.clone());
                    effects.mark_possible(source);
                    assert!(
                        !disarmed.may_fall_open(failure),
                        "{failure:?} must reject {source:?}"
                    );
                }
            }
        }
    }
}
