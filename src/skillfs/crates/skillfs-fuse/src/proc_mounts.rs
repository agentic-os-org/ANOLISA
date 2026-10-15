//! Decoding of `/proc/mounts` path fields.
//!
//! The kernel octal-escapes exactly four bytes in the mount paths it
//! renders — space (`\040`), tab (`\011`), newline (`\012`) and backslash
//! (`\134`; see `mangle_path` in `fs/proc_namespace.c`) — and passes
//! everything else, including non-UTF-8 bytes, through raw. Comparing the
//! rendered field verbatim against a real path therefore misses every
//! mountpoint containing one of the four (e.g. `/mnt/my skills`), blinding
//! stale-mount cleanup and force-unmount retry loops.
//!
//! All matching happens on raw bytes: a lossy UTF-8 view would conflate a
//! mounted invalid-byte path (`/mnt/\xff`) with a different valid-UTF-8
//! query (`/mnt/\u{FFFD}`), reporting a plain directory as mounted.
//! Pure byte logic; the callers stay platform-gated.

use std::borrow::Cow;

/// Whether the raw bytes of `/proc/mounts` list `target` (raw OS-string
/// bytes of a path, e.g. via `OsStrExt::as_bytes`) as a mountpoint.
pub fn mounts_contain_target(mounts: &[u8], target: &[u8]) -> bool {
    mounts
        .split(|&b| b == b'\n')
        .any(|line| line_mounts_target(line, target))
}

/// Whether one `/proc/mounts` line mounts `target`: the second
/// whitespace-delimited field, escape-decoded, equals the target bytes.
/// Splitting the raw line on whitespace is safe — the escaped rendering
/// never contains literal whitespace inside a field.
fn line_mounts_target(line: &[u8], target: &[u8]) -> bool {
    match nth_field(line, 1) {
        Some(field) => &*unescape_mount_field(field) == target,
        None => false,
    }
}

/// The `idx`-th non-empty whitespace-delimited field of `line`.
fn nth_field(line: &[u8], idx: usize) -> Option<&[u8]> {
    line.split(|b: &u8| b.is_ascii_whitespace())
        .filter(|field| !field.is_empty())
        .nth(idx)
}

/// Decode one whitespace-delimited `/proc/mounts` path field at the byte
/// level.
///
/// A backslash not followed by three octal digits is kept verbatim — the
/// kernel never emits one, and guessing at malformed input would invent
/// paths.
fn unescape_mount_field(field: &[u8]) -> Cow<'_, [u8]> {
    if !field.contains(&b'\\') {
        return Cow::Borrowed(field);
    }
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        if field[i] == b'\\'
            && i + 3 < field.len()
            && field[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let digit = |b: u8| (b - b'0') as u32;
            let decoded = digit(field[i + 1]) * 64 + digit(field[i + 2]) * 8 + digit(field[i + 3]);
            out.push(decoded as u8);
            i += 4;
        } else {
            out.push(field[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_field_is_borrowed_unchanged() {
        assert!(matches!(
            unescape_mount_field(b"/mnt/plain"),
            Cow::Borrowed(_)
        ));
        // Non-ASCII passes through raw — the kernel does not escape it.
        assert_eq!(
            unescape_mount_field("/中文挂载点".as_bytes()).as_ref(),
            "/中文挂载点".as_bytes()
        );
    }

    #[test]
    fn kernel_escapes_decode_to_the_real_bytes() {
        assert_eq!(
            unescape_mount_field(br"/mnt/my\040skills").as_ref(),
            b"/mnt/my skills"
        );
        assert_eq!(unescape_mount_field(br"/a\011b").as_ref(), b"/a\tb");
        assert_eq!(unescape_mount_field(br"/a\012b").as_ref(), b"/a\nb");
        assert_eq!(unescape_mount_field(br"/a\134b").as_ref(), b"/a\\b");
        assert_eq!(
            unescape_mount_field(br"/\040a\134b\011c\040").as_ref(),
            b"/ a\\b\tc "
        );
    }

    #[test]
    fn malformed_escapes_are_kept_verbatim() {
        assert_eq!(
            unescape_mount_field(br"/trailing\").as_ref(),
            b"/trailing\\"
        );
        assert_eq!(
            unescape_mount_field(br"/short\04x").as_ref(),
            b"/short\\04x"
        );
        assert_eq!(
            unescape_mount_field(br"/non-octal\890").as_ref(),
            b"/non-octal\\890"
        );
    }

    #[test]
    fn line_match_uses_the_unescaped_second_field() {
        let line = b"/dev/vda3 /mnt/my\\040skills ext4 rw,relatime 0 0";
        assert!(line_mounts_target(line, b"/mnt/my skills"));
        assert!(!line_mounts_target(line, br"/mnt/my\040skills"));
        assert!(!line_mounts_target(line, b"/mnt/other"));
        // Lines without a second field never match.
        assert!(!line_mounts_target(b"/dev/vda3", b"/dev/vda3"));
        assert!(!line_mounts_target(b"", b"/"));
    }

    #[test]
    fn invalid_bytes_do_not_collide_with_the_replacement_char() {
        // Regression for the lossy-comparison conflation: a mounted
        // invalid-byte path and a queried U+FFFD path are different
        // mountpoints and must never match each other.
        let mounts = b"fuse.skillfs /mnt/\xff fuse.skillfs rw,nosuid 0 0\n";
        let replacement = "/mnt/\u{FFFD}".as_bytes();
        assert!(!mounts_contain_target(mounts, replacement));
        assert!(mounts_contain_target(mounts, b"/mnt/\xff"));
        // The valid-UTF-8 side matches only its own mount.
        let mounts = "fuse.skillfs /mnt/\u{FFFD} fuse.skillfs rw,nosuid 0 0\n".as_bytes();
        assert!(mounts_contain_target(mounts, replacement));
        assert!(!mounts_contain_target(mounts, b"/mnt/\xff"));
    }

    #[test]
    fn multi_line_table_scans_every_line() {
        let mounts = b"proc /proc proc rw 0 0\n/dev/vda3 /mnt/my\\040skills ext4 rw 0 0\ntmpfs /tmp tmpfs rw 0 0\n";
        assert!(mounts_contain_target(mounts, b"/mnt/my skills"));
        assert!(mounts_contain_target(mounts, b"/tmp"));
        assert!(!mounts_contain_target(mounts, b"/mnt/absent"));
    }
}
