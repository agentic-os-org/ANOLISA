use super::*;

fn ask_once_hook(prompt: bool, workspace: &std::path::Path) -> crate::config::HookDefinition {
    crate::config::HookDefinition {
        command: if prompt {
            r#"if [ ! -e "$COSH_TEST_EXPOSURE_PROMPT_MARKER" ]; then : > "$COSH_TEST_EXPOSURE_PROMPT_MARKER"; printf '%s\n' '{"decision":"ask","reason":"test prompt exposure"}'; fi"#.into()
        } else {
            r#"printf '%s\n' '{"decision":"ask","reason":"test read exposure"}'"#.into()
        },
        name: Some("approval-exposure".into()),
        matcher: if prompt {
            None
        } else {
            Some("read_file".into())
        },
        timeout: Some(10_000),
        sequential: None,
        fail_open: false,
        env: [(
            "COSH_TEST_EXPOSURE_PROMPT_MARKER".into(),
            workspace
                .join("prompt-asked")
                .to_string_lossy()
                .into_owned(),
        )]
        .into(),
    }
}

async fn assert_exposure_guard(prompt: bool, resume: bool) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    config.session.persist_dir = dir.path().join("sessions").to_string_lossy().into_owned();
    config.hooks.enabled = true;
    if prompt {
        config.hooks.user_prompt_submit = vec![ask_once_hook(true, dir.path())];
    } else {
        config.hooks.pre_tool_use = vec![ask_once_hook(false, dir.path())];
    }
    std::fs::write(dir.path().join("read-only"), "read me").unwrap();
    let followup = || {
        tool_call(
            "write_file",
            json!({"path":"effect","content":"unapproved-followup"}),
        )
    };
    let calls = if prompt {
        vec![followup()]
    } else {
        vec![
            tool_call("read_file", json!({"path":"read-only"})),
            followup(),
        ]
    };
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config.clone(),
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        calls,
    );
    runtime.persist(&mut engine).unwrap();
    host_turn(
        &mut engine,
        dir.path().join("effect"),
        vec![(
            response(json!({
                "behavior":"host_executed_shell",
                "result":{"llmContent":"unauthorized host effect","metadata":{"exit_code":0}}
            })),
            true,
        )],
    )
    .await;
    runtime.persist(&mut engine).unwrap();
    if resume {
        let id = runtime.record.session_id.to_string();
        drop(engine);
        drop(runtime);
        config.hooks.enabled = false;
        let args = crate::cli::CliArgs::parse_from([
            "cosh-core",
            "--workspace",
            dir.path().to_str().unwrap(),
            "--resume",
            &id,
        ]);
        let resumed = SessionRuntime::initialize(&args, &config).unwrap();
        let (_, mut restored) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![followup()],
        );
        restored.session_id = resumed.record.session_id.to_string();
        restored.messages = resumed.record.messages.clone();
        restored.failed_side_effect_pending_approval =
            resumed.record.failed_side_effect_pending_approval;
        resumed.bind_reapproval_checkpoint(&mut restored).unwrap();
        runtime = resumed;
        engine = restored;
    }
    let id = if resume { "req-0" } else { "req-1" };
    turn(&mut engine, &response_for(id, json!({"behavior":"deny"})))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("effect")).unwrap(), "host-effect",
        "prompt={prompt}, resume={resume}: mismatched Host completion lost approval exposure and allowed automatic overwrite");
    assert_durable_pending(&runtime, &engine);
}

#[tokio::test]
async fn read_only_approval_exposure_blocks_automatic_followup() {
    assert_exposure_guard(false, false).await;
}

#[tokio::test]
async fn read_only_approval_exposure_survives_persist_and_resume() {
    assert_exposure_guard(false, true).await;
}

#[tokio::test]
async fn prompt_approval_exposure_blocks_automatic_followup() {
    assert_exposure_guard(true, false).await;
}

#[tokio::test]
async fn prompt_approval_exposure_survives_persist_and_resume() {
    assert_exposure_guard(true, true).await;
}

