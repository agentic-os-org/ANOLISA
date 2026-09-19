//! Fetch and validate one RPM repository snapshot for candidate queries.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, Write};
use std::path::Path;

mod transport;
use transport::Transport;

use crate::pkg_query::{PackageInfo, PackageVersion};
use crate::rpm_repo::RpmRepoSource;
use rpmrepo_metadata::{Checksum, PrimaryXml, RepomdXml, utils};
use sha2::{Digest, digest::DynDigest};

/// Repository transport, integrity, or metadata failure; never a package miss.
#[derive(Debug, thiserror::Error)]
pub enum RpmMetadataError {
    /// Repository configuration or downloaded metadata violates the query contract.
    #[error("RPM repository {url}: {reason}")]
    Invalid {
        /// Repository origin only; credentials, paths and query strings are omitted.
        url: String,
        /// Actionable failure details.
        reason: String,
    },
}

const INDEX_LIMIT: u64 = 4 * 1024 * 1024;
const DOWNLOAD_LIMIT: u64 = 128 * 1024 * 1024;
const EXPANDED_LIMIT: u64 = 512 * 1024 * 1024;

// Paths, query strings and userinfo may all carry repository access tokens.
fn diagnostic_origin(value: &str) -> String {
    match url::Url::parse(value) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url.origin().ascii_serialization(),
        Ok(url) if url.scheme() == "file" => "file://<local>".into(),
        _ => "<repository>".into(),
    }
}

#[derive(Default)]
pub(crate) struct RpmSnapshot {
    packages: Vec<(PackageInfo, Vec<String>)>,
}

impl RpmSnapshot {
    pub(crate) fn load(repo: &RpmRepoSource) -> Result<Self, RpmMetadataError> {
        Self::read(repo).map_err(|reason| RpmMetadataError::Invalid {
            url: diagnostic_origin(repo.base_url()),
            reason,
        })
    }

