//! Bootstrap PATH probe tables.
//!
//! Split out of `startup.rs` (as `startup/probes` data) when the login-probe
//! supervision change pushed that file over the 700-line layout threshold.
//! The tables are pure data; the probe-running logic stays in `startup.rs`.

/// A single PATH bootstrap probe: shell flags to invoke, a human-readable
/// source label, and the I/O channel the probe runs on.
#[derive(Clone, Copy)]
pub(super) struct BootstrapPathProbe {
    pub(super) flags: &'static str,
    pub(super) source: &'static str,
    pub(super) io: BootstrapPathProbeIo,
}

/// The I/O channel a bootstrap probe runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BootstrapPathProbeIo {
    Pipes,
    Pty,
}

pub(super) const BASH_NON_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[
    BootstrapPathProbe {
        flags: "-ic",
        source: "Bash interactive startup",
        io: BootstrapPathProbeIo::Pipes,
    },
    BootstrapPathProbe {
        flags: "-lic",
        source: "Bash interactive login startup",
        io: BootstrapPathProbeIo::Pty,
    },
];
// Login startup probes run under the supervised helper (Pty) for the same
// reason the non-login Bash plan's `-lic` probe does: login files may start
// background daemons, and an unsupervised probe leaves them behind as
// unmanaged orphans on every fresh login. The managed shell re-sources the
// same files afterwards, so the probe's copies are duplicates that must be
// reaped, not session-owned processes. The descendant supervisor behind that
// helper only exists on Linux (`ProfileProbeDescendants::enter()` reports
// `Unsupported` on other platforms), so non-Linux login probes keep the
// historical pipes path: a supervised probe there could never import the
// profile PATH and would needlessly record a possible login effect.
#[cfg(target_os = "linux")]
pub(super) const LOGIN_PROBE_IO: BootstrapPathProbeIo = BootstrapPathProbeIo::Pty;
#[cfg(not(target_os = "linux"))]
pub(super) const LOGIN_PROBE_IO: BootstrapPathProbeIo = BootstrapPathProbeIo::Pipes;
pub(super) const BASH_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[BootstrapPathProbe {
    flags: "-lic",
    source: "Bash interactive login startup",
    io: LOGIN_PROBE_IO,
}];
pub(super) const ZSH_NON_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[BootstrapPathProbe {
    flags: "-ic",
    source: "Zsh interactive startup",
    io: BootstrapPathProbeIo::Pipes,
}];
pub(super) const ZSH_LOGIN_PATH_PROBES: &[BootstrapPathProbe] = &[BootstrapPathProbe {
    flags: "-lic",
    source: "Zsh interactive login startup",
    io: LOGIN_PROBE_IO,
}];
