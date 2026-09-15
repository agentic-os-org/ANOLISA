//! Caller environment discovery is exercised through real CLI requests without global env mutation.

mod common;

use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;

#[tokio::test]
async fn batch_commands_discover_the_callers_anolisa_install_root() {
    let directory = common::Directory::new();
    let home = directory.0.join("home");
    let default_data = home.join(".local/share");
    let custom_data = directory.0.join("custom data");
    for data in [
        &default_data,
        &custom_data,
        &directory.0.join(".local/share"),
    ] {
        for relative in [
            "demo",
            "demo/.skill-meta/snapshot",
            ".hidden",
            "group/nested",
        ] {
            let skill = data.join("anolisa/skills").join(relative);
            fs::create_dir_all(&skill).unwrap();
            fs::write(skill.join("SKILL.md"), "Safe skill").unwrap();
        }
    }
    let socket = directory.0.join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let cases = [
        (Some(home.as_path()), None, Some(&default_data)),
        (
            Some(home.as_path()),
            Some(custom_data.clone()),
            Some(&custom_data),
        ),
        (Some(home.as_path()), Some("".into()), Some(&default_data)),
        (
            Some(home.as_path()),
            Some("relative".into()),
            Some(&default_data),
        ),
        (
            Some(home.as_path()),
            Some(custom_data.join(".")),
            Some(&default_data),
        ),
        (
            Some(home.as_path()),
            Some(directory.0.join("missing")),
            None,
        ),
        (None, Some(custom_data.clone()), Some(&custom_data)),
        (None, None, None),
        (Some(Path::new("")), None, None),
    ];
    for (home, data, expected) in cases {
        for arguments in [
            ["init", "--scanners=code-scanner"],
            ["scan", "--all"],
            ["check", "--all"],
            ["init", "--no-baseline"],
            ["check", "/explicit/skill"],
            ["scan", "/explicit/skill"],
        ] {
            let mut cli = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"));
            cli.current_dir(&directory.0)
                .arg("--socket")
                .arg(&socket)
                .args(["--timeout-ms", "2000", "skill-ledger"])
                .args(arguments)
                .env_remove("HOME")
                .env_remove("XDG_DATA_HOME");
            if let Some(home) = home {
                cli.env("HOME", home);
            }
            if let Some(data) = &data {
                cli.env("XDG_DATA_HOME", data);
            }
            let request = capture_request(&listener, cli).await;
            assert_eq!(request["method"], "action.skill_guard");
            let actual: Vec<_> = request["params"]["skillDirs"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .filter(|root| Path::new(root).starts_with(&directory.0))
                .collect();
            let discovers = arguments[1] == "--all" || arguments[1] == "--scanners=code-scanner";
            let expected: Vec<_> = expected
                .filter(|_| discovers)
                .map(|root| root.join("anolisa/skills/demo"))
                .into_iter()
                .collect();
            assert_eq!(
                actual,
                expected
                    .iter()
                    .map(|p| p.to_str().unwrap())
                    .collect::<Vec<_>>(),
                "arguments={arguments:?}, HOME={home:?}, XDG_DATA_HOME={data:?}"
            );
        }
    }
}

async fn capture_request(listener: &UnixListener, mut cli: Command) -> Value {
    let process = tokio::task::spawn_blocking(move || cli.output().unwrap());
    let request = tokio::time::timeout(Duration::from_secs(5), async {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = BufReader::new(stream);
        let mut line = String::new();
        stream.read_line(&mut line).await.unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let response = json!({"requestId":"discovery-test", "result":{
            "success":true,"exitCode":0,"error":null,"errorType":"","data":{}
        }});
        stream
            .get_mut()
            .write_all(format!("{response}\n").as_bytes())
            .await
            .unwrap();
        request
    })
    .await
    .unwrap();
    let output = process.await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    request
}
