//! Supported native dialects receive a rewrite only after checks see the revised command.

use super::*;

#[test]
fn same_call_rewrite_is_checked_before_native_adoption_for_supported_adapters() {
    for adapter in native::agents()
        .into_iter()
        .filter(|adapter| !matches!(adapter.as_str(), "openclaw" | "deepseek-harness" | "codex"))
    {
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
