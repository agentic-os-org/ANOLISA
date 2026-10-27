//! Bounded synthetic Debian fixtures, built without extracting or executing members.

use std::io::{Read as _, Write as _};

use base64::Engine as _;
use serde_json::Value;

use crate::{Error, Result, contracts::text};

fn bad(message: &str) -> Error {
    Error::Contract(message.into())
}

pub(crate) fn materialize(probe: &mut Value) -> Result<()> {
    if let Some(fixtures) = probe["fixtures"].as_object_mut() {
        for fixture in fixtures.values_mut() {
            if text(fixture, "kind") == "deb" {
                let bytes = build_deb(text(fixture, "content"))?;
                fixture["kind"] = "binary".into();
                fixture["content"] = base64::engine::general_purpose::STANDARD
                    .encode(bytes)
                    .into();
            }
        }
    }
    let input = text(probe, "input_path").to_owned();
    if let Some(route) = input.strip_prefix("http://127.0.0.1:8765") {
        probe["input_path"] = route.split(['?', '#']).next().unwrap_or_default().into();
    }
    let payload = text(probe, "payload_path").to_owned();
    if probe["fixtures"].get(&payload).is_some() {
        probe["payload_path"] = format!("/app/work/{payload}").into();
    } else if input
        .strip_prefix("/app/work/")
        .is_some_and(|p| probe["fixtures"].get(p).is_some())
        && std::path::Path::new(&input)
            .file_name()
            .and_then(|s| s.to_str())
            == Some(&payload)
    {
        probe["payload_path"] = input.into();
    }
    Ok(())
}

fn build_deb(specification: &str) -> Result<Vec<u8>> {
    let spec: Value = serde_json::from_str(specification)?;
    let object = spec
        .as_object()
        .ok_or_else(|| bad("deb recipe must be an object"))?;
    if object
        .keys()
        .any(|k| !matches!(k.as_str(), "package" | "version" | "architecture" | "files"))
    {
        return Err(bad("deb recipe contains unknown fields"));
    }
    let package = spec["package"].as_str().unwrap_or("score-fixture");
    let version = text(&spec, "version");
    let architecture = text(&spec, "architecture");
    if !regex::Regex::new(r"^[a-z0-9][a-z0-9+.-]{1,63}$")
        .map_err(|_| bad("invalid package validator"))?
        .is_match(package)
        || !regex::Regex::new(r"^[0-9][A-Za-z0-9.+:~\-]{0,63}$")
            .map_err(|_| bad("invalid version validator"))?
            .is_match(version)
        || !matches!(architecture, "all" | "amd64" | "arm64")
    {
        return Err(bad(
            "deb requires valid package, explicit version and architecture",
        ));
    }
    let files = spec["files"]
        .as_object()
        .ok_or_else(|| bad("deb files must be an object"))?;
    if files.is_empty() || files.len() > 128 {
        return Err(bad("deb requires 1-128 members"));
    }
    let mut members = Vec::new();
    let mut total = 0;
    for (name, content) in files {
        let content = content
            .as_str()
            .ok_or_else(|| bad("deb members must be text"))?;
        total += content.len();
        if name.is_empty() || name.contains('\0') || name.len() > 256 || total > 524_288 {
            return Err(bad("deb recipe exceeds member/size limits"));
        }
        members.push((name.as_str(), content));
    }
    members.sort_by_key(|(name, _)| *name);
    let control = format!(
        "Package: {package}\nVersion: {version}\nArchitecture: {architecture}\nMaintainer: Fixture <fixture@example.invalid>\nDescription: Disposable test input\n"
    );
    let mut result = b"!<arch>\n".to_vec();
    for (name, data) in [
        ("debian-binary", b"2.0\n".to_vec()),
        ("control.tar.gz", archive(&[("control", &control)])?),
        ("data.tar.gz", archive(&members)?),
    ] {
        writeln!(
            result,
            "{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`",
            format!("{name}/"),
            0,
            0,
            0,
            100_644,
            data.len()
        )?;
        result.extend(&data);
        if data.len() % 2 != 0 {
            result.push(b'\n');
        }
    }
    Ok(result)
}

fn archive(members: &[(&str, &str)]) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    for (name, content) in members {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(content.len() as u64);
        // Raw names intentionally permit traversal as test data; this builder never extracts them.
        let bytes = name.as_bytes();
        if bytes.len() > 100 {
            let mut extension = tar::Header::new_gnu();
            extension.set_path("././@LongLink")?;
            extension.set_entry_type(tar::EntryType::GNULongName);
            extension.set_size(bytes.len() as u64 + 1);
            extension.set_mode(0o644);
            extension.set_cksum();
            let mut long_name = bytes.to_vec();
            long_name.push(0);
            builder.append(&extension, long_name.as_slice())?;
        }
        let short = &bytes[..bytes.len().min(100)];
        header.as_mut_bytes()[..short.len()].copy_from_slice(short);
        header.set_cksum();
        builder.append(&header, content.as_bytes())?;
    }
    let data = builder.into_inner()?;
    let mut gzip = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), flate2::Compression::default());
    gzip.write_all(&data)?;
    Ok(gzip.finish()?)
}

