use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

use super::elf_buildid;
use super::sslsniff::StaticSslOffsets;

const HEAD_SIZE: usize = 65536;

/// Upper bound on persisted learned entries; the oldest entry is evicted first.
const MAX_LEARNED_ENTRIES: usize = 64;

#[derive(Debug)]
struct OffsetEntry {
    fingerprint: Fingerprint,
    offsets: Option<StaticSslOffsets>,
}

#[derive(Debug)]
struct Fingerprint {
    build_id: Option<String>,
    file_size: u64,
    head_64k_sha256: String,
}

pub struct OffsetTable {
    entries: Vec<OffsetEntry>,
}

impl OffsetTable {
    pub fn load(json_str: &str) -> Option<Self> {
        let root: serde_json::Value = serde_json::from_str(json_str).ok()?;
        let codex_offsets = root.get("codex_offsets")?;
        let entries_val = codex_offsets.get("entries")?.as_array()?;

        let mut entries = Vec::new();
        for e in entries_val {
            let fp = e.get("fingerprint")?;
            let fingerprint = Fingerprint {
                build_id: fp
                    .get("build_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                file_size: fp.get("file_size")?.as_u64()?,
                head_64k_sha256: fp.get("head_64k_sha256")?.as_str()?.to_string(),
            };

            let offsets = match e.get("offsets") {
                Some(serde_json::Value::Object(obj)) => {
                    let w = obj.get("ssl_write")?.as_u64()? as usize;
                    let r = obj.get("ssl_read")?.as_u64()? as usize;
                    let h = obj.get("ssl_do_handshake")?.as_u64()? as usize;
                    let write_is_ex = obj
                        .get("write_is_ex")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let read_is_ex = obj
                        .get("read_is_ex")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    Some(StaticSslOffsets {
                        ssl_write: w,
                        ssl_read: r,
                        ssl_do_handshake: h,
                        write_is_ex,
                        read_is_ex,
                    })
                }
                _ => None,
            };

            entries.push(OffsetEntry {
                fingerprint,
                offsets,
            });
        }

        Some(Self { entries })
    }

    pub fn lookup(&self, path: &str) -> Option<StaticSslOffsets> {
        match_by_fingerprint(&self.entries, path)
    }
}

/// Exact-content match shared by the shipped table and the learned cache.
///
/// The file size filters first (a cheap stat); a GNU build-id then decides
/// when present, otherwise the head-64KiB SHA-256 does. A replaced or
/// rewritten binary therefore can never match a stale entry.
fn match_by_fingerprint(entries: &[OffsetEntry], path: &str) -> Option<StaticSslOffsets> {
    let metadata = std::fs::metadata(path).ok()?;
    let file_size = metadata.len();

    let candidates: Vec<&OffsetEntry> = entries
        .iter()
        .filter(|e| e.fingerprint.file_size == file_size)
        .collect();

    if candidates.is_empty() {
        return None;
    }

    let build_id = elf_buildid::read_buildid(path);
    if let Some(ref bid) = build_id {
        for entry in &candidates {
            if entry.fingerprint.build_id.as_deref() == Some(bid.as_str()) {
                return entry.offsets.clone();
            }
        }
    }

    let head_sha = compute_head_sha256(path)?;
    for entry in candidates {
        if entry.fingerprint.head_64k_sha256 == head_sha {
            return entry.offsets.clone();
        }
    }

    None
}

/// Runtime-learned static-SSL offsets, persisted across service restarts.
///
/// A whole-binary byte-pattern scan finishes long after a warm-started agent
/// has issued its first TLS call, so an offset set resolved once must be
/// reusable synchronously at the next discovery — including after a restart
/// (#9762). Entries carry the same exact-content fingerprint as the shipped
/// table, and the file uses the same schema, so [`OffsetTable::load`] parses
/// it and a stale entry can never match a changed binary.
///
/// The file lives in the AgentSight storage directory (root-owned, like every
/// other state file there) and is written atomically via temp + rename; a
/// missing or corrupt file simply yields an empty cache.
pub(super) struct LearnedOffsets {
    entries: Vec<OffsetEntry>,
    path: PathBuf,
}

impl LearnedOffsets {
    /// Load the persisted cache; never fails, a problem yields an empty set.
    pub(super) fn load(path: PathBuf) -> Self {
        let entries = match std::fs::read_to_string(&path) {
            Ok(text) => match OffsetTable::load(&text) {
                Some(table) => table.entries,
                None => {
                    log::warn!(
                        "Ignoring corrupt learned SSL offset cache {}",
                        path.display()
                    );
                    Vec::new()
                }
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                log::warn!(
                    "Cannot read learned SSL offset cache {}: {error}",
                    path.display()
                );
                Vec::new()
            }
        };
        Self { entries, path }
    }

