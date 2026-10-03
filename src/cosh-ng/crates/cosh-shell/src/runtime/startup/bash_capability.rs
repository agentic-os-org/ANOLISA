use std::ffi::OsStr;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nix::pty::Winsize;
use tempfile::NamedTempFile;

use super::{descendants::run_supervised_profile_probe, BootstrapPathProbeError};

const R2_BASH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const R2_BASH_PROBE_SENTINEL: &str = "__COSH_R2_LOGIN_ENV_OK__";

fn capability_inject_script() -> String {
    format!(
        "set +o posix\nif shopt -q login_shell && [[ ! -o posix ]]; then printf '\\n__COSH_PATH_BEGIN__{R2_BASH_PROBE_SENTINEL}__COSH_PATH_END__\\n'; fi\nexec /bin/sh -c :\n"
    )
}

/// Resolve and probe the exact Bash that execvp would select after PATH
/// bootstrap. Candidates that cannot execute (including EACCES) are skipped;
/// the first process that starts is frozen for the eventual spawn. The probe
/// exercises the actual R2 contract (`argv0=-bash`, `--posix -i`, `$ENV`
/// startup and `set +o posix`) under the descendant-owning profile-probe
/// supervisor. It replaces the shell process instead of exiting, so discovery
/// cannot run `.bash_logout`. Any incomplete contract fails closed to `--rcfile`.
pub(crate) fn resolve_bash_for_r2(program: &str, winsize: &Winsize) -> Option<(PathBuf, bool)> {
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
        );
    }
    let path = std::env::var_os("PATH")?;
    resolve_bash_in_path(
        program,
        &path,
        inject.path(),
        winsize,
        R2_BASH_PROBE_TIMEOUT,
    )
}

fn resolve_bash_in_path(
    program: &str,
    path: &OsStr,
    inject_path: &Path,
    winsize: &Winsize,
    timeout: Duration,
) -> Option<(PathBuf, bool)> {
    std::env::split_paths(path)
        .map(|directory| directory.join(program))
        .find_map(|candidate| probe_bash(candidate, inject_path, winsize, timeout))
}

fn probe_bash(
    path: PathBuf,
    inject_path: &Path,
    winsize: &Winsize,
    timeout: Duration,
) -> Option<(PathBuf, bool)> {
    let mut command = Command::new(&path);
    command
        .args(["--posix", "-i"])
        .env("COSH_R2_CAPABILITY_INJECT", inject_path)
        .env("ENV", "${COSH_R2_CAPABILITY_INJECT}")
        .env("LC_ALL", "C");

    let supported =
        match run_supervised_profile_probe(command, Some(OsStr::new("-bash")), timeout, winsize) {
            Ok(marker) => marker == R2_BASH_PROBE_SENTINEL,
            Err(
                BootstrapPathProbeError::Containment(_)
                | BootstrapPathProbeError::Supervisor(_)
                | BootstrapPathProbeError::Spawn(_),
            ) => return None,
            Err(_) => false,
        };
    Some((path, supported))
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

        assert_eq!(
            probe_bash(
                wrapper.clone(),
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
            ),
            Some((wrapper, false)),
            "a shebang wrapper loses arg0=-bash and must not enable R2"
        );
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
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/bash\nexport HOME='{}'\nexec -a -bash /bin/bash \"$@\"\n",
                home.display()
            ),
        )
        .expect("write preserving wrapper");
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
            .expect("make wrapper executable");

        let (_, supported) = resolve_bash_for_r2(
            wrapper.to_str().expect("UTF-8 wrapper path"),
            &probe_winsize(),
        )
        .expect("probe preserving wrapper");
        assert!(supported, "wrapper preserves the complete R2 contract");
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

        let started = Instant::now();
        assert_eq!(
            probe_bash(
                hang.clone(),
                inject.path(),
                &probe_winsize(),
                Duration::from_millis(100),
            ),
            Some((hang, false))
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "capability probe must time out before blocking login startup"
        );

        let noisy = dir.join("noisy-bash");
        std::fs::write(&noisy, "#!/bin/sh\nyes x | head -c 2097152\n")
            .expect("write noisy wrapper");
        std::fs::set_permissions(&noisy, std::fs::Permissions::from_mode(0o755))
            .expect("make noisy executable");
        assert_eq!(
            probe_bash(
                noisy.clone(),
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
            ),
            Some((noisy, false)),
            "output beyond the startup probe limit must fail closed"
        );
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
        assert_eq!(
            resolve_bash_in_path(
                "bash",
                &denied_first,
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
            ),
            Some((wrapper, false)),
            "EACCES is skipped; the wrapper runs but loses login argv0"
        );

        #[cfg(target_os = "linux")]
        {
            let non_utf8_dir = dir.join(std::ffi::OsString::from_vec(vec![b'n', 0xff]));
            std::fs::create_dir_all(&non_utf8_dir).expect("create non-UTF-8 directory");
            let non_utf8 = non_utf8_dir.join("bash");
            std::fs::copy(&real_bash, &non_utf8).expect("copy real bash");
            std::fs::set_permissions(&non_utf8, std::fs::Permissions::from_mode(0o755))
                .expect("make copied bash executable");
            let non_utf8_path = std::env::join_paths([&non_utf8_dir]).expect("non-UTF-8 PATH");
            let (resolved, supported) = resolve_bash_in_path(
                "bash",
                &non_utf8_path,
                inject.path(),
                &probe_winsize(),
                Duration::from_secs(1),
            )
            .expect("resolve non-UTF-8 bash");
            assert_eq!(resolved, non_utf8);
            assert!(resolved.to_str().is_none(), "test path must be non-UTF-8");
            assert!(supported, "the real Bash must satisfy the R2 contract");
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
