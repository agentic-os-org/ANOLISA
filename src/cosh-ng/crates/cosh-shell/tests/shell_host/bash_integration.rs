use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Makes the module copy's compatibility round trips independent of wall-clock
/// deadlines and of the shared startup budget for the environment-conflict
/// fixtures, so a slow registration no longer decides whether they reach the
/// conflict they test. The production deadline, budget and quiet retreat stay
/// covered against the unmodified module by
/// `registration_deadline_quietly_retreats`.
fn decouple_compat_timing(module: &str, helper: &str) -> String {
    let mut body = module.to_string();
    let call = format!(
        "/usr/bin/timeout --kill-after=0.005 \"$timeout\" {helper} shell-integration \"$operation\""
    );
    let plain = format!("{helper} shell-integration \"$operation\"");
    for (old, new) in [
        (call.as_str(), plain.as_str()),
        ("-n 8193 -t \"$timeout\" field", "-n 8193 field"),
        (
            "-z $field && $__cosh_hook_now -lt $__cosh_hook_deadline &&",
            "-z $field &&",
        ),
        (
            "available=$((__cosh_hook_now + 10000 - start))\n        __cosh_hook_used=$((__cosh_hook_used + available))\n        __cosh_bash_spent_us=$((__cosh_hook_startup_spent + available))",
            "__cosh_hook_used=0\n        __cosh_bash_spent_us=0",
        ),
    ] {
        assert_eq!(
            body.matches(old).count(),
            1,
            "compat deadline marker moved: {old:?}"
        );
        body = body.replacen(old, new, 1);
    }
    body
}

