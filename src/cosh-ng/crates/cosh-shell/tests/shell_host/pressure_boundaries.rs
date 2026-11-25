use super::*;

struct BoundaryInput(std::sync::mpsc::Receiver<Vec<u8>>);

impl Read for BoundaryInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let bytes = self
            .0
            .recv_timeout(Duration::from_secs(15))
            .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?;
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
}

fn boundary_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "cosh-pressure-boundary-{label}-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("cg")).unwrap();
    std::fs::write(root.join("cg/memory.high"), "100\n").unwrap();
    std::fs::write(
        root.join("home/.bashrc"),
        format!(
            "_COSH_CGROUP_DIR={}\nPS1='BOUNDARY> '\nPS2='CONTINUE> '\nPS0='__USER_PS0__'\n",
            shell_arg(&root.join("cg"))
        ),
    )
    .unwrap();
    root
}

fn boundary_config(root: &std::path::Path) -> ShellHostConfig {
    let mut config = ShellHostConfig::new("pressure-boundary", root.join("work"))
        .with_env("HOME", root.join("home").display().to_string())
        .with_env("LANG", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8");
    config.slash_via_shell = false;
    config
}

fn send(sender: &std::sync::mpsc::Sender<Vec<u8>>, bytes: &[u8]) -> io::Result<()> {
    sender
        .send(bytes.to_vec())
        .map_err(|error| io::Error::other(error.to_string()))
}

fn assert_recovered(output: &ShellHostOutput, rendered: &[u8]) {
    let ledger = ledger_from_output(output);
    let command = "printf '%s\\n' __AFTER_BOUNDARY__";
    let blocks: Vec<_> = ledger
        .blocks
        .iter()
        .filter(|block| block.command == command)
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "{:?}; terminal={:?}",
        output.events,
        String::from_utf8_lossy(rendered)
    );
    assert_eq!(blocks[0].exit_code, 0);
    assert!(ledger_output_refs_text(&ledger).contains("__AFTER_BOUNDARY__"));
    assert!(std::str::from_utf8(rendered).is_ok(), "invalid UTF-8 PS0");
}

#[test]
fn pressure_boundary_foreground_transitions_preserve_completion() {
    for (initial, changed) in [(50, 200), (200, 50)] {
        let root = boundary_root(&format!("foreground-{initial}"));
        std::fs::write(root.join("cg/memory.current"), format!("{initial}\n")).unwrap();
        let command = "printf '%s\\n' __FG_READY__; while [[ ! -f \"$HOME/release\" ]]; do read -r -t 0.02 unused || :; done; printf '%s\\n' __FG_DONE__";
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut phase = 0;
        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_observer(
            &boundary_config(&root),
            BoundaryInput(receiver),
            &mut rendered,
            |_, output| {
                let text = String::from_utf8_lossy(output).replace("\x1b[>4;1m", "");
                match phase {
                    0 if text.ends_with("BOUNDARY> ") => {
                        phase = 1;
                        send(&sender, format!("{command}\n").as_bytes())?;
                    }
                    1 if text.contains("__FG_READY__\r\n") => {
                        phase = 2;
                        std::fs::write(root.join("cg/memory.current"), format!("{changed}\n"))?;
                        std::fs::write(root.join("home/release"), b"release")?;
                    }
                    2 if text.contains("__FG_DONE__\r\n") && text.ends_with("BOUNDARY> ") => {
                        phase = 3;
                        std::fs::write(root.join("cg/memory.current"), "50\n")?;
                        send(&sender, b"printf '%s\\n' __AFTER_BOUNDARY__\n")?;
                    }
                    3 if text.contains("__AFTER_BOUNDARY__\r\n")
                        && text.ends_with("BOUNDARY> ") =>
                    {
                        phase = 4;
                        send(&sender, b"exit\n")?;
                    }
                    _ => {}
                }
                Ok(())
            },
        )
        .unwrap_or_else(|error| {
            panic!(
                "foreground phase={phase}: {error}; terminal={:?}",
                String::from_utf8_lossy(&rendered)
            )
        });
        assert_eq!(phase, 4);
        assert_recovered(&output, &rendered);
        if initial == 50 {
            let ledger = ledger_from_output(&output);
            let block = ledger
                .blocks
                .iter()
                .find(|block| block.command == command)
                .expect("foreground block");
            assert_eq!(block.exit_code, 0);
        }
        std::fs::remove_dir_all(root).ok();
    }
}

