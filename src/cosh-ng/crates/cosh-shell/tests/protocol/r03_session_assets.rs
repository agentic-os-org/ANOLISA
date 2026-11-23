use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{build_adapter, AdapterInstance, AdapterKind};
use crate::adapter::{
    spawn_provider_child, PreparedInvocation, ProviderPromptArgMode, ProviderStdinMode,
};

struct RunningChild(Child);

impl Drop for RunningChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn compile_native(directory: &Path, name: &str) {
    let source = directory.join(format!("{name}.rs"));
    fs::write(
        &source,
        format!(
            r#"use std::io::{{self, Write}};
fn main() {{
    use std::os::unix::fs::MetadataExt;
    let mut descriptors = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd").unwrap() {{
        if let Ok(metadata) = entry.unwrap().path().metadata() {{
            if metadata.is_file() {{ descriptors.push((metadata.dev(), metadata.ino())); }}
        }}
    }}
    eprintln!("FD_REGULAR={{descriptors:?}}");
    println!("{name}");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
}}
"#
        ),
    )
    .unwrap();
    let executable = directory.join(name);
    let output = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .arg("--edition=2021")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .output()
        .unwrap();
    assert!(output.status.success(), "rustc: {output:?}");
    let bytes = fs::read(&executable).unwrap();
    assert_eq!(&bytes[..4], b"\x7fELF");
    println!("{name} SHA256={:x}", Sha256::digest(&bytes));
}

fn observe_native(child: Child) -> (String, u64, u64) {
    let mut child = RunningChild(child);
    let stdout = child.0.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let result = BufReader::new(stdout).read_line(&mut line);
        let _ = sender.send((result, line));
    });
    let (result, line) = receiver.recv_timeout(Duration::from_secs(10)).unwrap();
    result.unwrap();
    reader.join().unwrap();
    let pid = child.0.id();
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let start_time = stat
        .rsplit_once(") ")
        .unwrap()
        .1
        .split_whitespace()
        .nth(19)
        .unwrap();
    let executable = fs::metadata(format!("/proc/{pid}/exe")).unwrap();
    let maps = fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
    assert!(maps.lines().any(|mapping| {
        let fields: Vec<_> = mapping.split_whitespace().collect();
        fields.get(1).is_some_and(|mode| mode.contains('x'))
            && fields.get(4).and_then(|inode| inode.parse::<u64>().ok()) == Some(executable.ino())
    }));
    println!(
        "marker={} pid={pid} start={start_time} dev={} ino={}\n{maps}",
        line.trim(),
        executable.dev(),
        executable.ino()
    );
    child.0.stdin.take().unwrap().write_all(b"exit\n").unwrap();
    assert!(child.0.wait().unwrap().success());
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    println!("pid={pid} {stderr}");
    assert_eq!(
        stderr.trim(),
        "FD_REGULAR=[]",
        "native file descriptor leak"
    );
    (line.trim().to_owned(), executable.dev(), executable.ino())
}

fn provider_child(program: &str) -> Child {
    spawn_provider_child(
        &PreparedInvocation {
            program: program.to_owned(),
            args: Vec::new(),
            prompt: String::new(),
        },
        "r03-native-fd",
        ProviderStdinMode::Piped,
        ProviderPromptArgMode::None,
    )
    .unwrap()
}

