use super::*;

#[test]
fn null_probe_attach_error_carries_errno_without_overwriting_known_errors() {
    use std::io;
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("missing");
    let null = libbpf_rs::Error::from(io::Error::other("bpf call returned NULL"));
    let io_error = fs::File::open(&absent).unwrap_err();
    let errno = io_error.raw_os_error().unwrap();
    let message = format!(
        "{:#}",
        probe_attach_error(null, || "uretprobe SSL_write@fixture".into())
    );
    assert!(message.contains(&format!("errno={errno}")), "{message}");
    assert!(message.contains("returned NULL"));
    let known = libbpf_rs::Error::from_raw_os_error(libc::EPERM);
    let _ = fs::File::open(absent).unwrap_err();
    let message = format!(
        "{:#}",
        probe_attach_error(known, || "uprobe fixture".into())
    );
    assert!(
        !message.contains("libbpf errno="),
        "不能用残留 errno 覆盖已知错误"
    );
    assert!(message.contains("os error 1"), "{message}");
}

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

static EXIT_DURING_ATTACH_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static EXIT_DURING_ATTACH_OBSERVED: AtomicBool = AtomicBool::new(false);

fn exit_after_first_symbol(_level: libbpf_rs::PrintLevel, message: String) {
    if !message.contains("symbol address match for 'SSL_write'") {
        return;
    }
    let pid = EXIT_DURING_ATTACH_PID.swap(0, Ordering::SeqCst);
    if pid == 0 {
        return;
    }
    // PID 仅来自本测试刚启动且仍由 Child 持有的夹具，不作用于服务进程。
    if unsafe { libc::kill(pid as i32, libc::SIGKILL) } != 0 {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if fs::metadata(proc_pid_entry(pid as i32, "root")).is_err() {
            EXIT_DURING_ATTACH_OBSERVED.store(true, Ordering::SeqCst);
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

struct RestorePrint(Option<(libbpf_rs::PrintLevel, libbpf_rs::PrintCallback)>);

impl Drop for RestorePrint {
    fn drop(&mut self) {
        EXIT_DURING_ATTACH_PID.store(0, Ordering::SeqCst);
        libbpf_rs::set_print(self.0);
    }
}

#[test]
#[ignore = "需真实内核和动态 OpenSSL；必须单线程，定向注入夹具挂载中退出"]
fn dynamic_ssl_attach_survives_owner_exit_between_symbols() {
    let guard = super::tests::LEARNED_PATH_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut sniffer = super::tests::sniffer_with_isolated_learned_cache(&guard, &dir);
    let mut owner = TlsChild::spawn(TLS_CLIENT, &["churn", "unused"]);
    assert_eq!(owner.line(), "ready");
    let pid = owner.child.id();
    let (path, file, kind, mapped_path) = ssl_libs_from_maps(pid as i32)
        .unwrap()
        .into_iter()
        .find(|(_, _, kind, _)| *kind == SslLibKind::OpenSsl)
        .expect("需要动态 libssl 映射");
    EXIT_DURING_ATTACH_OBSERVED.store(false, Ordering::SeqCst);
    let restore = RestorePrint(libbpf_rs::set_print(Some((
        libbpf_rs::PrintLevel::Debug,
        exit_after_first_symbol,
    ))));
    EXIT_DURING_ATTACH_PID.store(pid, Ordering::SeqCst);
    let outcome = sniffer.build_attach(pid as i32, file, &path, kind, &mapped_path);
    drop(restore);
    let status = owner.child.wait().unwrap();
    assert!(!status.success(), "夹具必须在挂载期间退出");
    assert!(EXIT_DURING_ATTACH_OBSERVED.load(Ordering::SeqCst));
    assert!(fs::metadata(&path).is_err(), "原进程路径应失效");
    assert!(
        matches!(outcome, AttachOutcome::Attached(_)),
        "首符号解析后原进程退出，后续探针仍应挂到已经固定的同一文件"
    );
    eprintln!("挂载中退出后全部动态 SSL 链接创建成功");
}

struct TlsChild {
    child: std::process::Child,
    lines: std::sync::mpsc::Receiver<String>,
}

impl TlsChild {
    fn spawn(script: &str, args: &[&str]) -> Self {
        use std::io::{BufRead, BufReader};
        use std::process::{Command, Stdio};
        let python =
            std::env::var_os("AGENTSIGHT_TEST_DYNAMIC_PYTHON").unwrap_or_else(|| "python3".into());
        let mut child = Command::new(python)
            .args(["-u", "-c", script])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("启动动态 OpenSSL 测试进程");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.expect("读取夹具状态")).is_err() {
                    break;
                }
            }
        });
        Self { child, lines }
    }

    fn line(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(10))
            .expect("夹具必须在有界时间内就绪")
    }

    fn signal(&mut self) {
        use std::io::Write;
        writeln!(self.child.stdin.as_mut().unwrap()).unwrap();
    }
}

impl Drop for TlsChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const TLS_SERVER: &str = r#"
import socket, ssl, sys
context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
context.load_cert_chain(sys.argv[1], sys.argv[2])
server = socket.socket()
server.bind(('127.0.0.1', 0))
server.listen(8)
server.settimeout(15)
print(server.getsockname()[1], flush=True)
while True:
    raw, _ = server.accept()
    raw.settimeout(5)
    with context.wrap_socket(raw, server_side=True) as connection:
        request = b''
        while b'\r\n\r\n' not in request:
            chunk = connection.recv(4096)
            if not chunk:
                raise RuntimeError('incomplete request')
            request += chunk
        body = b'data: {"choices":[{"delta":{"content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}\n\ndata: [DONE]\n\n'
        connection.sendall(b'HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: ' + str(len(body)).encode() + b'\r\nConnection: close\r\n\r\n' + body)
