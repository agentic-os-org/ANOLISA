use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};

use nix::libc;

use crate::raw_input::{
    signal_foreground_process_group, write_all_pty, RawInputMode, RawObserverAction,
};
use crate::types::{ImplicitPagerPolicy, ShellHandoffRequest};

use super::super::osc::OscParser;
use super::super::prompt_presentation::PromptPresentation;
use super::super::prompt_replay::{prompt_replay_bytes, PromptReplayTracker};
use super::terminal_recovery::{PendingTerminalRecovery, TerminalRecoveryOwner};
use super::{mark_pending_prompt_replayed, write_pending_display, write_prompt_ghost};

fn write_handoff_request(path: &Path, command: &str) -> io::Result<()> {
    std::fs::write(path, command.as_bytes())
}

/// Sidecar of the pending-handoff request file that tells the marker script to
/// neutralize implicit pagers for this one command.
///
/// The policy travels out of band because the alternative — an assignment
/// prefix on the command line — is echoed verbatim by the interactive shell, so
/// the user would see `PAGER=cat GIT_PAGER=cat … git log` on their prompt line
/// no matter how thoroughly later surfaces strip it.
fn handoff_pager_policy_file(handoff_request_file: &Path) -> std::path::PathBuf {
    let mut path = handoff_request_file.as_os_str().to_os_string();
    path.push(".no-pager");
    std::path::PathBuf::from(path)
}

fn write_handoff_pager_policy(path: &Path, policy: ImplicitPagerPolicy) -> io::Result<()> {
    match policy {
        ImplicitPagerPolicy::Disable => std::fs::write(path, b""),
        // Clear any sidecar a previous handoff left behind, so an inherited
        // policy can never pick up a stale disable.
        ImplicitPagerPolicy::Inherit => match std::fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        },
    }
}

/// Sidecar carrying the one-time claim token (#2142). The marker script reads
/// it from the preexec hook and echoes the value back in the marker JSON, so
/// the OSC parser can claim the command block without relying on the possibly
/// redacted command text.
fn handoff_token_file(handoff_request_file: &Path) -> std::path::PathBuf {
    let mut path = handoff_request_file.as_os_str().to_os_string();
    path.push(".token");
    std::path::PathBuf::from(path)
}

