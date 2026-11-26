//! Codex app-server stdio launch contracts.

use super::*;

#[test]
fn app_server_uses_local_stdio_and_the_same_hook_chain() {
    for args in [
        vec![],
        vec!["--listen", "stdio://"],
        vec!["--listen=stdio://"],
    ] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        let cli = doc["spec"]["agents"]["codex"]["argv"][0].clone();
        doc["spec"]["agents"]["codex"]["argv"] = json!([cli, "app-server"]);
        f.save(&doc);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut launch = command(&f, "run");
        launch.env("CODEX_INTERNAL_APP_SERVER_REMOTE_CONTROL_DISABLED", "0");
        launch.arg("--").args(&args);
        let output = f.run(launch);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(f.root.join("policy-calls")).unwrap(),
            "scan_prompt\nscan\nretry\nrecord\n"
        );
        let mut expected = vec!["app-server"];
        expected.extend(args);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(f.root.join("codex-probe-args")).unwrap())
                .unwrap(),
            json!(expected)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(f.root.join("codex-started")).unwrap())
                .unwrap(),
            json!(expected)
        );
        assert_eq!(
            fs::read_to_string(f.root.join("codex-probe-remote-control")).unwrap(),
            "1"
        );
        assert_eq!(
            fs::read_to_string(f.root.join("codex-started-remote-control")).unwrap(),
            "1"
        );
        service.released(&f);
    }
}

#[test]
fn app_server_rejects_remote_and_mutating_startup_options() {
    for args in [
        vec!["--stdio"],
        vec!["--listen", "ws://127.0.0.1:9000"],
        vec!["--listen", "unix:///tmp/codex.sock"],
        vec!["daemon", "start"],
        vec!["--config", "features.hooks=false"],
        vec!["--print-startup-config"],
    ] {
        let f = Fixture::new();
        let mut doc = setup(&f);
        let cli = doc["spec"]["agents"]["codex"]["argv"][0].clone();
        doc["spec"]["agents"]["codex"]["argv"] = json!([cli, "app-server"]);
        f.save(&doc);
        let service = Service::start(&f, &doc);
        install(&f);
        let mut launch = command(&f, "run");
        launch.arg("--").args(args);
        let output = f.run(launch);
        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("local stdio"));
        assert!(!f.root.join("codex-started").exists());
        assert_eq!(service.status()["bindings"], 0);
    }
}
