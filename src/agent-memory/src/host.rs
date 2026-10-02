//! Host identity, captured before the process switches namespaces.
//!
//! `main` enters the user namespace before it constructs anything else, and
//! the default unprivileged mapping is `0 <real uid> 1`. From that point on
//! `geteuid()` reports 0 for *every* user on the box, so anything that has
//! to tell one host user from another — a per-user directory name, say —
//! must use the uid recorded here rather than the one the kernel reports
//! now.
//!
//! The converse matters just as much: anything that has to match filesystem
//! metadata (`st_uid`) must keep using the *current* uid, because that is
//! the namespace the metadata is reported in. Inside our own user
//! namespace a directory we own on `/tmp` shows up as uid 0, and one a
//! neighbour owns shows up as the overflow uid — comparing either of those
//! against the host uid would be wrong.

use std::sync::OnceLock;

/// The uid this process was launched with. Set once, before any `unshare`.
static HOST_UID: OnceLock<u32> = OnceLock::new();

/// Record the calling uid while it is still the host uid.
///
/// Idempotent — the first call wins — so it is safe (and intended) to call
/// it both from `main`, before `early_enter_userns`, and from
/// `LinuxUserNsMount::enter`, before the `unshare` itself.
pub fn capture_host_uid() -> u32 {
    *HOST_UID.get_or_init(|| nix::unistd::Uid::current().as_raw())
}

/// The uid this process was launched with, even after `unshare(CLONE_NEWUSER)`.
///
/// When nothing was captured — a library consumer that built `MemoryService`
/// without going through `main` — the answer is recovered from
/// `/proc/self/uid_map` instead, so a namespaced process still gets a
/// per-host-user value rather than the 0 every one of its neighbours shares.
pub fn host_uid() -> u32 {
    if let Some(uid) = HOST_UID.get() {
        return *uid;
    }
    match std::fs::read_to_string("/proc/self/uid_map") {
        Ok(map) => host_uid_from_uid_map(nix::unistd::Uid::current().as_raw(), &map)
            .unwrap_or_else(capture_host_uid),
        Err(_) => capture_host_uid(),
    }
}

/// Map `current`, a uid in this process's user namespace, back to the host
/// uid using the contents of `/proc/self/uid_map`.
///
/// Each line is `<inside> <outside> <count>`. Returns `None` when `current`
/// is not covered by any mapping (an unmapped uid, reported by the kernel as
/// the overflow id), in which case there is no host uid to recover.
fn host_uid_from_uid_map(current: u32, uid_map: &str) -> Option<u32> {
    uid_map.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let inside = fields.next()?.parse::<u32>().ok()?;
        let outside = fields.next()?.parse::<u32>().ok()?;
        let count = fields.next()?.parse::<u32>().ok()?;
        let offset = current.checked_sub(inside)?;
        (offset < count).then_some(outside + offset)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_the_host_uid_from_a_single_id_mapping() {
        // `LinuxUserNsMount::enter` writes exactly this: inside-0 is the
        // launching user, and every other host uid is unmapped.
        let map = "         0       1000          1\n";
        assert_eq!(host_uid_from_uid_map(0, map), Some(1000));
        assert_eq!(
            host_uid_from_uid_map(1, map),
            None,
            "an unmapped uid has no host uid"
        );
    }

    #[test]
    fn is_the_identity_outside_a_user_namespace() {
        let map = "         0          0 4294967295\n";
        assert_eq!(host_uid_from_uid_map(1000, map), Some(1000));
        assert_eq!(host_uid_from_uid_map(0, map), Some(0));
    }

    #[test]
    fn handles_a_multi_range_map_and_ignores_junk() {
        let map = "garbage\n0 1000 10\n10 2000 5\n";
        assert_eq!(host_uid_from_uid_map(0, map), Some(1000));
        assert_eq!(host_uid_from_uid_map(9, map), Some(1009));
        assert_eq!(host_uid_from_uid_map(12, map), Some(2002));
        assert_eq!(host_uid_from_uid_map(15, map), None);
    }

    #[test]
    fn host_uid_never_reports_the_namespace_uid_after_capture() {
        // Whatever namespace we happen to be in, the captured value is the
        // one callers get; this is the property the tmp-dir suffix relies on.
        let captured = capture_host_uid();
        assert_eq!(host_uid(), captured);
    }
}