fn run_fixture(case: &str, interactive: bool) -> String {
    let live = case.starts_with("hook_live_");
    let fault_live = case.starts_with("hook_live_fault_");
    let prompt_test = case.starts_with("hook_prompt_");
    let chld_test = case.starts_with("hook_chld_");
    let stdin_shell = live || prompt_test || chld_test;
    let shadow = case.starts_with("hook_shadow_");
    let readonly_prompt = case.starts_with("hook_readonly_");
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../packaging/bash")
        .canonicalize()
        .expect("Bash integration assets");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bash-integration");
    let work = tempfile::tempdir().expect("isolated Bash fixture");
    let root = work.path().canonicalize().unwrap();
    let assets = root.join("assets");
    fs::create_dir_all(assets.join("tests")).unwrap();
    let helper = assets.join("tests/helper.bash");
    fs::copy(fixtures.join("helper.bash"), &helper).unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    let profile = fs::read_to_string(source.join("../system-integration/profile.d/cosh.sh"))
        .unwrap()
        .replace(
            "/usr/libexec/anolisa/cosh-ng/cosh-shell",
            helper.to_str().unwrap(),
        );
    fs::write(assets.join("cosh-profile.sh"), profile).unwrap();
    let wrapper = fs::read_to_string(source.join("../system-integration/bashrc-source.bash"))
        .unwrap()
        .replace(
            "/usr/libexec/anolisa/cosh-ng/cosh-integration.bash",
            assets.join("cosh-integration.bash").to_str().unwrap(),
        );
    fs::write(assets.join("bashrc-source.bash"), wrapper).unwrap();
    for name in [
        "cosh-integration.bash",
        "tests/fixture.bash",
        "tests/hook-fixture.bash",
        "tests/hook-helper.bash",
        "tests/hook-fault-helper.bash",
        "tests/hook-output-failure.bash",
        "tests/hook-live.bash",
        "tests/hook-input.bash",
        "tests/hook-cargo.bash",
        "tests/hook-shadow.bash",
        "tests/hook-shadow-readonly.bash",
        "tests/hook-prompt.bash",
        "tests/hook-chld.bash",
        "tests/hook-shadow-helper.py",
    ] {
        let path = match name.strip_prefix("tests/") {
            Some(name) => fixtures.join(name),
            None => source.join(name),
        };
        let body = fs::read_to_string(path).unwrap().replace(
            "/usr/libexec/anolisa/cosh-ng/cosh-shell",
            helper.to_str().unwrap(),
        );
        fs::write(assets.join(name), body).unwrap();
    }
    if case.starts_with("hook_environment_") {
        let module = assets.join("cosh-integration.bash");
        let body = fs::read_to_string(&module).unwrap();
        fs::write(
            &module,
            decouple_compat_timing(&body, helper.to_str().unwrap()),
        )
        .unwrap();
    }
    if case == "hook_live_fault_printf_candidate" {
        let failed_output = assets.join("tests/hook-output-failure.bash");
        fs::set_permissions(&failed_output, fs::Permissions::from_mode(0o755)).unwrap();
        let module = assets.join("cosh-integration.bash");
        let body = fs::read_to_string(&module)
            .unwrap()
            .replace("/usr/bin/printf", failed_output.to_str().unwrap());
        fs::write(module, body).unwrap();
    }
    if case.starts_with("hook_") {
        fs::copy(
            assets.join(if fault_live {
                "tests/hook-fault-helper.bash"
            } else if shadow || prompt_test {
                "tests/hook-shadow-helper.py"
            } else {
                "tests/hook-helper.bash"
            }),
            &helper,
        )
        .unwrap();
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    if live || chld_test {
        fs::create_dir(root.join("runtime")).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::copy(assets.join("tests/hook-cargo.bash"), root.join("bin/cargo")).unwrap();
        fs::set_permissions(root.join("bin/cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    }
    if case == "missing_module" {
        fs::remove_file(assets.join("cosh-integration.bash")).unwrap();
    }
    let stdout = root.join("stdout");
    let stderr = root.join("stderr");
    let mut command = Command::new("/bin/bash");
    command.args(["--noprofile", "--norc"]);
    if interactive {
        command.arg("-i");
        if case != "nonlogin" {
            command.arg("-l");
        }
        if case == "explicit_command" {
            command.args([
                "-c",
                "builtin source \"$1\" \"$2\" \"$3\" \"$4\"",
                "cosh-fixture",
            ]);
        }
    }
    if !stdin_shell {
        command
            .arg(assets.join(if shadow {
                "tests/hook-shadow.bash"
            } else if readonly_prompt {
                "tests/hook-shadow-readonly.bash"
            } else if case.starts_with("hook_") {
                "tests/hook-fixture.bash"
            } else {
                "tests/fixture.bash"
            }))
            .arg(case)
            .arg(&assets)
            .arg(&root);
    }
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &root)
        .env("LC_ALL", "C")
        .env("COSH_BASH_TEST_WORKDIR", &root)
        .env("COSH_BASH_TEST_ASSETS", &assets)
        .env("COSH_BASH_TEST_CASE", case)
        .current_dir(&root);
    if case.starts_with("hook_environment_") || case == "hook_registration_timeout" {
        command.env("COSH_BASH_TEST_REGISTRATION_DELAY_MS", "60");
    }
    if shadow {
        let parts = case
            .strip_prefix("hook_shadow_")
            .unwrap()
            .split('_')
            .collect::<Vec<_>>();
        let phase = parts[0];
        let target = parts[1];
        assert!(["builtin", "local", "return", "command", "type", "double"].contains(&target));
        let safe = if target == "command" {
            "builtin"
        } else {
            "command builtin"
        };
        let cancel = if parts[2] == "self" {
            format!("{safe} export -nf {target};")
        } else {
            String::new()
        };
        let action = if ["builtin", "command"].contains(&target) {
            format!(r#"{safe} "$@";"#)
        } else {
            format!(r#"{safe} {target} "$@";"#)
        };
        let body = format!(
            r#"() {{ {safe} printf 'shadow:{target}:%s\n' "$1" >>"$COSH_BASH_TEST_WORKDIR/shadow.calls"; {cancel} {action} }}"#
        );
        let definition = if target == "double" {
            ["builtin", "command"].iter().map(|name| {
                format!(r#"{name} () {{ printf 'shadow:{name}:%s\n' "$1" >>"$COSH_BASH_TEST_WORKDIR/shadow.calls"; ((1)); }}"#)
            }).collect::<Vec<_>>().join("; ")
        } else {
            format!("{target} {body}")
        };
        command
            .env("COSH_BASH_SHADOW_PHASE", phase)
            .env("COSH_BASH_SHADOW_TARGET", target)
            .env("COSH_BASH_SHADOW_VARIANT", parts[2])
            .env("COSH_BASH_SHADOW_DEFINITION", definition);
        if phase == "registration" {
            if target == "double" {
                for name in ["builtin", "command"] {
                    command.env(format!("BASH_FUNC_{name}%%"), format!(r#"() {{ printf 'shadow:{name}:%s\n' "$1" >>"$COSH_BASH_TEST_WORKDIR/shadow.calls"; ((1)); }}"#));
                }
            } else {
                command.env(format!("BASH_FUNC_{target}%%"), body);
            }
        }
        if parts[2] == "disabled" || parts.get(3) == Some(&"disabled") {
            command.env("COSH_BASH_DISABLE", "1");
        }
    }
    let prompt_input = if prompt_test {
        let parts = case
            .strip_prefix("hook_prompt_")
            .unwrap()
            .split('_')
            .collect::<Vec<_>>();
        command
            .env("PS1", "__HOOK_PS1__ ")
            .env("COSH_PROMPT_PHASE", parts[0])
            .env("COSH_PROMPT_CODE", parts[1])
            .env("COSH_PROMPT_MODE", parts[2]);
        let failure = "; __round=1; false";
        let setup = "builtin source \"$COSH_BASH_TEST_ASSETS/tests/hook-prompt.bash\" setup";
        let mut lines = if parts[0] == "registered" {
            vec![format!("{setup}{failure}\n")]
        } else {
            vec![format!("{setup}\n"), format!("builtin source \"$COSH_BASH_TEST_ASSETS/tests/hook-prompt.bash\" configure{failure}\n")]
        };
        lines.push("__round=2; (exit 5)\n".to_string());
        lines.push(
            "builtin source \"$COSH_BASH_TEST_ASSETS/tests/hook-prompt.bash\" finish\n".to_string(),
        );
        lines
    } else if chld_test {
        let parts = case
            .strip_prefix("hook_chld_")
            .unwrap()
            .split('_')
            .collect::<Vec<_>>();
        command
            .env("PS1", "__HOOK_PS1__ ")
            .env("COSH_CHLD_PHASE", parts[0])
            .env("COSH_CHLD_MODE", parts[1]);
        let setup = "builtin source \"$COSH_BASH_TEST_ASSETS/tests/hook-chld.bash\" setup\n";
        let mut lines = vec![setup.to_string()];
        if parts[0] == "active" {
            lines.push(
                "builtin source \"$COSH_BASH_TEST_ASSETS/tests/hook-chld.bash\" configure\n"
                    .to_string(),
            );
        }
        for action in [
            "discovered",
            "idle",
            "idle",
            "idle",
            "after-idle",
            "cargo",
            "status",
            "finish",
        ] {
            lines.push(match action {
                "idle" => "builtin :\n".to_string(),
                "cargo" => "cargo build\n".to_string(),
                "status" => {
                    "builtin printf 'OBS_CARGO:%s:%s\\n' \"$?\" \"${PIPESTATUS[*]}\"\n".to_string()
                }
                _ => format!(
                    "builtin source \"$COSH_BASH_TEST_ASSETS/tests/hook-chld.bash\" {action}\n"
                ),
            });
        }
        lines
    } else {
        Vec::new()
    };
    let mut prompt_sent = 0;
    let mut master = if interactive {
        let pty = nix::pty::openpty(None, None).expect("Bash fixture PTY");
        let master = File::from(pty.master);
        let slave = File::from(pty.slave);
        if stdin_shell {
            let mut attributes = unsafe { std::mem::zeroed::<nix::libc::termios>() };
            assert_eq!(
                unsafe { nix::libc::tcgetattr(slave.as_raw_fd(), &mut attributes) },
                0
            );
            attributes.c_lflag &= !nix::libc::ECHO;
            assert_eq!(
                unsafe { nix::libc::tcsetattr(slave.as_raw_fd(), nix::libc::TCSANOW, &attributes) },
                0
            );
        }
        let flags = unsafe { nix::libc::fcntl(master.as_raw_fd(), nix::libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(
            unsafe {
                nix::libc::fcntl(
                    master.as_raw_fd(),
                    nix::libc::F_SETFL,
                    flags | nix::libc::O_NONBLOCK,
                )
            },
            0
        );
        command
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        unsafe {
            command.pre_exec(|| {
                if nix::libc::setsid() < 0
                    || nix::libc::ioctl(nix::libc::STDIN_FILENO, nix::libc::TIOCSCTTY as _, 0) < 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        Some(master)
    } else {
        command
            .stdin(Stdio::null())
            .stdout(File::create(&stdout).unwrap())
            .stderr(File::create(&stderr).unwrap());
        None
    };
    let mut child = command.spawn().expect("spawn Bash fixture");
    if live {
        let input = fs::read_to_string(assets.join("tests/hook-input.bash")).unwrap();
        let input = if case.starts_with("hook_live_function_") {
            input.replacen("\ncargo build\n", "\nf() { cargo build; }; f\n", 1)
        } else {
            input
        };
        master
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut output = Vec::new();
    let status = loop {
        let finished = child.try_wait().expect("Bash fixture status");
        let mut overflow = false;
        if let Some(master) = master.as_mut() {
            let mut chunk = [0; 4096];
            while let Ok(count) = master.read(&mut chunk) {
                if count == 0 {
                    break;
                }
                output.extend_from_slice(&chunk[..count]);
                if output.len() > 64 * 1024 {
                    overflow = true;
                    break;
                }
            }
        }
        if (prompt_test || chld_test)
            && prompt_sent < prompt_input.len()
            && String::from_utf8_lossy(&output)
                .matches("__HOOK_PS1__ ")
                .count()
                > prompt_sent
        {
            master
                .as_mut()
                .unwrap()
                .write_all(prompt_input[prompt_sent].as_bytes())
                .unwrap();
            prompt_sent += 1;
        }
        if let Some(status) = finished {
            assert!(!overflow, "unbounded Bash fixture output");
            break status;
        }
        if overflow || Instant::now() >= deadline {
            if interactive {
                unsafe { nix::libc::kill(-(child.id() as i32), nix::libc::SIGKILL) };
            }
            let _ = child.kill();
            let _ = child.wait();
            panic!("Bash fixture {case} exceeded its deadline or output limit");
        }
        thread::sleep(Duration::from_millis(5));
    };
    let output = if interactive {
        String::from_utf8_lossy(&output).into_owned()
    } else {
        fs::read_to_string(stdout).unwrap()
    };
    let errors = if interactive {
        String::new()
    } else {
        fs::read_to_string(stderr).unwrap()
    };
    let shadow_calls = if shadow {
        fs::read_to_string(root.join("shadow.calls")).unwrap_or_default()
    } else {
        String::new()
    };
    let helper_calls = if shadow || live || chld_test {
        fs::read_to_string(root.join("hook.calls")).unwrap_or_default()
    } else {
        String::new()
    };
    assert!(
        status.success(),
        "Bash fixture {case} failed ({status}): output={output:?}, stderr={errors:?}, shadow_calls={shadow_calls:?}, helper_calls={helper_calls:?}"
    );
    if chld_test {
        let notices = fs::read_to_string(root.join("chld.calls")).unwrap();
        let discovered = notices.find("DISCOVERED\n").unwrap() + "DISCOVERED\n".len();
        let idle_end = notices.find("AFTER_IDLE\n").unwrap();
        let discovery_count = notices[..discovered]
            .lines()
            .filter(|line| line.starts_with("CHLD:"))
            .count();
        let idle_count = notices[discovered..idle_end]
            .lines()
            .filter(|line| line.starts_with("CHLD:"))
            .count();
        eprintln!("{case}: discovery_chld={discovery_count}, idle_chld={idle_count}, notices={notices:?}, helper_calls={helper_calls:?}");
        assert_eq!(
            fs::read(root.join("chld.before")).unwrap(),
            fs::read(root.join("chld.after")).unwrap(),
            "{case}"
        );
        assert!(
            output.contains("CHLD_STATE:1:1:1"),
            "{case}: {output}; notices={notices:?}; helper_calls={helper_calls:?}"
        );
        assert_eq!(idle_count, 0, "{case}: {notices}");
        let expected_calls = if case.starts_with("hook_chld_registered_") {
            1
        } else {
            2
        };
        assert_eq!(
            helper_calls.lines().collect::<Vec<_>>(),
            vec!["shell-integration compat"; expected_calls],
            "{case}: {notices}"
        );
        assert_eq!(
            fs::read_to_string(root.join("tool.calls"))
                .unwrap()
                .lines()
                .count(),
            1,
            "{case}"
        );
        assert_eq!(output.matches("TOOL_OUTPUT").count(), 1, "{case}: {output}");
    }
    if live && case.ends_with("candidate") {
        let calls = fs::read_to_string(root.join("hook.calls")).unwrap();
        let skipped = case == "hook_live_skip_candidate";
        let fault = case
            .strip_prefix("hook_live_fault_")
            .and_then(|case| case.strip_suffix("_candidate"));
        let advise_count = if fault == Some("missing") {
            0
        } else if skipped {
            5
        } else {
            1
        };
        let expected_calls = [
            vec!["shell-integration compat"; 2],
            vec!["shell-integration advise"; advise_count],
        ]
        .concat();
        assert_eq!(
            calls.lines().collect::<Vec<_>>(),
            expected_calls,
            "{output}"
        );
        assert_eq!(
            output.matches("FRAGMENT_INVITATION").count(),
            usize::from(fault.is_none()),
            "{output}"
        );
        let decisions = fs::read_to_string(root.join("hook.advise")).unwrap_or_default();
        let expected = match fault {
            Some("missing") => String::new(),
            Some(mode) => format!("FAULT:{mode}\n"),
            None if skipped => format!("{}SHOW:cargo test\n", "SKIP:cargo build\n".repeat(4)),
            None => "SHOW:cargo build\n".to_string(),
        };
        assert_eq!(decisions, expected);
        if fault == Some("missing") {
            assert!(output.contains("HOOK_HELPER_REMOVED"), "{output}");
        }
        if fault == Some("printf") {
            assert_eq!(
                fs::read_to_string(root.join("output.calls")).unwrap(),
                "printf-attempt\n"
            );
        }
    }
    if live {
        assert!(output.contains("HOOK_LIFECYCLE_OK"), "{output}");
        let calls = fs::read_to_string(root.join("tool.calls")).unwrap();
        assert_eq!(calls.lines().count(), 7, "{calls}; {output}");
        assert_eq!(output.matches("TOOL_OUTPUT").count(), 7, "{output}");
        assert!(output.contains("READ_RESULT:readline-answer"), "{output}");
        assert!(output.contains("MULTILINE"), "{output}");
    }
    output
}

#[test]
fn noninteractive_sources_have_no_enhancement_side_effects() {
    assert!(run_fixture("noninteractive", false).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn known_chain_component_defers_err_until_activation_and_relinquishes_user_changes() {
    let output = run_fixture("hook_registration", true);
    assert!(output.contains("HOOK_COMPONENT_OK"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn user_traps_options_functions_and_native_prompt_keep_ownership() {
    for case in [
        "hook_option_change",
        "hook_trap_change",
        "hook_function_change",
        "hook_native_change",
        "hook_guide_off",
    ] {
        assert!(
            run_fixture(case, true).contains("HOOK_COMPONENT_OK"),
            "{case}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn observed_fragments_use_real_prompt_boundaries_without_replaying_commands() {
    let observations = |text: &str| {
        text.lines()
            .filter_map(|line| line.find("OBS_").map(|index| line[index..].to_string()))
            .collect::<Vec<_>>()
    };
    let baseline = observations(&run_fixture("hook_live_baseline", true));
    let function_baseline = observations(&run_fixture("hook_live_function_baseline", true));
    let skipped = observations(&run_fixture("hook_live_skip_baseline", true));
    assert_eq!(baseline.len(), 3);
    assert_eq!(function_baseline, baseline);
    assert_eq!(skipped.len(), 3);
    for scenario in [
        "",
        "function_",
        "skip_",
        "fault_exit_",
        "fault_empty_",
        "fault_tail_",
        "fault_version_",
        "fault_disposition_",
        "fault_generation_",
        "fault_runtime_",
        "fault_timeout_",
        "fault_missing_",
        "fault_closed_",
        "fault_printf_",
    ] {
        let candidate = run_fixture(&format!("hook_live_{scenario}candidate"), true);
        let expected = if scenario == "skip_" {
            &skipped
        } else {
            &baseline
        };
        assert_eq!(&observations(&candidate), expected, "{scenario}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn protocol_tail_is_rejected_without_registering_a_callback() {
    assert!(run_fixture("hook_bad_tail", true).contains("HOOK_BAD_TAIL_OK"));
}

#[cfg(target_os = "linux")]
#[test]
fn login_banner_runs_once_without_hook_support() {
    let output = run_fixture("banner", true);
    assert_eq!(output.matches("fixture resource summary").count(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn missing_hook_module_keeps_login_banner_independent() {
    let output = run_fixture("missing_module", true);
    assert!(output.contains("fixture resource summary"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn user_hooks_keep_ownership_while_banner_remains_available() {
    let output = run_fixture("user_conflict", true);
    assert!(output.contains("fixture resource summary"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn exhausted_shared_startup_budget_stays_quiet() {
    let output = run_fixture("budget_exhausted", true);
    assert!(!output.contains("fixture resource summary"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn excluded_entries_do_not_start_helpers() {
    for case in [
        "admin_disabled",
        "internal_shell",
        "dynamic_loader",
        "explicit_command",
        "nonlogin",
    ] {
        let output = run_fixture(case, true);
        assert!(
            !output.contains("fixture resource summary"),
            "{case}: {output}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn helper_failures_are_silent() {
    let output = run_fixture("helper_failure", true);
    assert!(!output.contains("fixture helper diagnostic"), "{output}");
    assert!(!output.contains("fixture resource summary"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn oversized_or_late_helper_output_is_discarded() {
    for case in ["helper_oversize", "helper_slow"] {
        let output = run_fixture(case, true);
        assert!(!output.contains("00000000"), "{case}: {output}");
        assert!(!output.contains("fixture late summary"), "{case}: {output}");
        assert!(
            !output.contains("fixture resource summary"),
            "{case}: {output}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn registration_rejects_critical_function_shadows_before_calling_them() {
    for target in ["builtin", "local", "return", "command", "type"] {
        for variant in ["plain", "self"] {
            let case = format!("hook_shadow_registration_{target}_{variant}");
            assert!(
                run_fixture(&case, true).contains("SHADOW_RELINQUISHED"),
                "{case}"
            );
        }
    }
    assert!(
        run_fixture("hook_shadow_registration_double_plain", true).contains("SHADOW_RELINQUISHED")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn disabled_interactive_entry_does_not_call_a_user_return_function() {
    for case in [
        "hook_shadow_registration_return_disabled",
        "hook_shadow_registration_return_self_disabled",
    ] {
        assert!(
            run_fixture(case, true).contains("SHADOW_RELINQUISHED"),
            "{case}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn activation_rechecks_critical_functions_before_running_the_callback() {
    for target in ["builtin", "local", "return", "command", "type"] {
        for variant in ["plain", "self"] {
            let case = format!("hook_shadow_activation_{target}_{variant}");
            assert!(
                run_fixture(&case, true).contains("SHADOW_RELINQUISHED"),
                "{case}"
            );
        }
    }
    assert!(
        run_fixture("hook_shadow_activation_double_plain", true).contains("SHADOW_RELINQUISHED")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn noevent_prompt_relinquishes_new_critical_functions_without_calling_them() {
    for target in ["builtin", "local", "return", "command", "type"] {
        for variant in ["plain", "self"] {
            let case = format!("hook_shadow_noevent_{target}_{variant}");
            assert!(
                run_fixture(&case, true).contains("SHADOW_RELINQUISHED"),
                "{case}"
            );
        }
    }
    assert!(run_fixture("hook_shadow_noevent_double_plain", true).contains("SHADOW_RELINQUISHED"));
}

#[cfg(target_os = "linux")]
#[test]
fn readonly_prompt_array_relinquishes_without_writes_or_user_err_calls() {
    for case in [
        "hook_readonly_registered",
        "hook_readonly_active",
        "hook_readonly_owned_active",
    ] {
        assert!(
            run_fixture(case, true).contains("READONLY_PROMPT_INERT"),
            "{case}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn system_entries_reject_builtin_shadows_without_calling_them() {
    for entry in ["profile", "wrapper"] {
        for variant in ["plain", "self"] {
            for enabled in ["enabled", "disabled"] {
                let case = format!("entry_{entry}_builtin_shadow_{variant}_{enabled}");
                assert!(
                    run_fixture(&case, true).contains("ENTRY_SHADOW_RELINQUISHED"),
                    "{case}"
                );
            }
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn unsupported_system_entry_modes_leave_user_state_unchanged() {
    for case in [
        "entry_profile_restricted",
        "entry_profile_posix",
        "entry_wrapper_restricted",
        "entry_wrapper_posix",
    ] {
        let output = run_fixture(case, true);
        assert!(
            output.contains("ENTRY_MODE_RELINQUISHED"),
            "{case}: {output}"
        );
        assert!(!output.contains("restricted:"), "{case}: {output}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn user_err_and_status_match_without_the_retired_callback_after_a_failed_command() {
    let observations = |text: &str| {
        text.lines()
            .filter_map(|line| line.find("OBS_").map(|index| line[index..].to_string()))
            .collect::<Vec<_>>()
    };
    for phase in ["registered", "active"] {
        for code in [7, 42] {
            let baseline = observations(&run_fixture(
                &format!("hook_prompt_{phase}_{code}_baseline"),
                true,
            ));
            let candidate = observations(&run_fixture(
                &format!("hook_prompt_{phase}_{code}_candidate"),
                true,
            ));
            assert_eq!(candidate, baseline, "{phase}/{code}");
            assert_eq!(
                candidate
                    .iter()
                    .filter(|line| line.starts_with("OBS_ERR:"))
                    .count(),
                4
            );
            assert!(candidate.contains(&"OBS_ERR:ERR:1:false".to_string()));
            assert!(candidate.contains(&format!(
                "OBS_ERR:ERR:{code}:builtin return \"$__user_code\""
            )));
            assert!(candidate.contains(&"OBS_ERR:ERR:5:( exit 5 )".to_string()));
            assert!(candidate.contains(&"OBS_PC:AFTER:1:1:1".to_string()));
            assert!(candidate.contains(&"OBS_PC:AFTER:2:5:5".to_string()));
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn registration_defers_to_new_environment_conflicts_before_activation() {
    for variable in [
        "LD_SENTINEL",
        "GLIBC_TUNABLES",
        "BASH_ENV",
        "ENV",
        "BASH_XTRACEFD",
        "XDG_CONFIG_HOME",
        "BASH_COMPLETION_COMPAT_DIR",
        "BASH_COMPLETION_USER_FILE",
        "BASH_COMPLETION_DEBUG",
        "COSH_INTERNAL_SHELL",
        "COSH_SESSION_ID",
        "COSH_SHELL_ISOLATED",
    ] {
        let case = format!("hook_environment_registered_{variable}");
        assert!(
            run_fixture(&case, true).contains("HOOK_ENVIRONMENT_OK"),
            "{case}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn active_hook_relinquishes_new_environment_conflicts_without_helpers() {
    for variable in [
        "LD_SENTINEL",
        "GLIBC_TUNABLES",
        "BASH_ENV",
        "ENV",
        "BASH_XTRACEFD",
        "XDG_CONFIG_HOME",
        "BASH_COMPLETION_COMPAT_DIR",
        "BASH_COMPLETION_USER_FILE",
        "BASH_COMPLETION_DEBUG",
        "COSH_INTERNAL_SHELL",
        "COSH_SESSION_ID",
        "COSH_SHELL_ISOLATED",
    ] {
        let case = format!("hook_environment_active_{variable}");
        assert!(
            run_fixture(&case, true).contains("HOOK_ENVIRONMENT_OK"),
            "{case}"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn registration_deadline_quietly_retreats() {
    let output = run_fixture("hook_registration_timeout", true);
    assert!(output.contains("HOOK_QUIET_RETREAT_OK"), "{output}");
    assert!(
        output.contains("check=registration phase=registration"),
        "missing registration diagnostics: {output}"
    );
    assert!(output.contains("prompt_count=1"), "{output}");
    assert!(output.contains("registered=unset"), "{output}");
}

#[cfg(target_os = "linux")]
#[test]
fn chld_trap_changes_retire_the_observer_after_the_first_detection_window() {
    let observation = |text: &str| {
        text.lines()
            .find_map(|line| {
                line.find("OBS_CARGO:")
                    .map(|index| line[index..].to_string())
            })
            .unwrap()
    };
    for phase in ["registered", "active"] {
        let baseline = run_fixture(&format!("hook_chld_{phase}_baseline"), true);
        let candidate = run_fixture(&format!("hook_chld_{phase}_candidate"), true);
        assert_eq!(observation(&candidate), observation(&baseline), "{phase}");
        assert_eq!(observation(&candidate), "OBS_CARGO:101:101", "{phase}");
    }
}
