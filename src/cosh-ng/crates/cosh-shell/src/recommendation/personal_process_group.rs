use std::thread;
use std::time::{Duration, Instant};

const TERM_GRACE: Duration = Duration::from_secs(1);
const KILL_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessStartIdentity {
    #[cfg(target_os = "linux")]
    Linux(u64),
    #[cfg(target_os = "macos")]
    MacOs { seconds: u64, microseconds: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ProcessGroupIdentity {
    pub(crate) owner_pid: u32,
    pub(crate) owner_start_identity: String,
    pub(crate) leader_pid: u32,
    pub(crate) leader_start_identity: String,
    pub(crate) process_group_id: u32,
}

pub(crate) fn process_group_identity_matches(identity: &ProcessGroupIdentity) -> bool {
    if identity.process_group_id != identity.leader_pid
        || process_start_identity_token(identity.owner_pid).as_deref()
            != Some(identity.owner_start_identity.as_str())
        || process_start_identity_token(identity.leader_pid).as_deref()
            != Some(identity.leader_start_identity.as_str())
        || process_parent_id(identity.leader_pid) != Some(identity.owner_pid)
    {
        return false;
    }
    let leader_pid = identity.leader_pid as nix::libc::pid_t;
    let expected_group = identity.process_group_id as nix::libc::pid_t;
    unsafe {
        nix::libc::getpgid(leader_pid) == expected_group
            && nix::libc::getsid(leader_pid) == leader_pid
    }
}

pub(crate) fn verified_terminate_process_group(identity: &ProcessGroupIdentity) -> bool {
    if identity.process_group_id != identity.leader_pid
        || process_start_identity_token(identity.owner_pid).as_deref()
            != Some(identity.owner_start_identity.as_str())
    {
        return false;
    }
    if !group_has_live_members(identity.process_group_id) {
        return true;
    }
    match process_start_identity_token(identity.leader_pid) {
        Some(start_identity) if start_identity == identity.leader_start_identity => {
            if !process_group_identity_matches(identity) {
                return false;
            }
        }
        Some(_) => return false,
        None if process_exists(identity.leader_pid) => return false,
        None => {}
    }
    signal_group(identity.process_group_id, nix::libc::SIGTERM);
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        if !group_has_live_members(identity.process_group_id) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if process_start_identity_token(identity.owner_pid).as_deref()
        != Some(identity.owner_start_identity.as_str())
    {
        return false;
    }
    // A PGID cannot be reused while members of the already-verified group still exist.
    signal_group(identity.process_group_id, nix::libc::SIGKILL);
    let deadline = Instant::now() + KILL_GRACE;
    while Instant::now() < deadline {
        if !group_has_live_members(identity.process_group_id) {
            return true;
        }
        thread::sleep(Duration::from_millis(10));
    }
    !group_has_live_members(identity.process_group_id)
}

pub(crate) fn analyzer_process_is_gone(identity: &ProcessGroupIdentity) -> bool {
    !group_has_live_members(identity.process_group_id)
}

pub(crate) fn process_start_identity_token(process_id: u32) -> Option<String> {
    process_start_identity(process_id).map(|identity| match identity {
        #[cfg(target_os = "linux")]
        ProcessStartIdentity::Linux(ticks) => format!("linux:{ticks}"),
        #[cfg(target_os = "macos")]
        ProcessStartIdentity::MacOs {
            seconds,
            microseconds,
        } => format!("macos:{seconds}:{microseconds}"),
    })
}

#[cfg(target_os = "linux")]
fn process_start_identity(process_id: u32) -> Option<ProcessStartIdentity> {
    let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat")).ok()?;
    let start_ticks = stat
        .get(stat.rfind(')')? + 1..)?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()?;
    Some(ProcessStartIdentity::Linux(start_ticks))
}

#[cfg(target_os = "macos")]
fn process_start_identity(process_id: u32) -> Option<ProcessStartIdentity> {
    let info = process_bsd_info(process_id)?;
    Some(ProcessStartIdentity::MacOs {
        seconds: info.pbi_start_tvsec,
        microseconds: info.pbi_start_tvusec,
    })
}

#[cfg(target_os = "macos")]
fn process_bsd_info(process_id: u32) -> Option<ProcBsdInfo> {
    use std::mem::{size_of, MaybeUninit};

    const PROC_PIDTBSDINFO: i32 = 3;
    let mut info = MaybeUninit::<ProcBsdInfo>::zeroed();
    let size = size_of::<ProcBsdInfo>();
    let read = unsafe {
        proc_pidinfo(
            process_id as i32,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if read != size as i32 {
        return None;
    }
    Some(unsafe { info.assume_init() })
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    pbi_rfu_1: u32,
    pbi_comm: [nix::libc::c_char; 16],
    pbi_name: [nix::libc::c_char; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

#[cfg(target_os = "macos")]
#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidinfo(
        pid: i32,
        flavor: i32,
        arg: u64,
        buffer: *mut nix::libc::c_void,
        buffersize: i32,
    ) -> i32;
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_start_identity(_process_id: u32) -> Option<ProcessStartIdentity> {
    None
}

#[cfg(target_os = "linux")]
fn process_parent_id(process_id: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat")).ok()?;
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let _state = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(target_os = "macos")]
fn process_parent_id(process_id: u32) -> Option<u32> {
    process_bsd_info(process_id).map(|info| info.pbi_ppid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_parent_id(_process_id: u32) -> Option<u32> {
    None
}

fn signal_group(process_group_id: u32, signal: i32) {
    unsafe {
        let _ = nix::libc::kill(-(process_group_id as i32), signal);
    }
}

fn group_exists(process_group_id: u32) -> bool {
    unsafe { nix::libc::kill(-(process_group_id as i32), 0) == 0 }
}

fn process_exists(process_id: u32) -> bool {
    let result = unsafe { nix::libc::kill(process_id as i32, 0) };
    result == 0 || io_error_is_permission_denied()
}

fn io_error_is_permission_denied() -> bool {
    std::io::Error::last_os_error().raw_os_error() == Some(nix::libc::EPERM)
}

#[cfg(target_os = "linux")]
pub(super) fn group_has_live_members(process_group_id: u32) -> bool {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return group_exists(process_group_id);
    };
    for entry in entries.flatten() {
        let Some(process_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{process_id}/stat")) else {
            continue;
        };
        let Some(fields) = stat.get(stat.rfind(')').unwrap_or(0) + 1..) else {
            continue;
        };
        let mut fields = fields.split_whitespace();
        let (Some(state), Some(_parent), Some(group)) =
            (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        if group.parse::<u32>().ok() == Some(process_group_id) && !matches!(state, "Z" | "X") {
            return true;
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
pub(super) fn group_has_live_members(process_group_id: u32) -> bool {
    group_exists(process_group_id)
}