fn gateway_child() -> Child {
    crate::adapter::command_with_trusted_startup_path(crate::adapter::gateway_executable().unwrap())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

fn write_script(path: &Path, marker: &str) {
    fs::write(path, format!("#!/bin/sh\nprintf '{marker}\\n'\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn admitted_core_survives_atomic_path_replacement() {
    const WORKER: &str = "COSH_R03_NATIVE_FD_TEST_WORKER";
    const PHASE: &str = "COSH_R03_NATIVE_FD_TEST_PHASE";
    if std::env::var_os(WORKER).is_none() {
        let directory = tempfile::tempdir().unwrap();
        let directory = directory.path();
        for name in ["OLD", "NEW", "OLD_GATEWAY", "NEW_GATEWAY"] {
            compile_native(directory, name);
        }
        assert_ne!(
            Sha256::digest(fs::read(directory.join("OLD")).unwrap()),
            Sha256::digest(fs::read(directory.join("NEW")).unwrap())
        );
        fs::create_dir(directory.join("external-bin")).unwrap();
        fs::copy(
            directory.join("NEW"),
            directory.join("external-bin/cosh-core"),
        )
        .unwrap();
        fs::rename(directory.join("OLD"), directory.join("cosh-core")).unwrap();
        fs::rename(
            directory.join("OLD_GATEWAY"),
            directory.join("cosh-gateway"),
        )
        .unwrap();
        write_script(&directory.join("script-core"), "SCRIPT_CORE");
        write_script(&directory.join("script-gateway"), "SCRIPT_GATEWAY");
        write_script(&directory.join("external-core"), "WRONG_PATH");
        fs::create_dir(directory.join("path-bin")).unwrap();
        write_script(&directory.join("path-bin/external-core"), "PATH_CORE");
        nix::unistd::mkfifo(&directory.join("fifo-core"), nix::sys::stat::Mode::S_IRWXU).unwrap();
        let test_module = module_path!().split_once("::").unwrap().1;
        for phase in ["pinned", "fresh", "script", "path", "fifo"] {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .arg("--exact")
                .arg(format!(
                    "{test_module}::admitted_core_survives_atomic_path_replacement"
                ))
                .arg("--nocapture")
                .env(WORKER, directory)
                .env(PHASE, phase)
                .env("COSH_CORE_PATH", directory.join("cosh-core"))
                .env("COSH_GATEWAY_EXECUTABLE", directory.join("cosh-gateway"));
            if phase == "script" {
                command
                    .env("COSH_CORE_PATH", directory.join("script-core"))
                    .env("COSH_GATEWAY_EXECUTABLE", directory.join("script-gateway"));
            } else if phase == "path" {
                command
                    .env("COSH_CORE_PATH", "external-core")
                    .env_remove("COSH_GATEWAY_EXECUTABLE")
                    .env(
                        "PATH",
                        format!("{}:{}", directory.display(), std::env::var("PATH").unwrap()),
                    );
            } else if phase == "fifo" {
                command.env("COSH_CORE_PATH", directory.join("fifo-core"));
            }
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            let mut worker = RunningChild(command.spawn().unwrap());
            let mut stdout_pipe = worker.0.stdout.take().unwrap();
            let mut stderr_pipe = worker.0.stderr.take().unwrap();
            let stdout_reader = std::thread::spawn(move || {
                let mut output = String::new();
                stdout_pipe.read_to_string(&mut output).unwrap();
                output
            });
            let stderr_reader = std::thread::spawn(move || {
                let mut output = String::new();
                stderr_pipe.read_to_string(&mut output).unwrap();
                output
            });
            let deadline = Instant::now() + Duration::from_secs(30);
            let status = loop {
                if let Some(status) = worker.0.try_wait().unwrap() {
                    break status;
                }
                assert!(Instant::now() < deadline, "{phase} worker timed out");
                std::thread::sleep(Duration::from_millis(20));
            };
            let stdout = stdout_reader.join().unwrap();
            println!("PHASE={phase}\n{stdout}");
            eprintln!("{}", stderr_reader.join().unwrap());
            assert!(stdout.contains("running 1 test"), "worker selected no test");
            assert!(
                status.success(),
                "{phase} isolated assertion failed: {}",
                status
            );
        }
        return;
    }

    let directory = std::env::var_os(WORKER).unwrap();
    let directory = Path::new(&directory);
    let phase = std::env::var(PHASE).unwrap();
    let selected = directory.join("cosh-core");
    let gateway = directory.join("cosh-gateway");
    let core_before = fs::metadata(&selected).unwrap();
    let gateway_before = fs::metadata(&gateway).unwrap();
    let AdapterInstance::CoshCore(adapter) = build_adapter(AdapterKind::CoshCore) else {
        panic!("expected existing Core adapter");
    };
    if phase == "pinned" {
        let new = fs::metadata(directory.join("NEW")).unwrap();
        assert_ne!(
            (core_before.dev(), core_before.ino()),
            (new.dev(), new.ino())
        );
        fs::rename(directory.join("NEW"), &selected).unwrap();
        fs::rename(directory.join("NEW_GATEWAY"), &gateway).unwrap();
        let direct = observe_native(
            Command::new(&selected)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        assert_eq!(direct, ("NEW".into(), new.dev(), new.ino()));
        for _ in 0..2 {
            assert_eq!(
                observe_native(provider_child(&adapter.program)),
                ("OLD".into(), core_before.dev(), core_before.ino())
            );
            assert_eq!(
                observe_native(gateway_child()),
                (
                    "OLD_GATEWAY".into(),
                    gateway_before.dev(),
                    gateway_before.ino()
                )
            );
        }
        let external = directory.join("external-bin/cosh-core");
        let metadata = fs::metadata(&external).unwrap();
        let child = crate::adapter::command_with_trusted_startup_path(&external)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        assert_eq!(
            observe_native(child),
            ("NEW".into(), metadata.dev(), metadata.ino())
        );
    } else if phase == "fresh" {
        assert_eq!(
            observe_native(provider_child(&adapter.program)),
            ("NEW".into(), core_before.dev(), core_before.ino())
        );
        assert_eq!(
            observe_native(gateway_child()),
            (
                "NEW_GATEWAY".into(),
                gateway_before.dev(),
                gateway_before.ino()
            )
        );
    } else if phase == "script" || phase == "path" {
        if phase == "path" {
            crate::adapter::record_trusted_startup_path(format!(
                "{}:{}",
                directory.join("path-bin").display(),
                std::env::var("PATH").unwrap()
            ));
        }
        let core = provider_child(&adapter.program).wait_with_output().unwrap();
        assert!(core.status.success());
        assert_eq!(
            String::from_utf8(core.stdout).unwrap().trim(),
            if phase == "script" {
                "SCRIPT_CORE"
            } else {
                "PATH_CORE"
            }
        );
        if phase == "script" {
            let output = gateway_child().wait_with_output().unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap().trim(),
                "SCRIPT_GATEWAY"
            );
        } else {
            assert_eq!(
                observe_native(gateway_child()),
                (
                    "NEW_GATEWAY".into(),
                    gateway_before.dev(),
                    gateway_before.ino()
                )
            );
        }
        println!("COMPATIBILITY={phase} PASS");
    } else if phase == "fifo" {
        let fifo = directory.join("fifo-core");
        fs::remove_file(&fifo).unwrap();
        fs::copy(directory.join("external-bin/cosh-core"), &fifo).unwrap();
        let error = match crate::adapter::command_with_trusted_startup_path(&fifo).spawn() {
            Err(error) => error,
            Ok(child) => {
                let _child = RunningChild(child);
                panic!("rebound failed admission");
            }
        };
        assert_eq!(error.raw_os_error(), Some(nix::libc::EACCES));
        println!("NONREGULAR_ADMISSION_FROZEN=PASS");
    } else {
        panic!("unexpected test phase");
    }
}