#[tokio::test]
async fn plain_non_effect_approvals_restore_only_the_exposure_baseline() {
    for prompt in [false, true] {
        for behavior in ["allow", "deny"] {
            for (memory_before, checkpoint_before) in [(false, false), (true, true), (false, true)]
            {
                let dir = tempfile::tempdir().unwrap();
                let mut config = CoreConfig::default();
                config.hooks.enabled = true;
                if prompt {
                    config.hooks.user_prompt_submit = vec![ask_once_hook(true, dir.path())];
                } else {
                    config.hooks.pre_tool_use = vec![ask_once_hook(false, dir.path())];
                }
                std::fs::write(dir.path().join("read-only"), "read me").unwrap();
                let calls = if prompt {
                    vec![vec![GenerateEvent::MessageEnd]]
                } else {
                    vec![tool_call("read_file", json!({"path":"read-only"}))]
                };
                let (mut runtime, mut engine) = fixture(
                    dir.path(),
                    config,
                    ExecutionProfile::Legacy,
                    ToolRegistry::with_defaults_for_test(),
                    calls,
                );
                runtime.persist(&mut engine).unwrap();
                engine.failed_side_effect_pending_approval = memory_before;
                engine
                    .reapproval_checkpoint
                    .as_ref()
                    .unwrap()
                    .set_pending(checkpoint_before)
                    .unwrap();
                turn(&mut engine, &response(json!({"behavior":behavior})))
                    .await
                    .unwrap();
                assert_eq!(engine.failed_side_effect_pending_approval, memory_before,
                    "prompt={prompt}, behavior={behavior}, memory={memory_before}, checkpoint={checkpoint_before}");
                assert_eq!(
                    engine
                        .reapproval_checkpoint
                        .as_ref()
                        .unwrap()
                        .pending()
                        .unwrap(),
                    checkpoint_before
                );
                assert!(!dir.path().join("effect").exists());
            }
        }
    }
}

#[tokio::test]
async fn exposure_arm_failure_prevents_every_non_effect_host_handoff() {
    for prompt in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.hooks.enabled = true;
        if prompt {
            config.hooks.user_prompt_submit = vec![ask_once_hook(true, dir.path())];
        } else {
            config.hooks.pre_tool_use = vec![ask_once_hook(false, dir.path())];
        }
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call("read_file", json!({"path":"read-only"}))],
        );
        runtime.persist(&mut engine).unwrap();
        let id = &runtime.record.session_id;
        let live = runtime
            .store
            .as_ref()
            .unwrap()
            .session_file(id)
            .parent()
            .unwrap()
            .join(format!(".{id}.live"));
        std::fs::remove_file(&live).unwrap();
        std::fs::write(live, "0").unwrap();
        let mut reader = BufReader::new(HostResponses {
            frames: vec![(response(json!({"behavior":"host_executed_shell","result":{"llmContent":"host","metadata":{"exit_code":0}}})),true)].into(),
            current: Vec::new(), offset: 0, effect: dir.path().join("effect"),
        }).lines();
        let mut output = Vec::new();
        assert!(engine
            .handle_user_message("perform effect", &mut reader, &mut output)
            .await
            .is_err());
        assert!(!String::from_utf8(output).unwrap().contains("can_use_tool"));
        assert!(
            !dir.path().join("effect").exists(),
            "arm failure must not read or deliver Host effect"
        );
    }
}

