//! Repository queries exercise real metadata bytes, with no installed RPM tooling.

use anolisa_platform::command::{CommandOutput, CommandRunner};
use anolisa_platform::pkg_query::{PackageQuery, PackageQueryError};
use anolisa_platform::rpm_query::RpmPackageQuery;
use anolisa_platform::rpm_repo::RpmRepoSource;
use rpmrepo_metadata::{CompressionType, utils};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Write};

struct NoCommands;
impl CommandRunner for NoCommands {
    fn run(&self, program: &str, _: &[&str]) -> std::io::Result<CommandOutput> {
        panic!("repository query must not spawn {program}");
    }
}

fn primary() -> String {
    let mut xml = String::from(
        r#"<?xml version="1.0"?><metadata xmlns="http://linux.duke.edu/metadata/common" xmlns:rpm="http://linux.duke.edu/metadata/rpm" packages="3">"#,
    );
    for (epoch, version, arch) in [
        ("0", "1.0", "noarch"),
        ("1", "2.0", "x86_64"),
        ("0", "2.0", "aarch64"),
    ] {
        xml.push_str(&format!(r#"<package type="rpm"><name>probe</name><arch>{arch}</arch><version epoch="{epoch}" ver="{version}" rel="2.al8"/><checksum type="sha256" pkgid="YES">{}</checksum><summary>probe</summary><description>fixture</description><packager/><url/><time file="0" build="0"/><size package="1" installed="1" archive="1"/><location href="probe.rpm"/><format><rpm:license>MIT</rpm:license><rpm:vendor/><rpm:group>test</rpm:group><rpm:buildhost>test</rpm:buildhost><rpm:sourcerpm>probe.src.rpm</rpm:sourcerpm><rpm:header-range start="0" end="0"/><rpm:provides><rpm:entry name="anolisa-component(probe)"/><rpm:entry name="probe" flags="EQ" epoch="{epoch}" ver="{version}" rel="2.al8"/></rpm:provides></format></package>"#, "0".repeat(64)));
    }
    xml.push_str("</metadata>");
    xml
}

fn fixture(xml: &str, compression: CompressionType) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("repodata")).unwrap();
    let (path, mut writer) =
        utils::writer_to_file(&dir.path().join("repodata/primary.xml"), compression).unwrap();
    writer.write_all(xml.as_bytes()).unwrap();
    drop(writer);
    let bytes = fs::read(&path).unwrap();
    fs::write(dir.path().join("repodata/repomd.xml"), format!(r#"<repomd xmlns="http://linux.duke.edu/metadata/repo"><data type="primary"><checksum type="sha256">{:x}</checksum><open-checksum type="sha256">{:x}</open-checksum><location href="repodata/{}"/><size>{}</size><open-size>{}</open-size><timestamp>0</timestamp></data></repomd>"#, Sha256::digest(&bytes), Sha256::digest(xml.as_bytes()), path.file_name().unwrap().to_str().unwrap(), bytes.len(), xml.len())).unwrap();
    dir
}

fn query(dir: &tempfile::TempDir) -> RpmPackageQuery<NoCommands> {
    RpmPackageQuery::with_runner_and_repo(
        NoCommands,
        RpmRepoSource::new(
            "fixture",
            url::Url::from_directory_path(dir.path())
                .unwrap()
                .to_string(),
            Some(true),
        ),
    )
}

#[test]
fn reads_all_formats_versions_arches_and_provides_once() {
    for format in [
        CompressionType::None,
        CompressionType::Gzip,
        CompressionType::Bz2,
        CompressionType::Xz,
        CompressionType::Zstd,
    ] {
        let dir = fixture(&primary(), format);
        let query = query(&dir);
        assert_eq!(query.installed_origin("probe").unwrap(), None);
        let packages = query.query_available("probe").unwrap();
        assert_eq!(packages.len(), 3);
        assert_eq!(packages[0].version.epoch, None);
        assert_eq!(packages[1].version.to_string(), "1:2.0-2.al8");
        assert_eq!(
            packages.iter().map(|p| p.arch.as_str()).collect::<Vec<_>>(),
            ["noarch", "x86_64", "aarch64"]
        );
        assert!(
            packages
                .iter()
                .all(|p| p.origin.as_deref() == Some("fixture"))
        );
        fs::remove_dir_all(dir.path().join("repodata")).unwrap();
        assert_eq!(query.query_available("probe").unwrap(), packages);
        assert_eq!(
            query
                .what_provides_available("anolisa-component(probe)")
                .unwrap(),
            ["probe"]
        );
        let provides = query.provided_capabilities_available("probe").unwrap();
        assert!(provides.contains(&"probe = 1:2.0-2.al8".into()));
        assert_eq!(
            provides
                .iter()
                .filter(|p| *p == "anolisa-component(probe)")
                .count(),
            1
        );
        assert!(query.query_available("absent").unwrap().is_empty());
        assert!(query.what_provides_available("absent").unwrap().is_empty());
        assert!(
            query
                .provided_capabilities_available("absent")
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn corruption_and_incomplete_metadata_are_errors_not_package_misses() {
    for case in [
        "download",
        "checksum",
        "open-checksum",
        "size",
        "open-size",
        "missing-primary",
        "bad-xml",
        "unsupported-checksum",
    ] {
        let dir = fixture(&primary(), CompressionType::None);
        let index = dir.path().join("repodata/repomd.xml");
        let mut metadata = fs::read_to_string(&index).unwrap();
        match case {
            "download" => fs::remove_file(dir.path().join("repodata/primary.xml")).unwrap(),
            "checksum" => {
                metadata = metadata.replacen(
                    &format!("{:x}", Sha256::digest(primary().as_bytes())),
                    &"0".repeat(64),
                    1,
                );
            }
            "open-checksum" => {
                let start = metadata.find("<open-checksum type=\"sha256\">").unwrap()
                    + "<open-checksum type=\"sha256\">".len();
                metadata.replace_range(start..start + 64, &"0".repeat(64));
            }
            "size" => {
                metadata = metadata.replace(
                    &format!("<size>{}</size>", primary().len()),
                    "<size>1</size>",
                )
            }
            "open-size" => {
                metadata = metadata.replace(
                    &format!("<open-size>{}</open-size>", primary().len()),
                    "<open-size>1</open-size>",
                )
            }
            "missing-primary" => metadata = metadata.replace("type=\"primary\"", "type=\"other\""),
            "bad-xml" => metadata = "<repomd><data".into(),
            "unsupported-checksum" => metadata = metadata.replace("sha256", "unsupported"),
            _ => unreachable!(),
        }
        fs::write(index, metadata).unwrap();
        let result = query(&dir).query_available("probe");
        assert!(
            matches!(result, Err(PackageQueryError::Repository(_))),
            "{case}: {result:?}"
        );
    }
}

#[test]
fn truncated_metadata_returns_errors_without_hanging() {
    const CHILD_CASE: &str = "ANOLISA_TEST_TRUNCATED_METADATA";
    if let Ok(case) = std::env::var(CHILD_CASE) {
        let xml = match case.as_str() {
            "primary-format" => "<metadata packages=\"1\"><package type=\"rpm\"><format>",
            "primary-provides" => {
                "<metadata packages=\"1\"><package type=\"rpm\"><format><rpm:provides>"
            }
            "primary-root" => "<metadata packages=\"0\">",
            "primary-mismatch" => "<metadata packages=\"0\"></other>",
            _ => "<metadata packages=\"0\"></metadata>",
        };
        // The fixture computes matching sizes and checksums even for broken XML.
        let dir = fixture(xml, CompressionType::Gzip);
        match case.as_str() {
            "index-data" => fs::write(
                dir.path().join("repodata/repomd.xml"),
                "<repomd><data type=\"primary\">",
            )
            .unwrap(),
            "index-tags" => {
                fs::write(dir.path().join("repodata/repomd.xml"), "<repomd><tags>").unwrap()
            }
            _ => {}
        }
        let result = query(&dir).query_available("probe");
        assert!(
            matches!(result, Err(PackageQueryError::Repository(_))),
            "{case}: {result:?}"
        );
        return;
    }

    // Isolate the parser so an EOF regression fails this test instead of hanging CI.
    let mut failures = Vec::new();
    for case in [
        "index-data",
        "index-tags",
        "primary-format",
        "primary-provides",
        "primary-root",
        "primary-mismatch",
    ] {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "truncated_metadata_returns_errors_without_hanging",
                "--nocapture",
            ])
            .env(CHILD_CASE, case)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                child.kill().unwrap();
                failures.push(format!("{case}: parser exceeded five seconds"));
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        if !output.status.success() {
            failures.push(format!(
                "{case}: {}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn primary_header_requires_a_valid_package_count() {
    for (xml, valid) in [
        ("<metadata/>", false),
        ("<metadata></metadata>", false),
        (
            "<?xml version=\"1.0\"?><metadata xmlns=\"http://linux.duke.edu/metadata/common\"/>",
            false,
        ),
        ("<metadata packages=\"\"/>", false),
        ("<metadata packages=\"invalid\"/>", false),
        ("<metadata packages=\"-1\"/>", false),
        ("<metadata packages=\"18446744073709551616\"/>", false),
        ("<metadata packages=\"1\"/>", false),
        ("<metadata packages=\"0\"/>", true),
        ("<metadata packages=\"0\"></metadata>", true),
        (
            "<?xml version=\"1.0\"?><metadata xmlns=\"http://linux.duke.edu/metadata/common\" packages=\"0\"/>",
            true,
        ),
    ] {
        let dir = fixture(xml, CompressionType::Gzip);
        let result = query(&dir).query_available("probe");
        if valid {
            assert!(result.unwrap().is_empty(), "{xml}");
        } else {
            assert!(
                matches!(result, Err(PackageQueryError::Repository(_))),
                "{xml}: {result:?}"
            );
        }
    }
}

#[test]
fn empty_or_unconfigured_repository_has_no_available_candidates() {
    let xml = r#"<metadata xmlns="http://linux.duke.edu/metadata/common" packages="0"></metadata>"#;
    let dir = fixture(xml, CompressionType::None);
    assert!(query(&dir).query_available("probe").unwrap().is_empty());
    let query = RpmPackageQuery::with_runner(NoCommands);
    assert_eq!(query.installed_origin("probe").unwrap(), None);
    assert!(query.query_available("probe").unwrap().is_empty());
    assert!(
        query
            .what_provides_available("anolisa-component(probe)")
            .unwrap()
            .is_empty()
    );
    assert!(
        query
            .provided_capabilities_available("probe")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn http_fetches_only_index_and_primary_then_reuses_snapshot() {
    let dir = fixture(&primary(), CompressionType::Gzip);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for expected in ["/repodata/repomd.xml", "/repodata/primary.xml.gz"] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            let count = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..count]).starts_with(&format!("GET {expected} "))
            );
            let bytes = fs::read(dir.path().join(expected.trim_start_matches('/'))).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bytes.len()
            )
            .unwrap();
            stream.write_all(&bytes).unwrap();
        }
    });
    let query = RpmPackageQuery::with_runner_and_repo(
        NoCommands,
        RpmRepoSource::new("http", format!("http://{address}"), None),
    );
    assert_eq!(query.query_available("probe").unwrap().len(), 3);
    server.join().unwrap();
    assert_eq!(query.query_available("probe").unwrap().len(), 3);
}

#[test]
fn primary_locations_preserve_authentication_only_within_the_repository_origin() {
    use base64::Engine;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    for case in [
        "relative",
        "absolute",
        "scheme-relative",
        "explicit",
        "cross-origin",
    ] {
        let dir = fixture(&primary(), CompressionType::Gzip);
        let index_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = index_listener.local_addr().unwrap();
        let primary_listener = if case == "cross-origin" {
            TcpListener::bind("127.0.0.1:0").unwrap()
        } else {
            index_listener.try_clone().unwrap()
        };
        let primary_address = primary_listener.local_addr().unwrap();
        let href = match case {
            "relative" => "repodata/primary.xml.gz".to_string(),
            "scheme-relative" => format!("//{address}/repodata/primary.xml.gz"),
            "explicit" => format!("http://other:new%2Fsecret@{address}/repodata/primary.xml.gz"),
            _ => format!("http://{primary_address}/repodata/primary.xml.gz"),
        };
        let index = dir.path().join("repodata/repomd.xml");
        fs::write(
            &index,
            fs::read_to_string(&index).unwrap().replace(
                "href=\"repodata/primary.xml.gz\"",
                &format!("href=\"{href}\""),
            ),
        )
        .unwrap();
        let primary_credentials = match case {
            "explicit" => Some("other:new/secret"),
            "cross-origin" => None,
            _ => Some("user:p/ss%2F"),
        };
        let server = std::thread::spawn(move || {
            for (listener, path, credentials) in [
                (index_listener, "/repodata/repomd.xml", Some("user:p/ss%2F")),
                (
                    primary_listener,
                    "/repodata/primary.xml.gz",
                    primary_credentials,
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                assert!(
                    request.starts_with(&format!("GET {path} ")),
                    "{case}: {request}"
                );
                let mut authorization = None;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("authorization")
                    {
                        authorization = Some(value.trim().to_string());
                    }
                }
                let expected = credentials.map(|value| {
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(value)
                    )
                });
                if authorization != expected {
                    stream.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                    panic!("{case}: authorization {authorization:?}, expected {expected:?}");
                }
                let body = fs::read(dir.path().join(path.trim_start_matches('/'))).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let query = RpmPackageQuery::with_runner_and_repo(
            NoCommands,
            RpmRepoSource::new(
                "private",
                format!("http://user:p%2Fss%252F@{address}"),
                None,
            ),
        );
        let result = query.query_available("probe");
        server.join().unwrap();
        assert_eq!(result.unwrap().len(), 3, "{case}");
    }
}

#[test]
fn metadata_errors_do_not_expose_repository_secrets() {
    for userinfo in ["user:secret@", "user:enc%2Fsecret@", ""] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            assert!(stream.read(&mut request).unwrap() > 0);
            stream.write_all(b"HTTP/1.1 404 Secret-Status-Text\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let query = RpmPackageQuery::with_runner_and_repo(
            NoCommands,
            RpmRepoSource::new(
                "private",
                format!("http://{userinfo}{address}/path-token?key=query-token#fragment-token"),
                None,
            ),
        );
        let error = query.query_available("probe").unwrap_err();
        let rendered = format!(
            "{error}\n{error:?}\n{}",
            serde_json::json!({"reason":error.to_string()})
        );
        server.join().unwrap();
        for secret in [
            "user",
            "secret",
            "enc%2F",
            "path-token",
            "query-token",
            "fragment-token",
            "Secret-Status-Text",
        ] {
            assert!(!rendered.contains(secret), "leaked {secret}: {rendered}");
        }
        assert!(rendered.contains("404"));
        assert!(rendered.contains(&address.to_string()));
    }
}

#[test]
fn declared_metadata_limits_fail_before_primary_download() {
    for (tag, size) in [
        ("size", 128 * 1024 * 1024 + 1),
        ("open-size", 512 * 1024 * 1024 + 1),
    ] {
        let dir = fixture(&primary(), CompressionType::None);
        let path = dir.path().join("repodata/repomd.xml");
        let xml = fs::read_to_string(&path).unwrap();
        let start = xml.find(&format!("<{tag}>")).unwrap() + tag.len() + 2;
        let end = xml[start..].find('<').unwrap() + start;
        fs::write(&path, format!("{}{size}{}", &xml[..start], &xml[end..])).unwrap();
        fs::remove_file(dir.path().join("repodata/primary.xml")).unwrap();
        let err = query(&dir)
            .query_available("probe")
            .unwrap_err()
            .to_string();
        assert!(err.contains("declared primary metadata size"), "{err}");
    }
}

#[test]
fn highly_compressed_primary_cannot_exceed_declared_open_size() {
    let xml = " ".repeat(64 * 1024);
    let dir = fixture(&xml, CompressionType::Gzip);
    let path = dir.path().join("repodata/repomd.xml");
    let index = fs::read_to_string(&path).unwrap().replace(
        &format!("<open-size>{}</open-size>", xml.len()),
        "<open-size>1024</open-size>",
    );
    fs::write(path, index).unwrap();
    let err = query(&dir)
        .query_available("probe")
        .unwrap_err()
        .to_string();
    assert!(err.contains("1024-byte limit"), "{err}");
}

#[test]
fn index_is_not_transparently_decompressed_beyond_the_download_limit() {
    let dir = fixture(&primary(), CompressionType::None);
    let index = dir.path().join("repodata/repomd.xml");
    let mut xml = fs::read(&index).unwrap();
    xml.extend(vec![b' '; 4 * 1024 * 1024]);
    let (compressed, mut writer) = utils::writer_to_file(&index, CompressionType::Gzip).unwrap();
    writer.write_all(&xml).unwrap();
    drop(writer);
    fs::rename(compressed, index).unwrap();
    assert!(query(&dir).query_available("probe").is_err());
}
