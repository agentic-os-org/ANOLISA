//! Vectorized SSL executable searches with bounded scratch space.

use super::SCAN_CHUNK_BYTES;
use std::{fs, io::Read};

/// Appends the absolute offset of every pattern match starting in `window`
/// below `limit`, one vector per entry of `patterns`, in ascending order.
///
/// Vectorized search skips nonmatching bytes while retaining every start offset.
pub(super) fn collect_pattern_hits(
    window: &[u8],
    patterns: &[&[u8]],
    base: usize,
    limit: usize,
    hits: &mut [Vec<usize>],
) {
    let limit = limit.min(window.len());
    for (slot, pattern) in hits.iter_mut().zip(patterns) {
        let finder = memchr::memmem::Finder::new(pattern);
        let mut start = 0;
        while start < limit {
            let Some(found) = finder.find(&window[start..]) else {
                break;
            };
            let position = start + found;
            if position >= limit {
                break;
            }
            slot.push(base + position);
            // Advancing by one preserves overlapping signatures and boundary ownership.
            start = position + 1;
        }
    }
}

/// Reads until `dst` is full or the file ends, returning the byte count.
///
/// `Read::read` is free to return short of the request before EOF, and a short
/// window would split a pattern that is actually present, so the fill has to be
/// driven to completion rather than trusted to one call.
///
/// # Errors
///
/// Returns `None` on any I/O error other than an interrupted call.
pub(super) fn fill_buf(file: &mut fs::File, mut dst: &mut [u8]) -> Option<usize> {
    let mut total = 0;
    while !dst.is_empty() {
        match file.read(dst) {
            Ok(0) => break,
            Ok(n) => {
                total += n;
                dst = &mut dst[n..];
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(total)
}

/// String markers whose presence in an executable indicates a statically
/// linked BoringSSL, regardless of the binary's name. BoringSSL builds keep
/// vendor path strings (`vendor/boringssl/...`) and the
/// `openssl_is_boringssl` symbol name in .rodata even when stripped.
/// Deliberately BoringSSL-only: statically-linked OpenSSL binaries stay
/// opt-in through the name-based fast path (codex, python3.x), keeping this
/// fallback scoped to the Bun/Node binary shape it was built for. This is a
/// cheap prefilter, not a verdict: a hit only justifies running the
/// (expensive) three-tier detection, which remains the source of truth.
///
/// The marker is stored reversed and reassembled at runtime so the
/// contiguous string never appears in AgentSight's own binary: AgentSight
/// resolves the https-rule domains itself, and a self-match would trigger a
/// pointless full byte-pattern scan of its own executable. (A capitalized
/// `BoringSSL` variant is unusable anyway: the aws-lc dependency embeds
/// that exact string, which would self-match the same way.)
pub(super) fn ssl_exe_markers() -> Vec<Vec<u8>> {
    vec![b"lssgnirob".iter().copied().rev().collect()]
}

/// True when any marker from [`ssl_exe_markers`] occurs in the file at
/// `path`.
///
/// Streams the file in rounds of [`SCAN_CHUNK_BYTES`] fresh bytes (the
/// buffer additionally holds the `max_marker_len - 1` bytes carried over
/// from the previous round, so a marker straddling a round boundary is
/// still seen) and stops at the first hit, so a matching binary costs only
/// the read up to its first marker.
pub(super) fn file_has_ssl_markers(path: &str) -> bool {
    let markers = ssl_exe_markers();
    let overlap = markers.iter().map(|m| m.len()).max().unwrap_or(1) - 1;
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut buf = vec![0u8; overlap + SCAN_CHUNK_BYTES];
    let mut carry = 0usize;
    loop {
        let Some(read) = fill_buf(&mut file, &mut buf[carry..]) else {
            return false;
        };
        let filled = carry + read;
        if filled == 0 {
            return false;
        }
        let window = &buf[..filled];
        if markers
            .iter()
            .any(|marker| memchr::memmem::find(window, marker).is_some())
        {
            return true;
        }
        if filled < buf.len() {
            return false;
        }
        buf.copy_within(filled - overlap..filled, 0);
        carry = overlap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_matches_and_deferred_window_starts_are_preserved() {
        let patterns: &[&[u8]] = &[b"aa", b"aaa"];
        let mut hits = vec![Vec::new(); 2];
        collect_pattern_hits(b"aaaa", patterns, 10, 2, &mut hits);
        assert_eq!(hits, vec![vec![10, 11], vec![10, 11]]);
        let mut hits = vec![Vec::new(); 2];
        collect_pattern_hits(b"aaaa", patterns, 0, 4, &mut hits);
        assert_eq!(hits, vec![vec![0, 1, 2], vec![0, 1]]);
    }
}
