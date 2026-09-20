use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use super::marker::{bash_marker_script, zsh_marker_script};
use super::model::ShellHostConfig;

/// #R2: the Bash launch shape when a marker is present. Pure decision extracted
/// from `configure_command` so it is unit-testable without constructing a
/// `Command` (keeps the argv contract out of the source-heavy test inventory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BashMarkerLaunch {
    /// R2 real login identity: `argv0="-bash" --posix -i`, marker via `$ENV`.
    LoginIdentity,
    /// Interactive non-login: `[--noprofile] --rcfile <marker> -i`.
    Rcfile { noprofile: bool },
}

pub(super) fn bash_marker_launch(uses_login_identity: bool, native_mode: bool) -> BashMarkerLaunch {
    if uses_login_identity {
        BashMarkerLaunch::LoginIdentity
    } else {
        BashMarkerLaunch::Rcfile {
            noprofile: !native_mode,
        }
    }
}

pub(super) trait ShellAdapter {
    fn executable<'a>(&self, config: &'a ShellHostConfig) -> &'a str;
    fn marker_filename(&self) -> &'static str;
    fn marker_script(&self) -> &'static str;
    fn isolates_readline(&self) -> bool;
    fn supports_zsh_path_prompt_buffering(&self) -> bool {
        false
    }
    fn configure_command(
        &self,
        command: &mut Command,
        marker_path: Option<&Path>,
        config: &ShellHostConfig,
    );

    /// #R2: whether this adapter launches its login session with a real login
    /// identity delivered through a `$ENV` inject file (Bash only) instead of
    /// the interactive-but-non-login `--rcfile <marker>`. When true, the caller
    /// (`start_shell_session`) writes the inject file and sets `$ENV`, and
    /// `configure_command` emits `argv0="-bash" --posix -i`. Kept as one shared
    /// predicate so the two sites never drift. Default false (Zsh keeps its own
    /// `-l` path; see R2-SDD §2.2, zsh regression H7).
    fn uses_login_identity_inject(&self, _config: &ShellHostConfig) -> bool {
        false
    }
}

pub(super) struct BashAdapter;

impl ShellAdapter for BashAdapter {
    fn executable<'a>(&self, config: &'a ShellHostConfig) -> &'a str {
        &config.bash_path
    }

    fn marker_filename(&self) -> &'static str {
        "cosh-marker.bash"
    }

    fn marker_script(&self) -> &'static str {
        bash_marker_script()
    }

    fn isolates_readline(&self) -> bool {
        true
    }

    fn uses_login_identity_inject(&self, config: &ShellHostConfig) -> bool {
        // Full R2 condition: Enhanced (marker present) + non-isolated
        // (native_mode) + login + gate on + the inner bash actually sources
        // `$ENV` in posix mode (bash >= 4; macOS system bash 3.2 does not, so
        // R2 would strand a marker-less shell there). The Enhanced leg is
        // included so this predicate alone is authoritative.
        config.integration.uses_markers()
            && config.native_mode
            && config.login_shell
            && config.login_identity
            && config.bash_login_env_posix
    }

    fn configure_command(
        &self,
        command: &mut Command,
        marker_path: Option<&Path>,
        config: &ShellHostConfig,
    ) {
        if let Some(marker_path) = marker_path {
            match bash_marker_launch(self.uses_login_identity_inject(config), config.native_mode) {
                BashMarkerLaunch::LoginIdentity => {
                    // R2: real login identity. argv0="-bash" gives the inner
                    // Bash a genuine login shell (shopt -q login_shell == yes);
                    // --posix makes $ENV the sole startup-injection point. The
                    // marker is delivered via $ENV=<inject> set by the caller
                    // (start_shell_session), not --rcfile. marker_path unused here.
                    command.arg0("-bash").args(["--posix", "-i"]);
                }
                BashMarkerLaunch::Rcfile { noprofile } => {
                    if noprofile {
                        command.args(["--noprofile", "--rcfile"]);
                    } else {
                        command.arg("--rcfile");
                    }
                    command.arg(marker_path).arg("-i");
                }
            }
            if config.login_shell {
                command.env("COSH_LOGIN_SHELL", "1");
            }
        } else {
            if !config.native_mode {
                command.args(["--noprofile", "--norc"]);
            }
            if config.login_shell {
                command.arg("--login");
            }
            command.arg("-i");
        }
    }
}