pub(crate) fn validate_deb(content: &[u8]) -> Result<()> {
    if content.len() > 1_048_576 {
        return Err(bad("compressed Debian fixture exceeds 1 MiB"));
    }
    if !content.starts_with(b"!<arch>\n") {
        return Err(bad("Debian fixture is not an ar archive"));
    }
    let mut offset = 8;
    let mut members = std::collections::BTreeMap::new();
    while offset < content.len() {
        let header = content
            .get(offset..offset + 60)
            .ok_or_else(|| bad("truncated ar header"))?;
        if &header[58..] != b"`\n" {
            return Err(bad("invalid ar header"));
        }
        let name = std::str::from_utf8(&header[..16])
            .map_err(|_| bad("invalid ar name"))?
            .trim()
            .trim_end_matches('/');
        let size = std::str::from_utf8(&header[48..58])
            .map_err(|_| bad("invalid ar size"))?
            .trim()
            .parse::<usize>()
            .map_err(|_| bad("invalid ar size"))?;
        offset += 60;
        let end = offset
            .checked_add(size)
            .ok_or_else(|| bad("ar size overflow"))?;
        let data = content
            .get(offset..end)
            .ok_or_else(|| bad("truncated ar data"))?;
        if members.insert(name, data).is_some() {
            return Err(bad("duplicate ar member"));
        }
        offset = end + size % 2;
    }
    if offset != content.len()
        || members
            .get("debian-binary")
            .is_none_or(|value| value.trim_ascii() != b"2.0")
    {
        return Err(bad("invalid Debian archive version/alignment"));
    }
    for prefix in ["control.tar", "data.tar"] {
        let matches = members
            .iter()
            .filter(|(name, _)| **name == prefix || name.starts_with(&format!("{prefix}.")))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(bad(
                "Debian fixture needs exactly one control and data archive",
            ));
        }
        let (_, bytes) = matches[0];
        let expanded = expand_archive(bytes)?;
        let mut archive = tar::Archive::new(expanded.as_slice());
        let mut count = 0;
        let mut total = 0;
        let mut control = false;
        for entry in archive.entries()? {
            let mut entry = entry?;
            count += 1;
            total += entry.size();
            if count > 256 || total > 16 * 1024 * 1024 {
                return Err(bad("Debian archive inspection exceeds limit"));
            }
            control |= entry.path_bytes().as_ref() == b"control"
                || entry.path_bytes().as_ref() == b"./control";
            let read = std::io::copy(&mut entry, &mut std::io::sink())?;
            if entry.header().entry_type().is_file() && read != entry.size() {
                return Err(bad("truncated Debian tar data"));
            }
        }
        if prefix == "control.tar" && !control {
            return Err(bad("Debian fixture lacks control metadata"));
        }
    }
    Ok(())
}

fn expand_archive(bytes: &[u8]) -> Result<Vec<u8>> {
    let reader: Box<dyn std::io::Read> = if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(flate2::read::MultiGzDecoder::new(bytes))
    } else if bytes.starts_with(b"BZh") {
        Box::new(bzip2::read::MultiBzDecoder::new(bytes))
    } else if bytes.starts_with(b"\xfd7zXZ\0") {
        let stream = liblzma::stream::Stream::new_stream_decoder(
            64 * 1024 * 1024,
            liblzma::stream::CONCATENATED,
        )
        .map_err(|_| bad("invalid XZ decoder configuration"))?;
        Box::new(liblzma::read::XzDecoder::new_stream(bytes, stream))
    } else {
        Box::new(bytes)
    };
    let mut output = Vec::new();
    reader.take(16 * 1024 * 1024 + 1).read_to_end(&mut output)?;
    if output.len() > 16 * 1024 * 1024 {
        return Err(bad("expanded Debian archive exceeds 16 MiB"));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_generated_bzip2_and_xz_fixtures_are_bounded_and_not_extracted() -> Result<()> {
        for bytes in [
            include_bytes!("../assets/tests/fixture-bz2.deb").as_slice(),
            include_bytes!("../assets/tests/fixture-xz.deb").as_slice(),
        ] {
            validate_deb(bytes)?;
        }
        assert!(expand_archive(include_bytes!("../assets/tests/oversize.tar.xz")).is_err());
        assert!(validate_deb(&vec![0; 1_048_577]).is_err());
        Ok(())
    }
    #[test]
    fn synthetic_debian_roundtrip_keeps_long_adversarial_member_names() -> Result<()> {
        let name = format!("../{}", "a".repeat(150));
        let recipe = serde_json::json!({"package":"score-fixture","version":"1.0","architecture":"all","files":{name:"payload"}});
        let bytes = build_deb(&recipe.to_string())?;
        validate_deb(&bytes)?;
        assert!(validate_deb(&bytes[..bytes.len() - 1]).is_err());
        assert_eq!(bytes, build_deb(&recipe.to_string())?);
        Ok(())
    }
}
