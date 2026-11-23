//! Read-only confirmation that a retired worker's process group no longer exists.

use std::io;
use std::thread;
use std::time::{Duration, Instant};

use nix::libc;

use super::process::COMMAND_POLL_INTERVAL;

pub(super) const GROUP_CONFIRM_BUDGET: Duration = Duration::from_millis(10);

pub(super) enum GroupState {
    Gone,
    Present,
    Unknown,
}

pub(super) fn group_state(pgid: u32) -> GroupState {
    let Ok(group) = i32::try_from(pgid) else {
        return GroupState::Unknown;
    };
    if unsafe { libc::kill(-group, 0) } == 0 {
        return GroupState::Present;
    }
    match io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => GroupState::Gone,
        _ => GroupState::Unknown,
    }
}

pub(super) fn confirm_group_gone(pgid: u32, limit: Instant) -> bool {
    loop {
        match group_state(pgid) {
            GroupState::Gone => return true,
            GroupState::Unknown => return false,
            GroupState::Present => {}
        }
        let now = Instant::now();
        if now >= limit {
            return false;
        }
        thread::sleep(COMMAND_POLL_INTERVAL.min(limit.saturating_duration_since(now)));
    }
}

#[cfg(test)]
mod tests {
    use super::super::process::{CleanupScope, CommandChild, ReapOwner};
    use super::*;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    struct OwnedChildJob {
        pgid: u32,
        child: CommandChild<'static>,
    }

    impl OwnedChildJob {
        fn id(&self) -> u32 {
            self.pgid
        }

        fn wait(&mut self) -> io::Result<()> {
            let limit = Instant::now() + Duration::from_millis(200);
            while !self.child.has_exited()? {
                if Instant::now() >= limit {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                thread::sleep(COMMAND_POLL_INTERVAL);
            }
            self.child
                .cleanup()?
                .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))?;
            Ok(())
        }
    }

    fn isolated_child(program: &str, args: &[&str]) -> OwnedChildJob {
        let permit = ReapOwner::global()
            .expect("child owner")
            .reserve()
            .expect("child slot");
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn isolated child");
        OwnedChildJob {
            pgid: child.id(),
            child: permit.register(
                child,
                CleanupScope::OwnedGroup,
                Instant::now() + Duration::from_secs(2),
            ),
        }
    }

    #[test]
    fn own_process_group_is_present_and_not_confirmed_gone() {
        let own = unsafe { libc::getpgrp() } as u32;
        assert!(matches!(group_state(own), GroupState::Present));
        assert!(!confirm_group_gone(
            own,
            Instant::now() + Duration::from_millis(5)
        ));
    }

    #[test]
    fn exited_leader_group_is_confirmed_gone_after_reap() {
        let mut child = isolated_child("/bin/sh", &["-c", "exit 0"]);
        let pgid = child.id();
        child.wait().expect("reap exited leader within budget");
        assert!(confirm_group_gone(
            pgid,
            Instant::now() + Duration::from_millis(50)
        ));
    }

    #[test]
    fn killed_descendant_group_is_confirmed_gone() {
        let mut child = isolated_child("/bin/sh", &["-c", "sleep 30 & sleep 30"]);
        let pgid = child.id();
        assert!(matches!(group_state(pgid), GroupState::Present));
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
        child.wait().expect("reap killed leader within budget");
        assert!(confirm_group_gone(
            pgid,
            Instant::now() + Duration::from_millis(200)
        ));
    }

    #[test]
    fn live_group_is_not_confirmed_gone_within_a_short_limit() {
        let mut child = isolated_child("/bin/sh", &["-c", "sleep 30"]);
        let pgid = child.id();
        assert!(!confirm_group_gone(
            pgid,
            Instant::now() + Duration::from_millis(5)
        ));
        unsafe {
            libc::kill(-(pgid as i32), libc::SIGKILL);
        }
        child.wait().expect("reap killed leader within budget");
    }

    library_tests! {
        #[test]
        fn isolated_child_is_cleaned_up_when_assertion_panics() {
            let mut pgid = 0;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let child = isolated_child("/bin/sleep", &["30"]);
                pgid = child.id();
                panic!("injected assertion failure before explicit cleanup");
            }));
            assert_ne!(
                pgid, 0,
                "isolated child must spawn before the injected panic"
            );
            let cleaned = confirm_group_gone(pgid, Instant::now() + Duration::from_millis(200));
            if !cleaned {
                unsafe { libc::kill(-(pgid as i32), libc::SIGKILL) };
                let limit = Instant::now() + Duration::from_millis(200);
                while unsafe { libc::waitpid(pgid as i32, std::ptr::null_mut(), libc::WNOHANG) } == 0
                    && Instant::now() < limit
                {
                    thread::sleep(COMMAND_POLL_INTERVAL);
                }
            }
            assert!(result.is_err());
            assert!(cleaned, "unwinding must clean up the owned process group");
        }
    }
}