    /// Synchronous fingerprint lookup, mirroring the shipped table.
    pub(super) fn lookup(&self, path: &str) -> Option<StaticSslOffsets> {
        match_by_fingerprint(&self.entries, path)
    }

    /// Persist offsets verified for the file at `path` (a pinned `/proc/self/fd`
    /// descriptor is acceptable). A no-op when the fingerprint is known.
    pub(super) fn record(&mut self, path: &str, offsets: &StaticSslOffsets) {
        let Some(fingerprint) = fingerprint_of(path) else {
            log::debug!("Cannot fingerprint SSL scan input {path}; not persisting offsets");
            return;
        };
        if self.entries.iter().any(|entry| {
            entry.fingerprint.file_size == fingerprint.file_size
                && entry.fingerprint.head_64k_sha256 == fingerprint.head_64k_sha256
        }) {
            return;
        }
        if self.entries.len() >= MAX_LEARNED_ENTRIES {
            self.entries.remove(0);
        }
        self.entries.push(OffsetEntry {
            fingerprint,
            offsets: Some(offsets.clone()),
        });
        log::info!(
            "Learned static SSL offsets for {} ({} entries cached)",
            path,
            self.entries.len()
        );
        self.save();
    }

    /// Number of cached entries (diagnostics/tests).
    #[cfg(test)]
    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    fn save(&self) {
        let entries: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|entry| {
                let mut fingerprint = serde_json::json!({
                    "file_size": entry.fingerprint.file_size,
                    "head_64k_sha256": entry.fingerprint.head_64k_sha256,
                });
                if let Some(ref build_id) = entry.fingerprint.build_id {
                    fingerprint["build_id"] = serde_json::json!(build_id);
                }
                let offsets = entry.offsets.as_ref().map(|off| {
                    serde_json::json!({
                        "ssl_write": off.ssl_write,
                        "ssl_read": off.ssl_read,
                        "ssl_do_handshake": off.ssl_do_handshake,
                        "write_is_ex": off.write_is_ex,
                        "read_is_ex": off.read_is_ex,
                    })
                });
                serde_json::json!({ "fingerprint": fingerprint, "offsets": offsets })
            })
            .collect();
        let json = serde_json::json!({
            "codex_offsets": { "schema_version": 1, "entries": entries },
        });
        let Some(parent) = self.path.parent() else {
            return;
        };
        if let Err(error) = std::fs::create_dir_all(parent) {
            log::warn!(
                "Cannot create directory for learned SSL offset cache {}: {error}",
                parent.display()
            );
            return;
        }
        let tmp = self.path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&tmp, json.to_string()) {
            log::warn!(
                "Cannot write learned SSL offset cache {}: {error}",
                tmp.display()
            );
            return;
        }
        if let Err(error) = std::fs::rename(&tmp, &self.path) {
            log::warn!(
                "Cannot persist learned SSL offset cache {}: {error}",
                self.path.display()
            );
        }
    }
}

/// Exact-content fingerprint of the file at `path`, matching the shipped
/// table's entry schema.
fn fingerprint_of(path: &str) -> Option<Fingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    Some(Fingerprint {
        build_id: elf_buildid::read_buildid(path),
        file_size: metadata.len(),
        head_64k_sha256: compute_head_sha256(path)?,
    })
}

