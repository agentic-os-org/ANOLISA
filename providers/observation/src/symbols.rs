//! Resolve exact dynamic OpenSSL symbols; offsets and pattern guessing are unsupported.

use anyhow::{ensure, Context, Result};
use object::{Object, ObjectSegment, ObjectSymbol};
use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt};

pub(crate) struct Symbol {
    pub address: u64,
    pub name: String,
}

pub(crate) fn resolve(pid: i32) -> Result<Vec<Symbol>> {
    let maps = fs::read_to_string(format!("/proc/{pid}/maps"))?;
    let mut candidates = BTreeSet::new();
    for line in maps.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 6 || !fields[1].contains('x') {
            continue;
        }
        let path = fields[5];
        if !path
            .rsplit('/')
            .next()
            .is_some_and(|p| p.starts_with("libssl.so"))
        {
            continue;
        }
        let start = u64::from_str_radix(fields[0].split('-').next().context("invalid_maps")?, 16)?;
        let end = u64::from_str_radix(fields[0].split('-').nth(1).context("invalid_maps")?, 16)?;
        let offset = u64::from_str_radix(fields[2], 16)?;
        let file_path = format!("/proc/{pid}/root{path}");
        let metadata = fs::metadata(&file_path)?;
        ensure!(
            metadata.ino().to_string() == fields[4],
            "mapped_library_changed"
        );
        ensure!(metadata.len() <= 32 * 1024 * 1024, "library_size_limit");
        let data = fs::read(file_path)?;
        ensure!(data.len() <= 32 * 1024 * 1024, "library_size_limit");
        let elf = object::File::parse(data.as_slice())?;
        ensure!(
            elf.architecture() == object::Architecture::X86_64 && elf.is_little_endian(),
            "unsupported_abi"
        );
        let segment = elf
            .segments()
            .find(|s| {
                let (off, len) = s.file_range();
                off <= offset && offset < off + len
            })
            .context("unsupported_load_layout")?;
        let (file_offset, _) = segment.file_range();
        let base = start
            .checked_sub(segment.address() + offset - file_offset)
            .context("unsupported_load_bias")?;
        for symbol in elf.dynamic_symbols().filter(|s| s.is_definition()) {
            let name = symbol.name().unwrap_or("");
            if !matches!(name, "SSL_write" | "SSL_write_ex") {
                continue;
            }
            let address = base
                .checked_add(symbol.address())
                .context("symbol_overflow")?;
            ensure!(
                (start..end).contains(&address),
                "symbol_outside_executable_mapping"
            );
            candidates.insert((address, name.to_owned()));
        }
    }
    ensure!(
        !candidates.is_empty() && candidates.len() <= 2,
        "unsupported_tls_symbols"
    );
    Ok(candidates
        .into_iter()
        .map(|(address, name)| Symbol { address, name })
        .collect())
}
