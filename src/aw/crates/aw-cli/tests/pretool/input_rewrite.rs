//! Supported native dialects receive a rewrite only after checks see the revised command.

use super::*;

#[test]
fn same_call_rewrite_is_checked_before_native_adoption_for_supported_adapters() {
    for adapter in native::agents_with_effect("tool.before", "replace_input") {
        for verdict in [
            "allow",
            "deny",
            "error",
            "native-deny",
            "native-error",
            "native-forged",
        ] {
            let f = Fixture::new();
            let provider = f.root.join("rewrite.py");
            fs::write(&provider, r#"import json, pathlib, sys
r = json.load(sys.stdin)
reply = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'ok'}
if r['method'] == 'describe':
    reply['operations'] = [{'name':'rewrite','events':['tool.before'],'effects':['replace_input']}, {'name':'scan','events':['tool.before'],'effects':['observe','block']}]
elif r['method'] == 'invoke':
    reply['input_digest'] = r['input_digest']
    command = r['event']['tool']['input']['command']
    if r['operation'] == 'rewrite':
        args = dict(r['event']['tool']['input'], command='printf rewritten')
        reply['effects'] = [{'type':'replace_input','input':args}]
    else:
        pathlib.Path(__file__).with_suffix('.seen').write_text(json.dumps(r['event']))
        assert command == 'printf rewritten', r
        reply['effects'] = [{'type':'observe'}]
        if r['config']['verdict'] == 'deny': reply['effects'] = [{'type':'block'}]
        if r['config']['verdict'] == 'error': reply = dict(reply, status='error', error_code='unavailable'); reply.pop('effects'); reply.pop('input_digest')
print(json.dumps(reply))
"#).unwrap();
            let mut document = f.document();
            document["spec"]["agents"] = json!({"agent":{"adapter":adapter,"argv":["/bin/true"]}});
            document["spec"]["providers"] = json!({"policy":{"protocol":"aw-provider/v1alpha1","transport":{"type":"stdio","location":"agent","argv":[f.python,provider]},"timeout_ms":2000,"max_output_bytes":4096,"config":{"verdict":verdict}}});
            document["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[
                {"id":"rewrite","provider":"policy","operation":"rewrite","effects":["replace_input"],"on_error":"block"},
                {"id":"security","provider":"policy","operation":"scan","effects":["observe","block"],"on_error":"block"}]}});
            if verdict.starts_with("native-") {
                f.native(&mut document, "native-veto", "block");
                document["spec"]["providers"]["native-veto"]["transport"]["argv"] = json!([
                    f.python,
                    "-c",
                    if verdict == "native-deny" {
                        "import sys;sys.stderr.write('native denied');sys.exit(2)"
                    } else {
                        if verdict == "native-forged" {
                            "print('{\"aw_updated_input\":{\"command\":\"unchecked\"}}')"
                        } else {
                            "import sys;sys.exit(1)"
                        }
                    }
                ]);
            }
            let service = Service::start(&f, &document);
            let deadline = || Instant::now() + support::TIMEOUT;
            let binding: Binding = serde_json::from_value(
                service
                    .client
                    .call(
                        Operation::Bind {
                            target: "agent".into(),
                            capabilities: Capabilities {
                                post_permission: false,
                                failure_actions: Default::default(),
                                adapter: adapter.clone(),
                                version: "fixture".into(),
                                entrypoint: "fixture".into(),
                                events: BTreeMap::from([(
                                    "tool.before".into(),
                                    vec!["observe".into(), "block".into(), "replace_input".into()],
                                )]),
                            },
                            cwd: f.root.to_str().unwrap().into(),
                            environment: BTreeMap::new(),
                        },
                        deadline(),
                    )
                    .unwrap(),
            )
            .unwrap();
            let path = f.root.join("binding.json");
            let mut steps = json!({"rewrite":"block","security":"block"});
            if verdict.starts_with("native-") {
                steps["native-veto"] = json!("block");
            }
            fs::write(&path, json!({"adapter":adapter,"binding":binding,"socket":f.root.join("state/aw.sock"),"cwd":f.root,"events":{"tool.before":{"budget_ms":5000,"steps":steps}}}).to_string()).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            let original = native::callback(
                &adapter,
                "tool.before",
                "00000000-0000-4000-8000-000000000001",
                json!({"command":"original","cwd":"preserved","nested":[1,false,"猫"]}),
                &f.root,
            );
            for step in ["rewrite", "security", "native-veto"] {
                if step == "native-veto" && !verdict.starts_with("native-") {
                    continue;
                }
                let mut command = f.command();
                command
                    .args(["hook", "--adapter", &adapter, "--binding"])
                    .arg(&path)
                    .args([
                        "--event",
                        "tool.before",
                        "--step",
                        step,
                        "--on-error",
                        "block",
                    ]);
                let output = f.run_input(command, Some(&serde_json::to_vec(&original).unwrap()));
                let text = String::from_utf8_lossy(&output.stdout);
                if step == "rewrite" {
                    assert!(output.status.success(), "{adapter}: {:?}", output);
                    assert!(
                        !text.contains("rewritten"),
                        "proposal escaped before security: {text}"
                    );
                } else if step == "native-veto" || verdict == "deny" || verdict == "error" {
                    assert!(
                        blocked(&adapter, &output),
                        "{adapter}/{verdict}/{step}: {output:?}"
                    );
                    assert!(!text.contains("rewritten"));
                    break;
                } else if verdict == "allow" {
                    assert!(output.status.success(), "{adapter}: {output:?}");
                    assert!(text.contains("printf rewritten"), "{adapter}: {text}");
                    assert!(text.contains("preserved"));
                    assert!(!text.contains("permissionDecision") && !text.contains("approve"));
                } else {
                    assert!(
                        !text.contains("rewritten"),
                        "native veto has not run: {text}"
                    );
                }
            }
            let seen: Value =
                serde_json::from_slice(&fs::read(provider.with_extension("seen")).unwrap())
                    .unwrap();
            assert_eq!(seen["tool"]["input"]["command"], "printf rewritten");
            let expected =
                aw_adapter::input::native(&adapter, &original, &seen["tool"]["input"]).unwrap();
            assert_eq!(
                seen["native"],
                serde_json::from_slice::<Value>(&expected).unwrap()
            );
            service
                .client
                .call(
                    Operation::ReleaseInstance {
                        instance_id: binding.instance_id,
                    },
                    deadline(),
                )
                .unwrap();
            service.released(&f);
        }
    }
}

