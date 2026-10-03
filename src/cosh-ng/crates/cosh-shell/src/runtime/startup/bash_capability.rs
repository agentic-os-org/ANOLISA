use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nix::pty::Winsize;
use tempfile::NamedTempFile;

use super::{
    descendants::run_supervised_profile_probe,
    probe_outcome::{record_probe_outcome, ProbeStartOutcome},
    BootstrapPathProbeError,
};
use crate::shell_host::{LoginEffectGuard, LoginEffectSource};

const R2_BASH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const R2_BASH_PROBE_SENTINEL: &str = "__COSH_R2_LOGIN_ENV_OK__";

fn capability_inject_script() -> String {
    format!(
        "set +o posix\nif shopt -q login_shell && [[ ! -o posix ]]; then printf '\\n__COSH_PATH_BEGIN__{R2_BASH_PROBE_SENTINEL}__COSH_PATH_END__\\n'; fi\nexec /bin/sh -c :\n"
    )
}

/// Resolve and probe the exact Bash that execvp would select after PATH
/// bootstrap. Candidates that cannot execute (including EACCES) are skipped;
/// only a proven spawn failure advances to the next candidate. Once execution
/// may have started, that candidate is frozen for the eventual spawn even when
/// the probe outcome is uncertain. The probe exercises the actual R2 contract
/// (`argv0=-bash`, `--posix -i`, `$ENV` startup and `set +o posix`) under the
/// descendant-owning profile-probe supervisor. It replaces the shell process
/// instead of exiting, so discovery cannot run `.bash_logout`. Any incomplete
/// contract fails closed to `--rcfile`.
pub(crate) fn resolve_bash_for_r2(
    program: &str,
    winsize: &Winsize,
    effects: &LoginEffectGuard,
) -> Option<(PathBuf, bool)> {
    let mut inject = NamedTempFile::new().ok()?;
    inject
        .write_all(capability_inject_script().as_bytes())
        .ok()?;
    inject.flush().ok()?;

    if Path::new(program).components().count() > 1 {
        return probe_bash(
            PathBuf::from(program),
            inject.path(),
            winsize,
            R2_BASH_PROBE_TIMEOUT,
            effects,
        );
    }
    let path = std::env::var_os("PATH")?;
    resolve_bash_in_path(
        program,
        &path,
        inject.path(),
        winsize,
        R2_BASH_PROBE_TIMEOUT,
        effects,
    )
}

fn resolve_bash_in_path(
    program: &str,
    path: &OsStr,
    inject_path: &Path,
    winsize: &Winsize,
    timeout: Duration,
    effects: &LoginEffectGuard,
) -> Option<(PathBuf, bool)> {
    resolve_bash_candidates(
        std::env::split_paths(path).map(|directory| directory.join(program)),
        effects,
        |candidate| run_bash_probe(candidate, inject_path, winsize, timeout),
    )
}

fn resolve_bash_candidates<I, F>(
    candidates: I,
    effects: &LoginEffectGuard,
    mut run_probe: F,
) -> Option<(PathBuf, bool)>
where
    I: IntoIterator<Item = PathBuf>,
    F: FnMut(&Path) -> Result<String, BootstrapPathProbeError>,
{
    for candidate in candidates {
        if !is_executable_candidate(&candidate) {
            continue;
        }
        let outcome = run_probe(&candidate);
        if record_probe_outcome(effects, LoginEffectSource::R2CapabilityProbe, &outcome)
            == ProbeStartOutcome::ProvenNotStarted
        {
            continue;
        }
        return Some((
            candidate,
            outcome.is_ok_and(|marker| marker == R2_BASH_PROBE_SENTINEL),
        ));
    }
    None
}

