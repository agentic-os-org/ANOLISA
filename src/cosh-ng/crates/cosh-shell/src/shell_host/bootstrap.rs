use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime};

use nix::libc;
use nix::pty::{openpty, Winsize};

use crate::raw_input::ZshPathPromptBuffering;

use super::adapter::{BashAdapter, ShellAdapter, ZshAdapter};
use super::auth::{generate_marker_token, marker_script_with_token};
use super::lifecycle::push_shell_started_event;
use super::login_effect::LoginEffectSource;
use super::model::ShellHostConfig;
use super::osc::OscParser;

const OUTPUT_REF_RETENTION: Duration = Duration::from_secs(24 * 60 * 60);
const ISOLATED_INPUTRC: &str = "set input-meta on\nset convert-meta off\nset output-meta on\n";
const ASSISTANCE_STATE_FILENAME: &str = "assistance-enabled";

pub(crate) fn assistance_state_file(config: &ShellHostConfig) -> PathBuf {
    config.work_dir.join(ASSISTANCE_STATE_FILENAME)
}

/// #R2: build the `$ENV` inject body. Order matters:
/// 1. `set +o posix` so the login session leaves posix mode *before* the marker
///    runs, matching `bash -l` `source foo` / startup semantics (checklist B2/B3).
/// 2. Undo posix-startup side effects using the final child environment captured
///    before R2 overrides it:
///    - `inherit_errexit`: disable the option unless the caller's exported
///      `BASHOPTS` already requested it. `set +o posix` alone leaves it enabled.
///    - `HOME`: when absent, recover Bash's non-posix login default through the
///      shell's own tilde lookup without exporting it; preserve explicit empty or
///      custom values and their inherited export attribute. Suspend `allexport`
///      around this and the `HISTFILE` default so their native attributes survive.
///    - `HISTFILE`: restore an explicit inherited/overridden value; when absent,
///      use the non-posix login default `~/.bash_history` instead of posix's
///      `~/.sh_history`. A replayed profile may still override it afterward.
///    - `ENV`: restore the caller's value (or unset it) so `$ENV` no longer
///      points at this bash-only marker. Otherwise an interactive POSIX subshell
///      (`sh -i`, `dash -i`, `bash --posix -i`) would re-source the marker.
/// 3. The marker verbatim (its profile replay may re-set these values).
///
/// The write is separately fail-closed: `start_shell_session` uses `fs::write`
/// with `?`, which write_all's the whole body or errors, so a short/ENOSPC
/// write aborts session creation rather than launching a marker-less posix
/// shell (checklist E9 / N-9).
fn login_inject_body(marker_script: &str) -> String {
    // Bash has already opened the file by the time this executes. Remove the
    // indirection variable immediately so it never leaks into the session.
    let clear_inject_path = "unset COSH_LOGIN_INJECT\n";
    let restore_inherit_errexit = "case :${COSH_PRIOR_BASHOPTS-}: in \
         *:inherit_errexit:*) ;; *) shopt -u inherit_errexit 2>/dev/null || true ;; esac\n\
         unset COSH_PRIOR_BASHOPTS\n";
    let suspend_allexport = "unset COSH_RESTORE_ALLEXPORT\n\
         case $- in *a*) set +a; COSH_RESTORE_ALLEXPORT=1 ;; esac\n";
    let restore_home = "if [ -z \"${HOME+x}\" ]; then HOME=~; fi\n";
    let restore_histfile = "if [ -n \"${COSH_PRIOR_HISTFILE+x}\" ]; then \
         HISTFILE=\"$COSH_PRIOR_HISTFILE\"; else HISTFILE=\"$HOME/.bash_history\"; fi\n\
         unset COSH_PRIOR_HISTFILE\n";
    let restore_allexport = "if [ -n \"${COSH_RESTORE_ALLEXPORT+x}\" ]; then \
         set -a; unset COSH_RESTORE_ALLEXPORT; fi\n";
    let restore_env = "if [ -n \"${COSH_PRIOR_ENV+x}\" ]; then export ENV=\"$COSH_PRIOR_ENV\"; \
         else unset ENV; fi\nunset COSH_PRIOR_ENV\n";
    format!(
        "set +o posix\n{clear_inject_path}{restore_inherit_errexit}{suspend_allexport}{restore_home}{restore_histfile}{restore_allexport}{restore_env}{marker_script}"
    )
}