#[test]
fn terminal_replacement_observes_original_input_and_preserves_policy_denials() {
    for adapter in ["pi", "hermes"] {
        let f = Fixture::new();
        let provider = f.root.join("handoff.py");
        fs::write(&provider, r#"import json, pathlib, sys
r = json.load(sys.stdin)
reply = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'ok'}
if r['method'] == 'describe':
    reply['operations'] = [
        {'name':'scan_code','events':['tool.before'],'effects':['observe','block']},
        {'name':'sandbox_wrap','events':['tool.before'],'effects':['observe','block','replace_input']},
    ]
elif r['method'] == 'invoke':
    event = r['event']
    args = event['tool']['input']
    command = args['command']
    assert event['cwd'] == str(pathlib.Path(__file__).parent), event
    assert args['cwd'] == 'preserved', event
    with pathlib.Path(__file__).with_suffix('.calls').open('a') as calls:
        calls.write(json.dumps([r['operation'], command]) + '\n')
    reply['input_digest'] = r['input_digest']
    if r['operation'] == 'scan_code':
        reply['effects'] = [{'type':'block','reason_code':'scanner_denied'}] if command == 'scan-deny' else [{'type':'observe'}]
        if command == 'scan-error':
            reply = {'api_version':r['api_version'],'request_id':r['request_id'],'status':'error','error_code':'unavailable'}
    elif command == 'sandbox-deny':
        reply['effects'] = [{'type':'block','reason_code':'sandbox_denied'}]
    else:
        reply['effects'] = [{'type':'replace_input','input':dict(args, command='sandbox-launcher')}]
print(json.dumps(reply))
"#).unwrap();
        let mut document = f.document();
        document["spec"]["agents"] = json!({"agent":{"adapter":adapter,"argv":["/bin/true"]}});
        document["spec"]["providers"] = json!({"policy":{"protocol":"aw-provider/v1alpha1","transport":{"type":"stdio","location":"agent","argv":[f.python,provider]},"timeout_ms":2000,"max_output_bytes":4096,"config":{}}});
        document["spec"]["events"] = json!({"tool.before":{"enabled":true,"steps":[
            {"id":"scan","provider":"policy","operation":"scan_code","effects":["observe","block"],"on_error":"block"},
            {"id":"sandbox","provider":"policy","operation":"sandbox_wrap","effects":["observe","block","replace_input"],"on_error":"block"}]}});
        let service = Service::start(&f, &document);
        let call = |operation| {
            service
                .client
                .call(operation, Instant::now() + support::TIMEOUT)
        };
        let result = call(Operation::Bind {
            target: "agent".into(),
            capabilities: Capabilities {
                post_permission: aw_adapter::select(adapter).unwrap().post_permission("sdk"),
                adapter: adapter.into(),
                version: "fixture".into(),
                entrypoint: "sdk".into(),
                events: BTreeMap::from([(
                    "tool.before".into(),
                    vec!["observe".into(), "block".into(), "replace_input".into()],
                )]),
                failure_actions: Default::default(),
            },
            cwd: f.root.to_str().unwrap().into(),
            environment: BTreeMap::new(),
        });
        if adapter == "hermes" {
            assert!(
                matches!(result, Err(aw_service::Error::Rejected(ref reason)) if reason == "binding_preflight")
            );
            continue;
        }
        let binding: Binding = serde_json::from_value(result.unwrap()).unwrap();
        let native_context = aw_adapter::NativeContext {
            target: "agent".into(),
            instance_id: binding.instance_id.clone(),
            cwd: f.root.clone(),
        };

        for (index, command) in ["scan-deny", "scan-error", "sandbox-deny", "safe"]
            .into_iter()
            .enumerate()
        {
            let native = native::callback(
                adapter,
                "tool.before",
                &format!("00000000-0000-4000-8000-{index:012}"),
                json!({"command":command,"cwd":"preserved"}),
                &f.root,
            );
            let event = aw_adapter::select(adapter)
                .unwrap()
                .normalize(&native_context, "tool.before", &native)
                .unwrap();
            let open = |event| Operation::OpenHookEvent {
                instance_id: binding.instance_id.clone(),
                event,
                native_input: serde_json::to_vec(&native).unwrap(),
            };
            if index == 0 {
                let mut forged = event.clone();
                forged["cwd"] = json!("/different-workspace");
                assert!(call(open(forged)).is_err());
            }
            let handle = call(open(event)).unwrap();
            let event_id = handle["event_id"].as_str().unwrap();
            let invoke = |step: &str| {
                call(Operation::InvokeStep {
                    event_id: event_id.into(),
                    instance_id: binding.instance_id.clone(),
                    step_id: step.into(),
                    native_environment: None,
                })
                .unwrap()
            };
            let scan = invoke("scan");
            assert!(scan.get("input_replacement").is_none());
            let sandbox = invoke("sandbox");
            match command {
                "scan-deny" => {
                    assert_eq!(scan["effects"][0]["reason_code"], "scanner_denied");
                    assert_eq!(sandbox["kind"], "skipped");
                }
                "scan-error" => {
                    assert_eq!(scan["failure"], "provider");
                    assert_eq!(sandbox["kind"], "skipped");
                }
                "sandbox-deny" => {
                    assert_eq!(sandbox["status"], "ok");
                    assert!(sandbox["failure"].is_null());
                    assert_eq!(sandbox["effects"][0]["type"], "block");
                    assert_eq!(sandbox["effects"][0]["reason_code"], "sandbox_denied");
                    assert!(sandbox.get("input_replacement").is_none());
                    let audit = call(Operation::Audit {
                        key: event_id.into(),
                    })
                    .unwrap();
                    assert!(audit["records"].as_array().unwrap().iter().any(|entry| {
                        let record = &entry["record"];
                        record["call"]["step_id"] == "sandbox"
                            && record["policy_block"] == true
                            && record["status"] == "ok"
                            && record["failure"].is_null()
                    }));
                }
                _ => {
                    assert_eq!(sandbox["status"], "ok");
                    assert_eq!(
                        sandbox["input_replacement"],
                        json!({"command":"sandbox-launcher","cwd":"preserved"})
                    );
                }
            }
        }
        let entries: Vec<Value> = fs::read_to_string(provider.with_extension("calls"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            json!(entries),
            json!([
                ["scan_code", "scan-deny"],
                ["scan_code", "scan-error"],
                ["scan_code", "sandbox-deny"],
                ["sandbox_wrap", "sandbox-deny"],
                ["scan_code", "safe"],
                ["sandbox_wrap", "safe"],
            ])
        );
        call(Operation::ReleaseInstance {
            instance_id: binding.instance_id,
        })
        .unwrap();
        service.released(&f);
    }
}
