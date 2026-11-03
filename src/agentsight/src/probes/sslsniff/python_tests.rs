use super::*;

#[test]
fn python_ssl_images_include_shared_and_static_builds() {
    for path in [
        "/opt/python/bin/python3.11",
        "/opt/python/bin/python3.14 (deleted)",
        "/opt/python/lib/libpython3.11.so.1.0",
        "/opt/python/lib/libpython3.13t.so.1.0",
        "/opt/python/lib/libpython3.11.so.1.0 (deleted)",
    ] {
        assert_eq!(
            classify_ssl_lib(path),
            Some(SslLibKind::Python),
            "Python SSL candidate was missed: {path}"
        );
    }
    for path in ["/usr/bin/python3", "/opt/python/lib/libpython3.11.a"] {
        assert_eq!(classify_ssl_lib(path), None);
    }
}

#[test]
fn uv_shared_python_maps_discover_the_ssl_bearing_library() {
    let maps = b"00400000-00401000 r--p 00000000 08:01 11 /opt/python/bin/python3.11\n\
00401000-00402000 r-xp 00001000 08:01 11 /opt/python/bin/python3.11\n\
70000000-70200000 r--p 00000000 08:01 22 /opt/python/lib/libpython3.11.so.1.0\n\
70200000-71000000 r-xp 00200000 08:01 22 /opt/python/lib/libpython3.11.so.1.0\n";
    let libs = ssl_libs_from_maps_text(maps, 4242);
    assert_eq!(libs.len(), 2, "each mapped image is inspected once");
    let library = libs
        .iter()
        .find(|(_, _, _, mapped)| mapped.ends_with("libpython3.11.so.1.0"))
        .expect("uv's SSL-bearing libpython must be discovered");
    assert_eq!(library.2, SslLibKind::Python);
    assert!(
        library
            .0
            .ends_with("/root/opt/python/lib/libpython3.11.so.1.0")
    );
    assert!(!library.0.ends_with("/exe"));
}

#[test]
fn deleted_libpython_uses_the_mapped_inode() {
    let maps = b"70000000-70200000 r-xp 00000000 08:01 22 \
                 /opt/python/lib/libpython3.11.so.1.0 (deleted)\n";
    let libs = ssl_libs_from_maps_text(maps, 4242);
    assert_eq!(libs.len(), 1);
    assert_eq!(libs[0].2, SslLibKind::Python);
    assert!(libs[0].0.ends_with("/map_files/70000000-70200000"));
}

#[test]
fn python_images_without_ssl_are_not_probed() {
    let dir = tempfile::tempdir().unwrap();
    let executable = dir.path().join("python3.11");
    let library = dir.path().join("libpython3.11.so.1.0");
    fs::write(&executable, b"small Python launcher without embedded SSL").unwrap();
    fs::write(&library, b"\0SSL_write\0SSL_read\0SSL_do_handshake\0").unwrap();

    assert!(!python_ssl_present(executable.to_str().unwrap()).unwrap());
    assert!(matches!(
        attach_python_ssl(4242, executable.to_str().unwrap(), || {
            panic!("a launcher without SSL symbols must not attempt attachment")
        }),
        AttachOutcome::Untraceable
    ));
    assert!(python_ssl_present(library.to_str().unwrap()).unwrap());
    assert!(python_ssl_present(dir.path().join("missing").to_str().unwrap()).is_err());
}

#[test]
fn python_image_inspection_and_attachment_failures_allow_later_retry() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("libpython3.11.so.1.0");
    let path = image.to_str().unwrap();
    assert!(matches!(
        attach_python_ssl(4242, path, || {
            panic!("an unreadable image must not attempt attachment")
        }),
        AttachOutcome::Failed
    ));
    fs::write(&image, b"\0SSL_write\0").unwrap();
    let mut attempts = 0;
    assert!(matches!(
        attach_python_ssl(4242, path, || {
            attempts += 1;
            Err(anyhow::anyhow!("transient fixture attachment failure"))
        }),
        AttachOutcome::Failed
    ));
    assert!(matches!(
        attach_python_ssl(4242, path, || {
            attempts += 1;
            Ok(Vec::new())
        }),
        AttachOutcome::Attached(_)
    ));
    assert_eq!(attempts, 2);
}

#[test]
fn python_ssl_prefilter_requires_the_complete_symbol_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("python3.11");
    fs::write(&path, b"\0SSL_write_ex\0SSL_write_extra\0").unwrap();
    assert!(!python_ssl_present(path.to_str().unwrap()).unwrap());
    fs::write(&path, b"\0SSL_write\0").unwrap();
    assert!(python_ssl_present(path.to_str().unwrap()).unwrap());
}

#[test]
#[ignore = "requires AGENTSIGHT_TEST_PYTHON pointing to uv Python 3.11.6"]
fn uv_python_shared_openssl_is_discovered() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::{Command, Stdio};

    let python = std::env::var_os("AGENTSIGHT_TEST_PYTHON").expect("set AGENTSIGHT_TEST_PYTHON");
    let mut child = Command::new(python)
        .args(["-u", "-c", "import ssl; print('ready'); input()"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("start the selected Python interpreter");
    let mut ready = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let discovered = ssl_libs_from_maps(child.id() as i32).map(|libs| {
        libs.into_iter()
            .filter(|(path, _, kind, _)| {
                *kind == SslLibKind::Python && python_ssl_present(path).unwrap_or(false)
            })
            .collect::<Vec<_>>()
    });
    let _ = writeln!(child.stdin.take().unwrap());
    let _ = child.wait();

    assert_eq!(ready.trim(), "ready");
    let libs = discovered.unwrap();
    assert_eq!(
        libs.len(),
        1,
        "select the SSL-bearing library, not the launcher"
    );
    assert!(libs[0].3.ends_with("libpython3.11.so.1.0"));
}