#[test]
fn pressure_boundary_multiline_release_waits_for_complete_command() {
    let root = boundary_root("multiline");
    std::fs::write(root.join("cg/memory.current"), "200\n").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut phase = 0;
    let mut premature = false;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_observer(
        &boundary_config(&root),
        BoundaryInput(receiver),
        &mut rendered,
        |events, output| {
            let text = String::from_utf8_lossy(output).replace("\x1b[>4;1m", "");
            match phase {
                0 if text.ends_with("BOUNDARY> ") => {
                    phase = 1;
                    send(&sender, b"printf '%s\\n' '\n")?;
                }
                1 if text.ends_with("CONTINUE> ") => {
                    premature = events
                        .iter()
                        .any(|event| event.kind == ShellEventKind::CommandStarted);
                    phase = 2;
                    std::fs::write(root.join("cg/memory.current"), "50\n")?;
                    send(&sender, b"__MULTILINE_DONE__'\n")?;
                }
                2 if text.contains("__MULTILINE_DONE__\r\n") && text.ends_with("BOUNDARY> ") => {
                    phase = 3;
                    send(&sender, b"printf '%s\\n' __AFTER_BOUNDARY__\n")?;
                }
                3 if text.contains("__AFTER_BOUNDARY__\r\n") && text.ends_with("BOUNDARY> ") => {
                    phase = 4;
                    send(&sender, b"exit\n")?;
                }
                _ => {}
            }
            Ok(())
        },
    )
    .unwrap_or_else(|error| {
        panic!(
            "multiline phase={phase}: {error}; terminal={:?}",
            String::from_utf8_lossy(&rendered)
        )
    });
    assert_eq!(phase, 4);
    assert!(
        !premature,
        "PS2 input must not invent an execution boundary"
    );
    assert_recovered(&output, &rendered);
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn pressure_boundary_custom_accept_preserves_foreground_bytes() {
    for (label, binding, accept) in [
        (
            "ctrl-x",
            r#""\C-x\C-a": accept-line"#,
            b"\x18\x01".as_slice(),
        ),
        ("macro", r#""\ej": "\C-m""#, b"\x1bj".as_slice()),
        ("vi", "set editing-mode vi", b"\n".as_slice()),
        (
            "ctrl-o",
            r#""\C-o": operate-and-get-next"#,
            b"\x0f".as_slice(),
        ),
    ] {
        let root = boundary_root(label);
        std::fs::write(root.join("cg/memory.current"), "200\n").unwrap();
        std::fs::write(
            root.join("inputrc"),
            format!("set editing-mode emacs\nset enable-bracketed-paste off\n{binding}\n"),
        )
        .unwrap();
        let config =
            boundary_config(&root).with_env("INPUTRC", root.join("inputrc").display().to_string());
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut phase = 0;
        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_observer(&config, BoundaryInput(receiver), &mut rendered, |_, output| {
            let text = String::from_utf8_lossy(output).replace("\x1b[>4;1m", "");
            match phase {
                0 if text.ends_with("BOUNDARY> ") => {
                    phase = 1; std::fs::write(root.join("cg/memory.current"), "50\n")?;
                    let mut bytes = b"printf '%s\\n' __READER_READY__; IFS= read -r payload; printf '__PAYLOAD__=<%s>\\n' \"$payload\"".to_vec();
                    bytes.extend_from_slice(accept); send(&sender, &bytes)?;
                }
                1 if text.contains("__READER_READY__\r\n") => { phase = 2; send(&sender, b"foreground-data\n")?; }
                2 if text.contains("__PAYLOAD__=<") && text.ends_with("BOUNDARY> ") => {
                    phase = 3; send(&sender, b"printf '%s\\n' __AFTER_BOUNDARY__\n")?;
                }
                3 if text.contains("__AFTER_BOUNDARY__\r\n") && text.ends_with("BOUNDARY> ") => { phase = 4; send(&sender, b"exit\n")?; }
                _ => {}
            }
            Ok(())
        }).unwrap_or_else(|error| panic!("{label} phase={phase}: {error}; terminal={:?}", String::from_utf8_lossy(&rendered)));
        assert_eq!(phase, 4, "{label}");
        let terminal = String::from_utf8_lossy(&rendered);
        assert!(
            terminal.contains("\r\n__PAYLOAD__=<foreground-data>\r\n"),
            "{label}: {terminal:?}"
        );
        assert_recovered(&output, &rendered);
        std::fs::remove_dir_all(root).ok();
    }
}

#[test]
fn pressure_boundary_handoff_has_one_correlated_command() {
    let root = boundary_root("handoff");
    std::fs::write(root.join("cg/memory.current"), "200\n").unwrap();
    let command = "printf '%s\\n' __HANDOFF_BOUNDARY__";
    let mut emitted = false;
    let mut rendered = Vec::new();
    let output = run_raw_relay_bash_with_actions_output_control(
        &boundary_config(&root),
        vec![
            RawRelayAction::wait(Duration::from_millis(500)),
            RawRelayAction::line("exit"),
        ],
        &mut rendered,
        |_, _| {
            if emitted {
                return Ok(RawObserverAction::Continue);
            }
            emitted = true;
            Ok(RawObserverAction::EmitToPty(
                ShellHandoffRequest::new(
                    command,
                    format!("$ {command}"),
                    "approved_provider_shell_tool",
                    "user",
                    "approval-boundary",
                    "run-boundary",
                    1,
                )
                .unwrap(),
            ))
        },
    )
    .unwrap();
    let ledger = ledger_from_output(&output);
    assert_eq!(
        output
            .events
            .iter()
            .filter(|event| event.kind == ShellEventKind::CommandStarted
                && event.command.as_deref() == Some(command))
            .count(),
        1
    );
    let block = ledger
        .blocks
        .iter()
        .find(|block| block.command == command)
        .unwrap();
    assert_eq!(block.exit_code, 0);
    assert_eq!(block.origin, cosh_shell::types::CommandOrigin::ProviderTool);
    assert!(block.audit_identity.is_some());
    std::fs::remove_dir_all(root).ok();
}

#[test]
fn pressure_boundary_editor_submit_preserves_foreground_bytes() {
    for (label, editing_mode, keys) in [("emacs-editor", "emacs", b"\x18\x05".as_slice())] {
        for initial in [50, 200] {
            let root = boundary_root(&format!("{label}-{initial}"));
            std::fs::write(root.join("cg/memory.current"), format!("{initial}\n")).unwrap();
            let editor = root.join("editor");
            std::fs::write(&editor, "#!/bin/sh\nprintf '%s\\n' 'printf \"%s\\n\" __EDITOR_READY__; IFS= read -r payload; printf \"__EDITOR_PAYLOAD__=<%s>\\n\" \"$payload\"' > \"$1\"\n").unwrap();
            make_executable(&editor);
            std::fs::write(root.join("inputrc"), format!("set editing-mode {editing_mode}\nset enable-bracketed-paste off\n\"\\C-x\\C-e\": edit-and-execute-command\n")).unwrap();
            let config = boundary_config(&root)
                .with_env("INPUTRC", root.join("inputrc").display().to_string())
                .with_env("VISUAL", editor.display().to_string())
                .with_env("EDITOR", editor.display().to_string());
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut phase = 0;
            let mut rendered = Vec::new();
            let output = run_raw_relay_bash_with_observer(
                &config,
                BoundaryInput(receiver),
                &mut rendered,
                |_, output| {
                    let text = String::from_utf8_lossy(output).replace("\x1b[>4;1m", "");
                    match phase {
                        0 if text.ends_with("BOUNDARY> ") => {
                            phase = 1;
                            std::fs::write(root.join("cg/memory.current"), "50\n")?;
                            send(&sender, keys)?;
                        }
                        1 if text.contains("__EDITOR_READY__\r\n") => {
                            phase = 2;
                            send(&sender, b"editor-data\n")?;
                        }
                        2 if text.contains("__EDITOR_PAYLOAD__=<")
                            && text.ends_with("BOUNDARY> ") =>
                        {
                            phase = 3;
                            send(&sender, b"printf '%s\\n' __AFTER_BOUNDARY__\n")?;
                        }
                        3 if text.contains("__AFTER_BOUNDARY__\r\n")
                            && text.ends_with("BOUNDARY> ") =>
                        {
                            phase = 4;
                            send(&sender, b"exit\n")?;
                        }
                        _ => {}
                    }
                    Ok(())
                },
            )
            .unwrap_or_else(|error| {
                panic!(
                    "{label} phase={phase}: {error}; terminal={:?}",
                    String::from_utf8_lossy(&rendered)
                )
            });
            let terminal = String::from_utf8_lossy(&rendered);
            assert_eq!(phase, 4, "{label}: {terminal:?}");
            assert!(
                terminal.contains("\r\n__EDITOR_PAYLOAD__=<editor-data>\r\n"),
                "{label}: {terminal:?}"
            );
            assert_recovered(&output, &rendered);
            std::fs::remove_dir_all(root).ok();
        }
    }
}

#[test]
fn pressure_boundary_meta_history_guard_blocks_command_substitution() {
    for split in [false, true] {
        let root = boundary_root(&format!("meta-history-{split}"));
        std::fs::write(root.join("cg/memory.current"), "50\n").unwrap();
        let witness = root.join("witness");
        let mut rc = std::fs::read_to_string(root.join("home/.bashrc")).unwrap();
        rc.push_str(&format!(
            "builtin history -s '/skills detail $(touch {})'\n",
            witness.display()
        ));
        std::fs::write(root.join("home/.bashrc"), rc).unwrap();
        let mut config = boundary_config(&root);
        config.slash_via_shell = true;
        let mut actions = vec![RawRelayAction::wait(Duration::from_millis(100))];
        if split {
            actions.push(RawRelayAction::write(b"\x1b".to_vec()));
            actions.push(RawRelayAction::wait(Duration::from_millis(10)));
            actions.push(RawRelayAction::write(b"<".to_vec()));
        } else {
            actions.push(RawRelayAction::write(b"\x1b<".to_vec()));
        }
        actions.extend([
            RawRelayAction::write(b"\n".to_vec()),
            RawRelayAction::wait(Duration::from_millis(150)),
            RawRelayAction::line("exit"),
        ]);
        let mut rendered = Vec::new();
        let output = run_raw_relay_bash_with_actions(&config, actions, &mut rendered).unwrap();
        assert!(
            !witness.exists(),
            "history expansion escaped guard: {:?}",
            String::from_utf8_lossy(&rendered)
        );
        assert!(
            output
                .events
                .iter()
                .any(|event| event.component.as_deref() == Some("slash")
                    && event
                        .input
                        .as_deref()
                        .is_some_and(|input| input.starts_with("/skills detail"))),
            "history action did not hit: {:?}",
            String::from_utf8_lossy(&rendered)
        );
        std::fs::remove_dir_all(root).ok();
    }
}
