//! Real CLI requests must leave default discovery and authorization to the daemon.

mod common;

use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::net::UnixListener;

#[tokio::test]
async fn caller_home_and_xdg_cannot_inject_implicit_skill_roots() {
    let directory = common::Directory::new();
    let home = directory.0.join("home");
    let default_data = home.join(".local/share");
    let custom_data = directory.0.join("custom data");
    for data in [&default_data, &custom_data] {
        let skill = data.join("anolisa/skills/demo");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "Safe skill").unwrap();
    }
    let socket = directory.0.join("daemon.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    for (home, data) in [
        (Some(home.as_path()), None),
        (Some(home.as_path()), Some(custom_data.as_path())),
        (Some(home.as_path()), Some(Path::new("relative"))),
        (None, Some(custom_data.as_path())),
        (None, None),
    ] {
        for arguments in [
            &["init", "--scanners=code-scanner"][..],
            &["scan", "--all"][..],
            &["check", "--all"][..],
            &["init", "--no-baseline"][..],
            &["init", "--skill-dir", "/explicit/skill"][..],
            &["check", "/explicit/skill"][..],
            &["scan", "/explicit/skill"][..],
        ] {
            let mut cli = Command::new(env!("CARGO_BIN_EXE_agent-sec-cli"));
            cli.current_dir(&directory.0)
                .arg("--socket")
                .arg(&socket)
                .args(["--timeout-ms", "2000", "skillsec"])
                .args(arguments)
                .env_remove("HOME")
                .env_remove("XDG_DATA_HOME");
            if let Some(home) = home {
                cli.env("HOME", home);
            }
            if let Some(data) = data {
                cli.env("XDG_DATA_HOME", data);
            }
            let request = capture_request(&listener, cli).await;
            assert_eq!(request["method"], "action.skill_sec");
            let expected = if arguments.contains(&"--skill-dir") {
                json!(["/explicit/skill"])
            } else {
                json!([])
            };
            assert_eq!(
                request["params"]["skillDirs"], expected,
                "arguments={arguments:?}, HOME={home:?}, XDG_DATA_HOME={data:?}"
            );
            if arguments[1] == "/explicit/skill" {
                assert_eq!(request["params"]["skillDir"], "/explicit/skill");
            }
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