/// Compute the value a child would inherit after applying env_overrides in
/// order. Later overrides win; if none target the key, preserve the process
/// environment including non-UTF-8 values.
fn final_child_env_value(
    env_overrides: &[(String, String)],
    key: &str,
    inherited: Option<&OsStr>,
) -> Option<OsString> {
    env_overrides
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| OsString::from(value))
        .or_else(|| inherited.map(OsString::from))
}

pub(super) struct PtySession {
    pub(super) master: File,
    pub(super) terminal: File,
    pub(super) child: Child,
    pub(super) parser: OscParser,
    pub(super) recovery_request_file: PathBuf,
    pub(super) handoff_request_file: PathBuf,
    pub(super) zsh_path_prompt_buffering: Option<ZshPathPromptBuffering>,
}

pub(super) fn start_bash_session(config: &ShellHostConfig) -> io::Result<PtySession> {
    start_shell_session(config, &BashAdapter)
}

pub(super) fn start_zsh_session(config: &ShellHostConfig) -> io::Result<PtySession> {
    start_shell_session(config, &ZshAdapter)
}

fn start_shell_session(
    config: &ShellHostConfig,
    adapter: &dyn ShellAdapter,
) -> io::Result<PtySession> {
    fs::create_dir_all(&config.work_dir)?;
    fs::set_permissions(&config.work_dir, fs::Permissions::from_mode(0o700))?;
    let output_ref_dir = config.work_dir.join("output-refs");
    fs::create_dir_all(&output_ref_dir)?;
    fs::set_permissions(&output_ref_dir, fs::Permissions::from_mode(0o700))?;
    cleanup_expired_output_refs(&output_ref_dir, OUTPUT_REF_RETENTION)?;
    let recovery_request_file = config.work_dir.join("terminal-recovery-request");
    let handoff_request_file = config.work_dir.join("shell-handoff-request");
    // #R2: when the adapter runs a real login identity (Bash `argv0="-bash"
    // --posix`), the marker is delivered via `$ENV=<inject>` rather than
    // `--rcfile`. Filled inside the marker block below.
    let mut login_inject_path: Option<PathBuf> = None;
    let marker = if config.integration.uses_markers() {
        let rcfile = config.work_dir.join(adapter.marker_filename());
        let assistance_state_file = assistance_state_file(config);
        let history_file_state = config
            .work_dir
            .join("terminal-recovery-request.history-file");
        fs::write(&assistance_state_file, b"enabled\n")?;
        fs::set_permissions(&assistance_state_file, fs::Permissions::from_mode(0o600))?;
        fs::write(&history_file_state, b"")?;
        fs::set_permissions(&history_file_state, fs::Permissions::from_mode(0o600))?;
        let marker_token = generate_marker_token();
        let recovery_request_file_str = recovery_request_file.to_string_lossy().to_string();
        let handoff_request_file_str = handoff_request_file.to_string_lossy().to_string();
        let marker_script = marker_script_with_token(
            adapter.marker_script(),
            &marker_token,
            &recovery_request_file_str,
            &handoff_request_file_str,
            &assistance_state_file.to_string_lossy(),
            config.input_classifier.ai_enabled(),
        );
        fs::write(&rcfile, &marker_script)?;
        fs::set_permissions(&rcfile, fs::Permissions::from_mode(0o600))?;
        if adapter.uses_login_identity_inject(config) {
            // #R2: deliver the marker via $ENV (not --rcfile). 0600; body layout
            // and fail-closed rationale live in login_inject_body.
            let inject = config.work_dir.join("cosh-login-inject.bash");
            fs::write(&inject, login_inject_body(&marker_script))?;
            fs::set_permissions(&inject, fs::Permissions::from_mode(0o600))?;
            login_inject_path = Some(inject);
        }
        Some((rcfile, marker_token))
    } else {
        None
    };
    let isolated_inputrc = if config.native_mode || !adapter.isolates_readline() {
        None
    } else {
        let path = config.work_dir.join("inputrc");
        fs::write(&path, ISOLATED_INPUTRC)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        Some(path)
    };
    let zsh_path_prompt_buffering = (marker.is_some()
        && adapter.supports_zsh_path_prompt_buffering())
    .then(ZshPathPromptBuffering::new);

    let (master, slave) = open_pty_pair(Some(&config.winsize))?;
    set_close_on_exec(master.as_raw_fd())?;
    set_nonblocking(master.as_raw_fd())?;

    set_interactive_terminal_baseline(slave.as_raw_fd())?;
    let terminal = slave.try_clone()?;
    let stdin = slave.try_clone()?;
    let stdout = slave.try_clone()?;
    set_close_on_exec(slave.as_raw_fd())?;
    set_close_on_exec(terminal.as_raw_fd())?;
    set_close_on_exec(stdin.as_raw_fd())?;
    set_close_on_exec(stdout.as_raw_fd())?;

    let mut command = Command::new(adapter.executable(config));
    adapter.configure_command(
        &mut command,
        marker.as_ref().map(|(path, _)| path.as_path()),
        config,
    );
    command
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(slave));
    if config.integration.uses_markers() {
        command.env("COSH_SESSION_ID", &config.session_id);
    }
    if config.native_mode {
        command.env_remove("COSH_SHELL_ISOLATED");
    } else {
        command
            .env("COSH_HISTFILE", config.work_dir.join("history"))
            .env("COSH_POC_PS1", &config.prompt)
            .env("BASH_SILENCE_DEPRECATION_WARNING", "1")
            .env("COSH_SHELL_ISOLATED", "1");
    }
    for (key, value) in &config.env_overrides {
        command.env(key, value);
    }
    // #R2: deliver the marker through `$ENV` after env_overrides so a stray
    // override cannot clobber marker delivery. Capture the values the child
    // would otherwise receive (last override wins, then inherited environment)
    // so the inject can restore ENV/HISTFILE without guessing set-ness.
    if let Some(inject_path) = &login_inject_path {
        for (key, prior_key) in [
            ("ENV", "COSH_PRIOR_ENV"),
            ("HISTFILE", "COSH_PRIOR_HISTFILE"),
            ("BASHOPTS", "COSH_PRIOR_BASHOPTS"),
        ] {
            let inherited = std::env::var_os(key);
            match final_child_env_value(&config.env_overrides, key, inherited.as_deref()) {
                Some(value) => {
                    command.env(prior_key, value);
                }
                None => {
                    command.env_remove(prior_key);
                }
            }
        }
        // Bash expands the value of ENV before opening it. Point ENV at an
        // indirect variable so literal `$`, backticks, or other expansion
        // characters in the real temp path are data, not shell syntax. Bash
        // performs one expansion pass, so the resulting path is not re-expanded.
        command
            .env("COSH_LOGIN_INJECT", inject_path)
            .env("ENV", "${COSH_LOGIN_INJECT}");
    }
    if let Some(inputrc) = isolated_inputrc {
        command.env("INPUTRC", inputrc);
    }

    unsafe {
        command.pre_exec(|| {
            // The inner user shell must observe the SIGPIPE disposition the
            // host process inherited, not the Rust runtime's rewrite.
            super::sigpipe::restore_in_child()?;
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            set_interactive_terminal_baseline(0)?;
            if libc::tcsetpgrp(0, libc::getpgrp()) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut parser = match marker {
        Some((_, marker_token)) => OscParser::with_retention(
            config.session_id.clone(),
            output_ref_dir,
            marker_token,
            config.transcript_retention,
            &config.work_dir,
        )?,
        None => OscParser::passthrough_with_retention(
            config.session_id.clone(),
            output_ref_dir,
            config.transcript_retention,
            &config.work_dir,
        )?,
    };
    if let Some(observer) = config.shell_environment_observer.clone() {
        parser = parser.with_environment_observer(observer);
    }
    if let Some(observer) = config.shell_history_file_observer.clone() {
        parser = parser.with_history_file_observer(observer);
    }

    // Build all fallible session-owned storage before spawning the shell so
    // an unwritable spool cannot leave an unmanaged child process behind.
    let child = command.spawn()?;
    config
        .login_effect_guard()
        .mark_possible(LoginEffectSource::ManagedShell);
    push_shell_started_event(&mut parser, config);

    Ok(PtySession {
        master,
        terminal,
        child,
        parser,
        recovery_request_file,
        handoff_request_file,
        zsh_path_prompt_buffering,
    })
}

pub(crate) fn spawn_profile_probe_on_pty(
    mut command: Command,
    winsize: &Winsize,
) -> io::Result<(Child, File)> {
    let (master, slave) = open_pty_pair(Some(winsize))?;
    set_close_on_exec(master.as_raw_fd())?;
    set_interactive_terminal_baseline(slave.as_raw_fd())?;
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    unsafe {
        command.pre_exec(|| {
            super::sigpipe::restore_in_child()?;
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            set_interactive_terminal_baseline(0)?;
            if libc::tcsetpgrp(0, libc::getpgrp()) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    drop(command);
    Ok((child, master))
}

fn open_pty_pair(winsize: Option<&Winsize>) -> io::Result<(File, File)> {
    let pty = openpty(winsize, None).map_err(nix_to_io)?;
    let master = unsafe { File::from_raw_fd(pty.master.into_raw_fd()) };
    let slave = unsafe { File::from_raw_fd(pty.slave.into_raw_fd()) };
    Ok((master, slave))
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_close_on_exec(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_interactive_terminal_baseline(fd: i32) -> io::Result<()> {
    let mut termios = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(fd, &mut termios) } < 0 {
        return Err(io::Error::last_os_error());
    }

    termios.c_lflag |= libc::ECHO | libc::ECHOE | libc::ECHOK | libc::ICANON | libc::ISIG;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        termios.c_lflag |= libc::IEXTEN;
    }
    termios.c_iflag |= libc::ICRNL | libc::IXON | libc::BRKINT;
    termios.c_iflag &= !(libc::IGNBRK | libc::INLCR | libc::IGNCR | libc::ISTRIP);
    termios.c_oflag |= libc::OPOST;
    #[cfg(any(target_os = "linux", target_os = "android", target_os = "macos"))]
    {
        termios.c_oflag |= libc::ONLCR;
    }
    set_control_char(&mut termios, libc::VINTR, 0x03);
    set_control_char(&mut termios, libc::VQUIT, 0x1c);
    set_control_char(&mut termios, libc::VERASE, 0x7f);
    set_control_char(&mut termios, libc::VKILL, 0x15);
    set_control_char(&mut termios, libc::VEOF, 0x04);
    set_control_char(&mut termios, libc::VEOL, 0x00);
    set_control_char(&mut termios, libc::VMIN, 0x01);
    set_control_char(&mut termios, libc::VTIME, 0x00);
    set_control_char(&mut termios, libc::VSUSP, 0x1a);
    set_control_char(&mut termios, libc::VSTART, 0x11);
    set_control_char(&mut termios, libc::VSTOP, 0x13);

    if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &termios) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_control_char(termios: &mut libc::termios, index: usize, value: u8) {
    if index < termios.c_cc.len() {
        termios.c_cc[index] = value as libc::cc_t;
    }
}

fn nix_to_io(err: nix::Error) -> io::Error {
    io::Error::other(err)
}

fn cleanup_expired_output_refs(dir: &Path, retention: Duration) -> io::Result<()> {
    cleanup_expired_output_refs_at(dir, retention, SystemTime::now())
}

fn cleanup_expired_output_refs_at(
    dir: &Path,
    retention: Duration,
    now: SystemTime,
) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };

    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_file() {
            continue;
        }
        let Ok(modified) = entry.metadata().and_then(|metadata| metadata.modified()) else {
            continue;
        };
        let expired = match now.duration_since(modified) {
            Ok(age) => age > retention,
            Err(_) => false,
        };
        if expired {
            let _ = fs::remove_file(entry.path());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::login_effect::LoginEffectSource;
    use super::*;

    #[test]
    fn failed_shell_spawn_keeps_login_effects_armed() {
        let dir = tempfile::tempdir().expect("create shell work directory");
        let mut config = ShellHostConfig::new("spawn-failure", dir.path());
        let effects = config.login_effect_guard();
        config.integration = crate::shell_host::ShellIntegration::Native;
        config.native_mode = false;
        config.bash_path = "/definitely/missing/cosh-shell".to_string();

        let result = start_bash_session(&config);

        assert!(result.is_err());
        assert!(!effects.may_have_started());
    }

    #[test]
    fn successful_shell_spawn_marks_managed_shell_effect() {
        let dir = tempfile::tempdir().expect("create shell work directory");
        let mut config = ShellHostConfig::new("spawn-success", dir.path());
        let effects = config.login_effect_guard();
        config.integration = crate::shell_host::ShellIntegration::Native;
        config.native_mode = false;
        config.bash_path = "/bin/bash".to_string();

        let mut session = start_bash_session(&config).expect("spawn managed shell");

        assert!(effects.has(LoginEffectSource::ManagedShell));
        let _ = session.child.kill();
        let _ = session.child.wait();
    }

    #[test]
    fn login_inject_body_clears_posix_restores_env_then_marker() {
        // S2 + environment hygiene: inject = `set +o posix` (line 1) → restore
        // HOME, HISTFILE, and ENV from child inputs/defaults → marker verbatim.
        // Ordering is load-bearing: all restoration precedes profile replay.
        let marker = "# cosh marker\n_cosh_prompt_ready() { :; }\n";
        let body = login_inject_body(marker);
        assert_eq!(
            body.lines().next(),
            Some("set +o posix"),
            "inject line 1 must be the posix-off directive, got {body:?}"
        );
        // ENV restore/unset sits between posix-off and the marker.
        let posix_end = "set +o posix\n".len();
        let marker_start = body
            .find(marker)
            .expect("marker must be present verbatim in the inject body");
        let restore = &body[posix_end..marker_start];
        assert!(
            restore.contains("unset ENV") && restore.contains("COSH_PRIOR_ENV"),
            "ENV must be restored/unset before the marker, got {restore:?}"
        );
        assert!(
            restore.contains("unset COSH_LOGIN_INJECT"),
            "inject path indirection must be cleared before the marker, got {restore:?}"
        );
        assert!(
            restore.contains("COSH_PRIOR_BASHOPTS") && restore.contains("shopt -u inherit_errexit"),
            "inherit_errexit must match the caller before the marker, got {restore:?}"
        );
        let clear_allexport = restore
            .find("unset COSH_RESTORE_ALLEXPORT")
            .expect("missing stale allexport marker cleanup");
        let suspend_allexport = restore.find("set +a").expect("missing allexport suspend");
        let restore_home = restore
            .find("if [ -z \"${HOME+x}\" ]; then HOME=~; fi")
            .expect("missing HOME recovery");
        let restore_histfile = restore
            .find("COSH_PRIOR_HISTFILE")
            .expect("missing HISTFILE recovery");
        let restore_allexport = restore.rfind("set -a").expect("missing allexport restore");
        assert!(
            clear_allexport < suspend_allexport
                && suspend_allexport < restore_home
                && restore_home < restore_histfile
                && restore_histfile < restore_allexport
                && restore.contains("HISTFILE=\"$HOME/.bash_history\""),
            "HOME/HISTFILE must recover with allexport suspended, got {restore:?}"
        );
        assert!(
            body.ends_with(marker),
            "marker script must follow verbatim, unaltered"
        );

        let overrides = vec![
            ("ENV".to_string(), "/first".to_string()),
            ("OTHER".to_string(), "ignored".to_string()),
            ("ENV".to_string(), "".to_string()),
        ];
        assert_eq!(
            final_child_env_value(&overrides, "ENV", Some(OsStr::new("/inherited"))),
            Some(OsString::new()),
            "last override wins, including an explicit empty value"
        );
        assert_eq!(
            final_child_env_value(&[], "ENV", Some(OsStr::new("/inherited"))),
            Some(OsString::from("/inherited"))
        );
        assert_eq!(final_child_env_value(&[], "ENV", None), None);
    }

    #[test]
    fn cleanup_expired_output_refs_removes_old_files_only() {
        let dir = std::env::temp_dir().join(format!(
            "cosh-shell-output-ref-cleanup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create dir");
        let stale = dir.join("cmd-old.txt");
        let subdir = dir.join("nested");
        fs::write(&stale, "old\n").expect("write stale");
        fs::create_dir_all(&subdir).expect("create subdir");

        cleanup_expired_output_refs_at(
            &dir,
            Duration::ZERO,
            SystemTime::now() + Duration::from_secs(1),
        )
        .expect("cleanup");

        assert!(!stale.exists(), "stale output ref should be removed");
        assert!(subdir.exists(), "cleanup must not remove directories");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_expired_output_refs_keeps_recent_files() {
        let dir = std::env::temp_dir().join(format!(
            "cosh-shell-output-ref-retain-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create dir");
        let recent = dir.join("cmd-recent.txt");
        fs::write(&recent, "recent\n").expect("write recent");

        cleanup_expired_output_refs_at(&dir, Duration::from_secs(60 * 60), SystemTime::now())
            .expect("cleanup");

        assert!(recent.exists(), "recent output ref should be retained");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn close_on_exec_sets_fd_flag() {
        let pty = openpty(None, None).expect("open pty");
        let fd = pty.master.as_raw_fd();

        set_close_on_exec(fd).expect("set close on exec");

        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
    }
}