"#;

const TLS_CLIENT: &str = r#"
import socket, ssl, sys
context = ssl._create_unverified_context()
print('ready', flush=True)
sys.stdin.readline()
if sys.argv[1] != 'churn':
    with context.wrap_socket(socket.create_connection(('127.0.0.1', int(sys.argv[1])), timeout=5), server_hostname='localhost') as connection:
        connection.sendall(b'POST /v1/chat/completions HTTP/1.1\r\nHost: localhost\r\nX-Churn-Test: ' + sys.argv[2].encode() + b'\r\nContent-Length: 0\r\n\r\n')
        response = b''
        while True:
            chunk = connection.recv(4096)
            if not chunk:
                break
            response += chunk
        assert b'200 OK' in response and b'[DONE]' in response
    print('done', flush=True)
    sys.stdin.readline()
"#;

#[test]
#[ignore = "需要 CAP_BPF/CAP_PERFMON、openssl CLI 及动态链接 OpenSSL 的 Python"]
fn dynamic_ssl_capture_survives_short_lived_processes() {
    use libbpf_rs::MapFlags;
    use std::process::Command;

    // 独立 skeleton、证书和进程；不读取模型凭证、不接触现有 trace 服务。
    let guard = super::tests::LEARNED_PATH_LOCK.lock().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let cert = dir.path().join("cert.pem");
    let key = dir.path().join("key.pem");
    let output = Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=localhost",
            "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .unwrap();
    assert!(output.status.success(), "生成本地 TLS 证书失败");
    let server = TlsChild::spawn(TLS_SERVER, &[cert.to_str().unwrap(), key.to_str().unwrap()]);
    let port = server.line();
    port.parse::<u16>().expect("服务必须返回监听端口");
    let mut sniffer = super::tests::sniffer_with_isolated_learned_cache(&guard, &dir);
    let _poller = sniffer.run().unwrap();
    let mut seed = TlsChild::spawn(TLS_CLIENT, &[&port, "seed"]);
    assert_eq!(seed.line(), "ready");
    let seed_pid = seed.child.id();
    let library = ssl_libs_from_maps(seed_pid as i32)
        .unwrap()
        .into_iter()
        .find(|(_, _, kind, _)| *kind == SslLibKind::OpenSsl)
        .expect("请选择动态链接 libssl 的 Python");
    let file = library.1;
    sniffer.attach_process(seed_pid as i32).unwrap();
    sniffer
        .skel
        .maps()
        .traced_processes()
        .update(&seed_pid.to_ne_bytes(), &1u32.to_ne_bytes(), MapFlags::ANY)
        .unwrap();
    seed.signal();
    assert_eq!(seed.line(), "done");
    assert_tls_directions(&sniffer, seed_pid, "seed");
    let link_count = sniffer.attachments[&file]._links.len();

    for round in 0..3 {
        let mut churn = TlsChild::spawn(TLS_CLIENT, &["churn", "unused"]);
        assert_eq!(churn.line(), "ready");
        let churn_pid = churn.child.id();
        sniffer.attach_process(churn_pid as i32).unwrap();
        churn.signal();
        assert!(churn.child.wait().unwrap().success());
        sniffer.detach_process(churn_pid);

        let marker = format!("churn-{round}");
        let mut client = TlsChild::spawn(TLS_CLIENT, &[&port, &marker]);
        assert_eq!(client.line(), "ready");
        let pid = client.child.id();
        sniffer.attach_process(pid as i32).unwrap();
        if round == 2 {
            // 不等待 30 秒：直接老化同一 PID 的挂载，覆盖 TTL 恢复路径。
            sniffer.attachments.get_mut(&file).unwrap().attached_at =
                Instant::now() - STALE_REATTACH_TTL;
            sniffer.attach_process(pid as i32).unwrap();
        }
        sniffer
            .skel
            .maps()
            .traced_processes()
            .update(&pid.to_ne_bytes(), &1u32.to_ne_bytes(), MapFlags::ANY)
            .unwrap();
        client.signal();
        assert_eq!(client.line(), "done");
        assert_tls_directions(&sniffer, pid, &marker);
        assert_eq!(sniffer.attachments[&file]._links.len(), link_count);
        client.signal();
        assert!(client.child.wait().unwrap().success());
        sniffer
            .skel
            .maps()
            .traced_processes()
            .delete(&pid.to_ne_bytes())
            .unwrap();
        sniffer.detach_process(pid);
    }
    seed.signal();
    assert!(seed.child.wait().unwrap().success());
    sniffer.detach_process(seed_pid);
    assert!(!sniffer.attachments.contains_key(&file));
}

fn assert_tls_directions(sniffer: &SslSniff, pid: u32, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut request = false;
    let mut response = false;
    while Instant::now() < deadline && !(request && response) {
        if let Ok(event) = sniffer.rx.recv_timeout(Duration::from_millis(100)) {
            if event.pid == pid {
                request |= event.is_http_request()
                    && event
                        .buf
                        .windows(marker.len())
                        .any(|bytes| bytes == marker.as_bytes());
                response |= event.is_http_response();
            }
        }
    }
    assert!(
        request && response,
        "进程 {pid} 的 TLS 请求/响应不完整：request={request}, response={response}"
    );
}