pub(super) struct ZshAdapter;

impl ShellAdapter for ZshAdapter {
    fn executable<'a>(&self, config: &'a ShellHostConfig) -> &'a str {
        &config.zsh_path
    }

    fn marker_filename(&self) -> &'static str {
        ".zshrc"
    }

    fn marker_script(&self) -> &'static str {
        zsh_marker_script()
    }

    fn isolates_readline(&self) -> bool {
        false
    }

    fn supports_zsh_path_prompt_buffering(&self) -> bool {
        true
    }

    fn configure_command(
        &self,
        command: &mut Command,
        marker_path: Option<&Path>,
        config: &ShellHostConfig,
    ) {
        if marker_path.is_some() {
            command.arg("-i").env("ZDOTDIR", &config.work_dir);
            if config.native_mode {
                if let Some(original) = configured_original_zdotdir(config) {
                    command.env("COSH_ZDOTDIR_ORIG", original);
                }
            }
            if config.login_shell {
                command.env("COSH_LOGIN_SHELL", "1");
            }
        } else {
            if !config.native_mode {
                command.env("ZDOTDIR", &config.work_dir);
            }
            if config.login_shell {
                command.arg("-l");
            }
            command.arg("-i");
        }
    }
}

fn configured_original_zdotdir(config: &ShellHostConfig) -> Option<String> {
    for key in ["ZDOTDIR", "HOME"] {
        if let Some((_, value)) = config
            .env_overrides
            .iter()
            .rev()
            .find(|(name, _)| name == key)
        {
            return Some(value.clone());
        }
    }
    std::env::var("ZDOTDIR")
        .or_else(|_| std::env::var("HOME"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::super::model::ShellIntegration;
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    fn test_config(native_mode: bool, login_shell: bool) -> ShellHostConfig {
        let mut config = ShellHostConfig::new("test", PathBuf::from("/tmp/test"));
        config.integration = ShellIntegration::Enhanced;
        config.native_mode = native_mode;
        config.login_shell = login_shell;
        config
    }

    fn collect_args(cmd: &Command) -> Vec<String> {
        cmd.get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn has_env(cmd: &Command, key: &str) -> bool {
        cmd.get_envs().any(|(k, _)| k == OsStr::new(key))
    }

    #[test]
    fn bash_native_mode_omits_noprofile() {
        let config = test_config(true, false);
        let mut cmd = Command::new("bash");
        let marker = PathBuf::from("/tmp/marker.bash");
        BashAdapter.configure_command(&mut cmd, Some(&marker), &config);
        let args = collect_args(&cmd);
        assert!(!args.contains(&"--noprofile".to_string()));
        assert!(args.contains(&"--rcfile".to_string()));
        assert!(args.contains(&"-i".to_string()));
    }

    #[test]
    fn bash_isolated_mode_includes_noprofile() {
        let config = test_config(false, false);
        let mut cmd = Command::new("bash");
        let marker = PathBuf::from("/tmp/marker.bash");
        BashAdapter.configure_command(&mut cmd, Some(&marker), &config);
        let args = collect_args(&cmd);
        assert!(args.contains(&"--noprofile".to_string()));
        assert!(args.contains(&"--rcfile".to_string()));
    }

    #[test]
    fn bash_login_shell_sets_env() {
        let config = test_config(true, true);
        let mut cmd = Command::new("bash");
        let marker = PathBuf::from("/tmp/marker.bash");
        BashAdapter.configure_command(&mut cmd, Some(&marker), &config);
        assert!(has_env(&cmd, "COSH_LOGIN_SHELL"));
    }

    #[test]
    fn bash_non_login_shell_no_env() {
        let config = test_config(true, false);
        let mut cmd = Command::new("bash");
        let marker = PathBuf::from("/tmp/marker.bash");
        BashAdapter.configure_command(&mut cmd, Some(&marker), &config);
        assert!(!has_env(&cmd, "COSH_LOGIN_SHELL"));
    }

    #[test]
    fn zsh_login_shell_sets_env() {
        let config = test_config(true, true);
        let mut cmd = Command::new("zsh");
        let marker = PathBuf::from("/tmp/marker.zsh");
        ZshAdapter.configure_command(&mut cmd, Some(&marker), &config);
        assert!(has_env(&cmd, "COSH_LOGIN_SHELL"));
    }

    #[test]
    fn zsh_native_mode_prefers_isolated_home_for_zdotdir_orig() {
        let mut config = test_config(true, false);
        config
            .env_overrides
            .push(("HOME".to_string(), "/tmp/cosh-test-home".to_string()));
        let mut cmd = Command::new("zsh");
        let marker = PathBuf::from("/tmp/marker.zsh");
        ZshAdapter.configure_command(&mut cmd, Some(&marker), &config);

        assert!(cmd.get_envs().any(|(key, value)| {
            key == OsStr::new("COSH_ZDOTDIR_ORIG")
                && value == Some(OsStr::new("/tmp/cosh-test-home"))
        }));
    }

    #[test]
    fn zsh_isolated_mode_no_zdotdir_orig() {
        let config = test_config(false, false);
        let mut cmd = Command::new("zsh");
        let marker = PathBuf::from("/tmp/marker.zsh");
        ZshAdapter.configure_command(&mut cmd, Some(&marker), &config);
        assert!(!has_env(&cmd, "COSH_ZDOTDIR_ORIG"));
    }

    // --- S1: R2 real-login-identity argv assembly (issue: R2 SDD §2.2-A) ---

    #[test]
    fn bash_r2_predicate_requires_all_legs() {
        // R2 fires only when every leg holds: Enhanced (marker) + native +
        // login + gate on + inner bash sources $ENV in posix. test_config sets
        // Enhanced + native/login; struct defaults give login_identity=false,
        // bash_login_env_posix=true. Dropping any leg must disable R2.
        let mut all = test_config(true, true);
        all.login_identity = true;
        assert!(
            BashAdapter.uses_login_identity_inject(&all),
            "all legs present => R2"
        );

        let mut gate_off = all.clone();
        gate_off.login_identity = false;
        assert!(
            !BashAdapter.uses_login_identity_inject(&gate_off),
            "gate off => not R2"
        );

        let mut non_login = test_config(true, false);
        non_login.login_identity = true;
        assert!(
            !BashAdapter.uses_login_identity_inject(&non_login),
            "non-login => not R2"
        );

        let mut isolated = test_config(false, true);
        isolated.login_identity = true;
        assert!(
            !BashAdapter.uses_login_identity_inject(&isolated),
            "isolated (not native) => not R2"
        );

        let mut old_bash = all.clone();
        old_bash.bash_login_env_posix = false;
        assert!(
            !BashAdapter.uses_login_identity_inject(&old_bash),
            "inner bash without posix $ENV (e.g. bash 3.2) => not R2"
        );
    }

    #[test]
    fn bash_marker_launch_maps_gate_to_argv_shape() {
        // Pure argv decision (no Command constructed): R2 login identity when
        // the predicate holds; otherwise --rcfile, with --noprofile only when
        // isolated (not native). argv0="-bash" / --posix effect is verified at
        // the S3 PTY / L4 container layer (std exposes no arg0 getter).
        assert_eq!(
            bash_marker_launch(true, true),
            BashMarkerLaunch::LoginIdentity
        );
        assert_eq!(
            bash_marker_launch(true, false),
            BashMarkerLaunch::LoginIdentity
        );
        assert_eq!(
            bash_marker_launch(false, true),
            BashMarkerLaunch::Rcfile { noprofile: false }
        );
        assert_eq!(
            bash_marker_launch(false, false),
            BashMarkerLaunch::Rcfile { noprofile: true }
        );
    }
}
