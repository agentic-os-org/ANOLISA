//! 验证 SDK 环境变量的发现和 JSONL 到 ATIF 的完整链路。

use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_trajectory_collector::{atif, discovery, qoder};

const SESSION: &str = "00000000-0000-0000-0000-000000000001";
const CHILD_ROOT: &str = "AGENTSIGHT_SDK_DISCOVERY_TEST_ROOT";

#[test]
fn inherited_config_is_discovered_and_explicit_roots_override_it() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!("sdk-discovery-{}-{nonce}", std::process::id()));
    let config_dir = root.join("arbitrary-state-directory");
    let project = config_dir.join("projects/-srv-workspace-example");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join(format!("{SESSION}.jsonl")),
        concat!(
            "{\"type\":\"user\",\"timestamp\":\"2026-10-21T01:00:00Z\",\"message\":{\"role\":\"user\",\"content\":\"Inspect fixture\"}}\n",
            "{\"type\":\"assistant\",\"timestamp\":\"2026-10-21T01:00:01Z\",\"message\":{\"role\":\"assistant\",\"model\":\"test-model\",\"usage\":{\"input_tokens\":10,\"output_tokens\":5},\"content\":[{\"type\":\"thinking\",\"thinking\":\"Need evidence\"},{\"type\":\"tool_use\",\"id\":\"call-test\",\"name\":\"Read\",\"input\":{\"file_path\":\"fixture.txt\"}}]}}\n",
            "{\"type\":\"user\",\"timestamp\":\"2026-10-21T01:00:02Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"call-test\",\"content\":\"Fixture read\"}]}}\n",
        ),
    ).unwrap();

    // 环境变量只在子进程设置，避免并行测试之间污染发现范围。
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "sdk_discovery_child", "--nocapture"])
        .env(CHILD_ROOT, &root)
        .env("CLAUDE_CONFIG_DIR", &config_dir)
        .env_remove("HERMES_HOME")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn sdk_discovery_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let sessions = discovery::discover_sessions(None);
    let session = sessions
        .iter()
        .find(|session| session.session_id == SESSION && session.path.starts_with(&root))
        .expect("SDK session was discovered");
    assert_eq!(session.source, "claude-code");
    // 只转换测试轨迹，不读取开发机上其他用户的会话正文。
    let content = std::fs::read_to_string(&session.path).unwrap();
    let events = qoder::load_jsonl_events(&content);
    let trajectory = atif::convert_qoder_events(&events, &session.source).unwrap();
    assert_eq!(trajectory.agent.name, "claude-code");
    assert_eq!(trajectory.steps.len(), 2);
    assert_eq!(
        trajectory
            .final_metrics
            .as_ref()
            .unwrap()
            .total_prompt_tokens,
        Some(10)
    );
    let doc = trajectory.to_json_value().unwrap();
    assert_eq!(doc["steps"][1]["reasoning_content"], "Need evidence");
    assert_eq!(doc["steps"][1]["tool_calls"][0]["function_name"], "Read");
    assert_eq!(
        doc["steps"][1]["observation"]["results"][0]["source_call_id"],
        "call-test"
    );

    let isolated = discovery::discover_sessions(Some(&[root.join("missing-directory")]));
    assert!(isolated.is_empty());
}