fn compute_head_sha256(path: &str) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD_SIZE];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let hash = Sha256::digest(&buf);
    Some(hash.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_JSON: &str = r#"{
        "codex_offsets": {
            "schema_version": 1,
            "entries": [
                {
                    "codex_version": "0.141.0",
                    "fingerprint": { "file_size": 100, "head_64k_sha256": "abc123" },
                    "offsets": { "ssl_write": 1000, "ssl_read": 2000, "ssl_do_handshake": 3000 }
                }
            ]
        }
    }"#;

    #[test]
    fn shipped_codex_entries_preserve_ex_probe_abi() {
        let table = OffsetTable::load(include_str!("../../agentsight.json")).unwrap();
        for (size, write, read, handshake) in [
            (258_278_208, 204_730_912, 204_730_320, 204_728_640),
            (294_415_048, 231_360_640, 231_358_720, 231_369_984),
        ] {
            let entry = table
                .entries
                .iter()
                .find(|entry| entry.fingerprint.file_size == size)
                .expect("validated musl release fingerprint");
            let offsets = entry.offsets.as_ref().expect("traceable release");
            assert_eq!(
                (
                    offsets.ssl_write,
                    offsets.ssl_read,
                    offsets.ssl_do_handshake
                ),
                (write, read, handshake)
            );
            assert!(offsets.write_is_ex && offsets.read_is_ex);
            assert_eq!(entry.fingerprint.head_64k_sha256.len(), 64);
        }
    }

    #[test]
    fn lookup_requires_matching_size_and_head_hash() {
        use std::io::Write;

        let mut binary = tempfile::NamedTempFile::new().unwrap();
        binary
            .write_all(b"synthetic release without Build ID")
            .unwrap();
        let path = binary.path().to_str().unwrap();
        let size = binary.as_file().metadata().unwrap().len();
        let hash = compute_head_sha256(path).unwrap();
        let json = serde_json::json!({"codex_offsets": {"entries": [{
            "fingerprint": {"file_size": size, "head_64k_sha256": hash},
            "offsets": {"ssl_write": 1, "ssl_read": 2, "ssl_do_handshake": 3,
                "write_is_ex": true, "read_is_ex": true}
        }]}})
        .to_string();
        let mut table = OffsetTable::load(&json).unwrap();
        assert!(table.lookup(path).is_some());
        table.entries[0].fingerprint.file_size += 1;
        assert!(table.lookup(path).is_none());
        table.entries[0].fingerprint.file_size = size;
        table.entries[0].fingerprint.head_64k_sha256 = "wrong hash".into();
        assert!(table.lookup(path).is_none());
    }

    #[test]
    fn load_table() {
        let table = OffsetTable::load(TEST_JSON).unwrap();
        assert_eq!(table.entries.len(), 1);
        assert_eq!(table.entries[0].offsets.as_ref().unwrap().ssl_write, 1000);
    }

    #[test]
    fn shipped_table_seeds_claude_code_bun2_builds() {
        // Cold-start seeds for the two Claude Code builds whose offsets were
        // validated with kernel uprobes on live TLS traffic (#9762). Both are
        // the Bun 2026 generation: identical offsets, plain (non-_ex) ABI.
        let table = OffsetTable::load(include_str!("../../agentsight.json")).unwrap();
        for size in [340_226_576u64, 341_914_128] {
            let entry = table
                .entries
                .iter()
                .find(|entry| entry.fingerprint.file_size == size)
                .expect("validated Claude Code fingerprint");
            let offsets = entry.offsets.as_ref().expect("traceable release");
            assert_eq!(
                (
                    offsets.ssl_write,
                    offsets.ssl_read,
                    offsets.ssl_do_handshake
                ),
                (0x2f8e950, 0x2f8eaf0, 0x2f8ef60)
            );
            assert!(
                !offsets.write_is_ex && !offsets.read_is_ex,
                "Bun exports plain SSL_read/SSL_write, not the _ex variants"
            );
            assert_eq!(entry.fingerprint.head_64k_sha256.len(), 64);
        }
    }

    #[test]
    fn shipped_table_seeds_claude_code_legacy_bun_build() {
        // Cold-start seed for Claude Code 2.1.301 (#10003): the pre-2026 Bun
        // generation, whose offsets the reporter's own detector log and the
        // captured telemetry SSL_write both confirm on that exact build.
        let table = OffsetTable::load(include_str!("../../agentsight.json")).unwrap();
        let entry = table
            .entries
            .iter()
            .find(|entry| entry.fingerprint.file_size == 267_955_384)
            .expect("validated Claude Code 2.1.301 fingerprint");
        let offsets = entry.offsets.as_ref().expect("traceable release");
        assert_eq!(
            (
                offsets.ssl_write,
                offsets.ssl_read,
                offsets.ssl_do_handshake
            ),
            (0x2410ab0, 0x24106c0, 0x2410370)
        );
        assert!(!offsets.write_is_ex && !offsets.read_is_ex);
        assert_eq!(entry.fingerprint.head_64k_sha256.len(), 64);
    }

    fn test_offsets() -> StaticSslOffsets {
        StaticSslOffsets {
            ssl_write: 0x1000,
            ssl_read: 0x2000,
            ssl_do_handshake: 0x3000,
            write_is_ex: false,
            read_is_ex: false,
        }
    }

    #[test]
    fn learned_offsets_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("learned.json");
        let binary = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(binary.path(), b"synthetic static ssl binary").unwrap();
        let path = binary.path().to_str().unwrap();

        let mut store = LearnedOffsets::load(cache_path.clone());
        assert!(store.lookup(path).is_none());
        store.record(path, &test_offsets());
        assert_eq!(store.entry_count(), 1);
        // Recording the same fingerprint again must not grow the cache.
        store.record(path, &test_offsets());
        assert_eq!(store.entry_count(), 1);

        // A fresh instance (service restart) resolves synchronously.
        let reloaded = LearnedOffsets::load(cache_path);
        let offsets = reloaded.lookup(path).expect("learned entry must reload");
        assert_eq!(offsets.ssl_write, 0x1000);
        assert_eq!(offsets.ssl_read, 0x2000);
        assert_eq!(offsets.ssl_do_handshake, 0x3000);
    }

    #[test]
    fn learned_offsets_reject_changed_content() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("learned.json");
        let binary = tempfile::NamedTempFile::new().unwrap();
        // Same byte length, different content: only the head hash can tell
        // the revisions apart, so a stale entry must not match.
        std::fs::write(binary.path(), b"first revision").unwrap();
        let path = binary.path().to_str().unwrap();
        let mut store = LearnedOffsets::load(cache_path);
        store.record(path, &test_offsets());
        std::fs::write(binary.path(), b"FIRST REVISION").unwrap();
        assert_eq!(std::fs::metadata(path).unwrap().len(), 14);
        assert!(
            store.lookup(path).is_none(),
            "rewritten content must not match the learned fingerprint"
        );
    }

    #[test]
    fn learned_offsets_survive_corrupt_cache_file() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("learned.json");
        std::fs::write(&cache_path, b"{ not json").unwrap();
        let store = LearnedOffsets::load(cache_path);
        assert_eq!(store.entry_count(), 0);
    }

    #[test]
    fn learned_offsets_cap_evicts_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let cache_path = dir.path().join("learned.json");
        let mut store = LearnedOffsets::load(cache_path.clone());
        let mut files = Vec::new();
        for i in 0..(MAX_LEARNED_ENTRIES + 5) {
            let file = tempfile::NamedTempFile::new().unwrap();
            // Distinct content per file → distinct fingerprint.
            std::fs::write(file.path(), format!("synthetic binary {i}")).unwrap();
            store.record(file.path().to_str().unwrap(), &test_offsets());
            files.push(file);
        }
        assert_eq!(store.entry_count(), MAX_LEARNED_ENTRIES);
        assert!(
            store
                .lookup(files.last().unwrap().path().to_str().unwrap())
                .is_some(),
            "newest entry must resolve"
        );
        assert!(
            store.lookup(files[0].path().to_str().unwrap()).is_none(),
            "oldest entry must be evicted"
        );
        // The persisted state matches memory.
        let reloaded = LearnedOffsets::load(cache_path);
        assert_eq!(reloaded.entry_count(), MAX_LEARNED_ENTRIES);
    }
}
