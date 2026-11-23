//! Wake maintenance for owner exits or deadlines while bounding external cancellation.

use crate::{runtime::Runtime, Result};
use std::{io, os::fd::AsRawFd, time::Duration, time::Instant};

const CANCELLATION_INTERVAL: Duration = Duration::from_millis(5);

/// Wait for socket activity, an owner exit or the nearest maintenance deadline.
pub(super) fn idle(listener: i32, runtime: &Runtime) -> Result<()> {
    let maintenance = runtime.maintenance()?;
    let mut descriptors = vec![libc::pollfd {
        fd: listener,
        events: libc::POLLIN,
        revents: 0,
    }];
    descriptors.extend(maintenance.exits.iter().map(|exit| libc::pollfd {
        fd: exit.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }));
    let remaining = maintenance
        .deadline
        .map_or(CANCELLATION_INTERVAL, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .min(CANCELLATION_INTERVAL)
        });
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: remaining.as_nanos() as _,
    };
    // SAFETY: owned exit FDs and listener remain live; ppoll only writes this array.
    let ready = unsafe {
        libc::ppoll(
            descriptors.as_mut_ptr(),
            descriptors.len() as _,
            &timeout,
            std::ptr::null(),
        )
    };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error.into());
        }
    } else if descriptors[0].revents != 0 && descriptors[0].revents & libc::POLLIN == 0 {
        return Err(io::Error::other(format!(
            "listener poll returned unexpected events: {}",
            descriptors[0].revents
        ))
        .into());
    }
    Ok(())
}
