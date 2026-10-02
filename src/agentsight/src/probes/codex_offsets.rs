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
    Some(hash.iter().map(|b| format!("{:02x}", b)).collect())
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
    fn load_table() {
        let table = OffsetTable::load(TEST_JSON).unwrap();
        assert_eq!(table.entries.len(), 1);
        assert_eq!(table.entries[0].offsets.as_ref().unwrap().ssl_write, 1000);
    }
}
