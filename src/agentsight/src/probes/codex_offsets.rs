use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::Read;

use super::elf_buildid;
use super::sslsniff::StaticSslOffsets;

const HEAD_SIZE: usize = 65536;

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
        let metadata = std::fs::metadata(path).ok()?;
        let file_size = metadata.len();

        let candidates: Vec<&OffsetEntry> = self
            .entries
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
        for entry in &candidates {
            if entry.fingerprint.head_64k_sha256 == head_sha {
                return entry.offsets.clone();
            }
        }

        None
    }
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
}
