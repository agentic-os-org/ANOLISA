use super::*;
use nix::libc;

#[test]
fn memfd_denial_refuses_project_hook_registration() {
    let home = tempfile::tempdir().unwrap();
    let hooks = home.path().join(".cosh/hooks");
    fs::create_dir_all(&hooks).unwrap();
    write_executable(
        &hooks.join("approved.sh"),
        "#!/bin/sh\n# cosh-hook: approved\nexit 0\n",
    );
    for denial in [0, libc::ENOSYS as u32, libc::EPERM as u32] {
        let mut command = raw_cli_command(env!("CARGO_BIN_EXE_cosh-shell"));
        command
            .args(["raw", "fake"])
            .current_dir(home.path())
            .env("HOME", home.path())
            .env("LANG", "C")
            .env("LC_ALL", "C")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if denial != 0 {
            unsafe {
                command.pre_exec(move || {
                    let mut filter = [
                        libc::sock_filter {
                            code: (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16,
                            jt: 0,
                            jf: 0,
                            k: 0,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16,
                            jt: 0,
                            jf: 1,
                            k: libc::SYS_memfd_create as u32,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_RET | libc::BPF_K) as u16,
                            jt: 0,
                            jf: 0,
                            k: libc::SECCOMP_RET_ERRNO | denial,
                        },
                        libc::sock_filter {
                            code: (libc::BPF_RET | libc::BPF_K) as u16,
                            jt: 0,
                            jf: 0,
                            k: libc::SECCOMP_RET_ALLOW,
                        },
                    ];
                    let program = libc::sock_fprog {
                        len: filter.len() as u16,
                        filter: filter.as_mut_ptr(),
                    };
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                        || libc::prctl(libc::PR_SET_SECCOMP, libc::SECCOMP_MODE_FILTER, &program)
                            != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"/hooks\nexit\n")
            .unwrap();
        if child
            .wait_timeout(Duration::from_secs(30))
            .unwrap()
            .is_none()
        {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("memfd denial fixture timed out: {denial}");
        }
        let output = child.wait_with_output().unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.status.success(), "{text}");
        assert!(
            text.contains(if denial == 0 {
                "project=1"
            } else {
                "project=0"
            }),
            "denial={denial}: {text}"
        );
    }
}
