//! Confirm only canonical removal; never read piped data as authorization.

use crate::{output::NotConfirmed, package::signals};
use std::io::{IsTerminal, Write};

pub(super) fn check(
    options: &super::package::Uninstall,
) -> crate::cli::Result<Option<NotConfirmed>> {
    if options.dry_run || options.yes {
        return Ok(None);
    }
    if options.prefix.output.json
        || !std::io::stdin().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        return Ok(Some(NotConfirmed::Required));
    }
    // Use the mutation handler through the transition to execution. A signal
    // between accepting the answer and the first library check must not vanish.
    signals::register()?;
    crate::output::removal_prompt(&options.prefix.prefix)?;
    let answer = answer()?;
    if let Some(signal) = signals::interrupted() {
        writeln!(std::io::stderr().lock())?;
        return Ok(Some(NotConfirmed::Interrupted(signal)));
    }
    if answer.as_deref().is_some_and(|answer| {
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    }) {
        Ok(None)
    } else {
        Ok(Some(NotConfirmed::Declined))
    }
}

fn answer() -> std::io::Result<Option<String>> {
    let mut answer = Vec::new();
    loop {
        if signals::interrupted().is_some() {
            return Ok(None);
        }
        let mut poll = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll points to one initialized descriptor and never changes it.
        let ready = unsafe { libc::poll(&mut poll, 1, 100) };
        if ready == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if ready == 0 {
            continue;
        }
        let mut byte = 0_u8;
        // SAFETY: byte is writable for exactly one byte; stdin is borrowed only.
        let count = unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) };
        if count == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        if count == 0 {
            return Ok(None);
        }
        if byte == b'\n' || byte == b'\r' {
            return Ok(String::from_utf8(answer)
                .ok()
                .map(|answer| answer.trim().into()));
        }
        if answer.len() >= 32 {
            return Ok(None);
        }
        answer.push(byte);
    }
}