#[tokio::test]
async fn read_only_failure_bypass_preserves_uncertainty_and_restores_only_plain_denials() {
    for uncertain_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = CoreConfig::default();
        config.agent.approval_mode = ApprovalMode::Auto;
        config.hooks.enabled = true;
        config.hooks.pre_tool_use = vec![ask_once_hook(false, dir.path())];
        let mut bypass = ask_once_hook(false, dir.path());
        bypass.command = r#"printf '%s\n' '{"hookSpecificOutput":{"sandbox_bypass_request":{"original_command":"printf forbidden > effect","reason":"read-only bypass exposure"}}}'"#.into();
        config.hooks.post_tool_use_failure = vec![bypass];
        let (mut runtime, mut engine) = fixture(
            dir.path(),
            config,
            ExecutionProfile::Legacy,
            ToolRegistry::with_defaults_for_test(),
            vec![tool_call("read_file", json!({"path":"missing"}))],
        );
        runtime.persist(&mut engine).unwrap();
        let first = if uncertain_first {
            json!({"behavior":"host_executed_shell","result":{"llmContent":"unauthorized effect","metadata":{"exit_code":0}}})
        } else {
            json!({"behavior":"deny"})
        };
        host_turn(
            &mut engine,
            dir.path().join("effect"),
            vec![
                (response(first), uncertain_first),
                (response_for("req-1", json!({"behavior":"deny"})), false),
            ],
        )
        .await;
        assert_eq!(engine.failed_side_effect_pending_approval, uncertain_first);
        assert_eq!(
            engine
                .reapproval_checkpoint
                .as_ref()
                .unwrap()
                .pending()
                .unwrap(),
            uncertain_first
        );
        runtime.persist(&mut engine).unwrap();
        if uncertain_first {
            assert_durable_pending(&runtime, &engine);
        } else {
            assert!(!dir.path().join("effect").exists());
        }
    }
}

async fn assert_read_only_bypass_success(uncertain_first: bool) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = CoreConfig::default();
    config.agent.approval_mode = ApprovalMode::Auto;
    config.hooks.enabled = true;
    let mut ask = ask_once_hook(false, dir.path());
    ask.matcher = Some("list_directory".into());
    config.hooks.pre_tool_use = vec![ask];
    let mut bypass = ask_once_hook(false, dir.path());
    bypass.matcher = Some("list_directory".into());
    bypass.command = r#"printf '%s\n' '{"hookSpecificOutput":{"sandbox_bypass_request":{"original_command":"printf forbidden > effect","reason":"read-only successful retry"}}}'"#.into();
    config.hooks.post_tool_use_failure = vec![bypass];
    let (mut runtime, mut engine) = fixture(
        dir.path(),
        config,
        ExecutionProfile::Legacy,
        ToolRegistry::with_defaults_for_test(),
        vec![
            tool_call("list_directory", json!({})),
            tool_call(
                "write_file",
                json!({"path":"effect","content":"unapproved-followup"}),
            ),
        ],
    );
    runtime.persist(&mut engine).unwrap();
    let first = if uncertain_first {
        json!({"behavior":"host_executed_shell","result":{"llmContent":"unauthorized Host effect","metadata":{"exit_code":0}}})
    } else {
        json!({"behavior":"deny"})
    };
    host_turn(
        &mut engine,
        dir.path().join("effect"),
        vec![
            (response(first), uncertain_first),
            (response_for("req-1", json!({"behavior":"allow"})), false),
        ],
    )
    .await;
    assert_eq!(engine.failed_side_effect_pending_approval, uncertain_first);
    assert_eq!(
        engine
            .reapproval_checkpoint
            .as_ref()
            .unwrap()
            .pending()
            .unwrap(),
        uncertain_first
    );
    runtime.persist(&mut engine).unwrap();
    turn(
        &mut engine,
        &response_for("req-2", json!({"behavior":"deny"})),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read_to_string(dir.path().join("effect")).unwrap(),
        if uncertain_first { "host-effect" } else { "unapproved-followup" },
        "a successful production list_directory retry must preserve prior uncertainty and restore only a new exposure");
    if uncertain_first {
        assert_durable_pending(&runtime, &engine);
    }
}

#[tokio::test]
async fn successful_read_only_bypass_never_clears_a_prior_host_uncertainty() {
    assert_read_only_bypass_success(true).await;
}

#[tokio::test]
async fn plain_read_only_bypass_success_restores_new_exposure() {
    assert_read_only_bypass_success(false).await;
}