    fn read(repo: &RpmRepoSource) -> Result<Self, String> {
        let base = url::Url::parse(&format!("{}/", repo.base_url().trim_end_matches('/')))
            .map_err(|e| e.to_string())?;
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        let index = dir.path().join("repomd.xml");
        let transport = if matches!(base.scheme(), "http" | "https") {
            Some(Transport::system()?)
        } else {
            None
        };
        fetch(
            &base
                .join("repodata/repomd.xml")
                .map_err(|e| e.to_string())?,
            &index,
            transport.as_ref(),
            INDEX_LIMIT,
        )?;
        // Parse plain XML here: transparent file decompression would bypass
        // the download limit (and decode primary a second time below).
        let metadata = RepomdXml::read_data(validated_xml_reader(&index, false)?)
            .map_err(|e| e.to_string())?;
        let record = metadata
            .get_record("primary")
            .ok_or("repomd.xml contains no primary metadata")?;
        if record.size.is_some_and(|size| size > DOWNLOAD_LIMIT)
            || record.open_size.is_some_and(|size| size > EXPANDED_LIMIT)
        {
            return Err(
                "declared primary metadata size exceeds the download or expansion limit".into(),
            );
        }
        let href = record
            .location_href
            .to_str()
            .ok_or("non-UTF-8 primary location")?;
        let mut location = base.join(href).map_err(|e| e.to_string())?;
        // Absolute metadata links lose base userinfo when joined. Reuse repository
        // credentials only within its origin, unless the link supplies its own.
        if matches!(base.scheme(), "http" | "https")
            && location.origin() == base.origin()
            && location.username().is_empty()
            && location.password().is_none()
        {
            location
                .set_username(base.username())
                .map_err(|()| "primary location cannot carry a username")?;
            location
                .set_password(base.password())
                .map_err(|()| "primary location cannot carry a password")?;
        }
        // Repository metadata is remote input, not permission to read local files.
        if base.scheme() != "file" && location.scheme() == "file" {
            return Err("remote repository primary location points to a local file".into());
        }
        if base.scheme() == "https" && location.scheme() == "http" {
            return Err("HTTPS repository primary location downgrades to HTTP".into());
        }
        let compressed = dir.path().join("primary.download");
        fetch(
            &location,
            &compressed,
            transport.as_ref(),
            record.size.unwrap_or(DOWNLOAD_LIMIT).min(DOWNLOAD_LIMIT),
        )?;
        verify(&compressed, &record.checksum, record.size)?;
        let primary = dir.path().join("primary.xml");
        let mut input = utils::reader_from_file(&compressed).map_err(|e| e.to_string())?;
        let mut output = File::create(&primary).map_err(|e| e.to_string())?;
        let size = copy_limited(
            &mut input,
            &mut output,
            record
                .open_size
                .unwrap_or(EXPANDED_LIMIT)
                .min(EXPANDED_LIMIT),
        )?;
        if record.open_size.is_some_and(|expected| size != expected) {
            return Err(format!(
                "primary open size mismatch: expected {:?}, got {size}",
                record.open_size
            ));
        }
        if let Some(checksum) = &record.open_checksum {
            verify(&primary, checksum, record.open_size)?;
        }
        let mut reader = PrimaryXml::new_reader(validated_xml_reader(&primary, true)?);
        let expected = reader.read_header().map_err(|e| e.to_string())?;
        let mut packages = Vec::new();
        loop {
            let mut package = None;
            reader
                .read_package(&mut package)
                .map_err(|e| e.to_string())?;
            let Some(package) = package else { break };
            let info = PackageInfo {
                name: package.name().into(),
                version: PackageVersion {
                    epoch: (package.epoch() != 0).then(|| package.epoch().to_string()),
                    version: package.version().into(),
                    release: (!package.release().is_empty()).then(|| package.release().to_string()),
                },
                arch: package.arch().into(),
                origin: Some(repo.id().into()),
            };
            let provides = package
                .provides()
                .iter()
                .map(|p| {
                    let mut text = p.name().to_string();
                    if let (Some(flags), Some(version)) = (p.flags(), p.version()) {
                        text.push_str(&format!(" {} ", flags.as_operator()));
                        if let Some(epoch) = p.epoch().filter(|e| *e != "0") {
                            text.push_str(epoch);
                            text.push(':');
                        }
                        text.push_str(version);
                        if let Some(release) = p.release() {
                            text.push('-');
                            text.push_str(release);
                        }
                    }
                    text
                })
                .collect();
            packages.push((info, provides));
        }
        if packages.len() != expected {
            return Err(format!(
                "primary package count mismatch: expected {expected}, got {}",
                packages.len()
            ));
        }
        Ok(Self { packages })
    }

    pub(crate) fn candidates(&self, name: &str) -> Vec<PackageInfo> {
        self.packages
            .iter()
            .filter(|(p, _)| p.name == name)
            .map(|(p, _)| p.clone())
            .collect()
    }

    pub(crate) fn providers(&self, capability: &str) -> Vec<String> {
        let mut names = Vec::new();
        for (package, provides) in &self.packages {
            if provides
                .iter()
                .any(|p| p.split_whitespace().next() == Some(capability))
                && !names.contains(&package.name)
            {
                names.push(package.name.clone());
            }
        }
        names
    }

    pub(crate) fn capabilities(&self, name: &str) -> Vec<String> {
        let mut result = Vec::new();
        for (_, provides) in self.packages.iter().filter(|(p, _)| p.name == name) {
            for provide in provides {
                if !result.contains(provide) {
                    result.push(provide.clone());
                }
            }
        }
        result
    }
}

// rpmrepo_metadata 0.7 can spin at EOF inside an unclosed element. Validate
// nesting before handing it either document; files already have byte limits.
// Its primary header parser also assumes the packages attribute is present.
fn validated_xml_reader(
    path: &Path,
    is_primary: bool,
) -> Result<quick_xml::Reader<BufReader<File>>, String> {
    use quick_xml::events::Event;

    let mut input = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut reader = utils::create_xml_reader(&mut input);
    let mut buffer = Vec::new();
    let mut depth = 0usize;
    loop {
        match reader
            .read_event_into(&mut buffer)
            .map_err(|e| e.to_string())?
        {
            Event::Start(element) => {
                if is_primary && depth == 0 && element.name().as_ref() == "metadata" {
                    element
                        .try_get_attribute("packages")
                        .map_err(|e| e.to_string())?
                        .ok_or("primary metadata header is missing the packages attribute")?;
                }
                depth += 1;
            }
            // quick-xml rejects unmatched or mismatched closing tags first.
            Event::End(_) => depth -= 1,
            Event::Eof if depth != 0 => return Err("unexpected EOF in RPM metadata XML".into()),
            Event::Eof => break,
            _ => {}
        }
        buffer.clear();
    }
    input.rewind().map_err(|e| e.to_string())?;
    Ok(utils::create_xml_reader(input))
}

