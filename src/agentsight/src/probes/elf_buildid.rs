use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";
const PT_NOTE: u32 = 4;
const NT_GNU_BUILD_ID: u32 = 3;

/// Parse GNU Build-ID from an ELF binary's PT_NOTE segment.
/// Returns the hex-encoded build-id string, or None if not present.
///
/// **ELF64 only.** 32-bit ELF (`class == 1`) is rejected early because
/// Codex CLI ships as a statically-linked musl x86-64 binary. If 32-bit
/// support is ever needed, the header layout and pointer-sized fields
/// (e_phoff, p_offset, p_filesz) must be read as u32 instead of u64.
pub fn read_buildid(path: &str) -> Option<String> {
    let mut f = File::open(path).ok()?;
    let mut ident = [0u8; 16];
    f.read_exact(&mut ident).ok()?;
    if &ident[0..4] != ELF_MAGIC {
        return None;
    }
    let class = ident[4];
    let le = ident[5] == 1;
    if class != 2 {
        return None;
    }

    let read_u16 = |buf: &[u8]| -> u16 {
        if le {
            u16::from_le_bytes([buf[0], buf[1]])
        } else {
            u16::from_be_bytes([buf[0], buf[1]])
        }
    };
    let read_u32 = |buf: &[u8]| -> u32 {
        if le {
            u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]])
        } else {
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]])
        }
    };
    let read_u64 = |buf: &[u8]| -> Option<u64> {
        let arr: [u8; 8] = buf[0..8].try_into().ok()?;
        Some(if le {
            u64::from_le_bytes(arr)
        } else {
            u64::from_be_bytes(arr)
        })
    };

    // ELF64 header: e_phoff at offset 32, e_phentsize at 54, e_phnum at 56
    let mut hdr = [0u8; 64];
    f.seek(SeekFrom::Start(0)).ok()?;
    f.read_exact(&mut hdr).ok()?;

    let e_phoff = read_u64(&hdr[32..])?;
    let e_phentsize = read_u16(&hdr[54..]) as u64;
    let e_phnum = read_u16(&hdr[56..]) as u64;

    for i in 0..e_phnum {
        let off = e_phoff + i * e_phentsize;
        let mut phdr = [0u8; 56];
        f.seek(SeekFrom::Start(off)).ok()?;
        f.read_exact(&mut phdr).ok()?;

        let p_type = read_u32(&phdr[0..]);
        if p_type != PT_NOTE {
            continue;
        }

        let p_offset = read_u64(&phdr[8..])?;
        let p_filesz = read_u64(&phdr[32..])?;

        let mut note_buf = vec![0u8; p_filesz as usize];
        f.seek(SeekFrom::Start(p_offset)).ok()?;
        f.read_exact(&mut note_buf).ok()?;

        let mut pos = 0usize;
        while pos + 12 <= note_buf.len() {
            let namesz = read_u32(&note_buf[pos..]) as usize;
            let descsz = read_u32(&note_buf[pos + 4..]) as usize;
            let note_type = read_u32(&note_buf[pos + 8..]);
            pos += 12;

            let name_aligned = (pos + namesz + 3) & !3;
            let desc_start = name_aligned;
            let desc_end = desc_start + descsz;
            let desc_aligned = (desc_end + 3) & !3;

            if desc_end > note_buf.len() {
                break;
            }

            if note_type == NT_GNU_BUILD_ID
                && namesz == 4
                && note_buf.get(pos..pos + 3) == Some(b"GNU")
            {
                let id_bytes = &note_buf[desc_start..desc_end];
                return Some(hex_encode(id_bytes));
            }

            pos = desc_aligned;
        }
    }
    None
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_note_section() {
        let mut elf = vec![0u8; 256];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2; // 64-bit
        elf[5] = 1; // little-endian
        elf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        elf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        elf[56..58].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        let ph_off = 64usize;
        elf[ph_off..ph_off + 4].copy_from_slice(&4u32.to_le_bytes()); // PT_NOTE
        elf[ph_off + 8..ph_off + 16].copy_from_slice(&128u64.to_le_bytes()); // p_offset
        elf[ph_off + 32..ph_off + 40].copy_from_slice(&32u64.to_le_bytes()); // p_filesz

        let note_off = 128usize;
        elf[note_off..note_off + 4].copy_from_slice(&4u32.to_le_bytes()); // namesz
        elf[note_off + 4..note_off + 8].copy_from_slice(&4u32.to_le_bytes()); // descsz
        elf[note_off + 8..note_off + 12].copy_from_slice(&3u32.to_le_bytes()); // NT_GNU_BUILD_ID
        elf[note_off + 12..note_off + 16].copy_from_slice(b"GNU\0");
        elf[note_off + 16..note_off + 20].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);

        let dir = std::env::temp_dir();
        let path = dir.join("test_buildid.elf");
        std::fs::write(&path, &elf).unwrap();
        let result = read_buildid(path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert_eq!(result, Some("deadbeef".to_string()));
    }

    #[test]
    fn no_buildid_returns_none() {
        let mut elf = vec![0u8; 128];
        elf[0..4].copy_from_slice(b"\x7fELF");
        elf[4] = 2;
        elf[5] = 1;
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&0u16.to_le_bytes()); // no program headers

        let dir = std::env::temp_dir();
        let path = dir.join("test_no_buildid.elf");
        std::fs::write(&path, &elf).unwrap();
        let result = read_buildid(path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert_eq!(result, None);
    }
}