fn write_handoff_token(path: &Path, token: &str) -> io::Result<()> {
    if token.is_empty() {
        // Requests deserialized from before #2142 carry no token; a previous
        // handoff's sidecar must never leak into such a handoff.
        return match std::fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(token.as_bytes())?;
    // `mode` only applies at creation; an existing sidecar keeps its old
    // permissions without this.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Stages the request file and its policy sidecar as one unit. The marker treats
/// the request file as proof that the next command is an approved handoff, so a
/// half-prepared pair must never outlive a failure — including a failure of the
/// very first write, which can leave a previous handoff's files behind.
fn stage_handoff_files(
    handoff_request_file: &Path,
    request: &ShellHandoffRequest,
) -> io::Result<()> {
    match stage_handoff_files_uncleaned(handoff_request_file, request) {
        Ok(()) => Ok(()),
        Err(err) => {
            clear_handoff_files(handoff_request_file);
            Err(err)
        }
    }
}

fn stage_handoff_files_uncleaned(
    handoff_request_file: &Path,
    request: &ShellHandoffRequest,
) -> io::Result<()> {
    write_handoff_request(handoff_request_file, &request.command)?;
    write_handoff_token(&handoff_token_file(handoff_request_file), &request.token)?;
    write_handoff_pager_policy(
        &handoff_pager_policy_file(handoff_request_file),
        request.implicit_pager_policy,
    )
}

fn clear_handoff_files(handoff_request_file: &Path) {
    let _ = std::fs::remove_file(handoff_request_file);
    let _ = std::fs::remove_file(handoff_pager_policy_file(handoff_request_file));
    let _ = std::fs::remove_file(handoff_token_file(handoff_request_file));
}

#[allow(clippy::too_many_arguments)]
pub(super) fn resolve_pty_emit<W: Write>(
    master: &mut File,
    child_pid: u32,
    terminal_fd: i32,
    parser: &mut OscParser,
    output: &mut W,
    input_mode: &Arc<Mutex<RawInputMode>>,
    action: RawObserverAction,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
    pending_terminal_restore: &mut PendingTerminalRecovery,
    recovery_request_file: &Path,
    handoff_request_file: &Path,
    bounded_bash_handoff: bool,
) -> io::Result<RawObserverAction> {
    // #2142 R4: a command-less prompt boundary expired an unclaimed handoff
    // (the runtime closes it as untracked at the same boundary). Remove the
    // staged request/token sidecars so the plaintext command and the claim
    // token cannot outlive the closed handoff and be adopted by a later
    // same-text user command.
    if parser.take_expired_handoff_staging() {
        clear_handoff_files(handoff_request_file);
    }
    match action {
        RawObserverAction::EmitToPty(request) => {
            emit_to_pty(
                master,
                terminal_fd,
                parser,
                output,
                request,
                display_start,
                prompt_replay,
                prompt_presentation,
                pending_terminal_restore,
                handoff_request_file,
                false,
                bounded_bash_handoff,
            )?;
            Ok(RawObserverAction::RawPassthrough)
        }
        RawObserverAction::EmitToPtyWithPromptRestore(request) => {
            emit_to_pty(
                master,
                terminal_fd,
                parser,
                output,
                request,
                display_start,
                prompt_replay,
                prompt_presentation,
                pending_terminal_restore,
                handoff_request_file,
                true,
                bounded_bash_handoff,
            )?;
            Ok(RawObserverAction::RawPassthrough)
        }
        RawObserverAction::InterruptForeground => {
            output.flush()?;
            pending_terminal_restore
                .mark_owner(TerminalRecoveryOwner::CoshTimeoutInterrupt, terminal_fd);
            signal_foreground_process_group(
                master.as_raw_fd(),
                terminal_fd,
                child_pid,
                libc::SIGINT,
            )?;
            pending_terminal_restore.restore_modes(terminal_fd)?;
            pending_terminal_restore.request_shell_recovery(recovery_request_file)?;
            parser.push_control_event("timeout_interrupt");
            Ok(RawObserverAction::Continue)
        }
        RawObserverAction::RestorePrompt {
            ghost_text,
            ghost_route,
        } => {
            output.flush()?;
            let raw_prompt = parser.last_prompt_display();
            let prompt = prompt_replay_bytes(raw_prompt);
            if prompt.is_empty() {
                return Ok(RawObserverAction::RestorePrompt {
                    ghost_text,
                    ghost_route,
                });
            }
            if parser.display_position() > *display_start {
                write_pending_display(
                    parser,
                    output,
                    display_start,
                    prompt_replay,
                    prompt_presentation,
                )?;
            } else {
                prompt_presentation.write_restored_prompt(output, prompt)?;
                mark_pending_prompt_replayed(parser, raw_prompt, display_start)?;
                prompt_replay.arm_for_replay(raw_prompt);
            }
            if let Some(text) = &ghost_text {
                let selection = matches!(
                    ghost_route,
                    crate::raw_input::PromptGhostRoute::AgentSelection { .. }
                );
                if let Ok(mut mode) = input_mode.lock() {
                    *mode = RawInputMode::PromptGhost {
                        text: text.clone(),
                        route: ghost_route,
                    };
                }
                write_prompt_ghost(output, text, selection)?;
            }
            output.flush()?;
            Ok(RawObserverAction::Continue)
        }
        other => Ok(other),
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_to_pty<W: Write>(
    master: &mut File,
    terminal_fd: i32,
    parser: &mut OscParser,
    output: &mut W,
    request: ShellHandoffRequest,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
    pending_terminal_restore: &mut PendingTerminalRecovery,
    handoff_request_file: &Path,
    restore_prompt: bool,
    bounded_bash_handoff: bool,
) -> io::Result<()> {
    output.flush()?;
    if restore_prompt {
        restore_prompt_display_before_handoff(
            parser,
            output,
            display_start,
            prompt_replay,
            prompt_presentation,
        )?;
    }
    let bytes = if bounded_bash_handoff {
        request.bounded_handoff_pty_bytes()
    } else {
        request.pty_bytes()
    }
    .map_err(|message| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("blocked shell handoff: {message}"),
        )
    })?;
    pending_terminal_restore.record_intervention_start(terminal_fd);
    // Must land before the shell sees the command: the marker reads both files
    // from the preexec hook that fires between the newline and the command
    // running.
    stage_handoff_files(handoff_request_file, &request)?;
    if bounded_bash_handoff {
        // Keep the transport wrapper executable by Bash while presenting only
        // the approved command on the user's prompt line.
        let echoed_command = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        // Validation excludes escapes and all controls except a safe tab or
        // quoted line feed, so replacement cannot alter terminal state.
        if let Err(err) =
            parser.arm_pending_handoff_echo(echoed_command, request.command.as_bytes())
        {
            clear_handoff_files(handoff_request_file);
            return Err(err);
        }
    }
    parser.register_pending_handoff_origin(&request);
    if let Err(err) = write_all_pty(master, &bytes) {
        clear_handoff_files(handoff_request_file);
        parser.clear_pending_handoff_origin();
        if bounded_bash_handoff {
            if let Err(flush_err) = parser.flush_pending_handoff_echo() {
                return Err(io::Error::new(
                    err.kind(),
                    format!(
                        "{err}; failed to release pending handoff echo after PTY write error: {flush_err}"
                    ),
                ));
            }
        }
        return Err(err);
    }
    Ok(())
}

pub(super) fn restore_prompt_display_before_handoff<W: Write>(
    parser: &OscParser,
    output: &mut W,
    display_start: &mut usize,
    prompt_replay: &mut PromptReplayTracker,
    prompt_presentation: &mut PromptPresentation,
) -> io::Result<()> {
    if parser.display_position() > *display_start {
        write_pending_display(
            parser,
            output,
            display_start,
            prompt_replay,
            prompt_presentation,
        )?;
        output.flush()?;
        return Ok(());
    }

    let raw_prompt = parser.last_prompt_display();
    let prompt = prompt_replay_bytes(raw_prompt);
    if prompt.is_empty() {
        return Ok(());
    }
    prompt_presentation.write_restored_prompt(output, prompt)?;
    output.flush()?;
    mark_pending_prompt_replayed(parser, raw_prompt, display_start)?;
    prompt_replay.arm_for_replay(raw_prompt);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        clear_handoff_files, handoff_pager_policy_file, handoff_token_file, stage_handoff_files,
        ImplicitPagerPolicy, ShellHandoffRequest,
    };
    use std::path::{Path, PathBuf};

    fn request(policy: ImplicitPagerPolicy) -> ShellHandoffRequest {
        let mut request = ShellHandoffRequest::new(
            "git log",
            "$ git log",
            "approved_provider_shell_tool",
            "user",
            "approval-1",
            "run-1",
            1,
        )
        .expect("handoff request");
        request.implicit_pager_policy = policy;
        request
    }

    fn work_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "cosh-shell-pty-emit-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("work dir");
        path
    }

    #[test]
    fn staging_writes_the_sidecar_only_when_the_policy_disables_pagers() {
        let dir = work_dir("policy");
        let request_file = dir.join("shell-handoff-request");
        let sidecar = handoff_pager_policy_file(&request_file);

        stage_handoff_files(&request_file, &request(ImplicitPagerPolicy::Disable)).expect("staged");
        assert_eq!(
            std::fs::read_to_string(&request_file).expect("request file"),
            "git log"
        );
        assert!(
            sidecar.exists(),
            "disable must announce itself to the marker"
        );

        // A later inherited handoff must not pick up the previous disable.
        stage_handoff_files(&request_file, &request(ImplicitPagerPolicy::Inherit)).expect("staged");
        assert!(request_file.exists());
        assert!(!sidecar.exists(), "stale sidecar must be cleared");
    }

    #[test]
    fn staging_clears_both_files_when_the_request_write_fails() {
        let dir = work_dir("request-failure");
        // A directory in the request file's place makes the first write fail
        // without any of the later steps running.
        let request_file = dir.join("shell-handoff-request");
        std::fs::create_dir(&request_file).expect("blocking directory");
        let sidecar = handoff_pager_policy_file(&request_file);
        std::fs::write(&sidecar, b"").expect("stale sidecar");
        let token_file = handoff_token_file(&request_file);
        std::fs::write(&token_file, b"stale-token").expect("stale token sidecar");

        let error = stage_handoff_files(&request_file, &request(ImplicitPagerPolicy::Disable))
            .expect_err("request write must fail");

        assert!(!sidecar.exists(), "{error}: stale sidecar must be cleared");
        // A failed staging must never leave a previous handoff's token behind:
        // the marker would echo it and the parser would associate the wrong
        // claim with the next handoff.
        assert!(
            !token_file.exists(),
            "{error}: stale token sidecar must be cleared"
        );
    }

    #[test]
    fn clearing_handoff_files_is_quiet_when_nothing_was_staged() {
        let dir = work_dir("absent");
        let request_file = dir.join("shell-handoff-request");

        clear_handoff_files(&request_file);

        assert!(!request_file.exists());
        assert!(!handoff_pager_policy_file(&request_file).exists());
        assert!(!handoff_token_file(&request_file).exists());
    }

    #[test]
    fn staging_writes_the_claim_token_sidecar_with_owner_only_permissions() {
        let dir = work_dir("token");
        let request_file = dir.join("shell-handoff-request");
        let token_file = handoff_token_file(&request_file);
        let request = request(ImplicitPagerPolicy::Inherit);

        stage_handoff_files(&request_file, &request).expect("staged");

        assert_eq!(
            std::fs::read_to_string(&token_file).expect("token sidecar"),
            request.token,
            "the marker reads the claim token from this sidecar"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&token_file)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "token sidecar must be owner-only");
        }

        clear_handoff_files(&request_file);
        assert!(!token_file.exists(), "clear must remove the token sidecar");
    }

    #[test]
    fn staging_a_tokenless_legacy_request_clears_a_stale_token_sidecar() {
        let dir = work_dir("legacy-token");
        let request_file = dir.join("shell-handoff-request");
        let token_file = handoff_token_file(&request_file);
        std::fs::write(&token_file, b"stale-token").expect("stale sidecar");

        let mut legacy = request(ImplicitPagerPolicy::Inherit);
        legacy.token = String::new();
        stage_handoff_files(&request_file, &legacy).expect("staged");

        assert!(
            !token_file.exists(),
            "a pre-#2142 request must not inherit a previous handoff's token"
        );
    }

    #[test]
    fn the_sidecar_sits_next_to_the_request_file_the_marker_already_knows() {
        assert_eq!(
            handoff_pager_policy_file(Path::new("/tmp/work/shell-handoff-request")),
            PathBuf::from("/tmp/work/shell-handoff-request.no-pager"),
            "the marker derives this path from COSH_HANDOFF_REQUEST_FILE"
        );
        assert_eq!(
            handoff_token_file(Path::new("/tmp/work/shell-handoff-request")),
            PathBuf::from("/tmp/work/shell-handoff-request.token"),
            "the marker derives this path from COSH_HANDOFF_REQUEST_FILE"
        );
    }
}