fn fetch(
    url: &url::Url,
    destination: &Path,
    transport: Option<&Transport>,
    limit: u64,
) -> Result<(), String> {
    let origin = diagnostic_origin(url.as_str());
    let mut input: Box<dyn Read> = match url.scheme() {
        "file" => Box::new(
            File::open(
                url.to_file_path()
                    .map_err(|_| format!("invalid file URL for {origin}"))?,
            )
            .map_err(|e| format!("fetch {origin}: {:?}", e.kind()))?,
        ),
        "http" | "https" => {
            let system;
            let transport = match transport {
                Some(transport) => transport,
                None => {
                    system = Transport::system()?;
                    &system
                }
            };
            let response = transport.get(url)?;
            if response
                .header("Content-Length")
                .and_then(|s| s.parse::<u64>().ok())
                .is_some_and(|size| size > limit)
            {
                return Err(format!(
                    "metadata response from {origin} exceeds {limit}-byte limit"
                ));
            }
            response.into_reader()
        }
        scheme => return Err(format!("unsupported metadata URL scheme: {scheme}")),
    };
    let mut output = File::create(destination).map_err(|e| e.to_string())?;
    copy_limited(&mut input, &mut output, limit).map_err(|e| format!("fetch {origin}: {e}"))?;
    Ok(())
}

// Cap bytes written even when the peer omits/forges a size or decoding expands
// a tiny compressed stream. Probe one extra byte without writing it to disk.
fn copy_limited(input: &mut impl Read, output: &mut impl Write, limit: u64) -> Result<u64, String> {
    let size = io::copy(&mut input.take(limit), output)
        .map_err(|e| format!("metadata stream I/O error: {:?}", e.kind()))?;
    let mut extra = [0; 1];
    if input
        .read(&mut extra)
        .map_err(|e| format!("metadata stream I/O error: {:?}", e.kind()))?
        != 0
    {
        return Err(format!("metadata exceeds {limit}-byte limit"));
    }
    Ok(size)
}

fn verify(path: &Path, checksum: &Checksum, size: Option<u64>) -> Result<(), String> {
    let (mut digest, expected): (Box<dyn DynDigest>, &str) = match checksum {
        Checksum::Sha224(value) => (Box::new(sha2::Sha224::new()), value),
        Checksum::Sha256(value) => (Box::new(sha2::Sha256::new()), value),
        Checksum::Sha384(value) => (Box::new(sha2::Sha384::new()), value),
        Checksum::Sha512(value) => (Box::new(sha2::Sha512::new()), value),
        Checksum::Sha1(value) => (Box::new(sha1::Sha1::new()), value),
        Checksum::Md5(value) => (Box::new(md5::Md5::new()), value),
        _ => return Err("missing or unsupported metadata checksum".into()),
    };
    let mut input = File::open(path).map_err(|e| e.to_string())?;
    let mut count = 0;
    let mut buffer = [0; 65536];
    loop {
        let read = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        count += read as u64;
        digest.update(&buffer[..read]);
    }
    if size.is_some_and(|expected| count != expected) {
        return Err(format!(
            "metadata size mismatch: expected {size:?}, got {count}"
        ));
    }
    let actual: String = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if !actual.eq_ignore_ascii_case(expected) {
        return Err("metadata checksum mismatch".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_copy_accepts_exact_limit_and_never_writes_the_excess() {
        for length in [0, 63, 64, 65] {
            let input = vec![b'x'; length];
            let mut output = Vec::new();
            let result = copy_limited(&mut input.as_slice(), &mut output, 64);
            assert_eq!(result.is_ok(), length <= 64);
            assert_eq!(output.len(), length.min(64));
        }
        let mut output = Vec::new();
        assert!(copy_limited(&mut io::repeat(b'x'), &mut output, 64).is_err());
        assert_eq!(output.len(), 64);
    }
}