fn is_executable_candidate(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn probe_bash(
    path: PathBuf,
    inject_path: &Path,
    winsize: &Winsize,
    timeout: Duration,
    effects: &LoginEffectGuard,
) -> Option<(PathBuf, bool)> {
    resolve_bash_candidates([path], effects, |candidate| {
        run_bash_probe(candidate, inject_path, winsize, timeout)
    })
}

fn run_bash_probe(
    path: &Path,
    inject_path: &Path,
    winsize: &Winsize,
    timeout: Duration,
) -> Result<String, BootstrapPathProbeError> {
    let mut command = Command::new(path);
    command
        .args(["--posix", "-i"])
        .env("COSH_R2_CAPABILITY_INJECT", inject_path)
        .env("ENV", "${COSH_R2_CAPABILITY_INJECT}")
        .env("LC_ALL", "C");

    run_supervised_profile_probe(command, Some(OsStr::new("-bash")), timeout, winsize)
}

/// POSIX Bash rejects exported function names that are not shell identifiers
/// before it reads `$ENV`, so the inject cannot recover them. Fall back to the
/// non-posix `--rcfile` launch whenever such a function is present; ordinary
/// exported functions remain compatible with R2.
pub(crate) fn exported_bash_functions_posix_compatible() -> bool {
    std::env::vars_os().all(|(key, _)| bash_function_env_key_posix_compatible(&key))
}

fn bash_function_env_key_posix_compatible(key: &OsStr) -> bool {
    const PREFIX: &[u8] = b"BASH_FUNC_";
    const SUFFIX: &[u8] = b"%%";
    let bytes = key.as_bytes();
    let Some(name) = bytes
        .strip_prefix(PREFIX)
        .and_then(|rest| rest.strip_suffix(SUFFIX))
    else {
        return true;
    };
    name.first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && name[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell_host::{LoginEffectGuard, LoginEffectSource};
    #[cfg(target_os = "linux")]
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn probe_winsize() -> Winsize {
        Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }

    fn temp_probe_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "cosh-bash-probe-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn capability_inject() -> NamedTempFile {
        let mut inject = NamedTempFile::new().expect("create capability inject");
        inject
            .write_all(capability_inject_script().as_bytes())
            .expect("write capability inject");
        inject.flush().expect("flush capability inject");
        inject
    }

    struct ScopedPath(Option<std::ffi::OsString>);

    impl ScopedPath {
        fn remove() -> Self {
            let previous = std::env::var_os("PATH");
            std::env::remove_var("PATH");
            Self(previous)
        }

        fn set(path: &Path) -> Self {
            let previous = std::env::var_os("PATH");
            std::env::set_var("PATH", path);
            Self(previous)
        }
    }

    impl Drop for ScopedPath {
        fn drop(&mut self) {
            match &self.0 {
                Some(path) => std::env::set_var("PATH", path),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    #[test]
    fn uncertain_first_candidate_freezes_without_probing_second_candidate() {
        let dir = temp_probe_dir("uncertain-first");
        std::fs::create_dir_all(&dir).expect("create candidate directory");
        let first = dir.join("first-bash");
        let second = dir.join("second-bash");
        for candidate in [&first, &second] {
            std::fs::write(candidate, "#!/bin/sh\nexit 0\n").expect("write candidate");
            std::fs::set_permissions(candidate, std::fs::Permissions::from_mode(0o755))
                .expect("make candidate executable");
        }
        let mut probed = Vec::new();
        let effects = LoginEffectGuard::new();

        let resolved =
            resolve_bash_candidates([first.clone(), second.clone()], &effects, |candidate| {
                probed.push(candidate.to_path_buf());
                if candidate == first {
                    Err(BootstrapPathProbeError::Supervisor(std::io::Error::other(
                        "helper failed after target execution became possible",
                    )))
                } else {
                    Ok(R2_BASH_PROBE_SENTINEL.to_string())
                }
            });

        assert_eq!(resolved, Some((first.clone(), false)));
        assert_eq!(probed, vec![first]);
        assert!(effects.has(LoginEffectSource::R2CapabilityProbe));
        std::fs::remove_dir_all(&dir).expect("remove candidate directory");
    }

    #[test]
    fn missing_path_skips_r2_probe_and_keeps_effects_armed() {
        let _env = crate::diagnostics::test_env::env_guard();
        let _path = ScopedPath::remove();
        let effects = LoginEffectGuard::new();

        assert_eq!(
            resolve_bash_for_r2("bash", &probe_winsize(), &effects),
            None
        );
        assert!(!effects.may_have_started());
    }

    #[test]
    fn empty_path_directory_skips_r2_probe_and_keeps_effects_armed() {
        let _env = crate::diagnostics::test_env::env_guard();
        let dir = temp_probe_dir("empty-path");
        std::fs::create_dir_all(&dir).expect("create empty PATH directory");
        let _path = ScopedPath::set(&dir);
        let effects = LoginEffectGuard::new();

        assert_eq!(
            resolve_bash_for_r2("bash", &probe_winsize(), &effects),
            None
        );
        assert!(!effects.may_have_started());

        std::fs::remove_dir_all(&dir).expect("remove empty PATH directory");
    }

    #[test]
    fn executable_bash_candidate_spawn_failure_keeps_r2_effects_armed() {
        let dir = temp_probe_dir("spawn-failure");
        std::fs::create_dir_all(&dir).expect("create spawn failure directory");
        let candidate = dir.join("bash");
        std::fs::write(&candidate, "#!/definitely/missing/cosh-r2-interpreter\n")
            .expect("write broken executable");
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))
            .expect("make broken executable executable");
        let path = std::env::join_paths([&dir]).expect("spawn failure PATH");
        let inject = capability_inject();
        let effects = LoginEffectGuard::new();

        assert_eq!(
            resolve_bash_in_path(
                "bash",
                &path,
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
                &effects,
            ),
            None
        );
        assert!(!effects.has(LoginEffectSource::R2CapabilityProbe));

        std::fs::remove_dir_all(&dir).expect("remove spawn failure directory");
    }

    #[test]
    fn bash_probe_rejects_wrapper_that_loses_login_argv0() {
        let dir = temp_probe_dir("wrapper");
        std::fs::create_dir_all(&dir).expect("create wrapper directory");
        let wrapper = dir.join("bash");
        std::fs::write(
            &wrapper,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then\n  echo 'GNU bash, version 5.2.0(1)-release'\n  exit 0\nfi\nexec /bin/bash \"$@\"\n",
        )
        .expect("write wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("make wrapper executable");
        let inject = capability_inject();
        let effects = LoginEffectGuard::new();

        assert_eq!(
            probe_bash(
                wrapper.clone(),
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
                &effects,
            ),
            Some((wrapper, false)),
            "a shebang wrapper loses arg0=-bash and must not enable R2"
        );
        assert!(effects.has(LoginEffectSource::R2CapabilityProbe));
        std::fs::remove_dir_all(&dir).expect("remove wrapper directory");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn bash_probe_does_not_run_logout_hooks() {
        let dir = temp_probe_dir("logout");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).expect("create probe home");
        let logout_marker = dir.join("logout-ran");
        std::fs::write(
            home.join(".bash_logout"),
            format!("printf logout > '{}'\n", logout_marker.display()),
        )
        .expect("write logout hook");
        let wrapper = dir.join("bash");
        let probe_hits = home.join(".r2-probe-hits");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/bash\nexport HOME='{}'\nprintf 'r2-hit\\n' >> \"$HOME/.r2-probe-hits\"\nexec -a -bash /bin/bash \"$@\"\n",
                home.display()
            ),
        )
        .expect("write preserving wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("make wrapper executable");
        let effects = LoginEffectGuard::new();

        let (_, supported) = resolve_bash_for_r2(
            wrapper.to_str().expect("UTF-8 wrapper path"),
            &probe_winsize(),
            &effects,
        )
        .expect("probe preserving wrapper");
        assert!(supported, "wrapper preserves the complete R2 contract");
        assert!(effects.has(LoginEffectSource::R2CapabilityProbe));
        assert_eq!(
            std::fs::read_to_string(probe_hits).expect("read probe hits"),
            "r2-hit\n"
        );
        assert!(
            !logout_marker.exists(),
            "capability discovery must not execute the user's logout hook"
        );

        std::fs::remove_dir_all(&dir).expect("remove logout fixture");
    }

    #[test]
    fn bash_probe_is_bounded_for_hang_and_excess_output() {
        let dir = temp_probe_dir("bounds");
        std::fs::create_dir_all(&dir).expect("create bounds directory");
        let hang = dir.join("hang-bash");
        std::fs::write(&hang, "#!/bin/sh\nsleep 3\n").expect("write hang wrapper");
        std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755))
            .expect("make hang executable");
        let inject = capability_inject();
        let hang_effects = LoginEffectGuard::new();

        let started = Instant::now();
        assert_eq!(
            probe_bash(
                hang.clone(),
                inject.path(),
                &probe_winsize(),
                Duration::from_millis(100),
                &hang_effects,
            ),
            Some((hang, false))
        );
        assert!(hang_effects.has(LoginEffectSource::R2CapabilityProbe));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "capability probe must time out before blocking login startup"
        );

        let noisy = dir.join("noisy-bash");
        std::fs::write(&noisy, "#!/bin/sh\nyes x | head -c 2097152\n")
            .expect("write noisy wrapper");
        std::fs::set_permissions(&noisy, std::fs::Permissions::from_mode(0o755))
            .expect("make noisy executable");
        let noisy_effects = LoginEffectGuard::new();
        assert_eq!(
            probe_bash(
                noisy.clone(),
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
                &noisy_effects,
            ),
            Some((noisy, false)),
            "output beyond the startup probe limit must fail closed"
        );
        assert!(noisy_effects.has(LoginEffectSource::R2CapabilityProbe));
        std::fs::remove_dir_all(&dir).expect("remove bounds directory");
    }

    #[test]
    fn bash_probe_uses_final_path_and_execution_result() {
        let dir = temp_probe_dir("path");
        let modern_dir = dir.join("modern");
        let denied_dir = dir.join("denied");
        for path in [&modern_dir, &denied_dir] {
            std::fs::create_dir_all(path).expect("create fake bash directory");
        }
        let write_wrapper = |directory: &Path, target: &Path, mode: u32| {
            let path = directory.join("bash");
            std::fs::write(
                &path,
                format!("#!/bin/sh\nexec '{}' \"$@\"\n", target.display()),
            )
            .expect("write bash wrapper");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
                .expect("set bash wrapper permissions");
            path
        };
        let real_bash = PathBuf::from("/bin/bash");
        let wrapper = write_wrapper(&modern_dir, &real_bash, 0o755);
        write_wrapper(&denied_dir, &real_bash, 0o000);
        let inject = capability_inject();
        let denied_first =
            std::env::join_paths([&denied_dir, &modern_dir]).expect("denied-first PATH");
        let path_effects = LoginEffectGuard::new();
        assert_eq!(
            resolve_bash_in_path(
                "bash",
                &denied_first,
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
                &path_effects,
            ),
            Some((wrapper, false)),
            "EACCES is skipped; the wrapper runs but loses login argv0"
        );
        assert!(path_effects.has(LoginEffectSource::R2CapabilityProbe));

        #[cfg(target_os = "linux")]
        {
            let non_utf8_dir = dir.join(std::ffi::OsString::from_vec(vec![b'n', 0xff]));
            std::fs::create_dir_all(&non_utf8_dir).expect("create non-UTF-8 directory");
            let non_utf8 = non_utf8_dir.join("bash");
            std::fs::copy(&real_bash, &non_utf8).expect("copy real bash");
            std::fs::set_permissions(&non_utf8, std::fs::Permissions::from_mode(0o755))
                .expect("make copied bash executable");
            let non_utf8_path = std::env::join_paths([&non_utf8_dir]).expect("non-UTF-8 PATH");
            let non_utf8_effects = LoginEffectGuard::new();
            let (resolved, supported) = resolve_bash_in_path(
                "bash",
                &non_utf8_path,
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
                &non_utf8_effects,
            )
            .expect("resolve non-UTF-8 bash");
            assert_eq!(resolved, non_utf8);
            assert!(resolved.to_str().is_none(), "test path must be non-UTF-8");
            assert!(supported, "the real Bash must satisfy the R2 contract");
            assert!(non_utf8_effects.has(LoginEffectSource::R2CapabilityProbe));
        }

        std::fs::remove_dir_all(&dir).expect("remove fake bash tree");
    }

    #[test]
    fn exported_function_names_gate_posix_compatibility() {
        assert!(bash_function_env_key_posix_compatible(OsStr::new("PATH")));
        assert!(bash_function_env_key_posix_compatible(OsStr::new(
            "BASH_FUNC_review_helper%%"
        )));
        assert!(!bash_function_env_key_posix_compatible(OsStr::new(
            "BASH_FUNC_review-helper%%"
        )));
        assert!(!bash_function_env_key_posix_compatible(OsStr::new(
            "BASH_FUNC_9helper%%"
        )));
    }
}
