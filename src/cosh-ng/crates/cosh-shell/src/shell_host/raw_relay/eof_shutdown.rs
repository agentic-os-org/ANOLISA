use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::process::Child;
use std::time::{Duration, Instant};

use nix::libc;

use crate::raw_input::{
    foreground_process_group_for_fds, process_group_exists, signal_process_group_id,
};

pub(super) struct EofShutdown {
    deadline: Instant,
    process_groups: Vec<i32>,
    kill_sent: bool,
}

impl EofShutdown {
    pub(super) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
}

pub(super) fn request_eof_shutdown(
    master: &File,
    terminal: &File,
    child: &Child,
    shutdown: &mut Option<EofShutdown>,
) -> io::Result<()> {
    if shutdown.is_some() {
        return Ok(());
    }
    let shell_group = child.id() as i32;
    let mut process_groups =
        foreground_process_group_for_fds(master.as_raw_fd(), terminal.as_raw_fd())
            .into_iter()
            .collect::<Vec<_>>();
    if !process_groups.contains(&shell_group) {
        process_groups.push(shell_group);
    }
    for process_group in &process_groups {
        signal_process_group_id(*process_group, libc::SIGHUP)?;
    }
    *shutdown = Some(EofShutdown {
        deadline: Instant::now() + Duration::from_millis(250),
        process_groups,
        kill_sent: false,
    });
    Ok(())
}

pub(super) fn advance_eof_shutdown(shutdown: &mut Option<EofShutdown>) -> io::Result<bool> {
    let Some(shutdown) = shutdown else {
        return Ok(true);
    };
    if shutdown
        .process_groups
        .iter()
        .all(|process_group| !process_group_exists(*process_group))
    {
        return Ok(true);
    }
    if Instant::now() < shutdown.deadline {
        return Ok(false);
    }
    if !shutdown.kill_sent {
        for process_group in &shutdown.process_groups {
            signal_process_group_id(*process_group, libc::SIGKILL)?;
        }
        shutdown.kill_sent = true;
    }
    Ok(true)
}
